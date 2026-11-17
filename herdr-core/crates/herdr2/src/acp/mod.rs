//! ACP session lifecycle: handshake, serial prompt inbox queue, updates, and permissions.

pub mod presets;
pub mod proto;
pub mod transport;

use crate::permissions::{self, Decision, PublicPermission, PublicPermissionOption};
use crate::platform;
use crate::rpc::RpcError;
use crate::runtime::{Core, Exit};
use crate::status::StatusReport;
use crate::transcript::Entry;
use proto::{
    AcpConfigChoice, AcpConfigOption, AcpContextUsage, ContentBlock, EnvVar, InitializeParams, InitializeResult,
    ParsedUpdate, SessionCancelParams, SessionLoadParams, SessionNewParams, SessionNewResult, SessionPromptParams,
    SessionPromptResult, SessionSetConfigOptionParams, SessionSetModeParams, SessionSetModelParams, StdioMcpServer,
    parse_request_permission, parse_session_config, parse_session_update, permission_cancelled, permission_selected,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use transport::{CallError, ChildWaiter, Inbound, Transport};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const TURN_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Debug)]
pub struct QueuedPrompt {
    pub text: String,
    pub from: Option<String>,
}

#[derive(Debug)]
pub enum SessionCommand {
    WakeInbox,
    Steer(QueuedPrompt),
    Interrupt,
    Signal(i32),
}

pub struct PendingPermission {
    pub request_id: String,
    pub rpc_id: Value,
    pub title: String,
    pub options: Vec<PublicPermissionOption>,
    pub transport: Transport,
    pub turn: u64,
}

pub struct SessionHandle {
    pub cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    pub transport: Transport,
}

pub struct SpawnSpec {
    pub address: String,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: String,
    pub resume_session_id: Option<String>,
    pub can_load_session: bool,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub dangerously_skip_permissions: bool,
    pub config_overrides: BTreeMap<String, Value>,
    pub inject_mcp: bool,
    pub mcp_exe: String,
    pub socket: String,
}

pub struct StartedSession {
    pub transport: Transport,
    pub waiter: ChildWaiter,
    pub inbound_rx: mpsc::UnboundedReceiver<Inbound>,
    pub session_id: String,
    pub agent_info: Option<String>,
    pub can_load_session: bool,
    pub config_options: Vec<AcpConfigOption>,
    pub context_usage: Option<AcpContextUsage>,
}

/// Spawns the ACP child process and runs `initialize` + `session/new` (or `session/load`).
pub async fn spawn_session(spec: SpawnSpec) -> Result<StartedSession, RpcError> {
    let (transport, waiter, mut inbound_rx) = Transport::spawn(&spec.argv, &spec.env, &spec.cwd)?;

    let init_res = match setup_session(&transport, &spec, &mut inbound_rx).await {
        Ok(res) => res,
        Err(err) => {
            transport.kill();
            let _ = tokio::time::timeout(Duration::from_secs(2), waiter.wait()).await;
            return Err(err);
        }
    };

    Ok(StartedSession {
        transport,
        waiter,
        inbound_rx,
        session_id: init_res.session_id,
        agent_info: init_res.agent_info,
        can_load_session: init_res.can_load_session,
        config_options: init_res.config_options,
        context_usage: init_res.context_usage,
    })
}

struct SetupOk {
    session_id: String,
    agent_info: Option<String>,
    can_load_session: bool,
    config_options: Vec<AcpConfigOption>,
    context_usage: Option<AcpContextUsage>,
}

/// Normalizes bypass aliases (such as `dangerously-skip-permissions`) to the ACP adapter's wire mode id.
pub fn normalize_mode_value(raw: &str, is_claude: bool, choices: &[AcpConfigChoice]) -> String {
    let normalized = raw.trim().trim_start_matches('-').to_ascii_lowercase();
    if matches!(
        normalized.as_str(),
        "dangerously-skip-permissions"
            | "dangerously_skip_permissions"
            | "bypass-permissions"
            | "bypass_permissions"
            | "bypasspermissions"
    ) {
        if choices.iter().any(|c| c.value == "bypassPermissions") || (is_claude && choices.is_empty()) {
            return "bypassPermissions".into();
        }
        if choices.iter().any(|c| c.value == "agent-full-access") {
            return "agent-full-access".into();
        }
    }
    raw.to_string()
}

