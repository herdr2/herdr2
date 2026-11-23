//! `terminal.*` API handlers: real interactive PTY terminals + Ghostty VT screens + holder IPC.

use super::*;
use crate::holder_proto as proto;
use crate::runtime::TargetKey;
use crate::templates;
use crate::terminals::{NewTerminal, Public as TerminalPublic, Terminal};

pub(crate) fn is_acp_caller(caller: Option<&str>) -> bool {
    let Some(caller) = caller.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    let local = caller.rsplit_once('/').map_or(caller, |(_, tail)| tail);
    matches!(TargetKey::parse(local), Some(TargetKey::Agent(_)))
}

pub(super) fn resolve_pty(core: &Core, address: &str) -> Result<u32, RpcError> {
    let local = crate::addr::local(address, &core.info.id, &core.info.name)?;
    core.lock().pty_reg.resolve(local)
}

pub(super) fn record_pty(core: &Core, id: u32) -> Result<Terminal, RpcError> {
    core.lock().pty_reg.terminals.get(&id).cloned().ok_or_else(|| {
        RpcError::new("not_found", format!("{} was removed", crate::terminals::address(id)), "see `h2 ls --all`")
    })
}

pub(super) async fn terminal_run(core: Ctx<'_>, mut p: TerminalRunParams) -> Result<TerminalRunResult, RpcError> {
    let template = match p.argv.first() {
        Some(name) => templates::load(&core.paths, name)?,
        None => None,
    };
    let first = template.as_ref().map(|t| templates::first_message(t, &p.argv[1..].join(" ")));
    match (&template, p.line.take(), p.argv.is_empty()) {
        (Some(t), None, _) => {
            if t.command.trim().is_empty() {
                return Err(RpcError::invalid_params("template has no terminal command"));
            }
            p.argv = vec!["sh".into(), "-c".into(), t.command.clone()];
        }
        (None, Some(line), true) => p.argv = crate::platform::shell_line(&line),
        (None, None, true) => p.argv = crate::config::load(&core.paths.config)?.shell()?,
        (None, None, false) => {}
        _ => return Err(RpcError::invalid_params("give argv or line, not both")),
    }
    let quiet_ready = template.as_ref().is_some_and(|t| t.ready_when_quiet);
    let caller = p.caller.clone();
    let timeout = if p.wait { Some(wait_timeout(core, p.timeout)?) } else { None };
    if p.wait {
        for pager in ["PAGER", "GIT_PAGER", "GH_PAGER"] {
            p.env.insert(pager.into(), "cat".into());
        }
    }
    let started = start_pty(core, p, template).await?;
    let id = started.id();
    let first_message = match first.filter(|text| !text.is_empty()) {
        Some(text) => Some(core.pty_first_message(id, quiet_ready, &text, caller.as_deref()).await),
        None => None,
    };
    if let Some(failed) = first_message.as_ref().filter(|f| !f.sent) {
        let mut st = core.lock();
        if let Some(t) = st.pty_reg.terminals.get_mut(&id) {
            t.first_message = Some(failed.clone());
            core.save_pty(&st);
        }
    }
    let (wait, output) = match timeout {
        None => (None, None),
        Some(timeout) => {
            let after_turn = first_message.as_ref().and_then(|f| f.turn);
            let r = watch(
                core,
                caller.clone(),
                TargetKey::Terminal(id),
                WaitSpec { output: None, timeout: Some(timeout), after_turn },
            )
            .await;
            let full_screen = core.lock().screens.get(&id).is_some_and(|s| s.alt_screen());
            let read = TerminalReadParams {
                terminal: crate::terminals::address(id),
                caller: None,
                lines: if full_screen { 0 } else { FINISHED_LINES },
            };
            let text = tail_address(
                terminal_read(core, read).await.map(|r| r.text).unwrap_or_default(),
                &crate::terminals::address(id),
            );
            has_seen(core, caller.as_deref(), TargetKey::Terminal(id));
            (Some(r), Some(text))
        }
    };
    core.flush_saved().await?;
    Ok(TerminalRunResult { terminal: TerminalPublic(started), first_message, wait, output })
}

