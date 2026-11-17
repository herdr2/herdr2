//! Native `herdr2` headless ACP worker pool lifecycle (`a*` sessions, warm reuse, transcripts, kill sync).

use super::notify::notify_coordinator;
use super::protocol::build_task_prompt;
use super::router::RouteDecision;
use super::store::NewDaemonEvent;
use super::types::*;
use super::{fail_or_retry_task, MAX_AUTO_RETRIES};
use crate::acp::{self, proto::AcpContextUsage};
use crate::api::run::{DroveSpawnSpec, spawn_drove_agent, update_agent_display};
use crate::runtime::{Core, TargetKey};
use crate::transcript::Entry;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub enum PoolSignal {
    Transcript { worker_id: String, entry: Entry },
    TurnFinished {
        worker_id: String,
        agent_id: u32,
        error: Option<String>,
        usage: Option<AcpContextUsage>,
    },
    AgentExited { worker_id: String, agent_id: u32, reason: String },
}

/// Runs in background processing transcript, turn-completion, and external kill signals from `Core`.
pub async fn run_signal_loop(core: Arc<Core>, mut rx: mpsc::UnboundedReceiver<PoolSignal>) {
    while let Some(sig) = rx.recv().await {
        handle_signal(&core, sig).await;
    }
}

async fn handle_signal(core: &Arc<Core>, sig: PoolSignal) {
    match sig {
        PoolSignal::Transcript { worker_id, entry } => {
            let attempt_id = core
                .drove
                .store
                .get_worker(&worker_id)
                .and_then(|w| w.current_attempt_id);
            record_transcript_item(core, &worker_id, attempt_id, entry);
            let _ = core
                .drove
                .store
                .update_worker(&worker_id, |w| w.last_active_at = now_iso());
        }
        PoolSignal::TurnFinished { worker_id, agent_id, error, usage } => {
            if let Some(u) = usage {
                let _ = core.drove.store.update_worker(&worker_id, |w| {
                    w.context = WorkerContext { used: u.used, size: u.size };
                    w.last_active_at = now_iso();
                });
            }
            let Some(worker) = core.drove.store.get_worker(&worker_id) else {
                return;
            };
            let Some(attempt_id) = worker.current_attempt_id.clone() else {
                return;
            };
            let Some(attempt) = core.drove.store.get_attempt(&attempt_id) else {
                return;
            };
            if attempt.outcome.is_some() || attempt.ended_at.is_some() {
                return;
            }
            // Don't prematurely fail if a blocking `drove worker ask` is currently open.
            if core.drove.has_active_ask(&attempt_id) {
                return;
            }
            if let Some(err_msg) = error {
                finish_unsettled_attempt(
                    core,
                    &worker,
                    &attempt,
                    FailureKind::AgentError,
                    err_msg,
                    true,
                )
                .await;
                return;
            }
            if !core.drove.mark_attempt_reminded(&attempt_id) {
                let msg = format!(
                    "You ended your turn without calling `h2 drove worker done`. \
                     If you finished task `{}`, run `h2 drove worker done --outcome done --summary \"...\"` \
                     (or `--outcome failed --reason \"...\"`).",
                    attempt.task_id
                );
                let _ = core.send(agent_id, &msg, false, None, Some("drove")).await;
                return;
            }
            finish_unsettled_attempt(
                core,
                &worker,
                &attempt,
                FailureKind::NoDone,
                "Worker ended turn without calling drove worker done".into(),
                true,
            )
            .await;
        }
        PoolSignal::AgentExited { worker_id, agent_id: _, reason } => {
            let Some(worker) = core.drove.store.get_worker(&worker_id) else {
                return;
            };
            let was_closed = worker.state == WorkerState::Closed;
            let open_att = worker
                .current_attempt_id
                .as_deref()
                .and_then(|aid| core.drove.store.get_attempt(aid))
                .filter(|a| a.outcome.is_none() && a.ended_at.is_none());
            let _ = core.drove.store.update_worker(&worker_id, |w| {
                if !was_closed {
                    w.state = WorkerState::Dead;
                }
                w.current_attempt_id = None;
                w.pid = None;
                w.last_active_at = now_iso();
            });
            if let Some(att) = open_att {
                let kind = if was_closed {
                    FailureKind::Cancelled
                } else {
                    FailureKind::ProcessExited
                };
                finish_unsettled_attempt(
                    core,
                    &worker,
                    &att,
                    kind,
                    format!("Worker ACP process ended ({reason})"),
                    false,
                )
                .await;
            }
            core.drove.wake_scheduler();
        }
    }
}

