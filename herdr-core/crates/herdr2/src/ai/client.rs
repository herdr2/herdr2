//! OpenAI-compatible HTTP transport (via pooled `reqwest` client), endpoint capability memory,
//! payload fallback, model discovery, and multi-vendor cache-token accounting.

use super::config::{is_loopback_http_url, validate_http_url};
use super::session::ChatMessage;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const AI_MODELS_TIMEOUT: Duration = Duration::from_secs(4);
pub const AI_CHAT_TIMEOUT: Duration = Duration::from_secs(12);
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
    pub discovered_model: Option<String>,
    #[serde(default)]
    pub available_models: Vec<String>,
    pub latency_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Remembered optional-field compatibility & KV-cache effectiveness per `(base_url, model)`
/// so repeated requests skip 400/422 probe roundtrips and avoid paying full price for
/// uncached large shared prefixes on gateways without prompt caching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointCapabilities {
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
            supports_prompt_cache_key: !anthropic_like,
            supports_reasoning_effort: true,
            supports_effort_none: !enable_thinking_host,
            supports_enable_thinking: enable_thinking_host,
            supports_num_ctx: !strict_cloud,
            supports_temperature: true,
            supports_json_object: !anthropic_like,
            supports_cache_control: anthropic_like,
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

pub fn is_anthropic_compat_url(base: &str) -> bool {
    let host = extract_host(base);
    host.eq_ignore_ascii_case("api.anthropic.com") || base.to_ascii_lowercase().contains("/anthropic")
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
    let lower = body.to_ascii_lowercase();
    let mut matched_specific = false;
    if attempted.used_cache_key && lower.contains("prompt_cache_key") {
        caps.supports_prompt_cache_key = false;
        matched_specific = true;
    }
    if attempted.used_cache_control && (lower.contains("cache_control") || lower.contains("content")) {
        caps.supports_cache_control = false;
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
            caps.supports_cache_control = false;
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
    if !winning.used_temperature {
        caps.supports_temperature = false;
    }
}

/// Extracts `(input_tokens, output_tokens, cached_input_tokens)` across OpenAI, DeepSeek, and Anthropic proxy schemas.
pub fn parse_usage_tokens(parsed: &Value) -> (u64, u64, u64) {
    let u = &parsed["usage"];
    let input = u["prompt_tokens"].as_u64().or_else(|| u["input_tokens"].as_u64()).unwrap_or(0);
    let output = u["completion_tokens"].as_u64().or_else(|| u["output_tokens"].as_u64()).unwrap_or(0);
    let cached = u["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .or_else(|| u["prompt_cache_hit_tokens"].as_u64())
        .or_else(|| u["cache_read_input_tokens"].as_u64())
        .or_else(|| u["input_tokens_details"]["cached_tokens"].as_u64())
        .unwrap_or(0);
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

pub fn models_endpoints(base: &str) -> Vec<String> {
    if base.ends_with("/v1") || base.contains("/v1/") {
        vec![format!("{base}/models")]
    } else {
        vec![format!("{base}/v1/models"), format!("{base}/models")]
    }
}

pub fn chat_endpoints(base: &str) -> Vec<String> {
    if base.ends_with("/v1") || base.contains("/v1/") {
        vec![format!("{base}/chat/completions")]
    } else {
        vec![format!("{base}/v1/chat/completions"), format!("{base}/chat/completions")]
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

pub async fn fetch_ai_models(base_url: &str, api_key: Option<&str>) -> Vec<String> {
    let Ok(base) = validate_http_url(base_url, "ai.baseUrl") else {
        return Vec::new();
    };
    for url in models_endpoints(&base) {
        if let Ok((st, body)) = curl_json_timeout("GET", &url, api_key, None, AI_MODELS_TIMEOUT).await
            && (200..300).contains(&st)
        {
            let ids = all_model_ids(&body);
            if !ids.is_empty() {
                return ids;
            }
        }
    }
    Vec::new()
}

/// Tests any OpenAI-compatible HTTP endpoint using `Service URL` + `API Key`.
pub async fn test_ai_connection(base_url: &str, api_key: Option<&str>, model_hint: Option<&str>) -> AiTestResult {
    let started = Instant::now();
    let base = match validate_http_url(base_url, "ai.baseUrl") {
        Ok(u) => u,
        Err(e) => {
            return AiTestResult {
                ok: false,
                base_url: base_url.to_string(),
                discovered_model: None,
                available_models: Vec::new(),
                latency_ms: started.elapsed().as_millis() as u64,
                error: Some(e.message),
            };
        }
    };

    let available_models = fetch_ai_models(&base, api_key).await;
    let chosen_model = model_hint
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| available_models.first().cloned());

    let Some(model) = chosen_model else {
        return AiTestResult {
            ok: false,
            base_url: base,
            discovered_model: None,
            available_models,
            latency_ms: started.elapsed().as_millis() as u64,
            error: Some("could not discover a model from /models; check Service URL or API Key".into()),
        };
    };

    let payload = json!({
        "model": model,
        "messages": [{"role": "user", "content": "Reply with OK"}],
        "max_tokens": 8,
    })
    .to_string();

    let mut last_err = String::new();
    for chat_url in chat_endpoints(&base) {
        match curl_json_timeout("POST", &chat_url, api_key, Some(&payload), AI_CHAT_TIMEOUT).await {
            Ok((status, body)) if (200..300).contains(&status) => {
                let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let discovered = parsed["model"].as_str().filter(|s| !s.is_empty()).unwrap_or(&model).to_string();
                let mut models = available_models;
                push_unique_str(&mut models, &discovered);
                return AiTestResult {
                    ok: true,
                    base_url: base,
                    discovered_model: Some(discovered),
                    available_models: models,
                    latency_ms: started.elapsed().as_millis() as u64,
                    error: None,
                };
            }
            Ok((status, body)) => {
                let msg = serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|v| v["error"]["message"].as_str().or_else(|| v["message"].as_str()).map(String::from))
                    .unwrap_or_else(|| format!("HTTP {status}"));
                last_err = format!("HTTP {status}: {msg}");
                if status == 401 || status == 403 {
                    break;
                }
            }
            Err(e) => {
                last_err = e;
            }
        }
    }

    AiTestResult {
        ok: false,
        base_url: base,
        discovered_model: Some(model),
        available_models,
        latency_ms: started.elapsed().as_millis() as u64,
        error: Some(if last_err.is_empty() { "chat completion probe failed".into() } else { last_err }),
    }
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

/// Calls an HTTP JSON endpoint via the shared pooled `reqwest` client (Keep-Alive / HTTP2).
/// Loopback URLs bypass environment proxies automatically.
pub async fn http_json_request(
    method: &str,
    url: &str,
    api_key: Option<&str>,
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
    if let Some(key) = api_key.map(str::trim).filter(|s| !s.is_empty()) {
        req = req.bearer_auth(key);
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