pub(super) async fn terminal_list(core: Ctx<'_>, p: ListParams) -> Result<TerminalListResult, RpcError> {
    let afar = from_afar(core);
    let (mut terminals, mut hidden_finished, machines) = {
        let mut st = core.lock();
        core.prune(&mut st);
        let matching: Vec<&Terminal> = st
            .pty_reg
            .terminals
            .values()
            .filter(|t| p.labels.iter().all(|Label(k, v)| t.labels.get(k) == Some(v)))
            .collect();
        let hidden = if p.all { 0 } else { matching.iter().filter(|t| !t.is_live()).count() };
        let terminals: Vec<serde_json::Value> =
            matching.into_iter().filter(|t| p.all || t.is_live()).cloned().map(|t| json!(TerminalPublic(t))).collect();
        let machines = if afar { Vec::new() } else { st.machines.clone() };
        (terminals, hidden, machines)
    };
    let asks: Vec<_> = machines
        .iter()
        .map(|m| {
            let (core, m, params) = (core.core.clone(), m.clone(), serde_json::to_value(&p).unwrap_or_default());
            tokio::spawn(async move {
                let ask = crate::remote::forward(&core, &m, "terminal.list", params);
                tokio::time::timeout(Duration::from_secs(3), ask).await.ok().and_then(Result::ok)
            })
        })
        .collect();
    for ask in asks {
        if let Ok(Some(r)) = ask.await {
            terminals.extend(r["terminals"].as_array().cloned().unwrap_or_default());
            hidden_finished += r["hidden_finished"].as_u64().unwrap_or(0) as usize;
        }
    }
    let machines = {
        let st = core.lock();
        machines.iter().map(|m| crate::remote::view(&st, m)).collect()
    };
    core.flush_saved().await?;
    Ok(TerminalListResult { terminals, hidden_finished, machines })
}

pub(super) async fn terminal_get(core: Ctx<'_>, p: TerminalParams) -> Result<TerminalPublic, RpcError> {
    let id = resolve_pty(core, &p.terminal)?;
    core.pty_saw(p.caller.as_deref(), id);
    let result = record_pty(core, id).map(TerminalPublic);
    core.flush_saved().await?;
    result
}

pub(super) async fn terminal_kill(core: Ctx<'_>, p: TerminalParams) -> Result<TerminalPublic, RpcError> {
    let id = resolve_pty(core, &p.terminal)?;
    if core.lock().pty_reg.terminals.get(&id).is_some_and(|t| t.is_live()) {
        core.lock().pty_killing.insert(id);
        core.pty_kill(&[id]).await;
    }
    core.pty_saw(p.caller.as_deref(), id);
    let result = record_pty(core, id).map(TerminalPublic);
    core.flush_saved().await?;
    result
}

pub(super) async fn terminal_rm(core: Ctx<'_>, p: TerminalParams) -> Result<RemoveResult, RpcError> {
    let id = resolve_pty(core, &p.terminal)?;
    let cleanup_wt: Option<(String, String, Option<String>)> = {
        let mut st = core.lock();
        if st.pty_reg.terminals.get(&id).is_some_and(|t| t.is_live()) {
            return Err(RpcError::new(
                "live",
                format!("{} is still running", crate::terminals::address(id)),
                format!("kill it first: h2 kill {}", crate::terminals::address(id)),
            ));
        }
        let wt = st.pty_reg.terminals.get(&id).and_then(|t| {
            let lin = t.lineage.as_ref()?;
            Some((lin.worktree_origin.clone()?, lin.worktree_path.clone()?, lin.worktree_branch.clone()))
        });
        core.pty_remove(&mut st, id);
        wt
    };
    if let Some((origin, wt_path, wt_branch)) = cleanup_wt {
        let _ = tokio::task::spawn_blocking(move || {
            crate::drove::integrate::cleanup_fork_worktree(
                &origin,
                std::path::Path::new(&wt_path),
                wt_branch.as_deref(),
            );
        })
        .await;
    }
    core.flush_saved().await?;
    Ok(RemoveResult { removed: crate::terminals::address(id) })
}

