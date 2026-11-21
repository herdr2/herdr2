//! `agent.run`: launch an ACP agent preset, command, or template, and optionally wait on its first turn.

use super::*;
use crate::acp::{self, SpawnSpec, presets};
use crate::status::StatusReport;
use crate::templates;

pub(super) async fn agent_run(core: Ctx<'_>, mut p: RunParams) -> Result<RunResult, RpcError> {
    not_stopping(&core.lock())?;
    let settings = crate::config::load(&core.paths.config)?;
    let timeout = if p.wait { Some(wait_timeout(core, p.timeout)?) } else { None };

    let template = match p.agent.is_none().then(|| p.argv.first()).flatten() {
        Some(name) => templates::load(&core.paths, name)?,
        None => None,
    };

    let (mut resolved, initial_prompt, model, mode, explicit_permissions, inject_mcp, system_prompt) = match &template {
        Some(t) => {
            if p.prompt.is_some() && p.argv.len() > 1 {
                return Err(RpcError::invalid_params(
                    "give initial prompt either with --prompt or after template name, not both",
                ));
            }
            let raw_task = p.prompt.take().unwrap_or_else(|| p.argv[1..].join(" "));
            let spec_str = t
                .agent
                .as_deref()
                .filter(|s| !s.is_empty())
                .or_else(|| (!t.command.is_empty()).then_some(t.command.as_str()));
            let resolved = presets::resolve(spec_str, &[], None, &settings.agents)?;
            let first = templates::first_message(t, &raw_task);
            let prompt = (!first.trim().is_empty()).then_some(first);
            let model = p.model.clone().or_else(|| t.model.clone());
            let mode = p.mode.clone().or_else(|| t.mode.clone());
            let explicit_permissions = p.permissions.or(t.permissions).or(settings.default_permissions);
            let inject_mcp = if p.no_mcp {
                false
            } else {
                t.inject_mcp.or(settings.inject_mcp).unwrap_or(true)
            };
            let system_prompt = p.system_prompt.clone().or_else(|| t.system_prompt.clone());
            (resolved, prompt, model, mode, explicit_permissions, inject_mcp, system_prompt)
        }
        None => {
            let resolved = presets::resolve(p.agent.as_deref(), &p.argv, p.prompt.take(), &settings.agents)?;
            let prompt = resolved.prompt.clone().filter(|s| !s.trim().is_empty());
            let explicit_permissions = p.permissions.or(settings.default_permissions);
            let inject_mcp = !p.no_mcp && settings.inject_mcp.unwrap_or(true);
            (resolved, prompt, p.model.clone(), p.mode.clone(), explicit_permissions, inject_mcp, p.system_prompt.clone())
        }
    };

    let is_claude = presets::is_claude_agent(resolved.preset.as_deref(), &resolved.argv);
    let default_claude_skip = is_claude && mode.is_none() && explicit_permissions.is_none();
    let mode = mode
        .map(|m| acp::normalize_mode_value(&m, is_claude, &[]))
        .or_else(|| default_claude_skip.then(|| "bypassPermissions".to_string()));
    let skip_perms = p.dangerously_skip_permissions
        || default_claude_skip
        || mode.as_deref().is_some_and(crate::permissions::is_bypass_mode_id);
    if skip_perms {
        presets::ensure_agy_skip_permissions(&mut resolved.argv);
    }
    let permissions = if skip_perms {
        crate::permissions::PermissionMode::ApproveAll
    } else {
        explicit_permissions.unwrap_or_default()
    };

    let caller = p.caller.clone();
    let from_afar = caller.as_deref().is_some_and(|c| c.contains('/'));
    let cwd = crate::exec::folder(p.cwd.as_deref(), from_afar);
    if p.async_start && !p.wait && !std::path::Path::new(&cwd).is_dir() {
        return Err(RpcError::new(
            "spawn_failed",
            format!("missing working directory {cwd}"),
            "",
        ));
    }
    let base_env = crate::platform::program_env().await;
    let env = child_env(base_env, p.env.clone());
    if let Some((_, effective_argv)) =
        presets::resolve_acp_command_in_env(resolved.preset.as_deref(), &resolved.argv, &env)
    {
        resolved.argv = effective_argv;
    }

    let parent = match (&p.parent, &caller) {
        (Some(parent), _) => Some(resolve_any(core, parent)?.address()),
        (None, Some(caller)) if !p.no_parent && from_afar => {
            caller.split_once('/').and_then(|(_, a)| TargetKey::parse(a)).map(|_| caller.clone())
        }
        (None, Some(caller)) if !p.no_parent => match TargetKey::parse(caller) {
            Some(TargetKey::Agent(id)) if core.lock().reg.agents.contains_key(&id) => {
                Some(crate::agents::address(id))
            }
            Some(TargetKey::Terminal(id)) if core.lock().pty_reg.terminals.contains_key(&id) => {
                Some(crate::terminals::address(id))
            }
            _ => None,
        },
        _ => None,
    };

    let async_mode = p.async_start && !p.wait;
    let (id, address, env, created_async) = {
        let mut st = core.lock();
        not_stopping(&st)?;
        if let Some(name) = p.name.as_deref()
            && st.pty_reg.terminals.values().any(|t| t.is_live() && t.name.as_deref() == Some(name))
        {
            return Err(RpcError::new(
                "name_taken",
                format!("name {name:?} is already taken by a live terminal"),
                "pick another with --name",
            ));
        }
        let new = NewAgent {
            template: template.clone(),
            die_with_parent: parent.is_some() && !p.outlive_parent,
            parent,
            notify: !p.no_notify,
            rm: p.rm,
            keep: p.keep,
            name: p.name.clone(),
            labels: labels(&p.labels),
            argv: resolved.argv.clone(),
            cwd: cwd.clone(),
            agent: resolved.preset.clone(),
            system_prompt: system_prompt.clone(),
            model: model.clone(),
            mode: mode.clone(),
            inject_mcp,
            permission_mode: permissions,
            lineage: p.lineage.clone(),
        };
        let id = st.reg.create(new, now_ms())?;
        st.recovering.insert(id);
        let (env, scope) = agent_env(core, id, &cwd, env);
        let a = st.reg.agents.get_mut(&id).expect("just created");
        a.scope = Some(scope);
        a.restart_env = p.env.clone();
        if async_mode {
            let _ = crate::status::apply(a, &StatusReport::root("starting"));
        }
        let created = a.clone();
        let address = a.address.clone();
        if async_mode {
            let public = serde_json::to_value(Public(created.clone())).unwrap_or_default();
            core.emit(&mut st, "created", id, public);
        }
        core.save(&st);
        if async_mode {
            core.changed.notify_waiters();
        }
        (id, address, env, created)
    };

    let spec_skip = p.dangerously_skip_permissions && mode.is_none();
    let mcp_exe = crate::client::exe_path().map(|exe| exe.display().to_string()).unwrap_or_else(|| "herdr2".into());
    let spec = SpawnSpec {
        address,
        argv: resolved.argv,
        env,
        cwd,
        resume_session_id: None,
        can_load_session: false,
        system_prompt,
        model,
        mode,
        dangerously_skip_permissions: spec_skip,
        config_overrides: BTreeMap::new(),
        inject_mcp,
        mcp_exe,
        socket: core.paths.socket.display().to_string(),
    };

    if async_mode {
        let core_bg = core.core.clone();
        let caller_bg = caller.clone();
        tokio::spawn(async move {
            match acp::spawn_session(spec).await {
                Ok(started) => {
                    {
                        let mut st = core_bg.lock();
                        st.recovering.remove(&id);
                        if st.stopping || st.killing.contains(&id) || !st.reg.agents.get(&id).is_some_and(Agent::is_live) {
                            started.transport.kill();
                            core_bg.remove(&mut st, id);
                            return;
                        }
                        let agent_info = started.agent_info.clone();
                        let acp_sid = started.session_id.clone();
                        let can_load = started.can_load_session;
                        let config_options = started.config_options.clone();
                        let context_usage = started.context_usage;
                        let handle = acp::spawn_actor(core_bg.clone(), id, started);
                        let pid = handle.transport.pid();
                        st.sessions.insert(id, handle);
                        core_bg.sync_inbox(&mut st, id, false);
                        let a = st.reg.agents.get_mut(&id).expect("just created");
                        a.pid = pid;
                        a.acp_session_id = Some(acp_sid);
                        a.can_load_session = can_load;
                        a.config_options = config_options;
                        if let Some(usage) = context_usage {
                            a.context_usage = Some(usage);
                        }
                        acp::sync_config_to_agent(a);
                        if agent_info.is_some() {
                            a.agent_info = agent_info;
                        }
                        a.program_started_ms = Some(now_ms());
                        a.reported = true;
                        let _ = crate::status::apply(a, &StatusReport::root("idle"));
                        let public = serde_json::to_value(Public(a.clone())).unwrap_or_default();
                        core_bg.emit(&mut st, "restarted", id, public);
                        core_bg.save(&st);
                        core_bg.changed.notify_waiters();
                    }
                    if let Some(text) = initial_prompt {
                        let tagged = core_bg.tagged(caller_bg.as_deref(), id, &text, true);
                        if let Ok(sent) = core_bg.send(id, &tagged, false, None, caller_bg.as_deref()).await {
                            let mut st = core_bg.lock();
                            if let Some(a) = st.reg.agents.get_mut(&id)
                                && a.is_live()
                            {
                                a.first_task_turn = Some(sent.after_turn);
                                core_bg.save(&st);
                            }
                        }
                    } else {
                        let _ = crate::checkpoint::capture_target_checkpoint(
                            &core_bg,
                            crate::runtime::TargetKey::Agent(id),
                            Some(0),
                            None,
                            None,
                            true,
                            false,
                        )
                        .await;
                    }
                }
                Err(err) => {
                    let mut st = core_bg.lock();
                    st.recovering.remove(&id);
                    if let Some(a) = st.reg.agents.get_mut(&id)
                        && a.is_live()
                    {
                        a.keep = false;
                        core_bg.record_transcript(
                            &mut st,
                            id,
                            crate::transcript::Entry::Error {
                                turn: 0,
                                message: err.message,
                                ts_ms: now_ms(),
                            },
                        );
                        core_bg.record_finish(&mut st, id, crate::runtime::Exit::default(), "spawn_failed");
                        core_bg.save(&st);
                        core_bg.changed.notify_waiters();
                    }
                }
            }
        });
        core.flush_saved().await?;
        return Ok(RunResult { agent: Public(created_async), first_message: None, wait: None, output: None });
    }

    let started = match acp::spawn_session(spec).await {
        Ok(started) => started,
        Err(err) => {
            let mut st = core.lock();
            st.recovering.remove(&id);
            core.remove(&mut st, id);
            return Err(err);
        }
    };

    let created_agent = {
        let mut st = core.lock();
        st.recovering.remove(&id);
        if st.stopping || st.killing.contains(&id) || !st.reg.agents.get(&id).is_some_and(Agent::is_live) {
            started.transport.kill();
            core.remove(&mut st, id);
            return Err(RpcError::new("unavailable", "the core is stopping", "run it again once restarted"));
        }
        let agent_info = started.agent_info.clone();
        let acp_sid = started.session_id.clone();
        let can_load = started.can_load_session;
        let config_options = started.config_options.clone();
        let handle = acp::spawn_actor(core.core.clone(), id, started);
        let pid = handle.transport.pid();
        st.sessions.insert(id, handle);
        let a = st.reg.agents.get_mut(&id).expect("just created");
        a.pid = pid;
        a.acp_session_id = Some(acp_sid);
        a.can_load_session = can_load;
        a.config_options = config_options;
        acp::sync_config_to_agent(a);
        if agent_info.is_some() {
            a.agent_info = agent_info;
        }
        a.program_started_ms = Some(now_ms());
        a.reported = true;
        let _ = crate::status::apply(a, &StatusReport::root("idle"));
        let created = a.clone();
        let public = serde_json::to_value(Public(created.clone())).unwrap_or_default();
        core.emit(&mut st, "created", id, public);
        core.save(&st);
        core.changed.notify_waiters();
        created
    };

    let (first_message, after_turn) = match initial_prompt {
        Some(text) => {
            let tagged = core.tagged(caller.as_deref(), id, &text, true);
            match core.send(id, &tagged, false, None, caller.as_deref()).await {
                Ok(sent) => {
                    let mut st = core.lock();
                    if let Some(a) = st.reg.agents.get_mut(&id)
                        && a.is_live()
                    {
                        a.first_task_turn = Some(sent.after_turn);
                        core.save(&st);
                    }
                    (
                        Some(templates::FirstMessage {
                            sent: true,
                            started: sent.started,
                            reason: None,
                            turn: Some(sent.turn),
                        }),
                        Some(sent.after_turn),
                    )
                }
                Err(err) => {
                    let failed = templates::FirstMessage {
                        sent: false,
                        started: None,
                        reason: Some(err.message),
                        turn: None,
                    };
                    let mut st = core.lock();
                    if let Some(a) = st.reg.agents.get_mut(&id) {
                        a.first_message = Some(failed.clone());
                        core.save(&st);
                    }
                    (Some(failed), None)
                }
            }
        }
        None => {
            let core_bg = Arc::clone(&core.core);
            tokio::spawn(async move {
                let _ = crate::checkpoint::capture_target_checkpoint(
                    &core_bg,
                    crate::runtime::TargetKey::Agent(id),
                    Some(0),
                    None,
                    None,
                    true,
                    false,
                )
                .await;
            });
            (None, None)
        }
    };

    let (wait, output) = match timeout {
        None => (None, None),
        Some(timeout) => {
            let target = crate::runtime::TargetKey::Agent(id);
            let r = watch(core, caller.clone(), target, WaitSpec { output: None, timeout: Some(timeout), after_turn }).await;
            let text = tail(
                core.read_transcript(id, Some(FINISHED_LINES), false).map(|t| t.0).unwrap_or_default(),
                id,
            );
            has_seen(core, caller.as_deref(), target);
            (Some(r), Some(text))
        }
    };

    let snapshot = record(core, id).unwrap_or(created_agent);
    core.flush_saved().await?;
    Ok(RunResult { agent: Public(snapshot), first_message, wait, output })
}

