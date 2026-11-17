//! M2+M3 orchestration and worker protocol HTTP handlers called by `drove::http`.

use super::http::ApiErr;
use super::notify::notify_coordinator;
use super::pool::{mark_worker_idle, send_to_task_worker, stop_run_workers, stop_worker};
use super::roles::{diff_touched_paths, find_scope_violations};
use super::store::{NewDaemonEvent, PostMessageInput, empty_task_counts};
use super::types::*;
use super::DroveRuntime;
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
    let att_id = body["attemptId"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::new(400, "bad_request", "attemptId must be a non-empty string"))?;
    let att = drove
        .store
        .get_attempt(att_id)
        .ok_or_else(|| ApiErr::new(404, "not_found", "Attempt not found"))?;
    if !drove.verify_worker_token(&att.worker_id, tok) {
        return Err(ApiErr::new(401, "unauthorized", "invalid worker token"));
    }
    let worker = drove
        .store
        .get_worker(&att.worker_id)
        .ok_or_else(|| ApiErr::new(404, "not_found", "Worker not found"))?;
    if att.outcome.is_some()
        || att.ended_at.is_some()
        || matches!(worker.state, WorkerState::Dead | WorkerState::Closed)
        || worker.current_attempt_id.as_deref() != Some(att.id.as_str())
    {
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
    let worker_id = body["workerId"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::new(400, "bad_request", "workerId must be a non-empty string"))?;
    let worker = drove
        .store
        .get_worker(worker_id)
        .ok_or_else(|| ApiErr::new(404, "not_found", "Worker not found"))?;
    if !drove.verify_worker_token(worker_id, tok) {
        return Err(ApiErr::new(401, "unauthorized", "invalid worker token"));
    }
    let att_id = worker
        .current_attempt_id
        .ok_or_else(|| ApiErr::new(409, "stale_attempt", "worker has no active attempt"))?;
    let att = drove
        .store
        .get_attempt(&att_id)
        .ok_or_else(|| ApiErr::new(409, "stale_attempt", "attempt missing"))?;
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
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect()
    });
    let recommend = body["recommend"].as_str().map(String::from);
    let why = body["why"].as_str().map(String::from);
    let run = core
        .drove
        .store
        .get_run(&att.run_id)
        .ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
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
        let task = drove
            .store
            .get_task(&att.task_id)
            .ok_or_else(|| ApiErr::new(404, "not_found", "Task not found"))?;
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
    let run = core
        .drove
        .store
        .get_run(&att.run_id)
        .ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    let task = core
        .drove
        .store
        .get_task(&att.task_id)
        .ok_or_else(|| ApiErr::new(404, "not_found", "Task not found"))?;
    let worker = core
        .drove
        .store
        .get_worker(&att.worker_id)
        .ok_or_else(|| ApiErr::new(404, "not_found", "Worker not found"))?;

    let outcome_raw = body["outcome"]
        .as_str()
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "done".into());
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
        mark_worker_idle(core, &worker.id);
        let max_r = if body["noRetry"].as_bool() == Some(true) { 0 } else { super::MAX_AUTO_RETRIES };
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
        body["filesModified"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
    };

    // M3 Gate 1: Task `spec.verify`
    let _ = super::verify::run_task_verify_gate(core, &run, &task, &worker, &att, work_cwd).await?;
    let refreshed_att = core.drove.store.get_attempt(&att.id).unwrap_or(att);
    let refreshed_task = core.drove.store.get_task(&task.id).unwrap_or(task);

    // M3 Gate 2: Adversarial read-only Review loop
    super::review::run_adversarial_review_gate(core, &run, &refreshed_task, &worker, &refreshed_att, work_cwd).await?;

    // M3 Gate 3: Worktree serialized merge-back & cleanup
    let merged_files = super::integrate::merge_and_cleanup_worktree(core, &run, worker.worktree.as_deref()).await?;
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
    let upd_task = core.drove.store.update_task(&refreshed_task.id, |t| {
        t.status = TaskStatus::Succeeded;
        t.exec_phase = None;
        t.result = Some(result.clone());
    });
    mark_worker_idle(core, &worker.id);
    if worker.worktree.is_some() {
        let _ = core.drove.store.update_worker(&worker.id, |w| {
            w.worktree = None;
            w.cwd = run.cwd.clone();
        });
    }
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

    // If all non-template tasks in this run are now terminal, notify coordinator cleanly.
    let all_tasks = core.drove.store.list_tasks(&run.id);
    if !all_tasks.is_empty()
        && all_tasks
            .iter()
            .filter(|t| t.template != Some(true))
            .all(|t| super::scheduler::is_terminal_status(t.status))
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

