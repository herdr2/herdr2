//! ACP agent binary/model discovery and generic AI endpoint connection testing.

use super::types::{ProbeSnapshot, ProbedAgent, now_iso};
use crate::acp::presets::{BUILTIN_PRESETS, resolve_acp_command_in_env, resolve_non_shim_binary_in_path};
use crate::acp::{SpawnSpec, spawn_session};
pub use crate::ai::client::{
    AiTestResult, all_model_ids, chat_endpoints, curl_json, curl_json_timeout, fetch_ai_models, first_model_id,
    models_endpoints, test_ai_connection,
};
use crate::runtime::Core;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const ACP_PROBE_CONCURRENCY: usize = 4;
const ACP_PROBE_RETRY_CONCURRENCY: usize = 2;
const ACP_PROBE_TIMEOUT: Duration = Duration::from_secs(45);
const ACP_PROBE_RETRY_TIMEOUT: Duration = Duration::from_secs(45);
const ACP_PROBE_RETRY_BACKOFF: Duration = Duration::from_secs(1);
const ACP_PROBE_TOTAL_BUDGET: Duration = Duration::from_secs(135);
const ACP_PROBE_MAX_RETRY_AGENTS: usize = 3;
const CLI_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug)]
struct ProbePolicy {
    concurrency: usize,
    retry_concurrency: usize,
    initial_timeout: Duration,
    retry_timeout: Duration,
    retry_backoff: Duration,
    total_budget: Duration,
    max_retry_agents: usize,
}

impl Default for ProbePolicy {
    fn default() -> Self {
        Self {
            concurrency: ACP_PROBE_CONCURRENCY,
            retry_concurrency: ACP_PROBE_RETRY_CONCURRENCY,
            initial_timeout: ACP_PROBE_TIMEOUT,
            retry_timeout: ACP_PROBE_RETRY_TIMEOUT,
            retry_backoff: ACP_PROBE_RETRY_BACKOFF,
            total_budget: ACP_PROBE_TOTAL_BUDGET,
            max_retry_agents: ACP_PROBE_MAX_RETRY_AGENTS,
        }
    }
}

struct AcpProbeTarget {
    id: String,
    argv: Vec<String>,
    exe_path: String,
    env: BTreeMap<String, String>,
    cwd: String,
}

struct TimedOutAttempt {
    target: AcpProbeTarget,
    detail: Option<String>,
    timeout: Duration,
}

enum ProbeAttemptOutcome {
    Done(ProbedAgent),
    TimedOut(TimedOutAttempt),
}

fn env_ignore_case<'a>(env: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str()).filter(|s| !s.is_empty())
}

fn is_unc_path(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    raw.starts_with(r"\\") || raw.starts_with("//")
}

/// Picks a stable, local, non-UNC working directory for headless ACP/CLI agent probing.
/// Inheriting `herdr2 serve`'s arbitrary `current_dir()` breaks Windows `.CMD` shims and
/// ACP adapters (e.g. `claude-agent-acp`) whenever the daemon started inside a WSL UNC share
/// (`\\wsl.localhost\...`), a read-only system directory, or a removed folder.
fn probe_working_dir(env: &BTreeMap<String, String>) -> String {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let home_keys: &[&str] = if cfg!(windows) { &["USERPROFILE", "HOME"] } else { &["HOME", "USERPROFILE"] };
    for key in home_keys {
        if let Some(val) = env_ignore_case(env, key) {
            candidates.push(PathBuf::from(val.trim()));
        }
        if let Ok(val) = std::env::var(key)
            && !val.trim().is_empty()
        {
            candidates.push(PathBuf::from(val.trim()));
        }
    }
    candidates.push(std::env::temp_dir());
    if let Ok(cur) = std::env::current_dir() {
        candidates.push(cur);
    }
    for cand in candidates {
        if !cand.as_os_str().is_empty() && !is_unc_path(&cand) && cand.is_dir() {
            return cand.display().to_string();
        }
    }
    std::env::temp_dir().display().to_string()
}

fn collect_probe_targets(core: &Core) -> Vec<(String, Vec<String>)> {
    let settings = core.settings();
    let mut seen_cmds: BTreeSet<Vec<String>> = BTreeSet::new();
    let mut targets: Vec<(String, Vec<String>)> = Vec::new();

    // Canonical display names avoid duplicate aliases (e.g. `agy` vs `antigravity`, `cursor` vs `cursor-agent`).
    for (name, cmd) in BUILTIN_PRESETS {
        if *name == "antigravity" || *name == "cursor-agent" {
            continue;
        }
        let argv: Vec<String> = cmd.iter().map(|s| (*s).to_string()).collect();
        if !settings.agents.contains_key(*name) && seen_cmds.insert(argv.clone()) {
            targets.push(((*name).to_string(), argv));
        }
    }
    for (name, cmd) in &settings.agents {
        if !cmd.is_empty() {
            seen_cmds.insert(cmd.clone());
            targets.push((name.clone(), cmd.clone()));
        }
    }
    targets.sort_by(|a, b| a.0.cmp(&b.0));
    targets
}

/// Resolves an ACP agent command for probing, skipping adapters whose required companion CLI
/// is missing from `PATH` (for example `pi-acp` installed without the `pi` CLI binary).
fn resolve_probe_command(
    id: &str,
    argv: &[String],
    env: &BTreeMap<String, String>,
) -> Option<(PathBuf, Vec<String>)> {
    let first = argv.first()?;
    if let Some(spec) = crate::acp::adapters::lookup_spec_for_preset_or_program(Some(id), first)
        && spec.requires_companion_cli
        && let Some(companion) = spec.companion_cli
    {
        let path_override = env_ignore_case(env, "PATH");
        let pathext_override = env_ignore_case(env, "PATHEXT");
        if resolve_non_shim_binary_in_path(companion, path_override, pathext_override).is_none() {
            return None;
        }
    }
    resolve_acp_command_in_env(Some(id), argv, env)
}

/// Fast (<2ms) stat-only refresh of `installed`/`path`/`command` flags against the current environment
/// so newly installed agents never remain stuck as "missing" in a cached probe snapshot.
/// Returns `true` if `snap` was modified.
pub async fn refresh_installed_status(core: &Core, snap: &mut ProbeSnapshot) -> bool {
    let targets = collect_probe_targets(core);
    let base_env = crate::platform::program_env().await;
    let mut changed = false;

    let target_ids: BTreeSet<String> = targets.iter().map(|(id, _)| id.clone()).collect();
    let before_len = snap.agents.len();
    snap.agents.retain(|a| target_ids.contains(&a.id));
    if snap.agents.len() != before_len {
        changed = true;
    }

    for (id, base_argv) in targets {
        let resolved = resolve_probe_command(&id, &base_argv, &base_env);
        let now_installed = resolved.is_some();
        let (now_path, now_cmd) = match resolved {
            Some((p, cmd)) => (Some(p.display().to_string()), cmd),
            None => (None, base_argv),
        };
        if let Some(existing) = snap.agents.iter_mut().find(|a| a.id == id) {
            let was_installed = existing.installed;
            if existing.installed != now_installed || existing.path != now_path || existing.command != now_cmd {
                existing.installed = now_installed;
                existing.path = now_path;
                existing.command = now_cmd;
                changed = true;
            }
            if !was_installed && now_installed {
                existing.error = None;
                if existing.models.is_empty() {
                    let (models, default_model) = discover_local_models(&id, true, &base_env);
                    existing.models = models;
                    if existing.default_model.is_none() {
                        existing.default_model = default_model;
                    }
                }
                if snap.full_probe {
                    snap.full_probe = false;
                    changed = true;
                }
            } else if was_installed && !now_installed {
                if existing.error.is_some() || !existing.models.is_empty() {
                    existing.error = None;
                    existing.models.clear();
                    changed = true;
                }
            }
        } else {
            let (models, default_model) = discover_local_models(&id, now_installed, &base_env);
            snap.agents.push(ProbedAgent {
                id,
                command: now_cmd,
                installed: now_installed,
                path: now_path,
                agent_info: None,
                models,
                default_model,
                efforts: default_efforts(),
                default_effort: Some("medium".into()),
                model_contexts: BTreeMap::new(),
                model_efforts: BTreeMap::new(),
                error: None,
            });
            if now_installed && snap.full_probe {
                snap.full_probe = false;
            }
            changed = true;
        }
    }

    if changed {
        snap.agents.sort_by(|a, b| b.installed.cmp(&a.installed).then_with(|| a.id.cmp(&b.id)));
    }
    changed
}