async fn finish_unsettled_attempt(
    core: &Arc<Core>,
    worker: &Worker,
    attempt: &Attempt,
    kind: FailureKind,
    message: String,
    keep_worker_idle: bool,
) {
    let ended_at = now_iso();
    let _ = core.drove.store.update_attempt(&attempt.id, |a| {
        if a.outcome.is_none() {
            a.outcome = Some(Outcome::Failed);
            a.ended_at = Some(ended_at.clone());
            a.failure = Some(AttemptFailure { kind, message: message.clone() });
        }
    });
    if keep_worker_idle {
        mark_worker_idle(core, &worker.id);
    } else {
        let _ = core.drove.store.update_worker(&worker.id, |w| {
            w.current_attempt_id = None;
            w.last_active_at = now_iso();
        });
    }
    fail_or_retry_task(core, &attempt.run_id, &attempt.task_id, &attempt.id, kind, &message, MAX_AUTO_RETRIES).await;
    core.drove.wake_scheduler();
}

/// Dispatches an agent task onto either an idle warm worker or a fresh `herdr2` headless ACP `a*` session.
pub async fn dispatch_agent_task(
    core: &Arc<Core>,
    run: &Run,
    task: &Task,
    decision: RouteDecision,
    effective_cwd: String,
    worktree: Option<String>,
    retry_of: Option<String>,
    hint: Option<String>,
) -> Result<Attempt, String> {
    let dep_tasks = resolved_dep_tasks(&core.drove.store.list_tasks(&run.id), task);
    let baseline = super::roles::capture_dirty_baseline(&effective_cwd);
    let attempt_id = super::crypto::rand_id("att");

    if let Some(mut warm) = decision.warm_worker.clone() {
        let aid = warm
            .h2_agent
            .as_deref()
            .and_then(crate::agents::parse_id)
            .ok_or_else(|| "warm worker missing h2 agent id".to_string())?;
        // Switch model/effort dynamically if needed.
        if decision.model.as_deref() != warm.model.as_deref()
            && let Some(m) = &decision.model
        {
            apply_warm_config(core, aid, "model", json!(m)).await;
        }
        if Some(decision.effort) != warm.effort {
            apply_warm_config(core, aid, "reasoning_effort", json!(decision.effort.as_str())).await;
        }

        let display = format!("{} · {}", decision.role, task.key);
        let mut patch = BTreeMap::new();
        patch.insert("drove.role".into(), Some(decision.role.clone()));
        patch.insert("drove.task".into(), Some(task.key.clone()));
        patch.insert("drove.display".into(), Some(display.clone()));
        update_agent_display(core, aid, display, patch);

        let now = now_iso();
        warm.state = WorkerState::Busy;
        warm.current_attempt_id = Some(attempt_id.clone());
        warm.last_active_at = now.clone();
        warm.model = decision.model.clone().or(warm.model);
        warm.effort = Some(decision.effort);
        warm.totals.attempts += 1;
        update_fingerprint(&mut warm.fingerprint, task);
        core.drove.store.update_worker(&warm.id, |w| *w = warm.clone());

        let attempt = build_attempt_record(
            &attempt_id,
            run,
            task,
            &warm.id,
            &decision,
            true,
            retry_of,
            hint.clone(),
        );
        core.drove.store.insert_attempt(attempt.clone());
        core.drove.record_attempt_baseline(&attempt_id, baseline);
        let preserved_source = next_assign_source(task);
        core.drove.store.update_task(&task.id, |t| {
            t.status = TaskStatus::Running;
            t.exec_phase = Some(TaskPhase::Author);
            t.assign = Some(Assign {
                agent: Some(decision.agent.clone()),
                model: decision.model.clone(),
                effort: Some(decision.effort),
                worker: Some(warm.id.clone()),
                reason: Some(decision.reason.clone()),
                source: preserved_source.clone(),
                role: Some(decision.role.clone()),
                fork: None,
            });
        });

        let prompt = build_task_prompt(
            run,
            task,
            &attempt,
            &decision.role,
            &decision.role_config,
            &dep_tasks,
            hint.as_deref(),
        );
        if let Err(e) = core.send(aid, &prompt, false, None, Some("drove")).await {
            let err_msg = e.message;
            mark_worker_idle(core, &warm.id);
            let _ = core.drove.store.update_attempt(&attempt_id, |a| {
                a.outcome = Some(Outcome::Failed);
                a.ended_at = Some(now_iso());
                a.failure = Some(AttemptFailure { kind: FailureKind::AgentError, message: err_msg.clone() });
            });
            return Err(err_msg);
        }
        return Ok(attempt);
    }

    // Spawn a brand-new `herdr2` headless ACP `a*` worker.
    let worker_id = super::crypto::rand_id("wrk");
    let worker_token = core.drove.register_worker_token(&worker_id);
    let port = core.drove.web_port.load(Ordering::SeqCst);
    let drove_url = format!("http://127.0.0.1:{port}");
    let display = format!("{} · {}", decision.role, task.key);

    let mut labels = BTreeMap::new();
    labels.insert("drove.run".into(), run.id.clone());
    labels.insert("drove.worker".into(), worker_id.clone());
    labels.insert("drove.role".into(), decision.role.clone());
    labels.insert("drove.task".into(), task.key.clone());
    labels.insert("drove.display".into(), display.clone());

    let mut extra_env = BTreeMap::new();
    extra_env.insert("H2_DROVE_URL".into(), drove_url);
    extra_env.insert("H2_DROVE_WORKER_TOKEN".into(), worker_token);
    extra_env.insert("H2_DROVE_WORKER_ID".into(), worker_id.clone());
    extra_env.insert("H2_DROVE_RUN_ID".into(), run.id.clone());
    extra_env.insert("H2_DROVE_ATTEMPT_ID".into(), attempt_id.clone());
    extra_env.insert("H2_DROVE_BIN_DIR".into(), core.drove.paths.state_dir.join("bin").display().to_string());

    let now = now_iso();
    let mut fp = Fingerprint::default();
    update_fingerprint(&mut fp, task);
    let worker = Worker {
        id: worker_id.clone(),
        run_id: run.id.clone(),
        agent: decision.agent.clone(),
        model: decision.model.clone(),
        effort: Some(decision.effort),
        session_key: format!("{}:{worker_id}", run.id),
        h2_agent: None,
        cwd: effective_cwd.clone(),
        worktree,
        state: WorkerState::Starting,
        current_attempt_id: Some(attempt_id.clone()),
        pid: None,
        fingerprint: fp,
        context: WorkerContext::default(),
        totals: WorkerTotals { tokens: 0, cost: 0.0, attempts: 1, estimated: None },
        created_at: now.clone(),
        last_active_at: now,
        role: Some(decision.role.clone()),
        forked_from: None,
        protocol_gen: Some(1),
        read_only_dir: None,
    };
    core.drove.store.insert_worker(worker);

    let attempt = build_attempt_record(
        &attempt_id,
        run,
        task,
        &worker_id,
        &decision,
        false,
        retry_of,
        hint.clone(),
    );
    core.drove.store.insert_attempt(attempt.clone());
    core.drove.record_attempt_baseline(&attempt_id, baseline);
    let preserved_source = next_assign_source(task);
    core.drove.store.update_task(&task.id, |t| {
        t.status = TaskStatus::Running;
        t.exec_phase = Some(TaskPhase::Author);
        t.assign = Some(Assign {
            agent: Some(decision.agent.clone()),
            model: decision.model.clone(),
            effort: Some(decision.effort),
            worker: Some(worker_id.clone()),
            reason: Some(decision.reason.clone()),
            source: preserved_source.clone(),
            role: Some(decision.role.clone()),
            fork: None,
        });
    });

    let access = decision.role_config.access.unwrap_or(RoleAccessMode::Writable);
    let spawn_res = spawn_drove_agent(
        core,
        DroveSpawnSpec {
            agent: decision.agent.clone(),
            cwd: effective_cwd,
            parent: resolve_local_parent(core, &run.coordinator.host_session),
            title: display,
            labels,
            env: extra_env,
            model: decision.model.clone(),
            effort: Some(decision.effort.as_str().to_string()),
            access,
            isolate_secrets: true,
        },
    )
    .await;

    let spawned = match spawn_res {
        Ok(s) => s,
        Err(e) => {
            let err_msg = e.message;
            let _ = core.drove.store.update_worker(&worker_id, |w| {
                w.state = WorkerState::Dead;
                w.current_attempt_id = None;
            });
            let _ = core.drove.store.update_attempt(&attempt_id, |a| {
                a.outcome = Some(Outcome::Failed);
                a.ended_at = Some(now_iso());
                a.failure = Some(AttemptFailure { kind: FailureKind::AgentError, message: err_msg.clone() });
            });
            return Err(err_msg);
        }
    };

    core.drove.store.update_worker(&worker_id, |w| {
        w.state = WorkerState::Busy;
        w.h2_agent = Some(spawned.address.clone());
        w.pid = spawned.pid;
        w.last_active_at = now_iso();
    });
    core.drove.store.append_daemon_event(NewDaemonEvent {
        run_id: Some(run.id.clone()),
        task_id: Some(task.id.clone()),
        attempt_id: Some(attempt_id.clone()),
        worker_id: Some(worker_id.clone()),
        message_id: None,
        category: "worker".into(),
        kind: "spawn".into(),
        status: "ok".into(),
        title: format!("Spawned {} as {}", decision.agent, spawned.address),
        detail: Some(format!("role={} sandbox={}", decision.role, spawned.sandbox_mode)),
        duration_ms: None,
        tokens: None,
        cost: None,
    });

    let prompt = build_task_prompt(
        run,
        task,
        &attempt,
        &decision.role,
        &decision.role_config,
        &dep_tasks,
        hint.as_deref(),
    );
    if let Err(e) = core.send(spawned.agent_id, &prompt, false, None, Some("drove")).await {
        let err_msg = e.message;
        let _ = core.drove.store.update_worker(&worker_id, |w| {
            w.state = WorkerState::Idle;
            w.current_attempt_id = None;
        });
        let _ = core.drove.store.update_attempt(&attempt_id, |a| {
            a.outcome = Some(Outcome::Failed);
            a.ended_at = Some(now_iso());
            a.failure = Some(AttemptFailure { kind: FailureKind::AgentError, message: err_msg.clone() });
        });
        return Err(err_msg);
    }

    Ok(attempt)
}