/// Output a run hands back: at most `RUN_OUTPUT` bytes, whole lines from the
/// end (errors are at the end), saying where the rest is.
pub(crate) fn tail(text: String, id: u32) -> String {
    tail_address(text, &crate::agents::address(id))
}

pub(crate) fn tail_address(text: String, address: &str) -> String {
    const RUN_OUTPUT: usize = 50_000;
    if text.len() <= RUN_OUTPUT {
        return text;
    }
    let mut from = text.len() - RUN_OUTPUT;
    while !text.is_char_boundary(from) {
        from += 1;
    }
    let from = text[from..].find('\n').map_or(from, |at| from + at + 1);
    format!(
        "[h2] earlier output cut: h2 read {address} --lines {FINISHED_LINES}\n{}",
        &text[from..]
    )
}

/// A stopping core starts nothing: whatever it started now would outlive the stop.
pub(crate) fn not_stopping(st: &crate::runtime::State) -> Result<(), RpcError> {
    match st.stopping {
        true => Err(RpcError::new("unavailable", "the core is stopping", "run it again once it has stopped")),
        false => Ok(()),
    }
}

/// Per-agent runtime environment variables.
pub(crate) fn agent_env(
    core: &Core,
    id: u32,
    cwd: &str,
    mut env: BTreeMap<String, String>,
) -> (BTreeMap<String, String>, String) {
    env.insert("H2_AGENT_ID".into(), crate::agents::address(id));
    env.insert("H2_AGENT_SOCKET".into(), core.paths.socket.display().to_string());
    let scope = crate::sweep::token();
    env.insert(crate::sweep::VAR.into(), scope.clone());
    // Preserve the requested spelling, including symlinks.
    env.insert("PWD".into(), cwd.into());
    (env, scope)
}

