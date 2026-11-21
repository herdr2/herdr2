//! OpenAI-compatible HTTP transport, endpoint capability memory, payload fallback,
//! model discovery, and multi-vendor cache-token accounting.

use super::config::validate_http_url;
use super::session::ChatMessage;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

pub const AI_MODELS_TIMEOUT: Duration = Duration::from_secs(4);
pub const AI_CHAT_TIMEOUT: Duration = Duration::from_secs(12);

const STRICT_CLOUD_HOSTS: [&str; 6] = [
    "api.openai.com",
    "api.deepseek.com",
    "open.bigmodel.cn",
    "dashscope.aliyuncs.com",
    "api.anthropic.com",
    "generativelanguage.googleapis.com",
];

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

/// Remembered optional-field compatibility per `(base_url, model)` so repeated requests
/// skip 400/422 probe roundtrips after the first call adapts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointCapabilities {
    pub supports_prompt_cache_key: bool,
    pub supports_reasoning_effort: bool,
    pub supports_effort_none: bool,
    pub supports_num_ctx: bool,
    pub supports_temperature: bool,
}

impl EndpointCapabilities {
    pub fn default_for_url(base_url: &str) -> Self {
        let strict_cloud = is_known_strict_cloud_url(base_url);
        Self {
            supports_prompt_cache_key: true,
            supports_reasoning_effort: true,
            supports_effort_none: true,
            supports_num_ctx: !strict_cloud,
            supports_temperature: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PayloadVariant {
    pub body: String,
    pub used_cache_key: bool,
    pub used_effort: Option<String>,
    pub used_num_ctx: bool,
    pub used_temperature: bool,
}

pub fn is_known_strict_cloud_url(base: &str) -> bool {
    let rest = base
        .trim()
        .strip_prefix("https://")
        .or_else(|| base.trim().strip_prefix("http://"))
        .unwrap_or("");
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    STRICT_CLOUD_HOSTS.iter().any(|h| host.eq_ignore_ascii_case(h))
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

#[allow(clippy::too_many_arguments)]
pub fn build_chat_payload_candidates(
    model: &str,
    messages: &[ChatMessage],
    max_tokens: u32,
    effort: &str,
    ctx_tokens: Option<u32>,
    temperature: f32,
    prompt_cache_key: Option<&str>,
    caps: EndpointCapabilities,
) -> Vec<PayloadVariant> {
    let msgs_val = serde_json::to_value(messages).unwrap_or_else(|_| json!([]));
    let want_effort = if !caps.supports_reasoning_effort || effort == "auto" || effort.is_empty() {
        None
    } else if effort == "none" && !caps.supports_effort_none {
        Some("low".to_string())
    } else {
        Some(effort.to_string())
    };
    let want_ctx = ctx_tokens.filter(|_| caps.supports_num_ctx);
    let want_cache_key = prompt_cache_key.filter(|s| caps.supports_prompt_cache_key && !s.is_empty());
    let want_temp = caps.supports_temperature;

    let make_variant = |eff: Option<&str>,
                        ctx: Option<u32>,
                        cache_key: Option<&str>,
                        with_temp: bool|
     -> PayloadVariant {
        let mut obj = serde_json::Map::new();
        obj.insert("model".into(), json!(model));
        obj.insert("messages".into(), msgs_val.clone());
        obj.insert("max_tokens".into(), json!(max_tokens));
        if with_temp {
            obj.insert("temperature".into(), json!(temperature));
        }
        if let Some(e) = eff {
            obj.insert("reasoning_effort".into(), json!(e));
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
            used_num_ctx: ctx.is_some(),
            used_temperature: with_temp,
        }
    };

    let mut out = Vec::new();
    let push_unique = |list: &mut Vec<PayloadVariant>, item: PayloadVariant| {
        if !list.iter().any(|x| x.body == item.body) {
            list.push(item);
        }
    };

    // 1. Full remembered-compatible payload.
    push_unique(&mut out, make_variant(want_effort.as_deref(), want_ctx, want_cache_key, want_temp));
    // 2. If `effort = "none"` was requested, fall back to `"low"` before stripping effort.
    if want_effort.as_deref() == Some("none") {
        push_unique(&mut out, make_variant(Some("low"), want_ctx, want_cache_key, want_temp));
    }
    // 3. Strip `prompt_cache_key` if strict proxy rejects it.
    if want_cache_key.is_some() {
        let eff_next = if want_effort.as_deref() == Some("none") { Some("low") } else { want_effort.as_deref() };
        push_unique(&mut out, make_variant(eff_next, want_ctx, None, want_temp));
    }
    // 4. Strip `options.num_ctx`.
    if want_ctx.is_some() {
        let eff_next = if want_effort.as_deref() == Some("none") { Some("low") } else { want_effort.as_deref() };
        push_unique(&mut out, make_variant(eff_next, None, None, want_temp));
    }
    // 5. Bare minimal OpenAI payload.
    push_unique(&mut out, make_variant(None, None, None, false));
    out
}

pub fn should_retry_without_optional_fields(status: u16, body: &str) -> bool {
    if status != 400 && status != 422 {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    [
        "prompt_cache_key",
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
pub fn refine_capabilities_from_error(
    caps: &mut EndpointCapabilities,
    attempted: &PayloadVariant,
    body: &str,
) {
    let lower = body.to_ascii_lowercase();
    let mut matched_specific = false;
    if attempted.used_cache_key && lower.contains("prompt_cache_key") {
        caps.supports_prompt_cache_key = false;
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
        if attempted.used_cache_key {
            caps.supports_prompt_cache_key = false;
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
    if !winning.used_num_ctx {
        caps.supports_num_ctx = false;
    }
    if winning.used_effort.is_none() {
        // Only disable reasoning_effort if we didn't intentionally omit it — handled via error refine.
    } else if winning.used_effort.as_deref() != Some("none") {
        // Keep current supports_effort_none value.
    }
    if !winning.used_temperature {
        caps.supports_temperature = false;
    }
}

/// Extracts `(input_tokens, output_tokens, cached_input_tokens)` across OpenAI, DeepSeek, and Anthropic proxy schemas.
pub fn parse_usage_tokens(parsed: &Value) -> (u64, u64, u64) {
    let u = &parsed["usage"];
    let input = u["prompt_tokens"]
        .as_u64()
        .or_else(|| u["input_tokens"].as_u64())
        .unwrap_or(0);
    let output = u["completion_tokens"]
        .as_u64()
        .or_else(|| u["output_tokens"].as_u64())
        .unwrap_or(0);
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
pub async fn test_ai_connection(
    base_url: &str,
    api_key: Option<&str>,
    model_hint: Option<&str>,
) -> AiTestResult {
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
                let discovered = parsed["model"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&model)
                    .to_string();
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
                    .and_then(|v| {
                        v["error"]["message"]
                            .as_str()
                            .or_else(|| v["message"].as_str())
                            .map(String::from)
                    })
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
        error: Some(if last_err.is_empty() {
            "chat completion probe failed".into()
        } else {
            last_err
        }),
    }
}

fn bg_tokio_command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd
}

pub async fn curl_json(
    method: &str,
    url: &str,
    api_key: Option<&str>,
    body: Option<&str>,
) -> Result<(u16, String), String> {
    curl_json_timeout(method, url, api_key, body, AI_CHAT_TIMEOUT).await
}

/// Calls an HTTP JSON endpoint via `curl --config -` so bearer tokens stay off process argv.
pub async fn curl_json_timeout(
    method: &str,
    url: &str,
    api_key: Option<&str>,
    body: Option<&str>,
    timeout: Duration,
) -> Result<(u16, String), String> {
    let mut cfg = format!("url = \"{}\"\nrequest = \"{}\"\n", escape_curl(url), escape_curl(method));
    cfg.push_str("header = \"Content-Type: application/json\"\n");
    if let Some(key) = api_key.map(str::trim).filter(|s| !s.is_empty()) {
        cfg.push_str(&format!("header = \"Authorization: Bearer {}\"\n", escape_curl(key)));
    }
    if let Some(payload) = body {
        cfg.push_str(&format!("data-raw = \"{}\"\n", escape_curl(payload)));
    }

    let timeout_secs = timeout.as_secs().max(1).to_string();
    let mut child = bg_tokio_command("curl")
        .args(["-sS", "--max-time", &timeout_secs, "-w", "\n%{http_code}", "--config", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn curl: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(cfg.as_bytes())
            .await
            .map_err(|e| format!("write curl stdin: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| format!("curl failed: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("curl exited {}", out.status)
        } else {
            err
        });
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let (resp_body, code_str) = raw.rsplit_once('\n').unwrap_or(("", raw.trim()));
    let status: u16 = code_str.trim().parse().unwrap_or(0);
    Ok((status, resp_body.to_string()))
}

fn escape_curl(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
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
    fn capability_memory_strips_rejected_prompt_cache_key() {
        let mut caps = EndpointCapabilities::default_for_url("https://api.deepseek.com");
        assert!(caps.supports_prompt_cache_key);
        assert!(!caps.supports_num_ctx);
        let cands = build_chat_payload_candidates(
            "deepseek-chat",
            &[ChatMessage::system("sys"), ChatMessage::user("hi")],
            64,
            "none",
            Some(32768),
            0.0,
            Some("h2:cp:v1"),
            caps,
        );
        assert!(cands[0].used_cache_key);
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
            caps,
        );
        assert!(!cands2[0].used_cache_key);
    }
}