/// Transitions a live worker back to idle and updates its bottom-left `herdr-app` display label.
pub fn mark_worker_idle(core: &Arc<Core>, worker_id: &str) {
    let Some(updated) = core.drove.store.update_worker(worker_id, |w| {
        if w.state != WorkerState::Closed && w.state != WorkerState::Dead {
            w.state = WorkerState::Idle;
        }
        w.current_attempt_id = None;
        w.last_active_at = now_iso();
    }) else {
        return;
    };
    if let Some(addr) = updated.h2_agent.as_deref()
        && let Some(aid) = crate::agents::parse_id(addr)
    {
        let role = updated.role.as_deref().unwrap_or("coder");
        let idle_title = format!("{role} · idle");
        let mut patch = BTreeMap::new();
        patch.insert("drove.task".into(), None);
        patch.insert("drove.display".into(), Some(idle_title.clone()));
        update_agent_display(core, aid, idle_title, patch);
    }
}

/// Stops a worker immediately: updates Store state, kills its `herdr2` ACP agent, and settles active attempt if open.
pub async fn stop_worker(core: &Arc<Core>, worker_id: &str) -> Option<Worker> {
    let worker = core.drove.store.get_worker(worker_id)?;
    let open_att = worker
        .current_attempt_id
        .as_deref()
        .and_then(|aid| core.drove.store.get_attempt(aid))
        .filter(|a| a.outcome.is_none() && a.ended_at.is_none());

    let updated = core.drove.store.update_worker(worker_id, |w| {
        w.state = WorkerState::Closed;
        w.current_attempt_id = None;
        w.last_active_at = now_iso();
    });

    if let Some(addr) = worker.h2_agent.as_deref()
        && let Some(aid) = crate::agents::parse_id(addr)
    {
        core.kill(&[aid]).await;
    }

    if let Some(att) = open_att {
        let ended = now_iso();
        let _ = core.drove.store.update_attempt(&att.id, |a| {
            a.outcome = Some(Outcome::Failed);
            a.ended_at = Some(ended);
            a.failure = Some(AttemptFailure { kind: FailureKind::Cancelled, message: "Worker stopped".into() });
        });
        let _ = core.drove.store.update_task(&att.task_id, |t| {
            if t.status == TaskStatus::Running {
                t.status = TaskStatus::Cancelled;
            }
        });
    }
    updated
}

