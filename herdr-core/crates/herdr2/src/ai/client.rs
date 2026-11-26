//! OpenAI & Claude (Anthropic Messages) HTTP transport (via pooled `reqwest` client),
//! automatic protocol detection, endpoint capability memory, payload fallback,
//! model discovery, and multi-vendor cache-token accounting.

use super::config::{is_loopback_http_url, validate_http_url};
use super::session::ChatMessage;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const AI_MODELS_TIMEOUT: Duration = Duration::from_secs(4);
pub const AI_CHAT_TIMEOUT: Duration = Duration::from_secs(12);
pub const ANTHROPIC_API_VERSION: &str = "2023-06-01";
/// Number of consecutive >=1024-token shared-prefix calls with `cached_input_tokens == 0`
/// before automatically degrading `StatelessSharedPrefix` to compact per-op prompts.
pub const KV_CACHE_DEGRADE_THRESHOLD: u8 = 5;

const STRICT_CLOUD_HOSTS: [&str; 6] = [
    "api.openai.com",
    "api.deepseek.com",
    "open.bigmodel.cn",
    "dashscope.aliyuncs.com",
    "api.anthropic.com",
    "generativelanguage.googleapis.com",
];

/// Wire protocol spoken by an AI endpoint (auto-detected without user selection).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AiProtocol {
    #[default]
    Openai,
    Anthropic,
}

impl AiProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Openai => Self::Anthropic,
            Self::Anthropic => Self::Openai,
        }
    }
}

impl fmt::Display for AiProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpAuthStyle {
    /// OpenAI style: `Authorization: Bearer <key>`
    Bearer,
    /// Anthropic standard style: `x-api-key: <key>` + `anthropic-version`
    AnthropicApiKey,
    /// Anthropic proxy Bearer style: `Authorization: Bearer <key>` + `anthropic-version`
    AnthropicBearer,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug)]
pub struct HttpJsonResponse {
    pub status: u16,
    pub body: String,
    pub retry_after_secs: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiTestResult {
    pub ok: bool,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<AiProtocol>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovered_model: Option<String>,
    #[serde(default)]
    pub available_models: Vec<String>,
    pub latency_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiModelsResult {
    pub ok: bool,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<AiProtocol>,
    #[serde(default)]
    pub models: Vec<String>,
    pub latency_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Remembered protocol, optional-field compatibility & KV-cache effectiveness per `(base_url, model)`
/// so repeated requests skip 400/422 probe roundtrips and avoid paying full price for
/// uncached large shared prefixes on gateways without prompt caching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<AiProtocol>,
    #[serde(default)]
    pub anthropic_bearer_auth: bool,
    #[serde(default = "default_true")]
    pub supports_prompt_cache_key: bool,
    #[serde(default = "default_true")]
    pub supports_reasoning_effort: bool,
    #[serde(default = "default_true")]
    pub supports_effort_none: bool,
    #[serde(default)]
    pub supports_enable_thinking: bool,
    #[serde(default)]
    pub supports_num_ctx: bool,
    #[serde(default = "default_true")]
    pub supports_temperature: bool,
    #[serde(default = "default_true")]
    pub supports_json_object: bool,
    #[serde(default)]
    pub supports_cache_control: bool,
    #[serde(default = "default_true")]
    pub supports_anthropic_cache_control: bool,
    #[serde(default)]
    pub prefix_probes_without_cache: u8,
    #[serde(default)]
    pub prefix_probes_with_cache: u8,
    #[serde(default = "default_true")]
    pub prompt_cache_effective: bool,
}

impl EndpointCapabilities {
    pub fn default_for_url(base_url: &str) -> Self {
        let strict_cloud = is_known_strict_cloud_url(base_url);
        let anthropic_like = is_anthropic_compat_url(base_url);
        let enable_thinking_host = is_enable_thinking_host(base_url);
        Self {
            protocol: None,
            anthropic_bearer_auth: false,
            supports_prompt_cache_key: !anthropic_like,
            supports_reasoning_effort: true,
            supports_effort_none: !enable_thinking_host,
            supports_enable_thinking: enable_thinking_host,
            supports_num_ctx: !strict_cloud,
            supports_temperature: true,
            supports_json_object: !anthropic_like,
            supports_cache_control: anthropic_like,
            supports_anthropic_cache_control: true,
            prefix_probes_without_cache: 0,
            prefix_probes_with_cache: 0,
            prompt_cache_effective: true,
        }
    }

    /// Clears prior KV-cache auto-detection state so the runtime re-probes this endpoint/model.
    pub fn reset_prompt_cache_detection(&mut self) {
        self.prefix_probes_without_cache = 0;
        self.prefix_probes_with_cache = 0;
        self.prompt_cache_effective = true;
    }

    /// Marks the active endpoint/model as not supporting KV prefix caching (switches auto mode to compact).
    pub fn mark_prompt_cache_ineffective(&mut self) {
        self.prefix_probes_without_cache = KV_CACHE_DEGRADE_THRESHOLD;
        self.prefix_probes_with_cache = 0;
        self.prompt_cache_effective = false;
    }

    /// Returns the remembered auto-detected prompt mode (`Some("shared_prefix")`, `Some("compact")`),
    /// or `None` when not yet determined / freshly reset.
    pub fn detected_prompt_mode(&self) -> Option<&'static str> {
        if !self.prompt_cache_effective {
            Some("compact")
        } else if self.prefix_probes_with_cache > 0 {
            Some("shared_prefix")
        } else {
            None
        }
    }

    /// Updates KV-cache effectiveness tracking after a `StatelessSharedPrefix` call using the
    /// canonical `>1024`-token control-plane system prompt. Returns `true` when state changed.
    pub fn record_shared_prefix_usage(&mut self, input_tokens: u64, cached_input_tokens: u64) -> bool {
        if input_tokens < 1024 {
            return false;
        }
        if cached_input_tokens > 0 {
            let next_hits = self.prefix_probes_with_cache.saturating_add(1);
            let changed = self.prefix_probes_without_cache != 0
                || !self.prompt_cache_effective
                || next_hits != self.prefix_probes_with_cache;
            self.prefix_probes_without_cache = 0;
            self.prefix_probes_with_cache = next_hits;
            self.prompt_cache_effective = true;
            return changed;
        }
        let next = self.prefix_probes_without_cache.saturating_add(1);
        let changed = next != self.prefix_probes_without_cache
            || (next >= KV_CACHE_DEGRADE_THRESHOLD && self.prompt_cache_effective);
        self.prefix_probes_without_cache = next;
        if next >= KV_CACHE_DEGRADE_THRESHOLD {
            self.prompt_cache_effective = false;
            self.prefix_probes_with_cache = 0;
        }
        changed
    }
}

#[derive(Clone, Debug)]
pub struct PayloadVariant {
    pub body: String,
    pub used_cache_key: bool,
    pub used_effort: Option<String>,
    pub used_enable_thinking: bool,
    pub used_num_ctx: bool,
    pub used_temperature: bool,
    pub used_json_object: bool,
    pub used_cache_control: bool,
}

fn extract_host(base: &str) -> &str {
    let rest = base.trim().strip_prefix("https://").or_else(|| base.trim().strip_prefix("http://")).unwrap_or("");
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    }
}

