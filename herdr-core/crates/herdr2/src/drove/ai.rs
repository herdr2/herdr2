//! Lightweight optional AI assistant hooks (`Service URL` + `0600 ai.key` + circuit breaker).

use super::config::{read_ai_key, validate_http_url};
use super::probe::{chat_endpoints, curl_json, first_model_id, models_endpoints};
use super::store::{NewDaemonEvent, PostMessageInput};
use super::types::*;
use crate::runtime::Core;
use serde_json::{Value, json};
use std::sync::Arc;

const COOLDOWN_SECS: u64 = 300;
const CIRCUIT_THRESHOLD: u32 = 3;

/// Calls configured OpenAI-compatible `/chat/completions`, honoring circuit breaker state.
pub async fn chat_completion(
    core: &Arc<Core>,
    run_id: &str,
    kind: &str,
    system_prompt: &str,
    user_prompt: &str,
    max_tokens: u32,
) -> Option<String> {
    let cfg = core.drove.config_snapshot();
    if !cfg.ai.enabled || cfg.ai.base_url.trim().is_empty() {
        return None;
    }
    let (key, _) = read_ai_key(&core.drove.paths);
    if key.is_none() && !super::config::is_loopback_http_url(&cfg.ai.base_url) {
        return None;
    }
    let base = validate_http_url(&cfg.ai.base_url, "ai.baseUrl").ok()?;

    if core.drove.is_ai_circuit_open() {
        return None;
    }

    let mut model = cfg.ai.model.trim().to_string();
    if model.is_empty() {
        for url in models_endpoints(&base) {
            if let Ok((st, body)) = curl_json("GET", &url, key.as_deref(), None).await
                && (200..300).contains(&st)
                && let Some(m) = first_model_id(&body)
            {
                model = m;
                break;
            }
        }
    }
    if model.is_empty() {
        core.drove.record_ai_failure("no model configured or discovered");
        return None;
    }

    let payload = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_prompt }
        ],
        "max_tokens": max_tokens,
        "temperature": 0.2,
    })
    .to_string();

    for url in chat_endpoints(&base) {
        match curl_json("POST", &url, key.as_deref(), Some(&payload)).await {
            Ok((status, body)) if (200..300).contains(&status) => {
                let parsed: Value = serde_json::from_str(&body).ok()?;
                let content = parsed["choices"][0]["message"]["content"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())?
                    .to_string();
                let in_tok = parsed["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
                let out_tok = parsed["usage"]["completion_tokens"].as_u64().unwrap_or(0);
                let cache_tok = parsed["usage"]["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);

                core.drove.record_ai_success();
                let _ = core.drove.store.update_run(run_id, |r| {
                    let mut u = r.ai_usage.clone().unwrap_or_default();
                    u.calls += 1;
                    u.input_tokens += in_tok;
                    u.output_tokens += out_tok;
                    u.cache_hit_tokens += cache_tok;
                    r.ai_usage = Some(u);
                });
                core.drove.store.append_daemon_event(NewDaemonEvent {
                    run_id: Some(run_id.to_string()),
                    task_id: None,
                    attempt_id: None,
                    worker_id: None,
                    message_id: None,
                    category: "ai".into(),
                    kind: kind.into(),
                    status: "ok".into(),
                    title: format!("AI {kind} ({model})"),
                    detail: None,
                    duration_ms: None,
                    tokens: Some(DaemonEventTokens { input: in_tok, output: out_tok, cache_hit: cache_tok }),
                    cost: None,
                });
                return Some(content);
            }
            Ok((status, body)) => {
                core.drove.record_ai_failure(&format!("HTTP {status}: {}", body.chars().take(120).collect::<String>()));
                if status == 401 || status == 403 {
                    return None;
                }
            }
            Err(e) => {
                core.drove.record_ai_failure(&e);
            }
        }
    }
    None
}

/// Generates a short human title for a newly created Run when `run.name` is unset.
pub fn spawn_auto_title(core: Arc<Core>, run_id: String, objective: String) {
    let cfg = core.drove.config_snapshot();
    if !cfg.ai.enabled || !cfg.ai.auto_title || cfg.ai.base_url.trim().is_empty() {
        return;
    }
    tokio::spawn(async move {
        let Some(title) = chat_completion(
            &core,
            &run_id,
            "auto_title",
            "Generate a concise 3-6 word title for this coding run. Reply with only the plain title, no quotes.",
            &objective,
            24,
        )
        .await
        else {
            return;
        };
        let clean: String = title.lines().next().unwrap_or("").trim().trim_matches('"').chars().take(60).collect();
        if !clean.is_empty() {
            let _ = core.drove.store.update_run(&run_id, |r| {
                if r.name.is_none() {
                    r.name = Some(clean.clone());
                }
            });
        }
    });
}

/// Generates a short diagnostic summary on failed verification output.
pub fn spawn_verify_digest(core: Arc<Core>, run_id: String, attempt_id: String, cmd_str: String, log_tail: String) {
    let cfg = core.drove.config_snapshot();
    if !cfg.ai.enabled || !cfg.ai.verify_digest || cfg.ai.base_url.trim().is_empty() {
        return;
    }
    tokio::spawn(async move {
        let prompt = format!("Command: `{cmd_str}`\nOutput tail:\n{log_tail}");
        let Some(digest) = chat_completion(
            &core,
            &run_id,
            "verify_digest",
            "Summarize the root failure cause in 1-2 brief sentences.",
            &prompt,
            120,
        )
        .await
        else {
            return;
        };
        let _ = core.drove.store.update_attempt(&attempt_id, |a| {
            a.verify_ai_digest = Some(digest.clone());
        });
    });
}

/// Drafts (or auto-answers if `question_mode == "auto"`) a worker question in background.
pub fn spawn_question_draft(
    core: Arc<Core>,
    run_id: String,
    message_id: String,
    question: String,
    options: Option<Vec<String>>,
) {
    let cfg = core.drove.config_snapshot();
    if !cfg.ai.enabled || cfg.ai.question_mode == "off" || cfg.ai.base_url.trim().is_empty() {
        return;
    }
    let auto_mode = cfg.ai.question_mode == "auto";
    tokio::spawn(async move {
        let opt_txt = options
            .as_ref()
            .filter(|o| !o.is_empty())
            .map(|o| format!("\nOptions: {}", o.join(", ")))
            .unwrap_or_default();
        let prompt = format!("Question: {question}{opt_txt}\nReply with JSON: {{\"answer\":\"...\",\"confidence\":0.0}}");
        let Some(raw) = chat_completion(
            &core,
            &run_id,
            "question_draft",
            "Draft a concise answer for a coding agent question. Output JSON with answer and confidence (0..1).",
            &prompt,
            120,
        )
        .await
        else {
            return;
        };
        let parsed: Value = serde_json::from_str(raw.trim()).unwrap_or_else(|_| json!({ "answer": raw.trim(), "confidence": 0.5 }));
        let answer = parsed["answer"].as_str().unwrap_or(raw.trim()).trim().to_string();
        if answer.is_empty() {
            return;
        }
        let confidence = parsed["confidence"].as_f64().unwrap_or(0.5);
        let auto_reply = auto_mode && confidence >= 0.85;
        let draft = MessageAiDraft {
            answer: answer.clone(),
            confidence,
            category: if auto_reply { "auto".into() } else { "draft".into() },
            sources: Vec::new(),
            auto_replied: auto_reply.then_some(true),
            created_at: now_iso(),
        };
        let msg = core.drove.store.update_message(&message_id, |m| m.ai_draft = Some(draft));
        if auto_reply
            && let Some(m) = msg
            && m.handled_at.is_none()
        {
            let _ = core.drove.store.post_message(PostMessageInput {
                run_id: m.run_id.clone(),
                kind: MessageType::Reply,
                from: "daemon-ai".into(),
                to: m.from.clone(),
                thread_id: Some(m.id.clone()),
                task_id: m.task_id.clone(),
                attempt_id: m.attempt_id.clone(),
                subject: format!("Re: {}", m.subject),
                body: Some(answer),
                options: None,
                recommend: None,
                why: Some("AI auto-answer".into()),
                raised_by: Some("daemon-ai".into()),
                raised_by_worker: None,
                tag: None,
                ai_draft: None,
                needs_action: false,
            });
            core.drove.store.mark_handled(&m.id);
            core.drove.message_reply_wake.notify_waiters();
        }
    });
}

pub fn circuit_cooldown_secs() -> u64 {
    COOLDOWN_SECS
}

pub fn circuit_threshold() -> u32 {
    CIRCUIT_THRESHOLD
}
