//! Lightweight optional AI assistant hooks (`Service URL` + `0600 ai.key` + circuit breaker).

use super::config::{clamp_effort_for_model, normalize_strengths, read_ai_key, validate_http_url};
use super::probe::{chat_endpoints, curl_json, curl_json_timeout, first_model_id, models_endpoints};
use super::store::{NewDaemonEvent, PostMessageInput};
use super::types::*;
use crate::runtime::Core;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// Optional fast AI tie-breaker when a role has multiple candidates with `strengths` configured
/// and deterministic keyword matching had no unique winner.
pub async fn pick_role_candidate(
    core: &Arc<Core>,
    run_id: &str,
    task: &Task,
    role_name: &str,
    pool: &[(usize, RoleTarget)],
) -> Option<(usize, String)> {
    let cfg = core.drove.config_snapshot();
    if !cfg.ai.enabled
        || cfg.ai.base_url.trim().is_empty()
        || core.drove.is_ai_circuit_open()
        || pool.len() < 2
    {
        return None;
    }

    let cands_desc: Vec<Value> = pool
        .iter()
        .enumerate()
        .map(|(pos, (_, c))| {
            json!({
                "index": pos,
                "agent": c.agent,
                "model": c.model.as_deref().unwrap_or("default"),
                "effort": format!("{:?}", c.effort).to_lowercase(),
                "strengths": c.strengths,
            })
        })
        .collect();
    let user_prompt = json!({
        "role": role_name,
        "task": {
            "key": task.key,
            "title": task.title,
            "tags": task.tags,
            "files": task.files.iter().take(16).collect::<Vec<_>>(),
            "target": task.spec.target,
            "change": task.spec.change.chars().take(600).collect::<String>(),
        },
        "candidates": cands_desc,
    })
    .to_string();

    let raw = tokio::time::timeout(
        Duration::from_secs(6),
        chat_completion(
            core,
            run_id,
            "route_candidate",
            "Choose the single best candidate index from `candidates` for the task based on each candidate's `strengths`, `agent`, and `model`. If none stands out, pick index 0. Reply ONLY with JSON: {\"index\":0,\"reason\":\"3-6 words\"}",
            &user_prompt,
            128,
        ),
    )
    .await
    .ok()??;

    let parsed = extract_json_object(&raw)?;
    let pos = parsed["index"].as_u64()? as usize;
    let &(chain_idx, _) = pool.get(pos)?;
    let reason: String = parsed["reason"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("ai")
        .chars()
        .take(40)
        .collect();
    Some((chain_idx, reason))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AiRoleConfigMode {
    Models,
    Strengths,
    All,
}

impl AiRoleConfigMode {
    fn from_opt(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("models" | "roles" | "agents") => Self::Models,
            Some("strengths" | "tasks" | "specialties") => Self::Strengths,
            _ => Self::All,
        }
    }
}

/// Uses the configured AI Assistant LLM to automatically configure Drove roles:
/// - `mode = "models"`: assigns `agent + model + effort (+ fast/context)` per role while keeping existing strengths.
/// - `mode = "strengths"`: keeps existing `agent + model` locked and assigns each candidate's work-type `strengths`.
/// - `mode = "all"` (default): configures both.
pub async fn configure_roles_with_ai(
    core: &Arc<Core>,
    params: DroveRolesAiConfigureParams,
) -> DroveRolesAiConfigureResult {
    let started = Instant::now();
    let mode = AiRoleConfigMode::from_opt(params.mode.as_deref());
    let fail = |msg: String, probed_refreshed: bool| DroveRolesAiConfigureResult {
        ok: false,
        applied: false,
        probed_refreshed,
        model_used: None,
        latency_ms: started.elapsed().as_millis() as u64,
        rationale: None,
        roles: None,
        settings: None,
        error: Some(msg),
    };

    let cfg = core.drove.config_snapshot();
    let raw_base = params
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(cfg.ai.base_url.trim());
    if raw_base.is_empty() {
        return fail("请先在下方「智能辅助模型」中配置远端服务地址（Service URL）".into(), false);
    }
    let base = match validate_http_url(raw_base, "ai.baseUrl") {
        Ok(u) => u,
        Err(e) => return fail(e.message, false),
    };
    let (stored_key, _) = read_ai_key(&core.drove.paths);
    let key = params
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or(stored_key);
    if key.is_none() && !super::config::is_loopback_http_url(&base) {
        return fail("请先在下方「智能辅助模型」中配置 API Key".into(), false);
    }

    let mut model = params
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(cfg.ai.model.trim())
        .to_string();
    if model.is_empty() {
        let status = core.drove.ai_status();
        if let Some(first) = status.available_models.first() {
            model = first.clone();
        }
    }
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
        return fail("未找到可用的智能辅助模型，请先在下方填写模型或点击「测试连通」".into(), false);
    }

    // Gather agent & model inventory; if no cached full probe or no installed agent has discovered models yet, refresh first.
    let mut probed = match core.drove.rpc_probe(false).await {
        Ok(snap) => snap,
        Err(_) => core.drove.cached_probe(),
    };
    let mut probed_refreshed = false;
    let has_cached_models = probed.full_probe && probed.agents.iter().any(|a| a.installed && !a.models.is_empty());
    if !has_cached_models
        && let Ok(full) = core.drove.rpc_probe(true).await
    {
        probed = full;
        probed_refreshed = true;
    }

    let any_installed = probed.agents.iter().any(|a| a.installed);
    let agents_inventory: Vec<Value> = probed
        .agents
        .iter()
        .filter(|a| !any_installed || a.installed)
        .map(|a| {
            let mut obj = json!({
                "id": a.id,
                "installed": a.installed,
                "defaultModel": a.default_model,
                "models": a.models.iter().take(24).collect::<Vec<_>>(),
                "efforts": a.efforts,
            });
            if !a.model_contexts.is_empty() {
                obj["modelContexts"] = json!(a.model_contexts);
            }
            if !a.model_efforts.is_empty() {
                obj["modelEfforts"] = json!(a.model_efforts);
            }
            obj
        })
        .collect();

    let quotas = if cfg.magpie.enabled {
        core.drove.magpie.fetch_if_enabled(&cfg.magpie, false).await.0
    } else {
        None
    };
    let quota_summary: Vec<Value> = quotas
        .unwrap_or_default()
        .into_iter()
        .map(|q| {
            let min_rem = q
                .windows
                .as_ref()
                .and_then(|ws| ws.iter().map(|w| w.remaining).reduce(f64::min));
            json!({
                "provider": q.provider,
                "minRemainingPct": min_rem,
                "balance": q.balance,
            })
        })
        .collect();

    let roles_desc: Vec<Value> = cfg
        .roles
        .iter()
        .map(|(id, r)| {
            let current_candidates: Vec<Value> = std::iter::once(&r.primary)
                .chain(r.fallbacks.iter())
                .enumerate()
                .map(|(idx, c)| {
                    json!({
                        "index": idx,
                        "agent": c.agent,
                        "model": c.model,
                        "effort": c.effort,
                        "fast": c.fast,
                        "context": c.context,
                        "strengths": c.strengths,
                    })
                })
                .collect();
            json!({
                "id": id,
                "label": r.label,
                "summary": r.summary,
                "access": r.access,
                "currentCandidates": current_candidates,
            })
        })
        .collect();

    let instruction = params.instruction.as_deref().map(str::trim).unwrap_or("");
    let prefer_zh = params
        .language
        .as_deref()
        .map(|l| l.to_ascii_lowercase().starts_with("zh"))
        .unwrap_or_else(|| instruction.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)) || true);

    let lang_rule = if prefer_zh {
        "Write `rationale` and any `strengths` tags in concise Simplified Chinese (2-6 chars per tag, e.g. \"复杂架构\", \"前端UI\", \"后端重构\", \"疑难排障\", \"快速修复\", \"单元测试\", \"安全审计\", \"源码调研\", \"长上下文\")."
    } else {
        "Write `rationale` and any `strengths` tags in concise English (2-3 words per tag, e.g. \"Backend Architecture\", \"Frontend UI\", \"Complex Refactor\", \"Bugfix & Debug\", \"Unit Tests\", \"Security Review\", \"Codebase Research\")."
    };

    let system_prompt = match mode {
        AiRoleConfigMode::Models => format!(
            "You are a pure JSON configuration architect for Drove (a multi-agent software engineering orchestrator). \
             Do NOT call tools or emit tool markup. \
             Configure ONLY the `agent`, `model`, `effort`, `fast`, and `context` of 1 to 3 complementary `candidates` for every role in `roles`.\n\
             Rules:\n\
             1. ONLY choose `agent` values from `availableAgents[].id`.\n\
             2. For each candidate's `model`, ONLY choose an exact string from that agent's `models` array, or `null` to use the agent's default.\n\
             3. `effort` must be supported by that model (`low`, `medium`, `high`, or `xhigh` ONLY for models supporting extra-high reasoning such as Opus/Fable/GPT-5.2+/GPT-6/Grok-4.7+/Gemini-4-Neon).\n\
             4. `fast` may be `true` ONLY for `codex` (otherwise `false`). `context` may be set ONLY for `cursor`/`cursor-agent` (otherwise `null`).\n\
             5. Prefer a different primary agent for `reviewer` than `coder` when multiple agents are available (for heterogeneous adversarial review).\n\
             6. If `quotas` shows a provider at 0% remaining with no balance, avoid putting it as candidate #1.\n\
             7. Strictly honor `userPreference` if non-empty.\n\
             8. {lang_rule}\n\
             9. Output ONLY valid JSON matching:\n\
             {{\"rationale\":\"1-2 sentences\",\"roles\":{{\"<roleId>\":{{\"candidates\":[{{\"agent\":\"...\",\"model\":\"...\"|null,\"effort\":\"high\",\"fast\":false,\"context\":null}}]}}}}}}"
        ),
        AiRoleConfigMode::Strengths => format!(
            "You are a pure JSON configuration architect for Drove (a multi-agent software engineering orchestrator). \
             Do NOT call tools or emit tool markup. \
             Each role in `roles` already has its `currentCandidates` (`index`, `agent`, `model`, `effort`). \
             Do NOT change, add, or remove any candidate's `agent` or `model`; configure ONLY the `strengths` array (2-4 distinct work-type tags) for EVERY existing candidate in EVERY role so the orchestrator can pick the best-suited candidate for each incoming task.\n\
             Rules:\n\
             1. Return an entry for EVERY role in `roles` and EVERY candidate index `0..currentCandidates.length`.\n\
             2. Within a role that has multiple candidates, assign complementary, clearly differentiated `strengths` matching each agent+model's real capabilities (e.g. complex architecture/backend vs frontend UI/layout vs fast bugfix/scripts vs long-context codebase analysis).\n\
             3. Strictly honor `userPreference` if non-empty.\n\
             4. {lang_rule}\n\
             5. Output ONLY valid JSON matching:\n\
             {{\"rationale\":\"1-2 sentences\",\"roles\":{{\"<roleId>\":{{\"candidates\":[{{\"index\":0,\"agent\":\"...\",\"model\":\"...\"|null,\"strengths\":[\"...\",\"...\"]}}]}}}}}}"
        ),
        AiRoleConfigMode::All => format!(
            "You are a pure JSON configuration architect for Drove (a multi-agent software engineering orchestrator). \
             Do NOT call tools or emit tool markup. \
             Configure every role in `roles` with 1 to 3 complementary `candidates` (`agent` + `model` + `effort` + `fast` + `context` + `strengths`) \
             so the orchestrator can automatically pick the best candidate for each task type and fall back cleanly on quota exhaustion or retries.\n\
             Rules:\n\
             1. ONLY choose `agent` values from `availableAgents[].id`.\n\
             2. For each candidate's `model`, ONLY choose an exact string from that agent's `models` array, or `null` to use the agent's default.\n\
             3. `effort` must be supported by that model (`low`, `medium`, `high`, or `xhigh` ONLY for models supporting extra-high reasoning).\n\
             4. `fast` may be `true` ONLY for `codex` (otherwise `false`). `context` may be set ONLY for `cursor`/`cursor-agent` (otherwise `null`).\n\
             5. Within roles that benefit from specialization (especially `coder`, `reviewer`, `researcher`, `tester`, `writer`, `illustrator`), when multiple agents/models are installed, assign 2-3 complementary candidates with distinct, non-overlapping `strengths` (2-4 tags each).\n\
             6. Prefer a different primary agent for `reviewer` than `coder` when multiple agents are available (for heterogeneous adversarial review).\n\
             7. If `quotas` shows a provider at 0% remaining with no balance, avoid putting it as candidate #1.\n\
             8. Strictly honor `userPreference` if non-empty.\n\
             9. {lang_rule}\n\
             10. Output ONLY valid JSON matching:\n\
             {{\"rationale\":\"1-2 sentences\",\"roles\":{{\"<roleId>\":{{\"candidates\":[{{\"agent\":\"...\",\"model\":\"...\"|null,\"effort\":\"high\",\"fast\":false,\"context\":null,\"strengths\":[\"...\",\"...\"]}}]}}}}}}"
        ),
    };

    let user_payload = json!({
        "mode": match mode {
            AiRoleConfigMode::Models => "models",
            AiRoleConfigMode::Strengths => "strengths",
            AiRoleConfigMode::All => "all",
        },
        "roles": roles_desc,
        "availableAgents": agents_inventory,
        "quotas": quota_summary,
        "userPreference": instruction,
    })
    .to_string();

    let effort = cfg.ai.effort.trim().to_ascii_lowercase();
    let effort = if effort.is_empty() { "low" } else { effort.as_str() };
    let ctx_tokens = cfg.ai.context_window.as_deref().and_then(super::config::parse_ai_token_size);
    let eff_max_tokens = effective_max_tokens(2048, cfg.ai.max_tokens, effort, ctx_tokens).max(1536);

    let framed_system = format!(
        "You are a pure text assistant with NO tools or shell access. \
         Never emit <tool_use>, DSML, XML tool tags, or shell commands. {system_prompt}"
    );
    let framed_user = format!(
        "Do NOT execute tasks or call tools. Return ONLY the JSON configuration for the input below:\n\
         <input>\n{user_payload}\n</input>"
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

    let mut raw_content: Option<String> = None;
    let mut last_err = String::from("AI endpoint did not respond");
    for url in chat_endpoints(&base) {
        let mut resp = Err("no payload candidates".into());
        for (idx, payload) in candidates.iter().enumerate() {
            resp = curl_json_timeout("POST", &url, key.as_deref(), Some(payload), Duration::from_secs(60)).await;
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
                match serde_json::from_str::<Value>(&body) {
                    Ok(parsed) => {
                        let content = parsed["choices"][0]["message"]["content"]
                            .as_str()
                            .map(str::trim)
                            .unwrap_or("");
                        if content.is_empty() {
                            last_err = "AI 返回内容为空".into();
                        } else {
                            raw_content = Some(content.to_string());
                            core.drove.record_ai_success();
                            break;
                        }
                    }
                    Err(e) => {
                        last_err = format!("解析 AI 响应失败: {e}");
                    }
                }
            }
            Ok((status, body)) => {
                let snippet: String = body.chars().take(180).collect();
                last_err = format!("HTTP {status}: {snippet}");
                if status == 401 || status == 403 {
                    break;
                }
            }
            Err(e) => {
                last_err = e;
            }
        }
    }

    let Some(raw) = raw_content else {
        core.drove.record_ai_failure(&last_err);
        return fail(last_err, probed_refreshed);
    };

    let (next_roles, rationale) = match parse_and_sanitize_ai_roles_for_mode(&raw, &cfg.roles, &probed, mode) {
        Ok(res) => res,
        Err(e) => return fail(e, probed_refreshed),
    };

    let should_apply = params.apply.unwrap_or(true);
    let settings = if should_apply {
        match core
            .drove
            .rpc_settings_set(DroveSettingsSetParams {
                roles: Some(next_roles.clone()),
                ..Default::default()
            })
            .await
        {
            Ok(s) => Some(s),
            Err(e) => return fail(format!("保存配置失败: {}", e.message), probed_refreshed),
        }
    } else {
        None
    };

    DroveRolesAiConfigureResult {
        ok: true,
        applied: should_apply,
        probed_refreshed,
        model_used: Some(model),
        latency_ms: started.elapsed().as_millis() as u64,
        rationale,
        roles: Some(next_roles),
        settings,
        error: None,
    }
}