pub fn is_known_strict_cloud_url(base: &str) -> bool {
    let host = extract_host(base);
    STRICT_CLOUD_HOSTS.iter().any(|h| host.eq_ignore_ascii_case(h))
}

/// Returns true when an endpoint URL may operate without an API key (e.g. local Magpie proxy
/// or custom self-hosted OpenAI-compatible gateway that isn't a known strict cloud API host).
pub fn allows_keyless_url(base: &str) -> bool {
    let trimmed = base.trim();
    !trimmed.is_empty() && !is_known_strict_cloud_url(trimmed)
}

pub fn is_anthropic_compat_url(base: &str) -> bool {
    let host = extract_host(base);
    let lower = base.trim().trim_end_matches('/').to_ascii_lowercase();
    host.eq_ignore_ascii_case("api.anthropic.com")
        || lower.contains("/anthropic")
        || lower.contains("/claude")
        || lower.ends_with("/messages")
}

/// Infers which wire protocol to probe first from URL and API key shape.
/// Both protocols are still probed automatically on mismatch unless locked in capability cache.
pub fn infer_preferred_protocol(base_url: &str, api_key: Option<&str>) -> AiProtocol {
    let lower = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    if lower.ends_with("/chat/completions") {
        return AiProtocol::Openai;
    }
    if is_anthropic_compat_url(&lower) {
        return AiProtocol::Anthropic;
    }
    if api_key.map(str::trim).is_some_and(|k| k.starts_with("sk-ant-")) {
        return AiProtocol::Anthropic;
    }
    AiProtocol::Openai
}

pub fn protocol_probe_order(
    base_url: &str,
    api_key: Option<&str>,
    remembered: Option<AiProtocol>,
) -> [AiProtocol; 2] {
    let first = remembered.unwrap_or_else(|| infer_preferred_protocol(base_url, api_key));
    [first, first.other()]
}

pub fn is_enable_thinking_host(base: &str) -> bool {
    let host = extract_host(base);
    host.eq_ignore_ascii_case("dashscope.aliyuncs.com")
        || host.eq_ignore_ascii_case("dashscope-intl.aliyuncs.com")
        || host.eq_ignore_ascii_case("open.bigmodel.cn")
}

pub fn truncate_for_context(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if max_chars == 0 || total <= max_chars {
        return text.to_string();
    }
    let half = (max_chars / 2).max(32);
    if half * 2 >= total {
        return text.to_string();
    }
    let head: String = text.chars().take(half).collect();
    let tail: String = text.chars().skip(total - half).collect();
    format!("{head}\n...[truncated to fit configured context window]...\n{tail}")
}

pub fn effective_max_tokens(
    default_max: u32,
    configured_max: Option<u32>,
    effort: &str,
    ctx_tokens: Option<u32>,
    cap_strict: bool,
) -> u32 {
    let raw = if cap_strict {
        match configured_max {
            Some(n) => default_max.min(n),
            None => default_max,
        }
    } else {
        match configured_max {
            Some(n) => n,
            None => match effort {
                "high" => default_max.max(4096),
                "medium" => default_max.max(2048),
                "auto" => default_max.max(1024),
                _ => default_max.max(512),
            },
        }
    };
    match ctx_tokens {
        Some(ctx) => raw.min((ctx / 2).max(64)),
        None => raw,
    }
}

fn encode_messages(messages: &[ChatMessage], use_cache_control: bool) -> Value {
    if !use_cache_control {
        return serde_json::to_value(messages).unwrap_or_else(|_| json!([]));
    }
    let encoded: Vec<Value> = messages
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            if idx == 0 && m.role == "system" {
                json!({
                    "role": "system",
                    "content": [{
                        "type": "text",
                        "text": m.content,
                        "cache_control": { "type": "ephemeral" }
                    }]
                })
            } else {
                json!({ "role": m.role, "content": m.content })
            }
        })
        .collect();
    Value::Array(encoded)
}

