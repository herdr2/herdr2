//! The method table: every method's description, errors and handler. Its
//! params and results are in `api/types.rs`; `run` and the waits have their
//! own files.

use crate::agents::{Agent, NewAgent, Public};
use crate::rpc::{RpcError, methods};
use crate::runtime::{Core, Exit, now_ms};
use crate::wait::{WaitResult, WaitSpec};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;

pub const GENERATION: u32 = 1;
pub const FEATURES: &[&str] = &["framed", "events", "status", "acp", "mcp", "restart_environment"];
pub const FINISHED_LINES: usize = 1000;
/// Agent identity variables stripped unless explicitly passed by a caller.
pub const IDENTITY_VARS: &[&str] = &["H2_AGENT_ID", "H2_AGENT_SOCKET"];

mod doc;
mod run;
mod types;
use doc::*;
mod waiting;
use run::*;
pub(crate) use run::{agent_env, child_env, not_stopping, tail};
pub use types::*;
use waiting::*;

/// The whole API as an OpenRPC document, generated from the method table.
pub fn openrpc() -> serde_json::Value {
    let (methods, defs) = discover();
    crate::rpc::openrpc(methods, defs, &version(), GENERATION)
}

/// One call: the core plus the connection it came on.
pub struct Call<'a> {
    pub core: &'a Arc<Core>,
    /// The framed connection, if any.
    pub conn: Option<&'a Arc<crate::wire::Conn>>,
    /// Where a plain connection receives its subscription.
    pub plain: Option<&'a std::sync::Mutex<Option<PlainSub>>>,
}

impl std::ops::Deref for Call<'_> {
    type Target = Arc<Core>;
    fn deref(&self) -> &Arc<Core> {
        self.core
    }
}

type Ctx<'a> = &'a Call<'a>;