fn parse_effort(raw: Option<&str>, fallback: Effort) -> Effort {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("low") => Effort::Low,
        Some("medium" | "med") => Effort::Medium,
        Some("high") => Effort::High,
        Some("xhigh" | "max" | "extra-high" | "extra_high") => Effort::Xhigh,
        _ => fallback,
    }
}

#[cfg(test)]
pub(crate) fn parse_and_sanitize_ai_roles(
    raw: &str,
    existing_roles: &BTreeMap<String, RoleConfig>,
    probed: &ProbeSnapshot,
) -> Result<(BTreeMap<String, RoleConfig>, Option<String>), String> {
    parse_and_sanitize_ai_roles_for_mode(raw, existing_roles, probed, AiRoleConfigMode::All)
}

pub(crate) fn parse_and_sanitize_ai_roles_for_mode(
    raw: &str,
    existing_roles: &BTreeMap<String, RoleConfig>,
    probed: &ProbeSnapshot,
    mode: AiRoleConfigMode,
) -> Result<(BTreeMap<String, RoleConfig>, Option<String>), String> {
    let parsed = extract_json_object(raw)
        .ok_or_else(|| format!("AI 未返回有效的 JSON 配置: {}", raw.chars().take(160).collect::<String>()))?;
    let roles_obj = parsed
        .get("roles")
        .and_then(Value::as_object)
        .or_else(|| parsed.as_object())
        .ok_or_else(|| "AI 响应中缺少 roles 字段".to_string())?;
    let rationale = parsed
        .get("rationale")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(400).collect::<String>());

    let any_installed = probed.agents.iter().any(|a| a.installed);
    let mut out = existing_roles.clone();
    let mut updated_any = false;

    for (role_id, existing) in existing_roles {
        let Some(role_val) = roles_obj.get(role_id) else {
            continue;
        };
        let mut raw_cands: Vec<&Value> = Vec::new();
        if let Some(arr) = role_val.get("candidates").and_then(Value::as_array) {
            raw_cands.extend(arr.iter());
        } else {
            if let Some(p) = role_val.get("primary") {
                raw_cands.push(p);
            }
            if let Some(fbs) = role_val.get("fallbacks").and_then(Value::as_array) {
                raw_cands.extend(fbs.iter());
            }
        }

        if mode == AiRoleConfigMode::Strengths {
            // Keep existing agents/models/efforts 100% locked; only update `strengths`.
            let mut chain: Vec<RoleTarget> = std::iter::once(existing.primary.clone())
                .chain(existing.fallbacks.iter().cloned())
                .collect();
            let mut role_changed = false;
            for (pos, item) in raw_cands.iter().enumerate() {
                let raw_strengths: Vec<String> = item
                    .get("strengths")
                    .and_then(Value::as_array)
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let clean = normalize_strengths(&raw_strengths);
                if clean.is_empty() {
                    continue;
                }
                let target_idx = item
                    .get("index")
                    .and_then(Value::as_u64)
                    .map(|i| i as usize)
                    .filter(|&i| i < chain.len())
                    .or_else(|| {
                        let a = item.get("agent").and_then(Value::as_str).map(str::trim)?;
                        let m = item.get("model").and_then(Value::as_str).map(str::trim);
                        chain
                            .iter()
                            .position(|c| {
                                c.agent.eq_ignore_ascii_case(a)
                                    && m.is_none_or(|mv| c.model.as_deref().is_some_and(|cm| cm.eq_ignore_ascii_case(mv)))
                            })
                            .or_else(|| chain.iter().position(|c| c.agent.eq_ignore_ascii_case(a)))
                    })
                    .or_else(|| (pos < chain.len()).then_some(pos));
                if let Some(idx) = target_idx {
                    chain[idx].strengths = clean;
                    role_changed = true;
                }
            }
            if role_changed && let Some(primary) = chain.first().cloned() {
                let mut next_role = existing.clone();
                next_role.primary = primary;
                next_role.fallbacks = chain.into_iter().skip(1).collect();
                out.insert(role_id.clone(), next_role);
                updated_any = true;
            }
            continue;
        }

        let existing_chain: Vec<&RoleTarget> = std::iter::once(&existing.primary)
            .chain(existing.fallbacks.iter())
            .collect();
        let mut clean_cands: Vec<RoleTarget> = Vec::new();
        let mut seen_pairs: HashSet<(String, Option<String>)> = HashSet::new();

        for item in raw_cands {
            let Some(raw_agent) = item.get("agent").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
                continue;
            };
            let matched_agent = probed.agents.iter().find(|a| {
                a.id.eq_ignore_ascii_case(raw_agent) && (!any_installed || a.installed)
            });
            let (agent_id, discovered_models) = match matched_agent {
                Some(a) => (a.id.clone(), a.models.as_slice()),
                None if !probed.agents.is_empty() => continue,
                None => (raw_agent.to_ascii_lowercase(), &[][..]),
            };

            let raw_model = item
                .get("model")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto") && !s.eq_ignore_ascii_case("default"));
            let model = match raw_model {
                Some(m) if !discovered_models.is_empty() => discovered_models
                    .iter()
                    .find(|dm| dm.eq_ignore_ascii_case(m))
                    .cloned(),
                Some(m) => Some(m.to_string()),
                None => None,
            };

            let pair_key = (agent_id.clone(), model.as_ref().map(|m| m.to_ascii_lowercase()));
            if !seen_pairs.insert(pair_key) {
                continue;
            }

            let raw_effort = parse_effort(item.get("effort").and_then(Value::as_str), existing.primary.effort);
            let effort = clamp_effort_for_model(&agent_id, model.as_deref(), raw_effort, matched_agent);
            let fast = agent_id == "codex" && item.get("fast").and_then(Value::as_bool).unwrap_or(false);
            let context = if matches!(agent_id.as_str(), "cursor" | "cursor-agent") {
                item.get("context")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto") && !s.eq_ignore_ascii_case("default"))
                    .map(String::from)
            } else {
                None
            };
            let strengths = if mode == AiRoleConfigMode::Models {
                // Preserve previously configured strengths for matching candidate when only re-picking models.
                existing_chain
                    .iter()
                    .find(|c| {
                        c.agent.eq_ignore_ascii_case(&agent_id)
                            && c.model.as_deref().map(str::to_ascii_lowercase)
                                == model.as_deref().map(str::to_ascii_lowercase)
                    })
                    .or_else(|| existing_chain.iter().find(|c| c.agent.eq_ignore_ascii_case(&agent_id)))
                    .map(|c| c.strengths.clone())
                    .unwrap_or_default()
            } else {
                let raw_strengths: Vec<String> = item
                    .get("strengths")
                    .and_then(Value::as_array)
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                normalize_strengths(&raw_strengths)
            };

            clean_cands.push(RoleTarget {
                agent: agent_id,
                model,
                effort,
                fast,
                context,
                strengths,
            });
            if clean_cands.len() >= 5 {
                break;
            }
        }

        if let Some(primary) = clean_cands.first().cloned() {
            let mut next_role = existing.clone();
            next_role.primary = primary;
            next_role.fallbacks = clean_cands.into_iter().skip(1).collect();
            out.insert(role_id.clone(), next_role);
            updated_any = true;
        }
    }

    if !updated_any {
        return Err(match mode {
            AiRoleConfigMode::Strengths => "AI 未返回任何有效的擅长任务标签".into(),
            _ => "AI 未生成任何匹配当前已安装 Agent 的有效角色配置".into(),
        });
    }
    Ok((out, rationale))
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

    #[test]
    fn parse_and_sanitize_ai_roles_filters_uninstalled_and_unknown_models() {
        let existing = super::super::config::default_roles();
        let probed = ProbeSnapshot {
            probed_at: now_iso(),
            full_probe: true,
            agents: vec![
                ProbedAgent {
                    id: "claude".into(),
                    command: vec!["claude".into()],
                    installed: true,
                    path: Some("/usr/bin/claude".into()),
                    agent_info: None,
                    models: vec!["claude-opus-4-6".into(), "claude-sonnet-4-6".into()],
                    default_model: Some("claude-sonnet-4-6".into()),
                    efforts: vec!["low".into(), "medium".into(), "high".into()],
                    default_effort: Some("high".into()),
                    model_contexts: BTreeMap::new(),
                    model_efforts: BTreeMap::new(),
                    error: None,
                },
                ProbedAgent {
                    id: "codex".into(),
                    command: vec!["codex".into()],
                    installed: true,
                    path: Some("/usr/bin/codex".into()),
                    agent_info: None,
                    models: vec!["gpt-5.4".into()],
                    default_model: Some("gpt-5.4".into()),
                    efforts: vec!["medium".into(), "high".into(), "xhigh".into()],
                    default_effort: Some("high".into()),
                    model_contexts: BTreeMap::new(),
                    model_efforts: BTreeMap::new(),
                    error: None,
                },
                ProbedAgent {
                    id: "gemini".into(),
                    command: vec!["gemini".into()],
                    installed: false,
                    path: None,
                    agent_info: None,
                    models: vec!["gemini-2.5-pro".into()],
                    default_model: None,
                    efforts: Vec::new(),
                    default_effort: None,
                    model_contexts: BTreeMap::new(),
                    model_efforts: BTreeMap::new(),
                    error: None,
                },
            ],
        };

        let raw = r#"```json
        {
          "rationale": "Claude 负责架构，Codex 负责快速修复与审查",
          "roles": {
            "coder": {
              "candidates": [
                { "agent": "gemini", "model": "gemini-2.5-pro", "effort": "high", "strengths": ["前端UI"] },
                { "agent": "Claude", "model": "claude-opus-4-6", "effort": "high", "fast": true, "strengths": ["复杂架构", "后端重构", "复杂架构"] },
                { "agent": "codex", "model": "hallucinated-model", "effort": "xhigh", "fast": true, "strengths": ["快速修复", "算法性能"] }
              ]
            },
            "reviewer": {
              "primary": { "agent": "codex", "model": "GPT-5.4", "effort": "high", "strengths": ["安全审计"] },
              "fallbacks": [
                { "agent": "claude", "model": "claude-sonnet-4-6", "effort": "medium", "strengths": ["架构审查"] }
              ]
            }
          }
        }
        ```"#;

        let (roles, rationale) = parse_and_sanitize_ai_roles(raw, &existing, &probed).unwrap();
        assert_eq!(rationale.as_deref(), Some("Claude 负责架构，Codex 负责快速修复与审查"));

        let coder = &roles["coder"];
        // Uninstalled gemini was dropped; claude became primary; fast stripped for non-codex; strengths deduped.
        assert_eq!(coder.primary.agent, "claude");
        assert_eq!(coder.primary.model.as_deref(), Some("claude-opus-4-6"));
        assert!(!coder.primary.fast);
        assert_eq!(coder.primary.strengths, vec!["复杂架构", "后端重构"]);
        assert_eq!(coder.fallbacks.len(), 1);
        assert_eq!(coder.fallbacks[0].agent, "codex");
        // Hallucinated model was dropped to None rather than persisted.
        assert_eq!(coder.fallbacks[0].model, None);
        assert!(coder.fallbacks[0].fast);
        assert_eq!(coder.fallbacks[0].effort, Effort::Xhigh);
        assert_eq!(coder.fallbacks[0].strengths, vec!["快速修复", "算法性能"]);

        // Reviewer preserved canonical model casing & existing readonly access.
        let reviewer = &roles["reviewer"];
        assert_eq!(reviewer.access, Some(RoleAccessMode::Readonly));
        assert_eq!(reviewer.primary.agent, "codex");
        assert_eq!(reviewer.primary.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(reviewer.fallbacks.len(), 1);
        assert_eq!(reviewer.fallbacks[0].agent, "claude");

        // Unmentioned roles are preserved verbatim.
        assert!(roles.contains_key("tester"));
        assert!(roles.contains_key("writer"));
        assert!(roles.contains_key("researcher"));

        // Mode = Strengths locks agents/models and updates strengths only.
        let strengths_raw = r#"{
          "rationale": "只更新擅长任务",
          "roles": {
            "coder": {
              "candidates": [
                { "index": 0, "agent": "gemini", "model": "other", "strengths": ["底层内核", "并发安全"] },
                { "index": 1, "agent": "codex", "strengths": ["脚本批处理"] }
              ]
            }
          }
        }"#;
        let (s_roles, _) =
            parse_and_sanitize_ai_roles_for_mode(strengths_raw, &roles, &probed, AiRoleConfigMode::Strengths)
                .unwrap();
        assert_eq!(s_roles["coder"].primary.agent, "claude");
        assert_eq!(s_roles["coder"].primary.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(s_roles["coder"].primary.strengths, vec!["底层内核", "并发安全"]);
        assert_eq!(s_roles["coder"].fallbacks[0].agent, "codex");
        assert_eq!(s_roles["coder"].fallbacks[0].strengths, vec!["脚本批处理"]);

        // Mode = Models clamps xhigh -> high for 3-level models (claude-sonnet-4-6) and preserves existing strengths.
        let models_raw = r#"{
          "rationale": "换用 sonnet",
          "roles": {
            "coder": {
              "candidates": [
                { "agent": "claude", "model": "claude-sonnet-4-6", "effort": "xhigh", "strengths": ["会被忽略"] }
              ]
            }
          }
        }"#;
        let (m_roles, _) =
            parse_and_sanitize_ai_roles_for_mode(models_raw, &s_roles, &probed, AiRoleConfigMode::Models)
                .unwrap();
        assert_eq!(m_roles["coder"].primary.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(m_roles["coder"].primary.effort, Effort::High, "3-level model must clamp xhigh -> high");
        assert_eq!(
            m_roles["coder"].primary.strengths,
            vec!["底层内核", "并发安全"],
            "models-only mode must preserve existing strengths"
        );
    }
}