#[allow(clippy::too_many_arguments)]
pub fn build_chat_payload_candidates(
    model: &str,
    messages: &[ChatMessage],
    max_tokens: u32,
    effort: &str,
    ctx_tokens: Option<u32>,
    temperature: f32,
    prompt_cache_key: Option<&str>,
    expect_json: bool,
    caps: EndpointCapabilities,
) -> Vec<PayloadVariant> {
    let want_effort = if !caps.supports_reasoning_effort || effort == "auto" || effort.is_empty() {
        None
    } else if effort == "none" && !caps.supports_effort_none {
        if caps.supports_enable_thinking { None } else { Some("low".to_string()) }
    } else {
        Some(effort.to_string())
    };
    let want_enable_thinking = effort == "none" && caps.supports_enable_thinking;
    let want_ctx = ctx_tokens.filter(|_| caps.supports_num_ctx);
    let want_cache_key = prompt_cache_key.filter(|s| caps.supports_prompt_cache_key && !s.is_empty());
    let want_temp = caps.supports_temperature;
    let want_json = expect_json && caps.supports_json_object;
    let want_cache_control = prompt_cache_key.is_some() && caps.supports_cache_control;

    let make_variant = |eff: Option<&str>,
                        enable_thinking: bool,
                        ctx: Option<u32>,
                        cache_key: Option<&str>,
                        with_temp: bool,
                        with_json: bool,
                        with_cache_control: bool|
     -> PayloadVariant {
        let mut obj = serde_json::Map::new();
        obj.insert("model".into(), json!(model));
        obj.insert("messages".into(), encode_messages(messages, with_cache_control));
        obj.insert("max_tokens".into(), json!(max_tokens));
        if with_temp {
            obj.insert("temperature".into(), json!(temperature));
        }
        if let Some(e) = eff {
            obj.insert("reasoning_effort".into(), json!(e));
        }
        if enable_thinking {
            obj.insert("enable_thinking".into(), json!(false));
        }
        if with_json {
            obj.insert("response_format".into(), json!({ "type": "json_object" }));
        }
        if let Some(c) = ctx {
            obj.insert("options".into(), json!({ "num_ctx": c }));
        }
        if let Some(ck) = cache_key {
            obj.insert("prompt_cache_key".into(), json!(ck));
        }
        PayloadVariant {
            body: Value::Object(obj).to_string(),
            used_cache_key: cache_key.is_some(),
            used_effort: eff.map(String::from),
            used_enable_thinking: enable_thinking,
            used_num_ctx: ctx.is_some(),
            used_temperature: with_temp,
            used_json_object: with_json,
            used_cache_control: with_cache_control,
        }
    };

    let mut out = Vec::new();
    let push_unique = |list: &mut Vec<PayloadVariant>, item: PayloadVariant| {
        if !list.iter().any(|x| x.body == item.body) {
            list.push(item);
        }
    };

    // 1. Full remembered-compatible payload.
    push_unique(
        &mut out,
        make_variant(
            want_effort.as_deref(),
            want_enable_thinking,
            want_ctx,
            want_cache_key,
            want_temp,
            want_json,
            want_cache_control,
        ),
    );
    // 2. If `effort = "none"` was requested, fall back to `"low"` before stripping effort.
    if want_effort.as_deref() == Some("none") {
        push_unique(
            &mut out,
            make_variant(
                Some("low"),
                want_enable_thinking,
                want_ctx,
                want_cache_key,
                want_temp,
                want_json,
                want_cache_control,
            ),
        );
    }
    let eff_next = if want_effort.as_deref() == Some("none") { Some("low") } else { want_effort.as_deref() };
    // 3. Strip `enable_thinking` or `cache_control` if rejected.
    if want_enable_thinking || want_cache_control {
        push_unique(&mut out, make_variant(eff_next, false, want_ctx, want_cache_key, want_temp, want_json, false));
    }
    // 4. Strip `prompt_cache_key` if strict proxy rejects it.
    if want_cache_key.is_some() {
        push_unique(&mut out, make_variant(eff_next, false, want_ctx, None, want_temp, want_json, false));
    }
    // 5. Strip `response_format` if endpoint/model does not support `json_object`.
    if want_json {
        push_unique(&mut out, make_variant(eff_next, false, want_ctx, None, want_temp, false, false));
    }
    // 6. Strip `options.num_ctx`.
    if want_ctx.is_some() {
        push_unique(&mut out, make_variant(eff_next, false, None, None, want_temp, false, false));
    }
    // 7. Bare minimal OpenAI payload.
    push_unique(&mut out, make_variant(None, false, None, None, false, false, false));
    out
}

/// Builds Anthropic Messages API (`POST /v1/messages`) payload candidates with progressive
/// optional-field fallback (`cache_control` -> plain text -> bare minimal without `temperature`).
pub fn build_anthropic_payload_candidates(
    model: &str,
    messages: &[ChatMessage],
    max_tokens: u32,
    temperature: f32,
    prompt_cache_key: Option<&str>,
    caps: EndpointCapabilities,
) -> Vec<PayloadVariant> {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut chat_msgs: Vec<(String, String)> = Vec::new();
    for m in messages {
        let role = m.role.trim();
        if role == "system" {
            let txt = m.content.trim();
            if !txt.is_empty() {
                system_parts.push(txt);
            }
            continue;
        }
        let mapped_role = if role == "assistant" { "assistant" } else { "user" };
        if let Some((last_role, last_content)) = chat_msgs.last_mut()
            && last_role == mapped_role
        {
            if !m.content.is_empty() {
                if !last_content.is_empty() {
                    last_content.push_str("\n\n");
                }
                last_content.push_str(&m.content);
            }
            continue;
        }
        chat_msgs.push((mapped_role.to_string(), m.content.clone()));
    }
    if chat_msgs.is_empty() || chat_msgs[0].0 != "user" {
        chat_msgs.insert(0, ("user".into(), "Continue.".into()));
    }
    let system_text = system_parts.join("\n\n");
    let want_cache_control = prompt_cache_key.is_some() && caps.supports_anthropic_cache_control;
    let want_temp = caps.supports_temperature;

    let make_variant = |with_temp: bool, with_cache_control: bool| -> PayloadVariant {
        let mut obj = serde_json::Map::new();
        obj.insert("model".into(), json!(model));
        obj.insert("max_tokens".into(), json!(max_tokens.max(1)));
        if !system_text.is_empty() {
            if with_cache_control {
                obj.insert(
                    "system".into(),
                    json!([{
                        "type": "text",
                        "text": system_text,
                        "cache_control": { "type": "ephemeral" }
                    }]),
                );
            } else {
                obj.insert("system".into(), json!(system_text));
            }
        }
        let cache_msg_idx = if with_cache_control && chat_msgs.len() >= 3 {
            Some(chat_msgs.len() - 2)
        } else {
            None
        };
        let encoded_msgs: Vec<Value> = chat_msgs
            .iter()
            .enumerate()
            .map(|(idx, (role, content))| {
                if cache_msg_idx == Some(idx) && !content.is_empty() {
                    json!({
                        "role": role,
                        "content": [{
                            "type": "text",
                            "text": content,
                            "cache_control": { "type": "ephemeral" }
                        }]
                    })
                } else {
                    json!({ "role": role, "content": content })
                }
            })
            .collect();
        obj.insert("messages".into(), Value::Array(encoded_msgs));
        if with_temp {
            obj.insert("temperature".into(), json!(temperature));
        }
        PayloadVariant {
            body: Value::Object(obj).to_string(),
            used_cache_key: false,
            used_effort: None,
            used_enable_thinking: false,
            used_num_ctx: false,
            used_temperature: with_temp,
            used_json_object: false,
            used_cache_control: with_cache_control,
        }
    };

    let mut out = Vec::new();
    let push_unique = |list: &mut Vec<PayloadVariant>, item: PayloadVariant| {
        if !list.iter().any(|x| x.body == item.body) {
            list.push(item);
        }
    };

    push_unique(&mut out, make_variant(want_temp, want_cache_control));
    if want_cache_control {
        push_unique(&mut out, make_variant(want_temp, false));
    }
    push_unique(&mut out, make_variant(false, false));
    out
}