/// Probes installed ACP agents (and custom `[agents]` entries in `config.toml`) to extract
/// live model and reasoning-effort options for `Settings -> Drove -> Roles`.
pub async fn probe_agents(core: &Core, skip_acp_handshake: bool) -> ProbeSnapshot {
    let targets = collect_probe_targets(core);
    let base_env = crate::platform::program_env().await;
    let cwd = probe_working_dir(&base_env);

    let probe_scope = crate::sweep::token();
    let mut quick_handles = Vec::new();
    let mut acp_targets = Vec::new();
    for (id, argv) in targets {
        let resolved = resolve_probe_command(&id, &argv, &base_env);
        let mut env = base_env.clone();
        if resolved.is_none() || skip_acp_handshake {
            let installed = resolved.is_some();
            let (exe_path, effective_argv) = match resolved {
                Some((p, cmd)) => (Some(p), cmd),
                None => (None, argv),
            };
            quick_handles.push(tokio::spawn(async move {
                let (models, default_model) = discover_local_models(&id, installed, &env);
                ProbedAgent {
                    id,
                    command: effective_argv,
                    installed,
                    path: exe_path.map(|p| p.display().to_string()),
                    agent_info: None,
                    models,
                    default_model,
                    efforts: default_efforts(),
                    default_effort: Some("medium".into()),
                    model_contexts: BTreeMap::new(),
                    model_efforts: BTreeMap::new(),
                    error: None,
                }
            }));
            continue;
        }
        let (exe_path, effective_argv) = resolved.unwrap();
        env.insert(crate::sweep::VAR.into(), probe_scope.clone());
        acp_targets.push(AcpProbeTarget {
            id,
            argv: effective_argv,
            exe_path: exe_path.display().to_string(),
            env,
            cwd: cwd.clone(),
        });
    }

    let mut agents = probe_acp_targets_with_policy(acp_targets, ProbePolicy::default()).await;
    for h in quick_handles {
        if let Ok(item) = h.await {
            agents.push(item);
        }
    }
    if !skip_acp_handshake {
        let _ = tokio::task::spawn_blocking(move || crate::sweep::sweep(&[probe_scope])).await;
    }
    agents.sort_by(|a, b| b.installed.cmp(&a.installed).then_with(|| a.id.cmp(&b.id)));
    ProbeSnapshot { probed_at: now_iso(), full_probe: !skip_acp_handshake, agents }
}

/// Runs ACP probes in two phases:
/// 1) Bounded concurrent wave (`policy.concurrency`, default 4) where each agent runs its ACP
///    handshake before any CLI model probe (preventing self-contention on compile caches/state).
/// 2) If only a small number of agents timed out (likely cold-start or transient network contention)
///    and total budget permits, retries those timed-out agents once at lower concurrency
///    (`policy.retry_concurrency`, default 2) after Phase 1 agents have exited.
async fn probe_acp_targets_with_policy(targets: Vec<AcpProbeTarget>, policy: ProbePolicy) -> Vec<ProbedAgent> {
    if targets.is_empty() {
        return Vec::new();
    }
    let started = Instant::now();
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(policy.concurrency.max(1)));
    let mut handles = Vec::with_capacity(targets.len());
    for target in targets {
        let permit_sem = sem.clone();
        handles.push(tokio::spawn(async move {
            let _permit = permit_sem.acquire_owned().await;
            probe_one_acp_attempt(target, policy.initial_timeout).await
        }));
    }

    let mut agents = Vec::with_capacity(handles.len());
    let mut timed_out = Vec::new();
    for h in handles {
        match h.await {
            Ok(ProbeAttemptOutcome::Done(agent)) => agents.push(agent),
            Ok(ProbeAttemptOutcome::TimedOut(attempt)) => timed_out.push(attempt),
            Err(_) => {}
        }
    }

    if timed_out.is_empty() {
        return agents;
    }

    let can_retry = timed_out.len() <= policy.max_retry_agents
        && started.elapsed().saturating_add(policy.retry_backoff).saturating_add(policy.retry_timeout)
            <= policy.total_budget;

    if can_retry {
        if !policy.retry_backoff.is_zero() {
            tokio::time::sleep(policy.retry_backoff).await;
        }
        let retry_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(policy.retry_concurrency.max(1)));
        let mut retry_handles = Vec::with_capacity(timed_out.len());
        for attempt in timed_out {
            let permit_sem = retry_sem.clone();
            retry_handles.push(tokio::spawn(async move {
                let _permit = permit_sem.acquire_owned().await;
                match probe_one_acp_attempt(attempt.target, policy.retry_timeout).await {
                    ProbeAttemptOutcome::Done(agent) => agent,
                    ProbeAttemptOutcome::TimedOut(final_attempt) => finalize_timed_out_probe(final_attempt, true).await,
                }
            }));
        }
        for h in retry_handles {
            if let Ok(agent) = h.await {
                agents.push(agent);
            }
        }
    } else {
        for attempt in timed_out {
            agents.push(finalize_timed_out_probe(attempt, false).await);
        }
    }

    agents
}

fn is_handshake_timeout_error(msg: &str) -> bool {
    msg.to_ascii_lowercase().contains("timed out waiting for acp response")
}

fn format_timeout_error(
    timeout: Duration,
    retried: bool,
    acp_detail: Option<&str>,
    cli_error: Option<&str>,
) -> String {
    let secs = timeout.as_secs();
    let header = if retried {
        format!("ACP probe timed out after {secs}s (retried once)")
    } else {
        format!("ACP probe timed out after {secs}s")
    };
    let mut parts = vec![header];
    if let Some(detail) = acp_detail.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(detail.to_string());
    }
    if let Some(cli_err) = cli_error.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(format!("CLI fallback error: {cli_err}"));
    }
    sanitize_probe_error(&parts.join("\n"))
}

async fn finalize_timed_out_probe(attempt: TimedOutAttempt, retried: bool) -> ProbedAgent {
    let AcpProbeTarget { id, argv, exe_path, env, cwd } = attempt.target;
    let (local_models, local_default) = discover_local_models(&id, true, &env);
    let cli_probe = probe_cli_models(&id, &env, &cwd).await;
    let err_msg = format_timeout_error(
        attempt.timeout,
        retried,
        attempt.detail.as_deref(),
        cli_probe.error.as_deref(),
    );
    let models = merge_model_lists(&[cli_probe.models, local_models]);
    let model_efforts = infer_model_efforts_from_variants(&models);
    let default_model = local_default.or_else(|| models.first().cloned());
    ProbedAgent {
        id,
        command: argv,
        installed: true,
        path: Some(exe_path),
        agent_info: None,
        models,
        default_model,
        efforts: default_efforts(),
        default_effort: Some("medium".into()),
        model_contexts: BTreeMap::new(),
        model_efforts,
        error: Some(err_msg),
    }
}

#[cfg(test)]
async fn probe_one_acp(
    id: String,
    argv: Vec<String>,
    exe_path: String,
    env: BTreeMap<String, String>,
    cwd: String,
) -> ProbedAgent {
    let target = AcpProbeTarget { id, argv, exe_path, env, cwd };
    match probe_one_acp_attempt(target, ACP_PROBE_TIMEOUT).await {
        ProbeAttemptOutcome::Done(agent) => agent,
        ProbeAttemptOutcome::TimedOut(attempt) => finalize_timed_out_probe(attempt, false).await,
    }
}

