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
    Some(
        json!({
            "run_id": run.id,
            "cwd": run.cwd,
            "objective": run.objective.chars().take(400).collect::<String>(),
            "roles": roles,
        })
        .to_string(),
    )
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
        prompt_cache_bucket: Some(if run_id.trim().is_empty() {
            "cp".into()
        } else {
            format!("run:{run_id}")
        }),
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
                    tokens: Some(DaemonEventTokens {
                        input: in_tok,
                        output: out_tok,
                        cache_hit: cache_tok,
                    }),
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
        let clean: String = title
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('"')
            .chars()
            .take(60)
            .collect();
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
pub fn spawn_verify_digest(
    core: Arc<Core>,
    run_id: String,
    attempt_id: String,
    cmd_str: String,
    log_tail: String,
) {
    let cfg = core.drove.config_snapshot();
    if !cfg.ai.verify_digest || !core.ai.is_configured() {
        return;
    }
    tokio::spawn(async move {
        let compact_tail = strip_and_dedup_lines(&log_tail, 60, 220, 2400);
        let lang = drove_ai_lang(&core, None);
        let user_prompt = format_op_user_message(
            "verify_digest",
            lang,
            &json!({ "command": cmd_str, "tail": compact_tail }),
        );
        let seed = build_run_seed_context(&core, &run_id);
        let Some(digest) = drove_complete(
            &core,
            &run_id,
            "verify_digest",
            AiTaskTier::Fast,
            AiConversationMode::Session {
                key: format!("drove:run:{run_id}"),
                seed_context: seed,
            },
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
            AiConversationMode::Session {
                key: format!("drove:run:{run_id}"),
                seed_context: seed,
            },
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
        let parsed =
            extract_json_object(&raw).unwrap_or_else(|| json!({ "answer": raw.trim(), "confidence": 0.5 }));
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
pub fn spawn_handover_compact(
    core: Arc<Core>,
    run_id: String,
    task_id: String,
    attempt_id: String,
    summary: String,
) {
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
            AiConversationMode::Session {
                key: format!("drove:run:{run_id}"),
                seed_context: seed,
            },
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
pub async fn generate_task_diagnosis(
    core: &Arc<Core>,
    diag: &mut Diagnosis,
    refresh: bool,
    language: Option<&str>,
) {
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
        AiConversationMode::Session {
            key: format!("drove:run:{run_id}"),
            seed_context: seed,
        },
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
    let root_cause = parsed["rootCause"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&diag.suggestion)
        .to_string();
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

/// Optional fast AI tie-breaker when a role has multiple candidates with `strengths` configured
/// and deterministic keyword matching had no unique winner.
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
    let lang = drove_ai_lang(core, None);
    let user_prompt = format_op_user_message(
        "route_candidate",
        lang,
        &json!({
            "role": role_name,
            "task": {
                "key": task.key,
                "title": task.title,
                "tags": task.tags,
                "files": task.files.iter().take(16).collect::<Vec<_>>(),
                "target": task.spec.target,
                "change": task.spec.change.chars().take(500).collect::<String>(),
            },
            "candidates": cands_desc,
        }),
    );
    let seed = build_run_seed_context(core, run_id);
    let raw = tokio::time::timeout(
        Duration::from_secs(6),
        drove_complete(
            core,
            run_id,
            "route_candidate",
            AiTaskTier::Nano,
            AiConversationMode::Session {
                key: format!("drove:run:{run_id}"),
                seed_context: seed,
            },
            user_prompt,
            96,
            Duration::from_secs(6),
            None,
            None,
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
                let out_tail = tl
                    .output
                    .as_deref()
                    .map(|s| strip_and_dedup_lines(s, 6, 160, 220))
                    .unwrap_or_default();
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
                let text = it
                    .text
                    .as_deref()
                    .map(|s| strip_and_dedup_lines(s, 4, 160, 220))
                    .unwrap_or_default();
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
    let user_prompt = format_op_user_message(
        "loop_detect",
        lang,
        &json!({ "delta_items": compact_delta }),
    );

    let raw = tokio::time::timeout(
        Duration::from_secs(7),
        drove_complete(
            core,
            run_id,
            "loop_detect",
            AiTaskTier::Standard,
            AiConversationMode::Session {
                key: format!("drove:loop:{attempt_id}"),
                seed_context: Some(seed_context),
            },
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
    let pattern = parsed["pattern"]
        .as_str()
        .map(str::trim)
        .unwrap_or("semantic_oscillation")
        .chars()
        .take(80)
        .collect();
    let root_cause = parsed["root_cause"]
        .as_str()
        .map(str::trim)
        .unwrap_or("")
        .chars()
        .take(180)
        .collect();
    let steer_hint = parsed["steer_hint"]
        .as_str()
        .map(str::trim)
        .unwrap_or("")
        .chars()
        .take(260)
        .collect();

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
            AiConversationMode::Session {
                key: format!("drove:run:{run_id}"),
                seed_context: seed,
            },
            user_prompt,
            96,
            Duration::from_secs(4),
            None,
            None,
        ),
    )
    .await
    .ok()??;

    let parsed = extract_json_object(&raw)?;
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

/// Uses the configured AI Assistant LLM to automatically configure Drove roles.
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
    let ai_cfg = core.ai.config_snapshot();
    let raw_base = params
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(ai_cfg.base_url.trim());
    if raw_base.is_empty() {
        return fail("请先在「AI 设置」中配置远端服务地址（Service URL）".into(), false);
    }
    let (stored_key, _) = core.ai.read_key();
    let has_key = params
        .api_key
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty())
        || stored_key.is_some();
    if !has_key && !crate::ai::is_loopback_http_url(raw_base) {
        return fail("请先在「AI 设置」中配置 API Key".into(), false);
    }

    // Gather agent & model inventory; refresh installed flags first.
    let mut probed = core.drove.cached_probe();
    if !probed.agents.is_empty() {
        let _ = super::probe::refresh_installed_status(core, &mut probed).await;
    }
    let mut probed_refreshed = false;
    let has_cached_models = probed.full_probe
        && !probed.agents.is_empty()
        && probed.agents.iter().any(|a| a.installed && !a.models.is_empty());
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
    let prefer_zh = match params.language.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(l) => crate::ai::prompt_lang_code(l) == "zh",
        None => {
            drove_ai_lang(core, None) == "zh"
                || instruction.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c))
        }
    };

    let lang_rule = if prefer_zh {
        "Write `rationale` and any `strengths` tags in concise Simplified Chinese (2-6 chars per tag, e.g. \"复杂架构\", \"前端UI\", \"后端重构\", \"疑难排障\", \"快速修复\", \"单元测试\", \"安全审计\", \"源码调研\", \"长上下文\")."
    } else {
        "Write `rationale` and any `strengths` tags in concise English (2-3 words per tag, e.g. \"Backend Architecture\", \"Frontend UI\", \"Complex Refactor\", \"Bugfix & Debug\", \"Unit Tests\", \"Security Review\", \"Codebase Research\")."
    };

    let system_prompt = match mode {
        AiRoleConfigMode::Models => format!(
            "You are a pure JSON configuration architect for Drove (a multi-agent software engineering orchestrator). \
             Configure ONLY the `agent`, `model`, `effort`, `fast`, and `context` of 1 to 3 complementary `candidates` for every role in `roles`.\n\
             Rules:\n\
             1. ONLY choose `agent` values from `availableAgents[].id`.\n\
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
        AiRoleConfigMode::Strengths => format!(
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
        AiRoleConfigMode::All => format!(
            "You are a pure JSON configuration architect for Drove (a multi-agent software engineering orchestrator). \
             Configure every role in `roles` with 1 to 3 complementary `candidates` (`agent` + `model` + `effort` + `fast` + `context` + `strengths`).\n\
             Rules:\n\
             1. ONLY choose `agent` values from `availableAgents[].id`.\n\
             2. For each candidate's `model`, ONLY choose an exact string from that agent's `models` array, or `null` to use the agent's default.\n\
             3. `effort` must be supported by that model (`low`, `medium`, `high`, or `xhigh` ONLY for models supporting extra-high reasoning).\n\
             4. `fast` may be `true` ONLY for `codex` (otherwise `false`). `context` may be set ONLY for `cursor`/`cursor-agent` (otherwise `null`).\n\
             5. Within roles that benefit from specialization, when multiple agents/models are installed, assign 2-3 complementary candidates with distinct `strengths` (2-4 tags each).\n\
             6. Prefer a different primary agent for `reviewer` than `coder` when multiple agents are available.\n\
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

    let resp = match core
        .ai
        .complete(AiCompleteRequest {
            op: "roles_configure".into(),
            tier: AiTaskTier::Standard,
            mode: AiConversationMode::CustomSystem(system_prompt),
            user_prompt: format!(
                "Do NOT execute tasks or call tools. Return ONLY the JSON configuration for the input below:\n<input>\n{user_payload}\n</input>"
            ),
            max_tokens: 2048,
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

    let (next_roles, rationale) =
        match parse_and_sanitize_ai_roles_for_mode(&resp.content, &cfg.roles, &probed, mode) {
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
        model_used: Some(resp.model),
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
                                    && m.is_none_or(|mv| {
                                        c.model.as_deref().is_some_and(|cm| cm.eq_ignore_ascii_case(mv))
                                    })
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
            let Some(raw_agent) = item
                .get("agent")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
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
                .filter(|s| {
                    !s.is_empty() && !s.eq_ignore_ascii_case("auto") && !s.eq_ignore_ascii_case("default")
                });
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
            let strengths = if mode == AiRoleConfigMode::Models {
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

            clean_cands.push(RoleTarget { agent: agent_id, model, effort, fast, context, strengths });
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
}