methods! { Ctx<'_>;
    "hello" => hello, HelloParams => HelloResult,
        "Handshake: protocol generation, core identity, methods and features. `framed: true` switches the connection to framed mode.",
        errors ["unsupported_generation"];
    "server.status" => server_status, Empty => StatusResult,
        "This core's identity, version, pid, uptime and agent counts.",
        errors [];
    "server.stop" => server_stop, Empty => StopResult,
        "Stop the core and its ACP agents. Kept agents recover when the core next starts.",
        errors [];
    "server.restart" => server_restart, RestartParams => StopResult,
        "Planned restart (e.g. after an update): live ACP sessions stop cleanly and kept agents resume on startup.",
        errors ["invalid_params"];
    "agent.run" => agent_run, RunParams => RunResult,
        "Start an ACP agent (preset, command, or template) with no PTY or terminal info, optionally queue its initial prompt, and return once the ACP session is ready.",
        errors ["invalid_params", "name_taken", "spawn_failed", "unavailable", "invalid_template", "not_found", "machine_unknown", "config"];
    "templates.list" => templates_list, Empty => TemplatesResult,
        "Templates by name and description, so you can pick a role without knowing its command.",
        errors [];
    "agent.list" => agent_list, ListParams => ListResult,
        "Live agents (all with `all`), optionally filtered by labels.",
        errors [];
    "agent.get" => agent_get, AgentParams => Public,
        "One agent's facts, live or finished.",
        errors ["not_found", "machine_unknown"];
    "agent.kill" => agent_kill, AgentParams => Public,
        "Terminate an ACP agent (HUP, then TERM, then KILL). Returns the observed record.",
        errors ["not_found", "machine_unknown"];
    "agent.rm" => agent_rm, AgentParams => RemoveResult,
        "Remove a finished agent.",
        errors ["not_found", "machine_unknown", "live"];
    "agent.keep" => agent_keep, KeepParams => Public,
        "Set whether a live agent resumes cleanly via `session/load` after crash or planned restart.",
        errors ["not_found", "invalid_params"];
    "agent.label" => agent_label, LabelParams => Public,
        "Set or remove labels (free-form `key=value`, never interpreted).",
        errors ["not_found", "machine_unknown", "invalid_params"];
    "agent.read" => agent_read, ReadParams => ReadResult,
        "Rendered JSONL transcript (`transcript`), `--last` final assistant message (`last`), or trailing history lines.",
        errors ["not_found", "machine_unknown", "invalid_params", "config"];
    "agent.send" => agent_send, SendParams => SendResult,
        "Enqueue a prompt into the agent's serial ACP inbox, or cancel the active turn and steer immediately with `steer`.",
        errors ["not_found", "machine_unknown", "finished", "unavailable", "blocked", "status_mismatch", "invalid_params", "config"];
    "agent.wait" => agent_wait, WaitParams => crate::wait::WaitResult,
        "Wait until an agent leaves `working` (idle, blocked, error, or exited). Already not working returns immediately. With `children`, wait on subordinate agents.",
        errors ["not_found", "machine_unknown", "invalid_params", "needs_target", "config"];
    "agent.interrupt" => agent_interrupt, InterruptParams => InterruptResult,
        "Send ACP `session/cancel` to abort the current turn cleanly without killing the agent process.",
        errors ["not_found", "machine_unknown", "finished", "unavailable", "invalid_params"];
    "agent.approve" => agent_approve, ApproveParams => ApproveResult,
        "Approve a blocked agent's pending ACP `session/request_permission` prompt.",
        errors ["not_found", "machine_unknown", "finished", "invalid_state", "invalid_params"];
    "agent.deny" => agent_deny, ApproveParams => ApproveResult,
        "Deny or cancel a blocked agent's pending ACP `session/request_permission` prompt.",
        errors ["not_found", "machine_unknown", "finished", "invalid_state", "invalid_params"];
    "agent.put" => agent_put, PutParams => PutResult,
        "Write bytes to a file beside a live agent, on its machine, for it to read. Lands in an isolated directory removed with the agent.",
        errors ["not_found", "machine_unknown", "finished", "invalid_params"];
    "machine.exec" => machine_exec, ExecParams => ExecResult,
        "Run one non-agent command here or on a machine (`on`); returns exit code and stdout/stderr (base64). `argv` runs directly, `line` in that machine's shell.",
        errors ["invalid_params", "spawn_failed", "machine_unknown", "unavailable", "needs_update"];
    "file.read" => file_read, FileReadParams => FileReadResult,
        "Read a text file here or on a machine (`on`): lines from `offset`, at most `limit` and 8 MB, and how many it has.",
        errors ["invalid_params", "not_found", "not_text", "io", "machine_unknown", "unavailable", "needs_update"];
    "file.write" => file_write, FileWriteParams => FileResult,
        "Write a file here or on a machine (`on`), making missing folders. Swapped atomically.",
        errors ["invalid_params", "io", "machine_unknown", "unavailable", "needs_update"];
    "file.edit" => file_edit, FileEditParams => FileResult,
        "Edit a text file here or on a machine (`on`): each `old` must match once; all or none.",
        errors ["invalid_params", "edit_failed", "not_found", "not_text", "io", "machine_unknown", "unavailable", "needs_update"];
    "doc.get" => doc_get, DocGetParams => DocGetResult,
        "Read an opaque JSON document and its version (zero when never written). Optional `on` selects another machine.",
        errors ["invalid_params", "machine_unknown", "unavailable", "needs_update"];
    "doc.set" => doc_set, DocSetParams => DocSetResult,
        "Write an opaque JSON document (at most 1 MiB) if its version still matches. Null deletes the value while retaining version.",
        errors ["invalid_params", "conflict", "internal", "machine_unknown", "unavailable", "needs_update"];
    "events.subscribe" => events_subscribe, SubscribeParams => SubscribeResult,
        "Snapshot of matching agents, then ordered fact changes (created, lost, restarted, keep, finished, removed, status, turn, title, labels, transcript, doc) as `event` notifications, `heartbeat` every 15 s, and `missed` + snapshot when behind.",
        errors ["not_found", "machine_unknown"];
    "machine.add" => machine_add, MachineAddParams => crate::machines::Machine,
        "Add another machine: dial it over ssh, check that h2 answers there, and keep it under a name.",
        errors ["invalid_params", "name_taken", "already_added", "unreachable", "h2_missing", "incompatible"];
    "machine.list" => machine_list, Empty => MachineListResult,
        "The machines this core has added.",
        errors [];
    "machine.connect" => machine_connect, MachineParams => crate::remote::MachineView,
        "Dial a machine now instead of at its next retry (after a login), and report its line.",
        errors ["machine_unknown"];
    "machine.rename" => machine_rename, MachineRenameParams => crate::machines::Machine,
        "Give a machine a new name here. Only this core's name for it changes.",
        errors ["invalid_params", "name_taken", "machine_unknown"];
    "machine.rm" => machine_rm, MachineParams => RemoveResult,
        "Forget a machine. Its agents keep running there.",
        errors ["machine_unknown"];
    "rpc.discover" => rpc_discover, Empty => serde_json::Value,
        "The whole API as an OpenRPC document: every method with its params, result and errors in JSON Schema.",
        errors [];
}