pub fn should_retry_without_optional_fields(status: u16, body: &str) -> bool {
    if status != 400 && status != 422 {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    [
        "prompt_cache_key",
        "cache_control",
        "response_format",
        "json_object",
        "enable_thinking",
        "options",
        "num_ctx",
        "reasoning_effort",
        "temperature",
        "system",
        "unknown",
        "unsupported",
        "unrecognized",
        "extra_forbidden",
        "invalid",
    ]
    .iter()
    .any(|kw| lower.contains(kw))
}

/// Updates remembered capabilities based on a rejected optional-field 400/422 response body.
pub fn refine_capabilities_from_error(caps: &mut EndpointCapabilities, attempted: &PayloadVariant, body: &str) {
    refine_capabilities_from_error_for_protocol(caps, AiProtocol::Openai, attempted, body);
}

pub fn refine_capabilities_from_error_for_protocol(
    caps: &mut EndpointCapabilities,
    protocol: AiProtocol,
    attempted: &PayloadVariant,
    body: &str,
) {
    let lower = body.to_ascii_lowercase();
    let mut matched_specific = false;
    if attempted.used_cache_key && lower.contains("prompt_cache_key") {
        caps.supports_prompt_cache_key = false;
        matched_specific = true;
    }
    if attempted.used_cache_control
        && (lower.contains("cache_control") || lower.contains("content") || lower.contains("system"))
    {
        match protocol {
            AiProtocol::Openai => caps.supports_cache_control = false,
            AiProtocol::Anthropic => caps.supports_anthropic_cache_control = false,
        }
        matched_specific = true;
    }
    if attempted.used_json_object && (lower.contains("response_format") || lower.contains("json_object")) {
        caps.supports_json_object = false;
        matched_specific = true;
    }
    if attempted.used_enable_thinking && lower.contains("enable_thinking") {
        caps.supports_enable_thinking = false;
        matched_specific = true;
    }
    if attempted.used_num_ctx && (lower.contains("options") || lower.contains("num_ctx")) {
        caps.supports_num_ctx = false;
        matched_specific = true;
    }
    if attempted.used_effort.as_deref() == Some("none") && lower.contains("reasoning_effort") {
        caps.supports_effort_none = false;
        matched_specific = true;
    } else if attempted.used_effort.is_some() && lower.contains("reasoning_effort") {
        caps.supports_reasoning_effort = false;
        matched_specific = true;
    }
    if attempted.used_temperature && lower.contains("temperature") {
        caps.supports_temperature = false;
        matched_specific = true;
    }
    if !matched_specific {
        if attempted.used_cache_control {
            match protocol {
                AiProtocol::Openai => caps.supports_cache_control = false,
                AiProtocol::Anthropic => caps.supports_anthropic_cache_control = false,
            }
        }
        if attempted.used_enable_thinking {
            caps.supports_enable_thinking = false;
        }
        if attempted.used_cache_key {
            caps.supports_prompt_cache_key = false;
        }
        if attempted.used_json_object {
            caps.supports_json_object = false;
        }
        if attempted.used_num_ctx {
            caps.supports_num_ctx = false;
        }
    }
}

/// Updates remembered capabilities once a specific fallback variant succeeds.
pub fn record_winning_variant(caps: &mut EndpointCapabilities, winning: &PayloadVariant) {
    record_winning_variant_for_protocol(caps, AiProtocol::Openai, winning);
}

pub fn record_winning_variant_for_protocol(
    caps: &mut EndpointCapabilities,
    protocol: AiProtocol,
    winning: &PayloadVariant,
) {
    match protocol {
        AiProtocol::Openai => {
            if !winning.used_cache_key {
                caps.supports_prompt_cache_key = false;
            }
            if !winning.used_cache_control {
                caps.supports_cache_control = false;
            }
            if !winning.used_enable_thinking {
                caps.supports_enable_thinking = false;
            }
            if !winning.used_num_ctx {
                caps.supports_num_ctx = false;
            }
        }
        AiProtocol::Anthropic => {
            if !winning.used_cache_control {
                caps.supports_anthropic_cache_control = false;
            }
        }
    }
    if !winning.used_temperature {
        caps.supports_temperature = false;
    }
}

/// Returns true when the JSON response body matches the structural shape of the expected wire protocol.
pub fn is_valid_protocol_response(parsed: &Value, protocol: AiProtocol) -> bool {
    match protocol {
        AiProtocol::Openai => parsed["choices"].as_array().is_some_and(|arr| !arr.is_empty()),
        AiProtocol::Anthropic => {
            parsed["content"].as_array().is_some() || parsed["type"].as_str() == Some("message")
        }
    }
}

/// Extracts `(assistant_text, normalized_finish_reason)` for OpenAI or Anthropic Messages responses.
pub fn extract_response_content(parsed: &Value, protocol: AiProtocol) -> (String, String) {
    match protocol {
        AiProtocol::Openai => {
            let content = parsed["choices"][0]["message"]["content"].as_str().map(str::trim).unwrap_or("").to_string();
            let finish = parsed["choices"][0]["finish_reason"].as_str().unwrap_or("").to_string();
            (content, finish)
        }
        AiProtocol::Anthropic => {
            let mut text = String::new();
            if let Some(blocks) = parsed["content"].as_array() {
                for block in blocks {
                    let is_text = block["type"].as_str().is_none_or(|t| t == "text");
                    if is_text
                        && let Some(chunk) = block["text"].as_str()
                    {
                        text.push_str(chunk);
                    }
                }
            }
            let stop_reason = parsed["stop_reason"].as_str().unwrap_or("");
            let normalized_finish = if stop_reason == "max_tokens" { "length" } else { stop_reason };
            (text.trim().to_string(), normalized_finish.to_string())
        }
    }
}

/// Extracts `(input_tokens, output_tokens, cached_input_tokens)` across OpenAI, DeepSeek, and Anthropic schemas.
pub fn parse_usage_tokens(parsed: &Value) -> (u64, u64, u64) {
    let u = &parsed["usage"];
    let output = u["completion_tokens"].as_u64().or_else(|| u["output_tokens"].as_u64()).unwrap_or(0);
    let cache_creation = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
    let cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
    let cached = u["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .or_else(|| u["prompt_cache_hit_tokens"].as_u64())
        .or_else(|| u["cache_read_input_tokens"].as_u64())
        .or_else(|| u["input_tokens_details"]["cached_tokens"].as_u64())
        .unwrap_or(0);
    let input = if let Some(pt) = u["prompt_tokens"].as_u64() {
        pt
    } else if let Some(raw_in) = u["input_tokens"].as_u64() {
        if u.get("cache_creation_input_tokens").is_some() || cache_read > raw_in {
            raw_in.saturating_add(cache_creation).saturating_add(cache_read)
        } else {
            raw_in
        }
    } else {
        0
    };
    (input, output, cached)
}

pub fn extract_json_object(raw: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(raw.trim())
        && v.is_object()
    {
        return Some(v);
    }
    for (idx, _) in raw.match_indices('{') {
        let mut de = serde_json::Deserializer::from_str(&raw[idx..]).into_iter::<Value>();
        if let Some(Ok(v)) = de.next()
            && v.is_object()
        {
            return Some(v);
        }
    }
    None
}

fn strip_terminal_endpoint_suffix(base: &str) -> &str {
    base.trim_end_matches('/')
        .strip_suffix("/chat/completions")
        .or_else(|| base.trim_end_matches('/').strip_suffix("/messages"))
        .unwrap_or_else(|| base.trim_end_matches('/'))
        .trim_end_matches('/')
}

pub fn models_endpoints(base: &str) -> Vec<String> {
    let root = strip_terminal_endpoint_suffix(base);
    if root.ends_with("/v1") || root.contains("/v1/") {
        vec![format!("{root}/models")]
    } else {
        vec![format!("{root}/v1/models"), format!("{root}/models")]
    }
}

pub fn chat_endpoints(base: &str) -> Vec<String> {
    let clean = base.trim_end_matches('/');
    if clean.ends_with("/chat/completions") {
        return vec![clean.to_string()];
    }
    let root = strip_terminal_endpoint_suffix(clean);
    if root.ends_with("/v1") || root.contains("/v1/") {
        vec![format!("{root}/chat/completions")]
    } else {
        vec![format!("{root}/v1/chat/completions"), format!("{root}/chat/completions")]
    }
}

pub fn anthropic_messages_endpoints(base: &str) -> Vec<String> {
    let clean = base.trim_end_matches('/');
    if clean.ends_with("/messages") {
        return vec![clean.to_string()];
    }
    let root = strip_terminal_endpoint_suffix(clean);
    if root.ends_with("/v1") || root.contains("/v1/") {
        vec![format!("{root}/messages")]
    } else {
        vec![format!("{root}/v1/messages"), format!("{root}/messages")]
    }
}

fn push_unique_str(list: &mut Vec<String>, s: &str) {
    let t = s.trim();
    if !t.is_empty() && !list.iter().any(|x| x == t) {
        list.push(t.to_string());
    }
}

pub fn all_model_ids(body: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let Ok(val) = serde_json::from_str::<Value>(body) else {
        return ids;
    };
    if let Some(arr) = val["data"].as_array() {
        for entry in arr {
            if let Some(id) = entry["id"].as_str() {
                push_unique_str(&mut ids, id);
            }
        }
    }
    if let Some(arr) = val["models"].as_array() {
        for entry in arr {
            if let Some(id) = entry["id"].as_str().or_else(|| entry.as_str()) {
                push_unique_str(&mut ids, id);
            }
        }
    }
    ids
}

pub fn first_model_id(body: &str) -> Option<String> {
    all_model_ids(body).into_iter().next()
}

fn format_http_error(status: u16, body: &str) -> String {
    let msg = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().or_else(|| v["message"].as_str()).map(String::from))
        .unwrap_or_else(|| format!("HTTP {status}"));
    format!("HTTP {status}: {msg}")
}

fn is_not_found_like(err: &str) -> bool {
    err.starts_with("HTTP 404:") || err.starts_with("HTTP 405:") || err.starts_with("HTTP 501:")
}

fn pick_better_probe_error(primary: Option<String>, secondary: Option<String>) -> Option<String> {
    match (primary, secondary) {
        (Some(p), Some(s)) => {
            if is_not_found_like(&p) && !is_not_found_like(&s) {
                Some(s)
            } else {
                Some(p)
            }
        }
        (Some(p), None) => Some(p),
        (None, Some(s)) => Some(s),
        (None, None) => None,
    }
}

fn auth_styles_for_protocol(protocol: AiProtocol, api_key: Option<&str>, prefer_bearer: bool) -> Vec<HttpAuthStyle> {
    let has_key = api_key.map(str::trim).is_some_and(|s| !s.is_empty());
    match protocol {
        AiProtocol::Openai => vec![HttpAuthStyle::Bearer],
        AiProtocol::Anthropic => {
            if !has_key {
                vec![HttpAuthStyle::AnthropicApiKey]
            } else if prefer_bearer {
                vec![HttpAuthStyle::AnthropicBearer, HttpAuthStyle::AnthropicApiKey]
            } else {
                vec![HttpAuthStyle::AnthropicApiKey, HttpAuthStyle::AnthropicBearer]
            }
        }
    }
}

pub async fn fetch_ai_models_detailed(base_url: &str, api_key: Option<&str>) -> AiModelsResult {
    let started = Instant::now();
    let base = match validate_http_url(base_url, "ai.baseUrl") {
        Ok(u) => u,
        Err(e) => {
            return AiModelsResult {
                ok: false,
                base_url: base_url.to_string(),
                protocol: None,
                models: Vec::new(),
                latency_ms: started.elapsed().as_millis() as u64,
                error: Some(e.message),
            };
        }
    };
    let endpoints = models_endpoints(&base);
    let order = protocol_probe_order(&base, api_key, None);
    let mut proto_errs: Vec<String> = Vec::new();
    for proto in order {
        let mut last_err = String::new();
        for url in &endpoints {
            let styles = auth_styles_for_protocol(proto, api_key, false);
            let mut auth_failed = false;
            for (s_idx, style) in styles.iter().enumerate() {
                match http_json_request_with_auth("GET", url, api_key, *style, None, AI_MODELS_TIMEOUT).await {
                    Ok(resp) if (200..300).contains(&resp.status) => {
                        let ids = all_model_ids(&resp.body);
                        if !ids.is_empty() {
                            // Only record Anthropic as a strong hint here; Bearer /models is shared by many gateways.
                            let detected = if proto == AiProtocol::Anthropic { Some(AiProtocol::Anthropic) } else { None };
                            return AiModelsResult {
                                ok: true,
                                base_url: base,
                                protocol: detected,
                                models: ids,
                                latency_ms: started.elapsed().as_millis() as u64,
                                error: None,
                            };
                        }
                        last_err = "endpoint returned no models".into();
                        break;
                    }
                    Ok(resp) => {
                        last_err = format_http_error(resp.status, &resp.body);
                        if (resp.status == 401 || resp.status == 403) && s_idx + 1 < styles.len() {
                            continue;
                        }
                        if resp.status == 401 || resp.status == 403 {
                            auth_failed = true;
                        }
                        break;
                    }
                    Err(e) => {
                        last_err = e;
                        break;
                    }
                }
            }
            if auth_failed {
                break;
            }
        }
        if !last_err.is_empty() {
            proto_errs.push(last_err);
        }
    }
    let best_err = pick_better_probe_error(proto_errs.first().cloned(), proto_errs.get(1).cloned());
    AiModelsResult {
        ok: false,
        base_url: base,
        protocol: None,
        models: Vec::new(),
        latency_ms: started.elapsed().as_millis() as u64,
        error: Some(best_err.unwrap_or_else(|| "failed to fetch models from /models".into())),
    }
}

pub async fn fetch_ai_models(base_url: &str, api_key: Option<&str>) -> Vec<String> {
    fetch_ai_models_detailed(base_url, api_key).await.models
}

/// Tests an AI HTTP endpoint using `Service URL` + `API Key`, automatically detecting
/// whether the endpoint speaks OpenAI Chat Completions or Claude (Anthropic Messages) protocol.
/// Returns `(AiTestResult, anthropic_bearer_auth)`.
pub async fn test_ai_connection_detailed(
    base_url: &str,
    api_key: Option<&str>,
    model_hint: Option<&str>,
) -> (AiTestResult, bool) {
    let started = Instant::now();
    let base = match validate_http_url(base_url, "ai.baseUrl") {
        Ok(u) => u,
        Err(e) => {
            return (
                AiTestResult {
                    ok: false,
                    base_url: base_url.to_string(),
                    protocol: None,
                    discovered_model: None,
                    available_models: Vec::new(),
                    latency_ms: started.elapsed().as_millis() as u64,
                    error: Some(e.message),
                },
                false,
            );
        }
    };

    let models_res = fetch_ai_models_detailed(&base, api_key).await;
    let available_models = models_res.models;
    let chosen_model = model_hint
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| available_models.first().cloned());

    let Some(model) = chosen_model else {
        return (
            AiTestResult {
                ok: false,
                base_url: base,
                protocol: models_res.protocol,
                discovered_model: None,
                available_models,
                latency_ms: started.elapsed().as_millis() as u64,
                error: Some(
                    "could not discover a model from /models; check Service URL/API Key or enter a custom model ID"
                        .into(),
                ),
            },
            false,
        );
    };

    let order = protocol_probe_order(&base, api_key, models_res.protocol);
    let mut proto_errs: Vec<String> = Vec::new();

    for proto in order {
        let (endpoints, payload) = match proto {
            AiProtocol::Openai => (
                chat_endpoints(&base),
                json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "Reply with OK"}],
                    "max_tokens": 8,
                })
                .to_string(),
            ),
            AiProtocol::Anthropic => (
                anthropic_messages_endpoints(&base),
                json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "Reply with OK"}],
                    "max_tokens": 16,
                })
                .to_string(),
            ),
        };

        let mut last_err = String::new();
        for url in endpoints {
            let styles = auth_styles_for_protocol(proto, api_key, false);
            let mut auth_failed = false;
            for (s_idx, style) in styles.iter().enumerate() {
                match http_json_request_with_auth("POST", &url, api_key, *style, Some(&payload), AI_CHAT_TIMEOUT).await {
                    Ok(resp) if (200..300).contains(&resp.status) => {
                        let parsed: Value = serde_json::from_str(&resp.body).unwrap_or(Value::Null);
                        if !is_valid_protocol_response(&parsed, proto) {
                            last_err = format!("HTTP {}: unexpected response format for {proto} protocol", resp.status);
                            break;
                        }
                        let discovered =
                            parsed["model"].as_str().filter(|s| !s.is_empty()).unwrap_or(&model).to_string();
                        let mut models = available_models;
                        push_unique_str(&mut models, &discovered);
                        let used_bearer = *style == HttpAuthStyle::AnthropicBearer;
                        return (
                            AiTestResult {
                                ok: true,
                                base_url: base,
                                protocol: Some(proto),
                                discovered_model: Some(discovered),
                                available_models: models,
                                latency_ms: started.elapsed().as_millis() as u64,
                                error: None,
                            },
                            used_bearer,
                        );
                    }
                    Ok(resp) => {
                        last_err = format_http_error(resp.status, &resp.body);
                        if (resp.status == 401 || resp.status == 403) && s_idx + 1 < styles.len() {
                            continue;
                        }
                        if resp.status == 401 || resp.status == 403 {
                            auth_failed = true;
                        }
                        break;
                    }
                    Err(e) => {
                        last_err = e;
                        break;
                    }
                }
            }
            if auth_failed {
                break;
            }
        }
        if !last_err.is_empty() {
            proto_errs.push(last_err);
        }
    }

    let best_err = pick_better_probe_error(proto_errs.first().cloned(), proto_errs.get(1).cloned());
    (
        AiTestResult {
            ok: false,
            base_url: base,
            protocol: None,
            discovered_model: Some(model),
            available_models,
            latency_ms: started.elapsed().as_millis() as u64,
            error: Some(best_err.unwrap_or_else(|| "chat completion probe failed".into())),
        },
        false,
    )
}