pub(super) async fn terminal_keep(core: Ctx<'_>, p: TerminalKeepParams) -> Result<TerminalPublic, RpcError> {
    let id = resolve_pty(core, &p.terminal)?;
    {
        let mut st = core.lock();
        let t = st.pty_reg.terminals.get_mut(&id).expect("resolved");
        if !t.is_live() {
            return Err(RpcError::invalid_params("only live terminals can be kept"));
        }
        t.keep = p.keep;
        let lost = (!p.keep && t.holder.is_none()).then(|| t.reason.clone().unwrap_or_else(|| "holder_lost".into()));
        core.emit_pty(&mut st, "keep", id, json!({"keep": p.keep}));
        if let Some(reason) = lost {
            core.pty_record_finish(&mut st, id, Exit { code: None, signal: None }, &reason);
            core.changed.notify_waiters();
        }
        core.save_pty(&st);
    }
    let result = record_pty(core, id).map(TerminalPublic);
    core.flush_saved().await?;
    result
}

pub(super) async fn terminal_label(core: Ctx<'_>, p: TerminalLabelParams) -> Result<TerminalPublic, RpcError> {
    let id = resolve_pty(core, &p.terminal)?;
    let trashed = {
        let mut st = core.lock();
        let was = st.pty_reg.terminals.get(&id).cloned();
        st.pty_reg.label(id, &labels(&p.labels))?;
        core.save_pty(&st);
        let labels = serde_json::to_value(&st.pty_reg.terminals[&id].labels).unwrap_or_default();
        core.emit_pty_from(&mut st, "labels", id, json!({"labels": labels}), was.as_ref());
        st.pty_reg.terminals[&id].labels.get("herdr.trashed").is_some_and(|s| !s.is_empty())
    };
    if trashed {
        let _ = core
            .drove
            .pool_tx
            .send(crate::drove::pool::PoolSignal::SessionClosed { address: crate::terminals::address(id) });
    }
    let result = record_pty(core, id).map(TerminalPublic);
    core.flush_saved().await?;
    result
}

pub(super) async fn terminal_read(core: Ctx<'_>, p: TerminalReadParams) -> Result<TerminalReadResult, RpcError> {
    let id = resolve_pty(core, &p.terminal)?;
    core.pty_saw(p.caller.as_deref(), id);
    let full_screen = core.lock().screens.get(&id).is_some_and(|s| s.alt_screen());
    if full_screen && p.lines > 0 {
        let lines = p.lines.min(FINISHED_LINES);
        let result = match core.stitch(id, lines).await {
            Some((text, complete)) => TerminalReadResult { text, complete, how: "stitched".into(), why: None },
            None => {
                let st = core.lock();
                let text = st.screens.get(&id).map(|s| crate::stitch::text(&s.screen_rows())).unwrap_or_default();
                let why = st
                    .pty_reg
                    .terminals
                    .get(&id)
                    .and_then(|t| t.status.as_ref())
                    .filter(|s| matches!(s.state.as_str(), "working" | "blocked" | "error"))
                    .map(|s| s.state.clone());
                TerminalReadResult { text, complete: false, how: "screen".into(), why }
            }
        };
        core.flush_saved().await?;
        return Ok(result);
    }
    let result = {
        let st = core.lock();
        let t = st.pty_reg.terminals.get(&id).expect("resolved");
        let lines = if t.is_live() { p.lines } else { p.lines.min(FINISHED_LINES) };
        let rows = t.rows as usize;
        let exact = t.screen == "exact";
        let text = core.screen_text(&st, id);
        let mut complete = exact && text.is_some();
        let text = text.unwrap_or_default();
        let all: Vec<&str> = text.trim_end_matches('\n').lines().collect();
        let start = all.len().saturating_sub(rows + lines);
        let kept = all.len().saturating_sub(rows).min(lines);
        if p.lines > kept && t.printed > (kept + rows) as u64 {
            complete = false;
        }
        if p.lines > 0 && !t.is_live() && t.full_screen {
            complete = false;
        }
        TerminalReadResult {
            text: all[start..].join("\n"),
            complete,
            how: if p.lines == 0 { "screen" } else { "history" }.into(),
            why: None,
        }
    };
    core.flush_saved().await?;
    Ok(result)
}