const TERMINAL_VARS: &[&str] = &["TERM", "COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION"];

/// Clean child environment without terminal-only variables or stale agent identities.
pub(crate) fn child_env(base: BTreeMap<String, String>, extra: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = base
        .into_iter()
        .filter(|(k, _)| {
            !IDENTITY_VARS.contains(&k.as_str())
                && !TERMINAL_VARS.contains(&k.as_str())
                && ![crate::sweep::VAR, crate::sweep::RUNTIME].contains(&k.as_str())
        })
        .collect();
    crate::platform::merge_env(&mut env, extra);
    if let Ok(session) = std::env::var("H2_SESSION") {
        env.insert("H2_SESSION".into(), session);
    }
    if let Some(lang) = crate::locale::for_programs(&env) {
        env.insert("LANG".into(), lang);
    }
    env
}

pub(crate) struct DroveSpawnSpec {
    pub agent: String,
    pub cwd: String,
    pub parent: Option<String>,
    pub title: String,
    pub labels: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
    pub model: Option<String>,
    pub context: Option<String>,
    pub effort: Option<String>,
    pub fast: Option<bool>,
    pub access: crate::drove::types::RoleAccessMode,
    pub isolate_secrets: bool,
}

pub(crate) struct DroveSpawnResult {
    pub agent_id: u32,
    pub address: String,
    pub pid: Option<u32>,
    pub sandbox_mode: &'static str,
}