/// Tests any OpenAI or Claude (Anthropic) HTTP endpoint using `Service URL` + `API Key`.
pub async fn test_ai_connection(base_url: &str, api_key: Option<&str>, model_hint: Option<&str>) -> AiTestResult {
    test_ai_connection_detailed(base_url, api_key, model_hint).await.0
}

fn http_client_direct() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .pool_idle_timeout(Duration::from_secs(90))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

fn http_client_proxied() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .pool_idle_timeout(Duration::from_secs(90))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

fn parse_retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim();
    let secs: u64 = raw.parse().ok()?;
    Some(secs.clamp(1, 120))
}

/// Calls an HTTP JSON endpoint via the shared pooled `reqwest` client (Keep-Alive / HTTP2)
/// using protocol-appropriate authentication and version headers.
/// Loopback URLs bypass environment proxies automatically.
pub async fn http_json_request_with_auth(
    method: &str,
    url: &str,
    api_key: Option<&str>,
    auth_style: HttpAuthStyle,
    body: Option<&str>,
    timeout: Duration,
) -> Result<HttpJsonResponse, String> {
    let parsed_method =
        reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| format!("invalid HTTP method {method}: {e}"))?;
    let client = if is_loopback_http_url(url) { http_client_direct() } else { http_client_proxied() };
    let mut req = client
        .request(parsed_method, url)
        .timeout(timeout.max(Duration::from_secs(1)))
        .header(reqwest::header::CONTENT_TYPE, "application/json");
    let clean_key = api_key.map(str::trim).filter(|s| !s.is_empty());
    match auth_style {
        HttpAuthStyle::Bearer => {
            if let Some(key) = clean_key {
                req = req.bearer_auth(key);
            }
        }
        HttpAuthStyle::AnthropicApiKey => {
            req = req.header("anthropic-version", ANTHROPIC_API_VERSION);
            if let Some(key) = clean_key {
                req = req.header("x-api-key", key);
            }
        }
        HttpAuthStyle::AnthropicBearer => {
            req = req.header("anthropic-version", ANTHROPIC_API_VERSION);
            if let Some(key) = clean_key {
                req = req.bearer_auth(key);
            }
        }
    }
    if let Some(payload) = body {
        req = req.body(payload.to_string());
    }
    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            format!("HTTP request timed out after {}s", timeout.as_secs().max(1))
        } else {
            format!("HTTP request failed: {e}")
        }
    })?;
    let status = resp.status().as_u16();
    let retry_after_secs = parse_retry_after_secs(resp.headers());
    let text = resp.text().await.map_err(|e| format!("read HTTP response body: {e}"))?;
    Ok(HttpJsonResponse { status, body: text, retry_after_secs })
}