pub(super) async fn terminal_attach(core: Ctx<'_>, p: AttachParams) -> Result<AttachResult, RpcError> {
    let Some(conn) = core.conn else {
        return Err(RpcError::new(
            "needs_framed",
            "attach needs a framed connection",
            "send hello with framed: true first, or use `h2 attach`",
        ));
    };
    let id = resolve_pty(core, &p.terminal)?;
    if p.caller.as_deref().and_then(crate::terminals::parse_id) == Some(id) {
        return Err(RpcError::new(
            "refused",
            format!(
                "{} is the terminal you are in: attached to itself, it would show its own output forever",
                crate::terminals::address(id)
            ),
            "attach from another terminal",
        ));
    }
    let req = crate::viewers::AttachRequest {
        observe: p.observe,
        resize_notifications: p.resize_notifications,
        history: p.history,
        size: (p.cols, p.rows),
        display: p.display,
        resumed: p.resumed,
    };
    let channel = core.attach(conn, id, req)?;
    Ok(AttachResult {
        channel,
        terminal: TerminalPublic(record_pty(core, id)?),
        resize_notifications: p.observe && p.resize_notifications,
        automatic_input: true,
    })
}

pub(super) async fn terminal_send(core: Ctx<'_>, p: TerminalSendParams) -> Result<TerminalSendResult, RpcError> {
    let timeout = wait_timeout(core, p.timeout)?;
    let id = resolve_pty(core, &p.terminal)?;
    if is_acp_caller(p.caller.as_deref()) {
        return Err(RpcError::invalid_params(format!(
            "ACP agent cannot send terminal input to {}; terminal agents only receive messages via `h2 wait` / `h2 read`",
            crate::terminals::address(id)
        )));
    }
    let text = core.pty_tagged(p.caller.as_deref(), id, &p.text, false);
    let sent = core.pty_send(id, &text, p.force, p.only_if.map(|w| word(&w)).as_deref()).await?;
    core.clear_manual_status_key(TargetKey::Terminal(id));
    core.clear_manual_status_caller(p.caller.as_deref());
    let target_addr = crate::terminals::address(id);
    if let Some(caller_key) = p.caller.as_deref().and_then(TargetKey::parse) {
        let mut st = core.lock();
        match caller_key {
            TargetKey::Agent(child) => {
                if let Some(c) =
                    st.reg.agents.get_mut(&child).filter(|c| c.parent.as_deref() == Some(target_addr.as_str()))
                {
                    c.told_parent = true;
                }
            }
            TargetKey::Terminal(child) => {
                if let Some(c) =
                    st.pty_reg.terminals.get_mut(&child).filter(|c| c.parent.as_deref() == Some(target_addr.as_str()))
                {
                    c.told_parent = true;
                }
            }
        }
    }
    if sent.status.as_deref() == Some("working") {
        let mut st = core.lock();
        let from = p
            .caller
            .as_deref()
            .and_then(TargetKey::parse)
            .and_then(|k| match k {
                TargetKey::Agent(c) => st.reg.agents.get(&c).map(|a| a.address.clone()),
                TargetKey::Terminal(c) => st.pty_reg.terminals.get(&c).map(|t| t.address.clone()),
            })
            .or_else(|| p.caller.clone().filter(|c| c.contains('/')));
        st.messages.insert(TargetKey::Terminal(id), from);
        drop(st);
        core.changed.notify_waiters();
    }
    core.pty_saw(p.caller.as_deref(), id);
    if let Some(caller) = &p.caller {
        core.lock().last_send.insert((caller.clone(), TargetKey::Terminal(id)), sent.turn);
    }
    let target = TargetKey::Terminal(id);
    let wait = if p.wait && sent.started == Some(false) {
        Some(core.wait_result(target, false, "not_started", None))
    } else if p.wait {
        let spec = WaitSpec { output: None, timeout: Some(timeout), after_turn: Some(sent.turn) };
        Some(watched(core, p.caller.clone(), target, spec).await)
    } else {
        None
    };
    core.flush_saved().await?;
    Ok(TerminalSendResult { status: sent.status, turn: sent.turn, started: sent.started, wait })
}

