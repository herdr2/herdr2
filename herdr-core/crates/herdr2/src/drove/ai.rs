//! Drove-specific AI hooks built on top of the shared Core AI runtime (`core.ai`).
//!
//! Uses shared immutable `HERDR2_CONTROL_PLANE_SYSTEM` prefixes, per-Run & per-Attempt
//! append-only `AiSessionPool` reuse, and input compaction to maximize KV-cache hit rates
//! while recording Drove Run token accounting and daemon events.

use super::config::{clamp_effort_for_model, normalize_strengths};
use super::store::{NewDaemonEvent, PostMessageInput};
use super::types::*;
use crate::ai::client::extract_json_object;
use crate::ai::compact::strip_and_dedup_lines;
use crate::ai::prompts::format_op_user_message;
use crate::ai::{AiCompleteRequest, AiConversationMode, AiTaskTier};
use crate::runtime::Core;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticLoopVerdict {
    pub looping: bool,
    pub confidence: f64,
    pub pattern: String,
    pub root_cause: String,
    pub steer_hint: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WriteConflictVerdict {
    pub conflict: bool,
    pub confidence: f64,
    pub reason: String,
}

/// Resolves the prompt language (`"zh"` or `"en"`) for internal Drove AI operations:
/// explicit per-request override > Core AI `language` (`ai.json`) > Drove `ai.language` (`drove.json`)
/// > local desktop app `settings.json` (`herdr2-app/settings.json`) > `"en"`.
pub(crate) fn drove_ai_lang(core: &Arc<Core>, explicit: Option<&str>) -> &'static str {
    if let Some(raw) = explicit
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto") && !s.eq_ignore_ascii_case("default"))
    {
        return crate::ai::prompt_lang_code(raw);
    }
    if let Some(lang) = core.ai.config_snapshot().language.as_deref() {
        return crate::ai::prompt_lang_code(lang);
    }
    if let Some(lang) = core.drove.config_snapshot().ai.language.as_deref() {
        return crate::ai::prompt_lang_code(lang);
    }
    core.ai.preferred_prompt_lang()
}

fn build_run_seed_context(core: &Arc<Core>, run_id: &str) -> Option<String> {
    let run = core.drove.store.get_run(run_id)?;
    let cfg = core.drove.config_snapshot();
    let roles: BTreeMap<String, String> = cfg
        .roles
        .iter()
        .map(|(k, r)| {
            let acc = format!("{:?}", r.access.unwrap_or(RoleAccessMode::Writable)).to_lowercase();
            (k.clone(), acc)
        })
        .collect();
    let mut seed = json!({
        "run_id": run.id,
        "cwd": run.cwd,
        "objective": run.objective.chars().take(400).collect::<String>(),
        "roles": roles,
    });
    if let Some(note) = cfg.note.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        seed["routing_note"] = json!(note);
    }
    Some(seed.to_string())
}

#[allow(clippy::too_many_arguments)]
async fn drove_complete(
    core: &Arc<Core>,
    run_id: &str,
    op: &str,
    tier: AiTaskTier,
    mode: AiConversationMode,
    user_prompt: String,
    max_tokens: u32,
    timeout: Duration,
    cache_key: Option<String>,
    cache_ttl: Option<Duration>,
) -> Option<String> {
    let req = AiCompleteRequest {
        op: op.to_string(),
        tier,
        mode,
        user_prompt,
        max_tokens,
        timeout,
        cache_key,
        cache_ttl,
        prompt_cache_bucket: Some(if run_id.trim().is_empty() { "cp".into() } else { format!("run:{run_id}") }),
        ..Default::default()
    };
    match core.ai.complete(req).await {
        Ok(resp) => {
            let in_tok = resp.usage.input_tokens;
            let out_tok = resp.usage.output_tokens;
            let cache_tok = resp.usage.cached_input_tokens;
            if !resp.from_local_cache && (in_tok > 0 || out_tok > 0 || cache_tok > 0) && !run_id.trim().is_empty() {
                let _ = core.drove.store.update_run(run_id, |r| {
                    let mut u = r.ai_usage.clone().unwrap_or_default();
                    u.calls += 1;
                    u.input_tokens += in_tok;
                    u.output_tokens += out_tok;
                    u.cache_hit_tokens += cache_tok;
                    r.ai_usage = Some(u);
                });
            }
            if !resp.from_local_cache {
                let tag = if resp.session_reused { " · session" } else { "" };
                core.drove.store.append_daemon_event(NewDaemonEvent {
                    run_id: (!run_id.trim().is_empty()).then(|| run_id.to_string()),
                    task_id: None,
                    attempt_id: None,
                    worker_id: None,
                    message_id: None,
                    category: "ai".into(),
                    kind: op.into(),
                    status: "ok".into(),
                    title: format!("AI {op} ({}{tag})", resp.model),
                    detail: None,
                    duration_ms: Some(resp.latency_ms),
                    tokens: Some(DaemonEventTokens { input: in_tok, output: out_tok, cache_hit: cache_tok }),
                    cost: None,
                });
            }
            Some(resp.content)
        }
        Err(reason) => {
            if !run_id.trim().is_empty() && !reason.contains("disabled") && !reason.contains("not configured") {
                core.drove.store.append_daemon_event(NewDaemonEvent {
                    run_id: Some(run_id.to_string()),
                    task_id: None,
                    attempt_id: None,
                    worker_id: None,
                    message_id: None,
                    category: "ai".into(),
                    kind: op.into(),
                    status: "warn".into(),
                    title: format!("AI {op} fallback"),
                    detail: Some(reason.chars().take(160).collect()),
                    duration_ms: None,
                    tokens: None,
                    cost: None,
                });
            }
            None
        }
    }
}

