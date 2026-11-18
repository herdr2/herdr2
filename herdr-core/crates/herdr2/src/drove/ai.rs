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

const STRICT_CLOUD_HOSTS: [&str; 6] = [
    "api.openai.com",
    "api.deepseek.com",
    "open.bigmodel.cn",
    "dashscope.aliyuncs.com",
    "api.anthropic.com",
    "generativelanguage.googleapis.com",
];

fn is_known_strict_cloud_url(base: &str) -> bool {
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

fn truncate_for_context(text: &str, max_chars: usize) -> String {
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

fn effective_max_tokens(default_max: u32, configured_max: Option<u32>, effort: &str, ctx_tokens: Option<u32>) -> u32 {
    let raw = match configured_max {
        Some(n) => n,
        None => match effort {
            "high" => default_max.max(4096),
            "medium" => default_max.max(2048),
            "auto" => default_max.max(1024),
            _ => default_max.max(512),
        },
    };
    match ctx_tokens {
        Some(ctx) => raw.min((ctx / 2).max(64)),
        None => raw,
    }
}

fn build_chat_payload_candidates(
    model: &str,
    framed_system: &str,
    framed_user: &str,
    max_tokens: u32,
    effort: &str,
    ctx_tokens: Option<u32>,
    base_url: &str,
) -> Vec<String> {
    let messages = json!([
        { "role": "system", "content": framed_system },
        { "role": "user", "content": framed_user }
    ]);
    let include_effort = effort != "auto" && !effort.is_empty();
    let include_options = ctx_tokens.filter(|_| !is_known_strict_cloud_url(base_url));

    let make_obj = |with_effort: bool, with_options: Option<u32>, with_temp: bool| -> String {
        let mut obj = serde_json::Map::new();
        obj.insert("model".into(), json!(model));
        obj.insert("messages".into(), messages.clone());
        obj.insert("max_tokens".into(), json!(max_tokens));
        if with_temp {
            obj.insert("temperature".into(), json!(0.2));
        }
        if with_effort {
            obj.insert("reasoning_effort".into(), json!(effort));
        }
        if let Some(ctx) = with_options {
            obj.insert("options".into(), json!({ "num_ctx": ctx }));
        }
        Value::Object(obj).to_string()
    };

    let mut out = Vec::new();
    let push_unique = |list: &mut Vec<String>, item: String| {
        if !list.contains(&item) {
            list.push(item);
        }
    };
    push_unique(&mut out, make_obj(include_effort, include_options, true));
    if include_options.is_some() {
        push_unique(&mut out, make_obj(include_effort, None, true));
    }
    push_unique(&mut out, make_obj(false, None, false));
    out
}

fn should_retry_without_optional_fields(status: u16, body: &str) -> bool {
    if status != 400 && status != 422 {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    [
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

    let effort = cfg.ai.effort.trim().to_ascii_lowercase();
    let effort = if effort.is_empty() { "low" } else { effort.as_str() };
    let ctx_tokens = cfg.ai.context_window.as_deref().and_then(super::config::parse_ai_token_size);
    let eff_max_tokens = effective_max_tokens(max_tokens, cfg.ai.max_tokens, effort, ctx_tokens);
    let bounded_user = match ctx_tokens {
        Some(ctx) => {
            let input_budget_tokens = ctx.saturating_sub(eff_max_tokens + 512).max(512);
            truncate_for_context(user_prompt, (input_budget_tokens as usize).saturating_mul(3))
        }
        None => user_prompt.to_string(),
    };

    let framed_system = format!(
        "You are a pure text assistant with NO tools or shell access. \
         Never emit <tool_use>, DSML, XML tool tags, or shell commands. {system_prompt}"
    );
    let framed_user = format!(
        "Do NOT execute tasks or call tools. Process the quoted text below according to system instructions:\n\
         <input>\n{bounded_user}\n</input>"
    );
    let candidates = build_chat_payload_candidates(
        &model,
        &framed_system,
        &framed_user,
        eff_max_tokens,
        effort,
        ctx_tokens,
        &base,
    );

    for url in chat_endpoints(&base) {
        let mut resp = Err("no payload candidates".into());
        for (idx, payload) in candidates.iter().enumerate() {
            resp = curl_json("POST", &url, key.as_deref(), Some(payload)).await;
            let has_next = idx + 1 < candidates.len();
            if has_next
                && let Ok((st, ref body)) = resp
                && should_retry_without_optional_fields(st, body)
            {
                continue;
            }
            break;
        }
        match resp {
            Ok((status, body)) if (200..300).contains(&status) => {
                let parsed: Value = serde_json::from_str(&body).ok()?;
                let in_tok = parsed["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
                let out_tok = parsed["usage"]["completion_tokens"].as_u64().unwrap_or(0);
                let cache_tok = parsed["usage"]["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);
                if (in_tok > 0 || out_tok > 0 || cache_tok > 0) && !run_id.trim().is_empty() {
                    let _ = core.drove.store.update_run(run_id, |r| {
                        let mut u = r.ai_usage.clone().unwrap_or_default();
                        u.calls += 1;
                        u.input_tokens += in_tok;
                        u.output_tokens += out_tok;
                        u.cache_hit_tokens += cache_tok;
                        r.ai_usage = Some(u);
                    });
                }

                let raw_content = parsed["choices"][0]["message"]["content"]
                    .as_str()
                    .map(str::trim)
                    .unwrap_or("");
                if raw_content.is_empty()
                    || raw_content.contains("<tool_use")
                    || raw_content.contains("DSML")
                {
                    let reason = if raw_content.is_empty() {
                        "empty assistant content"
                    } else {
                        "unexpected tool-call markup in content"
                    };
                    core.drove.record_ai_failure(reason);
                    core.drove.store.append_daemon_event(NewDaemonEvent {
                        run_id: (!run_id.trim().is_empty()).then(|| run_id.to_string()),
                        task_id: None,
                        attempt_id: None,
                        worker_id: None,
                        message_id: None,
                        category: "ai".into(),
                        kind: kind.into(),
                        status: "warn".into(),
                        title: format!("AI {kind} returned empty/invalid content ({model})"),
                        detail: Some(reason.into()),
                        duration_ms: None,
                        tokens: (in_tok + out_tok > 0).then_some(DaemonEventTokens {
                            input: in_tok,
                            output: out_tok,
                            cache_hit: cache_tok,
                        }),
                        cost: None,
                    });
                    return None;
                }

                core.drove.record_ai_success();
                core.drove.store.append_daemon_event(NewDaemonEvent {
                    run_id: (!run_id.trim().is_empty()).then(|| run_id.to_string()),
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
                return Some(raw_content.to_string());
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

fn extract_json_object(raw: &str) -> Option<Value> {
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
    let auto_mode = matches!(cfg.ai.question_mode.as_str(), "auto" | "auto_when_offline");
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
        let parsed = extract_json_object(&raw).unwrap_or_else(|| json!({ "answer": raw.trim(), "confidence": 0.5 }));
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

/// Compresses a long worker completion summary into a 1-sentence `compactSummary` for downstream handovers.
pub fn spawn_handover_compact(
    core: Arc<Core>,
    run_id: String,
    task_id: String,
    attempt_id: String,
    summary: String,
) {
    let cfg = core.drove.config_snapshot();
    if !cfg.ai.enabled
        || !cfg.ai.handover_compact
        || cfg.ai.base_url.trim().is_empty()
        || summary.chars().count() <= 140
    {
        return;
    }
    tokio::spawn(async move {
        let Some(compact) = chat_completion(
            &core,
            &run_id,
            "handover_compact",
            "Compress the worker completion summary into 1 concise factual sentence (max 120 chars) preserving modified files and key outcome. Reply with plain text only.",
            &summary,
            512,
        )
        .await
        else {
            return;
        };
        let clean: String = compact.lines().next().unwrap_or("").trim().chars().take(200).collect();
        if clean.is_empty() {
            return;
        }
        let _ = core.drove.store.update_attempt(&attempt_id, |a| {
            if let Some(res) = &mut a.result {
                res.compact_summary = Some(clean.clone());
            }
        });
        let _ = core.drove.store.update_task(&task_id, |t| {
            if let Some(res) = &mut t.result {
                res.compact_summary = Some(clean.clone());
            }
        });
    });
}

/// Enriches newly added sparse tasks (`files` empty or brief `acceptance`) in background.
pub fn spawn_task_enrich(core: Arc<Core>, run_id: String, task_id: String) {
    let cfg = core.drove.config_snapshot();
    if !cfg.ai.enabled || !cfg.ai.task_enrich || cfg.ai.base_url.trim().is_empty() {
        return;
    }
    tokio::spawn(async move {
        let Some(task) = core.drove.store.get_task(&task_id) else {
            return;
        };
        if task.kind.as_deref() == Some("exec")
            || task.template == Some(true)
            || (!task.files.is_empty() && task.spec.acceptance.trim().chars().count() >= 24)
        {
            return;
        }
        let prompt = format!(
            "Task key: {}\nTitle: {}\nTarget: {}\nChange:\n{}\nAcceptance: {}",
            task.key, task.title, task.spec.target, task.spec.change, task.spec.acceptance
        );
        let Some(raw) = chat_completion(
            &core,
            &run_id,
            "task_enrich",
            "Extract concrete workspace file paths and a concise 1-sentence acceptance criterion. Reply ONLY with JSON: {\"files\":[\"...\"],\"acceptance\":\"...\"}",
            &prompt,
            512,
        )
        .await
        else {
            return;
        };
        let Some(parsed) = extract_json_object(&raw) else {
            return;
        };
        let extracted_files: Vec<String> = parsed["files"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::trim))
                    .filter(|s| !s.is_empty() && !s.contains(' '))
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let extracted_acc = parsed["acceptance"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        if extracted_files.is_empty() && extracted_acc.is_none() {
            return;
        }
        let _ = core.drove.store.update_task(&task_id, |t| {
            if t.files.is_empty() && !extracted_files.is_empty() {
                t.files = extracted_files.clone();
            }
            if t.status == TaskStatus::Pending
                && t.spec.acceptance.trim().chars().count() < 24
                && let Some(acc) = &extracted_acc
            {
                t.spec.acceptance = acc.clone();
            }
        });
    });
}

/// Generates (or returns cached) structured AI failure diagnosis for `GET /api/tasks/:id/inspect?ai=1`.
pub async fn generate_task_diagnosis(core: &Arc<Core>, diag: &mut Diagnosis, refresh: bool) {
    let cfg = core.drove.config_snapshot();
    if !cfg.ai.enabled || cfg.ai.base_url.trim().is_empty() {
        return;
    }
    let Some(att) = diag.attempt.as_ref() else {
        return;
    };
    if !refresh && let Some(cached) = &att.ai_diagnosis {
        diag.ai = Some(cached.clone());
        return;
    }
    let att_id = att.id.clone();
    let run_id = diag.task.run_id.clone();
    let fail_txt = diag
        .failure
        .as_ref()
        .map(|f| format!("{:?}: {}", f.kind, f.message))
        .unwrap_or_else(|| "none".into());
    let recent_txt = diag
        .last_items
        .iter()
        .rev()
        .take(6)
        .filter_map(|it| it.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n---\n");
    let prompt = format!(
        "Task: {} (`{}`)\nStatus: {:?}\nAttempt: {} ({} / {:?})\nFailure: {fail_txt}\nSuggestion: {}\nRecent output:\n{}",
        diag.task.title,
        diag.task.key,
        diag.task.status,
        att_id,
        att.agent,
        att.model,
        diag.suggestion,
        recent_txt
    );
    let Some(raw) = chat_completion(
        core,
        &run_id,
        "task_diagnose",
        "Analyze the Drove task failure and reply ONLY with JSON: {\"rootCause\":\"...\",\"evidence\":[\"...\"],\"recommendedAction\":\"...\",\"suggestedCommand\":\"h2 drove retry ...\",\"hint\":\"...\"}",
        &prompt,
        512,
    )
    .await
    else {
        return;
    };
    let parsed = extract_json_object(&raw).unwrap_or_else(|| {
        json!({
            "rootCause": raw.trim().chars().take(300).collect::<String>(),
            "evidence": [],
            "recommendedAction": diag.suggestion.clone(),
        })
    });
    let root_cause = parsed["rootCause"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&diag.suggestion)
        .to_string();
    let evidence = parsed["evidence"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let recommended_action = parsed["recommendedAction"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&diag.suggestion)
        .to_string();
    let ai_diag = AttemptAiDiagnosis {
        at: now_iso(),
        based_on_updated_at: diag.task.updated_at.clone(),
        root_cause,
        evidence,
        recommended_action,
        suggested_command: parsed["suggestedCommand"].as_str().map(String::from),
        hint: parsed["hint"].as_str().map(String::from),
    };
    let _ = core.drove.store.update_attempt(&att_id, |a| {
        a.ai_diagnosis = Some(ai_diag.clone());
    });
    diag.ai = Some(ai_diag);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_for_context_preserves_head_and_tail() {
        let short = "hello world";
        assert_eq!(truncate_for_context(short, 100), short);

        let long = format!("{}{}{}", "A".repeat(80), "MIDDLE", "Z".repeat(80));
        let truncated = truncate_for_context(&long, 80);
        assert!(truncated.starts_with(&"A".repeat(40)));
        assert!(truncated.ends_with(&"Z".repeat(40)));
        assert!(truncated.contains("[truncated to fit configured context window]"));
        assert!(!truncated.contains("MIDDLE"));
    }

    #[test]
    fn effective_max_tokens_scales_with_effort_and_clamps_to_context() {
        assert_eq!(effective_max_tokens(24, None, "low", None), 512);
        assert_eq!(effective_max_tokens(24, None, "none", None), 512);
        assert_eq!(effective_max_tokens(24, None, "auto", None), 1024);
        assert_eq!(effective_max_tokens(24, None, "medium", None), 2048);
        assert_eq!(effective_max_tokens(24, None, "high", None), 4096);
        assert_eq!(effective_max_tokens(24, Some(8192), "high", None), 8192);
        assert_eq!(effective_max_tokens(24, Some(8192), "high", Some(8192)), 4096);
    }

    #[test]
    fn build_chat_payload_candidates_handles_local_options_and_cloud_skip() {
        let local_cands = build_chat_payload_candidates(
            "qwen3",
            "sys",
            "usr",
            2048,
            "medium",
            Some(32768),
            "http://127.0.0.1:11434/v1",
        );
        assert_eq!(local_cands.len(), 3);
        let p0: Value = serde_json::from_str(&local_cands[0]).unwrap();
        assert_eq!(p0["reasoning_effort"], "medium");
        assert_eq!(p0["options"]["num_ctx"], 32768);
        let p1: Value = serde_json::from_str(&local_cands[1]).unwrap();
        assert_eq!(p1["reasoning_effort"], "medium");
        assert!(p1.get("options").is_none());
        let p2: Value = serde_json::from_str(&local_cands[2]).unwrap();
        assert!(p2.get("reasoning_effort").is_none());
        assert!(p2.get("temperature").is_none());

        let cloud_cands = build_chat_payload_candidates(
            "deepseek-chat",
            "sys",
            "usr",
            1024,
            "auto",
            Some(65536),
            "https://api.deepseek.com",
        );
        assert_eq!(cloud_cands.len(), 2);
        let c0: Value = serde_json::from_str(&cloud_cands[0]).unwrap();
        assert!(c0.get("options").is_none());
        assert!(c0.get("reasoning_effort").is_none());
    }
}