pub(super) async fn add_tasks_http(core: &Arc<Core>, run_id: &str, body: &Value) -> Result<Value, ApiErr> {
    let run = core
        .drove
        .store
        .get_run(run_id)
        .ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
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

    let existing_keys: BTreeSet<String> = core
        .drove
        .store
        .list_tasks(&run.id)
        .into_iter()
        .map(|t| t.key)
        .collect();
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

    core.drove.wake_scheduler();
    Ok(json!({ "tasks": created }))
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
    let title = raw["title"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&key)
        .to_string();
    let spec: Spec = if raw.get("spec").is_some_and(Value::is_object) {
        serde_json::from_value(raw["spec"].clone())
            .map_err(|e| ApiErr::new(400, "invalid_field", format!("invalid spec on {key}: {e}")))?
    } else {
        Spec {
            target: raw["target"].as_str().unwrap_or("workspace").to_string(),
            change: raw["change"]
                .as_str()
                .or_else(|| raw["prompt"].as_str())
                .unwrap_or(&title)
                .to_string(),
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
    let foreach_from: Option<ForeachFrom> = raw
        .get("foreachFrom")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let is_template = foreach_from.is_some();

    let mut assign: Option<Assign> = raw
        .get("assign")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    if let Some(role) = raw["role"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        let mut a = assign.unwrap_or_default();
        if a.role.is_none() {
            a.role = Some(role.to_string());
        }
        assign = Some(a);
    }

    let initial_status = if is_template || !deps.is_empty() {
        TaskStatus::Pending
    } else {
        TaskStatus::Ready
    };

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
        isolation: raw
            .get("isolation")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(Isolation::None),
        timeout_sec: raw["timeoutSec"].as_u64(),
        stall_warn_sec: raw["stallWarnSec"].as_u64(),
        stall_kill_sec: raw["stallKillSec"].as_u64(),
        when: raw.get("when").and_then(|v| serde_json::from_value(v.clone()).ok()),
        assign,
        status: initial_status,
        result: None,
        kind: raw["kind"].as_str().map(String::from),
        trigger_rule: raw
            .get("triggerRule")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        context: raw.get("context").cloned(),
        exec: raw.get("exec").and_then(|v| serde_json::from_value(v.clone()).ok()),
        foreach_from,
        template: is_template.then_some(true),
        expanded_from: None,
        review: raw.get("review").and_then(|v| serde_json::from_value(v.clone()).ok()),
        cache: raw.get("cache").cloned(),
        cached_from: None,
    });
    Ok(task)
}

pub(super) async fn wait_run_http(
    core: &Arc<Core>,
    run_id: &str,
    query: &BTreeMap<String, String>,
) -> Result<Value, ApiErr> {
    let run = core
        .drove
        .store
        .get_run(run_id)
        .ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    let timeout_sec = query
        .get("timeoutSec")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(20)
        .clamp(1, 25);
    let since_seq = query.get("sinceSeq").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let _ = run;

    let deadline = Instant::now() + Duration::from_secs(timeout_sec);
    loop {
        let cur_run = core
            .drove
            .store
            .get_run(run_id)
            .ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
        let tasks = core.drove.store.list_tasks(run_id);
        let pending: Vec<Message> = core
            .drove
            .store
            .list_messages(run_id, Some(true), true, None)
            .into_iter()
            .filter(|m| m.seq > since_seq)
            .collect();
        let has_failure = tasks.iter().any(|t| t.status == TaskStatus::Failed);
        let all_terminal = !tasks.is_empty()
            && tasks
                .iter()
                .filter(|t| t.template != Some(true))
                .all(|t| super::scheduler::is_terminal_status(t.status));

        if !pending.is_empty() || cur_run.status != RunStatus::Active || all_terminal || has_failure {
            let counts = core
                .drove
                .store
                .count_tasks_by_run()
                .remove(run_id)
                .unwrap_or_else(empty_task_counts);
            return Ok(json!({
                "run": cur_run,
                "taskCounts": counts,
                "messages": pending,
                "tasks": tasks,
                "settled": all_terminal || cur_run.status != RunStatus::Active,
                "timedOut": false,
            }));
        }

        if Instant::now() >= deadline {
            let counts = core
                .drove
                .store
                .count_tasks_by_run()
                .remove(run_id)
                .unwrap_or_else(empty_task_counts);
            return Ok(json!({
                "run": cur_run,
                "taskCounts": counts,
                "messages": [],
                "tasks": tasks,
                "settled": false,
                "timedOut": true,
            }));
        }
        let notified = core.drove.store.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let _ = tokio::time::timeout(Duration::from_millis(500), notified).await;
    }
}