pub(super) async fn terminal_wait(core: Ctx<'_>, p: TerminalWaitParams) -> Result<WaitResult, RpcError> {
    wait_impl(core, p.terminal, p.children, p.output, p.timeout, false, p.caller, true).await
}

pub(super) async fn terminal_put(core: Ctx<'_>, p: TerminalPutParams) -> Result<PutResult, RpcError> {
    let too_big = || RpcError::invalid_params(format!("at most {} MB per put", crate::put::MAX_BYTES >> 20));
    if p.data.len() / 4 * 3 > crate::put::MAX_BYTES + 3 {
        return Err(too_big());
    }
    let bytes = crate::put::decode(&p.data).ok_or_else(|| RpcError::invalid_params("data is not base64"))?;
    if bytes.len() > crate::put::MAX_BYTES {
        return Err(too_big());
    }
    let id = resolve_pty(core, &p.terminal)?;
    if !record_pty(core, id)?.is_live() {
        return Err(RpcError::new(
            "finished",
            format!("{} has finished", crate::terminals::address(id)),
            "nothing there to read it",
        ));
    }
    let path = crate::put::stage_terminal(&core.paths.state, id, &p.name, &bytes)
        .map_err(|e| RpcError::internal(format!("cannot write the file: {e}")))?;
    Ok(PutResult { path: path.display().to_string(), bytes: bytes.len() })
}

pub(super) async fn terminal_keys(core: Ctx<'_>, p: KeysParams) -> Result<KeysResult, RpcError> {
    let id = resolve_pty(core, &p.terminal)?;
    if is_acp_caller(p.caller.as_deref()) {
        return Err(RpcError::invalid_params(format!(
            "ACP agent cannot send keys to {}; terminal agents only receive messages via `h2 wait` / `h2 read`",
            crate::terminals::address(id)
        )));
    }
    let (status, turn) = core.keys(id, &p.keys).await?;
    core.clear_manual_status_key(TargetKey::Terminal(id));
    core.clear_manual_status_caller(p.caller.as_deref());
    core.pty_saw(p.caller.as_deref(), id);
    core.flush_saved().await?;
    Ok(KeysResult { status, turn })
}

pub(super) async fn terminal_interrupt(
    core: Ctx<'_>,
    p: TerminalInterruptParams,
) -> Result<TerminalInterruptResult, RpcError> {
    use crate::pty_input::Interrupted;
    let timeout = seconds(p.timeout)?.unwrap_or(Duration::from_secs(10));
    let id = resolve_pty(core, &p.terminal)?;
    let (outcome, key) = core.pty_interrupt(id, timeout).await?;
    core.clear_manual_status_key(TargetKey::Terminal(id));
    let outcome = match outcome {
        Interrupted::Stopped => "stopped",
        Interrupted::Exited => "exited",
        Interrupted::StillWorking => "still_working",
        Interrupted::Unknown => "unknown",
    };
    core.flush_saved().await?;
    Ok(TerminalInterruptResult { outcome: outcome.into(), key })
}