/// Returns the bypass mode choice offered by `config_options` (when present).
pub fn bypass_mode_for_options(options: &[AcpConfigOption]) -> Option<(String, Option<String>)> {
    let opt = options.iter().find(|o| o.id == "mode" || o.category.as_deref() == Some("mode"))?;
    let wire = ["bypassPermissions", "agent-full-access"]
        .into_iter()
        .find(|candidate| opt.options.iter().any(|c| c.value == *candidate))?;
    Some((wire.to_string(), Some(opt.source.clone())))
}

/// Updates one config option in place (or inserts a minimal entry if missing).
pub fn patch_config_value(options: &mut Vec<AcpConfigOption>, config_id: &str, value: &Value) {
    if let Some(opt) = options.iter_mut().find(|o| o.id == config_id) {
        opt.current_value = value.clone();
        return;
    }
    options.push(AcpConfigOption {
        id: config_id.to_string(),
        name: config_id.to_string(),
        description: None,
        category: matches!(config_id, "model" | "mode").then(|| config_id.to_string()),
        kind: if value.is_boolean() { "boolean".into() } else { "select".into() },
        current_value: value.clone(),
        options: Vec::new(),
        source: "config".into(),
    });
}

/// Keeps top-level `Agent::{model, mode, permission_mode}` aligned with `config_options` when available.
pub fn sync_config_to_agent(a: &mut crate::agents::Agent) {
    if let Some(opt) = a.config_options.iter().find(|o| o.id == "model" || o.category.as_deref() == Some("model"))
        && let Some(v) = opt.current_value.as_str()
    {
        a.model = Some(v.to_string());
    }
    if let Some(opt) = a.config_options.iter().find(|o| o.id == "mode" || o.category.as_deref() == Some("mode"))
        && let Some(v) = opt.current_value.as_str()
    {
        if crate::permissions::is_bypass_mode_id(v) {
            a.permission_mode = crate::permissions::PermissionMode::ApproveAll;
        }
        a.mode = Some(v.to_string());
    }
}

pub(crate) fn config_event_data(a: &crate::agents::Agent) -> Value {
    json!({
        "model": a.model,
        "mode": a.mode,
        "permission_mode": a.permission_mode,
        "config_options": a.config_options,
    })
}

/// Applies one configuration change (`session/set_config_option` with legacy fallback) on an active session.
pub async fn apply_config_option(
    transport: &Transport,
    session_id: &str,
    config_id: &str,
    value: &Value,
    source: Option<&str>,
) -> Result<Option<Vec<AcpConfigOption>>, RpcError> {
    if source == Some("legacy_mode") {
        let mode_id = value.as_str().ok_or_else(|| RpcError::invalid_params("mode requires a string value"))?;
        transport
            .request(
                "session/set_mode",
                &SessionSetModeParams { session_id: session_id.into(), mode_id: mode_id.into() },
                HANDSHAKE_TIMEOUT,
            )
            .await
            .map_err(|e| e.into_rpc_error("ACP session/set_mode failed", &transport.recent_stderr()))?;
        return Ok(None);
    }
    if source == Some("legacy_model") {
        let model_id = value.as_str().ok_or_else(|| RpcError::invalid_params("model requires a string value"))?;
        transport
            .request(
                "session/set_model",
                &SessionSetModelParams { session_id: session_id.into(), model_id: model_id.into() },
                HANDSHAKE_TIMEOUT,
            )
            .await
            .map_err(|e| e.into_rpc_error("ACP session/set_model failed", &transport.recent_stderr()))?;
        return Ok(None);
    }

    let opt_type = if value.is_boolean() { "boolean" } else { "id" }.to_string();
    for with_type in [Some(opt_type), None] {
        let params = SessionSetConfigOptionParams {
            session_id: session_id.into(),
            config_id: config_id.into(),
            option_type: with_type.clone(),
            value: value.clone(),
        };
        match transport.request("session/set_config_option", &params, HANDSHAKE_TIMEOUT).await {
            Ok(resp) => {
                let parsed = parse_session_config(&resp);
                return Ok((!parsed.is_empty() || resp.get("configOptions").is_some()).then_some(parsed));
            }
            Err(e) if with_type.is_some() && e.is_invalid_params() => continue,
            Err(e) if e.is_method_not_found() && config_id == "model" => {
                let model_id = value.as_str().ok_or_else(|| RpcError::invalid_params("model requires a string value"))?;
                let legacy = SessionSetModelParams { session_id: session_id.into(), model_id: model_id.into() };
                transport
                    .request("session/set_model", &legacy, HANDSHAKE_TIMEOUT)
                    .await
                    .map_err(|err| err.into_rpc_error("ACP session/set_model failed", &transport.recent_stderr()))?;
                return Ok(None);
            }
            Err(e) if e.is_method_not_found() && config_id == "mode" => {
                let mode_id = value.as_str().ok_or_else(|| RpcError::invalid_params("mode requires a string value"))?;
                let legacy = SessionSetModeParams { session_id: session_id.into(), mode_id: mode_id.into() };
                transport
                    .request("session/set_mode", &legacy, HANDSHAKE_TIMEOUT)
                    .await
                    .map_err(|err| err.into_rpc_error("ACP session/set_mode failed", &transport.recent_stderr()))?;
                return Ok(None);
            }
            Err(e) => return Err(e.into_rpc_error("ACP session/set_config_option failed", &transport.recent_stderr())),
        }
    }
    unreachable!()
}