pub(super) async fn complete_run_http(core: &Arc<Core>, run_id: &str, body: &Value) -> Result<Value, ApiErr> {
    let run = core
        .drove
        .store
        .get_run(run_id)
        .ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    let force = body["force"].as_bool() == Some(true);
    let tasks = core.drove.store.list_tasks(run_id);
    let active_left = tasks.iter().any(|t| {
        t.template != Some(true)
            && matches!(t.status, TaskStatus::Pending | TaskStatus::Ready | TaskStatus::Running | TaskStatus::Blocked)
    });
    if active_left && !force {
        return Err(ApiErr::new(409, "tasks_incomplete", "run still has non-terminal tasks (pass force=true to complete anyway)"));
    }

    stop_run_workers(core, &run.id).await;
    let verify_log_id = if !force {
        let (log_id, repair_task) = super::verify::run_level_verify_gate(core, &run).await?;
        if let Some(rt) = repair_task {
            return Ok(json!({ "ok": false, "repairing": true, "repairTask": rt, "run": core.drove.store.get_run(&run.id) }));
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
    let run = core
        .drove
        .store
        .get_run(run_id)
        .ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
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

pub(super) fn retry_task_http(core: &Arc<Core>, id: &str, body: &Value) -> Result<Value, ApiErr> {
    let task = core
        .drove
        .store
        .get_task(id)
        .ok_or_else(|| ApiErr::new(404, "not_found", "Task not found"))?;
    if task.status == TaskStatus::Running {
        return Err(ApiErr::new(409, "invalid_state", "cannot retry a running task"));
    }
    if let Some(hint) = body["hint"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        core.drove.set_task_hint(&task.id, hint.to_string());
    }
    let updated = core.drove.store.update_task(&task.id, |t| {
        t.status = TaskStatus::Ready;
        t.failures = 0;
        let mut a = t.assign.clone().unwrap_or_default();
        if let Some(role) = body["role"].as_str() {
            a.role = Some(role.to_string());
        }
        if let Some(agent) = body["agent"].as_str() {
            a.agent = Some(agent.to_string());
        }
        if let Some(model) = body["model"].as_str() {
            a.model = Some(model.to_string());
        }
        if let Some(eff) = body.get("effort").and_then(|v| serde_json::from_value(v.clone()).ok()) {
            a.effort = Some(eff);
        }
        if body["fresh"].as_bool() == Some(true) {
            a.worker = None;
        }
        t.assign = Some(a);
    });
    core.drove.wake_scheduler();
    Ok(json!(updated))
}

pub(super) async fn cancel_task_http(core: &Arc<Core>, id: &str) -> Result<Value, ApiErr> {
    let task = core
        .drove
        .store
        .get_task(id)
        .ok_or_else(|| ApiErr::new(404, "not_found", "Task not found"))?;
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
            a.failure = Some(AttemptFailure { kind: FailureKind::Cancelled, message: "Cancelled by coordinator".into() });
        });
    }
    let updated = core.drove.store.update_task(&task.id, |t| t.status = TaskStatus::Cancelled);
    core.drove.wake_scheduler();
    Ok(json!(updated))
}

pub(super) async fn send_task_http(core: &Arc<Core>, id: &str, body: &Value) -> Result<Value, ApiErr> {
    let task = core
        .drove
        .store
        .get_task(id)
        .ok_or_else(|| ApiErr::new(404, "not_found", "Task not found"))?;
    let text = body["text"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::new(400, "bad_request", "text must be non-empty"))?;
    let interrupt = body["interrupt"].as_bool() == Some(true);
    send_to_task_worker(core, &task, text, interrupt)
        .await
        .map_err(|e| ApiErr::new(409, "send_failed", e))
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
        kind: body
            .get("type")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(MessageType::Note),
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
