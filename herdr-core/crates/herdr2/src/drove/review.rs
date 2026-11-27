//! Adversarial read-only reviewer loop (Full Diff -> R1, Delta Diff -> R2..Rn)
//! and persistent three-stage Review Circuit Breaker:
//!   1. Normal review fails >= max_normal_rounds (default 2) -> `handoff_to_coordinator`
//!   2. Coordinator fixes directly -> 1 single `post_handoff_review` allowed
//!   3. Post-handoff review still fails -> hard breaker `awaiting_user_decision` (blocks `retry` without `--user-override`).

use super::http::ApiErr;
use super::notify::notify_coordinator;
use super::pool::{mark_task_failed_and_propagate, mark_worker_idle};
use super::roles::diff_touched_paths;
use super::store::{NewDaemonEvent, PostMessageInput, Store};
use super::types::*;
use crate::api::run::{DroveSpawnSpec, spawn_drove_agent};
use crate::runtime::Core;
use crate::transcript::Entry;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Default consecutive non-passing review rounds before tripping handoff to Coordinator.
pub const DEFAULT_MAX_NORMAL_REVIEW_ROUNDS: u32 = 2;

/// Outcome of recording a non-passing review round into the task's `ReviewCircuit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewCircuitOutcome {
    ContinueNormal,
    HandoffToCoordinator,
    AwaitingUserDecision,
}

/// Returns `true` when this task is a dedicated read-only reviewer task (`role = reviewer`).
pub fn is_standalone_reviewer_task(task: &Task) -> bool {
    task.assign
        .as_ref()
        .and_then(|a| a.role.as_deref())
        .map(str::trim)
        .is_some_and(|r| r.eq_ignore_ascii_case("reviewer"))
}