fn version() -> String {
    env!("CARGO_PKG_VERSION").into()
}

fn resolve(core: &Core, address: &str) -> Result<u32, RpcError> {
    let local = crate::addr::local(address, &core.info.id, &core.info.name)?;
    core.lock().reg.resolve(local)
}

fn record(core: &Core, id: u32) -> Result<Agent, RpcError> {
    core.lock().reg.agents.get(&id).cloned().ok_or_else(|| {
        RpcError::new("not_found", format!("{} was removed", crate::agents::address(id)), "see `h2 ls --all`")
    })
}

async fn hello(core: Ctx<'_>, p: HelloParams) -> Result<HelloResult, RpcError> {
    if let Some(g) = p.generation.filter(|g| *g != GENERATION) {
        return Err(RpcError::new(
            "unsupported_generation",
            format!("protocol generation {g} not supported"),
            format!("this core speaks generation {GENERATION}"),
        ));
    }
    let (keeps_running_after_logout, logout_linger_user) = crate::platform::logout_linger();
    Ok(HelloResult {
        generation: GENERATION,
        version: version(),
        core: core.info.clone(),
        framed: p.framed,
        methods: METHOD_NAMES.iter().map(|m| m.to_string()).collect(),
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
        login_session: core.login_session,
        keeps_running_after_logout,
        logout_linger_user,
    })
}

async fn server_status(core: Ctx<'_>, _: Empty) -> Result<StatusResult, RpcError> {
    let (keeps_running_after_logout, logout_linger_user) = crate::platform::logout_linger();
    let st = core.lock();
    let live = st.reg.agents.values().filter(|a| a.is_live()).count();
    Ok(StatusResult {
        core: core.info.clone(),
        version: version(),
        pid: std::process::id(),
        uptime_ms: core.started.elapsed().as_millis() as u64,
        live,
        finished: st.reg.agents.len() - live,
        installed_by: crate::client::installed_by().map(String::from),
        keeps_running_after_logout,
        logout_linger_user,
    })
}

async fn server_stop(core: Ctx<'_>, _: Empty) -> Result<StopResult, RpcError> {
    if core.restart.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(RpcError::new("unavailable", "the core is restarting", "run it again once it has restarted"));
    }
    crate::restart::discard(&core.paths);
    if let Err(e) = core.shutdown().await {
        let _ = writeln!(io::stderr(), "core: shutdown registry flush failed: {}", e.message);
    }
    Ok(StopResult { stopped: true })
}

async fn server_restart(core: Ctx<'_>, p: RestartParams) -> Result<StopResult, RpcError> {
    crate::restart::prepare(core, p.environment).map_err(|e| {
        if e.kind() == std::io::ErrorKind::InvalidInput {
            RpcError::invalid_params(e.to_string())
        } else {
            RpcError::internal(e.to_string())
        }
    })?;
    Ok(StopResult { stopped: true })
}

async fn templates_list(core: Ctx<'_>, _: Empty) -> Result<TemplatesResult, RpcError> {
    Ok(TemplatesResult { templates: crate::templates::list(&core.paths) })
}

async fn agent_list(core: Ctx<'_>, p: ListParams) -> Result<ListResult, RpcError> {
    let afar = from_afar(core);
    let (mut agents, mut hidden_finished, machines) = {
        let mut st = core.lock();
        core.prune(&mut st);
        let matching: Vec<&Agent> =
            st.reg.agents.values().filter(|a| p.labels.iter().all(|Label(k, v)| a.labels.get(k) == Some(v))).collect();
        let hidden = if p.all { 0 } else { matching.iter().filter(|a| !a.is_live()).count() };
        let agents: Vec<serde_json::Value> =
            matching.into_iter().filter(|a| p.all || a.is_live()).cloned().map(|a| json!(Public(a))).collect();
        let machines = if afar { Vec::new() } else { st.machines.clone() };
        (agents, hidden, machines)
    };
    let asks: Vec<_> = machines
        .iter()
        .map(|m| {
            let (core, m, params) = (core.core.clone(), m.clone(), serde_json::to_value(&p).unwrap_or_default());
            tokio::spawn(async move {
                let ask = crate::remote::forward(&core, &m, "agent.list", params);
                tokio::time::timeout(Duration::from_secs(3), ask).await.ok().and_then(Result::ok)
            })
        })
        .collect();
    for ask in asks {
        if let Ok(Some(r)) = ask.await {
            agents.extend(r["agents"].as_array().cloned().unwrap_or_default());
            hidden_finished += r["hidden_finished"].as_u64().unwrap_or(0) as usize;
        }
    }
    let machines = {
        let st = core.lock();
        machines.iter().map(|m| crate::remote::view(&st, m)).collect()
    };
    core.flush_saved().await?;
    Ok(ListResult { agents, hidden_finished, machines })
}