/// Spawns a native `herdr2` headless ACP `a*` worker for Drove (without an initial prompt yet)
/// so the caller can persist Worker/Attempt state before sending the task prompt.
pub(crate) async fn spawn_drove_agent(
    core: &Arc<Core>,
    req: DroveSpawnSpec,
) -> Result<DroveSpawnResult, RpcError> {
    not_stopping(&core.lock())?;
    if !std::path::Path::new(&req.cwd).is_dir() {
        return Err(RpcError::new("spawn_failed", format!("missing working directory {}", req.cwd), ""));
    }
    let settings = crate::config::load(&core.paths.config)?;
    let mut resolved = presets::resolve(Some(&req.agent), &[], None, &settings.agents)?;

    let bin_dir = ensure_drove_bin_links(&core.drove.paths.state_dir);
    let base_env = crate::platform::program_env().await;
    let mut env = child_env(base_env, req.env);
    if let Some(bdir) = bin_dir {
        let old = env.get("PATH").cloned().unwrap_or_default();
        let combined = if old.is_empty() { bdir } else { format!("{bdir}:{old}") };
        env.insert("PATH".into(), combined);
    }
    if let Some((_, effective_argv)) =
        presets::resolve_acp_command_in_env(resolved.preset.as_deref(), &resolved.argv, &env)
    {
        resolved.argv = effective_argv;
    }

    let (mut wrapped_argv, sandbox_mode) = crate::drove::sandbox::wrap_worker_argv(
        &resolved.argv,
        &core.drove.paths,
        &req.cwd,
        req.access,
        req.isolate_secrets,
    );
    presets::ensure_agy_skip_permissions(&mut wrapped_argv);

    let mut overrides = BTreeMap::new();
    let is_agy = matches!(req.agent.as_str(), "agy" | "antigravity");
    let is_cursor = matches!(req.agent.as_str(), "cursor" | "cursor-agent");
    let cursor_has_effort = is_cursor && req.model.as_deref().is_some_and(crate::acp::model_has_embedded_effort);
    if !is_agy
        && !cursor_has_effort
        && let Some(eff) = &req.effort
        && !eff.trim().is_empty()
    {
        overrides.insert("reasoning_effort".into(), serde_json::Value::String(eff.clone()));
    }
    if is_cursor
        && let Some(ctx) = req.context.as_deref().map(str::trim)
        && !ctx.is_empty()
        && !ctx.eq_ignore_ascii_case("auto")
    {
        overrides.insert("context".into(), serde_json::Value::String(ctx.to_string()));
    }
    if let Some(fast) = req.fast
        && (fast || req.agent == "codex")
    {
        overrides.insert("fast-mode".into(), serde_json::Value::Bool(fast));
    }

    let (id, address, env) = {
        let mut st = core.lock();
        not_stopping(&st)?;
        let new = NewAgent {
            template: None,
            die_with_parent: false,
            parent: req.parent.clone(),
            notify: false,
            rm: false,
            keep: false,
            name: None,
            labels: req.labels.clone(),
            argv: wrapped_argv.clone(),
            cwd: req.cwd.clone(),
            agent: resolved.preset.clone(),
            system_prompt: None,
            model: req.model.clone(),
            mode: None,
            inject_mcp: false,
            permission_mode: crate::permissions::PermissionMode::ApproveAll,
            lineage: None,
        };
        let id = st.reg.create(new, now_ms())?;
        st.recovering.insert(id);
        let (env, scope) = agent_env(core, id, &req.cwd, env);
        let a = st.reg.agents.get_mut(&id).expect("just created");
        a.scope = Some(scope);
        a.title = req.title.clone();
        a.config_overrides = overrides.clone();
        let addr = a.address.clone();
        core.save(&st);
        (id, addr, env)
    };

    let mcp_exe = crate::client::exe_path().map(|exe| exe.display().to_string()).unwrap_or_else(|| "herdr2".into());
    let spec = SpawnSpec {
        address: address.clone(),
        argv: wrapped_argv,
        env,
        cwd: req.cwd,
        resume_session_id: None,
        can_load_session: false,
        system_prompt: None,
        model: req.model,
        mode: None,
        dangerously_skip_permissions: true,
        config_overrides: overrides,
        inject_mcp: false,
        mcp_exe,
        socket: core.paths.socket.display().to_string(),
    };

    let started = match acp::spawn_session(spec).await {
        Ok(started) => started,
        Err(err) => {
            let mut st = core.lock();
            st.recovering.remove(&id);
            core.remove(&mut st, id);
            return Err(err);
        }
    };

    let pid = {
        let mut st = core.lock();
        st.recovering.remove(&id);
        if st.stopping || st.killing.contains(&id) || !st.reg.agents.get(&id).is_some_and(Agent::is_live) {
            started.transport.kill();
            core.remove(&mut st, id);
            return Err(RpcError::new("unavailable", "the core is stopping", "run it again once restarted"));
        }
        let agent_info = started.agent_info.clone();
        let acp_sid = started.session_id.clone();
        let can_load = started.can_load_session;
        let config_options = started.config_options.clone();
        let handle = acp::spawn_actor(core.clone(), id, started);
        let pid = handle.transport.pid();
        st.sessions.insert(id, handle);
        let a = st.reg.agents.get_mut(&id).expect("just created");
        a.pid = pid;
        a.acp_session_id = Some(acp_sid);
        a.can_load_session = can_load;
        a.config_options = config_options;
        acp::sync_config_to_agent(a);
        if agent_info.is_some() {
            a.agent_info = agent_info;
        }
        a.program_started_ms = Some(now_ms());
        a.reported = true;
        let _ = crate::status::apply(a, &StatusReport::root("idle"));
        let created = a.clone();
        let public = serde_json::to_value(Public(created)).unwrap_or_default();
        core.emit(&mut st, "created", id, public);
        core.save(&st);
        core.changed.notify_waiters();
        pid
    };
    core.flush_saved().await?;
    let core_bg = Arc::clone(core);
    tokio::spawn(async move {
        let _ = crate::checkpoint::capture_target_checkpoint(
            &core_bg,
            crate::runtime::TargetKey::Agent(id),
            Some(0),
            None,
            None,
            true,
            false,
        )
        .await;
    });
    Ok(DroveSpawnResult { agent_id: id, address, pid, sandbox_mode })
}