/// Stops all non-closed workers belonging to `run_id`.
pub async fn stop_run_workers(core: &Arc<Core>, run_id: &str) {
    let workers = core.drove.store.list_workers(Some(run_id));
    for w in workers {
        if w.state != WorkerState::Closed && w.state != WorkerState::Dead {
            let _ = stop_worker(core, &w.id).await;
        }
    }
}

/// Sends a message to an active task's worker (optionally interrupting current ACP turn first).
pub async fn send_to_task_worker(
    core: &Arc<Core>,
    task: &Task,
    text: &str,
    interrupt: bool,
) -> Result<Value, String> {
    let attempts = core.drove.store.list_attempts(None, Some(&task.id), None);
    let latest = attempts
        .iter()
        .rev()
        .find(|a| a.outcome.is_none())
        .or_else(|| attempts.last())
        .ok_or_else(|| "Task has no worker attempt yet".to_string())?;
    let worker = core
        .drove
        .store
        .get_worker(&latest.worker_id)
        .ok_or_else(|| "Worker not found".to_string())?;
    let aid = worker
        .h2_agent
        .as_deref()
        .and_then(crate::agents::parse_id)
        .ok_or_else(|| "Worker has no live herdr2 ACP agent".to_string())?;

    if interrupt {
        let _ = core
            .interrupt(aid, std::time::Duration::from_secs(5))
            .await;
    }
    let _ = core.drove.store.post_message(super::store::PostMessageInput {
        run_id: task.run_id.clone(),
        kind: MessageType::Note,
        from: "coordinator".into(),
        to: format!("worker:{}", worker.id),
        thread_id: None,
        task_id: Some(task.id.clone()),
        attempt_id: Some(latest.id.clone()),
        subject: format!("Message for {}", task.key),
        body: Some(text.to_string()),
        options: None,
        recommend: None,
        why: None,
        raised_by: Some("coordinator".into()),
        raised_by_worker: None,
        tag: None,
        ai_draft: None,
        needs_action: false,
    });
    core.send(aid, text, interrupt, None, Some("drove"))
        .await
        .map_err(|e| e.message)?;
    Ok(json!({ "ok": true, "workerId": worker.id, "agent": worker.h2_agent, "interrupted": interrupt }))
}