/// Generates a short human title for a newly created Run when `run.name` is unset.
pub fn spawn_auto_title(core: Arc<Core>, run_id: String, objective: String) {
    let cfg = core.drove.config_snapshot();
    if !cfg.ai.auto_title || !core.ai.is_configured() {
        return;
    }
    tokio::spawn(async move {
        let lang = drove_ai_lang(&core, None);
        let user_prompt = format_op_user_message("auto_title", lang, &json!({ "objective": objective }));
        let Some(title) = drove_complete(
            &core,
            &run_id,
            "auto_title",
            AiTaskTier::Nano,
            AiConversationMode::StatelessSharedPrefix,
            user_prompt,
            32,
            Duration::from_secs(10),
            None,
            None,
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

fn run_seeded_mode(seed: Option<String>) -> AiConversationMode {
    match seed.filter(|s| !s.trim().is_empty()) {
        Some(seed_context) => AiConversationMode::SeededStateless { seed_context },
        None => AiConversationMode::StatelessSharedPrefix,
    }
}

/// Generates a short diagnostic summary on failed verification output.
pub fn spawn_verify_digest(core: Arc<Core>, run_id: String, attempt_id: String, cmd_str: String, log_tail: String) {
    let cfg = core.drove.config_snapshot();
    if !cfg.ai.verify_digest || !core.ai.is_configured() {
        return;
    }
    tokio::spawn(async move {
        let compact_tail = strip_and_dedup_lines(&log_tail, 60, 220, 2400);
        let lang = drove_ai_lang(&core, None);
        let user_prompt =
            format_op_user_message("verify_digest", lang, &json!({ "command": cmd_str, "tail": compact_tail }));
        let seed = build_run_seed_context(&core, &run_id);
        let Some(digest) = drove_complete(
            &core,
            &run_id,
            "verify_digest",
            AiTaskTier::Fast,
            run_seeded_mode(seed),
            user_prompt,
            160,
            Duration::from_secs(12),
            None,
            None,
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
    if cfg.ai.question_mode == "off" || !core.ai.is_configured() {
        return;
    }
    let auto_mode = matches!(cfg.ai.question_mode.as_str(), "auto" | "auto_when_offline");
    tokio::spawn(async move {
        let lang = drove_ai_lang(&core, None);
        let user_prompt = format_op_user_message(
            "question_draft",
            lang,
            &json!({
                "question": question,
                "options": options.unwrap_or_default(),
            }),
        );
        let seed = build_run_seed_context(&core, &run_id);
        let Some(raw) = drove_complete(
            &core,
            &run_id,
            "question_draft",
            AiTaskTier::Fast,
            run_seeded_mode(seed),
            user_prompt,
            160,
            Duration::from_secs(12),
            None,
            None,
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

/// Compresses a long worker completion summary into a 1-sentence `compactSummary` for downstream handovers.
pub fn spawn_handover_compact(core: Arc<Core>, run_id: String, task_id: String, attempt_id: String, summary: String) {
    let cfg = core.drove.config_snapshot();
    if !cfg.ai.handover_compact || !core.ai.is_configured() || summary.chars().count() <= 140 {
        return;
    }
    tokio::spawn(async move {
        let lang = drove_ai_lang(&core, None);
        let user_prompt = format_op_user_message("handover_compact", lang, &json!({ "summary": summary }));
        let Some(compact) = drove_complete(
            &core,
            &run_id,
            "handover_compact",
            AiTaskTier::Fast,
            AiConversationMode::StatelessSharedPrefix,
            user_prompt,
            160,
            Duration::from_secs(12),
            None,
            None,
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
    if !cfg.ai.task_enrich || !core.ai.is_configured() {
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
        let lang = drove_ai_lang(&core, None);
        let user_prompt = format_op_user_message(
            "task_enrich",
            lang,
            &json!({
                "key": task.key,
                "title": task.title,
                "target": task.spec.target,
                "change": task.spec.change.chars().take(1200).collect::<String>(),
                "acceptance": task.spec.acceptance,
            }),
        );
        let seed = build_run_seed_context(&core, &run_id);
        let Some(raw) = drove_complete(
            &core,
            &run_id,
            "task_enrich",
            AiTaskTier::Fast,
            run_seeded_mode(seed),
            user_prompt,
            200,
            Duration::from_secs(12),
            None,
            None,
        )
        .await
        else {
            return;
        };
        let Some(parsed) = extract_json_object(&raw) else {
            core.ai.record_parse_error("task_enrich");
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
        let extracted_acc = parsed["acceptance"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
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
pub async fn generate_task_diagnosis(core: &Arc<Core>, diag: &mut Diagnosis, refresh: bool, language: Option<&str>) {
    if !core.ai.is_configured() {
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
    let fail_txt =
        diag.failure.as_ref().map(|f| format!("{:?}: {}", f.kind, f.message)).unwrap_or_else(|| "none".into());
    let recent_txt =
        diag.last_items.iter().rev().take(6).filter_map(|it| it.text.as_deref()).collect::<Vec<_>>().join("\n---\n");
    let lang = drove_ai_lang(core, language);
    let user_prompt = format_op_user_message(
        "task_diagnose",
        lang,
        &json!({
            "task_key": diag.task.key,
            "title": diag.task.title,
            "status": format!("{:?}", diag.task.status),
            "attempt_id": att_id,
            "agent": att.agent,
            "model": att.model,
            "failure": fail_txt,
            "suggestion": diag.suggestion,
            "recent_output": strip_and_dedup_lines(&recent_txt, 50, 220, 2400),
        }),
    );
    let seed = build_run_seed_context(core, &run_id);
    let Some(raw) = drove_complete(
        core,
        &run_id,
        "task_diagnose",
        AiTaskTier::Standard,
        run_seeded_mode(seed),
        user_prompt,
        512,
        Duration::from_secs(16),
        None,
        None,
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
    let root_cause =
        parsed["rootCause"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(&diag.suggestion).to_string();
    let evidence = parsed["evidence"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
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

/// Builds the structured `route_candidate` input payload including local wall-clock time,
/// global `routing_note`, per-candidate `note`, and deterministic `strength_score`.
pub(crate) fn build_route_candidate_payload(
    cfg: &DroveConfig,
    task: &Task,
    role_name: &str,
    pool: &[(usize, RoleTarget)],
    local_time: &str,
) -> Value {
    let cands_desc: Vec<Value> = pool
        .iter()
        .enumerate()
        .map(|(pos, (_, c))| {
            let (strength_score, _) = super::router::score_candidate_strengths(c, task);
            let mut obj = json!({
                "index": pos,
                "agent": c.agent,
                "model": c.model.as_deref().unwrap_or("default"),
                "effort": format!("{:?}", c.effort).to_lowercase(),
                "strengths": c.strengths,
                "strength_score": strength_score,
            });
            if let Some(note) = c.note.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                obj["note"] = json!(note);
            }
            obj
        })
        .collect();
    let mut payload = json!({
        "role": role_name,
        "current_local_time": local_time,
        "task": {
            "key": task.key,
            "title": task.title,
            "tags": task.tags,
            "files": task.files.iter().take(16).collect::<Vec<_>>(),
            "target": task.spec.target,
            "change": task.spec.change.chars().take(500).collect::<String>(),
        },
        "candidates": cands_desc,
    });
    if let Some(note) = cfg.note.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload["routing_note"] = json!(note);
    }
    payload
}

/// Optional fast AI candidate selector when a role has routing notes configured or multiple
/// candidates with `strengths` where deterministic keyword matching had no unique winner.
pub async fn pick_role_candidate(
    core: &Arc<Core>,
    run_id: &str,
    task: &Task,
    role_name: &str,
    pool: &[(usize, RoleTarget)],
) -> Option<(usize, String)> {
    if !core.ai.is_configured() || core.ai.is_circuit_open() || pool.len() < 2 {
        return None;
    }

    let cfg = core.drove.config_snapshot();
    let local_time = now_local_time_context();
    let payload = build_route_candidate_payload(&cfg, task, role_name, pool, &local_time);
    let lang = drove_ai_lang(core, None);
    let user_prompt = format_op_user_message("route_candidate", lang, &payload);
    let seed = build_run_seed_context(core, run_id);
    let raw = tokio::time::timeout(
        Duration::from_secs(6),
        drove_complete(
            core,
            run_id,
            "route_candidate",
            AiTaskTier::Nano,
            run_seeded_mode(seed),
            user_prompt,
            96,
            Duration::from_secs(6),
            None,
            None,
        ),
    )
    .await
    .ok()??;

    let Some(parsed) = extract_json_object(&raw) else {
        core.ai.record_parse_error("route_candidate");
        return None;
    };
    let pos = parsed["index"].as_u64()? as usize;
    let &(chain_idx, _) = pool.get(pos)?;
    let reason: String =
        parsed["reason"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("ai").chars().take(40).collect();
    Some((chain_idx, reason))
}

/// Incremental multi-turn semantic loop / A-B oscillation audit for a running Drove attempt.
/// Uses a dedicated per-attempt `AiSessionPool` (`drove:loop:<attempt_id>`) so only NEW delta items
/// are transmitted on each audit step while preserving full prior progression in KV cache.
pub async fn detect_semantic_loop(
    core: &Arc<Core>,
    run_id: &str,
    attempt_id: &str,
    task: &Task,
    delta_items: &[Item],
) -> Option<SemanticLoopVerdict> {
    if !core.ai.is_configured() || core.ai.is_circuit_open() || delta_items.is_empty() {
        return None;
    }

    let compact_delta: Vec<Value> = delta_items
        .iter()
        .map(|it| {
            if let Some(tl) = &it.tool {
                let out_tail = tl.output.as_deref().map(|s| strip_and_dedup_lines(s, 6, 160, 220)).unwrap_or_default();
                json!({
                    "id": it.id,
                    "kind": "tool",
                    "tool_kind": tl.kind,
                    "title": tl.title,
                    "status": tl.status,
                    "locations": tl.locations,
                    "output_tail": out_tail,
                })
            } else {
                let text = it.text.as_deref().map(|s| strip_and_dedup_lines(s, 4, 160, 220)).unwrap_or_default();
                json!({
                    "id": it.id,
                    "kind": it.kind,
                    "text": text,
                })
            }
        })
        .collect();

    let seed_context = json!({
        "task_key": task.key,
        "title": task.title,
        "target": task.spec.target,
        "files": task.files,
        "change": task.spec.change.chars().take(500).collect::<String>(),
    })
    .to_string();

    let lang = drove_ai_lang(core, None);
    let user_prompt = format_op_user_message("loop_detect", lang, &json!({ "delta_items": compact_delta }));

    let raw = tokio::time::timeout(
        Duration::from_secs(7),
        drove_complete(
            core,
            run_id,
            "loop_detect",
            AiTaskTier::Standard,
            AiConversationMode::Session { key: format!("drove:loop:{attempt_id}"), seed_context: Some(seed_context) },
            user_prompt,
            220,
            Duration::from_secs(6),
            None,
            None,
        ),
    )
    .await
    .ok()??;

    let parsed = extract_json_object(&raw)?;
    let looping = parsed["looping"].as_bool().unwrap_or(false);
    let confidence = parsed["confidence"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
    let pattern =
        parsed["pattern"].as_str().map(str::trim).unwrap_or("semantic_oscillation").chars().take(80).collect();
    let root_cause = parsed["root_cause"].as_str().map(str::trim).unwrap_or("").chars().take(180).collect();
    let steer_hint = parsed["steer_hint"].as_str().map(str::trim).unwrap_or("").chars().take(260).collect();

    Some(SemanticLoopVerdict { looping, confidence, pattern, root_cause, steer_hint })
}

/// Fast semantic conflict check between a candidate writable task and concurrent peer writers
/// when deterministic file/scope disjointness could not decide. Reuses the Run's KV-cached session.
pub async fn assess_write_conflict(
    core: &Arc<Core>,
    run_id: &str,
    candidate: &Task,
    peers: &[&Task],
) -> Option<WriteConflictVerdict> {
    if !core.ai.is_configured() || core.ai.is_circuit_open() || peers.is_empty() {
        return None;
    }

    let brief = |t: &Task| {
        json!({
            "key": t.key,
            "role": t.assign.as_ref().and_then(|a| a.role.as_deref()).unwrap_or("coder"),
            "title": t.title,
            "target": t.spec.target,
            "files": t.files.iter().take(12).collect::<Vec<_>>(),
            "change": t.spec.change.chars().take(300).collect::<String>(),
        })
    };
    let peer_vals: Vec<Value> = peers.iter().map(|p| brief(p)).collect();
    let lang = drove_ai_lang(core, None);
    let user_prompt = format_op_user_message(
        "write_conflict",
        lang,
        &json!({
            "candidate": brief(candidate),
            "peers": peer_vals,
        }),
    );
    let seed = build_run_seed_context(core, run_id);

    let raw = tokio::time::timeout(
        Duration::from_secs(5),
        drove_complete(
            core,
            run_id,
            "write_conflict",
            AiTaskTier::Nano,
            run_seeded_mode(seed),
            user_prompt,
            96,
            Duration::from_secs(4),
            None,
            None,
        ),
    )
    .await
    .ok()??;

    let Some(parsed) = extract_json_object(&raw) else {
        core.ai.record_parse_error("write_conflict");
        return None;
    };
    let conflict = parsed["conflict"].as_bool().unwrap_or(true);
    let confidence = parsed["confidence"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
    let reason = parsed["reason"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("semantic check")
        .chars()
        .take(80)
        .collect();
    Some(WriteConflictVerdict { conflict, confidence, reason })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AiRoleConfigMode {
    Models,
    Strengths,
    Notes,
    All,
}

impl AiRoleConfigMode {
    fn from_opt(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("models" | "roles" | "agents") => Self::Models,
            Some("strengths" | "tasks" | "specialties") => Self::Strengths,
            Some("notes" | "note") => Self::Notes,
            _ => Self::All,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AiNoteScope {
    All,
    Global,
    Candidate,
}

impl AiNoteScope {
    fn from_params(raw: Option<&str>, target_role: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("global") => Self::Global,
            Some("candidate" | "single") => Self::Candidate,
            Some("all") => Self::All,
            _ if target_role.map(str::trim).is_some_and(|s| !s.is_empty()) => Self::Candidate,
            _ => Self::All,
        }
    }
}

/// Uses the configured AI Assistant LLM to automatically configure Drove roles and routing notes.
pub async fn configure_roles_with_ai(
    core: &Arc<Core>,
    params: DroveRolesAiConfigureParams,
) -> DroveRolesAiConfigureResult {
    let started = Instant::now();
    let mode = AiRoleConfigMode::from_opt(params.mode.as_deref());
    let note_scope = AiNoteScope::from_params(params.note_scope.as_deref(), params.target_role.as_deref());
    let is_single_candidate_strengths = mode == AiRoleConfigMode::Strengths
        && params.target_role.as_deref().map(str::trim).is_some_and(|s| !s.is_empty());
    let is_single_target =
        (mode == AiRoleConfigMode::Notes && note_scope != AiNoteScope::All) || is_single_candidate_strengths;
    let fail = |msg: String, probed_refreshed: bool| DroveRolesAiConfigureResult {
        ok: false,
        applied: false,
        probed_refreshed,
        model_used: None,
        latency_ms: started.elapsed().as_millis() as u64,
        rationale: None,
        note: None,
        strengths: None,
        roles: None,
        settings: None,
        error: Some(msg),
    };

    let cfg = core.drove.config_snapshot();
    let ai_cfg = core.ai.config_snapshot();
    let raw_base =
        params.base_url.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(ai_cfg.base_url.trim());
    if raw_base.is_empty() {
        return fail("请先在「AI 设置」中配置远端服务地址（Service URL）".into(), false);
    }
    let (stored_key, _) = core.ai.read_key();
    let has_key = params.api_key.as_deref().map(str::trim).is_some_and(|s| !s.is_empty()) || stored_key.is_some();
    if !has_key && !crate::ai::allows_keyless_url(raw_base) {
        return fail("请先在「AI 设置」中配置 API Key".into(), false);
    }

    // Gather agent & model inventory; refresh installed flags first.
    // Single-target generation (`global`/`candidate` note or single-candidate strengths) inspects current roles & quotas, so skip slow full probe.
    let mut probed = core.drove.cached_probe();
    let mut probed_refreshed = false;
    if !is_single_target {
        if !probed.agents.is_empty() {
            let _ = super::probe::refresh_installed_status(core, &mut probed).await;
        }
        let has_cached_models = probed.full_probe
            && !probed.agents.is_empty()
            && probed.agents.iter().any(|a| a.installed && !a.models.is_empty());
        if !has_cached_models && let Ok(full) = core.drove.rpc_probe(true).await {
            probed = full;
            probed_refreshed = true;
        }
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
            if let Some(err) = a.error.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                obj["error"] = json!(err);
            }
            obj
        })
        .collect();

    let quotas = if cfg.magpie.enabled { core.drove.magpie.fetch_if_enabled(&cfg.magpie, false).await.0 } else { None };
    let quota_summary: Vec<Value> = quotas
        .unwrap_or_default()
        .into_iter()
        .map(|q| {
            let min_rem = q.windows.as_ref().and_then(|ws| ws.iter().map(|w| w.remaining).reduce(f64::min));
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
                        "note": c.note,
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
    let prefer_zh = match params.language.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(l) => crate::ai::prompt_lang_code(l) == "zh",
        None => {
            drove_ai_lang(core, None) == "zh" || instruction.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c))
        }
    };

    let lang_rule = if prefer_zh {
        "Write `rationale`, any `note` fields, and any `strengths` tags in concise Simplified Chinese (2-6 chars per strength tag, e.g. \"复杂架构\", \"前端UI\", \"后端重构\", \"疑难排障\", \"快速修复\", \"单元测试\", \"安全审计\", \"源码调研\", \"长上下文\")."
    } else {
        "Write `rationale`, any `note` fields, and any `strengths` tags in concise English (2-3 words per strength tag, e.g. \"Backend Architecture\", \"Frontend UI\", \"Complex Refactor\", \"Bugfix & Debug\", \"Unit Tests\", \"Security Review\", \"Codebase Research\")."
    };

    // Validate candidate target up front for single-candidate note or strengths generation.
    let candidate_target_info: Option<(String, usize, Value)> = if (mode == AiRoleConfigMode::Notes
        && note_scope == AiNoteScope::Candidate)
        || is_single_candidate_strengths
    {
        let role_id = params.target_role.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let Some(rid) = role_id else {
            return fail("缺少 targetRole 参数，无法生成单候选配置".into(), probed_refreshed);
        };
        let Some(role_cfg) = cfg.roles.get(rid) else {
            return fail(format!("未找到角色 `{rid}`"), probed_refreshed);
        };
        let cand_idx = params.target_candidate_index.unwrap_or(0);
        let chain: Vec<&RoleTarget> = std::iter::once(&role_cfg.primary).chain(role_cfg.fallbacks.iter()).collect();
        let Some(target) = chain.get(cand_idx) else {
            return fail(format!("角色 `{rid}` 中不存在候选 #{cand_idx}"), probed_refreshed);
        };
        let siblings: Vec<Value> = chain
            .iter()
            .enumerate()
            .map(|(i, c)| {
                json!({
                    "index": i,
                    "isTarget": i == cand_idx,
                    "agent": c.agent,
                    "model": c.model,
                    "effort": c.effort,
                    "strengths": c.strengths,
                    "note": c.note,
                })
            })
            .collect();
        Some((
            rid.to_string(),
            cand_idx,
            json!({
                "roleId": rid,
                "roleLabel": role_cfg.label,
                "roleSummary": role_cfg.summary,
                "roleAccess": role_cfg.access,
                "candidateIndex": cand_idx,
                "agent": target.agent,
                "model": target.model,
                "effort": target.effort,
                "fast": target.fast,
                "context": target.context,
                "strengths": target.strengths,
                "currentNote": target.note,
                "siblings": siblings,
            }),
        ))
    } else {
        None
    };

    let system_prompt = match (mode, note_scope) {
        (AiRoleConfigMode::Models, _) => format!(
            "You are a pure JSON configuration architect for Drove (a multi-agent software engineering orchestrator). \
             Configure ONLY the `agent`, `model`, `effort`, `fast`, and `context` of 1 to 3 complementary `candidates` for every role in `roles`.\n\
             Rules:\n\
             1. ONLY choose `agent` values from `availableAgents[].id`, and avoid agents with a non-empty `error` field whenever healthy agents without `error` are available.\n\
             2. For each candidate's `model`, ONLY choose an exact string from that agent's `models` array, or `null` to use the agent's default.\n\
             3. `effort` must be supported by that model (`low`, `medium`, `high`, or `xhigh` ONLY for models supporting extra-high reasoning).\n\
             4. `fast` may be `true` ONLY for `codex` (otherwise `false`). `context` may be set ONLY for `cursor`/`cursor-agent` (otherwise `null`).\n\
             5. Prefer a different primary agent for `reviewer` than `coder` when multiple agents are available.\n\
             6. If `quotas` shows a provider at 0% remaining with no balance, avoid putting it as candidate #1.\n\
             7. Strictly honor `userPreference` if non-empty.\n\
             8. {lang_rule}\n\
             9. Output ONLY valid JSON matching:\n\
             {{\"rationale\":\"1-2 sentences\",\"roles\":{{\"<roleId>\":{{\"candidates\":[{{\"agent\":\"...\",\"model\":\"...\"|null,\"effort\":\"high\",\"fast\":false,\"context\":null}}]}}}}}}"
        ),
        (AiRoleConfigMode::Strengths, _) if is_single_candidate_strengths => format!(
            "You are a pure JSON configuration architect for Drove (a multi-agent software engineering orchestrator). \
             Configure ONLY the `strengths` array (2-4 concise, distinct work-type tags) specifically for `targetCandidate` (`agent` + `model` + `effort`) inside its role.\n\
             Rules:\n\
             1. Match `targetCandidate`'s real capabilities and differentiate its `strengths` cleanly from `targetCandidate.siblings` when siblings exist.\n\
             2. Consider `targetCandidate.roleLabel`, `roleSummary`, `roleAccess`, and `currentNote` if present.\n\
             3. Strictly honor `userPreference` if non-empty.\n\
             4. {lang_rule}\n\
             5. Output ONLY valid JSON matching:\n\
             {{\"rationale\":\"1 short sentence\",\"strengths\":[\"...\",\"...\"]}}"
        ),
        (AiRoleConfigMode::Strengths, _) => format!(
            "You are a pure JSON configuration architect for Drove (a multi-agent software engineering orchestrator). \
             Each role in `roles` already has its `currentCandidates` (`index`, `agent`, `model`, `effort`). \
             Do NOT change, add, or remove any candidate's `agent` or `model`; configure ONLY the `strengths` array (2-4 distinct work-type tags) for EVERY existing candidate in EVERY role.\n\
             Rules:\n\
             1. Return an entry for EVERY role in `roles` and EVERY candidate index `0..currentCandidates.length`.\n\
             2. Within a role that has multiple candidates, assign complementary, clearly differentiated `strengths` matching each agent+model's real capabilities.\n\
             3. Strictly honor `userPreference` if non-empty.\n\
             4. {lang_rule}\n\
             5. Output ONLY valid JSON matching:\n\
             {{\"rationale\":\"1-2 sentences\",\"roles\":{{\"<roleId>\":{{\"candidates\":[{{\"index\":0,\"agent\":\"...\",\"model\":\"...\"|null,\"strengths\":[\"...\",\"...\"]}}]}}}}}}"
        ),
        (AiRoleConfigMode::Notes, AiNoteScope::Global) => format!(
            "You are a pure JSON configuration architect for Drove (a multi-agent software engineering orchestrator). \
             Write ONE concise, practical global routing `note` (1-3 short sentences, <= 240 chars) that the AI sub-agent router will read before launching workers across all roles.\n\
             Rules:\n\
             1. Base the note on the configured `roles`, models/providers, `quotas`, and existing `routingNote`.\n\
             2. Include actionable selection policies: e.g., when to prefer flagship vs fast/lightweight models, heterogeneous reviewer preference, fallback/quota protection rules, and any time-window or cost constraints.\n\
             3. If `userPreference` is non-empty (such as a rough draft or explicit rule), preserve and refine all user-specified constraints without dropping any rule.\n\
             4. Do NOT invent fake time-window pricing rules unless mentioned in `userPreference` or `routingNote`.\n\
             5. {lang_rule}\n\
             6. Output ONLY valid JSON matching:\n\
             {{\"rationale\":\"1 short sentence\",\"note\":\"concise actionable routing note\"}}"
        ),
        (AiRoleConfigMode::Notes, AiNoteScope::Candidate) => format!(
            "You are a pure JSON configuration architect for Drove (a multi-agent software engineering orchestrator). \
             Write ONE concise, actionable routing `note` (1 short sentence, <= 120 chars) specifically for `targetCandidate`, which the AI sub-agent router will read when choosing between sibling candidates in that role.\n\
             Rules:\n\
             1. Focus on when to choose or avoid this specific `agent` + `model` relative to `targetCandidate.siblings` (e.g., preferred task scope, speed/cost/context trade-off, fallback role, or quota caution).\n\
             2. Respect the global `routingNote` if present, without merely repeating it verbatim.\n\
             3. If `userPreference` is non-empty (such as a rough draft or explicit rule), preserve and polish all user-specified constraints.\n\
             4. {lang_rule}\n\
             5. Output ONLY valid JSON matching:\n\
             {{\"rationale\":\"1 short sentence\",\"note\":\"concise candidate routing note\"}}"
        ),
        (AiRoleConfigMode::Notes, AiNoteScope::All) => format!(
            "You are a pure JSON configuration architect for Drove (a multi-agent software engineering orchestrator). \
             Each role in `roles` already has its `currentCandidates` (`index`, `agent`, `model`, `effort`, `strengths`). \
             Do NOT change, add, or remove any candidate's `agent`, `model`, `effort`, or `strengths`; configure ONLY:\n\
             - top-level global routing `note` (1-3 concise sentences, <= 240 chars, cross-role selection/fallback/quota policy), AND\n\
             - per-candidate `note` (1 crisp sentence, <= 100 chars) for EVERY existing candidate in EVERY role explaining when the AI router should prefer or avoid that candidate.\n\
             Rules:\n\
             1. Return top-level `note` and an entry for EVERY role in `roles` and EVERY candidate index `0..currentCandidates.length`.\n\
             2. Preserve any existing user constraints in `routingNote` / candidate `note` and strictly honor `userPreference` if non-empty.\n\
             3. Do NOT fabricate time-window pricing rules unless mentioned in `userPreference` or existing notes.\n\
             4. {lang_rule}\n\
             5. Output ONLY valid JSON matching:\n\
             {{\"rationale\":\"1-2 sentences\",\"note\":\"global routing note\",\"roles\":{{\"<roleId>\":{{\"candidates\":[{{\"index\":0,\"agent\":\"...\",\"model\":\"...\"|null,\"note\":\"concise candidate note\"}}]}}}}}}"
        ),
        (AiRoleConfigMode::All, _) => format!(
            "You are a pure JSON configuration architect for Drove (a multi-agent software engineering orchestrator). \
             Configure every role in `roles` with 1 to 3 complementary `candidates` (`agent` + `model` + `effort` + `fast` + `context` + `strengths` + `note`), AND a top-level global routing `note` for AI sub-agent selection.\n\
             Rules:\n\
             1. ONLY choose `agent` values from `availableAgents[].id`, and avoid agents with a non-empty `error` field whenever healthy agents without `error` are available.\n\
             2. For each candidate's `model`, ONLY choose an exact string from that agent's `models` array, or `null` to use the agent's default.\n\
             3. `effort` must be supported by that model (`low`, `medium`, `high`, or `xhigh` ONLY for models supporting extra-high reasoning).\n\
             4. `fast` may be `true` ONLY for `codex` (otherwise `false`). `context` may be set ONLY for `cursor`/`cursor-agent` (otherwise `null`).\n\
             5. Within roles that benefit from specialization, when multiple agents/models are installed, assign 2-3 complementary candidates with distinct `strengths` (2-4 tags each).\n\
             6. Prefer a different primary agent for `reviewer` than `coder` when multiple agents are available.\n\
             7. If `quotas` shows a provider at 0% remaining with no balance, avoid putting it as candidate #1.\n\
             8. Write a concise top-level global routing `note` (1-3 short sentences, <= 240 chars) and a concise per-candidate `note` (<= 100 chars) for each candidate describing when AI should pick or avoid it (preserve any existing constraints in `routingNote` or `userPreference`; do not invent fake time-window rules).\n\
             9. Strictly honor `userPreference` if non-empty.\n\
             10. {lang_rule}\n\
             11. Output ONLY valid JSON matching:\n\
             {{\"rationale\":\"1-2 sentences\",\"note\":\"global routing note\",\"roles\":{{\"<roleId>\":{{\"candidates\":[{{\"agent\":\"...\",\"model\":\"...\"|null,\"effort\":\"high\",\"fast\":false,\"context\":null,\"strengths\":[\"...\",\"...\"],\"note\":\"concise candidate note\"}}]}}}}}}"
        ),
    };

    let mut payload_val = json!({
        "mode": match mode {
            AiRoleConfigMode::Models => "models",
            AiRoleConfigMode::Strengths => "strengths",
            AiRoleConfigMode::Notes => "notes",
            AiRoleConfigMode::All => "all",
        },
        "noteScope": match note_scope {
            AiNoteScope::All => "all",
            AiNoteScope::Global => "global",
            AiNoteScope::Candidate => "candidate",
        },
        "roles": roles_desc,
        "routingNote": cfg.note,
        "availableAgents": agents_inventory,
        "quotas": quota_summary,
        "userPreference": instruction,
    });
    if let Some((_, _, ref target_val)) = candidate_target_info {
        payload_val["targetCandidate"] = target_val.clone();
    }
    let user_payload = payload_val.to_string();
    let max_tokens = if is_single_target { 512 } else { 2560 };

    let resp = match core
        .ai
        .complete(AiCompleteRequest {
            op: "roles_configure".into(),
            tier: AiTaskTier::Standard,
            mode: AiConversationMode::CustomSystem(system_prompt),
            user_prompt: format!(
                "Do NOT execute tasks or call tools. Return ONLY the JSON configuration for the input below:\n<input>\n{user_payload}\n</input>"
            ),
            max_tokens,
            timeout: Duration::from_secs(60),
            url_override: params.base_url,
            key_override: params.api_key,
            model_override: params.model,
            ..Default::default()
        })
        .await
    {
        Ok(r) => r,
        Err(e) => return fail(e, probed_refreshed),
    };

    let should_apply = params.apply.unwrap_or(true);

    // Fast path: single global note or single candidate note generation.
    if mode == AiRoleConfigMode::Notes && note_scope != AiNoteScope::All {
        let max_len = if note_scope == AiNoteScope::Global { 1000 } else { 400 };
        let (generated_note, rationale) = match parse_single_ai_note(&resp.content, max_len) {
            Ok(v) => v,
            Err(e) => return fail(e, probed_refreshed),
        };
        let mut updated_roles: Option<BTreeMap<String, RoleConfig>> = None;
        let settings = if should_apply {
            let patch = if let Some((ref rid, cand_idx, _)) = candidate_target_info {
                let mut next_roles = cfg.roles.clone();
                if let Some(role) = next_roles.get_mut(rid) {
                    if cand_idx == 0 {
                        role.primary.note = Some(generated_note.clone());
                    } else if let Some(fb) = role.fallbacks.get_mut(cand_idx - 1) {
                        fb.note = Some(generated_note.clone());
                    }
                }
                updated_roles = Some(next_roles.clone());
                DroveSettingsSetParams { roles: Some(next_roles), ..Default::default() }
            } else {
                DroveSettingsSetParams { note: Some(generated_note.clone()), ..Default::default() }
            };
            match core.drove.rpc_settings_set(patch).await {
                Ok(s) => Some(s),
                Err(e) => return fail(format!("保存备注失败: {}", e.message), probed_refreshed),
            }
        } else {
            None
        };
        return DroveRolesAiConfigureResult {
            ok: true,
            applied: should_apply,
            probed_refreshed,
            model_used: Some(resp.model),
            latency_ms: started.elapsed().as_millis() as u64,
            rationale,
            note: Some(generated_note),
            strengths: None,
            roles: updated_roles,
            settings,
            error: None,
        };
    }

    // Fast path: single-candidate task strengths generation.
    if is_single_candidate_strengths {
        let (generated_strengths, rationale) = match parse_single_ai_strengths(&resp.content) {
            Ok(v) => v,
            Err(e) => return fail(e, probed_refreshed),
        };
        let mut updated_roles: Option<BTreeMap<String, RoleConfig>> = None;
        let settings = if should_apply {
            let Some((ref rid, cand_idx, _)) = candidate_target_info else {
                return fail("缺少单候选定位信息".into(), probed_refreshed);
            };
            let mut next_roles = cfg.roles.clone();
            if let Some(role) = next_roles.get_mut(rid) {
                if cand_idx == 0 {
                    role.primary.strengths = generated_strengths.clone();
                } else if let Some(fb) = role.fallbacks.get_mut(cand_idx - 1) {
                    fb.strengths = generated_strengths.clone();
                }
            }
            updated_roles = Some(next_roles.clone());
            match core
                .drove
                .rpc_settings_set(DroveSettingsSetParams { roles: Some(next_roles), ..Default::default() })
                .await
            {
                Ok(s) => Some(s),
                Err(e) => return fail(format!("保存擅长任务失败: {}", e.message), probed_refreshed),
            }
        } else {
            None
        };
        return DroveRolesAiConfigureResult {
            ok: true,
            applied: should_apply,
            probed_refreshed,
            model_used: Some(resp.model),
            latency_ms: started.elapsed().as_millis() as u64,
            rationale,
            note: None,
            strengths: Some(generated_strengths),
            roles: updated_roles,
            settings,
            error: None,
        };
    }

    let (next_roles, next_note, rationale) =
        match parse_and_sanitize_ai_roles_with_note(&resp.content, &cfg.roles, &probed, mode) {
            Ok(res) => res,
            Err(e) => return fail(e, probed_refreshed),
        };

    let settings = if should_apply {
        let patch = DroveSettingsSetParams {
            roles: Some(next_roles.clone()),
            note: if matches!(mode, AiRoleConfigMode::All | AiRoleConfigMode::Notes) {
                next_note.clone()
            } else {
                None
            },
            ..Default::default()
        };
        match core.drove.rpc_settings_set(patch).await {
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
        model_used: Some(resp.model),
        latency_ms: started.elapsed().as_millis() as u64,
        rationale,
        note: next_note,
        strengths: None,
        roles: Some(next_roles),
        settings,
        error: None,
    }
}

pub(crate) fn parse_single_ai_strengths(raw: &str) -> Result<(Vec<String>, Option<String>), String> {
    let parsed = extract_json_object(raw)
        .ok_or_else(|| format!("AI 未返回有效的 JSON 擅长任务: {}", raw.chars().take(160).collect::<String>()))?;
    let raw_list: Vec<String> = parsed
        .get("strengths")
        .or_else(|| parsed.get("tags"))
        .or_else(|| parsed.get("tasks"))
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let strengths = normalize_strengths(&raw_list);
    if strengths.is_empty() {
        return Err("AI 未生成有效的擅长任务标签".to_string());
    }
    let rationale = parsed
        .get("rationale")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(400).collect::<String>());
    Ok((strengths, rationale))
}

pub(crate) fn parse_single_ai_note(raw: &str, max_chars: usize) -> Result<(String, Option<String>), String> {
    let parsed = extract_json_object(raw)
        .ok_or_else(|| format!("AI 未返回有效的 JSON 备注: {}", raw.chars().take(160).collect::<String>()))?;
    let note = super::config::normalize_note(
        parsed
            .get("note")
            .or_else(|| parsed.get("routingNote"))
            .or_else(|| parsed.get("routing_note"))
            .and_then(Value::as_str),
        max_chars,
    )
    .ok_or_else(|| "AI 未生成有效的备注内容".to_string())?;
    let rationale = parsed
        .get("rationale")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(400).collect::<String>());
    Ok((note, rationale))
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

fn companion_cli_name(agent_id: &str) -> &'static str {
    match agent_id {
        "agy" | "antigravity" => "agy",
        "claude" => "claude",
        "codex" => "codex",
        "cursor" | "cursor-agent" => "cursor-agent",
        "gemini" => "gemini",
        "opencode" => "opencode",
        "copilot" => "copilot",
        "dsh" => "dsh-tui",
        "kimi" => "kimi",
        "qwen" => "qwen",
        "pi" => "pi",
        "hermes" => "hermes",
        "zcode" => "zcode",
        "glm" => "glm-acp-agent",
        _ => "",
    }
}

fn normalize_probe_diag_category(raw: Option<&str>) -> String {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("env_proxy" | "proxy" | "env" | "environment") => "env_proxy".into(),
        Some("auth_required" | "auth" | "login" | "unauthorized") => "auth_required".into(),
        Some("missing_adapter" | "adapter" | "install" | "missing_binary") => "missing_adapter".into(),
        Some("timeout" | "network_timeout" | "slow") => "timeout".into(),
        Some("config" | "config_error" | "misconfigured") => "config".into(),
        Some("false_positive" | "workspace_ok" | "cli_only") => "false_positive".into(),
        _ => "unknown".into(),
    }
}

pub(crate) fn parse_safe_terminal_command_argv(raw: &str) -> Option<Vec<String>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 240 {
        return None;
    }
    if trimmed.contains(['\n', '\r', ';', '|', '&', '>', '<', '`']) || trimmed.contains("$(") {
        return None;
    }
    let argv = crate::acp::presets::split_words(trimmed).ok()?;
    let first = argv.first()?.trim();
    if first.is_empty() || first.starts_with('-') || first.contains('=') {
        return None;
    }
    Some(argv)
}

fn match_expected_probe_agent<'a>(raw_id: &str, expected_agents: &'a [String]) -> Option<&'a str> {
    if expected_agents.is_empty() {
        return None;
    }
    let trimmed = raw_id.trim();
    if let Some(exact) = expected_agents.iter().find(|e| e.eq_ignore_ascii_case(trimmed)) {
        return Some(exact.as_str());
    }
    let lower = trimmed.to_ascii_lowercase();
    let canonical = match lower.as_str() {
        "cursor-agent" => "cursor",
        "antigravity" => "agy",
        "claude-code" | "claude-agent-acp" => "claude",
        "codex-acp" => "codex",
        "zcode-acp-server" => "zcode",
        "dsh-tui" => "dsh",
        other => other,
    };
    expected_agents.iter().find(|e| e.eq_ignore_ascii_case(canonical)).map(String::as_str)
}

pub(crate) fn parse_probe_ai_diagnosis(
    raw: &str,
    expected_agents: &[String],
    prefer_zh: bool,
) -> Result<(Option<String>, Vec<DroveProbeDiagnosisItem>), String> {
    let parsed = extract_json_object(raw).ok_or_else(|| {
        if prefer_zh {
            format!("AI 未返回有效的 JSON 诊断结果: {}", raw.chars().take(160).collect::<String>())
        } else {
            format!("AI did not return valid JSON diagnosis: {}", raw.chars().take(160).collect::<String>())
        }
    })?;
    let summary = parsed
        .get("summary")
        .or_else(|| parsed.get("overview"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(500).collect::<String>());

    let mut by_agent: BTreeMap<String, DroveProbeDiagnosisItem> = BTreeMap::new();
    if let Some(arr) = parsed.get("items").or_else(|| parsed.get("agents")).and_then(Value::as_array) {
        for entry in arr {
            let Some(raw_agent_id) = entry
                .get("agent")
                .or_else(|| entry.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let agent_id = if !expected_agents.is_empty() {
                let Some(matched) = match_expected_probe_agent(raw_agent_id, expected_agents) else {
                    continue;
                };
                matched
            } else {
                raw_agent_id
            };
            let cause = entry
                .get("cause")
                .or_else(|| entry.get("reason"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(320).collect::<String>())
                .unwrap_or_default();
            let fix = entry
                .get("fix")
                .or_else(|| entry.get("action"))
                .or_else(|| entry.get("suggestion"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(420).collect::<String>())
                .unwrap_or_default();
            if cause.is_empty() && fix.is_empty() {
                continue;
            }
            let command = entry
                .get("command")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(240).collect::<String>());
            let command_argv = command.as_deref().and_then(parse_safe_terminal_command_argv);
            let category = normalize_probe_diag_category(entry.get("category").and_then(Value::as_str));
            let likely_false_positive = entry
                .get("likelyFalsePositive")
                .or_else(|| entry.get("likely_false_positive"))
                .and_then(Value::as_bool)
                .unwrap_or(category == "false_positive" || category == "env_proxy");
            let final_cause = if cause.is_empty() { fix.clone() } else { cause };
            let final_fix = if fix.is_empty() { final_cause.clone() } else { fix };
            by_agent.entry(agent_id.to_string()).or_insert(DroveProbeDiagnosisItem {
                agent: agent_id.to_string(),
                category,
                likely_false_positive,
                cause: final_cause,
                fix: final_fix,
                command,
                command_argv,
            });
        }
    }

    let mut items = Vec::with_capacity(expected_agents.len());
    for id in expected_agents {
        if let Some(item) = by_agent.remove(id) {
            items.push(item);
        }
    }
    for (_, extra) in by_agent {
        items.push(extra);
    }
    if summary.is_none() && items.is_empty() {
        return Err(if prefer_zh {
            "AI 未返回可用的异常诊断条目".into()
        } else {
            "AI returned no actionable diagnosis items".into()
        });
    }
    Ok((summary, items))
}

/// Uses the configured Core AI assistant to diagnose Drove ACP probe errors, distinguish workspace-CLI
/// false positives (e.g. daemon missing shell proxy/env or ACP auth quirks) from real setup issues,
/// and propose actionable remediation steps/commands.
pub async fn diagnose_probe_errors_with_ai(
    core: &Arc<Core>,
    params: DroveProbeAiDiagnoseParams,
) -> DroveProbeAiDiagnoseResult {
    let started = Instant::now();
    let prefer_zh = match params.language.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(l) => crate::ai::prompt_lang_code(l) == "zh",
        None => drove_ai_lang(core, None) == "zh",
    };
    let fail = |msg: String| DroveProbeAiDiagnoseResult {
        ok: false,
        model_used: None,
        latency_ms: started.elapsed().as_millis() as u64,
        summary: None,
        items: Vec::new(),
        error: Some(msg),
    };

    let filter_set: HashSet<String> = params
        .agents
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let target_machine = params.target_machine.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let supplied_errored: Vec<DroveProbeErrorInput> = params
        .errored_agents
        .unwrap_or_default()
        .into_iter()
        .filter(|a| {
            !a.id.trim().is_empty()
                && !a.error.trim().is_empty()
                && (filter_set.is_empty() || filter_set.contains(a.id.trim()))
        })
        .collect();

    let use_remote_snapshot = target_machine.is_some() && !supplied_errored.is_empty();
    let probed = if use_remote_snapshot { ProbeSnapshot::default() } else { core.drove.cached_probe() };
    let local_errored: Vec<&ProbedAgent> = if use_remote_snapshot {
        Vec::new()
    } else {
        probed
            .agents
            .iter()
            .filter(|a| {
                a.installed
                    && a.error.as_deref().map(str::trim).is_some_and(|s| !s.is_empty())
                    && (filter_set.is_empty() || filter_set.contains(&a.id))
            })
            .collect()
    };

    if local_errored.is_empty() && supplied_errored.is_empty() {
        return DroveProbeAiDiagnoseResult {
            ok: true,
            model_used: None,
            latency_ms: started.elapsed().as_millis() as u64,
            summary: Some(if prefer_zh {
                "当前未检测到需要处理的 Agent 异常。".into()
            } else {
                "No errored agents currently detected.".into()
            }),
            items: Vec::new(),
            error: None,
        };
    }

    let ai_cfg = core.ai.config_snapshot();
    let raw_base =
        params.base_url.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(ai_cfg.base_url.trim());
    if raw_base.is_empty() {
        return fail(if prefer_zh {
            "请先在「AI 助手」设置中配置远端服务地址（Service URL）".into()
        } else {
            "Configure the AI Assistant Service URL in Settings -> AI first".into()
        });
    }
    let (stored_key, _) = core.ai.read_key();
    let has_key = params.api_key.as_deref().map(str::trim).is_some_and(|s| !s.is_empty()) || stored_key.is_some();
    if !has_key && !crate::ai::allows_keyless_url(raw_base) {
        return fail(if prefer_zh {
            "请先在「AI 助手」设置中配置 API Key".into()
        } else {
            "Configure the AI Assistant API Key in Settings -> AI first".into()
        });
    }

    let is_npx_cmd = |cmd: &[String]| {
        cmd.first().is_some_and(|c| {
            let lower = c.rsplit(['/', '\\']).next().unwrap_or(c).to_ascii_lowercase();
            lower == "npx" || lower.starts_with("npx.")
        })
    };

    let (expected_ids, input_json) = if use_remote_snapshot {
        let healthy_agents: Vec<String> = params
            .healthy_agents
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let expected_ids: Vec<String> = supplied_errored.iter().map(|a| a.id.trim().to_string()).collect();
        let errored_payload: Vec<Value> = supplied_errored
            .iter()
            .map(|a| {
                json!({
                    "id": a.id.trim(),
                    "command": a.command,
                    "path": a.path,
                    "modelsCount": a.models_count,
                    "usesNpxFallback": is_npx_cmd(&a.command),
                    "error": a.error.trim(),
                })
            })
            .collect();
        let payload = json!({
            "targetMachine": target_machine,
            "remoteSnapshot": true,
            "healthyAgents": healthy_agents,
            "erroredAgents": errored_payload,
        })
        .to_string();
        (expected_ids, payload)
    } else {
        let base_env = crate::platform::program_env().await;
        let daemon_proxy_configured = base_env.iter().any(|(k, v)| {
            !v.trim().is_empty()
                && (k.eq_ignore_ascii_case("HTTP_PROXY")
                    || k.eq_ignore_ascii_case("HTTPS_PROXY")
                    || k.eq_ignore_ascii_case("ALL_PROXY"))
        });
        let path_override =
            base_env.iter().find(|(k, _)| k.eq_ignore_ascii_case("PATH")).map(|(_, v)| v.as_str());
        let pathext_override =
            base_env.iter().find(|(k, _)| k.eq_ignore_ascii_case("PATHEXT")).map(|(_, v)| v.as_str());
        let daemon_cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();

        let healthy_agents: Vec<String> = if !local_errored.is_empty() {
            probed
                .agents
                .iter()
                .filter(|a| a.installed && a.error.as_deref().map(str::trim).is_none_or(str::is_empty))
                .map(|a| a.id.clone())
                .collect()
        } else {
            params
                .healthy_agents
                .unwrap_or_default()
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };

        let (expected_ids, errored_payload): (Vec<String>, Vec<Value>) = if !local_errored.is_empty() {
            let ids = local_errored.iter().map(|a| a.id.clone()).collect();
            let items = local_errored
                .iter()
                .map(|a| {
                    let cli_bin = companion_cli_name(&a.id);
                    let has_cli = !cli_bin.is_empty()
                        && crate::acp::presets::resolve_non_shim_binary_in_path(
                            cli_bin,
                            path_override,
                            pathext_override,
                        )
                        .is_some();
                    json!({
                        "id": a.id,
                        "command": a.command,
                        "path": a.path,
                        "modelsCount": a.models.len(),
                        "hasCompanionCliOnDaemonPath": has_cli,
                        "usesNpxFallback": is_npx_cmd(&a.command),
                        "error": a.error,
                    })
                })
                .collect();
            (ids, items)
        } else {
            let ids = supplied_errored.iter().map(|a| a.id.trim().to_string()).collect();
            let items = supplied_errored
                .iter()
                .map(|a| {
                    let id = a.id.trim();
                    let cli_bin = companion_cli_name(id);
                    let has_cli = !cli_bin.is_empty()
                        && crate::acp::presets::resolve_non_shim_binary_in_path(
                            cli_bin,
                            path_override,
                            pathext_override,
                        )
                        .is_some();
                    json!({
                        "id": id,
                        "command": a.command,
                        "path": a.path,
                        "modelsCount": a.models_count,
                        "hasCompanionCliOnDaemonPath": has_cli,
                        "usesNpxFallback": is_npx_cmd(&a.command),
                        "error": a.error.trim(),
                    })
                })
                .collect();
            (ids, items)
        };

        let payload = json!({
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "daemonCwd": daemon_cwd,
            "daemonProxyConfigured": daemon_proxy_configured,
            "healthyAgents": healthy_agents,
            "erroredAgents": errored_payload,
        })
        .to_string();
        (expected_ids, payload)
    };

    let lang_rule = if prefer_zh {
        "Write `summary`, `cause`, and `fix` in concise, practical Simplified Chinese."
    } else {
        "Write `summary`, `cause`, and `fix` in concise, practical English."
    };

    let system_prompt = format!(
        "You are an expert troubleshooting assistant for Herdr2 Drove ACP agent probing.\n\
         Important Herdr2 architecture context:\n\
         - In Herdr2 workspaces, CLI agents launch inside an interactive login shell (loading ~/.zshrc / ~/.bashrc, proxy vars, NVM/PATH) using interactive TUI CLIs (`claude`, `codex`, `cursor-agent`, `agy`, etc.).\n\
         - Drove agent probing runs directly inside the headless `herdr2 serve` daemon using `program_env()` (which does NOT source interactive shell rc files), invoking headless ACP adapters (`claude-agent-acp`, `codex-acp`, `cursor-agent acp`, `zcode-acp-server`, `agy models`) with a 60s handshake timeout (`initialize` + `session/new`, 6 concurrent probes).\n\
         - If `remoteSnapshot` is true, the errors happened on remote machine `targetMachine`; infer its environment from the reported `path`/`command`/`error` rather than assuming a local OS.\n\
         - Common root causes:\n\
           1) `env_proxy`: daemon lacks proxy/API env vars set only in ~/.zshrc or ~/.bashrc -> timeouts or network errors even though workspace CLI works.\n\
           2) `auth_required`: ACP adapter requires separate CLI login/onboarding (`claude`, `cursor-agent login`, `agy login`, etc.) or `authenticate()`.\n\
           3) `missing_adapter`: only companion CLI is installed and `npx -y @agentclientprotocol/...` fallback timed out or failed.\n\
           4) `timeout`: cold-start / concurrent probe contention or slow network (re-probing or setting proxy fixes it).\n\
           5) `config`: invalid custom `[agents]` entry in `config.toml` or bad daemon cwd.\n\
           6) `false_positive`: workspace CLI works fine and only headless ACP probe check failed due to transient or adapter-specific limitation.\n\
         Rules:\n\
         1. Return ONE diagnosis item for EVERY agent in `erroredAgents` using its exact `id`.\n\
         2. `category` MUST be one of: `env_proxy`, `auth_required`, `missing_adapter`, `timeout`, `config`, `false_positive`, `unknown`.\n\
         3. Set `likelyFalsePositive` to `true` when the agent likely works in workspace interactive terminals and the issue is specific to headless daemon/ACP probing.\n\
         4. `command` is OPTIONAL: provide a single safe, non-destructive shell command ONLY when directly helpful (e.g. `cursor-agent login`, `claude`, `h2 server restart`, `npm i -g @agentclientprotocol/claude-agent-acp`). Do NOT use pipes, `&&`, `;`, or redirections in `command`.\n\
         5. {lang_rule}\n\
         6. Output ONLY valid JSON matching:\n\
         {{\"summary\":\"1-2 sentences\",\"items\":[{{\"agent\":\"<id>\",\"category\":\"env_proxy\",\"likelyFalsePositive\":true,\"cause\":\"...\",\"fix\":\"...\",\"command\":\"...\"|null}}]}}"
    );

    let resp = match core
        .ai
        .complete(AiCompleteRequest {
            op: "probe_diagnose".into(),
            tier: AiTaskTier::Standard,
            mode: AiConversationMode::CustomSystem(system_prompt),
            user_prompt: format!(
                "Analyze these Drove agent probe errors and return ONLY the JSON diagnosis:\n<input>\n{input_json}\n</input>"
            ),
            max_tokens: 1536,
            timeout: Duration::from_secs(90),
            url_override: params.base_url,
            key_override: params.api_key,
            model_override: params.model,
            ..Default::default()
        })
        .await
    {
        Ok(r) => r,
        Err(e) => return fail(e),
    };

    let (summary, items) = match parse_probe_ai_diagnosis(&resp.content, &expected_ids, prefer_zh) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };

    DroveProbeAiDiagnoseResult {
        ok: true,
        model_used: Some(resp.model),
        latency_ms: started.elapsed().as_millis() as u64,
        summary,
        items,
        error: None,
    }
}

#[cfg(test)]
pub(crate) fn parse_and_sanitize_ai_roles(
    raw: &str,
    existing_roles: &BTreeMap<String, RoleConfig>,
    probed: &ProbeSnapshot,
) -> Result<(BTreeMap<String, RoleConfig>, Option<String>), String> {
    let (roles, _note, rationale) =
        parse_and_sanitize_ai_roles_with_note(raw, existing_roles, probed, AiRoleConfigMode::All)?;
    Ok((roles, rationale))
}

pub(crate) fn parse_and_sanitize_ai_roles_with_note(
    raw: &str,
    existing_roles: &BTreeMap<String, RoleConfig>,
    probed: &ProbeSnapshot,
    mode: AiRoleConfigMode,
) -> Result<(BTreeMap<String, RoleConfig>, Option<String>, Option<String>), String> {
    let parsed = extract_json_object(raw)
        .ok_or_else(|| format!("AI 未返回有效的 JSON 配置: {}", raw.chars().take(160).collect::<String>()))?;
    let global_note = if matches!(mode, AiRoleConfigMode::All | AiRoleConfigMode::Notes) {
        super::config::normalize_note(
            parsed
                .get("note")
                .or_else(|| parsed.get("routingNote"))
                .or_else(|| parsed.get("routing_note"))
                .and_then(Value::as_str),
            1000,
        )
    } else {
        None
    };
    let roles_obj = parsed
        .get("roles")
        .and_then(Value::as_object)
        .or_else(|| parsed.as_object());
    let rationale = parsed
        .get("rationale")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(400).collect::<String>());

    let any_installed = probed.agents.iter().any(|a| a.installed);
    let mut out = existing_roles.clone();
    let mut updated_any = global_note.is_some();

    if let Some(roles_obj) = roles_obj {
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

            if matches!(mode, AiRoleConfigMode::Strengths | AiRoleConfigMode::Notes) {
                let mut chain: Vec<RoleTarget> =
                    std::iter::once(existing.primary.clone()).chain(existing.fallbacks.iter().cloned()).collect();
                let mut role_changed = false;
                for (pos, item) in raw_cands.iter().enumerate() {
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
                                        && m.is_none_or(|mv| {
                                            c.model.as_deref().is_some_and(|cm| cm.eq_ignore_ascii_case(mv))
                                        })
                                })
                                .or_else(|| chain.iter().position(|c| c.agent.eq_ignore_ascii_case(a)))
                        })
                        .or_else(|| (pos < chain.len()).then_some(pos));
                    let Some(idx) = target_idx else { continue };
                    if mode == AiRoleConfigMode::Strengths {
                        let raw_strengths: Vec<String> = item
                            .get("strengths")
                            .and_then(Value::as_array)
                            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .unwrap_or_default();
                        let clean = normalize_strengths(&raw_strengths);
                        if !clean.is_empty() {
                            chain[idx].strengths = clean;
                            role_changed = true;
                        }
                    } else if let Some(clean_note) =
                        super::config::normalize_note(item.get("note").and_then(Value::as_str), 400)
                    {
                        chain[idx].note = Some(clean_note);
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

            let existing_chain: Vec<&RoleTarget> =
                std::iter::once(&existing.primary).chain(existing.fallbacks.iter()).collect();
            let mut clean_cands: Vec<RoleTarget> = Vec::new();
            let mut seen_pairs: HashSet<(String, Option<String>)> = HashSet::new();

            for item in raw_cands {
                let Some(raw_agent) =
                    item.get("agent").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
                else {
                    continue;
                };
                let matched_agent = probed
                    .agents
                    .iter()
                    .find(|a| a.id.eq_ignore_ascii_case(raw_agent) && (!any_installed || a.installed));
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
                    Some(m) if !discovered_models.is_empty() => {
                        discovered_models.iter().find(|dm| dm.eq_ignore_ascii_case(m)).cloned()
                    }
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
                        .filter(|s| {
                            !s.is_empty() && !s.eq_ignore_ascii_case("auto") && !s.eq_ignore_ascii_case("default")
                        })
                        .map(String::from)
                } else {
                    None
                };
                let matched_existing = existing_chain
                    .iter()
                    .find(|c| {
                        c.agent.eq_ignore_ascii_case(&agent_id)
                            && c.model.as_deref().map(str::to_ascii_lowercase)
                                == model.as_deref().map(str::to_ascii_lowercase)
                    })
                    .or_else(|| existing_chain.iter().find(|c| c.agent.eq_ignore_ascii_case(&agent_id)));
                let strengths = if mode == AiRoleConfigMode::Models {
                    matched_existing.map(|c| c.strengths.clone()).unwrap_or_default()
                } else {
                    let raw_strengths: Vec<String> = item
                        .get("strengths")
                        .and_then(Value::as_array)
                        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    normalize_strengths(&raw_strengths)
                };
                let note = super::config::normalize_note(item.get("note").and_then(Value::as_str), 400)
                    .or_else(|| matched_existing.and_then(|c| c.note.clone()));

                clean_cands.push(RoleTarget { agent: agent_id, model, effort, fast, context, strengths, note });
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
    }

    if !updated_any {
        return Err(match mode {
            AiRoleConfigMode::Strengths => "AI 未返回任何有效的擅长任务标签".into(),
            AiRoleConfigMode::Notes => "AI 未返回任何有效的备注内容".into(),
            _ => "AI 未生成任何匹配当前已安装 Agent 的有效角色配置".into(),
        });
    }
    Ok((out, global_note, rationale))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(coder.primary.agent, "claude");
        assert_eq!(coder.primary.model.as_deref(), Some("claude-opus-4-6"));
        assert!(!coder.primary.fast);
        assert_eq!(coder.primary.strengths, vec!["复杂架构", "后端重构"]);
        assert_eq!(coder.fallbacks.len(), 1);
        assert_eq!(coder.fallbacks[0].agent, "codex");
        assert_eq!(coder.fallbacks[0].model, None);
        assert!(coder.fallbacks[0].fast);
        assert_eq!(coder.fallbacks[0].effort, Effort::Xhigh);
        assert_eq!(coder.fallbacks[0].strengths, vec!["快速修复", "算法性能"]);

        let reviewer = &roles["reviewer"];
        assert_eq!(reviewer.access, Some(RoleAccessMode::Readonly));
        assert_eq!(reviewer.primary.agent, "codex");
        assert_eq!(reviewer.primary.model.as_deref(), Some("gpt-5.4"));
    }

    #[test]
    fn route_candidate_payload_includes_local_time_global_note_and_candidate_notes() {
        let mut cfg = DroveConfig::default();
        cfg.note = Some("GLM 相关模型在下午 2:00–6:00 额度按两倍计算，应避免使用，除非没有其他 agent 可用。".into());
        let task = Task {
            id: "t1".into(),
            run_id: "r1".into(),
            key: "impl".into(),
            title: "Implement auth".into(),
            phase: None,
            spec: Spec {
                target: "src/auth.rs".into(),
                change: "Add middleware".into(),
                constraints: None,
                ownership: None,
                acceptance: "Tests pass".into(),
                verify: None,
            },
            tags: vec!["后端".into()],
            files: vec!["src/auth.rs".into()],
            deps: Vec::new(),
            output_schema: None,
            isolation: Isolation::None,
            effective_isolation: None,
            isolation_reason: None,
            timeout_sec: None,
            stall_warn_sec: None,
            stall_kill_sec: None,
            when: None,
            assign: None,
            status: TaskStatus::Ready,
            result: None,
            failures: 0,
            created_at: now_iso(),
            updated_at: now_iso(),
            kind: None,
            exec_phase: None,
            execution_version: None,
            trigger_rule: None,
            context: None,
            exec: None,
            foreach_from: None,
            template: None,
            expansion_state: None,
            expanded_from: None,
            review: None,
            review_state: None,
            review_circuit: None,
            candidate_result: None,
            cache: None,
            cached_from: None,
            retry_on_worker_reported: None,
        };
        let pool = vec![
            (
                0,
                RoleTarget {
                    agent: "claude".into(),
                    model: Some("zcode/GLM-5.3[1m]".into()),
                    effort: Effort::High,
                    fast: false,
                    context: None,
                    strengths: vec!["后端".into()],
                    note: Some("高峰期双倍计费".into()),
                },
            ),
            (
                1,
                RoleTarget {
                    agent: "codex".into(),
                    model: Some("gpt-5.4".into()),
                    effort: Effort::High,
                    fast: false,
                    context: None,
                    strengths: vec!["后端".into()],
                    note: None,
                },
            ),
        ];

        let payload = build_route_candidate_payload(&cfg, &task, "coder", &pool, "2026-11-23T15:30:00+08:00 (Mon)");
        assert_eq!(payload["current_local_time"], "2026-11-23T15:30:00+08:00 (Mon)");
        assert_eq!(
            payload["routing_note"],
            "GLM 相关模型在下午 2:00–6:00 额度按两倍计算，应避免使用，除非没有其他 agent 可用。"
        );
        assert_eq!(payload["candidates"][0]["note"], "高峰期双倍计费");
        assert!(payload["candidates"][1].get("note").is_none());
        assert!(payload["candidates"][0]["strength_score"].as_u64().unwrap_or(0) > 0);
    }

    #[test]
    fn ai_roles_configure_parses_notes_mode_and_all_mode_with_global_and_candidate_notes() {
        let existing = super::super::config::default_roles();
        let probed = ProbeSnapshot::default();

        // 1. All mode parses both top-level global note and per-candidate notes.
        let raw_all = r#"{
          "rationale": "完成角色与备注一键配置",
          "note": "GLM 相关模型在下午 2:00–6:00 额度按两倍计算，应避免使用，除非没有其他 agent 可用。",
          "roles": {
            "coder": {
              "candidates": [
                { "agent": "claude", "model": "claude-opus-4-6", "effort": "high", "strengths": ["复杂架构"], "note": "主力复杂任务" },
                { "agent": "codex", "model": "gpt-5.4", "effort": "high", "fast": true, "strengths": ["快速修复"], "note": "轻量快速修改" }
              ]
            }
          }
        }"#;
        let (all_roles, global_note, rationale) =
            parse_and_sanitize_ai_roles_with_note(raw_all, &existing, &probed, AiRoleConfigMode::All).unwrap();
        assert_eq!(rationale.as_deref(), Some("完成角色与备注一键配置"));
        assert_eq!(
            global_note.as_deref(),
            Some("GLM 相关模型在下午 2:00–6:00 额度按两倍计算，应避免使用，除非没有其他 agent 可用。")
        );
        assert_eq!(all_roles["coder"].primary.note.as_deref(), Some("主力复杂任务"));
        assert_eq!(all_roles["coder"].fallbacks[0].note.as_deref(), Some("轻量快速修改"));

        // 2. Notes mode keeps existing agents/models/strengths unchanged and updates notes only.
        let raw_notes = r#"{
          "rationale": "已更新路由备注",
          "note": "优先控制高峰期额度消耗，审查与编码尽量跨厂商。",
          "roles": {
            "coder": {
              "candidates": [
                { "index": 0, "note": "仅复杂架构首选" },
                { "index": 1, "note": "主力忙或超额时兜底" }
              ]
            }
          }
        }"#;
        let (notes_roles, notes_global, _) =
            parse_and_sanitize_ai_roles_with_note(raw_notes, &all_roles, &probed, AiRoleConfigMode::Notes).unwrap();
        assert_eq!(notes_global.as_deref(), Some("优先控制高峰期额度消耗，审查与编码尽量跨厂商。"));
        assert_eq!(notes_roles["coder"].primary.agent, "claude");
        assert_eq!(notes_roles["coder"].primary.strengths, vec!["复杂架构"]);
        assert_eq!(notes_roles["coder"].primary.note.as_deref(), Some("仅复杂架构首选"));
        assert_eq!(notes_roles["coder"].fallbacks[0].note.as_deref(), Some("主力忙或超额时兜底"));

        // 3. Single note parser for global / candidate inline AI Note buttons.
        let (single, single_why) =
            parse_single_ai_note(r#"{"rationale":"已整理","note":"下午 2:00–6:00 避开 GLM，除非无替代"}"#, 400).unwrap();
        assert_eq!(single, "下午 2:00–6:00 避开 GLM，除非无替代");
        assert_eq!(single_why.as_deref(), Some("已整理"));

        // 4. Single candidate strengths parser for inline AI Configure button in strengths popup.
        let (cand_str, cand_str_why) = parse_single_ai_strengths(
            r#"{"rationale":"已按模型特长配置","strengths":["复杂架构","后端重构","复杂架构"]}"#,
        )
        .unwrap();
        assert_eq!(cand_str, vec!["复杂架构", "后端重构"]);
        assert_eq!(cand_str_why.as_deref(), Some("已按模型特长配置"));
    }

    #[test]
    fn parse_safe_terminal_command_argv_filters_shell_metachars_and_parses_simple_commands() {
        assert_eq!(
            parse_safe_terminal_command_argv("cursor-agent login"),
            Some(vec!["cursor-agent".into(), "login".into()])
        );
        assert_eq!(
            parse_safe_terminal_command_argv(r#"npm i -g "@agentclientprotocol/claude-agent-acp""#),
            Some(vec!["npm".into(), "i".into(), "-g".into(), "@agentclientprotocol/claude-agent-acp".into()])
        );
        assert_eq!(parse_safe_terminal_command_argv("cursor-agent login && h2 drove probe"), None);
        assert_eq!(parse_safe_terminal_command_argv("echo hi | cat"), None);
        assert_eq!(parse_safe_terminal_command_argv("FOO=bar claude"), None);
        assert_eq!(parse_safe_terminal_command_argv("$(rm -rf /)"), None);
    }

    #[test]
    fn parse_probe_ai_diagnosis_normalizes_categories_order_and_commands() {
        let raw = r#"{
          "summary": "守护进程缺少代理变量且 Cursor 未登录 ACP。",
          "items": [
            {
              "agent": "cursor",
              "category": "auth",
              "likelyFalsePositive": false,
              "cause": "Cursor ACP 需要先登录",
              "fix": "在终端执行登录命令后刷新检测",
              "command": "cursor-agent login"
            },
            {
              "agent": "claude",
              "category": "proxy",
              "cause": "herdr2 serve 缺少 shell 代理环境变量",
              "fix": "在系统环境配置代理并重启服务",
              "command": "export HTTP_PROXY=http://127.0.0.1:7890 && h2 server restart"
            },
            {
              "agent": "unknown-extra",
              "category": "timeout",
              "cause": "should be filtered out",
              "fix": "ignore"
            }
          ]
        }"#;
        let expected = vec!["claude".to_string(), "cursor".to_string()];
        let (summary, items) = parse_probe_ai_diagnosis(raw, &expected, true).unwrap();
        assert_eq!(summary.as_deref(), Some("守护进程缺少代理变量且 Cursor 未登录 ACP。"));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].agent, "claude");
        assert_eq!(items[0].category, "env_proxy");
        assert!(items[0].likely_false_positive);
        assert!(items[0].command.is_some());
        assert_eq!(items[0].command_argv, None, "chained shell command must not become direct argv");
        assert_eq!(items[1].agent, "cursor");
        assert_eq!(items[1].category, "auth_required");
        assert!(!items[1].likely_false_positive);
        assert_eq!(items[1].command_argv, Some(vec!["cursor-agent".into(), "login".into()]));

        // Case-insensitive and alias matching (e.g. "Cursor-Agent" -> "cursor", "Antigravity" -> "agy").
        let alias_raw = r#"{
          "summary": "别名测试",
          "items": [
            { "agent": "Cursor-Agent", "category": "timeout", "cause": "c1", "fix": "f1" },
            { "agent": "Antigravity", "category": "auth", "cause": "c2", "fix": "f2" }
          ]
        }"#;
        let expected_alias = vec!["cursor".to_string(), "agy".to_string()];
        let (_, alias_items) = parse_probe_ai_diagnosis(alias_raw, &expected_alias, true).unwrap();
        assert_eq!(alias_items.len(), 2);
        assert_eq!(alias_items[0].agent, "cursor");
        assert_eq!(alias_items[1].agent, "agy");
        assert!(crate::ai::prompts::op_expects_json("probe_diagnose"));
    }
}
