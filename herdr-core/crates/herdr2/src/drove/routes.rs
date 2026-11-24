//! M2+M3 orchestration and worker protocol HTTP handlers called by `drove::http`.

use super::DroveRuntime;
use super::http::ApiErr;
use super::notify::notify_coordinator;
use super::pool::{mark_worker_idle, send_to_task_worker, stop_run_workers, stop_worker};
use super::roles::{diff_touched_paths, find_scope_violations};
use super::store::{NewDaemonEvent, PostMessageInput, empty_task_counts};
use super::types::*;
use crate::runtime::Core;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct AskGuard {
    drove: Arc<DroveRuntime>,
    attempt_id: String,
}

impl Drop for AskGuard {
    fn drop(&mut self) {
        self.drove.leave_ask(&self.attempt_id);
    }
}

pub(super) fn header<'a>(headers: &'a BTreeMap<String, String>, k1: &str, k2: &str) -> Option<&'a str> {
    headers.get(k1).or_else(|| headers.get(k2)).map(String::as_str)
}

pub(super) fn authenticate_worker(
    drove: &DroveRuntime,
    headers: &BTreeMap<String, String>,
    body: &Value,
) -> Result<Attempt, ApiErr> {
    let tok = header(headers, "x-drove-worker-token", "x-co-worker-token")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::new(401, "unauthorized", "missing worker token"))?;
    let body_worker_id = body["workerId"]
        .as_str()
        .or_else(|| header(headers, "x-drove-worker-id", "x-co-worker-id"))
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let req_att_id = body["attemptId"].as_str().map(str::trim).filter(|s| !s.is_empty());
    let explicit_attempt = body["explicitAttempt"].as_bool() == Some(true);

    let (worker_id, requested_att) = if let Some(wid) = body_worker_id {
        (wid.to_string(), req_att_id.and_then(|id| drove.store.get_attempt(id)))
    } else if let Some(att_id) = req_att_id {
        let att = drove.store.get_attempt(att_id).ok_or_else(|| ApiErr::new(404, "not_found", "Attempt not found"))?;
        (att.worker_id.clone(), Some(att))
    } else {
        return Err(ApiErr::new(400, "bad_request", "workerId or attemptId must be a non-empty string"));
    };

    if !drove.verify_worker_token(&worker_id, tok) {
        return Err(ApiErr::new(401, "unauthorized", "invalid worker token"));
    }
    let worker =
        drove.store.get_worker(&worker_id).ok_or_else(|| ApiErr::new(404, "not_found", "Worker not found"))?;
    if matches!(worker.state, WorkerState::Dead | WorkerState::Closed) {
        return Err(ApiErr::new(409, "stale_attempt", "Worker is no longer active"));
    }
    let active_att_id = worker
        .current_attempt_id
        .as_deref()
        .ok_or_else(|| ApiErr::new(409, "stale_attempt", "worker has no active attempt"))?;

    if explicit_attempt {
        let Some(req_id) = req_att_id else {
            return Err(ApiErr::new(400, "bad_request", "attemptId must be a non-empty string"));
        };
        if req_id != active_att_id {
            return Err(ApiErr::new(
                409,
                "stale_attempt",
                format!("Attempt `{req_id}` has already ended or no longer belongs to this worker (active: `{active_att_id}`)"),
            ));
        }
    }

    // Prefer the worker's live active attempt so reused workers with a frozen `H2_DROVE_ATTEMPT_ID`
    // env var from their initial attempt seamlessly operate on their current attempt.
    let att = match requested_att {
        Some(a) if a.id == active_att_id && a.worker_id == worker.id => a,
        _ => drove
            .store
            .get_attempt(active_att_id)
            .ok_or_else(|| ApiErr::new(409, "stale_attempt", "active attempt missing"))?,
    };
    if att.outcome.is_some() || att.ended_at.is_some() {
        return Err(ApiErr::new(409, "stale_attempt", "Attempt has already ended or no longer belongs to this worker"));
    }
    Ok(att)
}

pub(super) fn worker_context_http(
    drove: &DroveRuntime,
    headers: &BTreeMap<String, String>,
    body: &Value,
) -> Result<Value, ApiErr> {
    let tok = header(headers, "x-drove-worker-token", "x-co-worker-token")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::new(401, "unauthorized", "missing worker token"))?;
    let worker_id = if let Some(wid) = body["workerId"]
        .as_str()
        .or_else(|| header(headers, "x-drove-worker-id", "x-co-worker-id"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        wid.to_string()
    } else if let Some(att_id) = body["attemptId"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        drove
            .store
            .get_attempt(att_id)
            .ok_or_else(|| ApiErr::new(404, "not_found", "Attempt not found"))?
            .worker_id
    } else {
        return Err(ApiErr::new(400, "bad_request", "workerId must be a non-empty string"));
    };
    let worker = drove.store.get_worker(&worker_id).ok_or_else(|| ApiErr::new(404, "not_found", "Worker not found"))?;
    if !drove.verify_worker_token(&worker_id, tok) {
        return Err(ApiErr::new(401, "unauthorized", "invalid worker token"));
    }
    let att_id =
        worker.current_attempt_id.ok_or_else(|| ApiErr::new(409, "stale_attempt", "worker has no active attempt"))?;
    let att = drove.store.get_attempt(&att_id).ok_or_else(|| ApiErr::new(409, "stale_attempt", "attempt missing"))?;
    Ok(json!({
        "workerId": worker.id,
        "currentAttempt": att.id,
        "taskId": att.task_id,
        "runId": att.run_id,
    }))
}