/// Updates a live agent's `title` and `labels` and broadcasts `title` + `labels` events to subscribers.
pub(crate) fn update_agent_display(
    core: &Core,
    id: u32,
    title: String,
    labels_patch: BTreeMap<String, Option<String>>,
) {
    let mut st = core.lock();
    let was = st.reg.agents.get(&id).cloned();
    let Some(a) = st.reg.agents.get_mut(&id) else {
        return;
    };
    if !a.is_live() {
        return;
    }
    a.title = title.clone();
    for (k, v) in labels_patch {
        match v {
            Some(val) if !val.is_empty() => {
                a.labels.insert(k, val);
            }
            _ => {
                a.labels.remove(&k);
            }
        }
    }
    let labels = serde_json::to_value(&a.labels).unwrap_or_default();
    core.emit(&mut st, "title", id, serde_json::json!({ "title": title }));
    core.emit_from(&mut st, "labels", id, serde_json::json!({ "labels": labels }), was.as_ref());
    core.save(&st);
    core.changed.notify_waiters();
}

#[cfg(unix)]
fn ensure_drove_bin_links(state_dir: &std::path::Path) -> Option<String> {
    let exe = crate::client::exe_path()?;
    let bin_dir = state_dir.join("bin");
    let _ = std::fs::create_dir_all(&bin_dir);
    for name in ["h2", "drove", "herdr2"] {
        let link = bin_dir.join(name);
        if !link.exists() {
            let _ = std::os::unix::fs::symlink(&exe, &link);
        }
    }
    Some(bin_dir.display().to_string())
}

#[cfg(not(unix))]
fn ensure_drove_bin_links(state_dir: &std::path::Path) -> Option<String> {
    let exe = crate::client::exe_path()?;
    let bin_dir = state_dir.join("bin");
    if std::fs::create_dir_all(&bin_dir).is_err() {
        return exe.parent().map(|d| d.display().to_string());
    }
    for name in ["h2.exe", "drove.exe", "herdr2.exe"] {
        let link = bin_dir.join(name);
        if !link.exists() && std::fs::hard_link(&exe, &link).is_err() {
            let _ = std::fs::copy(&exe, &link);
        }
    }
    Some(bin_dir.display().to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_long_output_keeps_whole_lines_from_its_end_even_mid_character() {
        let text = "é".repeat(30_000) + "\nlast line";
        let cut = super::tail(text, 7);
        assert!(cut.starts_with("[h2] earlier output cut: h2 read a7") && cut.ends_with("\nlast line"));
        assert_eq!(super::tail("short".into(), 7), "short");
    }
}
