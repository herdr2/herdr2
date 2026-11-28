//! Semantic fallback arbitration for interactive PTY CLI agents that do not emit OSC 7501.
//!
//! Uses volatile-token normalization + local LRU caching (0 tokens on repeated screens) and
//! shares the canonical immutable `HERDR2_CONTROL_PLANE_SYSTEM` KV-cache prefix in stateless
//! `AiTaskTier::Nano` mode.

use super::client::extract_json_object;
use super::compact::{fingerprint_hex, normalize_for_fingerprint, strip_and_dedup_lines};
use super::prompts::format_op_user_message;
use super::{AiCompleteRequest, AiConversationMode, AiRuntime, AiTaskTier};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

const MIN_CONFIDENCE_TO_OVERRIDE: f64 = 0.70;
const LOCAL_CACHE_TTL: Duration = Duration::from_secs(600);
const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClassifyTerminalStateParams {
    /// Target machine (`--on`). Omit for local core (recommended for local VT arbitration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    /// Detected interactive CLI program key (e.g. `claude`, `codex`, `agy`, `opencode`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// Raw terminal window title (preserving spinners / status markers).
    #[serde(default)]
    pub raw_title: String,
    /// Bottom lines of the live terminal viewport (at most 15 lines kept).
    #[serde(default)]
    pub tail_lines: Vec<String>,
    /// Optional latest user prompt submitted to this session for task-completion context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_user_prompt: Option<String>,
    /// Preferred UI language (`zh-CN` / `en`) for concise `summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClassifyTerminalStateResult {
    /// Whether AI arbitration was enabled & attempted (false when disabled/unconfigured/circuit open).
    pub used_ai: bool,
    /// Whether the verdict met confidence threshold and should override deterministic `"idle"`.
    pub accepted: bool,
    /// Whether result came from the 0-token local normalized cache.
    #[serde(default)]
    pub from_cache: bool,
    /// Classified state (`working`, `blocked`, `done`, `error`, or `idle`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Sub-kind when `blocked` (`permission`, `question`, `auth`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Calibrated model confidence (0.0..=1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Concise human-readable reason (<= 48 chars) for sidebar tooltip/subtitle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Concise semantic task title (<= 24 chars) for unnamed tab / sidebar subtitle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_title: Option<String>,
    /// One-sentence real-time progress summary (<= 96 chars) for zero-click hover tooltip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub micro_summary: Option<String>,
}

fn clean_bounded(raw: Option<&str>, max_chars: usize) -> Option<String> {
    let s = raw?.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("null") || s.eq_ignore_ascii_case("none") {
        return None;
    }
    let clean: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if clean.is_empty() {
        return None;
    }
    Some(clean.chars().take(max_chars).collect())
}

fn clean_summary(raw: Option<&str>) -> Option<String> {
    clean_bounded(raw, 48)
}

pub async fn classify_terminal_state(
    ai: &AiRuntime,
    params: ClassifyTerminalStateParams,
) -> ClassifyTerminalStateResult {
    if !ai.status_arbitration_enabled() {
        return ClassifyTerminalStateResult::default();
    }

    let raw_joined = params
        .tail_lines
        .iter()
        .rev()
        .take(20)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let compact_tail = strip_and_dedup_lines(&raw_joined, 20, 200, 2200);
    if compact_tail.trim().is_empty() && params.raw_title.trim().is_empty() {
        return ClassifyTerminalStateResult::default();
    }

    let lang =
        if params.language.as_deref().is_some_and(|l| l.to_ascii_lowercase().starts_with("zh")) { "zh" } else { "en" };
    let prog = params.program.as_deref().unwrap_or("").trim().to_ascii_lowercase();
    let compact_prompt = params
        .last_user_prompt
        .as_deref()
        .map(|s| strip_and_dedup_lines(s, 4, 160, 280))
        .filter(|s| !s.trim().is_empty());
    let norm_title = normalize_for_fingerprint(&params.raw_title);
    let norm_tail = normalize_for_fingerprint(&compact_tail);
    let norm_prompt = compact_prompt.as_deref().map(normalize_for_fingerprint).unwrap_or_default();
    let cache_key = format!("pty_status:{lang}:{prog}:{}", fingerprint_hex(&[&norm_title, &norm_prompt, &norm_tail]));

    let mut payload = json!({
        "program": prog,
        "raw_title": strip_and_dedup_lines(&params.raw_title, 1, 120, 120),
        "tail": compact_tail,
    });
    if let Some(ref p) = compact_prompt {
        payload["last_user_prompt"] = json!(p);
    }
    let user_prompt = format_op_user_message("pty_status", lang, &payload);

    let Some(resp) = ai
        .complete_bg(AiCompleteRequest {
            op: "pty_status".into(),
            tier: AiTaskTier::Nano,
            mode: AiConversationMode::StatelessSharedPrefix,
            user_prompt,
            max_tokens: 112,
            timeout: CLASSIFY_TIMEOUT,
            cache_key: Some(cache_key),
            cache_ttl: Some(LOCAL_CACHE_TTL),
            prompt_cache_bucket: Some("pty".into()),
            ..Default::default()
        })
        .await
    else {
        return ClassifyTerminalStateResult { used_ai: false, ..Default::default() };
    };

    let verdict = parse_verdict(&resp.content, resp.from_local_cache);
    if !resp.from_local_cache {
        if verdict.state.is_none() && verdict.confidence.is_none() {
            ai.record_parse_error("pty_status");
        } else if !verdict.accepted {
            ai.record_low_conf_reject("pty_status");
        }
    }
    verdict
}