async fn agent_get(core: Ctx<'_>, p: AgentParams) -> Result<Public, RpcError> {
    let id = resolve(core, &p.agent)?;
    core.saw(p.caller.as_deref(), id);
    let result = record(core, id).map(Public);
    core.flush_saved().await?;
    result
}

async fn agent_kill(core: Ctx<'_>, p: AgentParams) -> Result<Public, RpcError> {
    let id = resolve(core, &p.agent)?;
    if core.lock().reg.agents.get(&id).is_some_and(|a| a.is_live()) {
        core.lock().killing.insert(id);
        core.kill(&[id]).await;
    }
    core.saw(p.caller.as_deref(), id);
    let result = record(core, id).map(Public);
    core.flush_saved().await?;
    result
}

async fn agent_rm(core: Ctx<'_>, p: AgentParams) -> Result<RemoveResult, RpcError> {
    let id = resolve(core, &p.agent)?;
    {
        let mut st = core.lock();
        if st.reg.agents.get(&id).is_some_and(|a| a.is_live()) {
            return Err(RpcError::new(
                "live",
                format!("{} is still running", crate::agents::address(id)),
                format!("kill it first: h2 kill {}", crate::agents::address(id)),
            ));
        }
        core.remove(&mut st, id);
    }
    core.flush_saved().await?;
    Ok(RemoveResult { removed: crate::agents::address(id) })
}

async fn agent_keep(core: Ctx<'_>, p: KeepParams) -> Result<Public, RpcError> {
    let id = resolve(core, &p.agent)?;
    {
        let mut st = core.lock();
        let a = st.reg.agents.get_mut(&id).expect("resolved");
        if !a.is_live() {
            return Err(RpcError::invalid_params("only live agents can be kept"));
        }
        a.keep = p.keep;
        let lost = (!p.keep && a.pid.is_none()).then(|| a.reason.clone().unwrap_or_else(|| "crashed".into()));
        core.emit(&mut st, "keep", id, json!({"keep": p.keep}));
        if let Some(reason) = lost {
            core.record_finish(&mut st, id, Exit::default(), &reason);
            core.changed.notify_waiters();
        }
        core.save(&st);
    }
    let result = record(core, id).map(Public);
    core.flush_saved().await?;
    result
}

async fn agent_label(core: Ctx<'_>, p: LabelParams) -> Result<Public, RpcError> {
    let id = resolve(core, &p.agent)?;
    {
        let mut st = core.lock();
        let was = st.reg.agents.get(&id).cloned();
        st.reg.label(id, &labels(&p.labels))?;
        core.save(&st);
        let labels = serde_json::to_value(&st.reg.agents[&id].labels).unwrap_or_default();
        core.emit_from(&mut st, "labels", id, json!({"labels": labels}), was.as_ref());
    }
    let result = record(core, id).map(Public);
    core.flush_saved().await?;
    result
}

async fn agent_read(core: Ctx<'_>, p: ReadParams) -> Result<ReadResult, RpcError> {
    let timeout = if p.wait { Some(wait_timeout(core, p.timeout)?) } else { None };
    let id = resolve(core, &p.agent)?;
    if let Some(timeout) = timeout {
        let spec = WaitSpec { output: None, timeout, after_turn: None };
        let _ = watched(core, p.caller.clone(), id, spec).await;
    } else {
        core.saw(p.caller.as_deref(), id);
    }
    let (text, complete, how) = core.read_transcript(id, p.lines, p.last)?;
    let entries = if !p.last && p.lines.is_none() {
        core.read_transcript_entries(id)?
    } else {
        Vec::new()
    };
    core.flush_saved().await?;
    Ok(ReadResult { text, complete, how: how.into(), why: None, entries })
}