async fn apply_warm_config(core: &Arc<Core>, aid: u32, cfg_id: &str, val: Value) {
    let (sid, src, tr) = {
        let st = core.lock();
        let Some(a) = st.reg.agents.get(&aid) else { return };
        let Some(sid) = a.acp_session_id.clone() else { return };
        let src = a.config_options.iter().find(|o| o.id == cfg_id).map(|o| o.source.clone());
        let Some(tr) = st.sessions.get(&aid).map(|h| h.transport.clone()) else { return };
        (sid, src, tr)
    };
    if let Ok(maybe_opts) = acp::apply_config_option(&tr, &sid, cfg_id, &val, src.as_deref()).await {
        let mut st = core.lock();
        if let Some(a) = st.reg.agents.get_mut(&aid) {
            a.config_overrides.insert(cfg_id.into(), val.clone());
            if let Some(opts) = maybe_opts {
                a.config_options = opts;
            } else {
                acp::patch_config_value(&mut a.config_options, cfg_id, &val);
            }
            acp::sync_config_to_agent(a);
            let data = acp::config_event_data(a);
            core.emit(&mut st, "config", aid, data);
            core.save(&st);
        }
    }
}

fn build_attempt_record(
    attempt_id: &str,
    run: &Run,
    task: &Task,
    worker_id: &str,
    decision: &RouteDecision,
    reused: bool,
    retry_of: Option<String>,
    hint: Option<String>,
) -> Attempt {
    Attempt {
        id: attempt_id.into(),
        task_id: task.id.clone(),
        run_id: run.id.clone(),
        worker_id: worker_id.into(),
        agent: decision.agent.clone(),
        model: decision.model.clone(),
        effort: Some(decision.effort),
        route_reason: decision.reason.clone(),
        reused,
        retry_of,
        hint,
        started_at: now_iso(),
        ended_at: None,
        awaiting_late_done: None,
        tokens: TokenCounts::default(),
        cost: None,
        estimated: None,
        outcome: None,
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
    }
}