pub(super) async fn worker_ask_http(
    core: &Arc<Core>,
    headers: &BTreeMap<String, String>,
    body: &Value,
) -> Result<Value, ApiErr> {
    let att = authenticate_worker(&core.drove, headers, body)?;
    let timeout_sec = body["timeoutSec"].as_u64().unwrap_or(300).clamp(1, 3600);
    let question = body["question"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::new(400, "bad_request", "question must be non-empty"))?
        .to_string();
    let options: Option<Vec<String>> = body["options"].as_array().map(|arr| {
        arr.iter().filter_map(|v| v.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect()
    });
    let recommend = body["recommend"].as_str().map(String::from);
    let why = body["why"].as_str().map(String::from);
    let run = core.drove.store.get_run(&att.run_id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    let task = core.drove.store.get_task(&att.task_id);
    let msg = core.drove.store.post_message(PostMessageInput {
        run_id: att.run_id.clone(),
        kind: MessageType::Question,
        from: format!("worker:{}", att.worker_id),
        to: "coordinator".into(),
        thread_id: None,
        task_id: Some(att.task_id.clone()),
        attempt_id: Some(att.id.clone()),
        subject: format!("Question from {}", task.as_ref().map(|t| t.key.as_str()).unwrap_or(&att.task_id)),
        body: Some(question.clone()),
        options,
        recommend,
        why,
        raised_by: Some("worker".into()),
        raised_by_worker: Some(att.worker_id.clone()),
        tag: None,
        ai_draft: None,
        needs_action: true,
    });

    core.drove.enter_ask(&att.id);
    let _guard = AskGuard { drove: core.drove.clone(), attempt_id: att.id.clone() };
    super::ai::spawn_question_draft(
        core.clone(),
        att.run_id.clone(),
        msg.id.clone(),
        question.clone(),
        msg.options.clone(),
    );
    let tkey = task.as_ref().map(|t| t.key.as_str()).unwrap_or(&att.task_id);
    notify_coordinator(
        core,
        &run,
        "question",
        &format!("Question on `{tkey}`"),
        &format!("[drove] Question ({}) on `{tkey}`: {question}", msg.id),
        Some(&att.task_id),
        Some(&att.id),
        Some(&att.worker_id),
        Some(&msg.id),
    )
    .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_sec);
    loop {
        if let Some(reply) = core.drove.store.latest_reply_for(&att.run_id, &msg.id) {
            return Ok(json!({ "questionId": msg.id, "reply": reply }));
        }
        let Some(cur_att) = core.drove.store.get_attempt(&att.id) else {
            return Err(ApiErr::new(409, "stale_attempt", "attempt removed"));
        };
        if cur_att.outcome.is_some() || cur_att.ended_at.is_some() {
            return Err(ApiErr::new(409, "stale_attempt", "attempt ended while waiting for reply"));
        }
        let woken = core.drove.message_reply_wake.notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        if let Some(reply) = core.drove.store.latest_reply_for(&att.run_id, &msg.id) {
            return Ok(json!({ "questionId": msg.id, "reply": reply }));
        }
        if tokio::time::timeout_at(deadline, woken).await.is_err() {
            return Err(ApiErr::new(408, "timeout", format!("timed out waiting for reply to {}", msg.id)));
        }
    }
}

pub(super) fn worker_note_http(
    drove: &DroveRuntime,
    headers: &BTreeMap<String, String>,
    body: &Value,
) -> Result<Value, ApiErr> {
    let att = authenticate_worker(drove, headers, body)?;
    let text = body["body"]
        .as_str()
        .or_else(|| body["text"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::new(400, "bad_request", "note body must be non-empty"))?;
    let subject = body["subject"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("Worker note");
    let msg = drove.store.post_message(PostMessageInput {
        run_id: att.run_id.clone(),
        kind: MessageType::Note,
        from: format!("worker:{}", att.worker_id),
        to: "coordinator".into(),
        thread_id: None,
        task_id: Some(att.task_id.clone()),
        attempt_id: Some(att.id.clone()),
        subject: subject.into(),
        body: Some(text.into()),
        options: None,
        recommend: None,
        why: None,
        raised_by: Some("worker".into()),
        raised_by_worker: Some(att.worker_id.clone()),
        tag: None,
        ai_draft: None,
        needs_action: false,
    });
    Ok(json!({ "ok": true, "message": msg }))
}

pub(super) fn worker_read_http(
    drove: &DroveRuntime,
    headers: &BTreeMap<String, String>,
    body: &Value,
) -> Result<Value, ApiErr> {
    let att = authenticate_worker(drove, headers, body)?;
    let target = body["target"].as_str().map(str::trim).unwrap_or("");
    let offset = body["offset"].as_u64().unwrap_or(0) as usize;
    let limit = body["limit"].as_u64().unwrap_or(65536).clamp(1, 262144) as usize;
    let grep = body["grep"].as_str();

    if target == "verify" || target.starts_with("verify-") {
        let art = if target == "verify" {
            att.verify_log_artifact_id
                .as_deref()
                .ok_or_else(|| ApiErr::new(404, "not_found", "no verify log for this attempt"))?
        } else {
            target
        };
        let slice = drove
            .store
            .read_artifact_slice(&att.run_id, art, offset, limit, grep)
            .map_err(|e| ApiErr::new(404, "not_found", e.to_string()))?;
        return Ok(slice);
    }
    if target == "review" {
        let task = drove.store.get_task(&att.task_id).ok_or_else(|| ApiErr::new(404, "not_found", "Task not found"))?;
        let art = att
            .review_round
            .as_ref()
            .and_then(|r| r.delta_patch_artifact_id.as_deref().or(r.patch_artifact_id.as_deref()))
            .or_else(|| task.review_state.as_ref().and_then(|s| s.patch_artifact_id.as_deref()))
            .ok_or_else(|| ApiErr::new(404, "not_found", "no review patch available"))?;
        let slice = drove
            .store
            .read_artifact_slice(&att.run_id, &format!("{art}.patch"), offset, limit, grep)
            .map_err(|e| ApiErr::new(404, "not_found", e.to_string()))?;
        return Ok(slice);
    }
    if let Some(dep) = drove.store.find_task_in_run(&att.run_id, target) {
        return Ok(json!({ "task": dep.key, "status": dep.status, "result": dep.result }));
    }
    Err(ApiErr::new(404, "not_found", format!("unknown read target {target:?}")))
}

pub(super) async fn worker_done_http(
    core: &Arc<Core>,
    headers: &BTreeMap<String, String>,
    body: &Value,
) -> Result<Value, ApiErr> {
    let att = authenticate_worker(&core.drove, headers, body)?;
    core.drove.clear_loop_watermark(&att.id);
    core.ai.evict_session(&format!("drove:loop:{}", att.id));
    let run = core.drove.store.get_run(&att.run_id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    let task =
        core.drove.store.get_task(&att.task_id).ok_or_else(|| ApiErr::new(404, "not_found", "Task not found"))?;
    let worker =
        core.drove.store.get_worker(&att.worker_id).ok_or_else(|| ApiErr::new(404, "not_found", "Worker not found"))?;

    let outcome_raw = body["outcome"].as_str().map(|s| s.trim().to_ascii_lowercase()).unwrap_or_else(|| "done".into());
    if !matches!(outcome_raw.as_str(), "done" | "succeeded" | "failed") {
        return Err(ApiErr::new(400, "bad_request", "outcome must be done or failed"));
    }

    if outcome_raw == "failed" {
        let reason = body["reason"]
            .as_str()
            .or_else(|| body["summary"].as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Worker reported failure")
            .to_string();
        let ended = now_iso();
        let upd_att = core.drove.store.update_attempt(&att.id, |a| {
            a.outcome = Some(Outcome::Failed);
            a.ended_at = Some(ended);
            a.failure = Some(AttemptFailure { kind: FailureKind::WorkerReported, message: reason.clone() });
        });
        let no_retry = body["noRetry"].as_bool() == Some(true);
        // `worker_reported` is an explicit business/review conclusion (e.g., a read-only reviewer
        // reporting blocking defects). Never auto-retry unless the task explicitly opts in via
        // `retryOnWorkerReported: true`, so the task fails immediately and wakes `h2 drove wait`.
        if super::review::is_standalone_reviewer_task(&task) {
            let cap = task
                .review
                .as_ref()
                .and_then(|r| r.max_rounds)
                .unwrap_or(super::review::DEFAULT_MAX_NORMAL_REVIEW_ROUNDS);
            let findings = super::review::extract_key_findings(body.get("data"), None, &reason);
            let (updated_opt, outcome) = super::review::record_review_non_pass(
                &core.drove.store,
                &task.id,
                cap,
                "changes_requested",
                &reason,
                findings,
                Some(&att.id),
            );
            match outcome {
                super::review::ReviewCircuitOutcome::HandoffToCoordinator
                | super::review::ReviewCircuitOutcome::AwaitingUserDecision => {
                    let failed_task = super::pool::mark_task_failed_and_propagate(core, &run.id, &task.id)
                        .or(updated_opt)
                        .unwrap_or_else(|| task.clone());
                    super::review::emit_review_circuit_escalation(
                        core,
                        &run,
                        &failed_task,
                        outcome,
                        Some(&att.id),
                        Some(&worker.id),
                    )
                    .await;
                }
                super::review::ReviewCircuitOutcome::ContinueNormal => {
                    let allow_retry = !no_retry && task.retry_on_worker_reported == Some(true);
                    let max_r = if allow_retry { super::MAX_AUTO_RETRIES } else { 0 };
                    super::fail_or_retry_task(
                        core,
                        &run.id,
                        &task.id,
                        &att.id,
                        FailureKind::WorkerReported,
                        &reason,
                        max_r,
                    )
                    .await;
                }
            }
        } else {
            let allow_retry = !no_retry && task.retry_on_worker_reported == Some(true);
            let max_r = if allow_retry { super::MAX_AUTO_RETRIES } else { 0 };
            super::fail_or_retry_task(core, &run.id, &task.id, &att.id, FailureKind::WorkerReported, &reason, max_r)
                .await;
        }
        if let Some(wt) = worker.worktree.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            if super::integrate::is_aosp_harness(&run.cwd) {
                super::integrate::cleanup_worktree_dir(&run.cwd, std::path::Path::new(wt));
                let _ = core.drove.store.update_worker(&worker.id, |w| {
                    w.worktree = None;
                    w.cwd = run.cwd.clone();
                });
            } else {
                let wt_str = wt.to_string();
                let _ = tokio::task::spawn_blocking(move || super::integrate::reset_worktree_clean(&wt_str)).await;
            }
        }
        mark_worker_idle(core, &worker.id);
        core.drove.wake_scheduler();
        return Ok(json!({ "ok": true, "attempt": upd_att, "task": core.drove.store.get_task(&task.id) }));
    }

    let summary = body["summary"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::new(400, "bad_request", "summary must be non-empty when outcome is done"))?
        .to_string();

    // Enforce role file-access scope against attempt baseline.
    let role_name = worker.role.as_deref().unwrap_or("coder");
    let cfg = core.drove.config_snapshot();
    let role_cfg = super::config::resolve_role_config(&cfg, role_name)
        .unwrap_or_else(|| super::config::default_roles().remove("coder").unwrap());
    let work_cwd = worker.worktree.as_deref().unwrap_or(&worker.cwd);
    let baseline = core.drove.get_attempt_baseline(&att.id).unwrap_or_default();
    let touched = diff_touched_paths(work_cwd, &baseline);
    let violations = find_scope_violations(&role_cfg, &touched);
    if !violations.is_empty() {
        return Err(ApiErr::new(
            422,
            "role_scope_violation",
            format!(
                "Role `{role_name}` cannot modify these workspace file(s): {}. Revert or move edits before calling done.",
                violations.join(", ")
            ),
        ));
    }

    let data = body.get("data").filter(|v| !v.is_null()).cloned();
    if let Some(schema) = &task.output_schema {
        let data_val = data.as_ref().unwrap_or(&Value::Null);
        let errs = validate_json_schema(schema, data_val, "data");
        if !errs.is_empty() {
            return Err(ApiErr::new(422, "schema_validation_failed", errs.join("; ")));
        }
    }

    let files_modified = if !touched.is_empty() {
        Some(touched)
    } else {
        body["filesModified"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
    };

    // M3 Gate 1: Task `spec.verify`
    let _ = super::verify::run_task_verify_gate(core, &run, &task, &worker, &att, work_cwd).await?;
    let refreshed_att = core.drove.store.get_attempt(&att.id).unwrap_or(att);
    let refreshed_task = core.drove.store.get_task(&task.id).unwrap_or(task);

    // M3 Gate 2: Adversarial read-only Review loop
    super::review::run_adversarial_review_gate(core, &run, &refreshed_task, &worker, &refreshed_att, work_cwd).await?;

    // M3 Gate 3: Worktree serialized merge-back & warm retention
    let keep_wt = worker.worktree.is_some() && !super::integrate::is_aosp_harness(&run.cwd);
    let merged_files =
        super::integrate::merge_and_finalize_worktree(core, &run, worker.worktree.as_deref(), keep_wt).await?;
    let merged_or_touched = if !merged_files.is_empty() { Some(merged_files) } else { files_modified };

    let result = TaskResult {
        summary: summary.clone(),
        compact_summary: None,
        data,
        files_modified: merged_or_touched,
        blockers: None,
        output_file_hashes: None,
    };

    let ended = now_iso();
    let upd_att = core.drove.store.update_attempt(&refreshed_att.id, |a| {
        a.outcome = Some(Outcome::Succeeded);
        a.ended_at = Some(ended);
        a.result = Some(result.clone());
    });
    let mut upd_task = core.drove.store.update_task(&refreshed_task.id, |t| {
        t.status = TaskStatus::Succeeded;
        t.exec_phase = None;
        t.result = Some(result.clone());
    });
    let mut review_escalated = false;
    if super::review::is_standalone_reviewer_task(&refreshed_task) {
        if let Some(verdict) =
            super::review::is_review_rejection_data(result.data.as_ref(), result.blockers.as_deref())
        {
            let cap = refreshed_task
                .review
                .as_ref()
                .and_then(|r| r.max_rounds)
                .unwrap_or(super::review::DEFAULT_MAX_NORMAL_REVIEW_ROUNDS);
            let findings = super::review::extract_key_findings(result.data.as_ref(), None, &summary);
            let (rc_task, outcome) = super::review::record_review_non_pass(
                &core.drove.store,
                &refreshed_task.id,
                cap,
                &verdict,
                &summary,
                findings,
                Some(&refreshed_att.id),
            );
            if let Some(t) = rc_task {
                upd_task = Some(t.clone());
                if matches!(
                    outcome,
                    super::review::ReviewCircuitOutcome::HandoffToCoordinator
                        | super::review::ReviewCircuitOutcome::AwaitingUserDecision
                ) {
                    review_escalated = true;
                    super::review::emit_review_circuit_escalation(
                        core,
                        &run,
                        &t,
                        outcome,
                        Some(&refreshed_att.id),
                        Some(&worker.id),
                    )
                    .await;
                }
            }
        } else if super::review::is_review_approval_data(result.data.as_ref()) {
            super::review::record_review_passed(&core.drove.store, &refreshed_task.id);
            upd_task = core.drove.store.get_task(&refreshed_task.id);
        }
    }
    super::ai::spawn_handover_compact(
        core.clone(),
        run.id.clone(),
        refreshed_task.id.clone(),
        refreshed_att.id.clone(),
        summary.clone(),
    );
    if !keep_wt && worker.worktree.is_some() {
        let _ = core.drove.store.update_worker(&worker.id, |w| {
            w.worktree = None;
            w.cwd = run.cwd.clone();
        });
    }
    mark_worker_idle(core, &worker.id);
    core.drove.store.append_daemon_event(NewDaemonEvent {
        run_id: Some(run.id.clone()),
        task_id: Some(refreshed_task.id.clone()),
        attempt_id: Some(refreshed_att.id.clone()),
        worker_id: Some(worker.id.clone()),
        message_id: None,
        category: "task".into(),
        kind: "done".into(),
        status: "ok".into(),
        title: format!("Task `{}` succeeded", refreshed_task.key),
        detail: Some(summary),
        duration_ms: None,
        tokens: None,
        cost: None,
    });
    core.drove.wake_scheduler();

    // If all non-template tasks in this run are now terminal (and we didn't just emit a review circuit escalation),
    // notify coordinator (keeping idle workers warm for reuse).
    let all_tasks = core.drove.store.list_tasks(&run.id);
    if !review_escalated
        && !all_tasks.is_empty()
        && all_tasks.iter().filter(|t| t.template != Some(true)).all(|t| super::scheduler::is_terminal_status(t.status))
    {
        notify_coordinator(
            core,
            &run,
            "run_ready",
            "All tasks finished",
            &format!("[drove] All tasks in run `{}` finished; inspect or run `h2 drove complete {}`", run.id, run.id),
            None,
            None,
            None,
            None,
        )
        .await;
    }

    Ok(json!({ "ok": true, "attempt": upd_att, "task": upd_task }))
}

pub(super) async fn add_tasks_http(
    core: &Arc<Core>,
    run_id: &str,
    coord_header: Option<&str>,
    body: &Value,
) -> Result<Value, ApiErr> {
    let run = core.drove.store.get_run(run_id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    if run.trashed_at.is_some() {
        return Err(ApiErr::new(409, "invalid_state", "Run is in trash"));
    }

    let raw_list: Vec<Value> = if let Some(arr) = body["tasks"].as_array() {
        arr.clone()
    } else if body.is_array() {
        body.as_array().cloned().unwrap_or_default()
    } else if body.is_object() {
        vec![body.clone()]
    } else {
        return Err(ApiErr::new(400, "bad_request", "expected task object or { tasks: [...] }"));
    };
    if raw_list.is_empty() {
        return Err(ApiErr::new(400, "bad_request", "at least one task is required"));
    }

    let mut worktree_warning: Option<String> = None;
    let skip_preflight = body["skipWorktreePreflight"].as_bool() == Some(true);
    if !skip_preflight {
        let cfg = core.drove.config_snapshot();
        let existing_tasks = core.drove.store.list_tasks(&run.id);
        if tasks_need_worktree_preflight(&cfg, &existing_tasks, &raw_list) {
            let cwd_check = run.cwd.clone();
            let check = tokio::task::spawn_blocking(move || super::integrate::check_drove_worktree_ready(&cwd_check))
                .await
                .map_err(|e| ApiErr::new(500, "internal", format!("preflight join error: {e}")))?;
            match check {
                Ok(ok) => worktree_warning = ok.dirty_warning,
                Err(blocker) => {
                    return Err(ApiErr::new(
                        422,
                        "worktree_not_ready",
                        format!("{} | 建议处理：{}", blocker.message, blocker.fix_hint),
                    ));
                }
            }
        }
    }

    let existing_keys: BTreeSet<String> = core.drove.store.list_tasks(&run.id).into_iter().map(|t| t.key).collect();
    let mut created = Vec::new();
    let mut seen_keys = existing_keys;

    for raw in raw_list {
        let base_key = raw["key"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ApiErr::new(400, "bad_request", "each task must have a non-empty key"))?
            .to_string();

        if let Some(foreach_arr) = raw.get("foreach").and_then(Value::as_array) {
            for (idx, item) in foreach_arr.iter().enumerate() {
                let child_key = format!("{base_key}#{}", idx + 1);
                if seen_keys.contains(&child_key) {
                    return Err(ApiErr::new(409, "duplicate_key", format!("duplicate task key {child_key:?}")));
                }
                seen_keys.insert(child_key.clone());
                let mut mutated = raw.clone();
                let item_str = match item {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                if let Some(obj) = mutated.as_object_mut() {
                    obj.insert("key".into(), Value::String(child_key));
                    let t_base = obj.get("title").and_then(Value::as_str).unwrap_or(&base_key);
                    obj.insert("title".into(), Value::String(format!("{t_base} [{}]", idx + 1)));
                    obj.insert("context".into(), item.clone());
                    replace_item_placeholder(obj.get_mut("spec"), &item_str);
                }
                created.push(create_one_task(core, &run.id, &mutated)?);
            }
        } else {
            if seen_keys.contains(&base_key) {
                return Err(ApiErr::new(409, "duplicate_key", format!("duplicate task key {base_key:?}")));
            }
            seen_keys.insert(base_key);
            created.push(create_one_task(core, &run.id, &raw)?);
        }
    }

    normalize_run_task_deps(core, &run.id);

    let rebind_sess = if super::pool::is_coordinator_closed(core, &run) {
        body["hostSession"].as_str().or(coord_header).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
    } else {
        None
    };
    let rebound_meta = if let Some(ref next_sess) = rebind_sess {
        let (agent, model, pid, _, _) =
            super::http::resolve_session_meta_and_entries(core, next_sess, true, false).await;
        let bound = super::http::is_local_session_bound(core, next_sess);
        Some((next_sess.clone(), agent, model, pid, bound))
    } else {
        None
    };
    let reactivate = run.status == RunStatus::Done
        || (run.status == RunStatus::Paused
            && matches!(run.pause_reason.as_deref(), Some("cancelled" | "coordinator_closed")));
    if rebound_meta.is_some() || reactivate {
        let _ = core.drove.store.update_run(&run.id, |r| {
            if let Some((ref next_sess, ref agent, ref model, pid, bound)) = rebound_meta {
                r.coordinator.generation += 1;
                r.coordinator.host_session = next_sess.clone();
                r.coordinator.agent = agent.clone();
                r.coordinator.model = model.clone();
                r.coordinator.host_pid = pid;
                r.coordinator.session_bound = bound;
            }
            if reactivate {
                r.status = RunStatus::Active;
                r.pause_reason = None;
            }
        });
    }

    let created: Vec<Task> = created.into_iter().filter_map(|t| core.drove.store.get_task(&t.id)).collect();
    for t in &created {
        super::ai::spawn_task_enrich(core.clone(), run.id.clone(), t.id.clone());
    }

    core.drove.wake_scheduler();
    let mut out = json!({ "tasks": created });
    if let Some(warn) = worktree_warning {
        out["worktreeWarning"] = Value::String(warn);
    }
    Ok(out)
}

/// Returns true when submitted tasks explicitly request `isolation: worktree`, or when
/// `conflictPolicy == "auto_worktree"` and >= 2 writable tasks may execute concurrently.
pub(super) fn tasks_need_worktree_preflight(
    cfg: &DroveConfig,
    existing_tasks: &[Task],
    raw_incoming: &[Value],
) -> bool {
    let is_writable_role = |role: Option<&str>| -> bool {
        let (_, _, access, _) = super::config::role_spec_for(cfg, role);
        access != RoleAccessMode::Readonly
    };

    let mut incoming_writers = 0usize;
    for raw in raw_incoming {
        let iso = raw["isolation"].as_str().map(|s| s.trim().to_ascii_lowercase());
        if iso.as_deref() == Some("worktree") {
            return true;
        }
        if raw["kind"].as_str() == Some("exec") {
            continue;
        }
        let role = raw["role"].as_str().or_else(|| raw["assign"]["role"].as_str());
        if is_writable_role(role) {
            let count = raw.get("foreach").and_then(Value::as_array).map_or(1, |a| a.len().max(1));
            incoming_writers += count;
        }
    }

    if cfg.ai.conflict_policy != "auto_worktree" {
        return false;
    }

    let existing_live_writers = existing_tasks
        .iter()
        .filter(|t| {
            !super::scheduler::is_terminal_status(t.status)
                && t.kind.as_deref() != Some("exec")
                && t.template != Some(true)
                && is_writable_role(t.assign.as_ref().and_then(|a| a.role.as_deref()))
        })
        .count();

    if existing_live_writers + incoming_writers < 2 {
        return false;
    }

    // If incoming tasks form a strict linear chain (every subsequent writable task declares deps)
    // and there are no existing live writers, they won't overlap in parallel.
    if existing_live_writers == 0 {
        let root_writers = raw_incoming
            .iter()
            .filter(|raw| {
                raw["kind"].as_str() != Some("exec")
                    && is_writable_role(raw["role"].as_str().or_else(|| raw["assign"]["role"].as_str()))
                    && raw["deps"].as_array().is_none_or(|d| d.is_empty())
            })
            .map(|raw| raw.get("foreach").and_then(Value::as_array).map_or(1, |a| a.len().max(1)))
            .sum::<usize>();
        if root_writers < 2 {
            // Check if any two incoming writers share the same dep (fan-out).
            let mut by_dep: BTreeMap<String, usize> = BTreeMap::new();
            for raw in raw_incoming {
                if raw["kind"].as_str() == Some("exec")
                    || !is_writable_role(raw["role"].as_str().or_else(|| raw["assign"]["role"].as_str()))
                {
                    continue;
                }
                if let Some(deps) = raw["deps"].as_array() {
                    for d in deps.iter().filter_map(Value::as_str) {
                        *by_dep.entry(d.to_string()).or_default() += 1;
                    }
                }
            }
            if !by_dep.values().any(|&c| c >= 2) {
                return false;
            }
        }
    }
    true
}

fn normalize_run_task_deps(core: &Arc<Core>, run_id: &str) {
    let all_tasks = core.drove.store.list_tasks(run_id);
    for task in &all_tasks {
        let mut resolved = Vec::new();
        for dep in &task.deps {
            if let Some(target) = all_tasks.iter().find(|c| &c.id == dep || &c.key == dep) {
                if !resolved.contains(&target.id) {
                    resolved.push(target.id.clone());
                }
            } else {
                let mut matched_foreach = false;
                for child in &all_tasks {
                    if child.key.strip_prefix(dep.as_str()).is_some_and(|rest| rest.starts_with('#')) {
                        matched_foreach = true;
                        if !resolved.contains(&child.id) {
                            resolved.push(child.id.clone());
                        }
                    }
                }
                if !matched_foreach && !resolved.contains(dep) {
                    resolved.push(dep.clone());
                }
            }
        }
        if resolved != task.deps {
            let _ = core.drove.store.update_task(&task.id, |t| {
                t.deps = resolved.clone();
                if t.status == TaskStatus::Ready && !t.deps.is_empty() && t.template != Some(true) {
                    t.status = TaskStatus::Pending;
                }
            });
        }
    }
}

fn replace_item_placeholder(spec_opt: Option<&mut Value>, val: &str) {
    let Some(Value::Object(spec)) = spec_opt else { return };
    for k in ["target", "change", "acceptance", "constraints", "ownership"] {
        if let Some(Value::String(s)) = spec.get_mut(k) {
            *s = s.replace("{{item}}", val);
        }
    }
}

fn create_one_task(core: &Arc<Core>, run_id: &str, raw: &Value) -> Result<Task, ApiErr> {
    let key = raw["key"].as_str().unwrap().trim().to_string();
    let title = raw["title"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(&key).to_string();
    let spec: Spec = if raw.get("spec").is_some_and(Value::is_object) {
        serde_json::from_value(raw["spec"].clone())
            .map_err(|e| ApiErr::new(400, "invalid_field", format!("invalid spec on {key}: {e}")))?
    } else {
        Spec {
            target: raw["target"].as_str().unwrap_or("workspace").to_string(),
            change: raw["change"].as_str().or_else(|| raw["prompt"].as_str()).unwrap_or(&title).to_string(),
            constraints: raw["constraints"].as_str().map(String::from),
            ownership: raw["ownership"].as_str().map(String::from),
            acceptance: raw["acceptance"].as_str().unwrap_or("Complete cleanly").to_string(),
            verify: raw.get("verify").and_then(|v| serde_json::from_value(v.clone()).ok()),
        }
    };
    let deps: Vec<String> = raw["deps"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let tags: Vec<String> = raw["tags"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let files: Vec<String> = raw["files"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let foreach_from: Option<ForeachFrom> = raw.get("foreachFrom").and_then(|v| serde_json::from_value(v.clone()).ok());
    let is_template = foreach_from.is_some();

    let mut assign: Option<Assign> = raw.get("assign").and_then(|v| serde_json::from_value(v.clone()).ok());
    if let Some(role) = raw["role"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        let mut a = assign.unwrap_or_default();
        if a.role.is_none() {
            a.role = Some(role.to_string());
        }
        assign = Some(a);
    }

    let initial_status = if is_template || !deps.is_empty() { TaskStatus::Pending } else { TaskStatus::Ready };

    let task = core.drove.store.create_task(super::store::CreateTaskInput {
        run_id: run_id.to_string(),
        key,
        title,
        phase: raw["phase"].as_str().map(String::from),
        spec,
        tags,
        files,
        deps,
        output_schema: raw.get("outputSchema").cloned(),
        isolation: raw.get("isolation").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or(Isolation::None),
        timeout_sec: raw["timeoutSec"].as_u64(),
        stall_warn_sec: raw["stallWarnSec"].as_u64(),
        stall_kill_sec: raw["stallKillSec"].as_u64(),
        when: raw.get("when").and_then(|v| serde_json::from_value(v.clone()).ok()),
        assign,
        status: initial_status,
        result: None,
        kind: raw["kind"].as_str().map(String::from),
        trigger_rule: raw.get("triggerRule").and_then(|v| serde_json::from_value(v.clone()).ok()),
        context: raw.get("context").cloned(),
        exec: raw.get("exec").and_then(|v| serde_json::from_value(v.clone()).ok()),
        foreach_from,
        template: is_template.then_some(true),
        expanded_from: None,
        review: raw.get("review").and_then(|v| serde_json::from_value(v.clone()).ok()),
        cache: raw.get("cache").cloned(),
        cached_from: None,
        retry_on_worker_reported: raw["retryOnWorkerReported"].as_bool(),
    });
    Ok(task)
}

fn extract_review_verdicts(tasks: &[Task]) -> Vec<Value> {
    let mut out = Vec::new();
    for t in tasks {
        if t.template == Some(true) {
            continue;
        }
        let Some(data) = t.result.as_ref().and_then(|r| r.data.as_ref()) else {
            continue;
        };
        let raw_verdict = data
            .get("review_status")
            .or_else(|| data.get("reviewStatus"))
            .or_else(|| data.get("verdict"))
            .and_then(Value::as_str)
            .map(|s| s.trim().to_ascii_lowercase());
        if let Some(v) = raw_verdict
            && matches!(v.as_str(), "changes_requested" | "request_changes" | "rejected")
        {
            out.push(json!({
                "taskId": t.id,
                "key": t.key,
                "verdict": v,
                "summary": t.result.as_ref().map(|r| r.summary.as_str()).unwrap_or(""),
            }));
        }
    }
    out
}

fn extract_review_circuits(tasks: &[Task]) -> Vec<Value> {
    let mut out = Vec::new();
    for t in tasks {
        if t.template == Some(true) {
            continue;
        }
        let Some(rc) = &t.review_circuit else {
            continue;
        };
        if matches!(
            rc.stage,
            ReviewCircuitStage::HandoffToCoordinator
                | ReviewCircuitStage::PostHandoffReview
                | ReviewCircuitStage::AwaitingUserDecision
        ) {
            out.push(json!({
                "taskId": t.id,
                "taskKey": t.key,
                "stage": rc.stage,
                "reviewRounds": rc.review_rounds,
                "maxNormalRounds": rc.max_normal_rounds,
                "handoffToCoordinator": rc.handoff_to_coordinator,
                "postHandoffReviewUsed": rc.post_handoff_review_used,
                "awaitingUserDecision": rc.awaiting_user_decision,
                "userOverrides": rc.user_overrides,
            }));
        }
    }
    out
}

pub(crate) fn compact_task_summary(t: &Task) -> Value {
    let mut obj = json!({
        "id": t.id,
        "key": t.key,
        "title": t.title,
        "status": t.status,
        "deps": t.deps,
        "isolation": t.isolation,
        "failures": t.failures,
        "updatedAt": t.updated_at,
    });
    if let Some(p) = &t.phase {
        obj["phase"] = Value::String(p.clone());
    }
    if let Some(ei) = t.effective_isolation {
        obj["effectiveIsolation"] = json!(ei);
    }
    if let Some(ir) = &t.isolation_reason {
        obj["isolationReason"] = Value::String(ir.clone());
    }
    if let Some(a) = &t.assign {
        obj["assign"] = json!(a);
    }
    if let Some(ep) = t.exec_phase {
        obj["execPhase"] = json!(ep);
    }
    if let Some(rc) = &t.review_circuit {
        obj["reviewCircuit"] = json!(rc);
    }
    if let Some(res) = &t.result {
        obj["result"] = json!(res);
    }
    obj
}

pub(super) async fn wait_run_http(
    core: &Arc<Core>,
    run_id: &str,
    query: &BTreeMap<String, String>,
) -> Result<Value, ApiErr> {
    let run = core.drove.store.get_run(run_id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    let timeout_sec = query.get("timeoutSec").and_then(|s| s.parse::<u64>().ok()).unwrap_or(20).clamp(1, 25);
    let since_seq = query.get("sinceSeq").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let compact = query.get("compact").is_some_and(|v| v == "1" || v == "true");
    let on_task_done = query.get("onTaskDone").is_some_and(|v| v == "1" || v == "true");
    let baseline_done: BTreeSet<String> = if let Some(raw) = query.get("knownDone").filter(|s| !s.is_empty()) {
        raw.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
    } else if on_task_done {
        core.drove
            .store
            .list_tasks(run_id)
            .into_iter()
            .filter(|t| t.template != Some(true) && super::scheduler::is_terminal_status(t.status))
            .map(|t| t.id)
            .collect()
    } else {
        BTreeSet::new()
    };
    let _ = run;

    let deadline = Instant::now() + Duration::from_secs(timeout_sec);
    loop {
        let cur_run = core.drove.store.get_run(run_id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
        let tasks = core.drove.store.list_tasks(run_id);
        let pending: Vec<Message> = core
            .drove
            .store
            .list_messages(run_id, Some(true), true, None)
            .into_iter()
            .filter(|m| m.seq > since_seq)
            .collect();
        let all_failed_msg_tasks: std::collections::HashSet<String> = core
            .drove
            .store
            .list_messages(run_id, None, false, None)
            .into_iter()
            .filter(|m| matches!(m.kind, MessageType::Failed | MessageType::Escalation))
            .filter_map(|m| m.task_id)
            .collect();
        let has_unreported_failure =
            tasks.iter().any(|t| t.status == TaskStatus::Failed && !all_failed_msg_tasks.contains(&t.id));
        let all_terminal = !tasks.is_empty()
            && tasks
                .iter()
                .filter(|t| t.template != Some(true))
                .all(|t| super::scheduler::is_terminal_status(t.status));
        let newly_completed: Vec<String> = if on_task_done {
            tasks
                .iter()
                .filter(|t| {
                    t.template != Some(true)
                        && super::scheduler::is_terminal_status(t.status)
                        && !baseline_done.contains(&t.id)
                })
                .map(|t| t.key.clone())
                .collect()
        } else {
            Vec::new()
        };

        let should_return = !pending.is_empty()
            || cur_run.status != RunStatus::Active
            || all_terminal
            || has_unreported_failure
            || !newly_completed.is_empty();
        let timed_out = !should_return && Instant::now() >= deadline;

        if should_return || timed_out {
            let counts = core.drove.store.count_tasks_by_run().remove(run_id).unwrap_or_else(empty_task_counts);
            let review_verdicts = extract_review_verdicts(&tasks);
            let review_circuits = extract_review_circuits(&tasks);
            let any_failed = tasks.iter().any(|t| t.template != Some(true) && t.status == TaskStatus::Failed);
            let any_circuit_attention = tasks.iter().any(|t| {
                t.template != Some(true)
                    && t.review_circuit.as_ref().is_some_and(|rc| {
                        matches!(
                            rc.stage,
                            ReviewCircuitStage::HandoffToCoordinator | ReviewCircuitStage::AwaitingUserDecision
                        )
                    })
            });
            let needs_attention =
                !pending.is_empty() || any_failed || !review_verdicts.is_empty() || any_circuit_attention;
            let tasks_json: Value = if compact {
                Value::Array(tasks.iter().map(compact_task_summary).collect())
            } else {
                json!(tasks)
            };
            let settled = if timed_out { false } else { all_terminal || cur_run.status != RunStatus::Active };
            let mut out = json!({
                "settled": settled,
                "timedOut": timed_out,
                "needsAttention": needs_attention,
                "taskCounts": counts,
                "messages": if timed_out { Value::Array(Vec::new()) } else { json!(pending) },
                "run": cur_run,
                "tasks": tasks_json,
            });
            if !review_verdicts.is_empty() {
                out["reviewVerdicts"] = Value::Array(review_verdicts);
            }
            if !review_circuits.is_empty() {
                out["reviewCircuits"] = Value::Array(review_circuits);
            }
            if !newly_completed.is_empty() {
                out["newlyCompleted"] = json!(newly_completed);
            }
            return Ok(out);
        }
        let notified = core.drove.store.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let _ = tokio::time::timeout(Duration::from_millis(500), notified).await;
    }
}

pub(super) async fn complete_run_http(core: &Arc<Core>, run_id: &str, body: &Value) -> Result<Value, ApiErr> {
    let run = core.drove.store.get_run(run_id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    let force = body["force"].as_bool() == Some(true);
    let tasks = core.drove.store.list_tasks(run_id);
    let active_left = tasks.iter().any(|t| {
        t.template != Some(true)
            && matches!(t.status, TaskStatus::Pending | TaskStatus::Ready | TaskStatus::Running | TaskStatus::Blocked)
    });
    if active_left && !force {
        return Err(ApiErr::new(
            409,
            "tasks_incomplete",
            "run still has non-terminal tasks (pass force=true to complete anyway)",
        ));
    }
    let failed_left = tasks.iter().any(|t| t.template != Some(true) && t.status == TaskStatus::Failed);
    if failed_left && !force {
        return Err(ApiErr::new(
            409,
            "tasks_failed",
            "run has failed tasks (retry/cancel them or pass force=true / --force to complete anyway)",
        ));
    }
    let awaiting_user_left = tasks.iter().any(|t| {
        t.template != Some(true)
            && !matches!(t.status, TaskStatus::Cancelled | TaskStatus::Skipped)
            && t.review_circuit.as_ref().is_some_and(|rc| rc.awaiting_user_decision)
    });
    if awaiting_user_left && !force {
        return Err(ApiErr::new(
            409,
            "review_circuit_open",
            "run has task(s) in `awaiting_user_decision` after review circuit breaker tripped; report status, divergence, and your recommendation to the user first, then pass `--force` or cancel the task after user approval",
        ));
    }

    let verify_log_id = if !force {
        let (log_id, repair_task) = super::verify::run_level_verify_gate(core, &run).await?;
        if let Some(rt) = repair_task {
            return Ok(
                json!({ "ok": false, "repairing": true, "repairTask": rt, "run": core.drove.store.get_run(&run.id) }),
            );
        }
        log_id
    } else {
        None
    };

    let updated = core.drove.store.update_run(&run.id, |r| {
        r.status = RunStatus::Done;
        r.verified_at = verify_log_id.as_ref().map(|_| now_iso());
        r.finalization = Some(RunFinalization {
            state: "done".into(),
            generation: r.finalization.as_ref().map(|f| f.generation + 1).unwrap_or(1),
            tree_sha: None,
            verified_tree_sha: None,
            command_id: None,
            repairs_used: r.finalization.as_ref().map(|f| f.repairs_used).unwrap_or(0),
            last_failure: None,
            log_artifact_id: verify_log_id,
        });
    });
    let _ = core.drove.store.post_message(PostMessageInput {
        run_id: run.id.clone(),
        kind: MessageType::RunDone,
        from: "drove".into(),
        to: "coordinator".into(),
        thread_id: None,
        task_id: None,
        attempt_id: None,
        subject: format!("Run `{}` completed", run.id),
        body: body["summary"].as_str().map(String::from),
        options: None,
        recommend: None,
        why: None,
        raised_by: Some("drove".into()),
        raised_by_worker: None,
        tag: None,
        ai_draft: None,
        needs_action: false,
    });
    Ok(json!({ "ok": true, "run": updated }))
}

pub(super) async fn cancel_run_http(core: &Arc<Core>, run_id: &str) -> Result<Value, ApiErr> {
    let run = core.drove.store.get_run(run_id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    let tasks = core.drove.store.list_tasks(&run.id);
    for t in tasks {
        if !super::scheduler::is_terminal_status(t.status) {
            let _ = core.drove.store.update_task(&t.id, |x| x.status = TaskStatus::Cancelled);
        }
    }
    stop_run_workers(core, &run.id).await;
    let updated = core.drove.store.update_run(&run.id, |r| {
        r.status = RunStatus::Done;
        r.pause_reason = Some("cancelled".into());
    });
    Ok(json!({ "ok": true, "run": updated }))
}

pub(super) async fn retry_task_http(core: &Arc<Core>, id: &str, body: &Value) -> Result<Value, ApiErr> {
    let mut task = core.drove.store.get_task(id).ok_or_else(|| ApiErr::new(404, "not_found", "Task not found"))?;
    if task.status == TaskStatus::Running {
        return Err(ApiErr::new(409, "invalid_state", "cannot retry a running task"));
    }
    let user_override = body["userOverride"].as_bool() == Some(true);
    let override_reason = body["overrideReason"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let run_opt = core.drove.store.get_run(&task.run_id);

    // 1. Backfill uncounted completed standalone reviewer attempts: if a `role=reviewer` task
    //    exited `--outcome done` without structured rejection JSON, yet the Coordinator calls
    //    `h2 drove retry`, that completed attempt counts as a non-passing review round so it
    //    cannot bypass the 2-round handoff + 1 post-handoff hard circuit breaker.
    if super::review::is_standalone_reviewer_task(&task) && task.status == TaskStatus::Succeeded {
        let attempts = core.drove.store.list_attempts(None, Some(&task.id), None);
        if let Some(att) = attempts.iter().rev().find(|a| a.outcome == Some(Outcome::Succeeded)) {
            let already_recorded = task.review_circuit.as_ref().is_some_and(|rc| {
                rc.stage == ReviewCircuitStage::Cleared
                    || rc.history.iter().any(|h| h.attempt_id.as_deref() == Some(att.id.as_str()))
            });
            if !already_recorded {
                let summary = att
                    .result
                    .as_ref()
                    .map(|r| r.summary.as_str())
                    .or_else(|| task.result.as_ref().map(|r| r.summary.as_str()))
                    .unwrap_or("Review round retried by coordinator");
                let findings = super::review::extract_key_findings(
                    att.result.as_ref().and_then(|r| r.data.as_ref()),
                    None,
                    summary,
                );
                let cap = task
                    .review
                    .as_ref()
                    .and_then(|r| r.max_rounds)
                    .unwrap_or(super::review::DEFAULT_MAX_NORMAL_REVIEW_ROUNDS);
                let (updated_opt, outcome) = super::review::record_review_non_pass(
                    &core.drove.store,
                    &task.id,
                    cap,
                    "retried_unpassed",
                    summary,
                    findings,
                    Some(&att.id),
                );
                if let Some(updated_task) = updated_opt {
                    task = updated_task;
                    if let Some(ref run) = run_opt
                        && !matches!(outcome, super::review::ReviewCircuitOutcome::ContinueNormal)
                    {
                        super::review::emit_review_circuit_escalation(
                            core,
                            run,
                            &task,
                            outcome,
                            Some(&att.id),
                            Some(&att.worker_id),
                        )
                        .await;
                    }
                    if !user_override {
                        match outcome {
                            super::review::ReviewCircuitOutcome::HandoffToCoordinator => {
                                return Err(ApiErr::new(
                                    409,
                                    "review_handoff_required",
                                    format!(
                                        "Task `{}` reached {} non-passing review rounds and has been handed off to Coordinator. Take over and fix directly first, then run `h2 drove retry {}` ONCE for the single post-handoff final review.",
                                        task.key,
                                        task.review_circuit.as_ref().map_or(2, |c| c.review_rounds),
                                        task.key
                                    ),
                                ));
                            }
                            super::review::ReviewCircuitOutcome::AwaitingUserDecision => {
                                return Err(ApiErr::new(
                                    409,
                                    "review_circuit_open",
                                    format!(
                                        "Task `{}` tripped the review hard circuit breaker after {} non-passing review rounds (post-handoff review did not pass). STOP retrying and immediately report current status, divergence, impact, and your recommendation to the user. Only pass `--user-override` after explicit user approval.",
                                        task.key,
                                        task.review_circuit.as_ref().map_or(3, |c| c.review_rounds)
                                    ),
                                ));
                            }
                            super::review::ReviewCircuitOutcome::ContinueNormal => {}
                        }
                    }
                }
            }
        }
    }

    // 2. Enforce three-stage Review Circuit Breaker on `retry`:
    //    - `HandoffToCoordinator` -> allows ONCE, transitioning to `PostHandoffReview` (1/1).
    //    - `AwaitingUserDecision` -> blocked with `409 review_circuit_open` unless explicit `userOverride: true`.
    let mut circuit_transition: Option<&'static str> = None;
    if let Some(rc) = &task.review_circuit {
        match rc.stage {
            ReviewCircuitStage::AwaitingUserDecision => {
                if !user_override {
                    return Err(ApiErr::new(
                        409,
                        "review_circuit_open",
                        format!(
                            "Task `{}` is in `awaiting_user_decision` after {} non-passing review rounds (including the single post-handoff final review). STOP retrying and immediately report current status, unresolved divergence, impact, and your concrete recommendation to the user. Only after explicit user approval may you run `h2 drove retry {} --user-override`.",
                            task.key, rc.review_rounds, task.key
                        ),
                    ));
                }
                circuit_transition = Some("user_override");
            }
            ReviewCircuitStage::HandoffToCoordinator => {
                if rc.post_handoff_review_used && !user_override {
                    return Err(ApiErr::new(
                        409,
                        "review_circuit_open",
                        format!(
                            "Task `{}` already consumed its single post-handoff final review. Report to the user with your recommendation, or pass `--user-override` after explicit user approval.",
                            task.key
                        ),
                    ));
                }
                circuit_transition = Some(if user_override && rc.post_handoff_review_used {
                    "user_override"
                } else {
                    "post_handoff_review"
                });
            }
            ReviewCircuitStage::PostHandoffReview => {
                let latest_att = core.drove.store.list_attempts(None, Some(&task.id), None).into_iter().last();
                let is_infra_crash = task.status == TaskStatus::Failed
                    && latest_att.as_ref().and_then(|a| a.failure.as_ref()).is_some_and(|f| {
                        !matches!(f.kind, FailureKind::ReviewRejected | FailureKind::WorkerReported)
                    });
                if !is_infra_crash && !user_override {
                    return Err(ApiErr::new(
                        409,
                        "review_circuit_open",
                        format!(
                            "Task `{}` already used its single post-handoff final review slot. Report current status and your recommendation to the user, or pass `--user-override` after user approval.",
                            task.key
                        ),
                    ));
                }
                if user_override {
                    circuit_transition = Some("user_override");
                }
            }
            ReviewCircuitStage::Normal | ReviewCircuitStage::Cleared => {}
        }
    }

    if let Some(hint) = body["hint"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        core.drove.set_task_hint(&task.id, hint.to_string());
    }
    let updated = core.drove.store.update_task(&task.id, |t| {
        t.status = TaskStatus::Ready;
        t.failures = 0;
        t.exec_phase = None;
        t.review_state = None;
        if let Some(rc) = t.review_circuit.as_mut() {
            match circuit_transition {
                Some("post_handoff_review") => {
                    rc.stage = ReviewCircuitStage::PostHandoffReview;
                    rc.post_handoff_review_used = true;
                    rc.awaiting_user_decision = false;
                }
                Some("user_override") => {
                    rc.user_overrides = rc.user_overrides.saturating_add(1);
                    rc.stage = ReviewCircuitStage::PostHandoffReview;
                    rc.post_handoff_review_used = true;
                    rc.awaiting_user_decision = false;
                }
                _ => {}
            }
        }
        if body["noReview"].as_bool() == Some(true) {
            t.review = None;
        } else {
            let rev_agent = body["reviewAgent"].as_str().map(str::trim).filter(|s| !s.is_empty());
            let rev_model = body["reviewModel"].as_str().map(str::trim).filter(|s| !s.is_empty());
            if rev_agent.is_some() || rev_model.is_some() {
                let rev = t.review.get_or_insert_with(|| ReviewSpec {
                    agent: None,
                    model: None,
                    effort: None,
                    fast: None,
                    different_agent: None,
                    criteria: None,
                    max_rounds: Some(2),
                });
                if let Some(ra) = rev_agent {
                    rev.agent = Some(ra.to_string());
                }
                if let Some(rm) = rev_model {
                    rev.model = Some(rm.to_string());
                }
            }
        }
        let mut a = t.assign.clone().unwrap_or_default();
        let req_agent = body["agent"].as_str().map(str::trim).filter(|s| !s.is_empty());
        let req_model = body["model"].as_str().map(str::trim).filter(|s| !s.is_empty());
        let req_effort: Option<Effort> =
            body.get("effort").filter(|v| !v.is_null()).and_then(|v| serde_json::from_value(v.clone()).ok());
        let req_role = body["role"].as_str().map(str::trim).filter(|s| !s.is_empty());
        let has_explicit_target = req_agent.is_some() || req_model.is_some() || req_effort.is_some();
        if let Some(role) = req_role {
            a.role = Some(role.to_string());
        }
        if !has_explicit_target && (req_role.is_some() || a.source.as_deref() == Some("router")) {
            a.source = None;
            a.agent = None;
            a.model = None;
            a.effort = None;
            a.fast = None;
        }
        if let Some(agent) = req_agent {
            a.agent = Some(agent.to_string());
        }
        if let Some(model) = req_model {
            a.model = Some(model.to_string());
        }
        if let Some(eff) = req_effort {
            a.effort = Some(eff);
        }
        if has_explicit_target {
            a.source = Some("user".into());
        }
        if body["fresh"].as_bool() == Some(true) || has_explicit_target || req_role.is_some() {
            a.worker = None;
        }
        t.assign = Some(a);
    });
    if let Some(kind) = circuit_transition {
        let (event_kind, status, title, detail) = if kind == "user_override" {
            let n = updated.as_ref().and_then(|t| t.review_circuit.as_ref()).map_or(1, |rc| rc.user_overrides);
            (
                "review_override",
                "warn",
                format!("User-approved review override #{n} on `{}`", task.key),
                override_reason.or_else(|| Some("Explicit --user-override after hard circuit breaker".into())),
            )
        } else {
            (
                "post_handoff_review",
                "ok",
                format!("Dispatched single post-handoff final review (1/1) on `{}`", task.key),
                Some("Coordinator takeover completed; running final verification pass".into()),
            )
        };
        core.drove.store.append_daemon_event(NewDaemonEvent {
            run_id: Some(task.run_id.clone()),
            task_id: Some(task.id.clone()),
            attempt_id: None,
            worker_id: None,
            message_id: None,
            category: "review".into(),
            kind: event_kind.into(),
            status: status.into(),
            title,
            detail,
            duration_ms: None,
            tokens: None,
            cost: None,
        });
    }
    reopen_skipped_dependents(core, &task);
    for m in core.drove.store.list_messages(&task.run_id, Some(true), true, None) {
        if m.task_id.as_deref() == Some(task.id.as_str())
            && matches!(m.kind, MessageType::Failed | MessageType::Escalation)
        {
            let _ = core.drove.store.mark_handled(&m.id);
        }
    }
    core.drove.wake_scheduler();
    Ok(json!(updated))
}

fn reopen_skipped_dependents(core: &Arc<Core>, root: &Task) {
    let tasks = core.drove.store.list_tasks(&root.run_id);
    let mut reopened: std::collections::HashSet<String> = [root.id.clone(), root.key.clone()].into_iter().collect();
    if let Some(tpl_id) = &root.expanded_from
        && let Some(tpl) = tasks.iter().find(|t| &t.id == tpl_id)
    {
        reopened.insert(tpl.id.clone());
        reopened.insert(tpl.key.clone());
    }
    loop {
        let mut changed = false;
        for t in &tasks {
            if t.template == Some(true) || t.status != TaskStatus::Skipped {
                continue;
            }
            let depends_on_reopened = t.deps.iter().any(|d| reopened.contains(d))
                || t.deps.iter().any(|d| root.key.strip_prefix(d.as_str()).is_some_and(|rest| rest.starts_with('#')));
            if depends_on_reopened && !reopened.contains(&t.id) {
                let _ = core.drove.store.update_task(&t.id, |upd| {
                    if upd.status == TaskStatus::Skipped {
                        upd.status = TaskStatus::Pending;
                        upd.result = None;
                    }
                });
                reopened.insert(t.id.clone());
                reopened.insert(t.key.clone());
                if let Some(tpl_id) = &t.expanded_from
                    && let Some(tpl) = tasks.iter().find(|cand| &cand.id == tpl_id)
                {
                    reopened.insert(tpl.id.clone());
                    reopened.insert(tpl.key.clone());
                }
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

pub(super) async fn cancel_task_http(core: &Arc<Core>, id: &str) -> Result<Value, ApiErr> {
    let task = core.drove.store.get_task(id).ok_or_else(|| ApiErr::new(404, "not_found", "Task not found"))?;
    let active_atts: Vec<Attempt> = core
        .drove
        .store
        .list_attempts(None, Some(&task.id), None)
        .into_iter()
        .filter(|a| a.outcome.is_none())
        .collect();
    for att in active_atts {
        if att.worker_id != "exec" {
            let _ = stop_worker(core, &att.worker_id).await;
        }
        let _ = core.drove.store.update_attempt(&att.id, |a| {
            a.outcome = Some(Outcome::Failed);
            a.ended_at = Some(now_iso());
            a.failure =
                Some(AttemptFailure { kind: FailureKind::Cancelled, message: "Cancelled by coordinator".into() });
        });
    }
    let updated = core.drove.store.update_task(&task.id, |t| t.status = TaskStatus::Cancelled);
    core.drove.wake_scheduler();
    Ok(json!(updated))
}

pub(super) async fn send_task_http(core: &Arc<Core>, id: &str, body: &Value) -> Result<Value, ApiErr> {
    let task = core.drove.store.get_task(id).ok_or_else(|| ApiErr::new(404, "not_found", "Task not found"))?;
    let text = body["text"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::new(400, "bad_request", "text must be non-empty"))?;
    let interrupt = body["interrupt"].as_bool() == Some(true);
    send_to_task_worker(core, &task, text, interrupt).await.map_err(|e| ApiErr::new(409, "send_failed", e))
}

pub(super) fn post_message_http(drove: &DroveRuntime, body: &Value) -> Result<Value, ApiErr> {
    let run_id = body["runId"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::new(400, "bad_request", "runId required"))?;
    if drove.store.get_run(run_id).is_none() {
        return Err(ApiErr::new(404, "not_found", "Run not found"));
    }
    let to = body["to"].as_str().unwrap_or("coordinator").to_string();
    let msg = drove.store.post_message(PostMessageInput {
        run_id: run_id.into(),
        kind: body.get("type").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or(MessageType::Note),
        from: body["from"].as_str().unwrap_or("coordinator").into(),
        to,
        thread_id: body["threadId"].as_str().map(String::from),
        task_id: body["taskId"].as_str().map(String::from),
        attempt_id: body["attemptId"].as_str().map(String::from),
        subject: body["subject"].as_str().unwrap_or("Note").into(),
        body: body["body"].as_str().map(String::from),
        options: None,
        recommend: None,
        why: None,
        raised_by: Some("coordinator".into()),
        raised_by_worker: None,
        tag: None,
        ai_draft: None,
        needs_action: false,
    });
    Ok(json!(msg))
}

fn validate_json_schema(schema: &Value, value: &Value, path: &str) -> Vec<String> {
    let Some(obj) = schema.as_object() else { return Vec::new() };
    let mut errs = Vec::new();
    let type_matches = |t: &str| match t {
        "null" => value.is_null(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        _ => false,
    };
    if let Some(t) = obj.get("type") {
        let ok = match t {
            Value::String(s) => type_matches(s),
            Value::Array(arr) => arr.iter().filter_map(Value::as_str).any(type_matches),
            _ => true,
        };
        if !ok {
            return vec![format!("{path}: expected type {t}")];
        }
    }
    if let Some(en) = obj.get("enum").and_then(Value::as_array)
        && !en.contains(value)
    {
        errs.push(format!("{path}: not in enum"));
    }
    if let Some(vmap) = value.as_object() {
        if let Some(req) = obj.get("required").and_then(Value::as_array) {
            for r in req.iter().filter_map(Value::as_str) {
                if !vmap.contains_key(r) {
                    errs.push(format!("{path}.{r}: missing required field"));
                }
            }
        }
        if let Some(props) = obj.get("properties").and_then(Value::as_object) {
            for (k, sub_schema) in props {
                if let Some(child) = vmap.get(k) {
                    errs.extend(validate_json_schema(sub_schema, child, &format!("{path}.{k}")));
                }
            }
        }
    }
    if let Some(varr) = value.as_array()
        && let Some(item_schema) = obj.get("items")
    {
        for (i, item) in varr.iter().enumerate() {
            errs.extend(validate_json_schema(item_schema, item, &format!("{path}[{i}]")));
        }
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;

    fn test_drove_runtime() -> Arc<DroveRuntime> {
        let dir = std::env::temp_dir().join(format!("herdr2-routes-test-{}", super::super::crypto::rand_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths {
            state: dir.clone(),
            config: dir.join("config.toml"),
            socket: dir.join("core.sock"),
            holder_socket: dir.join("core.holder.sock"),
        };
        let ai = crate::ai::AiRuntime::open(&paths).unwrap();
        DroveRuntime::open(&paths, ai).unwrap()
    }

    #[test]
    fn authenticate_worker_resolves_active_attempt_when_env_attempt_is_stale() {
        let drove = test_drove_runtime();
        let tok = drove.register_worker_token("wrk_reused");
        drove.store.insert_worker(Worker {
            id: "wrk_reused".into(),
            run_id: "run_1".into(),
            agent: "codex".into(),
            model: None,
            context_window: None,
            effort: None,
            fast: None,
            session_key: "run_1:wrk_reused".into(),
            h2_agent: Some("a5q".into()),
            cwd: "/tmp".into(),
            worktree: None,
            state: WorkerState::Busy,
            current_attempt_id: Some("att_new".into()),
            pid: None,
            fingerprint: Fingerprint::default(),
            context: WorkerContext::default(),
            totals: WorkerTotals::default(),
            created_at: now_iso(),
            last_active_at: now_iso(),
            role: Some("coder".into()),
            forked_from: None,
            protocol_gen: Some(1),
            read_only_dir: None,
        });
        let mk_att = |id: &str, ended: Option<String>, outcome: Option<Outcome>| Attempt {
            id: id.into(),
            task_id: "tsk_1".into(),
            run_id: "run_1".into(),
            worker_id: "wrk_reused".into(),
            agent: "codex".into(),
            model: None,
            effort: None,
            fast: None,
            route_reason: "test".into(),
            reused: id == "att_new",
            retry_of: None,
            hint: None,
            started_at: now_iso(),
            ended_at: ended,
            awaiting_late_done: None,
            tokens: TokenCounts::default(),
            cost: None,
            estimated: None,
            outcome,
            failure: None,
            result: None,
            kind: Some("author".into()),
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
        drove.store.insert_attempt(mk_att("att_old", Some(now_iso()), Some(Outcome::Succeeded)));
        drove.store.insert_attempt(mk_att("att_new", None, None));

        let mut headers = BTreeMap::new();
        headers.insert("x-drove-worker-token".into(), tok);

        // 1. Stale env `attemptId` alone resolves dynamically to worker's active attempt `att_new`.
        let resolved = authenticate_worker(&drove, &headers, &json!({ "attemptId": "att_old" })).unwrap();
        assert_eq!(resolved.id, "att_new");

        // 2. `workerId` + stale `attemptId` also resolves to `att_new`.
        let resolved2 =
            authenticate_worker(&drove, &headers, &json!({ "workerId": "wrk_reused", "attemptId": "att_old" })).unwrap();
        assert_eq!(resolved2.id, "att_new");

        // 3. Explicit `--attempt att_old` (`explicitAttempt: true`) must still reject stale attempt.
        let explicit_err = authenticate_worker(
            &drove,
            &headers,
            &json!({ "workerId": "wrk_reused", "attemptId": "att_old", "explicitAttempt": true }),
        );
        assert!(explicit_err.is_err());

        // 4. Explicit `--attempt att_new` succeeds.
        let explicit_ok = authenticate_worker(
            &drove,
            &headers,
            &json!({ "workerId": "wrk_reused", "attemptId": "att_new", "explicitAttempt": true }),
        )
        .unwrap();
        assert_eq!(explicit_ok.id, "att_new");
    }
}