async fn agent_send(core: Ctx<'_>, p: SendParams) -> Result<SendResult, RpcError> {
    let timeout = wait_timeout(core, p.timeout)?;
    let id = resolve(core, &p.agent)?;
    let sender = p.from.as_deref().or(p.caller.as_deref());
    let text = if p.text.trim_start().starts_with("<h2") { p.text.clone() } else { core.tagged(sender, id, &p.text, false) };
    let sent = core.send(id, &text, p.steer, p.only_if.map(|w| word(&w)).as_deref(), sender).await?;
    if let Some(child) = sender.and_then(crate::agents::parse_id) {
        let mut st = core.lock();
        if let Some(c) =
            st.reg.agents.get_mut(&child).filter(|c| c.parent.as_deref() == Some(crate::agents::address(id).as_str()))
        {
            c.told_parent = true;
        }
    }
    if sent.status.as_deref() == Some("working") {
        let mut st = core.lock();
        let from = sender
            .and_then(crate::agents::parse_id)
            .and_then(|c| st.reg.agents.get(&c))
            .map(|a| a.address.clone())
            .or_else(|| sender.filter(|c| c.contains('/')).map(str::to_string));
        st.messages.insert(id, from);
        drop(st);
        core.changed.notify_waiters();
    }
    core.saw(p.caller.as_deref(), id);
    if let Some(caller) = &p.caller {
        core.lock().last_send.insert((caller.clone(), id), sent.after_turn);
    }
    let wait = if p.wait && sent.started == Some(false) {
        Some(core.wait_result(id, false, "not_started", None))
    } else if p.wait {
        let spec = WaitSpec { output: None, timeout, after_turn: Some(sent.after_turn) };
        Some(watched(core, p.caller.clone(), id, spec).await)
    } else {
        None
    };
    core.flush_saved().await?;
    Ok(SendResult { status: sent.status, turn: sent.turn, started: sent.started, inbox_len: sent.inbox_len, wait })
}

async fn agent_approve(core: Ctx<'_>, p: ApproveParams) -> Result<ApproveResult, RpcError> {
    resolve_perm(core, p, true).await
}

async fn agent_deny(core: Ctx<'_>, p: ApproveParams) -> Result<ApproveResult, RpcError> {
    resolve_perm(core, p, false).await
}

async fn resolve_perm(core: Ctx<'_>, p: ApproveParams, approve: bool) -> Result<ApproveResult, RpcError> {
    let id = resolve(core, &p.agent)?;
    let (request_id, option_id) = core.resolve_permission(id, approve, p.option.as_deref())?;
    core.saw(p.caller.as_deref(), id);
    core.flush_saved().await?;
    let outcome = if approve { "approved" } else { "denied" }.into();
    Ok(ApproveResult { agent: crate::agents::address(id), request_id, option_id, outcome })
}

async fn machine_exec(core: Ctx<'_>, p: ExecParams) -> Result<ExecResult, RpcError> {
    crate::exec::run(core, p, from_afar(core)).await
}

fn from_afar(core: Ctx<'_>) -> bool {
    core.conn.is_some_and(|conn| crate::remote::is_peer(core, conn))
}

async fn file_read(core: Ctx<'_>, p: FileReadParams) -> Result<FileReadResult, RpcError> {
    let afar = from_afar(core);
    blocking(move || crate::files::read(p, afar)).await
}

async fn file_write(core: Ctx<'_>, p: FileWriteParams) -> Result<FileResult, RpcError> {
    let afar = from_afar(core);
    blocking(move || crate::files::write(p, afar)).await
}

async fn file_edit(core: Ctx<'_>, p: FileEditParams) -> Result<FileResult, RpcError> {
    let afar = from_afar(core);
    blocking(move || crate::files::edit(p, afar)).await
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, RpcError> + Send + 'static) -> Result<T, RpcError> {
    tokio::task::spawn_blocking(f).await.map_err(|e| RpcError::internal(e.to_string()))?
}

