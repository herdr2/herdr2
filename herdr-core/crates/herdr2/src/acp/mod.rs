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

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(90);
const TURN_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

static NEXT_INBOX_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct QueuedPrompt {
    pub id: u64,
    pub text: String,
    pub from: Option<String>,
    pub queued_ms: u64,
}

impl QueuedPrompt {
    pub fn new(text: String, from: Option<String>) -> Self {
        Self {
            id: NEXT_INBOX_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            text,
            from,
            queued_ms: crate::runtime::now_ms(),
        }
    }
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

static NEXT_SESSION_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub struct SessionHandle {
    pub cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    pub transport: Transport,
    pub token: u64,
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
pub async fn spawn_session(mut spec: SpawnSpec) -> Result<StartedSession, RpcError> {
    if spec.dangerously_skip_permissions {
        presets::ensure_agy_skip_permissions(&mut spec.argv);
    }
    presets::apply_cursor_env_quirks(&spec.argv, &mut spec.env);
    presets::apply_codex_env_quirks(&spec.argv, &mut spec.env);
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

/// Resolves a requested config option id against the session's advertised `config_options`.
/// Maps reasoning aliases (`reasoning_effort` / `thought_level` / `effort` / `thinking` / `reasoning`) cleanly across adapters.
pub fn resolve_config_option_id(requested_id: &str, options: &[AcpConfigOption]) -> String {
    if options.iter().any(|o| o.id == requested_id) {
        return requested_id.to_string();
    }
    if matches!(requested_id, "reasoning_effort" | "thought_level" | "effort" | "thinking" | "reasoning")
        && let Some(found) = options.iter().find(|o| {
            o.category.as_deref() == Some("thought_level")
                || matches!(o.id.as_str(), "reasoning_effort" | "thought_level" | "effort" | "thinking" | "reasoning")
        })
    {
        return found.id.clone();
    }
    if requested_id == "fast-mode"
        && let Some(found) = options.iter().find(|o| o.id == "fast" || o.id == "fast-mode")
    {
        return found.id.clone();
    }
    requested_id.to_string()
}

fn parse_context_tokens(raw: &str) -> Option<u64> {
    let s = raw.trim().to_ascii_lowercase();
    if let Some(num) = s.strip_suffix('k') {
        return num.trim().parse::<u64>().ok().map(|n| n.saturating_mul(1_000));
    }
    if let Some(num) = s.strip_suffix('m') {
        return num.trim().parse::<u64>().ok().map(|n| n.saturating_mul(1_000_000));
    }
    s.parse::<u64>().ok()
}

/// Resolves an ACP `config_overrides` entry `(cfg_id, val)` against a session's advertised
/// `config_options`, returning `Some((wire_id, wire_value, source))` or `None` when unsupported/no-op.
pub(crate) fn resolve_config_override(
    config_options: &[AcpConfigOption],
    cfg_id: &str,
    val: &Value,
) -> Option<(String, Value, Option<String>)> {
    if cfg_id.eq_ignore_ascii_case("context") {
        let opt = config_options.iter().find(|o| o.id.eq_ignore_ascii_case("context"))?;
        let raw = val.as_str()?.trim();
        if raw.is_empty() || raw.eq_ignore_ascii_case("auto") || raw.eq_ignore_ascii_case("default") {
            return None;
        }
        if opt.options.is_empty() {
            return Some((opt.id.clone(), Value::String(raw.to_string()), Some(opt.source.clone())));
        }
        let chosen = if raw.eq_ignore_ascii_case("max") {
            opt.options
                .iter()
                .max_by_key(|c| parse_context_tokens(&c.value).unwrap_or(0))
                .map(|c| c.value.clone())?
        } else if let Some(exact) = opt.options.iter().find(|c| c.value.eq_ignore_ascii_case(raw)) {
            exact.value.clone()
        } else if let Some(target_tokens) = parse_context_tokens(raw) {
            opt.options
                .iter()
                .filter_map(|c| parse_context_tokens(&c.value).map(|toks| (toks, c)))
                .min_by(|(a, _), (b, _)| {
                    let da = a.abs_diff(target_tokens);
                    let db = b.abs_diff(target_tokens);
                    da.cmp(&db).then_with(|| b.cmp(a))
                })
                .map(|(_, c)| c.value.clone())
                .unwrap_or_else(|| raw.to_string())
        } else {
            return None;
        };
        return Some((opt.id.clone(), Value::String(chosen), Some(opt.source.clone())));
    }

    let is_effort_req = matches!(cfg_id, "reasoning_effort" | "thought_level" | "effort" | "thinking" | "reasoning");
    let opt = if is_effort_req {
        config_options
            .iter()
            .find(|o| o.id == cfg_id)
            .or_else(|| {
                config_options.iter().find(|o| {
                    o.category.as_deref() == Some("thought_level")
                        || matches!(o.id.as_str(), "reasoning_effort" | "thought_level" | "effort" | "thinking" | "reasoning")
                })
            })
    } else if matches!(cfg_id, "fast-mode" | "fast") {
        config_options
            .iter()
            .find(|o| o.id == cfg_id)
            .or_else(|| config_options.iter().find(|o| matches!(o.id.as_str(), "fast-mode" | "fast")))
    } else {
        config_options.iter().find(|o| o.id == cfg_id)
    };

    if matches!(cfg_id, "fast-mode" | "fast") {
        if let Some(opt) = opt {
            let wire_val = if opt.kind == "select" {
                if let Some(b) = val.as_bool() {
                    Value::String(if b { "true" } else { "false" }.into())
                } else {
                    val.clone()
                }
            } else {
                val.clone()
            };
            return Some((opt.id.clone(), wire_val, Some(opt.source.clone())));
        }
        return Some((cfg_id.to_string(), val.clone(), None));
    }

    if is_effort_req && let Some(opt) = opt {
        if let Some(raw) = val.as_str() {
            if opt.options.is_empty() {
                if opt.kind == "boolean" {
                    let enable = !matches!(raw.trim().to_ascii_lowercase().as_str(), "none" | "off" | "false");
                    return Some((opt.id.clone(), Value::Bool(enable), Some(opt.source.clone())));
                }
                return Some((opt.id.clone(), val.clone(), Some(opt.source.clone())));
            }
            if let Some(exact) = opt.options.iter().find(|c| c.value.eq_ignore_ascii_case(raw)) {
                return Some((opt.id.clone(), Value::String(exact.value.clone()), Some(opt.source.clone())));
            }
            if opt.id == "thinking" {
                let target = if matches!(raw.trim().to_ascii_lowercase().as_str(), "none" | "off" | "false") {
                    "false"
                } else {
                    "true"
                };
                if let Some(found) = opt.options.iter().find(|c| c.value.eq_ignore_ascii_case(target)) {
                    return Some((opt.id.clone(), Value::String(found.value.clone()), Some(opt.source.clone())));
                }
            }
            if raw.eq_ignore_ascii_case("extra-high") {
                if let Some(xh) = opt.options.iter().find(|c| c.value.eq_ignore_ascii_case("xhigh")) {
                    return Some((opt.id.clone(), Value::String(xh.value.clone()), Some(opt.source.clone())));
                }
                if let Some(high) = opt.options.iter().find(|c| c.value.eq_ignore_ascii_case("high")) {
                    return Some((opt.id.clone(), Value::String(high.value.clone()), Some(opt.source.clone())));
                }
            }
            if raw.eq_ignore_ascii_case("xhigh") {
                if let Some(xh) = opt.options.iter().find(|c| c.value.eq_ignore_ascii_case("extra-high")) {
                    return Some((opt.id.clone(), Value::String(xh.value.clone()), Some(opt.source.clone())));
                }
                if let Some(high) = opt.options.iter().find(|c| c.value.eq_ignore_ascii_case("high")) {
                    return Some((opt.id.clone(), Value::String(high.value.clone()), Some(opt.source.clone())));
                }
            }
            if raw.eq_ignore_ascii_case("med")
                && let Some(medium) = opt.options.iter().find(|c| c.value.eq_ignore_ascii_case("medium"))
            {
                return Some((opt.id.clone(), Value::String(medium.value.clone()), Some(opt.source.clone())));
            }
            return None;
        }
        return Some((opt.id.clone(), val.clone(), Some(opt.source.clone())));
    }

    let effective_id = opt.map(|o| o.id.clone()).unwrap_or_else(|| resolve_config_option_id(cfg_id, config_options));
    let src = opt.map(|o| o.source.clone());
    Some((effective_id, val.clone(), src))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SplitModelVariant {
    pub model: String,
    pub effort: Option<String>,
    pub fast: Option<bool>,
    pub context: Option<String>,
}

const EFFORT_SUFFIXES: &[(&str, &str)] = &[
    ("-extra-high", "xhigh"),
    ("-xhigh", "xhigh"),
    ("-minimal", "minimal"),
    ("-medium", "medium"),
    ("-med", "medium"),
    ("-high", "high"),
    ("-low", "low"),
    ("-none", "none"),
    ("-max", "max"),
];

/// Returns `true` when `model` encodes a reasoning/thinking effort directly (e.g. `-high`,
/// `-high-fast`, `-thinking`, `-thinking-xhigh-fast`, `-extra-high`, or `[effort=medium]`).
pub fn model_has_embedded_effort(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    if lower.is_empty() || matches!(lower.as_str(), "auto" | "default" | "default[]") {
        return false;
    }
    let mut slug = lower.as_str();
    if let Some(bracket_start) = lower.find('[')
        && lower.ends_with(']')
    {
        let inner = &lower[bracket_start + 1..lower.len() - 1];
        for entry in inner.split(',') {
            let Some((k, v)) = entry.split_once('=') else { continue };
            let k = k.trim();
            let v = v.trim();
            if matches!(k, "effort" | "reasoning_effort" | "reasoning" | "thought_level" | "thinking")
                && !v.is_empty()
                && !matches!(v, "auto" | "default")
            {
                return true;
            }
        }
        slug = lower[..bracket_start].trim();
    }
    let slug = slug.strip_suffix("-fast").unwrap_or(slug);
    slug.ends_with("-thinking") || EFFORT_SUFFIXES.iter().any(|(suffix, _)| slug.ends_with(suffix))
}

fn find_model_choice(candidate: &str, model_opt: &AcpConfigOption) -> Option<String> {
    if let Some(found) = model_opt.options.iter().find(|c| c.value == candidate) {
        return Some(found.value.clone());
    }
    if let Some(stripped) = candidate.strip_prefix("cursor-")
        && let Some(found) = model_opt.options.iter().find(|c| c.value == stripped)
    {
        return Some(found.value.clone());
    }
    None
}

fn effort_option_accepts(eff_opt: &AcpConfigOption, eff: &str) -> bool {
    if eff_opt.options.is_empty() {
        return true;
    }
    if eff_opt.options.iter().any(|c| c.value.eq_ignore_ascii_case(eff)) {
        return true;
    }
    if eff_opt.id == "thinking" {
        let want = if matches!(eff, "none" | "off" | "false") { "false" } else { "true" };
        return eff_opt.options.iter().any(|c| c.value.eq_ignore_ascii_case(want));
    }
    if eff.eq_ignore_ascii_case("xhigh") {
        return eff_opt.options.iter().any(|c| c.value.eq_ignore_ascii_case("extra-high"));
    }
    false
}

/// Splits a variant model string (such as `"gemini-4-carbon-high"`, `"grok-4.7-high-fast"`,
/// `"claude-opus-5-thinking-xhigh-fast"`, `"gpt-5.5-extra-high"`, or `"claude-opus-5-5[context=300k,effort=medium,fast=false]"`)
/// into its parameterized ACP `model` choice and extracted `effort`/`fast`/`context` values.
pub fn split_model_variant_for_options(model: &str, options: &[AcpConfigOption]) -> SplitModelVariant {
    let unsplit = || SplitModelVariant { model: model.to_string(), ..Default::default() };
    let Some(model_opt) = options.iter().find(|o| o.id == "model" || o.category.as_deref() == Some("model")) else {
        return unsplit();
    };
    if model_opt.options.is_empty() || model_opt.options.iter().any(|c| c.value == model) {
        return unsplit();
    }

    let trimmed = model.trim();
    let mut raw_slug = trimmed;
    let mut bracket_effort: Option<String> = None;
    let mut bracket_fast: Option<bool> = None;
    let mut bracket_context: Option<String> = None;
    let mut has_brackets = false;

    if let Some(bracket_start) = trimmed.find('[')
        && trimmed.ends_with(']')
    {
        has_brackets = true;
        raw_slug = trimmed[..bracket_start].trim();
        let inner = &trimmed[bracket_start + 1..trimmed.len() - 1];
        for entry in inner.split(',') {
            let Some((k, v)) = entry.split_once('=') else { continue };
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            if v.is_empty() {
                continue;
            }
            match k.as_str() {
                "context" | "context_window" if !matches!(v.to_ascii_lowercase().as_str(), "auto" | "default") => {
                    bracket_context = Some(v.to_string());
                }
                "fast" | "fast-mode" => match v.to_ascii_lowercase().as_str() {
                    "true" | "1" | "on" | "yes" => bracket_fast = Some(true),
                    "false" | "0" | "off" | "no" => bracket_fast = Some(false),
                    _ => {}
                },
                "effort" | "reasoning_effort" | "reasoning" | "thought_level" | "thinking"
                    if !matches!(v.to_ascii_lowercase().as_str(), "auto" | "default") =>
                {
                    let lower_v = v.to_ascii_lowercase();
                    let normalized = match lower_v.as_str() {
                        "extra-high" => "xhigh".to_string(),
                        "med" => "medium".to_string(),
                        _ => lower_v,
                    };
                    bracket_effort = Some(normalized);
                }
                _ => {}
            }
        }
    }

    let mut matched_base = if has_brackets { find_model_choice(raw_slug, model_opt) } else { find_model_choice(raw_slug, model_opt) };
    let mut peeled_fast: Option<bool> = None;
    let mut peeled_effort: Option<String> = None;

    if matched_base.is_none() {
        let mut s = raw_slug;
        if let Some(without_fast) = s.strip_suffix("-fast") {
            peeled_fast = Some(true);
            s = without_fast;
            if let Some(base) = find_model_choice(s, model_opt) {
                matched_base = Some(base);
            }
        }
        if matched_base.is_none() {
            for (suffix, mapped) in EFFORT_SUFFIXES {
                if let Some(without_eff) = s.strip_suffix(suffix) {
                    let base = find_model_choice(without_eff, model_opt).or_else(|| {
                        without_eff
                            .strip_suffix("-thinking")
                            .and_then(|stem| find_model_choice(stem, model_opt))
                    });
                    if let Some(base) = base {
                        matched_base = Some(base);
                        peeled_effort = Some((*mapped).to_string());
                        break;
                    }
                }
            }
        }
        if matched_base.is_none()
            && let Some(without_thinking) = s.strip_suffix("-thinking")
            && let Some(base) = find_model_choice(without_thinking, model_opt)
        {
            matched_base = Some(base);
            peeled_effort = Some("high".to_string());
        }
    }

    let Some(base_model) = matched_base else {
        return unsplit();
    };

    let final_effort = bracket_effort.clone().or(peeled_effort);
    if bracket_effort.is_none()
        && let Some(ref eff) = final_effort
    {
        let eff_id = resolve_config_option_id("reasoning_effort", options);
        if let Some(eff_opt) = options.iter().find(|o| o.id == eff_id)
            && !effort_option_accepts(eff_opt, eff)
        {
            return unsplit();
        }
    }

    SplitModelVariant {
        model: base_model,
        effort: final_effort,
        fast: bracket_fast.or(peeled_fast),
        context: bracket_context,
    }
}

/// Splits an effort-suffixed model slug (such as `"gemini-4-carbon-high"`) into base model + thought level
/// when the session advertises only base model IDs alongside a thought/reasoning option.
pub fn split_model_effort_for_options(model: &str, options: &[AcpConfigOption]) -> (String, Option<String>) {
    let split = split_model_variant_for_options(model, options);
    (split.model, split.effort)
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
            match load_session(transport, prev_id, spec, server.as_ref()).await {
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
        let split = split_model_variant_for_options(model, &config_options);
        let src = config_options.iter().find(|o| o.id == "model").map(|o| o.source.clone());
        let val = json!(split.model);
        if let Some(fresh) = apply_config_option(transport, &session_id, "model", &val, src.as_deref()).await? {
            config_options = fresh;
        } else {
            patch_config_value(&mut config_options, "model", &val);
        }
        let has_explicit_effort = spec.config_overrides.keys().any(|k| {
            matches!(
                k.as_str(),
                "reasoning_effort" | "thought_level" | "effort" | "thinking" | "reasoning"
            )
        });
        let mut derived: Vec<(&str, Value)> = Vec::new();
        if !has_explicit_effort && let Some(eff) = split.effort {
            derived.push(("reasoning_effort", json!(eff)));
        }
        if !spec.config_overrides.contains_key("fast")
            && !spec.config_overrides.contains_key("fast-mode")
            && let Some(fast) = split.fast
        {
            derived.push(("fast-mode", json!(fast)));
        }
        if !spec.config_overrides.contains_key("context")
            && let Some(ctx) = split.context
        {
            derived.push(("context", json!(ctx)));
        }
        for (cfg_id, val) in derived {
            let Some((effective_id, effective_val, src)) = resolve_config_override(&config_options, cfg_id, &val) else {
                continue;
            };
            if let Ok(maybe_fresh) =
                apply_config_option(transport, &session_id, &effective_id, &effective_val, src.as_deref()).await
            {
                if let Some(fresh) = maybe_fresh {
                    config_options = fresh;
                } else {
                    patch_config_value(&mut config_options, &effective_id, &effective_val);
                }
            }
        }
    }

    for (cfg_id, val) in &spec.config_overrides {
        let Some((effective_id, effective_val, src)) = resolve_config_override(&config_options, cfg_id, val) else {
            continue;
        };
        if let Ok(maybe_fresh) = apply_config_option(transport, &session_id, &effective_id, &effective_val, src.as_deref()).await {
            if let Some(fresh) = maybe_fresh {
                config_options = fresh;
            } else {
                patch_config_value(&mut config_options, &effective_id, &effective_val);
            }
        }
    }

    Ok(SetupOk { session_id, agent_info, can_load_session, config_options, context_usage })
}

fn build_session_meta(spec: &SpawnSpec, include_system_prompt: bool) -> Option<Value> {
    let mut obj = serde_json::Map::new();
    if include_system_prompt
        && let Some(s) = spec.system_prompt.as_deref().filter(|s| !s.is_empty())
    {
        obj.insert("systemPrompt".into(), json!({ "append": s }));
    }
    if presets::is_claude_agent(None, &spec.argv) && !spec.env.contains_key("ENABLE_TOOL_SEARCH") {
        // Claude Code applies `~/.claude/settings.json` env after subprocess env. When a user has
        // `ENABLE_TOOL_SEARCH=true` alongside non-Anthropic/Magpie routes, Turn 1 fails with
        // `400 tools.defer_loading`. Programmatic `_meta.claudeCode.options.settings.env` overrides
        // user settings cleanly so tools are declared in full on every route.
        obj.insert(
            "claudeCode".into(),
            json!({
                "options": {
                    "settings": {
                        "env": {
                            "ENABLE_TOOL_SEARCH": "false"
                        }
                    }
                }
            }),
        );
    }
    (!obj.is_empty()).then_some(Value::Object(obj))
}

async fn new_session(
    transport: &Transport,
    spec: &SpawnSpec,
    mcp: Option<&StdioMcpServer>,
) -> Result<(String, Vec<AcpConfigOption>), RpcError> {
    let meta = build_session_meta(spec, true);

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
    spec: &SpawnSpec,
    mcp: Option<&StdioMcpServer>,
) -> Result<Vec<AcpConfigOption>, CallError> {
    let meta = build_session_meta(spec, false);
    for tagged in [true, false] {
        let mcp_servers = mcp.map(|m| m.to_value(tagged)).into_iter().collect();
        let params = SessionLoadParams {
            session_id: session_id.into(),
            cwd: spec.cwd.clone(),
            mcp_servers,
            meta: meta.clone(),
        };
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
    let token = NEXT_SESSION_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let handle = SessionHandle { cmd_tx: cmd_tx.clone(), transport: started.transport.clone(), token };
    tokio::spawn(run_actor(core, id, token, started, cmd_rx));
    handle
}

async fn run_actor(
    core: Arc<Core>,
    id: u32,
    token: u64,
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
            let expected_turn = current_turn(&core, id);
            let clean_prompt = crate::transcript::strip_h2_envelope(&prompt.text);
            let wire_text = prepare_acp_prompt(&core, id, expected_turn, &prompt.text, &clean_prompt).await;
            let turn_num = begin_turn(&core, id, &prompt);
            let req = SessionPromptParams {
                session_id: session_id.clone(),
                prompt: vec![ContentBlock::Text { text: wire_text }],
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
                        finish_exit(&core, id, token, exited);
                        return;
                    }
                }
            };

            if closed {
                break;
            }
            if let Some(res) = outcome {
                // Drain buffered inbound updates delivered alongside prompt completion before snapshotting.
                while let Ok(msg) = started.inbound_rx.try_recv() {
                    match msg {
                        Inbound::Update(params) => handle_update(&core, id, turn_num, &params),
                        Inbound::Stderr(line) => {
                            record_entry(&core, id, Entry::Stderr { text: line, ts_ms: crate::runtime::now_ms() });
                        }
                        _ => {}
                    }
                }
                if session_is_current(&core, id, token) {
                    let _ = crate::checkpoint::capture_target_checkpoint(
                        &core,
                        crate::runtime::TargetKey::Agent(id),
                        Some(turn_num),
                        Some(clean_prompt),
                        None,
                        true,
                        true,
                    )
                    .await;
                }
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
                finish_exit(&core, id, token, exited);
                return;
            }
        }
    }

    let exited = exit_fut.await;
    finish_exit(&core, id, token, exited);
}

fn session_is_current(core: &Core, id: u32, token: u64) -> bool {
    core.lock().sessions.get(&id).is_some_and(|h| h.token == token)
}

async fn prepare_acp_prompt(
    core: &Arc<Core>,
    id: u32,
    turn_num: u64,
    raw_prompt: &str,
    clean_prompt: &str,
) -> String {
    let (needs_baseline, handoff) = {
        let mut st = core.lock();
        let Some(addr) = st.reg.agents.get(&id).map(|a| a.address.clone()) else {
            return raw_prompt.to_string();
        };
        let mut tc = core.target_checkpoints(&mut st, &addr);
        tc.cli_kind = Some("acp".into());
        tc.active_turn_prompt = Some(clean_prompt.to_string());
        tc.active_turn_num = Some(turn_num);
        tc.pending_rollback_turn = None;
        let handoff = tc.pending_handoff.take();
        let needs_baseline = !tc.checkpoints.iter().any(|c| c.turn == 0);
        core.save_target_checkpoints(&mut st, tc);
        (needs_baseline, handoff)
    };

    if needs_baseline {
        let _ = crate::checkpoint::capture_target_checkpoint(
            core,
            crate::runtime::TargetKey::Agent(id),
            Some(0),
            None,
            None,
            true,
            false,
        )
        .await;
    }

    match handoff {
        Some(h) if !h.is_empty() => format!("{h}\n\n{raw_prompt}"),
        _ => raw_prompt.to_string(),
    }
}

fn finish_exit(core: &Arc<Core>, id: u32, token: u64, exited: Option<std::io::Result<std::process::ExitStatus>>) {
    let exit = match exited {
        Some(Ok(status)) => Exit { code: status.code(), signal: platform::exit_signal(&status) },
        Some(Err(_)) | None => Exit { code: None, signal: None },
    };
    let mut st = core.lock();
    if st.sessions.get(&id).is_some_and(|h| h.token != token) {
        return;
    }
    core.finish(&mut st, id, exit);
}

/// Rolls back a headless ACP `a*` agent by rebuilding a clean session first, optionally restoring
/// workspace files to `to_turn`, and rewinding the transcript with a bounded portable handoff.
pub async fn rollback_acp_agent(
    core: &Arc<Core>,
    id: u32,
    to_turn: u64,
    restore_files: bool,
    confirm: bool,
) -> Result<crate::checkpoint::CheckpointRollbackResult, RpcError> {
        let (
        addr,
        cwd,
        argv,
        restart_env,
        system_prompt,
        model,
        mode,
        overrides,
        inject_mcp,
        skip_perms,
        current_turn,
        target_ref,
        prev_acp_sid,
        can_load,
        agent_info,
    ) = {
        let mut st = core.lock();
        let Some(a) = st.reg.agents.get(&id).cloned() else {
            return Err(RpcError::new("not_found", format!("agent id {id} not found"), "run `h2 list` to inspect active agents"));
        };
        if !a.is_live() {
            return Err(RpcError::new(
                "unavailable",
                format!("{} has finished; only live agents can roll back", a.address),
                "start a new agent or inspect its existing checkpoints",
            ));
        }
        if a.status.as_ref().is_some_and(|s| s.state == "working") {
            return Err(RpcError::new(
                "busy",
                format!("{} is currently working; interrupt it before rolling back", a.address),
                format!("run `h2 interrupt {}` first", a.address),
            ));
        }
        let tc = core.target_checkpoints(&mut st, &a.address);
        let max_ckpt_turn = tc.checkpoints.iter().map(|c| c.turn).max().unwrap_or(0);
        if to_turn > a.turn.max(max_ckpt_turn) {
            return Err(RpcError::invalid_params(format!(
                "cannot roll back {} to turn {to_turn} beyond turn {}",
                a.address,
                a.turn.max(max_ckpt_turn)
            )));
        }
        let target_ref = tc.checkpoints.iter().find(|c| c.turn == to_turn).map(|c| c.ref_name.clone());
        let skip_perms = a.permission_mode == permissions::PermissionMode::ApproveAll;
        (
            a.address,
            a.cwd,
            a.argv,
            a.restart_env,
            a.system_prompt,
            a.model,
            a.mode,
            a.config_overrides,
            a.inject_mcp,
            skip_perms,
            a.turn,
            target_ref,
            a.acp_session_id,
            a.can_load_session,
            a.agent_info,
        )
    };

    let repo = crate::checkpoint::probe_repo(std::path::Path::new(&cwd));
    if restore_files {
        let Some(ref r) = repo else {
            return Err(RpcError::invalid_params(format!("workspace `{cwd}` is not a Git repository; pass --no-files")));
        };
        if target_ref.is_none() {
            return Err(RpcError::invalid_params(format!("no checkpoint found for {} at turn {to_turn}", addr)));
        }
        let live_snapshot = {
            let st = core.lock();
            crate::checkpoint_safety::snapshot_live_targets(&st)
        };
        crate::checkpoint_safety::enforce_restore_safety(&addr, r, &live_snapshot, confirm)?;
    }

    // Attempt native Claude/Codex JSONL fork when rolling back to `to_turn > 0` so `session/load`
    // restores exact provider session memory (falling back cleanly to fresh session + handoff otherwise).
    let forked_sid: Option<String> = if to_turn > 0
        && let Some(ref prev_sid) = prev_acp_sid
    {
        let is_claude = presets::is_claude_agent(agent_info.as_deref(), &argv);
        let is_codex = !is_claude
            && (argv.iter().any(|s| s.to_ascii_lowercase().contains("codex"))
                || agent_info.as_deref().is_some_and(|s| s.to_ascii_lowercase().contains("codex")));
        if is_claude || is_codex {
            let cwd_c = cwd.clone();
            let sid_c = prev_sid.clone();
            let fork_uuid = crate::checkpoint::random_uuid_v4();
            let fork_uuid_c = fork_uuid.clone();
            let ok = tokio::task::spawn_blocking(move || -> bool {
                if is_claude {
                    crate::pty_adapters::find_claude_session_file(&cwd_c, &sid_c)
                        .and_then(|src| crate::pty_adapters::fork_claude_jsonl(&src, to_turn, &fork_uuid_c).ok())
                        .is_some()
                } else {
                    crate::pty_adapters::find_codex_rollout_by_sid(&cwd_c, &sid_c)
                        .and_then(|src| crate::pty_adapters::fork_codex_jsonl(&src, to_turn, &fork_uuid_c).ok())
                        .is_some()
                }
            })
            .await
            .unwrap_or(false);
            ok.then_some(fork_uuid)
        } else {
            None
        }
    } else {
        None
    };

    // Session-first rebuild: spawn a clean ACP session BEFORE mutating disk files or history.
    let base_env = platform::program_env().await;
    let env = crate::api::child_env(base_env, restart_env);
    let (env, scope) = crate::api::agent_env(core, id, &cwd, env);
    let mcp_exe = crate::client::exe_path().map(|p| p.display().to_string()).unwrap_or_else(|| "herdr2".into());
    let spec = SpawnSpec {
        address: addr.clone(),
        argv,
        env,
        cwd: cwd.clone(),
        resume_session_id: forked_sid.clone(),
        can_load_session: can_load || forked_sid.is_some(),
        system_prompt,
        model,
        mode,
        dangerously_skip_permissions: skip_perms,
        config_overrides: overrides,
        inject_mcp,
        mcp_exe,
        socket: core.paths.socket.display().to_string(),
    };
    let started = spawn_session(spec).await?;

    let slug = core.session_slug();
    if let Some(ref r) = repo {
        let _guard = core.repo_ckpt_lock(&r.toplevel).lock_owned().await;
        let r_clone = r.clone();
        let ref_to_restore = target_ref.clone();
        let cwd_path = std::path::PathBuf::from(&cwd);
        let addr_clone = addr.clone();
        let slug_clone = slug.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            if restore_files
                && let Some(ref ref_name) = ref_to_restore
            {
                crate::checkpoint::restore_checkpoint(&r_clone, ref_name, Some(&cwd_path))?;
            }
            let _ = crate::checkpoint::delete_checkpoint_refs_after(&r_clone, &slug_clone, &addr_clone, to_turn);
            Ok(())
        })
        .await
        .map_err(|e| RpcError::internal(format!("rollback task failed: {e}")))?
        .map_err(RpcError::internal)?;
    }

    let (old_handle, restored_prompt) = {
        let mut st = core.lock();
        if !st.reg.agents.get(&id).is_some_and(|a| a.is_live()) {
            started.transport.kill();
            return Err(RpcError::new("unavailable", format!("{addr} exited during rollback"), ""));
        }

        let old_handle = st.sessions.remove(&id);
        st.inboxes.remove(&id);
        st.pending_perms.remove(&id);

        let acp_sid = started.session_id.clone();
        let used_native_load = forked_sid.as_deref() == Some(acp_sid.as_str());
        let can_load = started.can_load_session;
        let agent_info = started.agent_info.clone();
        let config_options = started.config_options.clone();
        let context_usage = started.context_usage.clone();

        let new_handle = spawn_actor(Arc::clone(core), id, started);
        let new_pid = new_handle.transport.pid();
        st.sessions.insert(id, new_handle);

        let tr_path = crate::transcript::path(&core.paths.state, &addr);
        let mut tr = st
            .transcripts
            .remove(&id)
            .unwrap_or_else(|| crate::transcript::load(&core.paths.state, &addr).unwrap_or_default());
        let handoff = if to_turn > 0 && !used_native_load {
            crate::transcript::build_portable_handoff(tr.entries(), to_turn, 24 * 1024)
        } else {
            None
        };
        let restored_prompt = tr.rewind_to_turn(&tr_path, to_turn, restore_files, crate::runtime::now_ms());
        st.transcripts.insert(id, tr);

        let mut tc = core.target_checkpoints(&mut st, &addr);
        tc.cli_kind = Some("acp".into());
        tc.pending_handoff = handoff;
        tc.pending_rollback_turn = if to_turn == 0 { None } else { Some(to_turn) };
        tc.active_turn_prompt = None;
        tc.active_turn_num = None;
        tc.checkpoints.retain(|c| c.turn <= to_turn);
        core.save_target_checkpoints(&mut st, tc);

        if let Some(a) = st.reg.agents.get_mut(&id) {
            a.pid = new_pid;
            a.scope = Some(scope);
            a.acp_session_id = Some(acp_sid);
            a.can_load_session = can_load;
            a.config_options = config_options;
            if let Some(info) = agent_info {
                a.agent_info = Some(info);
            }
            if let Some(usage) = context_usage {
                a.context_usage = Some(usage);
            }
            a.inbox_len = 0;
            a.inbox.clear();
            a.pending_permission = None;
            a.program_started_ms = Some(crate::runtime::now_ms());
            sync_config_to_agent(a);
            let _ = crate::status::apply(a, &StatusReport::root("idle"));
            a.turn = to_turn.min(current_turn.max(to_turn));
        }

        let public = serde_json::to_value(crate::agents::Public(st.reg.agents[&id].clone())).unwrap_or_default();
        let rewind_entry = st.transcripts.get(&id).and_then(|t| t.entries().last()).cloned();
        core.emit(&mut st, "restarted", id, public);
        if let Some(entry) = rewind_entry {
            core.emit(&mut st, "transcript", id, json!({ "entry": entry }));
        }
        core.emit(
            &mut st,
            "checkpoint",
            id,
            json!({ "rewound_to": to_turn, "restored_files": restore_files }),
        );
        core.save(&st);
        core.changed.notify_waiters();
        (old_handle, restored_prompt)
    };

    if let Some(old) = old_handle {
        old.transport.kill();
    }

    Ok(crate::checkpoint::CheckpointRollbackResult {
        target: addr,
        turn: to_turn,
        restored_files: restore_files,
        rewound_session: true,
        restored_prompt,
    })
}