/// Classifies the final assistant response of a completed ACP turn using the shared Core AI runtime
/// (`op = "pty_status"`, `AiTaskTier::Nano`, KV-cache shared prefix) to accurately arbitrate
/// whether the agent finished (`done`/`idle`) or stopped awaiting user input/choice (`blocked`).
pub async fn classify_acp_turn_state(
    ai: &AiRuntime,
    program: &str,
    last_user_prompt: Option<&str>,
    assistant_text: &str,
    language: Option<&str>,
) -> ClassifyTerminalStateResult {
    if !ai.status_arbitration_enabled() {
        return ClassifyTerminalStateResult::default();
    }

    let stripped = crate::status::strip_fenced_code_blocks(assistant_text);
    let prose_source = if stripped.trim().is_empty() { assistant_text } else { stripped.as_str() };
    let compact_tail = strip_and_dedup_lines(prose_source, 24, 220, 2200);
    if compact_tail.trim().is_empty() {
        return ClassifyTerminalStateResult::default();
    }

    let lang = if language.is_some_and(|l| l.to_ascii_lowercase().starts_with("zh")) { "zh" } else { "en" };
    let prog = program.trim().to_ascii_lowercase();
    let compact_prompt =
        last_user_prompt.map(|s| strip_and_dedup_lines(s, 4, 160, 280)).filter(|s| !s.trim().is_empty());
    let norm_tail = normalize_for_fingerprint(&compact_tail);
    let norm_prompt = compact_prompt.as_deref().map(normalize_for_fingerprint).unwrap_or_default();
    let cache_key = format!("acp_turn_status:{lang}:{prog}:{}", fingerprint_hex(&[&norm_prompt, &norm_tail]));

    let mut payload = json!({
        "program": prog,
        "raw_title": "ACP turn finished: return blocked (kind=question) ONLY if the agent paused asking the user a question/choice to proceed; return done if the agent completed the task without asking a blocking question",
        "tail": compact_tail,
    });
    if let Some(ref p) = compact_prompt {
        payload["last_user_prompt"] = json!(p);
    }
    let user_prompt = format_op_user_message("pty_status", lang, &payload);

    let Some(resp) = ai
        .complete_bg(AiCompleteRequest {
            op: "pty_status".into(),
            tier: AiTaskTier::Nano,
            mode: AiConversationMode::StatelessSharedPrefix,
            user_prompt,
            max_tokens: 112,
            timeout: CLASSIFY_TIMEOUT,
            cache_key: Some(cache_key),
            cache_ttl: Some(LOCAL_CACHE_TTL),
            prompt_cache_bucket: Some("pty".into()),
            ..Default::default()
        })
        .await
    else {
        return ClassifyTerminalStateResult { used_ai: false, ..Default::default() };
    };

    let verdict = parse_verdict(&resp.content, resp.from_local_cache);
    if !resp.from_local_cache {
        if verdict.state.is_none() && verdict.confidence.is_none() {
            ai.record_parse_error("pty_status");
        } else if !verdict.accepted {
            ai.record_low_conf_reject("pty_status");
        }
    }
    verdict
}

pub(crate) fn parse_verdict(raw: &str, from_cache: bool) -> ClassifyTerminalStateResult {
    let Some(v) = extract_json_object(raw) else {
        return ClassifyTerminalStateResult { used_ai: true, accepted: false, from_cache, ..Default::default() };
    };
    let state = v["state"]
        .as_str()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| matches!(s.as_str(), "working" | "blocked" | "done" | "error" | "idle"));
    let confidence = v["confidence"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
    let kind = v["kind"]
        .as_str()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| matches!(s.as_str(), "permission" | "question" | "auth"));
    let summary = clean_summary(v["summary"].as_str());
    let semantic_title = clean_bounded(v["semantic_title"].as_str(), 28);
    let micro_summary = clean_bounded(v["micro_summary"].as_str(), 96);
    let accepted = state.is_some() && confidence >= MIN_CONFIDENCE_TO_OVERRIDE;

    ClassifyTerminalStateResult {
        used_ai: true,
        accepted,
        from_cache,
        state,
        kind,
        confidence: Some(confidence),
        summary,
        semantic_title,
        micro_summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_verdicts_and_rejects_low_confidence() {
        let ok = parse_verdict(
            r#"{"state":"blocked","kind":"question","confidence":0.92,"summary":"询问认证方案选型"}"#,
            false,
        );
        assert!(ok.used_ai && ok.accepted);
        assert_eq!(ok.state.as_deref(), Some("blocked"));
        assert_eq!(ok.kind.as_deref(), Some("question"));
        assert_eq!(ok.summary.as_deref(), Some("询问认证方案选型"));

        let low = parse_verdict(r#"{"state":"blocked","kind":"question","confidence":0.55,"summary":"maybe"}"#, false);
        assert!(low.used_ai && !low.accepted);

        let bad_state = parse_verdict(r#"{"state":"unknown","confidence":0.99}"#, false);
        assert!(!bad_state.accepted);
    }
}