async fn agent_put(core: Ctx<'_>, p: PutParams) -> Result<PutResult, RpcError> {
    let too_big = || RpcError::invalid_params(format!("at most {} MB per put", crate::put::MAX_BYTES >> 20));
    if p.data.len() / 4 * 3 > crate::put::MAX_BYTES + 3 {
        return Err(too_big());
    }
    let bytes = crate::put::decode(&p.data).ok_or_else(|| RpcError::invalid_params("data is not base64"))?;
    if bytes.len() > crate::put::MAX_BYTES {
        return Err(too_big());
    }
    let id = resolve(core, &p.agent)?;
    if !record(core, id)?.is_live() {
        return Err(RpcError::new(
            "finished",
            format!("{} has finished", crate::agents::address(id)),
            "nothing there to read it",
        ));
    }
    let path = crate::put::stage(&core.paths.state, id, &p.name, &bytes)
        .map_err(|e| RpcError::internal(format!("cannot write the file: {e}")))?;
    Ok(PutResult { path: path.display().to_string(), bytes: bytes.len() })
}

async fn agent_interrupt(core: Ctx<'_>, p: InterruptParams) -> Result<InterruptResult, RpcError> {
    use crate::input::Interrupted;
    let timeout = seconds(p.timeout)?.unwrap_or(Duration::from_secs(10));
    let id = resolve(core, &p.agent)?;
    let (outcome, method) = core.interrupt(id, timeout).await?;
    let outcome = match outcome {
        Interrupted::Stopped => "stopped",
        Interrupted::Exited => "exited",
        Interrupted::StillWorking => "still_working",
        Interrupted::Unknown => "unknown",
    };
    core.flush_saved().await?;
    Ok(InterruptResult { outcome: outcome.into(), method })
}

async fn events_subscribe(core: Ctx<'_>, p: SubscribeParams) -> Result<SubscribeResult, RpcError> {
    use crate::events::{Filter, Sink};
    let address = |a: &str| match a.split_once('/').filter(|(m, _)| core.lock().machines.iter().any(|x| x.name == *m)) {
        Some(_) => Ok(a.to_string()),
        None => resolve(core, a).map(crate::agents::address),
    };
    let agent = p.agent.as_deref().map(address).transpose()?;
    let children_of = p.children_of.as_deref().map(address).transpose()?;
    let filter = Filter { agent, children_of, labels: labels(&p.labels), docs: p.docs, here_only: from_afar(core) };
    let (sink, channel) = match (core.conn, core.plain) {
        (Some(conn), _) => {
            let channel = conn.new_channel();
            conn.with(|out| out.open(channel));
            (Sink::Framed { conn: conn.clone(), channel }, Some(channel))
        }
        (None, Some(slot)) => {
            let (tx, rx) = tokio::sync::mpsc::channel(crate::events::PLAIN_QUEUE);
            let behind = Arc::new(std::sync::atomic::AtomicBool::new(false));
            *slot.lock().unwrap_or_else(|p| p.into_inner()) =
                Some(PlainSub { rx, behind: behind.clone(), filter: filter.clone() });
            (Sink::Plain { tx, behind }, None)
        }
        (None, None) => return Err(RpcError::internal("no connection to deliver events on")),
    };
    let (seq, agents) = {
        let mut st = core.lock();
        let agents = core.matching(&st, &filter);
        core.subscribe(&mut st, filter, sink);
        (st.events.seq, agents)
    };
    if let Some(channel) = channel {
        crate::server::heartbeat(core.core.clone(), core.conn.expect("framed").clone(), channel);
    }
    Ok(SubscribeResult { seq, agents, channel })
}

/// A machine name that is free: valid, not another machine's, not this core's own.
fn free_name(core: &Core, st: &crate::runtime::State, name: &str) -> Result<(), RpcError> {
    crate::addr::check_name(name)?;
    if name == core.info.name || name == core.info.id || st.machines.iter().any(|m| m.name == name) {
        return Err(RpcError::new(
            "name_taken",
            format!("{name:?} is already a machine here"),
            "pick another with --name",
        ));
    }
    Ok(())
}

