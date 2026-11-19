//! The method table: every method's description, errors and handler. Its
//! params and results are in `api/types.rs`; `run` and the waits have their
//! own files.

use crate::agents::{Agent, NewAgent, Public};
use crate::rpc::{RpcError, methods};
use crate::runtime::{Core, Exit, TargetKey, now_ms};
use crate::wait::{WaitResult, WaitSpec};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;

pub const GENERATION: u32 = 1;
pub const FEATURES: &[&str] = &[
    "framed",
    "events",
    "status",
    "acp",
    "mcp",
    "attach",
    "shell",
    "resize_notifications",
    "automatic_input",
    "restart_environment",
    "drove",
];
pub const FINISHED_LINES: usize = 1000;
/// Agent and terminal identity variables stripped unless explicitly passed by a caller.
pub const IDENTITY_VARS: &[&str] = &[
    "H2_AGENT_ID",
    "H2_AGENT_SOCKET",
    "H2_TERMINAL_ID",
    "H2_TERMINAL_SOCKET",
    "ANTIGRAVITY_SOURCE_METADATA",
    "AGY_LLM_GATEWAY_PROXY_URL",
];

mod doc;
mod handoff;
mod pty;
pub(crate) mod run;
mod types;
use doc::*;
use pty::*;
pub(crate) use pty::{pty_child_env, terminal_env};
mod waiting;
use run::*;
pub(crate) use run::{agent_env, child_env, not_stopping, tail_address};
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
    "agent.inbox_edit" => agent_inbox_edit, InboxEditParams => Public,
        "Edit a queued prompt in an ACP agent's serial inbox before its turn starts.",
        errors ["not_found", "machine_unknown", "finished", "invalid_params"];
    "agent.inbox_remove" => agent_inbox_remove, InboxRemoveParams => Public,
        "Remove a queued prompt from an ACP agent's serial inbox before its turn starts.",
        errors ["not_found", "machine_unknown", "finished"];
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
    "agent.config" => agent_config, ConfigParams => Public,
        "Set a live ACP session configuration option (model, reasoning effort, fast mode, session mode) and return the updated agent record.",
        errors ["not_found", "machine_unknown", "finished", "unavailable", "invalid_params", "spawn_failed"];
    "agent.put" => agent_put, PutParams => PutResult,
        "Write bytes to a file beside a live agent, on its machine, for it to read. Lands in an isolated directory removed with the agent.",
        errors ["not_found", "machine_unknown", "finished", "invalid_params"];
    "agent.handoff" => handoff::agent_handoff, HandoffParams => HandoffResult,
        "Hand off, fork, open a side chat, or merge back session context across ACP agents and PTY terminals.",
        errors ["not_found", "machine_unknown", "invalid_params", "name_taken", "config", "spawn_failed", "unavailable", "stopping"];
    "terminal.run" => terminal_run, TerminalRunParams => TerminalRunResult,
        "Start a program in a pseudo-terminal (`shell`, command, or template). With `wait`, wait for it to finish and return its screen.",
        errors ["invalid_params", "name_taken", "spawn_failed", "unavailable", "invalid_template", "not_found", "machine_unknown", "config"];
    "terminal.list" => terminal_list, ListParams => TerminalListResult,
        "Live interactive PTY terminals (all with `all`), optionally filtered by labels.",
        errors [];
    "terminal.get" => terminal_get, TerminalParams => crate::terminals::Public,
        "One terminal's facts, live or finished.",
        errors ["not_found", "machine_unknown"];
    "terminal.kill" => terminal_kill, TerminalParams => crate::terminals::Public,
        "End a terminal's program (HUP, then TERM, then KILL). Returns the observed record.",
        errors ["not_found", "machine_unknown"];
    "terminal.rm" => terminal_rm, TerminalParams => RemoveResult,
        "Remove a finished terminal.",
        errors ["not_found", "machine_unknown", "live"];
    "terminal.keep" => terminal_keep, TerminalKeepParams => crate::terminals::Public,
        "Keep a live terminal recoverable after crash or restart by launching a replacement shell.",
        errors ["not_found", "invalid_params"];
    "terminal.label" => terminal_label, TerminalLabelParams => crate::terminals::Public,
        "Set or remove labels (free-form `key=value`, never interpreted).",
        errors ["not_found", "machine_unknown", "invalid_params"];
    "terminal.read" => terminal_read, TerminalReadParams => TerminalReadResult,
        "A terminal's screen, or scrollback history lines.",
        errors ["not_found", "machine_unknown"];
    "terminal.attach" => terminal_attach, AttachParams => AttachResult,
        "Open a live terminal viewer stream on this framed connection.",
        errors ["not_found", "machine_unknown", "finished", "needs_framed", "refused"];
    "terminal.send" => terminal_send, TerminalSendParams => TerminalSendResult,
        "Paste text into a live terminal and submit with Enter.",
        errors ["not_found", "machine_unknown", "finished", "unavailable", "blocked", "status_mismatch", "invalid_params", "config"];
    "terminal.wait" => terminal_wait, TerminalWaitParams => crate::wait::WaitResult,
        "Wait until a terminal leaves `working` (done, idle, blocked, error, exited) or matches `output`.",
        errors ["not_found", "machine_unknown", "invalid_params", "needs_target", "config"];
    "terminal.keys" => terminal_keys, KeysParams => KeysResult,
        "Send named keys (enter, esc, tab, ctrl+c, ...) to a live terminal.",
        errors ["not_found", "machine_unknown", "finished", "unavailable", "invalid_params", "config"];
    "terminal.interrupt" => terminal_interrupt, TerminalInterruptParams => TerminalInterruptResult,
        "Send interrupt key (Esc or Ctrl-C) to a live terminal and watch for an effect.",
        errors ["not_found", "machine_unknown", "finished", "unavailable", "invalid_params"];
    "terminal.put" => terminal_put, TerminalPutParams => PutResult,
        "Write bytes to a file beside a live terminal for its program to read.",
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
    "drove.settings.get" => drove_settings_get, DroveOnParams => crate::drove::types::DroveSettingsResult,
        "Read Drove roles, AI configuration, optional Magpie settings, and known ACP agents.",
        errors ["machine_unknown", "unavailable"];
    "drove.settings.set" => drove_settings_set, crate::drove::types::DroveSettingsSetParams => crate::drove::types::DroveSettingsResult,
        "Update Drove configuration and secrets (0600).",
        errors ["invalid_params", "machine_unknown", "unavailable"];
    "drove.probe" => drove_probe, DroveProbeParams => crate::drove::types::ProbeSnapshot,
        "Probe installed ACP agents and discover their model and effort options.",
        errors ["machine_unknown", "unavailable"];
    "drove.ai.test" => drove_ai_test, DroveAiTestParams => crate::drove::probe::AiTestResult,
        "Test generic OpenAI-compatible AI endpoint URL and API key.",
        errors ["invalid_params", "machine_unknown", "unavailable"];
    "drove.roles.ai_configure" => drove_roles_ai_configure, crate::drove::types::DroveRolesAiConfigureParams => crate::drove::types::DroveRolesAiConfigureResult,
        "Use the configured AI assistant model to automatically configure Drove roles, models, and candidate strengths.",
        errors ["invalid_params", "machine_unknown", "unavailable"];
    "drove.magpie.test" => drove_magpie_test, DroveMagpieTestParams => crate::drove::magpie::MagpieTestResult,
        "Test optional Magpie quota gateway connectivity.",
        errors ["invalid_params", "machine_unknown", "unavailable"];
    "drove.web.open" => drove_web_open, DroveWebOpenParams => crate::drove::types::DroveWebOpenResult,
        "Return or open the Drove Web UI URL (proxying open requests back to desktop peers over SSH).",
        errors ["invalid_params", "machine_unknown", "unavailable"];
    "drove.http_proxy" => drove_http_proxy, crate::drove::http::HttpProxyRequest => crate::drove::http::HttpProxyResponse,
        "Internal HTTP federation proxy for Drove Web across connected SSH machines.",
        errors ["invalid_params", "machine_unknown", "unavailable"];
    "drove.subscribe" => drove_subscribe, Empty => DroveSubscribeResult,
        "Subscribe to Drove change and item events on a framed connection.",
        errors ["needs_framed"];
    "checkpoint.list" => checkpoint_list, CheckpointListParams => CheckpointListResult,
        "List hidden Git ref checkpoints and workspace isolation status for an agent or terminal.",
        errors ["not_found", "machine_unknown", "invalid_params"];
    "checkpoint.capture" => checkpoint_capture, CheckpointCaptureParams => CheckpointCaptureResult,
        "Capture a manual hidden Git ref checkpoint for an agent or terminal without modifying the user's index.",
        errors ["not_found", "machine_unknown", "needs_target", "invalid_params"];
    "checkpoint.diff" => checkpoint_diff, CheckpointDiffParams => crate::checkpoint::CheckpointDiff,
        "Compute the pure authored unified diff and file stats between two checkpoints, excluding upstream pull/merge noise.",
        errors ["not_found", "machine_unknown", "invalid_params"];
    "checkpoint.rollback" => checkpoint_rollback, CheckpointRollbackParams => crate::checkpoint::CheckpointRollbackResult,
        "Roll back an agent or terminal session (and optionally workspace files) to an earlier checkpoint turn.",
        errors ["not_found", "machine_unknown", "invalid_params", "busy", "unavailable", "confirm_required", "shared_workspace", "spawn_failed"];
    "checkpoint.hook" => checkpoint_hook, CheckpointHookParams => CheckpointHookResult,
        "Internal lifecycle hook callback invoked by instrumented terminal CLIs (Claude, Codex, Agy).",
        errors ["not_found", "invalid_params"];
    "rpc.discover" => rpc_discover, Empty => serde_json::Value,
        "The whole API as an OpenRPC document: every method with its params, result and errors in JSON Schema.",
        errors [];
}

