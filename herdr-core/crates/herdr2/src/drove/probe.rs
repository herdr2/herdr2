//! ACP agent binary/model discovery and generic AI endpoint connection testing.

use super::types::{ProbeSnapshot, ProbedAgent, now_iso};
use crate::acp::{SpawnSpec, spawn_session};
use crate::acp::presets::{BUILTIN_PRESETS, resolve_acp_command_in_env};
pub use crate::ai::client::{
    AiTestResult, all_model_ids, chat_endpoints, curl_json, curl_json_timeout, fetch_ai_models,
    first_model_id, models_endpoints, test_ai_connection,
};
use crate::runtime::Core;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

const ACP_PROBE_TIMEOUT: Duration = Duration::from_secs(35);
const CLI_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

fn env_ignore_case<'a>(env: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    env.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.as_str())
        .filter(|s| !s.is_empty())
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

/// Fast (<2ms) stat-only refresh of `installed`/`path`/`command` flags against the current environment
/// so newly installed agents never remain stuck as "missing" in a cached probe snapshot.
/// Returns `true` if `snap` was modified.
pub async fn refresh_installed_status(core: &Core, snap: &mut ProbeSnapshot) -> bool {
    let targets = collect_probe_targets(core);
    let base_env = crate::platform::program_env().await;
    let mut changed = false;

    for (id, base_argv) in targets {
        let resolved = resolve_acp_command_in_env(Some(&id), &base_argv, &base_env);
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
        snap.agents
            .sort_by(|a, b| b.installed.cmp(&a.installed).then_with(|| a.id.cmp(&b.id)));
    }
    changed
}