async fn machine_add(core: Ctx<'_>, p: MachineAddParams) -> Result<crate::machines::Machine, RpcError> {
    let (target, via) = match (p.target, p.via) {
        (Some(target), None) => (crate::machines::ssh_target(target), false),
        (None, Some(command)) => (command, true),
        _ => return Err(RpcError::invalid_params("give either target or via")),
    };
    let to = crate::machines::Reach { target: &target, via };
    crate::machines::check_target(to)?;
    if let Some(session) = &p.session {
        crate::addr::check_name(session)?;
    }
    let name = |st: &crate::runtime::State| match &p.name {
        Some(name) => free_name(core, st, name).map(|_| name.clone()),
        None => (!via)
            .then(|| crate::machines::default_name(&target, |name| free_name(core, st, name).is_err()))
            .flatten()
            .ok_or_else(|| {
                RpcError::invalid_params(format!("can't make a name from {target:?}")).with_hint("give one with --name")
            }),
    };
    name(&core.lock())?;
    let hello = crate::machines::hello(&core.paths, to, p.session.as_deref()).await?;
    let text = |k: &str| hello["core"][k].as_str().unwrap_or_default().to_string();
    let mut st = core.lock();
    let machine = crate::machines::Machine {
        name: name(&st)?,
        target,
        via,
        temporary: p.temporary,
        session: p.session,
        id: text("id"),
        host: text("name"),
        version: hello["version"].as_str().unwrap_or_default().into(),
        keeps_running_after_logout: hello["keeps_running_after_logout"].as_bool(),
        logout_linger_user: hello["logout_linger_user"].as_str().map(String::from),
    };
    if machine.id == core.info.id {
        return Err(RpcError::invalid_params(format!("{} is this core", machine.target))
            .with_hint("add another machine, or another --session"));
    }
    if let Some(m) = st.machines.iter().find(|m| m.id == machine.id) {
        if (m.target.as_str(), m.via) != (machine.target.as_str(), machine.via) {
            return Err(RpcError::new(
                "already_added",
                format!("{} answers as {}'s core: it looks like a copy of {}", machine.target, m.name, m.name),
                "on the copy, stop h2 and delete its core.json in h2's state folder, then add it again",
            ));
        }
        return Err(RpcError::new(
            "already_added",
            format!("that core is already added as {}", m.name),
            format!("rename it: h2 machine rename {} <name>", m.name),
        ));
    }
    st.machines.push(machine.clone());
    crate::machines::save(&core.paths.state, &st.machines);
    drop(st);
    crate::remote::keep_up(core.core, machine.id.clone());
    Ok(machine)
}

async fn machine_list(core: Ctx<'_>, _: Empty) -> Result<MachineListResult, RpcError> {
    let st = core.lock();
    Ok(MachineListResult { machines: st.machines.iter().map(|m| crate::remote::view(&st, m)).collect() })
}

async fn machine_connect(core: Ctx<'_>, p: MachineParams) -> Result<crate::remote::MachineView, RpcError> {
    let m = {
        let st = core.lock();
        st.machines
            .iter()
            .find(|m| m.name == p.machine)
            .cloned()
            .ok_or_else(|| crate::machines::unknown(&st.machines, &p.machine))?
    };
    let connected = matches!(core.lock().lines.get(&m.id), Some(crate::remote::LineState::Connected(_)));
    if !connected {
        crate::remote::keep_up(core.core, m.id.clone());
        crate::remote::dialed(core, &m.id).await;
    }
    Ok(crate::remote::view(&core.lock(), &m))
}

async fn machine_rename(core: Ctx<'_>, p: MachineRenameParams) -> Result<crate::machines::Machine, RpcError> {
    let mut st = core.lock();
    let i = st
        .machines
        .iter()
        .position(|m| m.name == p.machine)
        .ok_or_else(|| crate::machines::unknown(&st.machines, &p.machine))?;
    if p.new_name != p.machine {
        free_name(core, &st, &p.new_name)?;
    }
    let id = st.machines[i].id.clone();
    st.machines[i].name = p.new_name.clone();
    if p.new_name != p.machine {
        crate::remote::rename_mirror(core, &mut st, &id, &p.machine, &p.new_name);
    }
    crate::machines::save(&core.paths.state, &st.machines);
    Ok(st.machines[i].clone())
}

async fn machine_rm(core: Ctx<'_>, p: MachineParams) -> Result<RemoveResult, RpcError> {
    let mut st = core.lock();
    let i = st
        .machines
        .iter()
        .position(|m| m.name == p.machine)
        .ok_or_else(|| crate::machines::unknown(&st.machines, &p.machine))?;
    let gone = st.machines.remove(i);
    crate::remote::forget(core, &mut st, &gone.id);
    crate::machines::save(&core.paths.state, &st.machines);
    let last_on_target = !st.machines.iter().any(|m| m.target == gone.target);
    drop(st);
    if last_on_target {
        crate::machines::logout(&core.paths, gone.reach());
    }
    Ok(RemoveResult { removed: gone.name })
}

async fn rpc_discover(_: Ctx<'_>, _: Empty) -> Result<serde_json::Value, RpcError> {
    Ok(openrpc())
}