/// Calls an HTTP JSON endpoint via the shared pooled `reqwest` client using OpenAI Bearer auth.
pub async fn http_json_request(
    method: &str,
    url: &str,
    api_key: Option<&str>,
    body: Option<&str>,
    timeout: Duration,
) -> Result<HttpJsonResponse, String> {
    http_json_request_with_auth(method, url, api_key, HttpAuthStyle::Bearer, body, timeout).await
}

pub async fn curl_json(
    method: &str,
    url: &str,
    api_key: Option<&str>,
    body: Option<&str>,
) -> Result<(u16, String), String> {
    curl_json_timeout(method, url, api_key, body, AI_CHAT_TIMEOUT).await
}

/// Compatibility wrapper around `http_json_request` returning `(status, body)`.
pub async fn curl_json_timeout(
    method: &str,
    url: &str,
    api_key: Option<&str>,
    body: Option<&str>,
    timeout: Duration,
) -> Result<(u16, String), String> {
    let resp = http_json_request(method, url, api_key, body, timeout).await?;
    Ok((resp.status, resp.body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_major_provider_cache_token_fields() {
        let openai = json!({"usage":{"prompt_tokens":1500,"completion_tokens":40,"prompt_tokens_details":{"cached_tokens":1200}}});
        assert_eq!(parse_usage_tokens(&openai), (1500, 40, 1200));

        let deepseek = json!({"usage":{"prompt_tokens":1500,"completion_tokens":40,"prompt_cache_hit_tokens":1100}});
        assert_eq!(parse_usage_tokens(&deepseek), (1500, 40, 1100));

        let anthropic = json!({"usage":{"input_tokens":1600,"output_tokens":32,"cache_read_input_tokens":1280}});
        assert_eq!(parse_usage_tokens(&anthropic), (1600, 32, 1280));

        // Native Anthropic separated cache accounting (input_tokens excludes cache_creation + cache_read).
        let anthropic_native_create = json!({
            "usage": {
                "input_tokens": 30,
                "cache_creation_input_tokens": 1500,
                "cache_read_input_tokens": 0,
                "output_tokens": 45
            }
        });
        assert_eq!(parse_usage_tokens(&anthropic_native_create), (1530, 45, 0));

        let anthropic_native_hit = json!({
            "usage": {
                "input_tokens": 30,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 1500,
                "output_tokens": 45
            }
        });
        assert_eq!(parse_usage_tokens(&anthropic_native_hit), (1530, 45, 1500));
    }

    #[test]
    fn infers_protocols_endpoints_and_builds_anthropic_payloads() {
        assert_eq!(infer_preferred_protocol("https://api.anthropic.com", None), AiProtocol::Anthropic);
        assert_eq!(infer_preferred_protocol("https://api.deepseek.com/anthropic", None), AiProtocol::Anthropic);
        assert_eq!(infer_preferred_protocol("https://relay.example.com/claude/v1", None), AiProtocol::Anthropic);
        assert_eq!(infer_preferred_protocol("https://relay.example.com/v1/messages", None), AiProtocol::Anthropic);
        assert_eq!(
            infer_preferred_protocol("https://relay.example.com/v1", Some("sk-ant-api03-secret")),
            AiProtocol::Anthropic
        );
        assert_eq!(infer_preferred_protocol("https://api.deepseek.com/v1", Some("sk-123")), AiProtocol::Openai);
        assert_eq!(
            infer_preferred_protocol("https://relay.example.com/v1/chat/completions", Some("sk-ant-123")),
            AiProtocol::Openai
        );

        assert_eq!(
            anthropic_messages_endpoints("https://api.anthropic.com"),
            vec!["https://api.anthropic.com/v1/messages", "https://api.anthropic.com/messages"]
        );
        assert_eq!(
            anthropic_messages_endpoints("https://api.anthropic.com/v1"),
            vec!["https://api.anthropic.com/v1/messages"]
        );
        assert_eq!(
            anthropic_messages_endpoints("https://api.anthropic.com/v1/messages"),
            vec!["https://api.anthropic.com/v1/messages"]
        );
        assert_eq!(
            models_endpoints("https://api.anthropic.com/v1/messages"),
            vec!["https://api.anthropic.com/v1/models"]
        );

        let caps = EndpointCapabilities::default_for_url("https://relay.example.com/v1");
        let msgs = vec![
            ChatMessage::system("sys rule"),
            ChatMessage::user("seed"),
            ChatMessage::assistant("{\"ack\":true}"),
            ChatMessage::user("actual turn"),
        ];
        let cands = build_anthropic_payload_candidates("claude-sonnet-4-6", &msgs, 256, 0.1, Some("h2:cp:v1"), caps);
        assert_eq!(cands.len(), 3);
        let first: Value = serde_json::from_str(&cands[0].body).unwrap();
        assert_eq!(first["model"], "claude-sonnet-4-6");
        assert_eq!(first["max_tokens"], 256);
        assert_eq!(first["system"][0]["text"], "sys rule");
        assert_eq!(first["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(first["messages"].as_array().unwrap().len(), 3);
        assert_eq!(first["messages"][1]["role"], "assistant");
        assert_eq!(first["messages"][1]["content"][0]["cache_control"]["type"], "ephemeral");
        assert!(first.get("reasoning_effort").is_none());
        assert!(first.get("response_format").is_none());

        // Fallback strips cache_control and uses plain string system/messages.
        let second: Value = serde_json::from_str(&cands[1].body).unwrap();
        assert_eq!(second["system"], "sys rule");
        assert_eq!(second["messages"][1]["content"], "{\"ack\":true}");
        assert!(second.get("temperature").is_some());

        // Minimal fallback omits temperature as well.
        let third: Value = serde_json::from_str(&cands[2].body).unwrap();
        assert!(third.get("temperature").is_none());

        // Anthropic response parsing ignores thinking blocks and maps stop_reason=max_tokens -> length.
        let resp = json!({
            "type": "message",
            "content": [
                {"type": "thinking", "thinking": "internal"},
                {"type": "text", "text": "{\"ok\":"},
                {"type": "text", "text": "true}"}
            ],
            "stop_reason": "max_tokens"
        });
        assert!(is_valid_protocol_response(&resp, AiProtocol::Anthropic));
        assert!(!is_valid_protocol_response(&resp, AiProtocol::Openai));
        assert_eq!(extract_response_content(&resp, AiProtocol::Anthropic), ("{\"ok\":true}".into(), "length".into()));
    }

    #[test]
    fn capability_memory_strips_rejected_prompt_cache_key_and_json_format() {
        let mut caps = EndpointCapabilities::default_for_url("https://api.deepseek.com");
        assert!(caps.supports_prompt_cache_key);
        assert!(caps.supports_json_object);
        assert!(!caps.supports_num_ctx);
        let cands = build_chat_payload_candidates(
            "deepseek-chat",
            &[ChatMessage::system("sys"), ChatMessage::user("hi")],
            64,
            "none",
            Some(32768),
            0.0,
            Some("h2:cp:v1"),
            true,
            caps,
        );
        assert!(cands[0].used_cache_key);
        assert!(cands[0].used_json_object);
        refine_capabilities_from_error(&mut caps, &cands[0], "unknown field prompt_cache_key");
        assert!(!caps.supports_prompt_cache_key);
        let cands2 = build_chat_payload_candidates(
            "deepseek-chat",
            &[ChatMessage::system("sys"), ChatMessage::user("hi")],
            64,
            "none",
            Some(32768),
            0.0,
            Some("h2:cp:v1"),
            true,
            caps,
        );
        assert!(!cands2[0].used_cache_key);
        assert!(cands2[0].used_json_object);
    }

    #[test]
    fn kv_cache_auto_degrades_after_five_uncached_large_prefix_calls() {
        let mut caps = EndpointCapabilities::default_for_url("https://proxy.example.com/v1");
        assert!(caps.prompt_cache_effective);
        for _ in 0..4 {
            caps.record_shared_prefix_usage(1800, 0);
            assert!(caps.prompt_cache_effective);
        }
        caps.record_shared_prefix_usage(1800, 0);
        assert!(!caps.prompt_cache_effective);
        // Any subsequent cache hit restores effectiveness immediately.
        caps.record_shared_prefix_usage(1800, 1200);
        assert!(caps.prompt_cache_effective);
        assert_eq!(caps.prefix_probes_without_cache, 0);
    }
}