/// Probes installed ACP agents (and custom `[agents]` entries in `config.toml`) to extract
/// live model and reasoning-effort options for `Settings -> Drove -> Roles`.
pub async fn probe_agents(core: &Core, skip_acp_handshake: bool) -> ProbeSnapshot {
    let targets = collect_probe_targets(core);
    let base_env = crate::platform::program_env().await;
    let cwd = std::env::current_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
        .display()
        .to_string();

    let probe_scope = crate::sweep::token();
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
    let mut handles = Vec::new();
    for (id, argv) in targets {
        let resolved = resolve_acp_command_in_env(Some(&id), &argv, &base_env);
        let mut env = base_env.clone();
        if resolved.is_none() || skip_acp_handshake {
            let installed = resolved.is_some();
            let (exe_path, effective_argv) = match resolved {
                Some((p, cmd)) => (Some(p), cmd),
                None => (None, argv),
            };
            handles.push(tokio::spawn(async move {
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
        let path_str = exe_path.display().to_string();
        let probe_cwd = cwd.clone();
        let permit_sem = sem.clone();
        handles.push(tokio::spawn(async move {
            let _permit = permit_sem.acquire_owned().await;
            probe_one_acp(id, effective_argv, path_str, env, probe_cwd).await
        }));
    }

    let mut agents = Vec::with_capacity(handles.len());
    for h in handles {
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

async fn probe_one_acp(
    id: String,
    argv: Vec<String>,
    exe_path: String,
    env: BTreeMap<String, String>,
    cwd: String,
) -> ProbedAgent {
    let (local_models, local_default) = discover_local_models(&id, true, &env);
    if matches!(id.as_str(), "agy" | "antigravity") {
        // `agy acp` takes ~29s to boot CCPA + MCP servers on session/new; `agy models` returns the live catalog in ~5s.
        let (cli_models, cli_ver) = tokio::join!(
            probe_cli_models(&id, &env, &cwd),
            probe_cli_version(&exe_path, &env, &cwd),
        );
        let models = merge_model_lists(&[cli_models, local_models]);
        let model_efforts = infer_model_efforts_from_variants(&models);
        let default_model = local_default.or_else(|| models.first().cloned());
        return ProbedAgent {
            id,
            command: argv,
            installed: true,
            path: Some(exe_path),
            agent_info: cli_ver.map(|v| format!("Antigravity CLI {v}")),
            models,
            default_model,
            efforts: default_efforts(),
            default_effort: Some("medium".into()),
            model_contexts: BTreeMap::new(),
            model_efforts,
            error: None,
        };
    }

    let spec = SpawnSpec {
        address: format!("probe-{id}"),
        argv: argv.clone(),
        env: env.clone(),
        cwd: cwd.clone(),
        resume_session_id: None,
        can_load_session: false,
        system_prompt: None,
        model: None,
        mode: None,
        dangerously_skip_permissions: false,
        config_overrides: BTreeMap::new(),
        inject_mcp: false,
        mcp_exe: "herdr2".into(),
        socket: String::new(),
    };
    let (res, cli_models) = tokio::join!(
        tokio::time::timeout(ACP_PROBE_TIMEOUT, spawn_session(spec)),
        probe_cli_models(&id, &env, &cwd)
    );

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
                && let Ok(extra) = started
                    .transport
                    .request("cursor/list_available_models", &json!({}), Duration::from_secs(4))
                    .await
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

            let models = merge_model_lists(&[acp_models, cli_models, local_models]);
            let default_model = acp_default
                .filter(|s| !s.trim().is_empty())
                .or(local_default)
                .or_else(|| models.first().cloned());
            if !default_model_contexts.is_empty()
                && let Some(ref dm) = default_model
            {
                model_contexts.entry(dm.clone()).or_insert(default_model_contexts);
            }

            for (k, v) in infer_model_efforts_from_variants(&models) {
                model_efforts.entry(k).or_insert(v);
            }

            ProbedAgent {
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
            }
        }
        Ok(Err(err)) => {
            let err_opt = if cli_models.is_empty() { Some(err.message) } else { None };
            let models = merge_model_lists(&[cli_models, local_models]);
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
                error: err_opt,
            }
        }
        Err(_) => {
            let err_opt = if cli_models.is_empty() {
                Some(format!("ACP probe timed out after {}s", ACP_PROBE_TIMEOUT.as_secs()))
            } else {
                None
            };
            let models = merge_model_lists(&[cli_models, local_models]);
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
                error: err_opt,
            }
        }
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

async fn probe_cli_version(program: &str, env: &BTreeMap<String, String>, cwd: &str) -> Option<String> {
    let argv = vec![program.to_string(), "--version".to_string()];
    let mut cmd = crate::platform::agent_command(&argv, env, cwd).ok()?;
    cmd.stdin(std::process::Stdio::null());
    let out = tokio::time::timeout(CLI_PROBE_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = strip_ansi(&text).lines().next().unwrap_or("").trim().to_string();
    (!line.is_empty()).then_some(line)
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

async fn probe_cli_models(agent_id: &str, env: &BTreeMap<String, String>, cwd: &str) -> Vec<String> {
    let cli_argv: Vec<String> = match agent_id {
        "agy" | "antigravity" => vec!["agy".into(), "models".into()],
        "cursor" | "cursor-agent" => vec!["cursor-agent".into(), "--list-models".into()],
        "opencode" => vec!["opencode".into(), "models".into()],
        _ => return Vec::new(),
    };
    let Ok(mut cmd) = crate::platform::agent_command(&cli_argv, env, cwd) else {
        return Vec::new();
    };
    cmd.stdin(std::process::Stdio::null());
    let Ok(Ok(out)) = tokio::time::timeout(CLI_PROBE_TIMEOUT, cmd.output()).await else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_cli_models(agent_id, &text)
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
        if lower.starts_with("fetching ")
            || lower.starts_with("loading ")
            || lower.starts_with("available ")
        {
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
    for val in [
        env_ignore_case(env, "HOME").map(String::from),
        env_ignore_case(env, "USERPROFILE").map(String::from),
        std::env::var("HOME").ok(),
        std::env::var("USERPROFILE").ok(),
    ] {
        if let Some(raw) = val {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                let pb = PathBuf::from(trimmed);
                if !homes.contains(&pb) {
                    homes.push(pb);
                }
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

pub(crate) fn discover_local_models(
    agent_id: &str,
    installed: bool,
    env: &BTreeMap<String, String>,
) -> (Vec<String>, Option<String>) {
    discover_local_models_in_homes(agent_id, installed, &user_home_dirs(env))
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
                            if let Some(id) = entry
                                .as_str()
                                .or_else(|| entry["id"].as_str())
                                .or_else(|| entry["model"].as_str())
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
                    if let Some(m) = val["model"].as_str().or_else(|| val["env"]["ANTHROPIC_MODEL"].as_str()).filter(|s| !s.is_empty()) {
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
                            if let Some(id) = entry
                                .as_str()
                                .or_else(|| entry["id"].as_str())
                                .or_else(|| entry["model"].as_str())
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
                    && let Some(raw_m) = val["model"].as_str().or_else(|| val["model"]["name"].as_str()).filter(|s| !s.is_empty())
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
        "gemini" => {
            for home in homes {
                if let Ok(text) = std::fs::read_to_string(home.join(".gemini/settings.json"))
                    && let Ok(val) = serde_json::from_str::<Value>(&text)
                    && let Some(m) = val["model"].as_str().or_else(|| val["model"]["name"].as_str()).filter(|s| !s.is_empty())
                {
                    default_model.get_or_insert_with(|| m.to_string());
                    push_unique(&mut models, m);
                }
            }
            for curated in ["gemini-2.5-pro", "gemini-2.5-flash"] {
                push_unique(&mut models, curated);
            }
        }
        "cursor" | "cursor-agent" => {
            for home in homes {
                if let Ok(text) = std::fs::read_to_string(home.join(".cursor/cli-config.json"))
                    && let Ok(val) = serde_json::from_str::<Value>(&text)
                {
                    let m = val["model"]["modelId"]
                        .as_str()
                        .or_else(|| val["model"].as_str())
                        .filter(|s| !s.is_empty());
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
        "kimi" => {
            for home in homes {
                for rel in [".kimi/config.toml", ".kimi-code/config.toml"] {
                    if let Ok(text) = std::fs::read_to_string(home.join(rel))
                        && let Ok(val) = toml::from_str::<toml::Value>(&text)
                    {
                        if let Some(m) = val.get("default_model").and_then(toml::Value::as_str).filter(|s| !s.is_empty()) {
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
            for curated in ["glm-5", "glm-4.7"] {
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
        std::fs::write(
            tmp.join(".gemini/antigravity-cli/settings.json"),
            r#"{"model":"gemini-4-hydrogen-high"}"#,
        )
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
}