fn resolved_dep_tasks(all: &[Task], task: &Task) -> Vec<Task> {
    all.iter()
        .filter(|candidate| {
            task.deps
                .iter()
                .any(|d| d == &candidate.id || d == &candidate.key)
        })
        .cloned()
        .collect()
}

fn resolve_local_parent(core: &Core, host_session: &str) -> Option<String> {
    let local = match host_session.rsplit_once('/') {
        Some((_, r)) => r,
        None => host_session,
    };
    let tk = TargetKey::parse(local)?;
    let st = core.lock();
    match tk {
        TargetKey::Terminal(id) => st
            .pty_reg
            .terminals
            .get(&id)
            .filter(|t| t.is_live())
            .map(|_| crate::terminals::address(id)),
        TargetKey::Agent(id) => st
            .reg
            .agents
            .get(&id)
            .filter(|a| a.is_live())
            .map(|_| crate::agents::address(id)),
    }
}

fn update_fingerprint(fp: &mut Fingerprint, task: &Task) {
    if !fp.task_ids.contains(&task.id) {
        fp.task_ids.push(task.id.clone());
    }
    for f in &task.files {
        if !fp.files.contains(f) {
            fp.files.push(f.clone());
        }
    }
    for t in &task.tags {
        if !fp.tags.contains(t) {
            fp.tags.push(t.clone());
        }
    }
}