/// Extracts a normalized review verdict string from structured task result `data` if present.
pub fn extract_data_review_verdict(data: Option<&Value>) -> Option<String> {
    let d = data?;
    if let Some(v) = d
        .get("review_status")
        .or_else(|| d.get("reviewStatus"))
        .or_else(|| d.get("verdict"))
        .or_else(|| d.get("decision"))
        .or_else(|| d.get("status"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
    {
        return Some(v);
    }
    if d.get("approved").and_then(Value::as_bool) == Some(false)
        || d.get("passed").and_then(Value::as_bool) == Some(false)
        || d.get("pass").and_then(Value::as_bool) == Some(false)
    {
        return Some("changes_requested".into());
    }
    if d.get("approved").and_then(Value::as_bool) == Some(true)
        || d.get("passed").and_then(Value::as_bool) == Some(true)
        || d.get("pass").and_then(Value::as_bool) == Some(true)
    {
        return Some("approved".into());
    }
    None
}

/// Returns `Some(verdict)` when structured `data` or `blockers` indicates a non-passing review verdict.
pub fn is_review_rejection_data(data: Option<&Value>, blockers: Option<&[String]>) -> Option<String> {
    if let Some(v) = extract_data_review_verdict(data)
        && matches!(
            v.as_str(),
            "changes_requested" | "request_changes" | "rejected" | "reject" | "needs_changes" | "fail" | "failed"
        )
    {
        return Some(v);
    }
    if blockers.is_some_and(|b| !b.is_empty()) {
        return Some("changes_requested".into());
    }
    None
}

/// Returns `true` when structured `data` explicitly indicates an approved/passing review verdict.
pub fn is_review_approval_data(data: Option<&Value>) -> bool {
    extract_data_review_verdict(data)
        .is_some_and(|v| matches!(v.as_str(), "approve" | "approved" | "pass" | "passed" | "ok" | "lgtm"))
}

/// Extracts up to 5 concise finding lines from `ReviewFinding` slice, `data.findings`/`issues`/`blockers`, or `summary`.
pub fn extract_key_findings(data: Option<&Value>, findings: Option<&[ReviewFinding]>, summary: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(fs) = findings {
        for f in fs.iter().take(5) {
            let sev = f.severity.as_deref().unwrap_or("issue");
            let file = f.file.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or("*");
            out.push(format!("[{sev}] {file}: {}", f.message.trim().chars().take(180).collect::<String>()));
        }
    }
    if out.is_empty()
        && let Some(d) = data
    {
        for key in ["findings", "issues", "blockers", "blocking_issues", "blockingIssues"] {
            if let Some(arr) = d.get(key).and_then(Value::as_array) {
                for item in arr.iter().take(5) {
                    if let Some(s) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                        out.push(s.chars().take(180).collect());
                    } else if let Some(obj) = item.as_object() {
                        let sev = obj.get("severity").and_then(Value::as_str).unwrap_or("issue");
                        let file = obj
                            .get("file")
                            .or_else(|| obj.get("path"))
                            .and_then(Value::as_str)
                            .unwrap_or("*");
                        let msg = obj
                            .get("message")
                            .or_else(|| obj.get("title"))
                            .or_else(|| obj.get("summary"))
                            .or_else(|| obj.get("description"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .trim();
                        if !msg.is_empty() {
                            out.push(format!("[{sev}] {file}: {}", msg.chars().take(180).collect::<String>()));
                        }
                    }
                }
                if !out.is_empty() {
                    break;
                }
            }
        }
    }
    if out.is_empty() && !summary.trim().is_empty() {
        out.push(summary.trim().chars().take(240).collect());
    }
    out
}

/// Persistently records one non-passing review round on `task.review_circuit` and transitions the circuit stage.
pub fn record_review_non_pass(
    store: &Store,
    task_id: &str,
    max_normal_rounds: u32,
    verdict: &str,
    summary: &str,
    key_findings: Vec<String>,
    attempt_id: Option<&str>,
) -> (Option<Task>, ReviewCircuitOutcome) {
    let mut outcome = ReviewCircuitOutcome::ContinueNormal;
    let updated = store.update_task(task_id, |t| {
        let cap = max_normal_rounds.clamp(1, 5);
        let circuit = t.review_circuit.get_or_insert_with(|| ReviewCircuit {
            stage: ReviewCircuitStage::Normal,
            review_rounds: 0,
            max_normal_rounds: cap,
            handoff_to_coordinator: false,
            handoff_at: None,
            post_handoff_review_used: false,
            awaiting_user_decision: false,
            tripped_at: None,
            user_overrides: 0,
            history: Vec::new(),
        });
        if circuit.max_normal_rounds == 0 {
            circuit.max_normal_rounds = cap;
        }
        let was_post_handoff =
            circuit.stage == ReviewCircuitStage::PostHandoffReview || circuit.post_handoff_review_used;
        circuit.review_rounds = circuit.review_rounds.saturating_add(1);
        circuit.history.push(ReviewRoundRecord {
            round: circuit.review_rounds,
            verdict: verdict.to_string(),
            summary: summary.trim().chars().take(400).collect(),
            attempt_id: attempt_id.map(String::from),
            key_findings: key_findings.clone(),
            at: now_iso(),
            post_handoff: was_post_handoff,
        });
        if circuit.history.len() > 8 {
            let drop_n = circuit.history.len() - 8;
            circuit.history.drain(..drop_n);
        }
        if was_post_handoff {
            circuit.stage = ReviewCircuitStage::AwaitingUserDecision;
            circuit.handoff_to_coordinator = true;
            circuit.post_handoff_review_used = true;
            circuit.awaiting_user_decision = true;
            circuit.tripped_at = Some(now_iso());
            outcome = ReviewCircuitOutcome::AwaitingUserDecision;
        } else if circuit.review_rounds >= circuit.max_normal_rounds {
            circuit.stage = ReviewCircuitStage::HandoffToCoordinator;
            circuit.handoff_to_coordinator = true;
            if circuit.handoff_at.is_none() {
                circuit.handoff_at = Some(now_iso());
            }
            circuit.awaiting_user_decision = false;
            outcome = ReviewCircuitOutcome::HandoffToCoordinator;
        } else {
            circuit.stage = ReviewCircuitStage::Normal;
            outcome = ReviewCircuitOutcome::ContinueNormal;
        }
    });
    (updated, outcome)
}

/// Marks a task's review circuit as cleared when a review round passes cleanly.
pub fn record_review_passed(store: &Store, task_id: &str) {
    let _ = store.update_task(task_id, |t| {
        if let Some(rc) = t.review_circuit.as_mut() {
            rc.stage = ReviewCircuitStage::Cleared;
            rc.awaiting_user_decision = false;
        }
    });
}

fn format_circuit_history_block(rc: &ReviewCircuit) -> String {
    if rc.history.is_empty() {
        return "（无历史审查摘要）".into();
    }
    rc.history
        .iter()
        .map(|rec| {
            let phase_tag = if rec.post_handoff { " [主 Agent 接管后终审]" } else { "" };
            let mut line = format!("Round {}{phase_tag} ({}): {}", rec.round, rec.verdict, rec.summary);
            if !rec.key_findings.is_empty() {
                let items = rec
                    .key_findings
                    .iter()
                    .take(3)
                    .map(|f| format!("  - {f}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                line.push('\n');
                line.push_str(&items);
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Posts a high-priority `Escalation` message, sends a coordinator wakeup notification,
/// and logs a `DaemonEvent` when the Review Circuit Breaker transitions to handoff or hard stop.
pub async fn emit_review_circuit_escalation(
    core: &Arc<Core>,
    run: &Run,
    task: &Task,
    outcome: ReviewCircuitOutcome,
    attempt_id: Option<&str>,
    worker_id: Option<&str>,
) {
    let Some(rc) = task.review_circuit.as_ref() else {
        return;
    };
    let history_block = format_circuit_history_block(rc);
    match outcome {
        ReviewCircuitOutcome::ContinueNormal => {}
        ReviewCircuitOutcome::HandoffToCoordinator => {
            let subject = format!(
                "[Review 熔断转交] 任务 `{}` 连续 {} 轮未通过 — 请主 Agent 亲自接管修复",
                task.key, rc.review_rounds
            );
            let body = format!(
                "任务 `{}` (`{}`) 的审查已连续未通过 {}/{} 轮，触发 Review 熔断，停止常规 worker 修复-review 循环，现转交主 Agent（Coordinator）亲自接管处理。\n\n\
                 前几轮关键未通过项汇总：\n{history_block}\n\n\
                 下一步要求（强制）：\n\
                 1. 主 Agent 亲自检查代码与分歧项并直接完成修复与本地验证（不要再派常规 worker 反复拉锯，也不要通过 `h2 drove add` 新建重复 review 任务）。\n\
                 2. 修复完成后，仅允许再执行 **1 次** `h2 drove retry {}` 进行终审复核（post-handoff review 1/1）。\n\
                 3. 若该次终审复核仍未通过，Drove 将进入硬熔断（等待用户决策），届时必须立即停手向用户汇报并附带你的处理建议。",
                task.key, task.title, rc.review_rounds, rc.max_normal_rounds, task.key
            );
            let msg = core.drove.store.post_message(PostMessageInput {
                run_id: run.id.clone(),
                kind: MessageType::Escalation,
                from: "drove".into(),
                to: "coordinator".into(),
                thread_id: None,
                task_id: Some(task.id.clone()),
                attempt_id: attempt_id.map(String::from),
                subject: subject.clone(),
                body: Some(body.clone()),
                options: None,
                recommend: Some(format!(
                    "主 Agent 亲自接管修复后，仅执行 1 次 `h2 drove retry {}` 终审复核",
                    task.key
                )),
                why: Some(format!(
                    "连续 {} 轮 review 未通过（上限 {} 轮），已转交主 Agent 接管",
                    rc.review_rounds, rc.max_normal_rounds
                )),
                raised_by: Some("scheduler".into()),
                raised_by_worker: worker_id.map(String::from),
                tag: Some("review_handoff".into()),
                ai_draft: None,
                needs_action: true,
            });
            let line = format!(
                "[drove] [REVIEW HANDOFF] 任务 `{}` 已连续 {} 轮审查未通过，触发熔断并转交主 Agent 亲自接管修复（完成后仅允许再执行 1 次 `h2 drove retry {}` 终审复核）。查看汇总: `h2 drove show {}`",
                task.key, rc.review_rounds, task.key, msg.id
            );
            notify_coordinator(
                core,
                run,
                "review_handoff",
                &subject,
                &line,
                Some(&task.id),
                attempt_id,
                worker_id,
                Some(&msg.id),
            )
            .await;
            core.drove.store.append_daemon_event(NewDaemonEvent {
                run_id: Some(run.id.clone()),
                task_id: Some(task.id.clone()),
                attempt_id: attempt_id.map(String::from),
                worker_id: worker_id.map(String::from),
                message_id: Some(msg.id),
                category: "review".into(),
                kind: "review_handoff".into(),
                status: "warn".into(),
                title: format!("Review handoff to Coordinator on `{}` (R{}/{})", task.key, rc.review_rounds, rc.max_normal_rounds),
                detail: Some(history_block),
                duration_ms: None,
                tokens: None,
                cost: None,
            });
        }
        ReviewCircuitOutcome::AwaitingUserDecision => {
            let subject = format!(
                "[Review 硬熔断 · 等待用户决策] 任务 `{}` 终审仍未通过（累计 {} 轮）",
                task.key, rc.review_rounds
            );
            let body = format!(
                "任务 `{}` (`{}`) 在主 Agent 接管后的唯一 1 次终审复核中仍未通过（累计 {} 轮未通过），已触发 **Review 硬熔断（awaiting_user_decision）**。\n\
                 Drove 已默认拦截后续常规 `h2 drove retry {}`。\n\n\
                 历轮未通过项与分歧汇总：\n{history_block}\n\n\
                 强制要求：\n\
                 1. **立即停止继续重试或新增 review 任务**。\n\
                 2. **立即向用户汇报**：当前状态、未通过的核心分歧点与实际影响。\n\
                 3. **必须给出主 Agent 自己的明确处理建议**（例如：接受现状直接收口 / 简化设计规避二阶复杂度 / 定向继续修某项）。\n\
                 4. 待用户明确拍板后再行动：\n\
                    - 用户批准定向继续修并再审：`h2 drove retry {} --user-override --override-reason \"<用户决策>\"`\n\
                    - 用户批准接受现状直接收口：`h2 drove cancel {}` 或 `h2 drove complete --force`",
                task.key, task.title, rc.review_rounds, task.key, task.key, task.key
            );
            let msg = core.drove.store.post_message(PostMessageInput {
                run_id: run.id.clone(),
                kind: MessageType::Escalation,
                from: "drove".into(),
                to: "coordinator".into(),
                thread_id: None,
                task_id: Some(task.id.clone()),
                attempt_id: attempt_id.map(String::from),
                subject: subject.clone(),
                body: Some(body.clone()),
                options: Some(vec![
                    "接受现状直接收口".into(),
                    "简化设计后收口".into(),
                    "定向修复指定问题并 --user-override 复审".into(),
                ]),
                recommend: Some(
                    "立即向用户说明当前状态、未通过分歧点与影响，并给出主 Agent 自己的处理建议，等待用户拍板".into(),
                ),
                why: Some(format!(
                    "主 Agent 接管后的终审复核仍未通过（累计 {} 轮），触发硬熔断",
                    rc.review_rounds
                )),
                raised_by: Some("scheduler".into()),
                raised_by_worker: worker_id.map(String::from),
                tag: Some("review_circuit_open".into()),
                ai_draft: None,
                needs_action: true,
            });
            let line = format!(
                "[drove] [REVIEW CIRCUIT OPEN · AWAITING USER DECISION] 任务 `{}` 主 Agent 接管后终审仍未通过（累计 {} 轮），已硬熔断拦截常规 retry。请立即停手向用户说明当前状态、分歧点与影响，并给出你的处理建议由用户拍板。详情: `h2 drove show {}`",
                task.key, rc.review_rounds, msg.id
            );
            notify_coordinator(
                core,
                run,
                "review_circuit_open",
                &subject,
                &line,
                Some(&task.id),
                attempt_id,
                worker_id,
                Some(&msg.id),
            )
            .await;
            core.drove.store.append_daemon_event(NewDaemonEvent {
                run_id: Some(run.id.clone()),
                task_id: Some(task.id.clone()),
                attempt_id: attempt_id.map(String::from),
                worker_id: worker_id.map(String::from),
                message_id: Some(msg.id),
                category: "review".into(),
                kind: "review_circuit_open".into(),
                status: "failed".into(),
                title: format!("Review hard circuit breaker tripped on `{}` ({} rounds)", task.key, rc.review_rounds),
                detail: Some(history_block),
                duration_ms: None,
                tokens: None,
                cost: None,
            });
        }
    }
}

/// Runs an adversarial review pass when `task.review` is set.
/// Returns `Ok(())` on approval or when review is not enabled; returns `Err(422)` on rejection.
pub(super) async fn run_adversarial_review_gate(
    core: &Arc<Core>,
    run: &Run,
    task: &Task,
    author_worker: &Worker,
    attempt: &Attempt,
    work_cwd: &str,
) -> Result<(), ApiErr> {
    let Some(spec) = &task.review else {
        return Ok(());
    };
    let max_rounds = spec.max_rounds.unwrap_or(DEFAULT_MAX_NORMAL_REVIEW_ROUNDS).clamp(1, 5);
    let is_post_handoff = task
        .review_circuit
        .as_ref()
        .is_some_and(|rc| rc.stage == ReviewCircuitStage::PostHandoffReview || rc.post_handoff_review_used);
    let round = attempt.review_round.as_ref().map(|r| r.round + 1).unwrap_or(1);

    let _ = core.drove.store.update_task(&task.id, |t| t.exec_phase = Some(TaskPhase::Review));
    let _rev_permit =
        core.drove.reviewer_semaphore(&run.id, run.max_concurrency as usize).acquire_owned().await.ok();

    // 1. Capture candidate patch (`<artifact>.patch`) and tree SHA.
    let author_baseline = core.drove.get_attempt_baseline(&attempt.id).unwrap_or_default();
    let tree_sha = git_tree_sha(work_cwd);
    let patch_text = capture_candidate_patch(work_cwd, &author_baseline);
    let artifact_id = format!("review-{}-r{round}", attempt.id);
    let _ = core.drove.store.write_artifact(&run.id, &format!("{artifact_id}.patch"), patch_text.as_bytes());

    let prev_round = attempt.review_round.clone();
    let delta_artifact_id = if round > 1 {
        let delta_id = format!("review-{}-delta-r{round}", attempt.id);
        let delta_text = match (prev_round.as_ref().and_then(|r| r.tree_sha.as_deref()), tree_sha.as_deref()) {
            (Some(old_t), Some(new_t)) if old_t != new_t => {
                git_diff_trees(work_cwd, old_t, new_t).unwrap_or_else(|| patch_text.clone())
            }
            _ => patch_text.clone(),
        };
        let _ = core.drove.store.write_artifact(&run.id, &format!("{delta_id}.patch"), delta_text.as_bytes());
        Some(delta_id)
    } else {
        None
    };

    let mode = spec.mode.unwrap_or(ReviewMode::Static);
    if matches!(mode, ReviewMode::Breaker | ReviewMode::Hybrid) {
        let breaker_ran = run_adversarial_breaker_pass(
            core,
            run,
            task,
            author_worker,
            attempt,
            spec,
            work_cwd,
            &patch_text,
            tree_sha.clone(),
            &artifact_id,
            delta_artifact_id.clone(),
            round,
            max_rounds,
            is_post_handoff,
        )
        .await?;
        if mode == ReviewMode::Breaker && breaker_ran {
            record_review_passed(&core.drove.store, &task.id);
            return Ok(());
        }
    }

    // 2. Build ordered reviewer candidate chain (supporting fallback + differentAgent + quota filtering + warm reuse).
    let cfg = core.drove.config_snapshot();
    let role_cfg = super::config::resolve_role_config(&cfg, "reviewer")
        .unwrap_or_else(|| super::config::default_roles().remove("reviewer").unwrap());
    let quotas = if cfg.magpie.enabled { core.drove.magpie.fetch_if_enabled(&cfg.magpie, false).await.0 } else { None };
    let run_workers = core.drove.store.list_workers(Some(&run.id));
    let run_attempts = core.drove.store.list_attempts(Some(&run.id), None, None);
    let mut candidates =
        build_reviewer_candidates(&role_cfg, spec, &attempt.agent, quotas.as_deref(), &run_workers, &run_attempts);
    if spec.agent.as_deref().map(str::trim).is_none_or(str::is_empty)
        && candidates.len() >= 2
        && cfg.ai.enabled
        && super::router::has_routing_notes(&cfg, &candidates.iter().collect::<Vec<_>>())
    {
        let pool: Vec<(usize, RoleTarget)> = candidates.iter().cloned().enumerate().collect();
        if let Some((pref_idx, _)) = super::ai::pick_role_candidate(core, &run.id, task, "reviewer", &pool).await
            && pref_idx > 0
            && pref_idx < candidates.len()
        {
            let preferred = candidates.remove(pref_idx);
            candidates.insert(0, preferred);
        }
    }

    let mut chosen_eval: Option<(String, Option<ReviewVerdict>, String)> = None;
    let mut last_agent_err = String::from("No reviewer candidates configured");

    for (cand_idx, cand) in candidates.iter().enumerate() {
        let is_last = cand_idx + 1 >= candidates.len();
        let rev_agent = cand.agent.clone();
        let rev_model = cand.model.clone();
        let rev_effort = cand.effort;
        let rev_fast = cand.fast;

        let rev_att_id = super::crypto::rand_id("att");
        let display = format!("reviewer · {}", task.key);

        let warm_reviewer = core.drove.store.list_workers(Some(&run.id)).into_iter().find(|w| {
            w.state == WorkerState::Idle
                && w.role.as_deref() == Some("reviewer")
                && w.read_only_dir.is_some()
                && w.agent == rev_agent
                && w.h2_agent
                    .as_deref()
                    .and_then(crate::agents::parse_id)
                    .is_some_and(|aid| core.lock().reg.agents.get(&aid).is_some_and(|a| a.is_live()))
        });

        let (rev_worker_id, spawn_agent_id, reused_worker) = if let Some(warm) = warm_reviewer {
            let Some(aid) = warm.h2_agent.as_deref().and_then(crate::agents::parse_id) else {
                continue;
            };
            if rev_model.as_deref() != warm.model.as_deref()
                && let Some(ref m) = rev_model
            {
                super::pool::apply_warm_config(core, aid, "model", serde_json::json!(m)).await;
            }
            let mut labels_patch = BTreeMap::new();
            labels_patch.insert("drove.task".into(), Some(task.key.clone()));
            labels_patch.insert("drove.display".into(), Some(display.clone()));
            crate::api::run::update_agent_display(core, aid, display.clone(), labels_patch);

            let _ = core.drove.store.update_worker(&warm.id, |w| {
                w.state = WorkerState::Busy;
                w.current_attempt_id = Some(rev_att_id.clone());
                w.read_only_dir = Some(work_cwd.to_string());
                w.model = rev_model.clone().or_else(|| w.model.clone());
                w.effort = Some(rev_effort);
                w.fast = (rev_fast || rev_agent == "codex").then_some(rev_fast);
                w.totals.attempts += 1;
                w.last_active_at = now_iso();
            });
            let route_reason = if cand_idx == 0 {
                format!("review round={round} warm")
            } else {
                format!("review round={round} fallback={cand_idx} warm")
            };
            let rev_attempt = Attempt {
                id: rev_att_id.clone(),
                task_id: task.id.clone(),
                run_id: run.id.clone(),
                worker_id: warm.id.clone(),
                agent: rev_agent.clone(),
                model: rev_model.clone(),
                effort: Some(rev_effort),
                fast: rev_fast.then_some(true),
                route_reason,
                reused: true,
                retry_of: None,
                hint: None,
                started_at: now_iso(),
                ended_at: None,
                awaiting_late_done: None,
                tokens: TokenCounts::default(),
                cost: None,
                estimated: None,
                outcome: None,
                failure: None,
                result: None,
                kind: Some("review".into()),
                review_round: None,
                verifying: None,
                verify_rejections: None,
                stagnant_rejections: None,
                last_verify_hash: None,
                last_tree_sha: None,
                verify_log_artifact_id: None,
                verify_ai_digest: None,
                ai_diagnosis: None,
                spinning_detected_at: None,
            };
            core.drove.store.insert_attempt(rev_attempt);
            core.drove.set_worker_last_attempt(&warm.id, &rev_att_id);
            (warm.id, aid, true)
        } else {
            let port = core.drove.web_port.load(std::sync::atomic::Ordering::SeqCst);
            let rev_worker_id = super::crypto::rand_id("wrk");
            let rev_tok = core.drove.register_worker_token(&rev_worker_id);

            let mut labels = BTreeMap::new();
            labels.insert("drove.run".into(), run.id.clone());
            labels.insert("drove.worker".into(), rev_worker_id.clone());
            labels.insert("drove.role".into(), "reviewer".into());
            labels.insert("drove.task".into(), task.key.clone());
            labels.insert("drove.display".into(), display.clone());

            let mut extra_env = BTreeMap::new();
            super::pool::inject_drove_path_env(&mut extra_env);
            extra_env.insert("H2_DROVE_URL".into(), format!("http://127.0.0.1:{port}"));
            extra_env.insert("H2_DROVE_WORKER_TOKEN".into(), rev_tok.clone());
            extra_env.insert("H2_DROVE_WORKER_TICKET".into(), rev_tok);
            extra_env.insert("H2_DROVE_WORKER_ID".into(), rev_worker_id.clone());
            extra_env.insert("H2_DROVE_RUN_ID".into(), run.id.clone());
            extra_env.insert("H2_DROVE_ATTEMPT_ID".into(), rev_att_id.clone());

            let rev_worker = Worker {
                id: rev_worker_id.clone(),
                run_id: run.id.clone(),
                agent: rev_agent.clone(),
                model: rev_model.clone(),
                context_window: cand.context.clone(),
                effort: Some(rev_effort),
                fast: (rev_fast || rev_agent == "codex").then_some(rev_fast),
                session_key: format!("{}:{rev_worker_id}", run.id),
                h2_agent: None,
                cwd: run.cwd.clone(),
                worktree: None,
                state: WorkerState::Starting,
                current_attempt_id: Some(rev_att_id.clone()),
                pid: None,
                fingerprint: Fingerprint::default(),
                context: WorkerContext::default(),
                totals: WorkerTotals { tokens: 0, cost: 0.0, attempts: 1, estimated: None },
                created_at: now_iso(),
                last_active_at: now_iso(),
                role: Some("reviewer".into()),
                forked_from: None,
                protocol_gen: Some(1),
                read_only_dir: Some(work_cwd.to_string()),
            };
            core.drove.store.insert_worker(rev_worker);

            let route_reason = if cand_idx == 0 {
                format!("review round={round}")
            } else {
                format!("review round={round} fallback={cand_idx}")
            };
            let rev_attempt = Attempt {
                id: rev_att_id.clone(),
                task_id: task.id.clone(),
                run_id: run.id.clone(),
                worker_id: rev_worker_id.clone(),
                agent: rev_agent.clone(),
                model: rev_model.clone(),
                effort: Some(rev_effort),
                fast: rev_fast.then_some(true),
                route_reason,
                reused: false,
                retry_of: None,
                hint: None,
                started_at: now_iso(),
                ended_at: None,
                awaiting_late_done: None,
                tokens: TokenCounts::default(),
                cost: None,
                estimated: None,
                outcome: None,
                failure: None,
                result: None,
                kind: Some("review".into()),
                review_round: None,
                verifying: None,
                verify_rejections: None,
                stagnant_rejections: None,
                last_verify_hash: None,
                last_tree_sha: None,
                verify_log_artifact_id: None,
                verify_ai_digest: None,
                ai_diagnosis: None,
                spinning_detected_at: None,
            };
            core.drove.store.insert_attempt(rev_attempt);
            core.drove.set_worker_last_attempt(&rev_worker_id, &rev_att_id);

            let spawn = match spawn_drove_agent(
                core,
                DroveSpawnSpec {
                    agent: rev_agent.clone(),
                    cwd: run.cwd.clone(),
                    parent: resolve_local_parent_addr(core, &run.coordinator.host_session),
                    title: display,
                    labels,
                    env: extra_env,
                    model: rev_model,
                    context: cand.context.clone(),
                    effort: Some(rev_effort.as_str().to_string()),
                    fast: Some(rev_fast),
                    access: RoleAccessMode::Readonly,
                    allowed_paths: Vec::new(),
                    isolate_secrets: true,
                    resume_session_id: None,
                    fallback_handoff_context: None,
                    lineage: None,
                },
            )
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    let err_msg = format!("Failed spawning reviewer `{rev_agent}`: {}", e.message);
                    mark_reviewer_attempt_failed(core, run, task, &rev_worker_id, &rev_att_id, &rev_agent, &err_msg);
                    last_agent_err = err_msg;
                    continue;
                }
            };

            core.drove.store.update_worker(&rev_worker_id, |w| {
                w.state = WorkerState::Busy;
                w.h2_agent = Some(spawn.address.clone());
                w.pid = spawn.pid;
            });
            (rev_worker_id, spawn.agent_id, false)
        };
        let _ = reused_worker;

        const MAX_INLINE_REVIEW_PATCH_CHARS: usize = 48_000;
        let mut patch_preview: String = patch_text.chars().take(MAX_INLINE_REVIEW_PATCH_CHARS).collect();
        if patch_text.chars().count() > MAX_INLINE_REVIEW_PATCH_CHARS {
            patch_preview.push_str(&format!(
                "\n... [diff truncated at {MAX_INLINE_REVIEW_PATCH_CHARS} chars; inspect full files under `{work_cwd}` or run `h2 drove worker read review`]"
            ));
        }
        let mode_title = if round == 1 { "Full Diff (Round 1)" } else { "Delta Diff" };
        let criteria_block = spec
            .criteria
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|c| format!("\nReview criteria:\n{c}\n"))
            .unwrap_or_default();
        let post_handoff_block = if is_post_handoff {
            "\nPOST-HANDOFF FINAL REVIEW (1/1): The Coordinator directly took over and fixed prior blocking issues. Focus strictly on whether prior blocking issues are resolved; do NOT expand scope with speculative second-order issues.\n"
        } else {
            ""
        };
        let cwd_disambiguation = if work_cwd != run.cwd {
            format!(
                "IMPORTANT PATH RULE: Candidate changes live in isolated worktree `{work_cwd}` (your process cwd is base repo `{}`). When reading files or searching symbols, ALWAYS pass absolute paths under `{work_cwd}/...` — NEVER relative paths (which would read unmerged stale files in `{}`).\n",
                run.cwd, run.cwd
            )
        } else {
            String::new()
        };
        let review_prompt = format!(
            "You are the read-only Drove Reviewer for task `{}` (`{}`).\n\
             Candidate workspace: `{work_cwd}` (READONLY — do NOT modify, create, or delete any files).\n\
             {cwd_disambiguation}Target: {}\nAcceptance: {}\n{criteria_block}{post_handoff_block}\n\
             {mode_title}:\n```diff\n{patch_preview}\n```\n\n\
             Reply ONLY with a single JSON object (no other prose if possible):\n\
             {{\"verdict\":\"approve\"|\"changes_requested\",\"summary\":\"...\",\"findings\":[{{\"severity\":\"high\"|\"medium\",\"file\":\"...\",\"message\":\"...\"}}]}}",
            task.key, task.title, task.spec.target, task.spec.acceptance
        );

        let rev_work_baseline = super::roles::capture_dirty_baseline(work_cwd);
        let rev_main_baseline =
            (work_cwd != run.cwd).then(|| super::roles::capture_dirty_baseline(&run.cwd));

        let prev_turn = core.lock().reg.agents.get(&spawn_agent_id).map(|a| a.turn).unwrap_or(0);
        let prev_entry_len = core.read_transcript_entries(spawn_agent_id).map(|v| v.len()).unwrap_or(0);
        if let Err(e) = core.send(spawn_agent_id, &review_prompt, false, None, Some("drove")).await {
            let err_msg = format!("Reviewer `{rev_agent}` send failed: {}", e.message);
            core.kill(&[spawn_agent_id]).await;
            mark_reviewer_attempt_failed(core, run, task, &rev_worker_id, &rev_att_id, &rev_agent, &err_msg);
            last_agent_err = err_msg;
            continue;
        }

        // Wait for reviewer's single ACP turn to finish.
        let deadline = Instant::now() + Duration::from_secs(180);
        let mut timed_out = false;
        loop {
            let done = {
                let st = core.lock();
                match st.reg.agents.get(&spawn_agent_id) {
                    None => true,
                    Some(a) => {
                        !a.is_live() || (a.turn > prev_turn && a.status.as_ref().is_none_or(|s| s.state != "working"))
                    }
                }
            };
            if done {
                break;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                break;
            }
            let notified = core.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let _ = tokio::time::timeout(Duration::from_millis(400), notified).await;
        }

        // Guard against rogue file writes in unsandboxed environments (e.g. Windows job-object or container without bwrap),
        // while filtering out concurrent legitimate writes/merges into `run.cwd` by sibling tasks.
        let raw_work_diff = diff_touched_paths(work_cwd, &rev_work_baseline);
        let mut rogue_touched = if work_cwd == run.cwd {
            filter_concurrent_shared_cwd_changes(core, &run.id, &task.id, &run.cwd, spawn_agent_id, prev_entry_len, raw_work_diff)
        } else {
            raw_work_diff
        };
        if let Some(ref main_base) = rev_main_baseline {
            let filtered_base = filter_concurrent_shared_cwd_changes(
                core,
                &run.id,
                &task.id,
                &run.cwd,
                spawn_agent_id,
                prev_entry_len,
                diff_touched_paths(&run.cwd, main_base),
            );
            for rel in filtered_base {
                rogue_touched.push(format!("(base) {rel}"));
            }
        }
        if !rogue_touched.is_empty() {
            let err_msg = format!(
                "Reviewer `{rev_agent}` violated read-only guardrail by modifying: {}",
                rogue_touched.join(", ")
            );
            mark_reviewer_attempt_failed(core, run, task, &rev_worker_id, &rev_att_id, &rev_agent, &err_msg);
            core.kill(&[spawn_agent_id]).await;
            last_agent_err = err_msg;
            continue;
        }

        // Collect output before closing or idling reviewer session.
        let (assistant_text, transcript_err) = collect_reviewer_turn_output(core, spawn_agent_id, prev_entry_len);
        let structured = try_parse_review_verdict(&assistant_text);
        let agent_err = if structured.is_some() {
            None
        } else if timed_out {
            Some("Reviewer timed out after 180s".into())
        } else if let Some(err) = transcript_err {
            Some(err)
        } else if let Some(err) = looks_like_provider_error(&assistant_text) {
            Some(err)
        } else if assistant_text.trim().is_empty() {
            Some("Reviewer produced empty output".into())
        } else if !is_last {
            Some("Reviewer produced no structured verdict JSON".into())
        } else {
            None
        };

        if let Some(err_msg) = agent_err {
            mark_reviewer_attempt_failed(core, run, task, &rev_worker_id, &rev_att_id, &rev_agent, &err_msg);
            core.kill(&[spawn_agent_id]).await;
            last_agent_err = err_msg;
            continue;
        }

        let parsed = structured.unwrap_or_else(|| parse_review_verdict(&assistant_text));
        let _ = core.drove.store.update_attempt(&rev_att_id, |a| {
            a.outcome = Some(Outcome::Succeeded);
            a.failure = None;
            a.ended_at = Some(now_iso());
            a.result = Some(TaskResult {
                summary: parsed.summary.clone(),
                compact_summary: None,
                data: serde_json::to_value(&parsed).ok(),
                files_modified: None,
                blockers: None,
                output_file_hashes: None,
            });
        });

        mark_worker_idle(core, &rev_worker_id);
        chosen_eval = Some((rev_worker_id, Some(parsed), assistant_text));
        break;
    }

    let Some((rev_worker_id, parsed_opt, assistant_text)) = chosen_eval else {
        let msg = format!("All reviewer candidates failed: {last_agent_err}");
        let _ = core.drove.store.update_attempt(&attempt.id, |a| {
            a.outcome = Some(Outcome::Failed);
            a.ended_at = Some(now_iso());
            a.failure = Some(AttemptFailure { kind: FailureKind::AgentError, message: msg.clone() });
        });
        mark_worker_idle(core, &author_worker.id);
        super::fail_or_retry_task(core, &run.id, &task.id, &attempt.id, FailureKind::AgentError, &msg, 0).await;
        notify_coordinator(
            core,
            run,
            "reviewer_failed",
            &format!("Reviewer unavailable for `{}`", task.key),
            &format!("[drove] Reviewer unavailable for `{}`: {last_agent_err}", task.key),
            Some(&task.id),
            Some(&attempt.id),
            Some(&author_worker.id),
            None,
        )
        .await;
        return Err(ApiErr::new(502, "reviewer_unavailable", msg));
    };

    let parsed = parsed_opt.unwrap_or_else(|| parse_review_verdict(&assistant_text));

    let round_info = AttemptReview {
        round,
        max_rounds: Some(max_rounds),
        base_tree_sha: None,
        tree_sha: tree_sha.clone(),
        patch_artifact_id: Some(artifact_id.clone()),
        delta_patch_artifact_id: delta_artifact_id.clone(),
        verdict: Some(parsed.verdict.clone()),
        summary: Some(parsed.summary.clone()),
        findings: parsed.findings.clone(),
    };
    let approved = parsed.verdict.eq_ignore_ascii_case("approve") || parsed.verdict.eq_ignore_ascii_case("approved");
    let _ = core.drove.store.update_attempt(&attempt.id, |a| a.review_round = Some(round_info));
    let _ = core.drove.store.update_task(&task.id, |t| {
        t.review_state = Some(ReviewState {
            round,
            reviewer_worker_id: Some(rev_worker_id.clone()),
            base_tree_sha: None,
            last_reviewed_tree_sha: tree_sha.clone(),
            current_tree_sha: tree_sha.clone(),
            approved_tree_sha: approved.then(|| tree_sha.clone()).flatten(),
            patch_artifact_id: Some(artifact_id.clone()),
            findings: parsed.findings.clone(),
            last_summary: Some(parsed.summary.clone()),
        });
        if !approved {
            t.exec_phase = Some(TaskPhase::Rework);
        }
    });

    core.drove.store.append_daemon_event(NewDaemonEvent {
        run_id: Some(run.id.clone()),
        task_id: Some(task.id.clone()),
        attempt_id: Some(attempt.id.clone()),
        worker_id: Some(rev_worker_id),
        message_id: None,
        category: "review".into(),
        kind: format!("round_{round}"),
        status: if approved { "ok".into() } else { "changes_requested".into() },
        title: format!("Review R{round} on `{}`: {}", task.key, parsed.verdict),
        detail: Some(parsed.summary.clone()),
        duration_ms: None,
        tokens: None,
        cost: None,
    });

    if approved {
        record_review_passed(&core.drove.store, &task.id);
        return Ok(());
    }

    let key_findings = extract_key_findings(None, parsed.findings.as_deref(), &parsed.summary);
    let (updated_task_opt, circuit_outcome) = record_review_non_pass(
        &core.drove.store,
        &task.id,
        max_rounds,
        &parsed.verdict,
        &parsed.summary,
        key_findings,
        Some(&attempt.id),
    );

    let findings_txt = parsed
        .findings
        .as_ref()
        .map(|fs| {
            fs.iter()
                .map(|f| {
                    format!(
                        "- [{}] {}: {}",
                        f.severity.as_deref().unwrap_or("info"),
                        f.file.as_deref().unwrap_or("*"),
                        f.message
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let total_rounds = updated_task_opt.as_ref().and_then(|t| t.review_circuit.as_ref()).map_or(round, |rc| rc.review_rounds);
    let reject_msg = if findings_txt.is_empty() {
        format!("Review round {total_rounds}/{max_rounds} requested changes: {}", parsed.summary)
    } else {
        format!("Review round {total_rounds}/{max_rounds} requested changes: {}\n{findings_txt}", parsed.summary)
    };

    match circuit_outcome {
        ReviewCircuitOutcome::HandoffToCoordinator | ReviewCircuitOutcome::AwaitingUserDecision => {
            let _ = super::integrate::sync_worktree_for_coordinator_handoff(
                core,
                run,
                author_worker.worktree.as_deref(),
            )
            .await;
            let _ = core.drove.store.update_attempt(&attempt.id, |a| {
                a.outcome = Some(Outcome::Failed);
                a.ended_at = Some(now_iso());
                a.failure = Some(AttemptFailure { kind: FailureKind::ReviewRejected, message: reject_msg.clone() });
            });
            mark_worker_idle(core, &author_worker.id);
            let failed_task = mark_task_failed_and_propagate(core, &run.id, &task.id)
                .or(updated_task_opt)
                .unwrap_or_else(|| task.clone());
            emit_review_circuit_escalation(
                core,
                run,
                &failed_task,
                circuit_outcome,
                Some(&attempt.id),
                Some(&author_worker.id),
            )
            .await;
            let err_code = if circuit_outcome == ReviewCircuitOutcome::AwaitingUserDecision {
                "review_circuit_open"
            } else {
                "review_handoff_to_coordinator"
            };
            let worker_stop_msg = format!(
                "{reject_msg}\n\n[DROVE CIRCUIT BREAKER TRIPPED — ATTEMPT CLOSED] Maximum review rounds reached; this task has been handed off to the Coordinator and candidate changes have been synced. STOP IMMEDIATELY: do NOT edit any more files, do NOT run further commands, and do NOT call `drove_finish` again. End your turn now."
            );
            return Err(ApiErr::new(422, err_code, worker_stop_msg));
        }
        ReviewCircuitOutcome::ContinueNormal => {
            Err(ApiErr::new(422, "review_changes_requested", reject_msg))
        }
    }
}

struct BreakerWorktreeGuard {
    run_cwd: String,
    wt_dir: std::path::PathBuf,
}

impl Drop for BreakerWorktreeGuard {
    fn drop(&mut self) {
        super::integrate::cleanup_worktree_dir(&self.run_cwd, &self.wt_dir);
    }
}

pub(crate) fn default_breaker_test_globs() -> Vec<String> {
    vec![
        "tests/**".into(),
        "**/tests/**".into(),
        "test/**".into(),
        "**/test/**".into(),
        "**/*_test.*".into(),
        "**/*_tests.*".into(),
        "**/*.test.*".into(),
        "**/*.spec.*".into(),
    ]
}

fn resolve_breaker_verify_cmd(spec: &ReviewSpec, task: &Task) -> Option<(String, u64)> {
    if let Some(cmd) = spec.verify_cmd.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return Some((cmd.to_string(), 180));
    }
    match task.spec.verify.as_ref()? {
        VerifySpec::Cmd(s) => {
            let trimmed = s.trim();
            (!trimmed.is_empty()).then(|| (trimmed.to_string(), 180))
        }
        VerifySpec::Obj(o) => {
            let trimmed = o.cmd.trim();
            (!trimmed.is_empty()).then(|| (trimmed.to_string(), o.timeout_sec.unwrap_or(180).clamp(10, 600)))
        }
    }
}

/// Parses a structured Breaker verdict (`pass` / `broken` / `approve` / `changes_requested`).
pub fn try_parse_breaker_verdict(raw: &str) -> Option<ReviewVerdict> {
    let is_valid = |v: &ReviewVerdict| {
        matches!(
            v.verdict.trim().to_ascii_lowercase().as_str(),
            "pass" | "passed" | "approve" | "approved" | "broken" | "fail" | "failed" | "changes_requested" | "reject" | "rejected"
        ) && v.summary != "..."
    };
    if let Ok(v) = serde_json::from_str::<ReviewVerdict>(raw.trim())
        && is_valid(&v)
    {
        return Some(v);
    }
    for (idx, _) in raw.match_indices('{') {
        let mut de = serde_json::Deserializer::from_str(&raw[idx..]).into_iter::<ReviewVerdict>();
        if let Some(Ok(v)) = de.next()
            && is_valid(&v)
        {
            return Some(v);
        }
    }
    None
}

fn is_breaker_broken_claim(verdict: &str) -> bool {
    matches!(
        verdict.trim().to_ascii_lowercase().as_str(),
        "broken" | "fail" | "failed" | "changes_requested" | "reject" | "rejected"
    )
}

/// Runs an Antigravity `/boost`-inspired adversarial `Breaker` pass inside a throwaway Git worktree:
/// - Production code is locked read-only (`RoleAccessMode::ScopedWrite` restricted to `test_paths`).
/// - The Breaker writes edge-case/concurrency tests and runs `verify_cmd`.
/// - Candidate rejection occurs ONLY if the Breaker claims a defect AND the daemon independently
///   reproduces a non-zero exit from `verify_cmd` without any out-of-scope file modifications.
/// Returns `Ok(true)` if the Breaker pass ran and candidate survived, `Ok(false)` if skipped/fallback,
/// or `Err(422)` if empirically falsified.
#[allow(clippy::too_many_arguments)]
async fn run_adversarial_breaker_pass(
    core: &Arc<Core>,
    run: &Run,
    task: &Task,
    author_worker: &Worker,
    attempt: &Attempt,
    spec: &ReviewSpec,
    work_cwd: &str,
    patch_text: &str,
    tree_sha: Option<String>,
    artifact_id: &str,
    delta_artifact_id: Option<String>,
    round: u32,
    max_rounds: u32,
    is_post_handoff: bool,
) -> Result<bool, ApiErr> {
    let Some((verify_cmd, verify_timeout)) = resolve_breaker_verify_cmd(spec, task) else {
        return Ok(false);
    };
    let brk_cwd = match super::integrate::create_breaker_worktree(
        core,
        run,
        work_cwd,
        &format!("{}-r{round}", attempt.id),
    )
    .await
    {
        Ok(dir) => dir,
        Err(reason) => {
            core.drove.store.append_daemon_event(NewDaemonEvent {
                run_id: Some(run.id.clone()),
                task_id: Some(task.id.clone()),
                attempt_id: Some(attempt.id.clone()),
                worker_id: None,
                message_id: None,
                category: "review".into(),
                kind: "breaker_fallback".into(),
                status: "warn".into(),
                title: format!("Breaker worktree unavailable on `{}`, falling back", task.key),
                detail: Some(reason),
                duration_ms: None,
                tokens: None,
                cost: None,
            });
            return Ok(false);
        }
    };
    let _brk_guard = BreakerWorktreeGuard {
        run_cwd: run.cwd.clone(),
        wt_dir: std::path::PathBuf::from(&brk_cwd),
    };

    let allowed_paths = spec
        .test_paths
        .clone()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(default_breaker_test_globs);
    let scope_cfg = RoleConfig {
        label: Some("Breaker".into()),
        summary: None,
        access: Some(RoleAccessMode::ScopedWrite),
        allowed_paths: allowed_paths.clone(),
        primary: RoleTarget {
            agent: "codex".into(),
            model: None,
            effort: Effort::High,
            fast: false,
            context: None,
            strengths: Vec::new(),
            note: None,
        },
        fallbacks: Vec::new(),
        prompt: None,
    };

    let cfg = core.drove.config_snapshot();
    let role_cfg = super::config::resolve_role_config(&cfg, "reviewer")
        .unwrap_or_else(|| super::config::default_roles().remove("reviewer").unwrap());
    let quotas = if cfg.magpie.enabled { core.drove.magpie.fetch_if_enabled(&cfg.magpie, false).await.0 } else { None };
    let run_workers = core.drove.store.list_workers(Some(&run.id));
    let run_attempts = core.drove.store.list_attempts(Some(&run.id), None, None);
    let mut candidates =
        build_reviewer_candidates(&role_cfg, spec, &attempt.agent, quotas.as_deref(), &run_workers, &run_attempts);
    if candidates.is_empty() {
        return Ok(false);
    }
    if spec.agent.as_deref().map(str::trim).is_none_or(str::is_empty)
        && candidates.len() >= 2
        && cfg.ai.enabled
        && super::router::has_routing_notes(&cfg, &candidates.iter().collect::<Vec<_>>())
    {
        let pool: Vec<(usize, RoleTarget)> = candidates.iter().cloned().enumerate().collect();
        if let Some((pref_idx, _)) = super::ai::pick_role_candidate(core, &run.id, task, "reviewer", &pool).await
            && pref_idx > 0
            && pref_idx < candidates.len()
        {
            let preferred = candidates.remove(pref_idx);
            candidates.insert(0, preferred);
        }
    }

    const MAX_INLINE_BREAKER_PATCH_CHARS: usize = 36_000;
    let mut patch_preview: String = patch_text.chars().take(MAX_INLINE_BREAKER_PATCH_CHARS).collect();
    if patch_text.chars().count() > MAX_INLINE_BREAKER_PATCH_CHARS {
        patch_preview.push_str(&format!(
            "\n... [diff truncated at {MAX_INLINE_BREAKER_PATCH_CHARS} chars; inspect full files in `{brk_cwd}`]"
        ));
    }
    let criteria_block = spec
        .criteria
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|c| format!("\nReview criteria:\n{c}\n"))
        .unwrap_or_default();
    let post_handoff_block = if is_post_handoff {
        "\nPOST-HANDOFF FINAL CHECK (1/1): Focus strictly on verifying whether prior blocking defects are truly fixed; do NOT invent speculative out-of-spec requirements.\n"
    } else {
        ""
    };

    let mut chosen: Option<(String, ReviewVerdict, Vec<String>)> = None;
    for (cand_idx, cand) in candidates.iter().enumerate() {
        // Ensure clean throwaway tree before each breaker candidate attempt.
        if cand_idx > 0 {
            super::integrate::reset_worktree_clean(&brk_cwd);
        }
        let brk_baseline = super::roles::capture_dirty_baseline(&brk_cwd);
        let author_wt_baseline = (work_cwd != run.cwd).then(|| super::roles::capture_dirty_baseline(work_cwd));
        let main_baseline = super::roles::capture_dirty_baseline(&run.cwd);

        let port = core.drove.web_port.load(std::sync::atomic::Ordering::SeqCst);
        let brk_worker_id = super::crypto::rand_id("wrk");
        let brk_att_id = super::crypto::rand_id("att");
        let brk_tok = core.drove.register_worker_token(&brk_worker_id);
        let display = format!("breaker · {}", task.key);

        let mut labels = BTreeMap::new();
        labels.insert("drove.run".into(), run.id.clone());
        labels.insert("drove.worker".into(), brk_worker_id.clone());
        labels.insert("drove.role".into(), "reviewer".into());
        labels.insert("drove.task".into(), task.key.clone());
        labels.insert("drove.display".into(), display.clone());

        let mut extra_env = BTreeMap::new();
        super::pool::inject_drove_path_env(&mut extra_env);
        extra_env.insert("H2_DROVE_URL".into(), format!("http://127.0.0.1:{port}"));
        extra_env.insert("H2_DROVE_WORKER_TOKEN".into(), brk_tok.clone());
        extra_env.insert("H2_DROVE_WORKER_TICKET".into(), brk_tok);
        extra_env.insert("H2_DROVE_WORKER_ID".into(), brk_worker_id.clone());
        extra_env.insert("H2_DROVE_RUN_ID".into(), run.id.clone());
        extra_env.insert("H2_DROVE_ATTEMPT_ID".into(), brk_att_id.clone());

        core.drove.store.insert_worker(Worker {
            id: brk_worker_id.clone(),
            run_id: run.id.clone(),
            agent: cand.agent.clone(),
            model: cand.model.clone(),
            context_window: cand.context.clone(),
            effort: Some(cand.effort),
            fast: (cand.fast || cand.agent == "codex").then_some(cand.fast),
            session_key: format!("{}:{brk_worker_id}", run.id),
            h2_agent: None,
            cwd: brk_cwd.clone(),
            worktree: Some(brk_cwd.clone()),
            state: WorkerState::Starting,
            current_attempt_id: Some(brk_att_id.clone()),
            pid: None,
            fingerprint: Fingerprint::default(),
            context: WorkerContext::default(),
            totals: WorkerTotals { tokens: 0, cost: 0.0, attempts: 1, estimated: None },
            created_at: now_iso(),
            last_active_at: now_iso(),
            role: Some("reviewer".into()),
            forked_from: None,
            protocol_gen: Some(1),
            read_only_dir: Some(work_cwd.to_string()),
        });
        core.drove.store.insert_attempt(Attempt {
            id: brk_att_id.clone(),
            task_id: task.id.clone(),
            run_id: run.id.clone(),
            worker_id: brk_worker_id.clone(),
            agent: cand.agent.clone(),
            model: cand.model.clone(),
            effort: Some(cand.effort),
            fast: cand.fast.then_some(true),
            route_reason: format!("breaker round={round}"),
            reused: false,
            retry_of: None,
            hint: None,
            started_at: now_iso(),
            ended_at: None,
            awaiting_late_done: None,
            tokens: TokenCounts::default(),
            cost: None,
            estimated: None,
            outcome: None,
            failure: None,
            result: None,
            kind: Some("review".into()),
            review_round: None,
            verifying: None,
            verify_rejections: None,
            stagnant_rejections: None,
            last_verify_hash: None,
            last_tree_sha: None,
            verify_log_artifact_id: None,
            verify_ai_digest: None,
            ai_diagnosis: None,
            spinning_detected_at: None,
        });
        core.drove.set_worker_last_attempt(&brk_worker_id, &brk_att_id);

        let spawn = match spawn_drove_agent(
            core,
            DroveSpawnSpec {
                agent: cand.agent.clone(),
                cwd: brk_cwd.clone(),
                parent: resolve_local_parent_addr(core, &run.coordinator.host_session),
                title: display,
                labels,
                env: extra_env,
                model: cand.model.clone(),
                context: cand.context.clone(),
                effort: Some(cand.effort.as_str().to_string()),
                fast: Some(cand.fast),
                access: RoleAccessMode::ScopedWrite,
                allowed_paths: allowed_paths.clone(),
                isolate_secrets: true,
                resume_session_id: None,
                fallback_handoff_context: None,
                lineage: None,
            },
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                let err_msg = format!("Failed spawning breaker `{}`: {}", cand.agent, e.message);
                mark_reviewer_attempt_failed(core, run, task, &brk_worker_id, &brk_att_id, &cand.agent, &err_msg);
                continue;
            }
        };

        let _ = core.drove.store.update_worker(&brk_worker_id, |w| {
            w.state = WorkerState::Busy;
            w.h2_agent = Some(spawn.address.clone());
            w.pid = spawn.pid;
        });

        let breaker_prompt = format!(
            "You are the adversarial Drove Breaker for task `{}` (`{}`).\n\
             Throwaway sandbox worktree: `{brk_cwd}`\n\
             ALLOWED TEST WRITE GLOBS: `{}` (ALL production source files are strictly READONLY; never modify production files).\n\
             Verification command: `{verify_cmd}`\n\
             Target: {}\nAcceptance: {}\n{criteria_block}{post_handoff_block}\n\
             Candidate Diff:\n```diff\n{patch_preview}\n```\n\n\
             Your mission: Try to FALSIFY the candidate implementation using realistic boundary, empty/malformed input, state-transition, or concurrency tests that strictly conform to the task Spec & Acceptance.\n\
             1. Write or extend test(s) ONLY inside the allowed test globs above.\n\
             2. Run `{verify_cmd}` to verify whether your test compiles and exposes a real defect.\n\
             3. If your own test has a syntax/import mistake, fix or revert your test — never report `broken` unless `{verify_cmd}` fails due to a genuine defect in the candidate code.\n\
             Reply ONLY with a single JSON object:\n\
             {{\"verdict\":\"pass\"|\"broken\",\"summary\":\"...\",\"findings\":[{{\"severity\":\"high\"|\"medium\",\"file\":\"...\",\"message\":\"...\"}}]}}",
            task.key,
            task.title,
            allowed_paths.join(", "),
            task.spec.target,
            task.spec.acceptance,
        );

        let prev_turn = core.lock().reg.agents.get(&spawn.agent_id).map(|a| a.turn).unwrap_or(0);
        let prev_entry_len = core.read_transcript_entries(spawn.agent_id).map(|v| v.len()).unwrap_or(0);
        if let Err(e) = core.send(spawn.agent_id, &breaker_prompt, false, None, Some("drove")).await {
            let err_msg = format!("Breaker `{}` send failed: {}", cand.agent, e.message);
            core.kill(&[spawn.agent_id]).await;
            mark_reviewer_attempt_failed(core, run, task, &brk_worker_id, &brk_att_id, &cand.agent, &err_msg);
            continue;
        }

        let deadline = Instant::now() + Duration::from_secs(240);
        let mut timed_out = false;
        loop {
            let done = {
                let st = core.lock();
                match st.reg.agents.get(&spawn.agent_id) {
                    None => true,
                    Some(a) => {
                        !a.is_live() || (a.turn > prev_turn && a.status.as_ref().is_none_or(|s| s.state != "working"))
                    }
                }
            };
            if done {
                break;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                break;
            }
            let notified = core.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let _ = tokio::time::timeout(Duration::from_millis(400), notified).await;
        }

        let (assistant_text, transcript_err) = collect_reviewer_turn_output(core, spawn.agent_id, prev_entry_len);
        // Close throwaway breaker session before its temporary worktree directory is removed.
        core.kill(&[spawn.agent_id]).await;
        let _ = core.drove.store.update_worker(&brk_worker_id, |w| {
            w.state = WorkerState::Closed;
            w.current_attempt_id = None;
            w.worktree = None;
            w.last_active_at = now_iso();
        });

        let touched: Vec<String> = diff_touched_paths(&brk_cwd, &brk_baseline)
            .into_iter()
            .filter(|p| !super::roles::is_ephemeral_cache_path(p))
            .collect();
        let mut violations = super::roles::find_scope_violations(&scope_cfg, &touched);
        if let Some(ref wt_base) = author_wt_baseline {
            for rel in diff_touched_paths(work_cwd, wt_base) {
                if !super::roles::is_ephemeral_cache_path(&rel) {
                    violations.push(format!("(candidate) {rel}"));
                }
            }
        }
        let filtered_base = filter_concurrent_shared_cwd_changes(
            core,
            &run.id,
            &task.id,
            &run.cwd,
            spawn.agent_id,
            prev_entry_len,
            diff_touched_paths(&run.cwd, &main_baseline),
        );
        for rel in filtered_base {
            violations.push(format!("(base) {rel}"));
        }
        if !violations.is_empty() {
            let err_msg = format!(
                "Breaker `{}` violated test-only scope by modifying: {}",
                cand.agent,
                violations.join(", ")
            );
            mark_reviewer_attempt_failed(core, run, task, &brk_worker_id, &brk_att_id, &cand.agent, &err_msg);
            continue;
        }

        let structured = try_parse_breaker_verdict(&assistant_text);
        if structured.is_none()
            && (timed_out
                || transcript_err.is_some()
                || looks_like_provider_error(&assistant_text).is_some()
                || assistant_text.trim().is_empty())
        {
            let err_msg = transcript_err
                .or_else(|| looks_like_provider_error(&assistant_text))
                .unwrap_or_else(|| {
                    if timed_out {
                        "Breaker timed out after 240s".into()
                    } else {
                        "Breaker produced empty output".into()
                    }
                });
            mark_reviewer_attempt_failed(core, run, task, &brk_worker_id, &brk_att_id, &cand.agent, &err_msg);
            continue;
        }

        let parsed = structured.unwrap_or(ReviewVerdict {
            verdict: "pass".into(),
            summary: assistant_text.trim().chars().take(280).collect(),
            findings: None,
        });
        let _ = core.drove.store.update_attempt(&brk_att_id, |a| {
            a.outcome = Some(Outcome::Succeeded);
            a.failure = None;
            a.ended_at = Some(now_iso());
            a.result = Some(TaskResult {
                summary: parsed.summary.clone(),
                compact_summary: None,
                data: serde_json::to_value(&parsed).ok(),
                files_modified: (!touched.is_empty()).then(|| touched.clone()),
                blockers: None,
                output_file_hashes: None,
            });
        });
        chosen = Some((brk_worker_id, parsed, touched));
        break;
    }

    let Some((brk_worker_id, parsed, touched)) = chosen else {
        return Ok(false);
    };

    // Daemon-side empirical falsification check: run `verify_cmd` independently in `brk_cwd`.
    if !touched.is_empty() {
        let brk_patch = capture_candidate_patch(&brk_cwd, &super::roles::capture_dirty_baseline(work_cwd));
        let brk_art_id = format!("breaker-{}-r{round}", attempt.id);
        let _ = core.drove.store.write_artifact(&run.id, &format!("{brk_art_id}.patch"), brk_patch.as_bytes());

        let vres = super::verify::run_verify_command(&brk_cwd, &verify_cmd, verify_timeout).await;
        if !vres.success && is_breaker_broken_claim(&parsed.verdict) {
            let out_tail: String =
                vres.combined_output.chars().rev().take(1800).collect::<String>().chars().rev().collect();
            let summary = format!("Breaker falsified candidate via `{verify_cmd}`: {}", parsed.summary);
            let round_info = AttemptReview {
                round,
                max_rounds: Some(max_rounds),
                base_tree_sha: None,
                tree_sha: tree_sha.clone(),
                patch_artifact_id: Some(artifact_id.to_string()),
                delta_patch_artifact_id: delta_artifact_id,
                verdict: Some("changes_requested".into()),
                summary: Some(summary.clone()),
                findings: parsed.findings.clone(),
            };
            let _ = core.drove.store.update_attempt(&attempt.id, |a| a.review_round = Some(round_info));
            let _ = core.drove.store.update_task(&task.id, |t| {
                t.review_state = Some(ReviewState {
                    round,
                    reviewer_worker_id: Some(brk_worker_id.clone()),
                    base_tree_sha: None,
                    last_reviewed_tree_sha: tree_sha.clone(),
                    current_tree_sha: tree_sha.clone(),
                    approved_tree_sha: None,
                    patch_artifact_id: Some(brk_art_id.clone()),
                    findings: parsed.findings.clone(),
                    last_summary: Some(summary.clone()),
                });
                t.exec_phase = Some(TaskPhase::Rework);
            });
            core.drove.store.append_daemon_event(NewDaemonEvent {
                run_id: Some(run.id.clone()),
                task_id: Some(task.id.clone()),
                attempt_id: Some(attempt.id.clone()),
                worker_id: Some(brk_worker_id),
                message_id: None,
                category: "review".into(),
                kind: format!("breaker_r{round}"),
                status: "changes_requested".into(),
                title: format!("Breaker R{round} falsified `{}`", task.key),
                detail: Some(summary.clone()),
                duration_ms: None,
                tokens: None,
                cost: None,
            });

            let key_findings = extract_key_findings(None, parsed.findings.as_deref(), &summary);
            let (updated_task_opt, circuit_outcome) = record_review_non_pass(
                &core.drove.store,
                &task.id,
                max_rounds,
                "changes_requested",
                &summary,
                key_findings,
                Some(&attempt.id),
            );
            let total_rounds = updated_task_opt
                .as_ref()
                .and_then(|t| t.review_circuit.as_ref())
                .map_or(round, |rc| rc.review_rounds);
            let reject_msg = format!(
                "Breaker round {total_rounds}/{max_rounds} reproduced a defect in [{}] (`{verify_cmd}` failed; artifact `{brk_art_id}.patch`):\n{}\n\n--- Failure Output ---\n{out_tail}",
                touched.join(", "),
                parsed.summary
            );

            return match circuit_outcome {
                ReviewCircuitOutcome::HandoffToCoordinator | ReviewCircuitOutcome::AwaitingUserDecision => {
                    let _ = super::integrate::sync_worktree_for_coordinator_handoff(
                        core,
                        run,
                        author_worker.worktree.as_deref(),
                    )
                    .await;
                    let _ = core.drove.store.update_attempt(&attempt.id, |a| {
                        a.outcome = Some(Outcome::Failed);
                        a.ended_at = Some(now_iso());
                        a.failure = Some(AttemptFailure { kind: FailureKind::ReviewRejected, message: reject_msg.clone() });
                    });
                    mark_worker_idle(core, &author_worker.id);
                    let failed_task = mark_task_failed_and_propagate(core, &run.id, &task.id)
                        .or(updated_task_opt)
                        .unwrap_or_else(|| task.clone());
                    emit_review_circuit_escalation(
                        core,
                        run,
                        &failed_task,
                        circuit_outcome,
                        Some(&attempt.id),
                        Some(&author_worker.id),
                    )
                    .await;
                    let err_code = if circuit_outcome == ReviewCircuitOutcome::AwaitingUserDecision {
                        "review_circuit_open"
                    } else {
                        "review_handoff_to_coordinator"
                    };
                    let worker_stop_msg = format!(
                        "{reject_msg}\n\n[DROVE CIRCUIT BREAKER TRIPPED — ATTEMPT CLOSED] Maximum review rounds reached; this task has been handed off to the Coordinator and candidate changes have been synced. STOP IMMEDIATELY: do NOT edit any more files, do NOT run further commands, and do NOT call `drove_finish` again. End your turn now."
                    );
                    Err(ApiErr::new(422, err_code, worker_stop_msg))
                }
                ReviewCircuitOutcome::ContinueNormal => {
                    Err(ApiErr::new(422, "review_changes_requested", reject_msg))
                }
            };
        }

        if vres.success && spec.keep_regression_tests == Some(true) {
            super::integrate::copy_breaker_tests_into_candidate(&brk_cwd, work_cwd, &touched);
        }
    }

    if spec.mode == Some(ReviewMode::Breaker) {
        let pass_summary = format!("Breaker R{round} passed (`{verify_cmd}`): {}", parsed.summary);
        let round_info = AttemptReview {
            round,
            max_rounds: Some(max_rounds),
            base_tree_sha: None,
            tree_sha: tree_sha.clone(),
            patch_artifact_id: Some(artifact_id.to_string()),
            delta_patch_artifact_id: delta_artifact_id,
            verdict: Some("approved".into()),
            summary: Some(pass_summary.clone()),
            findings: None,
        };
        let _ = core.drove.store.update_attempt(&attempt.id, |a| a.review_round = Some(round_info));
        let _ = core.drove.store.update_task(&task.id, |t| {
            t.review_state = Some(ReviewState {
                round,
                reviewer_worker_id: Some(brk_worker_id.clone()),
                base_tree_sha: None,
                last_reviewed_tree_sha: tree_sha.clone(),
                current_tree_sha: tree_sha.clone(),
                approved_tree_sha: tree_sha.clone(),
                patch_artifact_id: Some(artifact_id.to_string()),
                findings: None,
                last_summary: Some(pass_summary.clone()),
            });
        });
        core.drove.store.append_daemon_event(NewDaemonEvent {
            run_id: Some(run.id.clone()),
            task_id: Some(task.id.clone()),
            attempt_id: Some(attempt.id.clone()),
            worker_id: Some(brk_worker_id),
            message_id: None,
            category: "review".into(),
            kind: format!("breaker_r{round}"),
            status: "ok".into(),
            title: format!("Breaker R{round} on `{}`: pass", task.key),
            detail: Some(parsed.summary),
            duration_ms: None,
            tokens: None,
            cost: None,
        });
    }
    Ok(true)
}

fn build_reviewer_candidates(
    role_cfg: &RoleConfig,
    spec: &ReviewSpec,
    author_agent: &str,
    quotas: Option<&[QuotaCard]>,
    run_workers: &[Worker],
    run_attempts: &[Attempt],
) -> Vec<RoleTarget> {
    if let Some(explicit_agent) = spec.agent.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let matched =
            std::iter::once(&role_cfg.primary).chain(role_cfg.fallbacks.iter()).find(|t| t.agent == explicit_agent);
        return vec![RoleTarget {
            agent: explicit_agent.to_string(),
            model: spec.model.clone().or_else(|| matched.and_then(|t| t.model.clone())),
            effort: spec.effort.unwrap_or_else(|| matched.map_or(role_cfg.primary.effort, |t| t.effort)),
            fast: spec.fast.unwrap_or_else(|| matched.is_some_and(|t| t.fast)),
            context: matched.and_then(|t| t.context.clone()),
            strengths: matched.map(|t| t.strengths.clone()).unwrap_or_default(),
            note: matched.and_then(|t| t.note.clone()),
        }];
    }

    let mut list: Vec<RoleTarget> = Vec::new();
    for raw in std::iter::once(&role_cfg.primary).chain(role_cfg.fallbacks.iter()) {
        let mut cand = raw.clone();
        if let Some(eff) = spec.effort {
            cand.effort = eff;
        }
        if let Some(fast) = spec.fast {
            cand.fast = fast;
        }
        if let Some(m) = spec.model.as_ref()
            && raw.agent == role_cfg.primary.agent
        {
            cand.model = Some(m.clone());
        }
        if !list.iter().any(|existing| existing.agent == cand.agent && existing.model == cand.model) {
            list.push(cand);
        }
    }

    if let Some(cards) = quotas {
        let avail: Vec<RoleTarget> = list
            .iter()
            .filter(|c| super::magpie::quota_exhausted_reason(&c.agent, c.model.as_deref(), cards).is_none())
            .cloned()
            .collect();
        if !avail.is_empty() {
            list = avail;
        }
    }

    let is_failed_cand = |c: &RoleTarget| {
        run_attempts.iter().any(|a| {
            a.kind.as_deref() == Some("review")
                && a.outcome == Some(Outcome::Failed)
                && a.agent == c.agent
                && (c.model.is_none() || a.model == c.model)
                && a.failure.as_ref().is_some_and(|f| {
                    matches!(
                        f.kind,
                        FailureKind::AgentError
                            | FailureKind::Timeout
                            | FailureKind::ProcessExited
                            | FailureKind::QuotaExhausted
                    )
                })
        })
    };
    let has_warm_reviewer = |c: &RoleTarget| {
        run_workers.iter().any(|w| {
            w.state == WorkerState::Idle
                && w.role.as_deref() == Some("reviewer")
                && w.read_only_dir.is_some()
                && w.agent == c.agent
                && w.h2_agent.is_some()
        })
    };

    list.sort_by_key(|c| {
        let failed_rank = u8::from(is_failed_cand(c));
        let same_agent_rank = if spec.different_agent == Some(true) { u8::from(c.agent == author_agent) } else { 0 };
        let cold_rank = u8::from(!has_warm_reviewer(c));
        (failed_rank, same_agent_rank, cold_rank)
    });
    list
}

fn mark_reviewer_attempt_failed(
    core: &Core,
    run: &Run,
    task: &Task,
    rev_worker_id: &str,
    rev_att_id: &str,
    rev_agent: &str,
    err_msg: &str,
) {
    let _ = core.drove.store.update_worker(rev_worker_id, |w| {
        w.state = WorkerState::Dead;
        w.current_attempt_id = None;
        w.last_active_at = now_iso();
    });
    let _ = core.drove.store.update_attempt(rev_att_id, |a| {
        a.outcome = Some(Outcome::Failed);
        a.ended_at = Some(now_iso());
        a.failure = Some(AttemptFailure { kind: FailureKind::AgentError, message: err_msg.to_string() });
    });
    core.drove.store.append_daemon_event(NewDaemonEvent {
        run_id: Some(run.id.clone()),
        task_id: Some(task.id.clone()),
        attempt_id: Some(rev_att_id.to_string()),
        worker_id: Some(rev_worker_id.to_string()),
        message_id: None,
        category: "review".into(),
        kind: "reviewer_error".into(),
        status: "warn".into(),
        title: format!("Reviewer `{rev_agent}` failed on `{}`", task.key),
        detail: Some(err_msg.to_string()),
        duration_ms: None,
        tokens: None,
        cost: None,
    });
}

/// Filters `diff_touched_paths(&run.cwd, ...)` during concurrent runs so legitimate merges or
/// writes into `run.cwd` by sibling tasks are never misattributed to a read-only Reviewer or Breaker.
fn filter_concurrent_shared_cwd_changes(
    core: &Core,
    run_id: &str,
    current_task_id: &str,
    run_cwd: &str,
    agent_id: u32,
    start_idx: usize,
    raw_diffs: Vec<String>,
) -> Vec<String> {
    if raw_diffs.is_empty() {
        return raw_diffs;
    }
    let sibling_tasks = core.drove.store.list_tasks(run_id);
    let mut sibling_owned: Vec<String> = Vec::new();
    for t in &sibling_tasks {
        if t.id == current_task_id || t.template == Some(true) {
            continue;
        }
        if let Some(res) = &t.result
            && let Some(modified) = &res.files_modified
        {
            for m in modified {
                let norm = m.trim().replace('\\', "/").trim_start_matches("./").to_string();
                if !norm.is_empty() {
                    sibling_owned.push(norm);
                }
            }
        }
        if matches!(t.status, TaskStatus::Running | TaskStatus::Succeeded) {
            for f in &t.files {
                let norm = f.trim().replace('\\', "/").trim_start_matches("./").trim_end_matches('/').to_string();
                if !norm.is_empty() {
                    sibling_owned.push(norm);
                }
            }
            let tgt = t.spec.target.trim().replace('\\', "/").trim_start_matches("./").trim_end_matches('/').to_string();
            if !tgt.is_empty() && (tgt.contains('/') || tgt.contains('.')) && !tgt.contains(' ') {
                sibling_owned.push(tgt);
            }
        }
    }

    let mutating_haystack: String = core
        .read_transcript_entries(agent_id)
        .unwrap_or_default()
        .into_iter()
        .skip(start_idx)
        .filter_map(|e| match e {
            Entry::ToolCall { title, tool_kind, detail, .. } => {
                let kind = tool_kind.as_deref().unwrap_or("").to_ascii_lowercase();
                if matches!(kind.as_str(), "read" | "search" | "think" | "fetch" | "list" | "glob" | "grep") {
                    None
                } else {
                    Some(format!("{title}\n{}", detail.unwrap_or_default()))
                }
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let norm_run_cwd = run_cwd.trim_end_matches('/').replace('\\', "/");
    raw_diffs
        .into_iter()
        .filter(|p| !super::roles::is_ephemeral_cache_path(p))
        .filter(|rel| {
            let norm_rel = rel.replace('\\', "/");
            let claimed_by_sibling = sibling_owned.iter().any(|own| {
                own == &norm_rel
                    || norm_rel.strip_prefix(own.as_str()).is_some_and(|r| r.starts_with('/'))
            });
            if claimed_by_sibling && !mutating_haystack.contains(&format!("{norm_run_cwd}/{norm_rel}")) {
                return false;
            }
            if mutating_haystack.is_empty() {
                return false;
            }
            let basename = norm_rel.rsplit('/').next().unwrap_or(&norm_rel);
            mutating_haystack.contains(&norm_rel)
                || mutating_haystack.contains(&norm_run_cwd)
                || (!basename.is_empty() && mutating_haystack.contains(basename))
        })
        .collect()
}

fn collect_reviewer_turn_output(core: &Core, agent_id: u32, start_idx: usize) -> (String, Option<String>) {
    let Ok(entries) = core.read_transcript_entries(agent_id) else {
        return (String::new(), None);
    };
    let mut parts = Vec::new();
    let mut err_msg = None;
    for e in entries.into_iter().skip(start_idx) {
        match e {
            Entry::Assistant { text, .. } => parts.push(text),
            Entry::Error { message, .. } => {
                err_msg = Some(message);
            }
            _ => {}
        }
    }
    (parts.join(""), err_msg)
}

pub fn looks_like_provider_error(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    for (idx, _) in trimmed.match_indices('{') {
        let mut de = serde_json::Deserializer::from_str(&trimmed[idx..]).into_iter::<serde_json::Value>();
        if let Some(Ok(v)) = de.next()
            && let Some(obj) = v.as_object()
        {
            if let Some(err) = obj.get("error") {
                let msg =
                    err.get("message").and_then(|m| m.as_str()).map(String::from).unwrap_or_else(|| err.to_string());
                return Some(msg);
            }
            if obj.get("type").and_then(|t| t.as_str()) == Some("error")
                && let Some(msg) = obj.get("message").and_then(|m| m.as_str())
            {
                return Some(msg.to_string());
            }
        }
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("model_not_found") || lower.contains("invalid_request_error") {
        return Some(trimmed.chars().take(240).collect());
    }
    None
}

/// Parses a structured `ReviewVerdict` JSON object when present, ignoring non-verdict prose or placeholders.
pub fn try_parse_review_verdict(raw: &str) -> Option<ReviewVerdict> {
    if let Ok(v) = serde_json::from_str::<ReviewVerdict>(raw.trim())
        && matches!(v.verdict.as_str(), "approve" | "approved" | "changes_requested" | "reject" | "rejected")
        && v.summary != "..."
    {
        return Some(v);
    }
    for (idx, _) in raw.match_indices('{') {
        let mut de = serde_json::Deserializer::from_str(&raw[idx..]).into_iter::<ReviewVerdict>();
        if let Some(Ok(v)) = de.next()
            && matches!(v.verdict.as_str(), "approve" | "approved" | "changes_requested" | "reject" | "rejected")
            && v.summary != "..."
        {
            return Some(v);
        }
    }
    None
}

/// Parses a `ReviewVerdict` JSON object even when surrounded by markdown fences or prose.
pub fn parse_review_verdict(raw: &str) -> ReviewVerdict {
    if let Some(v) = try_parse_review_verdict(raw) {
        return v;
    }
    ReviewVerdict {
        verdict: "changes_requested".into(),
        summary: if raw.trim().is_empty() {
            "Reviewer produced no structured verdict JSON".into()
        } else {
            raw.trim().chars().take(300).collect()
        },
        findings: None,
    }
}

fn capture_candidate_patch(work_cwd: &str, baseline: &super::roles::DirtyBaseline) -> String {
    if let Some(cur_tree) = git_tree_sha(work_cwd) {
        let base_tree = baseline.tree_sha.clone().or_else(|| {
            super::bg_std_command("git")
                .args(["-C", work_cwd, "rev-parse", "--verify", "HEAD^{tree}"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
        });
        if let Some(base_t) = base_tree {
            if base_t == cur_tree {
                return "No workspace changes\n".into();
            }
            if let Some(diff) = git_diff_trees(work_cwd, &base_t, &cur_tree) {
                if diff.trim().is_empty() {
                    return "No workspace changes\n".into();
                }
                return diff;
            }
        }
    }
    let touched: Vec<String> = diff_touched_paths(work_cwd, baseline)
        .into_iter()
        .filter(|p| !super::roles::is_ephemeral_cache_path(p))
        .collect();
    if touched.is_empty() {
        "No workspace changes\n".into()
    } else {
        format!("Modified files:\n{}\n", touched.join("\n"))
    }
}

fn git_tree_sha(work_cwd: &str) -> Option<String> {
    super::roles::compute_git_working_tree_sha(work_cwd)
}

fn git_diff_trees(work_cwd: &str, a: &str, b: &str) -> Option<String> {
    let mut diff_cmd = super::bg_std_command("git");
    diff_cmd.args(["-C", work_cwd, "diff", a, b, "--", "."]);
    diff_cmd.args(super::roles::DIFF_CACHE_EXCLUDES);
    let out = diff_cmd.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn resolve_local_parent_addr(core: &Core, host_session: &str) -> Option<String> {
    let local = host_session.rsplit_once('/').map(|(_, r)| r).unwrap_or(host_session);
    let tk = crate::runtime::TargetKey::parse(local)?;
    let st = core.lock();
    match tk {
        crate::runtime::TargetKey::Terminal(id) => {
            st.pty_reg.terminals.get(&id).filter(|t| t.is_live()).map(|_| crate::terminals::address(id))
        }
        crate::runtime::TargetKey::Agent(id) => {
            st.reg.agents.get(&id).filter(|a| a.is_live()).map(|_| crate::agents::address(id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_error_detection_catches_codex_model_not_found() {
        let raw = "Warning: Model metadata for `gpt-6.2-sol` not found.\n\n{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"The 'gpt-6.2-sol' model is not supported when using Codex with a ChatGPT account.\",\"code\":\"model_not_found\"},\"status\":400,\"type\":\"error\"}\n";
        assert!(try_parse_review_verdict(raw).is_none());
        let err = looks_like_provider_error(raw).expect("expected provider error");
        assert!(err.contains("gpt-6.2-sol"), "{err}");
    }

    #[test]
    fn build_reviewer_candidates_orders_fallbacks_and_respects_different_agent() {
        let cfg = RoleConfig {
            label: None,
            summary: None,
            access: Some(RoleAccessMode::Readonly),
            allowed_paths: Vec::new(),
            primary: RoleTarget {
                agent: "codex".into(),
                model: Some("gpt-6.2-sol".into()),
                effort: Effort::High,
                fast: false,
                context: None,
                strengths: Vec::new(),
                note: None,
            },
            fallbacks: vec![RoleTarget {
                agent: "agy".into(),
                model: Some("gemini-4-carbon-high".into()),
                effort: Effort::High,
                fast: false,
                context: None,
                strengths: Vec::new(),
                note: None,
            }],
            prompt: None,
        };
        let spec = ReviewSpec {
            different_agent: Some(true),
            max_rounds: Some(1),
            ..Default::default()
        };
        let cands = build_reviewer_candidates(&cfg, &spec, "codex", None, &[], &[]);
        assert_eq!(cands.len(), 2);
        assert_eq!(cands[0].agent, "agy");
        assert_eq!(cands[1].agent, "codex");

        let failed_att = Attempt {
            id: "att_rev_fail".into(),
            task_id: "tsk_1".into(),
            run_id: "run_1".into(),
            worker_id: "wrk_rev_fail".into(),
            agent: "codex".into(),
            model: Some("gpt-6.2-sol".into()),
            effort: None,
            fast: None,
            route_reason: "review".into(),
            reused: false,
            retry_of: None,
            hint: None,
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            awaiting_late_done: None,
            tokens: TokenCounts::default(),
            cost: None,
            estimated: None,
            outcome: Some(Outcome::Failed),
            failure: Some(AttemptFailure { kind: FailureKind::AgentError, message: "model_not_found".into() }),
            result: None,
            kind: Some("review".into()),
            review_round: None,
            verifying: None,
            verify_rejections: None,
            stagnant_rejections: None,
            last_verify_hash: None,
            last_tree_sha: None,
            verify_log_artifact_id: None,
            verify_ai_digest: None,
            ai_diagnosis: None,
            spinning_detected_at: None,
        };
        let cands_after_fail = build_reviewer_candidates(
            &cfg,
            &ReviewSpec { max_rounds: Some(1), ..Default::default() },
            "claude",
            None,
            &[],
            &[failed_att],
        );
        assert_eq!(cands_after_fail[0].agent, "agy");
        assert_eq!(cands_after_fail[1].agent, "codex");
    }

    #[test]
    fn review_circuit_state_machine_enforces_two_rounds_handoff_then_single_post_handoff_then_hard_stop() {
        let dir = std::env::temp_dir().join(format!("herdr2-review-cb-{}", super::super::crypto::rand_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = crate::paths::Paths {
            state: dir.clone(),
            config: dir.join("config.toml"),
            socket: dir.join("core.sock"),
            holder_socket: dir.join("core.holder.sock"),
        };
        let dpaths = super::super::config::DrovePaths::from_paths(&paths);
        let store = super::super::store::Store::open(&dpaths).unwrap();
        let run = store.create_run(super::super::store::CreateRunInput {
            objective: "test review circuit".into(),
            cwd: dir.display().to_string(),
            host: "herdr2".into(),
            host_session: "t1".into(),
            agent: None,
            model: None,
            host_pid: None,
            session_bound: None,
            max_concurrency: 2,
            budget: None,
            verify: None,
            goal: None,
        });
        let task = store.create_task(super::super::store::CreateTaskInput {
            run_id: run.id.clone(),
            key: "final-review".into(),
            title: "Final Review".into(),
            phase: None,
            spec: Spec {
                target: "workspace".into(),
                change: "Review".into(),
                constraints: None,
                ownership: None,
                acceptance: "Approved".into(),
                verify: None,
            },
            tags: Vec::new(),
            files: Vec::new(),
            deps: Vec::new(),
            output_schema: None,
            isolation: Isolation::None,
            timeout_sec: None,
            stall_warn_sec: None,
            stall_kill_sec: None,
            when: None,
            assign: Some(Assign { role: Some("reviewer".into()), ..Default::default() }),
            status: TaskStatus::Ready,
            result: None,
            kind: None,
            trigger_rule: None,
            context: None,
            exec: None,
            foreach_from: None,
            template: None,
            expanded_from: None,
            review: None,
            cache: None,
            cached_from: None,
            retry_on_worker_reported: None,
            fork_from: None,
            fork_mode: None,
            channels: Vec::new(),
        });
        assert!(is_standalone_reviewer_task(&task));

        // Round 1 fails -> still Normal (1/2).
        let (t1, out1) = record_review_non_pass(
            &store,
            &task.id,
            DEFAULT_MAX_NORMAL_REVIEW_ROUNDS,
            "changes_requested",
            "R1 issue",
            vec!["missing timeout".into()],
            Some("att_1"),
        );
        assert_eq!(out1, ReviewCircuitOutcome::ContinueNormal);
        let rc1 = t1.unwrap().review_circuit.unwrap();
        assert_eq!(rc1.stage, ReviewCircuitStage::Normal);
        assert_eq!(rc1.review_rounds, 1);
        assert!(!rc1.handoff_to_coordinator);

        // Round 2 fails -> trips HandoffToCoordinator (2/2).
        let (t2, out2) = record_review_non_pass(
            &store,
            &task.id,
            DEFAULT_MAX_NORMAL_REVIEW_ROUNDS,
            "changes_requested",
            "R2 issue",
            vec!["race condition".into()],
            Some("att_2"),
        );
        assert_eq!(out2, ReviewCircuitOutcome::HandoffToCoordinator);
        let rc2 = t2.unwrap().review_circuit.unwrap();
        assert_eq!(rc2.stage, ReviewCircuitStage::HandoffToCoordinator);
        assert_eq!(rc2.review_rounds, 2);
        assert!(rc2.handoff_to_coordinator);
        assert!(!rc2.post_handoff_review_used);
        assert!(!rc2.awaiting_user_decision);
        assert_eq!(rc2.history.len(), 2);

        // Simulate Coordinator triggering the single post-handoff review.
        store.update_task(&task.id, |t| {
            let rc = t.review_circuit.as_mut().unwrap();
            rc.stage = ReviewCircuitStage::PostHandoffReview;
            rc.post_handoff_review_used = true;
        });

        // Round 3 (post-handoff) fails -> trips hard AwaitingUserDecision!
        let (t3, out3) = record_review_non_pass(
            &store,
            &task.id,
            DEFAULT_MAX_NORMAL_REVIEW_ROUNDS,
            "rejected",
            "R3 disagreement",
            vec!["second-order fault injection".into()],
            Some("att_3"),
        );
        assert_eq!(out3, ReviewCircuitOutcome::AwaitingUserDecision);
        let rc3 = t3.unwrap().review_circuit.unwrap();
        assert_eq!(rc3.stage, ReviewCircuitStage::AwaitingUserDecision);
        assert_eq!(rc3.review_rounds, 3);
        assert!(rc3.handoff_to_coordinator);
        assert!(rc3.post_handoff_review_used);
        assert!(rc3.awaiting_user_decision);
        assert!(rc3.history.last().unwrap().post_handoff);

        // Approval clears circuit cleanly.
        record_review_passed(&store, &task.id);
        let rc4 = store.get_task(&task.id).unwrap().review_circuit.unwrap();
        assert_eq!(rc4.stage, ReviewCircuitStage::Cleared);
        assert!(!rc4.awaiting_user_decision);

        // Helper checks for structured verdicts & findings.
        assert_eq!(
            is_review_rejection_data(Some(&serde_json::json!({ "review_status": "changes_requested" })), None)
                .as_deref(),
            Some("changes_requested")
        );
        assert_eq!(
            is_review_rejection_data(Some(&serde_json::json!({ "verdict": "reject" })), None).as_deref(),
            Some("reject")
        );
        assert_eq!(
            is_review_rejection_data(Some(&serde_json::json!({ "approved": false })), None).as_deref(),
            Some("changes_requested")
        );
        assert_eq!(
            is_review_rejection_data(None, Some(&["blocking defect".into()])).as_deref(),
            Some("changes_requested")
        );
        assert!(is_review_approval_data(Some(&serde_json::json!({ "reviewStatus": "approved" }))));
        assert!(is_review_approval_data(Some(&serde_json::json!({ "verdict": "lgtm" }))));
        assert!(is_review_approval_data(Some(&serde_json::json!({ "approved": true }))));
        assert!(!is_review_approval_data(Some(&serde_json::json!({ "approved": false }))));

        let extracted = extract_key_findings(
            Some(&serde_json::json!({ "findings": [{ "severity": "high", "file": "a.rs", "message": "leak" }] })),
            None,
            "fallback",
        );
        assert_eq!(extracted, vec!["[high] a.rs: leak"]);
        assert_eq!(
            extract_key_findings(
                Some(&serde_json::json!({ "blockers": ["missing lock", "race on drop"] })),
                None,
                "fallback",
            ),
            vec!["missing lock", "race on drop"]
        );
        assert_eq!(extract_key_findings(None, None, "fallback summary"), vec!["fallback summary"]);

        // History caps at 8 entries even across repeated override failures.
        for i in 0..8 {
            let _ = record_review_non_pass(
                &store,
                &task.id,
                DEFAULT_MAX_NORMAL_REVIEW_ROUNDS,
                "changes_requested",
                &format!("extra {i}"),
                Vec::new(),
                None,
            );
        }
        let capped = store.get_task(&task.id).unwrap().review_circuit.unwrap();
        assert_eq!(capped.history.len(), 8);
        assert_eq!(capped.stage, ReviewCircuitStage::AwaitingUserDecision);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn candidate_patch_excludes_preexisting_dirty_files_and_preserves_git_index() {
        let dir = std::env::temp_dir().join(format!("herdr2-patch-iso-{}", super::super::crypto::rand_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = dir.to_str().unwrap();
        for args in [
            &["init"][..],
            &["config", "user.email", "t@drove.local"][..],
            &["config", "user.name", "Drove"][..],
        ] {
            assert!(super::super::bg_std_command("git").arg("-C").arg(&dir).args(args).output().unwrap().status.success());
        }
        std::fs::write(dir.join("committed.txt"), "base\n").unwrap();
        std::fs::write(dir.join("user_dirty.txt"), "clean\n").unwrap();
        assert!(
            super::super::bg_std_command("git")
                .args(["-C", cwd, "add", "."])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(
            super::super::bg_std_command("git")
                .args(["-C", cwd, "commit", "-m", "init"])
                .output()
                .unwrap()
                .status
                .success()
        );

        // User has pre-existing unstaged + untracked changes BEFORE the task starts (`isolation: none`).
        std::fs::write(dir.join("user_dirty.txt"), "user-uncommitted-edit\n").unwrap();
        std::fs::write(dir.join("user_untracked.txt"), "user-scratch\n").unwrap();
        let index_before = std::fs::read(dir.join(".git/index")).unwrap();

        let baseline = super::super::roles::capture_dirty_baseline(cwd);
        assert!(baseline.tree_sha.is_some());

        // Task author modifies `committed.txt` and creates `task_new.txt`.
        std::fs::write(dir.join("committed.txt"), "author-updated\n").unwrap();
        std::fs::write(dir.join("task_new.txt"), "author-new\n").unwrap();

        let patch = capture_candidate_patch(cwd, &baseline);
        let index_after = std::fs::read(dir.join(".git/index")).unwrap();

        assert_eq!(index_before, index_after, "capture_candidate_patch must never mutate .git/index");
        assert!(patch.contains("committed.txt") && patch.contains("+author-updated"), "missing author edit: {patch}");
        assert!(patch.contains("task_new.txt") && patch.contains("+author-new"), "missing author new file: {patch}");
        assert!(!patch.contains("user_dirty.txt"), "leaked pre-existing unstaged file into review patch: {patch}");
        assert!(!patch.contains("user_untracked.txt"), "leaked pre-existing untracked file into review patch: {patch}");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn breaker_verdict_parser_and_scope_globs_enforce_test_only_mutations() {
        let broken = try_parse_breaker_verdict(
            r#"Found race condition: {"verdict":"broken","summary":"panics on empty slice","findings":[{"severity":"high","file":"tests/edge_test.rs","message":"panicked at index 0"}]}"#,
        )
        .expect("should parse broken verdict");
        assert!(is_breaker_broken_claim(&broken.verdict));
        assert_eq!(broken.summary, "panics on empty slice");

        let passed = try_parse_breaker_verdict(r#"{"verdict":"pass","summary":"All boundary tests green"}"#)
            .expect("should parse pass verdict");
        assert!(!is_breaker_broken_claim(&passed.verdict));

        let scope_cfg = RoleConfig {
            label: None,
            summary: None,
            access: Some(RoleAccessMode::ScopedWrite),
            allowed_paths: default_breaker_test_globs(),
            primary: RoleTarget {
                agent: "codex".into(),
                model: None,
                effort: Effort::High,
                fast: false,
                context: None,
                strengths: Vec::new(),
                note: None,
            },
            fallbacks: Vec::new(),
            prompt: None,
        };
        assert!(
            super::super::roles::find_scope_violations(
                &scope_cfg,
                &["tests/auth_edge.rs".into(), "crates/api/src/foo_test.rs".into()]
            )
            .is_empty()
        );
        assert_eq!(
            super::super::roles::find_scope_violations(
                &scope_cfg,
                &["tests/auth_edge.rs".into(), "src/auth.rs".into()]
            ),
            vec!["src/auth.rs".to_string()]
        );
    }
}