async fn setup_session(
    transport: &Transport,
    spec: &SpawnSpec,
    inbound_rx: &mut mpsc::UnboundedReceiver<Inbound>,
) -> Result<SetupOk, RpcError> {
    let raw_init = transport
        .request("initialize", &InitializeParams::herdr2(), HANDSHAKE_TIMEOUT)
        .await
        .map_err(|e| e.into_rpc_error("ACP initialize failed", &transport.recent_stderr()))?;
    let init: InitializeResult = serde_json::from_value(raw_init).unwrap_or_default();
    let agent_info = init.agent_info.as_ref().and_then(|a| a.display_label());
    let can_load_session = init.agent_capabilities.as_ref().is_some_and(|c| c.load_session);

    let server = if spec.inject_mcp {
        let mut env = vec![
            EnvVar { name: "H2_AGENT_ID".into(), value: spec.address.clone() },
            EnvVar { name: "H2_AGENT_SOCKET".into(), value: spec.socket.clone() },
        ];
        if let Ok(sess) = std::env::var("H2_SESSION")
            && !sess.is_empty()
        {
            env.push(EnvVar { name: "H2_SESSION".into(), value: sess });
        }
        Some(StdioMcpServer {
            name: "herdr2".into(),
            command: spec.mcp_exe.clone(),
            args: vec!["mcp".into()],
            env,
        })
    } else {
        None
    };

    let (session_id, mut config_options, context_usage) = match (spec.resume_session_id.as_deref(), can_load_session) {
        (Some(prev_id), true) => {
            match load_session(transport, prev_id, &spec.cwd, server.as_ref()).await {
                Ok(mut opts) => {
                    let mut usage = None;
                    // Drain historical session/update notifications replayed by session/load
                    // before returning so run_actor does not append old turns onto the next turn.
                    while let Ok(msg) = inbound_rx.try_recv() {
                        if let Inbound::Update(params) = msg
                            && let Some((_, parsed)) = parse_session_update(&params)
                        {
                            match parsed {
                                ParsedUpdate::ConfigOptions(fresh) => opts = fresh,
                                ParsedUpdate::Usage(u) => usage = Some(u),
                                _ => {}
                            }
                        }
                    }
                    (prev_id.to_string(), opts, usage)
                }
                Err(_) => {
                    let (sid, opts) = new_session(transport, spec, server.as_ref()).await?;
                    (sid, opts, None)
                }
            }
        }
        _ => {
            let (sid, opts) = new_session(transport, spec, server.as_ref()).await?;
            (sid, opts, None)
        }
    };

    if let Some(mode_id) = &spec.mode
        && !spec.config_overrides.contains_key("mode")
    {
        let is_claude = presets::is_claude_agent(None, &spec.argv);
        let mode_opt = config_options.iter().find(|o| o.id == "mode" || o.category.as_deref() == Some("mode"));
        let src = mode_opt.map(|o| o.source.clone());
        let choices = mode_opt.map(|o| o.options.as_slice()).unwrap_or(&[]);
        let normalized = normalize_mode_value(mode_id, is_claude, choices);
        let val = json!(normalized);
        if let Some(fresh) = apply_config_option(transport, &session_id, "mode", &val, src.as_deref()).await? {
            config_options = fresh;
        } else {
            patch_config_value(&mut config_options, "mode", &val);
        }
    } else if spec.dangerously_skip_permissions
        && !spec.config_overrides.contains_key("mode")
        && let Some((wire_mode, src)) = bypass_mode_for_options(&config_options)
    {
        let val = json!(wire_mode);
        if let Ok(maybe_fresh) = apply_config_option(transport, &session_id, "mode", &val, src.as_deref()).await {
            if let Some(fresh) = maybe_fresh {
                config_options = fresh;
            } else {
                patch_config_value(&mut config_options, "mode", &val);
            }
        }
    }

    if let Some(model) = &spec.model
        && !spec.config_overrides.contains_key("model")
    {
        let src = config_options.iter().find(|o| o.id == "model").map(|o| o.source.clone());
        let val = json!(model);
        if let Some(fresh) = apply_config_option(transport, &session_id, "model", &val, src.as_deref()).await? {
            config_options = fresh;
        } else {
            patch_config_value(&mut config_options, "model", &val);
        }
    }

    for (cfg_id, val) in &spec.config_overrides {
        let src = config_options.iter().find(|o| o.id == *cfg_id).map(|o| o.source.clone());
        if let Ok(maybe_fresh) = apply_config_option(transport, &session_id, cfg_id, val, src.as_deref()).await {
            if let Some(fresh) = maybe_fresh {
                config_options = fresh;
            } else {
                patch_config_value(&mut config_options, cfg_id, val);
            }
        }
    }

    Ok(SetupOk { session_id, agent_info, can_load_session, config_options, context_usage })
}