fn version() -> String {
    env!("CARGO_PKG_VERSION").into()
}

pub(super) fn resolve(core: &Core, address: &str) -> Result<u32, RpcError> {
    let local = crate::addr::local(address, &core.info.id, &core.info.name)?;
    core.lock().reg.resolve(local)
}

pub(crate) fn resolve_any(core: &Core, address: &str) -> Result<TargetKey, RpcError> {
    let local = crate::addr::local(address, &core.info.id, &core.info.name)?;
    let st = core.lock();
    match st.reg.resolve(local) {
        Ok(id) => Ok(TargetKey::Agent(id)),
        Err(agent_err) => match st.pty_reg.resolve(local) {
            Ok(id) => Ok(TargetKey::Terminal(id)),
            Err(_) => Err(agent_err),
        },
    }
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
    let live_agents = st.reg.agents.values().filter(|a| a.is_live()).count();
    let live_terminals = st.pty_reg.terminals.values().filter(|t| t.is_live()).count();
    let total = st.reg.agents.len() + st.pty_reg.terminals.len();
    let live = live_agents + live_terminals;
    Ok(StatusResult {
        core: core.info.clone(),
        version: version(),
        pid: std::process::id(),
        uptime_ms: core.started.elapsed().as_millis() as u64,
        live,
        finished: total - live,
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

fn compact_list_entry(mut v: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = v.as_object_mut() {
        obj.remove("config_options");
    }
    v
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
            matching.into_iter().filter(|a| p.all || a.is_live()).cloned().map(|a| compact_list_entry(json!(Public(a)))).collect();
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
            agents.extend(r["agents"].as_array().cloned().unwrap_or_default().into_iter().map(compact_list_entry));
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
    match resolve_any(core, &p.agent)? {
        TargetKey::Terminal(id) => {
            terminal_rm(core, TerminalParams { terminal: crate::terminals::address(id), caller: p.caller }).await
        }
        TargetKey::Agent(id) => {
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
    }
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
    let (trashed, worker_id) = {
        let mut st = core.lock();
        let was = st.reg.agents.get(&id).cloned();
        st.reg.label(id, &labels(&p.labels))?;
        core.save(&st);
        let labels = serde_json::to_value(&st.reg.agents[&id].labels).unwrap_or_default();
        core.emit_from(&mut st, "labels", id, json!({"labels": labels}), was.as_ref());
        let a = &st.reg.agents[&id];
        (
            a.labels.get("herdr.trashed").is_some_and(|s| !s.is_empty()),
            a.labels.get("drove.worker").cloned(),
        )
    };
    if trashed {
        let sig = if let Some(wid) = worker_id {
            crate::drove::pool::PoolSignal::WorkerTrashed { worker_id: wid }
        } else {
            crate::drove::pool::PoolSignal::SessionClosed {
                address: crate::agents::address(id),
            }
        };
        let _ = core.drove.pool_tx.send(sig);
    }
    let result = record(core, id).map(Public);
    core.flush_saved().await?;
    result
}

async fn agent_read(core: Ctx<'_>, p: ReadParams) -> Result<ReadResult, RpcError> {
    let range_requested = p.from_turn.is_some() || p.to_turn.is_some();
    match resolve_any(core, &p.agent)? {
        TargetKey::Terminal(id) => {
            let term_addr = crate::terminals::address(id);
            if p.wait {
                let timeout = wait_timeout(core, p.timeout)?;
                let spec = WaitSpec { output: None, timeout: Some(timeout), after_turn: None };
                let _ = watched(core, p.caller.clone(), TargetKey::Terminal(id), spec).await;
            }
            let snap = ((!p.last && p.lines.is_none()) || range_requested).then(|| {
                let st = core.lock();
                st.pty_reg
                    .terminals
                    .get(&id)
                    .map(|t| crate::pty_transcript::snapshot_from_terminal(t, st.pty_prompts.get(&id)))
            }).flatten();
            let (entries, session_meta) = if let Some(snap) = snap {
                blocking(move || Ok(crate::pty_transcript::extract_transcript(&snap))).await?
            } else {
                (Vec::new(), None)
            };
            if range_requested {
                let sliced = crate::transcript::slice_turn_range(&entries, p.from_turn, p.to_turn);
                let text = crate::transcript::render_entries(&sliced);
                return Ok(ReadResult {
                    text,
                    complete: true,
                    how: "transcript".into(),
                    why: None,
                    entries: sliced,
                    session_meta,
                });
            }
            let lines = match p.lines {
                Some(n) => n,
                None if !entries.is_empty() => 0,
                None => {
                    if core.lock().screens.get(&id).is_some_and(|s| s.alt_screen()) { 0 } else { 160 }
                }
            };
            let r = terminal_read(
                core,
                TerminalReadParams { terminal: term_addr, caller: p.caller, lines },
            )
            .await?;
            let how = if !entries.is_empty() { "transcript".into() } else { r.how };
            Ok(ReadResult { text: r.text, complete: r.complete, how, why: r.why, entries, session_meta })
        }
        TargetKey::Agent(id) => {
            let timeout = if p.wait { Some(wait_timeout(core, p.timeout)?) } else { None };
            if let Some(timeout) = timeout {
                let spec = WaitSpec { output: None, timeout: Some(timeout), after_turn: None };
                let _ = watched(core, p.caller.clone(), TargetKey::Agent(id), spec).await;
            } else {
                core.saw(p.caller.as_deref(), id);
            }
            if range_requested {
                let all_entries = core.read_transcript_entries(id)?;
                let sliced = crate::transcript::slice_turn_range(&all_entries, p.from_turn, p.to_turn);
                let text = crate::transcript::render_entries(&sliced);
                core.flush_saved().await?;
                return Ok(ReadResult {
                    text,
                    complete: true,
                    how: "transcript".into(),
                    why: None,
                    entries: sliced,
                    session_meta: None,
                });
            }
            let (text, complete, how) = core.read_transcript(id, p.lines, p.last)?;
            let entries = if !p.last && p.lines.is_none() {
                core.read_transcript_entries(id)?
            } else {
                Vec::new()
            };
            core.flush_saved().await?;
            Ok(ReadResult { text, complete, how: how.into(), why: None, entries, session_meta: None })
        }
    }
}

pub(super) async fn agent_send(core: Ctx<'_>, p: SendParams) -> Result<SendResult, RpcError> {
    match resolve_any(core, &p.agent)? {
        TargetKey::Terminal(id) => {
            let r = terminal_send(
                core,
                TerminalSendParams {
                    terminal: crate::terminals::address(id),
                    text: p.text,
                    force: p.steer,
                    only_if: p.only_if,
                    wait: p.wait,
                    timeout: p.timeout,
                    caller: p.from.or(p.caller),
                },
            )
            .await?;
            Ok(SendResult { status: r.status, turn: r.turn, started: r.started, inbox_len: 0, wait: r.wait })
        }
        TargetKey::Agent(id) => {
            let timeout = wait_timeout(core, p.timeout)?;
            let sender = p.from.as_deref().or(p.caller.as_deref());
            let text =
                if p.text.trim_start().starts_with("<h2") { p.text.clone() } else { core.tagged(sender, id, &p.text, false) };
            let sent = core.send(id, &text, p.steer, p.only_if.map(|w| word(&w)).as_deref(), sender).await?;
            core.clear_manual_status_key(TargetKey::Agent(id));
            core.clear_manual_status_caller(p.from.as_deref());
            core.clear_manual_status_caller(p.caller.as_deref());
            let target_addr = crate::agents::address(id);
            if let Some(caller_key) = sender.and_then(TargetKey::parse) {
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
                        if let Some(c) = st
                            .pty_reg
                            .terminals
                            .get_mut(&child)
                            .filter(|c| c.parent.as_deref() == Some(target_addr.as_str()))
                        {
                            c.told_parent = true;
                        }
                    }
                }
            }
            if sent.status.as_deref() == Some("working") {
                let mut st = core.lock();
                let from = sender
                    .and_then(TargetKey::parse)
                    .and_then(|k| match k {
                        TargetKey::Agent(c) => st.reg.agents.get(&c).map(|a| a.address.clone()),
                        TargetKey::Terminal(c) => st.pty_reg.terminals.get(&c).map(|t| t.address.clone()),
                    })
                    .or_else(|| sender.filter(|c| c.contains('/')).map(str::to_string));
                st.messages.insert(TargetKey::Agent(id), from);
                drop(st);
                core.changed.notify_waiters();
            }
            core.saw(p.caller.as_deref(), id);
            if let Some(caller) = &p.caller {
                core.lock().last_send.insert((caller.clone(), TargetKey::Agent(id)), sent.after_turn);
            }
            let target = TargetKey::Agent(id);
            let wait = if p.wait && sent.started == Some(false) {
                Some(core.wait_result(target, false, "not_started", None))
            } else if p.wait {
                let spec = WaitSpec { output: None, timeout: Some(timeout), after_turn: Some(sent.after_turn) };
                Some(watched(core, p.caller.clone(), target, spec).await)
            } else {
                None
            };
            core.flush_saved().await?;
            Ok(SendResult {
                status: sent.status,
                turn: sent.turn,
                started: sent.started,
                inbox_len: sent.inbox_len,
                wait,
            })
        }
    }
}

async fn agent_inbox_edit(core: Ctx<'_>, p: InboxEditParams) -> Result<Public, RpcError> {
    let id = resolve(core, &p.agent)?;
    let updated = core.edit_inbox(id, p.id, &p.text)?;
    core.flush_saved().await?;
    Ok(Public(updated))
}

async fn agent_inbox_remove(core: Ctx<'_>, p: InboxRemoveParams) -> Result<Public, RpcError> {
    let id = resolve(core, &p.agent)?;
    let updated = core.remove_inbox(id, p.id)?;
    core.flush_saved().await?;
    Ok(Public(updated))
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
    core.clear_manual_status_key(TargetKey::Agent(id));
    core.clear_manual_status_caller(p.caller.as_deref());
    core.saw(p.caller.as_deref(), id);
    core.flush_saved().await?;
    let outcome = if approve { "approved" } else { "denied" }.into();
    Ok(ApproveResult { agent: crate::agents::address(id), request_id, option_id, outcome })
}

async fn agent_config(core: Ctx<'_>, p: ConfigParams) -> Result<Public, RpcError> {
    if p.config_id.trim().is_empty() {
        return Err(RpcError::invalid_params("--id cannot be empty"));
    }
    let val = match (&p.value, p.bool_value) {
        (Some(v), None) => serde_json::Value::String(v.clone()),
        (None, Some(b)) => serde_json::Value::Bool(b),
        (Some(_), Some(_)) => return Err(RpcError::invalid_params("pass either value or bool_value, not both")),
        (None, None) => return Err(RpcError::invalid_params("pass --value <VALUE> or --bool <true|false>")),
    };
    let id = resolve(core, &p.agent)?;

    let need_auto_approve = if p.config_id == "permissions" {
        apply_permissions_config(core, id, &val).await?
    } else {
        apply_acp_config(core, id, &p.config_id, val).await?
    };

    if need_auto_approve {
        let _ = core.resolve_permission(id, true, None);
    }
    let updated = {
        let mut st = core.lock();
        core.clear_manual_status_agent_locked(&mut st, id);
        let (data, snapshot) = {
            let a = st.reg.agents.get(&id).ok_or_else(|| {
                RpcError::new("not_found", format!("{} was removed", crate::agents::address(id)), "see `h2 ls --all`")
            })?;
            (crate::acp::config_event_data(a), Public(a.clone()))
        };
        core.emit(&mut st, "config", id, data);
        core.save(&st);
        core.changed.notify_waiters();
        snapshot
    };
    core.flush_saved().await?;
    Ok(updated)
}

async fn apply_permissions_config(core: Ctx<'_>, id: u32, val: &serde_json::Value) -> Result<bool, RpcError> {
    let raw_str = val.as_str().map(str::trim);
    let next_perm = match val {
        serde_json::Value::String(s) => s.parse::<crate::permissions::PermissionMode>().map_err(RpcError::invalid_params)?,
        serde_json::Value::Bool(true) => crate::permissions::PermissionMode::ApproveAll,
        serde_json::Value::Bool(false) => crate::permissions::PermissionMode::ApproveReads,
        _ => unreachable!(),
    };
    let wants_bypass = next_perm == crate::permissions::PermissionMode::ApproveAll
        && (val.as_bool() == Some(true) || raw_str.is_some_and(crate::permissions::is_bypass_mode_id));
    let live_bypass = {
        let st = core.lock();
        let a = st.reg.agents.get(&id).ok_or_else(|| {
            RpcError::new("not_found", format!("{} was removed", crate::agents::address(id)), "see `h2 ls --all`")
        })?;
        if !a.is_live() {
            return Err(RpcError::new("finished", format!("{} has finished", a.address), ""));
        }
        a.check_program()?;
        if wants_bypass
            && let Some((wire_mode, src)) = crate::acp::bypass_mode_for_options(&a.config_options)
            && let Some(sid) = a.acp_session_id.clone()
            && let Some(tr) = st.sessions.get(&id).map(|h| h.transport.clone())
        {
            Some((sid, wire_mode, src, tr))
        } else {
            None
        }
    };
    let fresh_mode = if let Some((sid, wire_mode, src, tr)) = live_bypass {
        let v = serde_json::Value::String(wire_mode);
        let opts = crate::acp::apply_config_option(&tr, &sid, "mode", &v, src.as_deref()).await?;
        Some((v, opts))
    } else {
        None
    };

    let mut st = core.lock();
    let a = st.reg.agents.get_mut(&id).ok_or_else(|| {
        RpcError::new("not_found", format!("{} was removed", crate::agents::address(id)), "see `h2 ls --all`")
    })?;
    if !a.is_live() {
        return Err(RpcError::new("finished", format!("{} has finished", a.address), ""));
    }
    a.permission_mode = next_perm;
    if let Some((v, opts)) = fresh_mode {
        a.config_overrides.insert("mode".into(), v.clone());
        if let Some(fresh_opts) = opts {
            a.config_options = fresh_opts;
        } else {
            crate::acp::patch_config_value(&mut a.config_options, "mode", &v);
        }
        crate::acp::sync_config_to_agent(a);
    }
    Ok(next_perm == crate::permissions::PermissionMode::ApproveAll && st.pending_perms.contains_key(&id))
}

async fn apply_acp_config(
    core: Ctx<'_>,
    id: u32,
    config_id: &str,
    val: serde_json::Value,
) -> Result<bool, RpcError> {
    let default_permissions = crate::config::load(&core.paths.config)
        .ok()
        .and_then(|c| c.default_permissions)
        .unwrap_or_default();
    let (session_id, effective_id, source, transport, is_claude, mode_choices, opts_snapshot) = {
        let st = core.lock();
        let a = st.reg.agents.get(&id).ok_or_else(|| {
            RpcError::new("not_found", format!("{} was removed", crate::agents::address(id)), "see `h2 ls --all`")
        })?;
        if !a.is_live() {
            return Err(RpcError::new("finished", format!("{} has finished", a.address), ""));
        }
        a.check_program()?;
        let session_id = a.acp_session_id.clone().ok_or_else(|| {
            RpcError::new("unavailable", format!("{} ACP session is still starting", a.address), "retry shortly")
        })?;
        let effective_id = crate::acp::resolve_config_option_id(config_id, &a.config_options);
        let source = a.config_options.iter().find(|o| o.id == effective_id).map(|o| o.source.clone());
        let is_claude = crate::acp::presets::is_claude_agent(a.agent.as_deref(), &a.argv);
        let mode_choices = a
            .config_options
            .iter()
            .find(|o| o.id == "mode" || o.category.as_deref() == Some("mode"))
            .map(|o| o.options.clone())
            .unwrap_or_default();
        let transport = st
            .sessions
            .get(&id)
            .map(|h| h.transport.clone())
            .ok_or_else(|| RpcError::new("unavailable", format!("{} has no active ACP transport", a.address), ""))?;
        (session_id, effective_id, source, transport, is_claude, mode_choices, a.config_options.clone())
    };

    let (val, split_effort) = if effective_id == "mode"
        && let Some(raw) = val.as_str()
    {
        (serde_json::Value::String(crate::acp::normalize_mode_value(raw, is_claude, &mode_choices)), None)
    } else if effective_id == "model"
        && let Some(raw) = val.as_str()
    {
        let (wire_model, effort) = crate::acp::split_model_effort_for_options(raw, &opts_snapshot);
        (serde_json::Value::String(wire_model), effort)
    } else {
        (val, None)
    };
    let wants_bypass = effective_id == "mode" && val.as_str().is_some_and(crate::permissions::is_bypass_mode_id);

    let mut fresh = match crate::acp::apply_config_option(&transport, &session_id, &effective_id, &val, source.as_deref()).await {
        Ok(opts) => opts,
        Err(_) if wants_bypass => None,
        Err(e) => return Err(e),
    };

    let mut applied_effort: Option<(String, serde_json::Value)> = None;
    if let Some(eff) = split_effort {
        let base_opts = fresh.as_deref().unwrap_or(&opts_snapshot);
        let eff_id = crate::acp::resolve_config_option_id("reasoning_effort", base_opts);
        let eff_src = base_opts.iter().find(|o| o.id == eff_id).map(|o| o.source.clone());
        let eff_val = serde_json::json!(eff);
        if let Ok(eff_fresh) =
            crate::acp::apply_config_option(&transport, &session_id, &eff_id, &eff_val, eff_src.as_deref()).await
        {
            if let Some(opts) = eff_fresh {
                fresh = Some(opts);
            }
            applied_effort = Some((eff_id, eff_val));
        }
    }

    let mut st = core.lock();
    let a = st.reg.agents.get_mut(&id).ok_or_else(|| {
        RpcError::new("not_found", format!("{} was removed", crate::agents::address(id)), "see `h2 ls --all`")
    })?;
    if !a.is_live() {
        return Err(RpcError::new("finished", format!("{} has finished", a.address), ""));
    }
    a.config_overrides.insert(effective_id.clone(), val.clone());
    if let Some((ref eff_id, ref eff_val)) = applied_effort {
        a.config_overrides.insert(eff_id.clone(), eff_val.clone());
    }
    if let Some(opts) = fresh {
        a.config_options = opts;
    } else {
        crate::acp::patch_config_value(&mut a.config_options, &effective_id, &val);
        if let Some((ref eff_id, ref eff_val)) = applied_effort {
            crate::acp::patch_config_value(&mut a.config_options, eff_id, eff_val);
        }
    }
    crate::acp::sync_config_to_agent(a);
    if effective_id == "mode"
        && let Some(m) = val.as_str()
    {
        if crate::permissions::is_bypass_mode_id(m) {
            a.permission_mode = crate::permissions::PermissionMode::ApproveAll;
        } else if is_claude && a.permission_mode == crate::permissions::PermissionMode::ApproveAll {
            a.permission_mode = default_permissions;
        }
    }
    Ok(a.permission_mode == crate::permissions::PermissionMode::ApproveAll && st.pending_perms.contains_key(&id))
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
    match resolve_any(core, &p.agent)? {
        TargetKey::Terminal(id) => {
            terminal_put(
                core,
                TerminalPutParams { terminal: crate::terminals::address(id), name: p.name, data: p.data },
            )
            .await
        }
        TargetKey::Agent(id) => {
            let too_big = || RpcError::invalid_params(format!("at most {} MB per put", crate::put::MAX_BYTES >> 20));
            if p.data.len() / 4 * 3 > crate::put::MAX_BYTES + 3 {
                return Err(too_big());
            }
            let bytes = crate::put::decode(&p.data).ok_or_else(|| RpcError::invalid_params("data is not base64"))?;
            if bytes.len() > crate::put::MAX_BYTES {
                return Err(too_big());
            }
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
    }
}

async fn agent_interrupt(core: Ctx<'_>, p: InterruptParams) -> Result<InterruptResult, RpcError> {
    match resolve_any(core, &p.agent)? {
        TargetKey::Terminal(id) => {
            let r = terminal_interrupt(
                core,
                TerminalInterruptParams { terminal: crate::terminals::address(id), timeout: p.timeout },
            )
            .await?;
            Ok(InterruptResult { outcome: r.outcome, method: r.key })
        }
        TargetKey::Agent(id) => {
            use crate::input::Interrupted;
            let timeout = seconds(p.timeout)?.unwrap_or(Duration::from_secs(10));
            let (outcome, method) = core.interrupt(id, timeout).await?;
            core.clear_manual_status_key(TargetKey::Agent(id));
            let outcome = match outcome {
                Interrupted::Stopped => "stopped",
                Interrupted::Exited => "exited",
                Interrupted::StillWorking => "still_working",
                Interrupted::Unknown => "unknown",
            };
            core.flush_saved().await?;
            Ok(InterruptResult { outcome: outcome.into(), method })
        }
    }
}

async fn events_subscribe(core: Ctx<'_>, p: SubscribeParams) -> Result<SubscribeResult, RpcError> {
    use crate::events::{Filter, Sink};
    let address_agent = |a: &str| match a.split_once('/').filter(|(m, _)| core.lock().machines.iter().any(|x| x.name == *m)) {
        Some(_) => Ok(a.to_string()),
        None => resolve(core, a).map(crate::agents::address),
    };
    let address_term = |t: &str| match t.split_once('/').filter(|(m, _)| core.lock().machines.iter().any(|x| x.name == *m)) {
        Some(_) => Ok(t.to_string()),
        None => resolve_pty(core, t).map(crate::terminals::address),
    };
    let address_any = |x: &str| match x.split_once('/').filter(|(m, _)| core.lock().machines.iter().any(|x| x.name == *m)) {
        Some(_) => Ok(x.to_string()),
        None => resolve_any(core, x).map(TargetKey::address),
    };
    let agent = p.agent.as_deref().map(address_agent).transpose()?;
    let terminal = p.terminal.as_deref().map(address_term).transpose()?;
    let children_of = p.children_of.as_deref().map(address_any).transpose()?;
    let filter = Filter {
        agent,
        terminal,
        children_of,
        labels: labels(&p.labels),
        docs: p.docs,
        here_only: from_afar(core),
    };
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
    let (seq, agents, terminals) = {
        let mut st = core.lock();
        let agents = core.matching(&st, &filter);
        let terminals = core.matching_terminals(&st, &filter);
        core.subscribe(&mut st, filter, sink);
        (st.events.seq, agents, terminals)
    };
    if let Some(channel) = channel {
        crate::server::heartbeat(core.core.clone(), core.conn.expect("framed").clone(), channel);
    }
    Ok(SubscribeResult { seq, agents, terminals, channel })
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

async fn drove_settings_get(
    core: Ctx<'_>,
    _: DroveOnParams,
) -> Result<crate::drove::types::DroveSettingsResult, RpcError> {
    Ok(core.drove.rpc_settings_get().await)
}

async fn drove_settings_set(
    core: Ctx<'_>,
    p: crate::drove::types::DroveSettingsSetParams,
) -> Result<crate::drove::types::DroveSettingsResult, RpcError> {
    core.drove.rpc_settings_set(p).await
}

async fn drove_probe(core: Ctx<'_>, p: DroveProbeParams) -> Result<crate::drove::types::ProbeSnapshot, RpcError> {
    core.drove.rpc_probe(p.refresh).await
}

async fn drove_ai_test(core: Ctx<'_>, p: DroveAiTestParams) -> Result<crate::drove::probe::AiTestResult, RpcError> {
    core.drove.rpc_ai_test(p.base_url, p.api_key, p.model).await
}

async fn drove_roles_ai_configure(
    core: Ctx<'_>,
    p: crate::drove::types::DroveRolesAiConfigureParams,
) -> Result<crate::drove::types::DroveRolesAiConfigureResult, RpcError> {
    core.drove.rpc_roles_ai_configure(p).await
}

async fn drove_magpie_test(
    core: Ctx<'_>,
    p: DroveMagpieTestParams,
) -> Result<crate::drove::magpie::MagpieTestResult, RpcError> {
    core.drove.rpc_magpie_test(p.gateway_url).await
}

async fn drove_web_open(core: Ctx<'_>, p: DroveWebOpenParams) -> Result<crate::drove::types::DroveWebOpenResult, RpcError> {
    core.drove.open_web(p.run_id.as_deref(), p.no_open)
}

async fn drove_http_proxy(
    core: Ctx<'_>,
    p: crate::drove::http::HttpProxyRequest,
) -> Result<crate::drove::http::HttpProxyResponse, RpcError> {
    core.drove.rpc_http_proxy(p).await
}

async fn drove_subscribe(core: Ctx<'_>, _: Empty) -> Result<DroveSubscribeResult, RpcError> {
    let Some(conn) = core.conn else {
        return Err(RpcError::new("needs_framed", "drove.subscribe needs a framed connection", "send hello with framed: true"));
    };
    let val = core.drove.rpc_subscribe(conn)?;
    let ch = val["channel"].as_u64().unwrap_or(0) as u32;
    Ok(DroveSubscribeResult { channel: ch })
}

async fn checkpoint_list(core: Ctx<'_>, p: CheckpointListParams) -> Result<CheckpointListResult, RpcError> {
    let tk = resolve_any(core, &p.target)?;
    if let TargetKey::Terminal(id) = tk {
        let try_reattach = {
            let mut st = core.lock();
            if let Some(t) = st.pty_reg.terminals.get(&id).cloned()
                && t.is_live()
            {
                let tc = core.target_checkpoints(&mut st, &t.address);
                let fg_pid = t.foreground.as_ref().and_then(|f| u32::try_from(f.pid).ok());
                let resumed = crate::pty_adapters::terminal_active_resume_argv(&t).is_some();
                resumed && crate::pty_adapters::needs_resumed_reattach(&tc, fg_pid)
            } else {
                false
            }
        };
        if try_reattach {
            let _ = crate::pty_adapters::reattach_resumed_cli_checkpoints(core, id).await;
        }
    }
    let (addr, cwd, tc, live_snapshot) = {
        let mut st = core.lock();
        let (addr, cwd) = match tk {
            TargetKey::Agent(id) => {
                let a = st.reg.agents.get(&id).ok_or_else(|| RpcError::new("not_found", "agent not found", ""))?;
                (a.address.clone(), a.cwd.clone())
            }
            TargetKey::Terminal(id) => {
                let t = st.pty_reg.terminals.get(&id).ok_or_else(|| RpcError::new("not_found", "terminal not found", ""))?;
                let cwd = t
                    .foreground
                    .as_ref()
                    .map(|f| f.cwd.as_str())
                    .filter(|s| !s.is_empty())
                    .or(t.last_cwd.as_deref())
                    .unwrap_or(&t.cwd)
                    .to_string();
                (t.address.clone(), cwd)
            }
        };
        let tc = core.target_checkpoints(&mut st, &addr);
        let live_snapshot = crate::checkpoint_safety::snapshot_live_targets(&st);
        (addr, cwd, tc, live_snapshot)
    };
    let repo = crate::checkpoint::probe_repo(std::path::Path::new(&cwd));
    let isolation = repo
        .as_ref()
        .map(|r| crate::checkpoint_safety::evaluate_isolation(&addr, r, &live_snapshot));
    Ok(CheckpointListResult {
        target: addr,
        repo_root: tc.repo_root.or_else(|| repo.map(|r| r.toplevel.to_string_lossy().to_string())),
        cli_kind: tc.cli_kind,
        isolation,
        checkpoints: tc.checkpoints,
    })
}

async fn checkpoint_capture(core: Ctx<'_>, p: CheckpointCaptureParams) -> Result<CheckpointCaptureResult, RpcError> {
    let Some(target_str) = p.target else {
        return Err(RpcError::new(
            "needs_target",
            "missing target address",
            "pass target (<a*|t*>) or run inside an h2 terminal/agent",
        ));
    };
    let tk = resolve_any(core, &target_str)?;
    let record_entry = matches!(tk, TargetKey::Agent(_));
    let meta = crate::checkpoint::capture_target_checkpoint(core, tk, None, None, p.label, false, record_entry)
        .await
        .map_err(RpcError::internal)?
        .ok_or_else(|| RpcError::invalid_params("target directory is not inside a Git repository"))?;
    let addr = match tk {
        TargetKey::Agent(id) => crate::agents::address(id),
        TargetKey::Terminal(id) => crate::terminals::address(id),
    };
    Ok(CheckpointCaptureResult { target: addr, checkpoint: meta })
}

async fn checkpoint_diff(core: Ctx<'_>, p: CheckpointDiffParams) -> Result<crate::checkpoint::CheckpointDiff, RpcError> {
    let tk = resolve_any(core, &p.target)?;
    let (addr, cwd, tc) = {
        let mut st = core.lock();
        let (addr, cwd) = match tk {
            TargetKey::Agent(id) => {
                let a = st.reg.agents.get(&id).ok_or_else(|| RpcError::new("not_found", "agent not found", ""))?;
                (a.address.clone(), a.cwd.clone())
            }
            TargetKey::Terminal(id) => {
                let t = st.pty_reg.terminals.get(&id).ok_or_else(|| RpcError::new("not_found", "terminal not found", ""))?;
                let cwd = t
                    .foreground
                    .as_ref()
                    .map(|f| f.cwd.as_str())
                    .filter(|s| !s.is_empty())
                    .or(t.last_cwd.as_deref())
                    .unwrap_or(&t.cwd)
                    .to_string();
                (t.address.clone(), cwd)
            }
        };
        let tc = core.target_checkpoints(&mut st, &addr);
        (addr, cwd, tc)
    };
    if tc.checkpoints.is_empty() {
        return Err(RpcError::invalid_params(format!("no checkpoints recorded for {addr}")));
    }
    let to_turn = p
        .to
        .or_else(|| tc.checkpoints.iter().map(|c| c.turn).max())
        .ok_or_else(|| RpcError::invalid_params(format!("no target checkpoint for {addr}")))?;
    let from_turn = p
        .from
        .or_else(|| tc.checkpoints.iter().filter(|c| c.turn < to_turn).map(|c| c.turn).max())
        .unwrap_or(0);
    let from_meta = tc
        .checkpoints
        .iter()
        .find(|c| c.turn == from_turn)
        .ok_or_else(|| RpcError::invalid_params(format!("no checkpoint at turn {from_turn} for {addr}")))?
        .clone();
    let to_meta = tc
        .checkpoints
        .iter()
        .find(|c| c.turn == to_turn)
        .ok_or_else(|| RpcError::invalid_params(format!("no checkpoint at turn {to_turn} for {addr}")))?
        .clone();
    let repo_dir = tc.repo_root.as_deref().unwrap_or(&cwd);
    let repo = crate::checkpoint::probe_repo(std::path::Path::new(repo_dir))
        .ok_or_else(|| RpcError::invalid_params(format!("workspace `{repo_dir}` is not a Git repository")))?;
    let _guard = core.repo_ckpt_lock(&repo.toplevel).lock_owned().await;
    tokio::task::spawn_blocking(move || {
        crate::checkpoint::diff_checkpoints(&repo, from_turn, to_turn, &from_meta.ref_name, &to_meta.ref_name)
    })
    .await
    .map_err(|e| RpcError::internal(format!("diff task panicked: {e}")))?
    .map_err(RpcError::internal)
}

async fn checkpoint_rollback(
    core: Ctx<'_>,
    p: CheckpointRollbackParams,
) -> Result<crate::checkpoint::CheckpointRollbackResult, RpcError> {
    match resolve_any(core, &p.target)? {
        TargetKey::Agent(id) => crate::acp::rollback_acp_agent(core, id, p.to, p.restore_files, p.confirm).await,
        TargetKey::Terminal(id) => {
            crate::pty_adapters::rollback_pty_terminal(core, id, p.to, p.restore_files, p.confirm).await
        }
    }
}

async fn checkpoint_hook(core: Ctx<'_>, p: CheckpointHookParams) -> Result<CheckpointHookResult, RpcError> {
    let tk = resolve_any(core, &p.terminal)?;
    let TargetKey::Terminal(id) = tk else {
        return Err(RpcError::invalid_params(format!("{} is not a terminal", p.terminal)));
    };
    let val = crate::pty_adapters::handle_checkpoint_hook(
        core,
        id,
        &p.source,
        p.phase.as_deref(),
        p.event.as_deref(),
        &p.payload,
    )
    .await;
    let inject_steps = val.get("injectSteps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    Ok(CheckpointHookResult { inject_steps })
}