async fn probe_one_acp_attempt(target: AcpProbeTarget, timeout: Duration) -> ProbeAttemptOutcome {
    let AcpProbeTarget { id, argv, exe_path, env, cwd } = target;
    let (local_models, local_default) = discover_local_models(&id, true, &env);
    if matches!(id.as_str(), "agy" | "antigravity") {
        // `agy acp` takes ~29s to boot CCPA + MCP servers on session/new; `agy models` returns the live catalog in ~5s.
        let (cli_probe, cli_ver) =
            tokio::join!(probe_cli_models(&id, &env, &cwd), probe_cli_version(&exe_path, &env, &cwd));
        let (agent_info, ver_err) = match cli_ver {
            Ok(ver) => (ver.map(|v| format!("Antigravity CLI {v}")), None),
            Err(err) => (None, Some(err)),
        };
        let error = cli_probe.error.or(ver_err);
        let models = merge_model_lists(&[cli_probe.models, local_models]);
        let model_efforts = infer_model_efforts_from_variants(&models);
        let default_model = local_default.or_else(|| models.first().cloned());
        return ProbeAttemptOutcome::Done(ProbedAgent {
            id,
            command: argv,
            installed: true,
            path: Some(exe_path),
            agent_info,
            models,
            default_model,
            efforts: default_efforts(),
            default_effort: Some("medium".into()),
            model_contexts: BTreeMap::new(),
            model_efforts,
            error,
        });
    }

    let spec = SpawnSpec {
        address: format!("probe-{id}"),
        argv: argv.clone(),
        env: env.clone(),
        cwd: cwd.clone(),
        resume_session_id: None,
        can_load_session: false,
        system_prompt: None,
        fallback_handoff_context: None,
        model: None,
        mode: None,
        dangerously_skip_permissions: false,
        config_overrides: BTreeMap::new(),
        inject_mcp: false,
        mcp_profile: None,
        mcp_exe: "herdr2".into(),
        socket: String::new(),
        handshake_timeout: Some(timeout),
    };
    // Keep a small outer safety margin so inner `setup_session` timeouts fire first and preserve
    // the exact stalled phase (`initialize` vs `session/new`) and child stderr.
    let outer_timeout = timeout.saturating_add(Duration::from_secs(5));
    let res = tokio::time::timeout(outer_timeout, spawn_session(spec)).await;

    match res {
        Ok(Ok(started)) => {
            let mut acp_models = Vec::new();
            let mut acp_default = None;
            let mut efforts = Vec::new();
            let mut default_effort = None;
            let mut model_contexts: BTreeMap<String, Vec<String>> = BTreeMap::new();
            let mut model_efforts: BTreeMap<String, Vec<String>> = BTreeMap::new();
            let mut default_model_contexts = Vec::new();

            if matches!(id.as_str(), "cursor" | "cursor-agent")
                && let Ok(extra) =
                    started.transport.request("cursor/list_available_models", &json!({}), Duration::from_secs(4)).await
                && let Some(models) = extra.get("models").and_then(Value::as_array)
            {
                for item in models {
                    let Some(m_id) = item["value"].as_str().map(str::trim).filter(|s| !s.is_empty()) else {
                        continue;
                    };
                    if !m_id.eq_ignore_ascii_case("default") {
                        push_unique(&mut acp_models, m_id);
                    }
                    if let Some(opts) = item.get("configOptions").and_then(Value::as_array) {
                        if let Some(ctx_opt) = opts.iter().find(|o| o["id"].as_str() == Some("context"))
                            && let Some(choices) = ctx_opt.get("options").and_then(Value::as_array)
                        {
                            let mut vals = Vec::new();
                            for ch in choices {
                                if let Some(v) = ch["value"].as_str() {
                                    push_unique(&mut vals, v);
                                }
                            }
                            if !vals.is_empty() {
                                model_contexts.insert(m_id.to_string(), vals);
                            }
                        }
                        if let Some(eff_opt) = opts.iter().find(|o| {
                            let oid = o["id"].as_str().unwrap_or("").to_ascii_lowercase();
                            let ocat = o["category"].as_str().unwrap_or("").to_ascii_lowercase();
                            oid.contains("effort")
                                || oid == "reasoning"
                                || oid == "thinking"
                                || oid == "thought"
                                || oid == "thought_level"
                                || ocat == "thought_level"
                                || ocat == "effort"
                        }) && let Some(choices) = eff_opt.get("options").and_then(Value::as_array)
                        {
                            let mut vals = Vec::new();
                            for ch in choices {
                                if let Some(v) = ch["value"].as_str() {
                                    push_unique(&mut vals, v);
                                }
                            }
                            if !vals.is_empty() {
                                model_efforts.insert(m_id.to_string(), vals);
                            }
                        }
                    }
                }
            }

            for opt in &started.config_options {
                let lower_id = opt.id.to_ascii_lowercase();
                let lower_cat = opt.category.as_deref().unwrap_or("").to_ascii_lowercase();
                if lower_id == "model" || lower_cat == "model" {
                    for choice in &opt.options {
                        push_unique(&mut acp_models, &choice.value);
                    }
                    acp_default = opt.current_value.as_str().map(String::from);
                    if let Some(dm) = &acp_default
                        && !acp_models.contains(dm)
                    {
                        acp_models.insert(0, dm.clone());
                    }
                } else if lower_id.contains("effort")
                    || lower_id == "thought"
                    || lower_cat == "thought_level"
                    || lower_cat == "effort"
                {
                    for choice in &opt.options {
                        push_unique(&mut efforts, &choice.value);
                    }
                    default_effort = opt.current_value.as_str().map(String::from);
                } else if lower_id == "context" {
                    for choice in &opt.options {
                        push_unique(&mut default_model_contexts, &choice.value);
                    }
                }
            }
            if efforts.is_empty() {
                efforts = default_efforts();
                default_effort = Some("medium".into());
            }
            let agent_info = started.agent_info.clone();
            started.transport.kill();
            let _ = tokio::time::timeout(Duration::from_secs(1), started.waiter.wait()).await;

            // Run CLI model catalog discovery AFTER the ACP child has exited so agents like
            // `cursor-agent` never launch two Node processes concurrently competing on compile
            // cache (`NODE_COMPILE_CACHE`), state files, and proxy connections.
            let cli_probe = probe_cli_models(&id, &env, &cwd).await;

            let models = merge_model_lists(&[acp_models, cli_probe.models, local_models]);
            let default_model =
                acp_default.filter(|s| !s.trim().is_empty()).or(local_default).or_else(|| models.first().cloned());
            if !default_model_contexts.is_empty()
                && let Some(ref dm) = default_model
            {
                model_contexts.entry(dm.clone()).or_insert(default_model_contexts);
            }

            for (k, v) in infer_model_efforts_from_variants(&models) {
                model_efforts.entry(k).or_insert(v);
            }

            ProbeAttemptOutcome::Done(ProbedAgent {
                id,
                command: argv,
                installed: true,
                path: Some(exe_path),
                agent_info,
                models,
                default_model,
                efforts,
                default_effort,
                model_contexts,
                model_efforts,
                error: None,
            })
        }
        Ok(Err(err)) if is_handshake_timeout_error(&err.message) => {
            let detail = sanitize_probe_error(&err.message);
            ProbeAttemptOutcome::TimedOut(TimedOutAttempt {
                target: AcpProbeTarget { id, argv, exe_path, env, cwd },
                detail: (!detail.is_empty()).then_some(detail),
                timeout,
            })
        }
        Ok(Err(err)) => {
            // Always surface ACP spawn/handshake failures even when CLI or local model catalogs
            // are still available, so broken/unauthenticated agents are never misreported as healthy.
            let cli_probe = probe_cli_models(&id, &env, &cwd).await;
            let err_msg = format_probe_acp_error(&id, &err.message);
            let err_opt = Some(if err_msg.is_empty() { "ACP probe failed".to_string() } else { err_msg });
            let models = merge_model_lists(&[cli_probe.models, local_models]);
            let model_efforts = infer_model_efforts_from_variants(&models);
            let default_model = local_default.or_else(|| models.first().cloned());
            ProbeAttemptOutcome::Done(ProbedAgent {
                id,
                command: argv,
                installed: true,
                path: Some(exe_path),
                agent_info: None,
                models,
                default_model,
                efforts: default_efforts(),
                default_effort: Some("medium".into()),
                model_contexts: BTreeMap::new(),
                model_efforts,
                error: err_opt,
            })
        }
        Err(_) => ProbeAttemptOutcome::TimedOut(TimedOutAttempt {
            target: AcpProbeTarget { id, argv, exe_path, env, cwd },
            detail: None,
            timeout,
        }),
    }
}

fn infer_model_efforts_from_variants(models: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for m in models {
        let lower = m.trim().to_ascii_lowercase();
        let trimmed = lower.strip_suffix("-fast").unwrap_or(&lower);
        for &(suf, norm) in &[
            ("-extra-high", "xhigh"),
            ("-xhigh", "xhigh"),
            ("-high", "high"),
            ("-medium", "medium"),
            ("-med", "medium"),
            ("-low", "low"),
        ] {
            if let Some(base) = trimmed.strip_suffix(suf)
                && !base.is_empty()
            {
                let entry = map.entry(base.to_string()).or_default();
                push_unique(entry, norm);
                break;
            }
        }
    }
    map.retain(|_, v| v.len() >= 2);
    map
}

#[derive(Debug, Default)]
struct CliModelsProbe {
    models: Vec<String>,
    error: Option<String>,
}

fn is_probe_auth_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("authentication required")
        || lower.contains("auth_required")
        || lower.contains("configure an api key")
        || lower.contains("onboarding is incomplete")
        || lower.contains("please run /login")
        || lower.contains("not logged in")
        || lower.contains("login required")
        || lower.contains("please log in")
        || lower.contains("please login")
        || lower.contains("sign in required")
        || lower.contains("session expired")
        || lower.contains("unauthenticated")
        || lower.contains("unauthorized")
        || (lower.contains("-32000") && (lower.contains("auth") || lower.contains("login") || lower.contains("api key")))
}

fn strip_raw_auth_methods_noise(msg: &str) -> String {
    let without_sdk_stderr = if let Some((head, tail)) = msg.split_once("\nstderr:\n")
        && tail.trim_start().starts_with("Error handling request")
    {
        head.trim()
    } else {
        msg.trim()
    };
    if let Some((prefix, rest)) = without_sdk_stderr.split_once(": {\"authMethods\":") {
        let code_suffix = rest.rfind("(code ").map(|idx| format!(" {}", &rest[idx..])).unwrap_or_default();
        return format!("{}{code_suffix}", prefix.trim());
    }
    without_sdk_stderr.to_string()
}

fn format_probe_acp_error(agent_id: &str, raw: &str) -> String {
    let pre_trimmed = raw.trim();
    if pre_trimmed.is_empty() {
        return String::new();
    }
    if !is_probe_auth_error(pre_trimmed) {
        return sanitize_probe_error(pre_trimmed);
    }
    if agent_id.eq_ignore_ascii_case("pi") {
        let env_note = if cfg!(windows) {
            "; Windows API key env vars must be set in User/System environment, not only PowerShell $PROFILE"
        } else {
            ""
        };
        return format!(
            "Authentication required: Pi has no configured API key or OAuth login. Run `pi` and execute `/login` (or configure ~/.pi/agent/models.json{env_note})."
        );
    }
    sanitize_probe_error(&strip_raw_auth_methods_noise(pre_trimmed))
}