async fn new_session(
    transport: &Transport,
    spec: &SpawnSpec,
    mcp: Option<&StdioMcpServer>,
) -> Result<(String, Vec<AcpConfigOption>), RpcError> {
    let meta = spec
        .system_prompt
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| json!({ "systemPrompt": { "append": s } }));

    for tagged in [true, false] {
        let mcp_servers = mcp.map(|m| m.to_value(tagged)).into_iter().collect();
        let params = SessionNewParams { cwd: spec.cwd.clone(), mcp_servers, meta: meta.clone() };
        match transport.request("session/new", &params, HANDSHAKE_TIMEOUT).await {
            Ok(val) => {
                let config_options = parse_session_config(&val);
                let parsed: SessionNewResult = serde_json::from_value(val)
                    .map_err(|e| RpcError::new("spawn_failed", format!("invalid session/new result: {e}"), ""))?;
                return Ok((parsed.session_id, config_options));
            }
            Err(e) if tagged && e.is_invalid_params() && mcp.is_some() => continue,
            Err(e) => return Err(e.into_rpc_error("ACP session/new failed", &transport.recent_stderr())),
        }
    }
    unreachable!()
}

async fn load_session(
    transport: &Transport,
    session_id: &str,
    cwd: &str,
    mcp: Option<&StdioMcpServer>,
) -> Result<Vec<AcpConfigOption>, CallError> {
    for tagged in [true, false] {
        let mcp_servers = mcp.map(|m| m.to_value(tagged)).into_iter().collect();
        let params = SessionLoadParams { session_id: session_id.into(), cwd: cwd.into(), mcp_servers };
        match transport.request("session/load", &params, HANDSHAKE_TIMEOUT).await {
            Ok(val) => return Ok(parse_session_config(&val)),
            Err(e) if tagged && e.is_invalid_params() && mcp.is_some() => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Attaches the background actor driving serial ACP turns, updates, and permissions.
pub fn spawn_actor(core: Arc<Core>, id: u32, started: StartedSession) -> SessionHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let handle = SessionHandle { cmd_tx: cmd_tx.clone(), transport: started.transport.clone() };
    tokio::spawn(run_actor(core, id, started, cmd_rx));
    handle
}

async fn run_actor(
    core: Arc<Core>,
    id: u32,
    mut started: StartedSession,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
) {
    let transport = started.transport.clone();
    let session_id = started.session_id.clone();
    let waiter = started.waiter;
    let mut exit_fut = Box::pin(async move { Some(waiter.wait().await) });
    let mut closed = false;

    loop {
        if let Some(prompt) = pop_next_prompt(&core, id) {
            let turn_num = begin_turn(&core, id, &prompt);
            let req = SessionPromptParams {
                session_id: session_id.clone(),
                prompt: vec![ContentBlock::Text { text: prompt.text }],
            };
            let prompt_fut = transport.request("session/prompt", &req, TURN_TIMEOUT);
            tokio::pin!(prompt_fut);
            let mut cancel_sent = false;

            let outcome = loop {
                tokio::select! {
                    res = &mut prompt_fut => break Some(res),
                    msg = started.inbound_rx.recv() => match msg {
                        Some(Inbound::Update(params)) => {
                            handle_update(&core, id, turn_num, &params);
                        }
                        Some(Inbound::Permission { rpc_id, params }) => {
                            handle_permission(&core, &transport, id, turn_num, rpc_id, &params);
                        }
                        Some(Inbound::Stderr(line)) => {
                            record_entry(&core, id, Entry::Stderr { text: line, ts_ms: crate::runtime::now_ms() });
                        }
                        Some(Inbound::Closed) | None => {
                            closed = true;
                            break None;
                        }
                    },
                    cmd = cmd_rx.recv() => match cmd {
                        Some(SessionCommand::WakeInbox) => {}
                        Some(SessionCommand::Interrupt) => {
                            cancel_pending_perm(&core, id, turn_num);
                            if !cancel_sent {
                                let _ = transport.notify("session/cancel", &SessionCancelParams { session_id: session_id.clone() });
                                cancel_sent = true;
                            }
                        }
                        Some(SessionCommand::Steer(next)) => {
                            push_front_prompt(&core, id, next);
                            cancel_pending_perm(&core, id, turn_num);
                            if !cancel_sent {
                                let _ = transport.notify("session/cancel", &SessionCancelParams { session_id: session_id.clone() });
                                cancel_sent = true;
                            }
                        }
                        Some(SessionCommand::Signal(sig)) => {
                            transport.signal(sig);
                        }
                        None => {
                            closed = true;
                            break None;
                        }
                    },
                    exited = &mut exit_fut => {
                        finish_exit(&core, id, exited);
                        return;
                    }
                }
            };

            if closed {
                break;
            }
            if let Some(res) = outcome {
                finish_turn(&core, id, turn_num, res);
            }
            continue;
        }

        if closed {
            break;
        }

        tokio::select! {
            msg = started.inbound_rx.recv() => match msg {
                Some(Inbound::Update(params)) => {
                    let turn_num = current_turn(&core, id);
                    handle_update(&core, id, turn_num, &params);
                }
                Some(Inbound::Permission { rpc_id, params }) => {
                    let turn_num = current_turn(&core, id);
                    handle_permission(&core, &transport, id, turn_num, rpc_id, &params);
                }
                Some(Inbound::Stderr(line)) => {
                    record_entry(&core, id, Entry::Stderr { text: line, ts_ms: crate::runtime::now_ms() });
                }
                Some(Inbound::Closed) | None => {
                    closed = true;
                }
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(SessionCommand::WakeInbox) => {}
                Some(SessionCommand::Steer(next)) => {
                    push_front_prompt(&core, id, next);
                }
                Some(SessionCommand::Interrupt) => {
                    cancel_pending_perm(&core, id, current_turn(&core, id));
                }
                Some(SessionCommand::Signal(sig)) => {
                    transport.signal(sig);
                }
                None => {
                    closed = true;
                }
            },
            exited = &mut exit_fut => {
                finish_exit(&core, id, exited);
                return;
            }
        }
    }

    let exited = exit_fut.await;
    finish_exit(&core, id, exited);
}

fn finish_exit(core: &Arc<Core>, id: u32, exited: Option<std::io::Result<std::process::ExitStatus>>) {
    let exit = match exited {
        Some(Ok(status)) => Exit { code: status.code(), signal: platform::exit_signal(&status) },
        Some(Err(_)) | None => Exit { code: None, signal: None },
    };
    let mut st = core.lock();
    core.finish(&mut st, id, exit);
}

fn pop_next_prompt(core: &Core, id: u32) -> Option<QueuedPrompt> {
    let mut st = core.lock();
    if !st.reg.agents.get(&id).is_some_and(|a| a.is_live()) {
        return None;
    }
    let prompt = st.inboxes.get_mut(&id).and_then(|q| q.pop_front());
    let len = st.inboxes.get(&id).map_or(0, std::collections::VecDeque::len);
    if let Some(a) = st.reg.agents.get_mut(&id) {
        a.inbox_len = len;
    }
    prompt
}

fn push_front_prompt(core: &Core, id: u32, prompt: QueuedPrompt) {
    let mut st = core.lock();
    st.inboxes.entry(id).or_default().push_front(prompt);
    let len = st.inboxes.get(&id).map_or(0, std::collections::VecDeque::len);
    if let Some(a) = st.reg.agents.get_mut(&id) {
        a.inbox_len = len;
    }
}

fn current_turn(core: &Core, id: u32) -> u64 {
    core.lock().reg.agents.get(&id).map_or(1, |a| a.turn + 1)
}

fn begin_turn(core: &Core, id: u32, prompt: &QueuedPrompt) -> u64 {
    let mut st = core.lock();
    let turn_num = st.reg.agents.get(&id).map_or(1, |a| a.turn + 1);
    let entry = Entry::User {
        turn: turn_num,
        from: prompt.from.clone(),
        text: prompt.text.clone(),
        ts_ms: crate::runtime::now_ms(),
    };
    core.record_transcript(&mut st, id, entry);
    core.apply_status(&mut st, id, &StatusReport::root("working"));
    turn_num
}

fn finish_turn(core: &Core, id: u32, turn_num: u64, res: Result<Value, CallError>) {
    let mut st = core.lock();
    st.pending_perms.remove(&id);
    if let Some(a) = st.reg.agents.get_mut(&id) {
        a.pending_permission = None;
    }
    let mut turn_err = None;
    match res {
        Ok(val) => {
            let _parsed: SessionPromptResult = serde_json::from_value(val).unwrap_or_default();
            core.apply_status(&mut st, id, &StatusReport::root("idle"));
        }
        Err(err) => {
            let msg = match err {
                CallError::Rpc(e) => e.message,
                CallError::Timeout => "ACP turn timed out".into(),
                CallError::Closed => "agent closed stdio during turn".into(),
                CallError::Io(e) => e,
            };
            turn_err = Some(msg.clone());
            core.record_transcript(
                &mut st,
                id,
                Entry::Error { turn: turn_num, message: msg.clone(), ts_ms: crate::runtime::now_ms() },
            );
            core.apply_status(
                &mut st,
                id,
                &StatusReport { state: "error", message: msg, ..StatusReport::root("error") },
            );
        }
    }
    if let Some(a) = st.reg.agents.get(&id)
        && let Some(wid) = a.labels.get("drove.worker")
    {
        let _ = core.drove.pool_tx.send(crate::drove::pool::PoolSignal::TurnFinished {
            worker_id: wid.clone(),
            agent_id: id,
            error: turn_err,
            usage: a.context_usage,
        });
    }
}

fn handle_update(core: &Core, id: u32, turn_num: u64, params: &Value) {
    let Some((_, update)) = parse_session_update(params) else {
        return;
    };
    let ts_ms = crate::runtime::now_ms();
    let entry = match update {
        ParsedUpdate::AssistantChunk(text) => Entry::Assistant { turn: turn_num, text, ts_ms },
        ParsedUpdate::ThoughtChunk(text) => Entry::Thought { turn: turn_num, text, ts_ms },
        ParsedUpdate::UserChunk(_) => return,
        ParsedUpdate::ToolCall { id: tc_id, title, kind, status } => {
            Entry::ToolCall { turn: turn_num, id: tc_id, title, tool_kind: kind, status, ts_ms }
        }
        ParsedUpdate::Plan(items) => Entry::Plan { turn: turn_num, items, ts_ms },
        ParsedUpdate::ConfigOptions(opts) => {
            let mut st = core.lock();
            if let Some(a) = st.reg.agents.get_mut(&id) {
                a.config_options = opts;
                sync_config_to_agent(a);
                let data = config_event_data(a);
                core.emit(&mut st, "config", id, data);
                core.save(&st);
            }
            return;
        }
        ParsedUpdate::CurrentMode(mode_id) => {
            let mut st = core.lock();
            if let Some(a) = st.reg.agents.get_mut(&id) {
                let val = json!(mode_id);
                if crate::permissions::is_bypass_mode_id(&mode_id) {
                    a.permission_mode = crate::permissions::PermissionMode::ApproveAll;
                }
                a.mode = Some(mode_id);
                patch_config_value(&mut a.config_options, "mode", &val);
                let data = config_event_data(a);
                core.emit(&mut st, "config", id, data);
                core.save(&st);
            }
            return;
        }
        ParsedUpdate::Usage(usage) => {
            let mut st = core.lock();
            if let Some(a) = st.reg.agents.get_mut(&id) {
                a.context_usage = Some(usage);
                core.emit(&mut st, "usage", id, json!({ "used": usage.used, "size": usage.size }));
                core.save(&st);
                core.changed.notify_waiters();
            }
            return;
        }
        ParsedUpdate::Other => return,
    };
    record_entry(core, id, entry);
}

fn handle_permission(
    core: &Core,
    transport: &Transport,
    id: u32,
    turn_num: u64,
    rpc_id: Value,
    params: &Value,
) {
    let Some(parsed) = parse_request_permission(params) else {
        transport.reply_error(rpc_id, -32602, "invalid session/request_permission params");
        return;
    };
    let mut st = core.lock();
    let mode = st.reg.agents.get(&id).map(|a| a.permission_mode).unwrap_or_default();
    let decision = permissions::decide(mode, parsed.kind.as_deref(), &parsed.options);
    let ts_ms = crate::runtime::now_ms();

    match decision {
        Decision::AutoSelect(opt_id) => {
            transport.reply_result(rpc_id, permission_selected(&opt_id));
            core.record_transcript(
                &mut st,
                id,
                Entry::Permission {
                    turn: turn_num,
                    title: parsed.title,
                    outcome: format!("auto-selected ({opt_id})"),
                    ts_ms,
                },
            );
        }
        Decision::AutoCancel => {
            transport.reply_result(rpc_id, permission_cancelled());
            core.record_transcript(
                &mut st,
                id,
                Entry::Permission { turn: turn_num, title: parsed.title, outcome: "auto-denied".into(), ts_ms },
            );
        }
        Decision::AskUser => {
            let request_id = format!("perm-{}-{}", id, platform::random_hex().get(..8).unwrap_or("0"));
            let public = PublicPermission {
                request_id: request_id.clone(),
                session_id: parsed.session_id,
                title: parsed.title.clone(),
                kind: parsed.kind.clone(),
                options: parsed.options.clone(),
            };
            st.pending_perms.insert(
                id,
                PendingPermission {
                    request_id,
                    rpc_id,
                    title: parsed.title.clone(),
                    options: parsed.options,
                    transport: transport.clone(),
                    turn: turn_num,
                },
            );
            if let Some(a) = st.reg.agents.get_mut(&id) {
                a.pending_permission = Some(public);
            }
            core.record_transcript(
                &mut st,
                id,
                Entry::Permission { turn: turn_num, title: parsed.title.clone(), outcome: "pending".into(), ts_ms },
            );
            core.apply_status(
                &mut st,
                id,
                &StatusReport {
                    state: "blocked",
                    kind: Some("permission"),
                    progress: None,
                    id: String::new(),
                    app: String::new(),
                    title: parsed.title,
                    message: "approval needed (`h2 approve` or `h2 deny`)".into(),
                },
            );
        }
    }
}

fn cancel_pending_perm(core: &Core, id: u32, turn_num: u64) {
    let mut st = core.lock();
    if let Some(pending) = st.pending_perms.remove(&id) {
        pending.transport.reply_result(pending.rpc_id, permission_cancelled());
        if let Some(a) = st.reg.agents.get_mut(&id) {
            a.pending_permission = None;
        }
        core.record_transcript(
            &mut st,
            id,
            Entry::Permission {
                turn: turn_num,
                title: pending.title,
                outcome: "cancelled".into(),
                ts_ms: crate::runtime::now_ms(),
            },
        );
    }
}

fn record_entry(core: &Core, id: u32, entry: Entry) {
    let mut st = core.lock();
    core.record_transcript(&mut st, id, entry);
}