fn record_transcript_item(core: &Core, worker_id: &str, attempt_id: Option<String>, entry: Entry) {
    match entry {
        Entry::Assistant { text, .. } if !text.is_empty() => {
            core.drove
                .store
                .append_item(worker_id, attempt_id, "assistant", Some(text), None, None);
        }
        Entry::Thought { text, .. } if !text.is_empty() => {
            core.drove
                .store
                .append_item(worker_id, attempt_id, "thought", Some(text), None, None);
        }
        Entry::ToolCall { id, title, tool_kind, status, .. } => {
            core.drove.store.append_item(
                worker_id,
                attempt_id,
                "tool",
                None,
                Some(ItemTool {
                    id,
                    title: Some(title),
                    kind: tool_kind,
                    status: Some(status),
                    locations: None,
                    input: None,
                    output: None,
                }),
                None,
            );
        }
        Entry::Error { message, .. } => {
            core.drove.store.append_item(
                worker_id,
                attempt_id,
                "system",
                Some(format!("[error] {message}")),
                None,
                None,
            );
        }
        Entry::Stderr { text, .. } if !text.trim().is_empty() => {
            core.drove
                .store
                .append_item(worker_id, attempt_id, "system", Some(text), None, None);
        }
        _ => {}
    }
}

/// Preserves user/plan explicit overrides while marking router-selected assignments as `"router"` so retries can step down fallback chains.
fn next_assign_source(task: &Task) -> Option<String> {
    let prev_source = task.assign.as_ref().and_then(|a| a.source.as_deref());
    let had_explicit_agent = task
        .assign
        .as_ref()
        .and_then(|a| a.agent.as_deref())
        .is_some_and(|s| !s.trim().is_empty())
        && prev_source != Some("router");
    match prev_source {
        Some(s) => Some(s.to_string()),
        None if had_explicit_agent => Some("plan".into()),
        None => Some("router".into()),
    }
}

/// Runs automatic retry or marks task Failed and notifies coordinator.
pub async fn handle_task_failure(
    core: &Arc<Core>,
    run_id: &str,
    task_id: &str,
    attempt_id: &str,
    kind: FailureKind,
    message: &str,
    max_retries: u32,
) {
    let cfg = core.drove.config_snapshot();
    let effective_max_retries = core
        .drove
        .store
        .get_task(task_id)
        .map(|t| {
            let role_name = t
                .assign
                .as_ref()
                .and_then(|a| a.role.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("coder");
            let chain_len = super::config::resolve_role_config(&cfg, role_name)
                .map_or(1, |r| 1 + r.fallbacks.len());
            max_retries.max(chain_len.saturating_sub(1) as u32).min(8)
        })
        .unwrap_or(max_retries);

    let mut should_notify = false;
    let updated_task = core.drove.store.update_task(task_id, |t| {
        if matches!(t.status, TaskStatus::Cancelled | TaskStatus::Skipped | TaskStatus::Succeeded) {
            return;
        }
        t.failures += 1;
        if kind != FailureKind::Cancelled && t.failures <= effective_max_retries {
            t.status = TaskStatus::Ready;
        } else {
            t.status = TaskStatus::Failed;
            should_notify = true;
        }
    });
    if should_notify
        && let (Some(run), Some(task)) = (core.drove.store.get_run(run_id), updated_task)
    {
        let _ = core.drove.store.post_message(super::store::PostMessageInput {
            run_id: run.id.clone(),
            kind: MessageType::Failed,
            from: "drove".into(),
            to: "coordinator".into(),
            thread_id: None,
            task_id: Some(task.id.clone()),
            attempt_id: Some(attempt_id.into()),
            subject: format!("Task `{}` failed", task.key),
            body: Some(message.to_string()),
            options: None,
            recommend: None,
            why: None,
            raised_by: Some("scheduler".into()),
            raised_by_worker: None,
            tag: None,
            ai_draft: None,
            needs_action: true,
        });
        let line = format!("[drove] Task `{}` failed: {}", task.key, message);
        notify_coordinator(
            core,
            &run,
            "task_failed",
            &format!("Task `{}` failed", task.key),
            &line,
            Some(&task.id),
            Some(attempt_id),
            None,
            None,
        )
        .await;
    }
}

/// Simple token store helpers used across worker endpoints.
pub fn build_token_map() -> HashMap<String, String> {
    HashMap::new()
}