async fn start_pty(
    core: &Arc<Core>,
    p: TerminalRunParams,
    template: Option<crate::templates::Template>,
) -> Result<Terminal, RpcError> {
    not_stopping(&core.lock())?;
    core.ensure_holder().await.map_err(|e| {
        RpcError::new(
            "unavailable",
            format!("no holder: {e}"),
            "run h2 server restart on this machine, then retry; see holder.log and core.log in the state directory",
        )
    })?;
    let settings = crate::config::load(&core.paths.config)?;
    let (cols, rows) = match p.size {
        Some(size) if size.cols > 0 && size.rows > 0 => (size.cols, size.rows),
        Some(_) => return Err(RpcError::invalid_params("size.cols and size.rows must be at least 1")),
        None => settings.headless()?,
    };
    let from_afar = p.caller.as_deref().is_some_and(|c| c.contains('/'));
    let cwd = crate::exec::folder(p.cwd.as_deref(), from_afar);
    let env = pty_child_env(crate::platform::program_env().await, p.env.clone());
    let identity = format!(
        "{} {}",
        env.get("TERM_PROGRAM").map_or("", String::as_str),
        env.get("TERM_PROGRAM_VERSION").map_or("", String::as_str)
    )
    .trim()
    .to_string();
    let parent = match (&p.parent, &p.caller) {
        (Some(parent), _) => Some(resolve_any(core, parent)?.address()),
        (None, Some(caller)) if !p.no_parent && from_afar => {
            caller.split_once('/').and_then(|(_, addr)| TargetKey::parse(addr)).map(|_| caller.clone())
        }
        (None, Some(caller)) if !p.no_parent => match TargetKey::parse(caller) {
            Some(TargetKey::Terminal(id)) if core.lock().pty_reg.terminals.contains_key(&id) => {
                Some(crate::terminals::address(id))
            }
            Some(TargetKey::Agent(id)) if core.lock().reg.agents.contains_key(&id) => Some(crate::agents::address(id)),
            _ => None,
        },
        _ => None,
    };
    let (id, rx) = {
        let mut st = core.lock();
        not_stopping(&st)?;
        if let Some(name) = p.name.as_deref()
            && st.reg.agents.values().any(|a| a.is_live() && a.name.as_deref() == Some(name))
        {
            return Err(RpcError::new(
                "name_taken",
                format!("name {name:?} is already taken by a live agent"),
                "pick another with --name",
            ));
        }
        let instance = st
            .current()
            .ok_or_else(|| {
                RpcError::new(
                    "unavailable",
                    "no holder: the holder disconnected before the terminal could start",
                    "retry; if it keeps failing, run h2 server restart on this machine and check holder.log",
                )
            })?
            .instance
            .clone();
        let new = NewTerminal {
            template,
            die_with_parent: parent.is_some() && !p.outlive_parent,
            parent,
            notify: !p.no_notify,
            rm: p.rm,
            keep: p.keep,
            name: p.name,
            labels: labels(&p.labels),
            argv: p.argv.clone(),
            cwd: cwd.clone(),
            cols,
            rows,
            identity: identity.clone(),
            lineage: p.lineage,
        };
        let id = st.pty_reg.create(new, now_ms())?;
        let mut screen = ghostty_vt::Terminal::new(cols, rows, settings.history_bytes());
        screen.set_xtversion(&identity);
        if let Some(display) = &p.display {
            display.apply(&mut screen);
        }
        st.screens.insert(id, screen);
        let (mut env, scope) = terminal_env(core, id, &cwd, env);
        if let Some(dark) = p.display.as_ref().and_then(|d| d.is_dark()) {
            env.entry("COLORFGBG".into()).or_insert_with(|| if dark { "15;0".into() } else { "0;15".into() });
        }
        let mut argv = p.argv;
        let prep = crate::pty_adapters::prepare_pty_launch(core, id, &mut argv, &mut env);
        let initial_saved_cli =
            crate::pty_adapters::extract_saved_cli_from_argv(&argv, None, prep.session_id.clone(), None);
        let addr = st.pty_reg.terminals[&id].address.clone();
        let mut tc = core.target_checkpoints(&mut st, &addr);
        tc.cli_kind = Some(prep.kind.as_str().into());
        if let Some(sid) = prep.session_id {
            tc.cli_session_id = Some(sid);
        }
        core.save_target_checkpoints(&mut st, tc);
        {
            let term = st.pty_reg.terminals.get_mut(&id).expect("just created");
            term.scope = Some(scope);
            term.argv = argv.clone();
            term.restart_env = p.env;
            term.holder = Some(instance);
            term.saved_cli = initial_saved_cli;
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        st.pending.insert(id, tx);
        let spawn = proto::Spawn { argv, env, cwd, cols, rows };
        core.save_pty(&st);
        st.link(id).expect("assigned holder").send(&proto::json_frame(proto::SPAWN, id, &spawn));
        (id, rx)
    };
    let spawn_ms = std::env::var("H2_SPAWN_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(10_000);
    let spawned = tokio::time::timeout(Duration::from_millis(spawn_ms), rx).await;
    let mut st = core.lock();
    match spawned {
        Ok(Ok(Ok(started))) => {
            drop(st);
            let core_bg = Arc::clone(core);
            tokio::spawn(async move {
                let _ = crate::checkpoint::capture_target_checkpoint(
                    &core_bg,
                    crate::runtime::TargetKey::Terminal(id),
                    Some(0),
                    None,
                    None,
                    true,
                    false,
                )
                .await;
            });
            Ok(started)
        }
        other => {
            st.pending.remove(&id);
            let keep = st.pty_reg.terminals.get(&id).is_some_and(|t| {
                t.keep && t.is_live() && (t.reason.is_some() || t.pid.is_some() || !matches!(&other, Ok(Ok(Err(_)))))
            });
            if keep {
                let t = st.pty_reg.terminals.get_mut(&id).expect("kept live record");
                if t.pid.is_none() && t.reason.is_none() {
                    t.reason = Some("spawn_pending".into());
                    core.save_pty(&st);
                }
            } else {
                st.screens.remove(&id);
                core.pty_remove(&mut st, id);
            }
            let error = match other {
                Ok(Ok(Err(e))) => e,
                _ => "the holder did not answer".into(),
            };
            Err(RpcError::new("spawn_failed", error, "check the program name and cwd"))
        }
    }
}

/// Per-program runtime fields, shared by initial PTY runs and replacement shells.
pub(crate) fn terminal_env(
    core: &Core,
    id: u32,
    cwd: &str,
    mut env: BTreeMap<String, String>,
) -> (BTreeMap<String, String>, String) {
    env.insert("H2_TERMINAL_ID".into(), crate::terminals::address(id));
    env.insert("H2_TERMINAL_SOCKET".into(), core.paths.socket.display().to_string());
    let scope = crate::sweep::token();
    env.insert(crate::sweep::VAR.into(), scope.clone());
    env.insert("PWD".into(), cwd.into());
    (env, scope)
}

/// Core environment prepared for PTY terminal programs (`TERM`, `COLORTERM`, `TERM_PROGRAM`).
pub(crate) fn pty_child_env(
    base: BTreeMap<String, String>,
    extra: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = base
        .into_iter()
        .filter(|(k, _)| {
            !IDENTITY_VARS.contains(&k.as_str()) && ![crate::sweep::VAR, crate::sweep::RUNTIME].contains(&k.as_str())
        })
        .collect();
    env.insert("TERM".into(), "xterm-256color".into());
    env.insert("COLORTERM".into(), "truecolor".into());
    env.insert("TERM_PROGRAM".into(), "ghostty".into());
    crate::platform::merge_env(&mut env, extra);
    if let Ok(session) = std::env::var("H2_SESSION") {
        env.insert("H2_SESSION".into(), session);
    }
    if let Some(lang) = crate::locale::for_programs(&env) {
        env.insert("LANG".into(), lang);
    }
    if env.get("TERM_PROGRAM").is_some_and(|p| p == "herdr2" || p == "herdr") {
        env.insert("TERM_PROGRAM".into(), "ghostty".into());
        env.remove("TERM_PROGRAM_VERSION");
    }
    env
}