fn pop_next_prompt(core: &Core, id: u32) -> Option<QueuedPrompt> {
    let mut st = core.lock();
    if !st.reg.agents.get(&id).is_some_and(|a| a.is_live()) {
        return None;
    }
    let prompt = st.inboxes.get_mut(&id).and_then(|q| q.pop_front());
    if st.inboxes.get(&id).is_some_and(std::collections::VecDeque::is_empty) {
        st.inboxes.remove(&id);
    }
    if prompt.is_some() {
        core.sync_inbox(&mut st, id, true);
        core.save_soon();
    }
    prompt
}

fn push_front_prompt(core: &Core, id: u32, prompt: QueuedPrompt) {
    let mut st = core.lock();
    st.inboxes.entry(id).or_default().push_front(prompt);
    core.sync_inbox(&mut st, id, true);
    core.save_soon();
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
    let preserve_title = crate::ai::digest::is_followup_meta_prompt(&prompt.text);
    core.apply_status(&mut st, id, &StatusReport::root("working"));
    core.refresh_agent_digest_deterministic(&mut st, id, preserve_title);
    drop(st);
    core.spawn_agent_digest_ai(id, Duration::from_secs(2));
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
    core.refresh_agent_digest_deterministic(&mut st, id, true);
    drop(st);
    core.spawn_agent_digest_ai(id, Duration::from_secs(3));
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
        ParsedUpdate::ToolCall { id: tc_id, title, kind, status, detail } => {
            Entry::ToolCall { turn: turn_num, id: tc_id, title, tool_kind: kind, status, detail, ts_ms }
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
            core.refresh_agent_digest_deterministic(&mut st, id, true);
            drop(st);
            core.spawn_agent_digest_ai(id, Duration::from_secs(5));
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
    let refresh = matches!(entry, Entry::ToolCall { .. } | Entry::Plan { .. } | Entry::Error { .. });
    let mut st = core.lock();
    core.record_transcript(&mut st, id, entry);
    if refresh {
        core.refresh_agent_digest_deterministic(&mut st, id, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(value: &str) -> AcpConfigChoice {
        AcpConfigChoice { value: value.into(), name: value.into(), description: None, group: None }
    }

    fn agy_options() -> Vec<AcpConfigOption> {
        vec![
            AcpConfigOption {
                id: "model".into(),
                name: "Model".into(),
                description: None,
                category: Some("model".into()),
                kind: "select".into(),
                current_value: json!("gemini-4-carbon"),
                options: vec![choice("gemini-4-carbon"), choice("claude-opus-5-5")],
                source: "config".into(),
            },
            AcpConfigOption {
                id: "thought_level".into(),
                name: "Thought Level".into(),
                description: None,
                category: Some("thought_level".into()),
                kind: "select".into(),
                current_value: json!("medium"),
                options: vec![choice("low"), choice("medium"), choice("high")],
                source: "config".into(),
            },
        ]
    }

    fn claude_options() -> Vec<AcpConfigOption> {
        vec![
            AcpConfigOption {
                id: "model".into(),
                name: "Model".into(),
                description: None,
                category: Some("model".into()),
                kind: "select".into(),
                current_value: json!("claude-sonnet-4-5"),
                options: vec![choice("claude-sonnet-4-5")],
                source: "config".into(),
            },
            AcpConfigOption {
                id: "effort".into(),
                name: "Effort".into(),
                description: None,
                category: Some("thought_level".into()),
                kind: "select".into(),
                current_value: json!("high"),
                options: vec![choice("low"), choice("medium"), choice("high"), choice("max")],
                source: "config".into(),
            },
        ]
    }

    #[test]
    fn resolves_reasoning_effort_aliases_for_agy_and_claude() {
        let agy = agy_options();
        assert_eq!(resolve_config_option_id("reasoning_effort", &agy), "thought_level");
        assert_eq!(resolve_config_option_id("thought_level", &agy), "thought_level");
        assert_eq!(resolve_config_option_id("model", &agy), "model");

        let claude = claude_options();
        assert_eq!(resolve_config_option_id("reasoning_effort", &claude), "effort");
        assert_eq!(resolve_config_option_id("effort", &claude), "effort");
    }

    #[test]
    fn splits_agy_suffixed_model_slug_when_options_use_base_ids() {
        let agy = agy_options();
        assert_eq!(
            split_model_effort_for_options("gemini-4-carbon-high", &agy),
            ("gemini-4-carbon".into(), Some("high".into()))
        );
        assert_eq!(
            split_model_effort_for_options("claude-opus-5-5-medium", &agy),
            ("claude-opus-5-5".into(), Some("medium".into()))
        );
        // Base model stays unsplit; unknown suffix stays unsplit.
        assert_eq!(
            split_model_effort_for_options("gemini-4-carbon", &agy),
            ("gemini-4-carbon".into(), None)
        );
        assert_eq!(
            split_model_effort_for_options("gemini-4-carbon-xhigh", &agy),
            ("gemini-4-carbon-xhigh".into(), None)
        );
    }

    #[test]
    fn resolves_cursor_context_and_fast_config_overrides() {
        let opts = vec![
            AcpConfigOption {
                id: "model".into(),
                name: "Model".into(),
                description: None,
                category: Some("model".into()),
                kind: "select".into(),
                current_value: json!("grok-4.7"),
                options: vec![choice("grok-4.7")],
                source: "config".into(),
            },
            AcpConfigOption {
                id: "context".into(),
                name: "Context".into(),
                description: None,
                category: Some("model_config".into()),
                kind: "select".into(),
                current_value: json!("256k"),
                options: vec![choice("256k"), choice("500k")],
                source: "config".into(),
            },
            AcpConfigOption {
                id: "fast".into(),
                name: "Fast".into(),
                description: None,
                category: Some("model_config".into()),
                kind: "select".into(),
                current_value: json!("false"),
                options: vec![choice("false"), choice("true")],
                source: "config".into(),
            },
        ];

        assert_eq!(
            resolve_config_override(&opts, "context", &json!("500k")),
            Some(("context".into(), json!("500k"), Some("config".into())))
        );
        assert_eq!(
            resolve_config_override(&opts, "context", &json!("max")),
            Some(("context".into(), json!("500k"), Some("config".into())))
        );
        assert_eq!(
            resolve_config_override(&opts, "context", &json!("1m")),
            Some(("context".into(), json!("500k"), Some("config".into())))
        );
        assert_eq!(
            resolve_config_override(&opts, "context", &json!("auto")),
            None
        );
        assert_eq!(
            resolve_config_override(&opts, "fast-mode", &json!(true)),
            Some(("fast".into(), json!("true"), Some("config".into())))
        );
        // No context option advertised on this model -> returns None cleanly.
        let agy = agy_options();
        assert_eq!(resolve_config_override(&agy, "context", &json!("500k")), None);
        assert_eq!(
            resolve_config_override(&agy, "reasoning_effort", &json!("xhigh")),
            Some(("thought_level".into(), json!("high"), Some("config".into())))
        );
    }

    #[test]
    fn detects_embedded_effort_and_splits_cursor_model_variants() {
        for embedded in [
            "grok-4.7-high",
            "grok-4.7-high-fast",
            "claude-opus-5-thinking-xhigh-fast",
            "gpt-5.5-extra-high",
            "gpt-5.6-sol-none",
            "muse-wonder-1.0-minimal",
            "sonnet-4-thinking",
            "claude-opus-5-5[context=300k,effort=medium]",
            "grok-4.7[reasoning=high]",
        ] {
            assert!(model_has_embedded_effort(embedded), "{embedded} should report embedded effort");
        }
        for base in [
            "",
            "auto",
            "default",
            "grok-4.7",
            "claude-opus-5-5",
            "gpt-5.3-codex-fast",
            "composer-2.5-fast",
            "grok-4.7[context=500k,fast=true]",
        ] {
            assert!(!model_has_embedded_effort(base), "{base} should not report embedded effort");
        }

        // Before model selection on Cursor ACP, only "model" options list base ids (without effort/fast/context yet).
        let cursor_initial = vec![AcpConfigOption {
            id: "model".into(),
            name: "Model".into(),
            description: None,
            category: Some("model".into()),
            kind: "select".into(),
            current_value: json!("default"),
            options: vec![
                choice("grok-4.7"),
                choice("claude-opus-5"),
                choice("claude-opus-5-5"),
                choice("gpt-5.5"),
                choice("gpt-5.3-codex-fast"),
            ],
            source: "config".into(),
        }];

        assert_eq!(
            split_model_variant_for_options("gpt-5.3-codex-fast", &cursor_initial),
            SplitModelVariant { model: "gpt-5.3-codex-fast".into(), effort: None, fast: None, context: None }
        );
        assert_eq!(
            split_model_variant_for_options("grok-4.7-high-fast", &cursor_initial),
            SplitModelVariant {
                model: "grok-4.7".into(),
                effort: Some("high".into()),
                fast: Some(true),
                context: None,
            }
        );
        assert_eq!(
            split_model_variant_for_options("claude-opus-5-thinking-xhigh-fast", &cursor_initial),
            SplitModelVariant {
                model: "claude-opus-5".into(),
                effort: Some("xhigh".into()),
                fast: Some(true),
                context: None,
            }
        );
        assert_eq!(
            split_model_variant_for_options("gpt-5.5-extra-high", &cursor_initial),
            SplitModelVariant {
                model: "gpt-5.5".into(),
                effort: Some("xhigh".into()),
                fast: None,
                context: None,
            }
        );
        assert_eq!(
            split_model_variant_for_options("claude-opus-5-5[context=300k,effort=medium,fast=false]", &cursor_initial),
            SplitModelVariant {
                model: "claude-opus-5-5".into(),
                effort: Some("medium".into()),
                fast: Some(false),
                context: Some("300k".into()),
            }
        );

        let cursor_reasoning_opts = vec![AcpConfigOption {
            id: "reasoning".into(),
            name: "Reasoning".into(),
            description: None,
            category: Some("thought_level".into()),
            kind: "select".into(),
            current_value: json!("medium"),
            options: vec![choice("none"), choice("low"), choice("medium"), choice("high"), choice("xhigh")],
            source: "config".into(),
        }];
        assert_eq!(resolve_config_option_id("reasoning_effort", &cursor_reasoning_opts), "reasoning");
        assert_eq!(
            resolve_config_override(&cursor_reasoning_opts, "reasoning_effort", &json!("high")),
            Some(("reasoning".into(), json!("high"), Some("config".into())))
        );
        assert_eq!(
            resolve_config_override(&cursor_reasoning_opts, "reasoning_effort", &json!("extra-high")),
            Some(("reasoning".into(), json!("xhigh"), Some("config".into())))
        );
    }
}