fn sanitize_probe_error(raw: &str) -> String {
    let cleaned = strip_ansi(raw);
    let mut lines: Vec<&str> = Vec::new();
    for part in cleaned.split(['\n', '\r']) {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.chars().next().is_some_and(|c| ('\u{2800}'..='\u{28FF}').contains(&c)) {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("fetching ") || lower.starts_with("loading ") {
            continue;
        }
        lines.push(trimmed);
    }
    let joined = lines.join("\n");
    const MAX_LEN: usize = 600;
    if joined.chars().count() <= MAX_LEN {
        joined
    } else {
        let prefix: String = joined.chars().take(MAX_LEN).collect();
        format!("{}…", prefix.trim_end())
    }
}

fn extract_cli_output_snippet(stderr: &[u8], stdout: &[u8]) -> Option<String> {
    for bytes in [stderr, stdout] {
        let raw = String::from_utf8_lossy(bytes);
        let cleaned = sanitize_probe_error(&raw);
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    None
}

fn summarize_cli_failure(cmd_label: &str, status: std::process::ExitStatus, stderr: &[u8], stdout: &[u8]) -> String {
    let status_desc = match status.code() {
        Some(code) => format!("exited with code {code}"),
        None => "terminated by signal".to_string(),
    };
    match extract_cli_output_snippet(stderr, stdout) {
        Some(detail) => format!("`{cmd_label}` {status_desc}\nstderr:\n{detail}"),
        None => format!("`{cmd_label}` {status_desc}"),
    }
}

async fn probe_cli_version(program: &str, env: &BTreeMap<String, String>, cwd: &str) -> Result<Option<String>, String> {
    let argv = vec![program.to_string(), "--version".to_string()];
    let mut cmd = crate::platform::agent_command(&argv, env, cwd)
        .map_err(|e| format!("failed to prepare `{program} --version`: {e}"))?;
    cmd.stdin(std::process::Stdio::null());
    let out = match tokio::time::timeout(CLI_PROBE_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("failed to run `{program} --version`: {e}")),
        Err(_) => return Err(format!("`{program} --version` timed out after {}s", CLI_PROBE_TIMEOUT.as_secs())),
    };
    if !out.status.success() {
        return Err(summarize_cli_failure(&format!("{program} --version"), out.status, &out.stderr, &out.stdout));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = strip_ansi(&text).lines().next().unwrap_or("").trim().to_string();
    Ok((!line.is_empty()).then_some(line))
}

fn push_unique(dest: &mut Vec<String>, item: &str) {
    let trimmed = item.trim();
    if !trimmed.is_empty() && !dest.iter().any(|existing| existing == trimmed) {
        dest.push(trimmed.to_string());
    }
}

fn merge_model_lists(lists: &[Vec<String>]) -> Vec<String> {
    let mut out = Vec::new();
    for list in lists {
        for item in list {
            push_unique(&mut out, item);
        }
    }
    out
}

fn default_efforts() -> Vec<String> {
    vec!["low".into(), "medium".into(), "high".into(), "xhigh".into()]
}

async fn probe_cli_models(agent_id: &str, env: &BTreeMap<String, String>, cwd: &str) -> CliModelsProbe {
    let cli_argv: Vec<String> = match agent_id {
        "agy" | "antigravity" => vec!["agy".into(), "models".into()],
        "cursor" | "cursor-agent" => vec!["cursor-agent".into(), "--list-models".into()],
        "opencode" => vec!["opencode".into(), "models".into()],
        _ => return CliModelsProbe::default(),
    };
    let label = cli_argv.join(" ");
    let mut cmd = match crate::platform::agent_command(&cli_argv, env, cwd) {
        Ok(c) => c,
        Err(e) => return CliModelsProbe { models: Vec::new(), error: Some(format!("failed to prepare `{label}`: {e}")) },
    };
    cmd.stdin(std::process::Stdio::null());
    let out = match tokio::time::timeout(CLI_PROBE_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return CliModelsProbe { models: Vec::new(), error: Some(format!("failed to run `{label}`: {e}")) },
        Err(_) => {
            return CliModelsProbe {
                models: Vec::new(),
                error: Some(format!("`{label}` timed out after {}s", CLI_PROBE_TIMEOUT.as_secs())),
            };
        }
    };
    if !out.status.success() {
        return CliModelsProbe {
            models: Vec::new(),
            error: Some(summarize_cli_failure(&label, out.status, &out.stderr, &out.stdout)),
        };
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let models = parse_cli_models(agent_id, &text);
    if models.is_empty() && matches!(agent_id, "agy" | "antigravity") {
        let error = match extract_cli_output_snippet(&out.stderr, &out.stdout) {
            Some(detail) => format!("`{label}` returned no models\nstderr:\n{detail}"),
            None => format!("`{label}` returned no models"),
        };
        return CliModelsProbe { models, error: Some(error) };
    }
    CliModelsProbe { models, error: None }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    while let Some(&nc) = chars.peek() {
                        chars.next();
                        if ('@'..='~').contains(&nc) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(nc) = chars.next() {
                        if nc == '\x07' {
                            break;
                        }
                        if nc == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub(crate) fn parse_cli_models(agent_id: &str, text: &str) -> Vec<String> {
    let cleaned = strip_ansi(text);
    let mut models = Vec::new();
    for raw_line in cleaned.split(['\n', '\r']) {
        let line = raw_line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with("──")
            || line.chars().next().is_some_and(|c| ('\u{2800}'..='\u{28FF}').contains(&c))
        {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("fetching ") || lower.starts_with("loading ") || lower.starts_with("available ") {
            continue;
        }
        let id = match agent_id {
            "agy" | "antigravity" => {
                // `<id>\t<label>` or `<id>  <label>`
                let cand = line.split('\t').next().unwrap_or(line).split_whitespace().next().unwrap_or("");
                if cand.eq_ignore_ascii_case("id")
                    || cand.eq_ignore_ascii_case("model")
                    || !cand.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
                {
                    ""
                } else {
                    cand
                }
            }
            "cursor" | "cursor-agent" => {
                // `<id> - <display name>`
                line.split(" - ").next().unwrap_or(line).split_whitespace().next().unwrap_or("")
            }
            "opencode" => {
                let cand = line.split_whitespace().next().unwrap_or("");
                if cand.contains('/') { cand } else { "" }
            }
            _ => "",
        };
        if !id.is_empty() && id.len() <= 120 && !id.starts_with('[') && !id.starts_with('─') {
            push_unique(&mut models, id);
        }
    }
    models
}

fn user_home_dirs(env: &BTreeMap<String, String>) -> Vec<PathBuf> {
    let mut homes = Vec::new();
    let candidates = if cfg!(windows) {
        [
            env_ignore_case(env, "USERPROFILE").map(String::from),
            env_ignore_case(env, "HOME").map(String::from),
            std::env::var("USERPROFILE").ok(),
            std::env::var("HOME").ok(),
        ]
    } else {
        [
            env_ignore_case(env, "HOME").map(String::from),
            env_ignore_case(env, "USERPROFILE").map(String::from),
            std::env::var("HOME").ok(),
            std::env::var("USERPROFILE").ok(),
        ]
    };
    for val in candidates.into_iter().flatten() {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            let pb = PathBuf::from(trimmed);
            if !homes.contains(&pb) {
                homes.push(pb);
            }
        }
    }
    homes
}

fn collect_catalog_models(path: &Path, dest: &mut Vec<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(val) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    if let Some(arr) = val.as_array() {
        for entry in arr {
            if let Some(id) = entry.as_str().or_else(|| entry["id"].as_str()) {
                push_unique(dest, id);
            }
        }
        return;
    }
    for key in ["models", "availableModels"] {
        if let Some(arr) = val[key].as_array() {
            for entry in arr {
                if let Some(id) = entry.as_str().or_else(|| entry["id"].as_str()).or_else(|| entry["slug"].as_str()) {
                    push_unique(dest, id);
                }
            }
        }
    }
}

fn normalize_agy_model_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.contains([' ', '(', ')']) {
        return Some(trimmed.to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower == "orchestrator (carbon)" {
        return Some("gemini-pro-orchestrator".into());
    }
    if lower == "orchestrator (argon)" {
        return Some("gemini-pro-orchestrator-argon".into());
    }
    let replaced = lower.replace("5.5", "5-5");
    let mut slug = String::with_capacity(replaced.len());
    let mut prev_dash = false;
    for ch in replaced.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' {
            slug.push(ch);
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
    }
    let cleaned = slug.trim_end_matches('-').to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

fn push_semicolon_cap_models(raw: &str, dest: &mut Vec<String>) {
    for part in raw.split(';') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let model = trimmed.split('=').next().unwrap_or("").trim();
        if !model.is_empty() {
            push_unique(dest, model);
        }
    }
}

fn expand_tilde_in_homes(raw: &str, homes: &[PathBuf]) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed == "~"
        && let Some(home) = homes.first()
    {
        return home.clone();
    }
    if let Some(rest) = trimmed.strip_prefix("~/").or_else(|| trimmed.strip_prefix(r"~\"))
        && let Some(home) = homes.first()
    {
        return home.join(rest);
    }
    PathBuf::from(trimmed)
}

fn collect_pi_agent_dir_models(agent_dir: &Path, models: &mut Vec<String>, default_model: &mut Option<String>) {
    if let Ok(text) = std::fs::read_to_string(agent_dir.join("settings.json"))
        && let Ok(val) = serde_json::from_str::<Value>(&text)
    {
        let prov = val["defaultProvider"].as_str().map(str::trim).filter(|s| !s.is_empty());
        let dm = val["defaultModel"].as_str().map(str::trim).filter(|s| !s.is_empty());
        if let Some(m) = dm {
            let qualified = if m.contains('/') {
                m.to_string()
            } else if let Some(p) = prov {
                format!("{p}/{m}")
            } else {
                m.to_string()
            };
            default_model.get_or_insert_with(|| qualified.clone());
            push_unique(models, &qualified);
        }
        if let Some(arr) = val["enabledModels"].as_array() {
            for item in arr {
                if let Some(m) = item.as_str().map(str::trim).filter(|s| !s.is_empty())
                    && !m.contains(['*', '?', '[', '{'])
                {
                    push_unique(models, m);
                }
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(agent_dir.join("models.json"))
        && let Ok(val) = serde_json::from_str::<Value>(&text)
        && let Some(providers) = val["providers"].as_object()
    {
        for (pname, pval) in providers {
            let pname = pname.trim();
            if pname.is_empty() {
                continue;
            }
            if let Some(arr) = pval["models"].as_array() {
                for entry in arr {
                    if let Some(mid) =
                        entry["id"].as_str().or_else(|| entry.as_str()).map(str::trim).filter(|s| !s.is_empty())
                    {
                        let qualified = if mid.contains('/') { mid.to_string() } else { format!("{pname}/{mid}") };
                        push_unique(models, &qualified);
                    }
                }
            } else if let Some(obj) = pval["models"].as_object() {
                for mkey in obj.keys() {
                    let mid = mkey.trim();
                    if !mid.is_empty() {
                        let qualified = if mid.contains('/') { mid.to_string() } else { format!("{pname}/{mid}") };
                        push_unique(models, &qualified);
                    }
                }
            }
        }
    }
}

pub(crate) fn discover_local_models(
    agent_id: &str,
    installed: bool,
    env: &BTreeMap<String, String>,
) -> (Vec<String>, Option<String>) {
    let homes = user_home_dirs(env);
    if agent_id == "pi"
        && let Some(custom_dir) = env_ignore_case(env, "PI_CODING_AGENT_DIR")
            .map(String::from)
            .or_else(|| std::env::var("PI_CODING_AGENT_DIR").ok().filter(|s| !s.trim().is_empty()))
    {
        let mut models = Vec::new();
        let mut default_model = None;
        collect_pi_agent_dir_models(&expand_tilde_in_homes(&custom_dir, &homes), &mut models, &mut default_model);
        let (home_models, home_default) = discover_local_models_in_homes(agent_id, installed, &homes);
        let merged = merge_model_lists(&[models, home_models]);
        let dm = default_model.or(home_default).or_else(|| merged.first().cloned());
        return (merged, dm);
    }
    discover_local_models_in_homes(agent_id, installed, &homes)
}

pub(crate) fn discover_local_models_in_homes(
    agent_id: &str,
    _installed: bool,
    homes: &[PathBuf],
) -> (Vec<String>, Option<String>) {
    let mut models = Vec::new();
    let mut default_model: Option<String> = None;

    match agent_id {
        "claude" => {
            for home in homes {
                collect_catalog_models(&home.join(".orca-claude/agent-model-catalog.json"), &mut models);
                collect_catalog_models(&home.join(".orca/agent-model-catalog.json"), &mut models);
                if let Ok(text) = std::fs::read_to_string(home.join(".cache/claude-magpie/picker-settings.json"))
                    && let Ok(val) = serde_json::from_str::<Value>(&text)
                {
                    if let Some(m) = val["env"]["ANTHROPIC_MODEL"].as_str().filter(|s| !s.is_empty()) {
                        default_model.get_or_insert_with(|| m.to_string());
                        push_unique(&mut models, m);
                    }
                    for key in [
                        "ANTHROPIC_DEFAULT_OPUS_MODEL",
                        "ANTHROPIC_DEFAULT_SONNET_MODEL",
                        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                        "ANTHROPIC_DEFAULT_FABLE_MODEL",
                        "ANTHROPIC_SMALL_FAST_MODEL",
                    ] {
                        if let Some(m) = val["env"][key].as_str().filter(|s| !s.is_empty()) {
                            push_unique(&mut models, m);
                        }
                    }
                    if let Some(caps) = val["env"]["CLAUDE_CODE_MODEL_CAPABILITIES"].as_str() {
                        push_semicolon_cap_models(caps, &mut models);
                    }
                    if let Some(arr) = val["modelPicker"]["options"].as_array() {
                        for entry in arr {
                            if let Some(id) = entry["model"]
                                .as_str()
                                .or_else(|| entry["id"].as_str())
                                .or_else(|| entry["value"].as_str())
                                .or_else(|| entry.as_str())
                            {
                                push_unique(&mut models, id);
                            }
                        }
                    }
                    if let Some(arr) = val["availableModels"].as_array() {
                        for entry in arr {
                            if let Some(id) =
                                entry.as_str().or_else(|| entry["id"].as_str()).or_else(|| entry["model"].as_str())
                            {
                                push_unique(&mut models, id);
                            }
                        }
                    }
                }
                if let Ok(text) = std::fs::read_to_string(home.join(".cache/claude-magpie/capabilities")) {
                    push_semicolon_cap_models(&text, &mut models);
                }
                if let Ok(text) = std::fs::read_to_string(home.join(".claude/settings.json"))
                    && let Ok(val) = serde_json::from_str::<Value>(&text)
                {
                    if let Some(m) = val["model"]
                        .as_str()
                        .or_else(|| val["env"]["ANTHROPIC_MODEL"].as_str())
                        .filter(|s| !s.is_empty())
                    {
                        default_model.get_or_insert_with(|| m.to_string());
                        push_unique(&mut models, m);
                    }
                    for key in [
                        "ANTHROPIC_DEFAULT_OPUS_MODEL",
                        "ANTHROPIC_DEFAULT_SONNET_MODEL",
                        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                        "ANTHROPIC_DEFAULT_FABLE_MODEL",
                        "ANTHROPIC_SMALL_FAST_MODEL",
                    ] {
                        if let Some(m) = val["env"][key].as_str().filter(|s| !s.is_empty()) {
                            push_unique(&mut models, m);
                        }
                    }
                    if let Some(caps) = val["env"]["CLAUDE_CODE_MODEL_CAPABILITIES"].as_str() {
                        push_semicolon_cap_models(caps, &mut models);
                    }
                    if let Some(arr) = val["availableModels"].as_array() {
                        for entry in arr {
                            if let Some(id) =
                                entry.as_str().or_else(|| entry["id"].as_str()).or_else(|| entry["model"].as_str())
                            {
                                push_unique(&mut models, id);
                            }
                        }
                    }
                }
            }
            for curated in [
                "default",
                "opus",
                "sonnet",
                "haiku",
                "opusplan",
                "opus[1m]",
                "sonnet[1m]",
                "claude-opus-4-7",
                "claude-opus-4-7[1m]",
                "claude-opus-4-6",
                "claude-opus-4-6[1m]",
                "claude-sonnet-4-6",
                "claude-sonnet-4-6[1m]",
                "claude-sonnet-4-5",
                "claude-haiku-4-5",
            ] {
                push_unique(&mut models, curated);
            }
        }
        "codex" => {
            for home in homes {
                collect_catalog_models(&home.join(".orca-codex/agent-model-catalog.json"), &mut models);
                if let Ok(text) = std::fs::read_to_string(home.join(".codex/models_cache.json"))
                    && let Ok(val) = serde_json::from_str::<Value>(&text)
                    && let Some(arr) = val["models"].as_array()
                {
                    for entry in arr {
                        if let Some(id) = entry["slug"].as_str().or_else(|| entry["id"].as_str()) {
                            push_unique(&mut models, id);
                        }
                    }
                }
                if let Ok(text) = std::fs::read_to_string(home.join(".codex/config.toml"))
                    && let Ok(val) = toml::from_str::<toml::Value>(&text)
                    && let Some(m) = val.get("model").and_then(toml::Value::as_str).filter(|s| !s.is_empty())
                {
                    default_model.get_or_insert_with(|| m.to_string());
                    push_unique(&mut models, m);
                }
            }
            for curated in ["gpt-5.4", "gpt-5.3-codex", "gpt-5.2-codex", "o3"] {
                push_unique(&mut models, curated);
            }
        }
        "agy" | "antigravity" => {
            for home in homes {
                if let Ok(text) = std::fs::read_to_string(home.join(".gemini/antigravity-cli/settings.json"))
                    && let Ok(val) = serde_json::from_str::<Value>(&text)
                    && let Some(raw_m) =
                        val["model"].as_str().or_else(|| val["model"]["name"].as_str()).filter(|s| !s.is_empty())
                    && let Some(norm) = normalize_agy_model_id(raw_m)
                {
                    default_model.get_or_insert_with(|| norm.clone());
                    push_unique(&mut models, &norm);
                }
            }
            for curated in [
                "gemini-4-carbon-high",
                "gemini-4-carbon-medium",
                "gemini-4-carbon-low",
                "gemini-4-argon-high",
                "gemini-4-argon-medium",
                "gemini-4-argon-low",
                "gemini-4-hydrogen-xhigh",
                "gemini-4-hydrogen-high",
                "gemini-4-hydrogen-medium",
                "gemini-4-hydrogen-low",
                "gemini-pro-orchestrator",
                "gemini-pro-orchestrator-argon",
                "gemini-3.8-flash-high",
                "gemini-3.8-flash-medium",
                "gemini-3.8-flash-low",
                "claude-opus-5-5-high",
                "claude-opus-5-5-medium",
                "claude-opus-5-5-low",
                "claude-sonnet-5-5-high",
                "claude-sonnet-5-5-medium",
                "claude-sonnet-5-5-low",
                "gpt-oss-120b-medium",
            ] {
                push_unique(&mut models, curated);
            }
        }
        "cursor" | "cursor-agent" => {
            for home in homes {
                if let Ok(text) = std::fs::read_to_string(home.join(".cursor/cli-config.json"))
                    && let Ok(val) = serde_json::from_str::<Value>(&text)
                {
                    let m =
                        val["model"]["modelId"].as_str().or_else(|| val["model"].as_str()).filter(|s| !s.is_empty());
                    if let Some(m) = m {
                        default_model.get_or_insert_with(|| m.to_string());
                        push_unique(&mut models, m);
                    }
                }
            }
            for curated in ["auto", "claude-4.6-sonnet-thinking", "gpt-5.4", "gemini-2.5-pro"] {
                push_unique(&mut models, curated);
            }
        }
        "opencode" => {
            for home in homes {
                if let Ok(text) = std::fs::read_to_string(home.join(".config/opencode/opencode.json"))
                    && let Ok(val) = serde_json::from_str::<Value>(&text)
                {
                    if let Some(m) = val["model"].as_str().filter(|s| !s.is_empty()) {
                        default_model.get_or_insert_with(|| m.to_string());
                        push_unique(&mut models, m);
                    }
                    if let Some(providers) = val["provider"].as_object() {
                        for (pname, pval) in providers {
                            if let Some(pmodels) = pval["models"].as_object() {
                                for mkey in pmodels.keys() {
                                    push_unique(&mut models, &format!("{pname}/{mkey}"));
                                }
                            }
                        }
                    }
                }
            }
        }
        "pi" => {
            for home in homes {
                collect_pi_agent_dir_models(&home.join(".pi/agent"), &mut models, &mut default_model);
            }
        }
        "kimi" => {
            for home in homes {
                for rel in [".kimi/config.toml", ".kimi-code/config.toml"] {
                    if let Ok(text) = std::fs::read_to_string(home.join(rel))
                        && let Ok(val) = toml::from_str::<toml::Value>(&text)
                    {
                        if let Some(m) =
                            val.get("default_model").and_then(toml::Value::as_str).filter(|s| !s.is_empty())
                        {
                            default_model.get_or_insert_with(|| m.to_string());
                            push_unique(&mut models, m);
                        }
                        if let Some(tbl) = val.get("models").and_then(toml::Value::as_table) {
                            for k in tbl.keys() {
                                push_unique(&mut models, k);
                            }
                        }
                    }
                }
            }
            for curated in ["kimi-k2.5", "kimi-for-coding"] {
                push_unique(&mut models, curated);
            }
        }
        "qwen" => {
            for home in homes {
                if let Ok(text) = std::fs::read_to_string(home.join(".qwen/settings.json"))
                    && let Ok(val) = serde_json::from_str::<Value>(&text)
                    && let Some(m) = val["model"].as_str().filter(|s| !s.is_empty())
                {
                    default_model.get_or_insert_with(|| m.to_string());
                    push_unique(&mut models, m);
                }
            }
            for curated in ["qwen3-coder-plus", "qwen-max"] {
                push_unique(&mut models, curated);
            }
        }
        "dsh" => {
            for home in homes {
                if let Ok(text) = std::fs::read_to_string(home.join(".config/dsh/config.toml"))
                    && let Ok(val) = toml::from_str::<toml::Value>(&text)
                    && let Some(m) = val.get("model").and_then(toml::Value::as_str).filter(|s| !s.is_empty())
                {
                    default_model.get_or_insert_with(|| m.to_string());
                    push_unique(&mut models, m);
                }
            }
            for curated in ["deepseek-chat", "deepseek-reasoner"] {
                push_unique(&mut models, curated);
            }
        }
        "zcode" | "glm" => {
            for home in homes {
                if let Ok(text) = std::fs::read_to_string(home.join(".zcode/v2/provider_config.json"))
                    && let Ok(val) = serde_json::from_str::<Value>(&text)
                {
                    let cfg = &val["config"];
                    if let Some(dm) = cfg["defaultModelSelection"]["modelId"].as_str().filter(|s| !s.is_empty()) {
                        default_model.get_or_insert_with(|| dm.to_string());
                        push_unique(&mut models, dm);
                    }
                    if let Some(rules) = cfg["providerConfigRules"]["providerRules"].as_array() {
                        for rule in rules {
                            for field in ["modelOrder", "personalModelIds", "builtinModelIds"] {
                                if let Some(arr) = rule["config"][field].as_array() {
                                    for item in arr {
                                        if let Some(m) = item.as_str().filter(|s| !s.is_empty()) {
                                            push_unique(&mut models, m);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    for key in ["providerModelRules", "manualProviderModelRules"] {
                        if let Some(arr) = cfg["modelConfigRules"][key].as_array() {
                            for rule in arr {
                                if rule["config"]["enabled"].as_bool() == Some(false) {
                                    continue;
                                }
                                if let Some(m) = rule["modelId"].as_str().filter(|s| !s.is_empty()) {
                                    push_unique(&mut models, m);
                                }
                            }
                        }
                    }
                }
                if let Ok(text) =
                    std::fs::read_to_string(home.join(".zcode/runtime/current/agent/provider/zcode-builtin.json"))
                    && let Ok(val) = serde_json::from_str::<Value>(&text)
                {
                    let cfg = &val["config"];
                    if let Some(rules) = cfg["providerConfigRules"]["providerRules"].as_array() {
                        for rule in rules {
                            if rule["providerId"].as_str() == Some("account:bigmodel-individual-coding-plan")
                                && let Some(arr) = rule["config"]["builtinModelIds"].as_array()
                            {
                                for item in arr {
                                    if let Some(m) = item.as_str().filter(|s| !s.is_empty()) {
                                        push_unique(&mut models, m);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(arr) = cfg["modelConfigRules"]["builtinProviderModelRules"].as_array() {
                        for rule in arr {
                            if rule["providerId"].as_str() == Some("account:bigmodel-individual-coding-plan")
                                && rule["config"]["enabled"].as_bool() != Some(false)
                                && let Some(m) = rule["modelId"].as_str().filter(|s| !s.is_empty())
                            {
                                push_unique(&mut models, m);
                            }
                        }
                    }
                }
            }
            for curated in ["GLM-5.4", "GLM-5.3", "GLM-5.3-Flash", "GLM-5.2", "GLM-5-Turbo"] {
                push_unique(&mut models, curated);
            }
        }
        _ => {}
    }

    if default_model.is_none() {
        default_model = models.first().cloned();
    }
    (models, default_model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_model_ids_parses_openai_and_models_shapes() {
        let openai = r#"{"object":"list","data":[{"id":"gpt-4o-mini"},{"id":"deepseek-chat"},{"id":"gpt-4o-mini"}]}"#;
        assert_eq!(all_model_ids(openai), vec!["gpt-4o-mini", "deepseek-chat"]);

        let alt = r#"{"models":[{"id":"m1"},"m2",{"id":""}]}"#;
        assert_eq!(all_model_ids(alt), vec!["m1", "m2"]);
    }

    #[test]
    fn discover_local_models_reads_claude_and_codex_configs() {
        let tmp = crate::test_tmp().join(format!("drove-probe-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(tmp.join(".claude")).unwrap();
        std::fs::write(
            tmp.join(".claude/settings.json"),
            r#"{"model":"claude-opus-4-6","availableModels":["claude-sonnet-4-6","custom-proxy"]}"#,
        )
        .unwrap();
        let (models, def) = discover_local_models_in_homes("claude", true, std::slice::from_ref(&tmp));
        assert_eq!(def.as_deref(), Some("claude-opus-4-6"));
        assert!(models.contains(&"custom-proxy".to_string()));
        assert!(models.contains(&"claude-sonnet-4-6".to_string()));

        std::fs::create_dir_all(tmp.join(".codex")).unwrap();
        std::fs::write(tmp.join(".codex/config.toml"), "model = \"gpt-5.4-pro\"\n").unwrap();
        std::fs::write(
            tmp.join(".codex/models_cache.json"),
            r#"{"models":[{"slug":"gpt-5.3-codex"},{"slug":"gpt-5.4"}]}"#,
        )
        .unwrap();
        let (codex_models, codex_def) = discover_local_models_in_homes("codex", true, &[tmp]);
        assert_eq!(codex_def.as_deref(), Some("gpt-5.4-pro"));
        assert!(codex_models.contains(&"gpt-5.3-codex".to_string()));
        assert!(codex_models.contains(&"gpt-5.4-pro".to_string()));
    }

    #[test]
    fn parse_cli_models_handles_agy_carriage_return_spinner_and_ansi() {
        let raw = "\r\x1b[2K⠋ Fetching available models...\r\x1b[2K⠙ Fetching available models...\rgemini-4-carbon-medium         Gemini 4 Carbon (Medium)\ngemini-4-carbon-high           Gemini 4 Carbon (High)\nclaude-opus-5-5-high           Claude Opus 5.5 (High)\n";
        assert_eq!(
            parse_cli_models("agy", raw),
            vec!["gemini-4-carbon-medium", "gemini-4-carbon-high", "claude-opus-5-5-high"]
        );
    }

    #[test]
    fn discover_local_models_reads_magpie_picker_caps_and_agy_settings() {
        let tmp = crate::test_tmp().join(format!("drove-magpie-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(tmp.join(".cache/claude-magpie")).unwrap();
        std::fs::create_dir_all(tmp.join(".claude")).unwrap();
        std::fs::create_dir_all(tmp.join(".gemini/antigravity-cli")).unwrap();

        std::fs::write(
            tmp.join(".cache/claude-magpie/picker-settings.json"),
            r#"{"modelPicker":{"options":[{"model":"group/deepseek-v4.1-flash[1m]"},{"model":"zcode/GLM-5.3[1m]"}]}}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.join(".cache/claude-magpie/capabilities"),
            "codex/gpt-6.1-sol=1m;agy-cli/gemini-4-carbon[1m]=1m",
        )
        .unwrap();
        std::fs::write(
            tmp.join(".claude/settings.json"),
            r#"{"env":{"ANTHROPIC_DEFAULT_OPUS_MODEL":"zcode/GLM-5.3[1m]","CLAUDE_CODE_MODEL_CAPABILITIES":"cursor/grok-4.7@500k=500k"}}"#,
        )
        .unwrap();
        std::fs::write(tmp.join(".gemini/antigravity-cli/settings.json"), r#"{"model":"gemini-4-hydrogen-high"}"#)
            .unwrap();

        let (claude_models, _) = discover_local_models_in_homes("claude", true, std::slice::from_ref(&tmp));
        for expected in [
            "group/deepseek-v4.1-flash[1m]",
            "zcode/GLM-5.3[1m]",
            "codex/gpt-6.1-sol",
            "agy-cli/gemini-4-carbon[1m]",
            "cursor/grok-4.7@500k",
            "opusplan",
            "claude-opus-4-7",
        ] {
            assert!(claude_models.contains(&expected.to_string()), "missing {expected} in {claude_models:?}");
        }

        let (agy_models, agy_def) = discover_local_models_in_homes("agy", false, std::slice::from_ref(&tmp));
        assert_eq!(agy_def.as_deref(), Some("gemini-4-hydrogen-high"));
        assert!(agy_models.contains(&"gemini-4-carbon-medium".to_string()));
        assert!(agy_models.contains(&"claude-opus-5-5-high".to_string()));
        assert_eq!(normalize_agy_model_id("Gemini 4 Carbon (Medium)").as_deref(), Some("gemini-4-carbon-medium"));
        assert_eq!(normalize_agy_model_id("Orchestrator (Carbon)").as_deref(), Some("gemini-pro-orchestrator"));
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn discover_local_models_reads_zcode_provider_and_builtin_configs() {
        let tmp = crate::test_tmp().join(format!("drove-zcode-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(tmp.join(".zcode/v2")).unwrap();
        std::fs::create_dir_all(tmp.join(".zcode/runtime/current/agent/provider")).unwrap();
        std::fs::write(
            tmp.join(".zcode/v2/provider_config.json"),
            r#"{
                "config": {
                    "defaultModelSelection": { "providerId": "account:bigmodel-individual-coding-plan", "modelId": "GLM-5.3-Flash" },
                    "providerConfigRules": {
                        "providerRules": [
                            { "providerId": "account:bigmodel-individual-coding-plan", "config": { "personalModelIds": ["GLM-5.4"], "modelOrder": ["GLM-5.3", "GLM-5.3-Flash", "GLM-5.4"] } }
                        ]
                    },
                    "modelConfigRules": {
                        "providerModelRules": [
                            { "providerId": "account:bigmodel-individual-coding-plan", "modelId": "GLM-5.4", "config": { "enabled": true } }
                        ]
                    }
                }
            }"#,
        )
        .unwrap();
        std::fs::write(
            tmp.join(".zcode/runtime/current/agent/provider/zcode-builtin.json"),
            r#"{
                "config": {
                    "providerConfigRules": {
                        "providerRules": [
                            { "providerId": "account:bigmodel-individual-coding-plan", "config": { "builtinModelIds": ["GLM-5.3", "GLM-5.3-Flash"] } }
                        ]
                    },
                    "modelConfigRules": {
                        "builtinProviderModelRules": [
                            { "providerId": "account:bigmodel-individual-coding-plan", "modelId": "GLM-5.2", "config": { "enabled": true } },
                            { "providerId": "account:bigmodel-individual-coding-plan", "modelId": "GLM-5-Turbo", "config": { "enabled": true } }
                        ]
                    }
                }
            }"#,
        )
        .unwrap();

        let (models, def) = discover_local_models_in_homes("zcode", false, std::slice::from_ref(&tmp));
        assert_eq!(def.as_deref(), Some("GLM-5.3-Flash"));
        for expected in ["GLM-5.3-Flash", "GLM-5.3", "GLM-5.4", "GLM-5.2", "GLM-5-Turbo"] {
            assert!(models.contains(&expected.to_string()), "missing {expected} in {models:?}");
        }
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn sanitize_probe_error_strips_ansi_and_spinner_noise() {
        let raw = "\r\x1b[2K⠋ Fetching available models...\r\x1b[31mError: authentication expired\x1b[0m\n\nPlease run login again";
        assert_eq!(sanitize_probe_error(raw), "Error: authentication expired\nPlease run login again");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_one_acp_reports_error_when_agent_exits_nonzero_even_with_local_models() {
        let tmp = crate::test_tmp().join(format!("drove-probe-err-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(tmp.join(".claude")).unwrap();
        std::fs::write(tmp.join(".claude/settings.json"), r#"{"model":"claude-opus-4-6"}"#).unwrap();

        let mut env = BTreeMap::new();
        env.insert("HOME".into(), tmp.display().to_string());
        let probed = probe_one_acp(
            "claude".into(),
            vec!["/bin/sh".into(), "-c".into(), "echo 'Error: Not logged in (401)' >&2; exit 1".into()],
            "/bin/sh".into(),
            env,
            tmp.display().to_string(),
        )
        .await;

        assert!(probed.installed);
        assert!(probed.models.contains(&"claude-opus-4-6".to_string()));
        let err = probed.error.as_deref().expect("expected non-empty probe error");
        assert!(err.contains("Not logged in (401)"), "unexpected error: {err}");
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_cli_models_reports_error_on_nonzero_agy_exit() {
        let tmp = crate::test_tmp().join(format!("drove-agy-err-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&tmp).unwrap();
        let fake_agy = tmp.join("agy");
        std::fs::write(&fake_agy, "#!/bin/sh\necho 'Auth token invalid' >&2\nexit 2\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_agy, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut env = BTreeMap::new();
        env.insert("PATH".into(), format!("{}:/usr/bin:/bin", tmp.display()));
        env.insert("HOME".into(), tmp.display().to_string());

        let probed = probe_one_acp(
            "agy".into(),
            vec![fake_agy.display().to_string(), "acp".into()],
            fake_agy.display().to_string(),
            env,
            tmp.display().to_string(),
        )
        .await;

        assert!(probed.installed);
        let err = probed.error.as_deref().expect("expected agy probe error");
        assert!(err.contains("Auth token invalid"), "unexpected agy error: {err}");
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[cfg(unix)]
    fn write_exec_script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_acp_targets_retries_once_on_timeout_and_recovers() {
        let tmp = crate::test_tmp().join(format!("drove-probe-retry-ok-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&tmp).unwrap();
        let marker = tmp.join("attempts");
        let fake_agent = tmp.join("fake-acp");
        write_exec_script(
            &fake_agent,
            &format!(
                r#"#!/bin/sh
MARKER="{marker}"
if [ ! -f "$MARKER" ]; then
  echo 1 > "$MARKER"
  sleep 5
  exit 0
fi
echo 2 >> "$MARKER"
while IFS= read -r line; do
  case "$line" in
    *'"initialize"'*)
      printf '%s\n' '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":1,"agentInfo":{{"name":"FakeCursor","version":"1.2.3"}},"agentCapabilities":{{}}}}}}'
      ;;
    *'"session/new"'*)
      printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{{"sessionId":"s-1","configOptions":[{{"id":"model","category":"model","currentValue":"m-fast","options":[{{"value":"m-fast"}}]}}]}}}}'
      ;;
  esac
done
"#,
                marker = marker.display()
            ),
        );

        let mut env = BTreeMap::new();
        env.insert("HOME".into(), tmp.display().to_string());
        let policy = ProbePolicy {
            concurrency: 2,
            retry_concurrency: 1,
            initial_timeout: Duration::from_millis(300),
            retry_timeout: Duration::from_secs(3),
            retry_backoff: Duration::from_millis(20),
            total_budget: Duration::from_secs(10),
            max_retry_agents: 3,
        };
        let results = probe_acp_targets_with_policy(
            vec![AcpProbeTarget {
                id: "custom-slow".into(),
                argv: vec![fake_agent.display().to_string()],
                exe_path: fake_agent.display().to_string(),
                env,
                cwd: tmp.display().to_string(),
            }],
            policy,
        )
        .await;

        assert_eq!(results.len(), 1);
        let a = &results[0];
        assert!(a.error.is_none(), "expected recovery on retry, got: {:?}", a.error);
        assert_eq!(a.agent_info.as_deref(), Some("FakeCursor 1.2.3"));
        assert!(a.models.contains(&"m-fast".to_string()));
        let attempts = std::fs::read_to_string(&marker).unwrap().lines().count();
        assert_eq!(attempts, 2);
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_acp_targets_does_not_retry_non_timeout_errors() {
        let tmp = crate::test_tmp().join(format!("drove-probe-no-retry-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&tmp).unwrap();
        let marker = tmp.join("attempts");
        let fake_agent = tmp.join("fake-err");
        write_exec_script(
            &fake_agent,
            &format!(
                "#!/bin/sh\necho x >> \"{}\"\necho 'Error: unauthorized' >&2\nexit 1\n",
                marker.display()
            ),
        );

        let mut env = BTreeMap::new();
        env.insert("HOME".into(), tmp.display().to_string());
        let policy = ProbePolicy {
            concurrency: 2,
            retry_concurrency: 1,
            initial_timeout: Duration::from_secs(2),
            retry_timeout: Duration::from_secs(2),
            retry_backoff: Duration::from_millis(10),
            total_budget: Duration::from_secs(10),
            max_retry_agents: 3,
        };
        let results = probe_acp_targets_with_policy(
            vec![AcpProbeTarget {
                id: "custom-err".into(),
                argv: vec![fake_agent.display().to_string()],
                exe_path: fake_agent.display().to_string(),
                env,
                cwd: tmp.display().to_string(),
            }],
            policy,
        )
        .await;

        assert_eq!(results.len(), 1);
        let err = results[0].error.as_deref().expect("expected error");
        assert!(err.contains("unauthorized"), "unexpected error: {err}");
        let attempts = std::fs::read_to_string(&marker).unwrap().lines().count();
        assert_eq!(attempts, 1, "non-timeout errors must not be retried");
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_acp_targets_timeout_preserves_phase_and_stderr() {
        let tmp = crate::test_tmp().join(format!("drove-probe-timeout-stderr-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&tmp).unwrap();
        let fake_agent = tmp.join("fake-hang-session");
        write_exec_script(
            &fake_agent,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{}}}'
      ;;
    *'"session/new"'*)
      echo 'proxy handshake stalled on connect' >&2
      sleep 5
      ;;
  esac
done
"#,
        );

        let mut env = BTreeMap::new();
        env.insert("HOME".into(), tmp.display().to_string());
        let policy = ProbePolicy {
            concurrency: 2,
            retry_concurrency: 1,
            initial_timeout: Duration::from_millis(350),
            retry_timeout: Duration::from_millis(350),
            retry_backoff: Duration::from_millis(20),
            total_budget: Duration::from_secs(10),
            max_retry_agents: 3,
        };
        let results = probe_acp_targets_with_policy(
            vec![AcpProbeTarget {
                id: "custom-hang".into(),
                argv: vec![fake_agent.display().to_string()],
                exe_path: fake_agent.display().to_string(),
                env,
                cwd: tmp.display().to_string(),
            }],
            policy,
        )
        .await;

        assert_eq!(results.len(), 1);
        let err = results[0].error.as_deref().expect("expected timeout error");
        assert!(err.contains("ACP probe timed out"), "missing timeout prefix: {err}");
        assert!(err.contains("retried once"), "missing retry marker: {err}");
        assert!(err.contains("ACP session/new failed"), "missing stalled phase: {err}");
        assert!(err.contains("proxy handshake stalled on connect"), "missing child stderr: {err}");
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_acp_targets_skips_retry_when_too_many_agents_time_out() {
        let tmp = crate::test_tmp().join(format!("drove-probe-systemic-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&tmp).unwrap();
        let marker = tmp.join("attempts");
        let fake_agent = tmp.join("fake-hang");
        write_exec_script(
            &fake_agent,
            &format!("#!/bin/sh\necho x >> \"{}\"\nsleep 5\n", marker.display()),
        );

        let mut env = BTreeMap::new();
        env.insert("HOME".into(), tmp.display().to_string());
        let policy = ProbePolicy {
            concurrency: 4,
            retry_concurrency: 2,
            initial_timeout: Duration::from_millis(250),
            retry_timeout: Duration::from_millis(250),
            retry_backoff: Duration::from_millis(10),
            total_budget: Duration::from_secs(10),
            max_retry_agents: 2,
        };
        let targets = (0..3)
            .map(|i| AcpProbeTarget {
                id: format!("hang-{i}"),
                argv: vec![fake_agent.display().to_string()],
                exe_path: fake_agent.display().to_string(),
                env: env.clone(),
                cwd: tmp.display().to_string(),
            })
            .collect();
        let results = probe_acp_targets_with_policy(targets, policy).await;

        assert_eq!(results.len(), 3);
        for a in &results {
            let err = a.error.as_deref().expect("expected timeout error");
            assert!(err.contains("ACP probe timed out"), "unexpected error: {err}");
            assert!(!err.contains("retried once"), "systemic outage should skip retry: {err}");
        }
        let attempts = std::fs::read_to_string(&marker).unwrap().lines().count();
        assert_eq!(attempts, 3, "expected exactly 1 attempt per agent when systemic outage skips retry");
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn probe_working_dir_skips_unc_paths_and_prefers_local_home() {
        assert!(is_unc_path(Path::new(r"\\wsl.localhost\Ubuntu2404\home\dev\repo")));
        assert!(is_unc_path(Path::new("//server/share/dir")));
        assert!(!is_unc_path(Path::new(r"D:\Users\dev")));
        assert!(!is_unc_path(Path::new("/home/dev")));

        let tmp = crate::test_tmp().join(format!("drove-probe-cwd-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&tmp).unwrap();

        let mut env = BTreeMap::new();
        env.insert("HOME".into(), tmp.display().to_string());
        env.insert("USERPROFILE".into(), tmp.display().to_string());
        assert_eq!(probe_working_dir(&env), tmp.display().to_string());

        let mut unc_env = BTreeMap::new();
        unc_env.insert("HOME".into(), r"\\wsl.localhost\Ubuntu2404\home\dev".into());
        unc_env.insert("USERPROFILE".into(), r"\\wsl.localhost\Ubuntu2404\home\dev".into());
        let picked = probe_working_dir(&unc_env);
        assert!(!is_unc_path(Path::new(&picked)), "must never return UNC path: {picked}");
        assert!(Path::new(&picked).is_dir(), "must return existing dir: {picked}");
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn resolve_probe_command_skips_pi_acp_when_pi_cli_is_missing() {
        let tmp = crate::test_tmp().join(format!("drove-probe-pi-companion-{}", crate::platform::random_hex()));
        let bin_dir = tmp.join("bin");
        let adapters_dir = tmp.join("adapters");
        let pi_bin_dir = adapters_dir.join("pi/node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(&pi_bin_dir).unwrap();

        let pi_acp = pi_bin_dir.join(if cfg!(windows) { "pi-acp.cmd" } else { "pi-acp" });
        std::fs::write(&pi_acp, "#!/bin/sh\n").unwrap();

        let env = BTreeMap::from([
            ("HOME".into(), tmp.display().to_string()),
            ("PATH".into(), bin_dir.display().to_string()),
            ("HERDR2_ADAPTERS_DIR".into(), adapters_dir.display().to_string()),
        ]);

        // Without `pi` CLI on PATH, `pi-acp` cannot run and must not be probed as installed.
        assert!(resolve_probe_command("pi", &["pi-acp".into()], &env).is_none());

        // Once `pi` CLI is installed on PATH, `resolve_probe_command` resolves the managed `pi-acp`.
        let pi_cli = bin_dir.join(if cfg!(windows) { "pi.cmd" } else { "pi" });
        std::fs::write(&pi_cli, "#!/bin/sh\n").unwrap();
        let resolved = resolve_probe_command("pi", &["pi-acp".into()], &env).expect("pi-acp should resolve with pi CLI");
        assert_eq!(resolved.0, pi_acp);

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn format_probe_acp_error_normalizes_pi_auth_and_discovers_local_pi_models() {
        let raw_win_err = r#"ACP session/new failed: Authentication required: Configure an API key or log in with an OAuth provider.: {"authMethods":[{"_meta":{"terminal-auth":{"args":["D:\\Users\\zhihao.zou\\AppData\\Local\\herdr2\\adapters\\pi\\node_modules\\pi-acp\\dist\\index.js","--terminal-login"],"command":"C:\\Program Files\\nodejs\\node.exe","label":"Launch pi"}},"args":["--terminal-login"],"description":"Start pi in an interactive terminal to configure API keys or login"}]} (code -32000)
stderr:
Error handling request {
  id: 2,
  jsonrpc: '2.0',
  method: 'session/new',
  params: { cwd: 'D:\\Users\\zhihao.zou', mcpServers: [] }
} {
  code: -32000
}"#;
        let pi_formatted = format_probe_acp_error("pi", raw_win_err);
        assert!(pi_formatted.starts_with("Authentication required:"));
        assert!(pi_formatted.contains("/login"));
        assert!(pi_formatted.contains("models.json"));
        assert!(!pi_formatted.contains("authMethods"));
        assert!(!pi_formatted.contains("Error handling request"));

        let other_formatted = format_probe_acp_error("custom-acp", raw_win_err);
        assert!(!other_formatted.contains("authMethods"), "other agents must also strip authMethods JSON: {other_formatted}");
        assert!(!other_formatted.contains("Error handling request"), "other agents must strip SDK echo: {other_formatted}");
        assert!(other_formatted.contains("(code -32000)"));

        let tmp = crate::test_tmp().join(format!("drove-probe-pi-models-{}", crate::platform::random_hex()));
        let home_pi_dir = tmp.join("home/.pi/agent");
        let custom_pi_dir = tmp.join("custom-pi-agent");
        std::fs::create_dir_all(&home_pi_dir).unwrap();
        std::fs::create_dir_all(&custom_pi_dir).unwrap();

        std::fs::write(
            home_pi_dir.join("settings.json"),
            r#"{"defaultProvider":"anthropic","defaultModel":"claude-sonnet-4-5","enabledModels":["openai/gpt-5.4","anthropic/*"]}"#,
        )
        .unwrap();
        std::fs::write(
            home_pi_dir.join("models.json"),
            r#"{"providers":{"ollama":{"models":[{"id":"qwen2.5-coder:7b"}]}}}"#,
        )
        .unwrap();
        std::fs::write(
            custom_pi_dir.join("settings.json"),
            r#"{"defaultModel":"deepseek/deepseek-chat"}"#,
        )
        .unwrap();

        let home_env = BTreeMap::from([("HOME".into(), tmp.join("home").display().to_string())]);
        let (models, dm) = discover_local_models("pi", true, &home_env);
        assert_eq!(dm.as_deref(), Some("anthropic/claude-sonnet-4-5"));
        assert_eq!(models, vec!["anthropic/claude-sonnet-4-5", "openai/gpt-5.4", "ollama/qwen2.5-coder:7b"]);

        let override_env = BTreeMap::from([
            ("HOME".into(), tmp.join("home").display().to_string()),
            ("PI_CODING_AGENT_DIR".into(), custom_pi_dir.display().to_string()),
        ]);
        let (models2, dm2) = discover_local_models("pi", true, &override_env);
        assert_eq!(dm2.as_deref(), Some("deepseek/deepseek-chat"));
        assert_eq!(models2[0], "deepseek/deepseek-chat");
        assert!(models2.contains(&"ollama/qwen2.5-coder:7b".to_string()));

        let _ = std::fs::remove_dir_all(tmp);
    }
}
