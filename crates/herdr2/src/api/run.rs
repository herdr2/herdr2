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

    let (resolved, initial_prompt, model, mode, permissions, inject_mcp, system_prompt) = match &template {
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
            let permissions = p.permissions.or(t.permissions).or(settings.default_permissions).unwrap_or_default();
            let inject_mcp = if p.no_mcp {
                false
            } else {
                t.inject_mcp.or(settings.inject_mcp).unwrap_or(true)
            };
            let system_prompt = p.system_prompt.clone().or_else(|| t.system_prompt.clone());
            (resolved, prompt, model, mode, permissions, inject_mcp, system_prompt)
        }
        None => {
            let resolved = presets::resolve(p.agent.as_deref(), &p.argv, p.prompt.take(), &settings.agents)?;
            let prompt = resolved.prompt.clone().filter(|s| !s.trim().is_empty());
            let permissions = p.permissions.or(settings.default_permissions).unwrap_or_default();
            let inject_mcp = !p.no_mcp && settings.inject_mcp.unwrap_or(true);
            (resolved, prompt, p.model.clone(), p.mode.clone(), permissions, inject_mcp, p.system_prompt.clone())
        }
    };

    let caller = p.caller.clone();
    let from_afar = caller.as_deref().is_some_and(|c| c.contains('/'));
    let cwd = crate::exec::folder(p.cwd.as_deref(), from_afar);
    let base_env = crate::platform::program_env().await;
    let env = child_env(base_env, p.env.clone());

    let parent = match (&p.parent, &caller) {
        (Some(parent), _) => Some(crate::agents::address(resolve(core, parent)?)),
        (None, Some(caller)) if !p.no_parent && from_afar => {
            caller.split_once('/').and_then(|(_, a)| crate::agents::parse_id(a)).map(|_| caller.clone())
        }
        (None, Some(caller)) if !p.no_parent => crate::agents::parse_id(caller)
            .filter(|id| core.lock().reg.agents.contains_key(id))
            .map(crate::agents::address),
        _ => None,
    };

    let (id, address, env) = {
        let mut st = core.lock();
        not_stopping(&st)?;
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
        };
        let id = st.reg.create(new, now_ms())?;
        let (env, scope) = agent_env(core, id, &cwd, env);
        let a = st.reg.agents.get_mut(&id).expect("just created");
        a.scope = Some(scope);
        a.restart_env = p.env.clone();
        let address = a.address.clone();
        core.save(&st);
        (id, address, env)
    };

    let mcp_exe = std::env::current_exe().map(|exe| exe.display().to_string()).unwrap_or_else(|_| "herdr2".into());
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
        inject_mcp,
        mcp_exe,
        socket: core.paths.socket.display().to_string(),
    };

    let started = match acp::spawn_session(spec).await {
        Ok(started) => started,
        Err(err) => {
            let mut st = core.lock();
            core.remove(&mut st, id);
            return Err(err);
        }
    };

    let created_agent = {
        let mut st = core.lock();
        if st.stopping || st.killing.contains(&id) || !st.reg.agents.get(&id).is_some_and(Agent::is_live) {
            started.transport.kill();
            core.remove(&mut st, id);
            return Err(RpcError::new("unavailable", "the core is stopping", "run it again once restarted"));
        }
        let agent_info = started.agent_info.clone();
        let acp_sid = started.session_id.clone();
        let can_load = started.can_load_session;
        let handle = acp::spawn_actor(core.core.clone(), id, started);
        let pid = handle.transport.pid();
        st.sessions.insert(id, handle);
        let a = st.reg.agents.get_mut(&id).expect("just created");
        a.pid = pid;
        a.acp_session_id = Some(acp_sid);
        a.can_load_session = can_load;
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
        None => (None, None),
    };

    let (wait, output) = match timeout {
        None => (None, None),
        Some(timeout) => {
            let r = watch(core, caller.clone(), id, WaitSpec { output: None, timeout, after_turn }).await;
            let text = tail(
                core.read_transcript(id, Some(FINISHED_LINES), false).map(|t| t.0).unwrap_or_default(),
                id,
            );
            has_seen(core, caller.as_deref(), id);
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
        "[h2] earlier output cut: h2 read {} --lines {FINISHED_LINES}\n{}",
        crate::agents::address(id),
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
