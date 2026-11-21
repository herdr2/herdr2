//! Native `herdr2` headless ACP worker pool lifecycle (`a*` sessions, warm reuse, transcripts, kill sync).

use super::notify::notify_coordinator;
use super::protocol::{PriorAttemptHandover, build_task_prompt_with_handover};
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
    SessionClosed { address: String },
    WorkerTrashed { worker_id: String },
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
            let is_tool_done = matches!(
                &entry,
                Entry::ToolCall { status, .. } if status == "completed" || status == "failed"
            );
            let worker_snap = core.drove.store.get_worker(&worker_id);
            let attempt_id = worker_snap
                .as_ref()
                .and_then(|w| w.current_attempt_id.clone());
            record_transcript_item(core, &worker_id, attempt_id.clone(), entry);
            let _ = core
                .drove
                .store
                .update_worker(&worker_id, |w| w.last_active_at = now_iso());
            if is_tool_done
                && let (Some(w), Some(aid)) = (worker_snap, attempt_id)
            {
                maybe_detect_spinning_loop(core, &w, &aid).await;
            }
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
            if attempt.kind.as_deref() == Some("review") {
                return;
            }
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
            let recent_assistant = core
                .drove
                .store
                .list_items(&worker_id, Some(&attempt_id), None, 10)
                .into_iter()
                .filter(|it| it.kind == "assistant")
                .filter_map(|it| it.text)
                .collect::<Vec<_>>()
                .join("");
            if let Some(prov_err) = super::review::looks_like_provider_error(&recent_assistant) {
                finish_unsettled_attempt(
                    core,
                    &worker,
                    &attempt,
                    FailureKind::AgentError,
                    prov_err,
                    false,
                )
                .await;
                let _ = stop_worker(core, &worker_id).await;
                let _ = core.drove.store.update_worker(&worker_id, |w| {
                    w.state = WorkerState::Dead;
                });
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
            let coord_closed = core
                .drove
                .store
                .get_run(&worker.run_id)
                .is_some_and(|r| is_coordinator_closed(core, &r));
            let was_closed = worker.state == WorkerState::Closed || coord_closed;
            let open_att = worker
                .current_attempt_id
                .as_deref()
                .and_then(|aid| core.drove.store.get_attempt(aid))
                .filter(|a| a.outcome.is_none() && a.ended_at.is_none());
            let _ = core.drove.store.update_worker(&worker_id, |w| {
                w.state = if was_closed { WorkerState::Closed } else { WorkerState::Dead };
                w.current_attempt_id = None;
                w.worktree = None;
                w.pid = None;
                w.last_active_at = now_iso();
            });
            if let Some(wt) = worker.worktree.as_deref().map(str::trim).filter(|s| !s.is_empty())
                && let Some(run) = core.drove.store.get_run(&worker.run_id)
            {
                super::integrate::cleanup_worktree_dir(&run.cwd, std::path::Path::new(wt));
            }
            core.drove.remove_worker_token(&worker_id);
            if let Some(att) = open_att
                && att.kind.as_deref() != Some("review")
            {
                if coord_closed {
                    let ended = now_iso();
                    let _ = core.drove.store.update_attempt(&att.id, |a| {
                        a.outcome = Some(Outcome::Failed);
                        a.ended_at = Some(ended);
                        a.failure = Some(AttemptFailure {
                            kind: FailureKind::Cancelled,
                            message: format!("Coordinator closed ({reason})"),
                        });
                    });
                    let _ = core.drove.store.update_task(&att.task_id, |t| {
                        if t.status == TaskStatus::Running {
                            t.status = TaskStatus::Cancelled;
                        }
                    });
                } else {
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
            }
            if coord_closed {
                if let Some(run) = core.drove.store.get_run(&worker.run_id) {
                    close_run_for_coordinator(core, &run).await;
                }
            } else {
                core.drove.wake_scheduler();
            }
        }
        PoolSignal::SessionClosed { address } => {
            let runs = core.drove.store.list_runs();
            for run in runs {
                let sess = run.coordinator.host_session.trim();
                let matches_addr = if address.contains('/') {
                    sess == address
                } else {
                    local_session_part(core, sess) == Some(address.as_str())
                };
                let has_open_workers = core
                    .drove
                    .store
                    .list_workers(Some(&run.id))
                    .iter()
                    .any(|w| !matches!(w.state, WorkerState::Closed | WorkerState::Dead));
                if (matches_addr || is_coordinator_closed(core, &run))
                    && (run.status == RunStatus::Active || has_open_workers)
                {
                    close_run_for_coordinator(core, &run).await;
                }
            }
        }
        PoolSignal::WorkerTrashed { worker_id } => {
            let _ = stop_worker(core, &worker_id).await;
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
    fail_or_retry_task(core, &attempt.run_id, &attempt.task_id, &attempt.id, kind, &message, MAX_AUTO_RETRIES).await;
    if keep_worker_idle {
        core.drove.clear_loop_watermark(&attempt.id);
    core.ai.evict_session(&format!("drove:loop:{}", attempt.id));
    if let Some(wt) = worker.worktree.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let run_opt = core.drove.store.get_run(&attempt.run_id);
            let is_aosp = run_opt
                .as_ref()
                .is_some_and(|r| super::integrate::is_aosp_harness(&r.cwd));
            if is_aosp {
                if let Some(ref r) = run_opt {
                    super::integrate::cleanup_worktree_dir(&r.cwd, std::path::Path::new(wt));
                }
                let run_cwd = run_opt.map(|r| r.cwd).unwrap_or_else(|| worker.cwd.clone());
                let _ = core.drove.store.update_worker(&worker.id, |w| {
                    w.worktree = None;
                    w.cwd = run_cwd;
                });
            } else {
                let wt_str = wt.to_string();
                let _ = tokio::task::spawn_blocking(move || super::integrate::reset_worktree_clean(&wt_str)).await;
            }
        }
        mark_worker_idle(core, &worker.id);
    } else {
        if let Some(wt) = worker.worktree.as_deref().map(str::trim).filter(|s| !s.is_empty())
            && let Some(run) = core.drove.store.get_run(&attempt.run_id)
        {
            super::integrate::cleanup_worktree_dir(&run.cwd, std::path::Path::new(wt));
        }
        let _ = core.drove.store.update_worker(&worker.id, |w| {
            w.current_attempt_id = None;
            w.worktree = None;
            w.last_active_at = now_iso();
        });
    }
    core.drove.wake_scheduler();
}

/// Dispatches an agent task onto either an idle warm worker or a fresh `herdr2` headless ACP `a*` session.
pub async fn dispatch_agent_task(
    core: &Arc<Core>,
    run: &Run,
    task: &Task,
    decision: RouteDecision,
    mut effective_cwd: String,
    mut worktree: Option<String>,
    retry_of: Option<String>,
    hint: Option<String>,
) -> Result<Attempt, String> {
    let dep_tasks = resolved_dep_tasks(&core.drove.store.list_tasks(&run.id), task);
    let attempt_id = super::crypto::rand_id("att");

    let live_warm = if let Some(warm) = decision.warm_worker.clone() {
        if let Some(aid) = live_untrashed_agent_id(core, warm.h2_agent.as_deref()) {
            Some((warm, aid))
        } else {
            let _ = core.drove.store.update_worker(&warm.id, |w| {
                if !matches!(w.state, WorkerState::Closed | WorkerState::Dead) {
                    w.state = WorkerState::Dead;
                }
                w.current_attempt_id = None;
                w.worktree = None;
            });
            if let Some(wt) = warm.worktree.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                super::integrate::cleanup_worktree_dir(&run.cwd, std::path::Path::new(wt));
            }
            if worktree.is_some() || task.isolation == Isolation::Worktree {
                let (next_cwd, next_wt) = super::worktree_for_task(core, run, task, None).await?;
                effective_cwd = next_cwd;
                worktree = next_wt;
            }
            None
        }
    } else {
        None
    };

    let baseline = super::roles::capture_dirty_baseline(&effective_cwd);
    let (prior_handover, forked_from_agent) =
        build_attempt_handover(core, retry_of.as_deref(), &effective_cwd).await;

    if let Some((mut warm, aid)) = live_warm {
        // Switch model/context/effort/fast dynamically if needed.
        if decision.model.as_deref() != warm.model.as_deref()
            && let Some(m) = &decision.model
        {
            apply_warm_config(core, aid, "model", json!(m)).await;
        }
        let is_agy = matches!(decision.agent.as_str(), "agy" | "antigravity");
        let is_cursor = matches!(decision.agent.as_str(), "cursor" | "cursor-agent");
        let cursor_has_effort = is_cursor && decision.model.as_deref().is_some_and(acp::model_has_embedded_effort);
        if is_cursor && decision.context != warm.context_window
            && let Some(ctx) = decision.context.as_deref().map(str::trim).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"))
        {
            apply_warm_config(core, aid, "context", json!(ctx)).await;
        }
        if !is_agy && !cursor_has_effort && Some(decision.effort) != warm.effort {
            apply_warm_config(core, aid, "reasoning_effort", json!(decision.effort.as_str())).await;
        }
        if (decision.fast || warm.fast == Some(true)) && Some(decision.fast) != warm.fast {
            apply_warm_config(core, aid, "fast-mode", json!(decision.fast)).await;
        }

        let display = format!("{} · {}", decision.role, task.key);
        let mut patch = BTreeMap::new();
        patch.insert("drove.role".into(), Some(decision.role.clone()));
        patch.insert("drove.task".into(), Some(task.key.clone()));
        patch.insert("drove.display".into(), Some(display.clone()));
        update_agent_display(core, aid, display, patch);

        let now = now_iso();
        warm.state = WorkerState::Busy;
        warm.cwd = effective_cwd.clone();
        warm.worktree = worktree;
        warm.current_attempt_id = Some(attempt_id.clone());
        warm.last_active_at = now.clone();
        warm.model = decision.model.clone().or(warm.model);
        warm.context_window = decision.context.clone().or(warm.context_window);
        warm.effort = Some(decision.effort);
        warm.fast = (decision.fast || decision.agent == "codex").then_some(decision.fast);
        warm.totals.attempts += 1;
        if let Some(src_agent) = &forked_from_agent {
            warm.forked_from = Some(src_agent.clone());
        }
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
                fast: decision.fast.then_some(true),
                worker: Some(warm.id.clone()),
                reason: Some(decision.reason.clone()),
                source: preserved_source.clone(),
                role: Some(decision.role.clone()),
                fork: forked_from_agent.clone(),
            });
        });

        let prompt = build_task_prompt_with_handover(
            run,
            task,
            &attempt,
            &decision.role,
            &decision.role_config,
            &dep_tasks,
            hint.as_deref(),
            prior_handover.as_ref(),
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
    inject_drove_path_env(&mut extra_env);
    extra_env.insert("H2_DROVE_URL".into(), drove_url);
    extra_env.insert("H2_DROVE_WORKER_TOKEN".into(), worker_token.clone());
    extra_env.insert("H2_DROVE_WORKER_TICKET".into(), worker_token);
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
        context_window: decision.context.clone(),
        effort: Some(decision.effort),
        fast: (decision.fast || decision.agent == "codex").then_some(decision.fast),
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
        forked_from: forked_from_agent.clone(),
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
            fast: decision.fast.then_some(true),
            worker: Some(worker_id.clone()),
            reason: Some(decision.reason.clone()),
            source: preserved_source.clone(),
            role: Some(decision.role.clone()),
            fork: forked_from_agent.clone(),
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
            context: decision.context.clone(),
            effort: Some(decision.effort.as_str().to_string()),
            fast: Some(decision.fast),
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

    let prompt = build_task_prompt_with_handover(
        run,
        task,
        &attempt,
        &decision.role,
        &decision.role_config,
        &dep_tasks,
        hint.as_deref(),
        prior_handover.as_ref(),
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
        w.worktree = None;
        w.last_active_at = now_iso();
    });

    if let Some(wt) = worker.worktree.as_deref().map(str::trim).filter(|s| !s.is_empty())
        && let Some(run) = core.drove.store.get_run(&worker.run_id)
    {
        super::integrate::cleanup_worktree_dir(&run.cwd, std::path::Path::new(wt));
    }

    if let Some(addr) = worker.h2_agent.as_deref()
        && let Some(aid) = crate::agents::parse_id(addr)
    {
        core.kill(&[aid]).await;
    }
    core.drove.remove_worker_token(worker_id);

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

/// Stops all workers belonging to `run_id` and reaps any live `h2_agent` left on closed/dead workers.
pub async fn stop_run_workers(core: &Arc<Core>, run_id: &str) {
    let workers = core.drove.store.list_workers(Some(run_id));
    for w in workers {
        if w.state != WorkerState::Closed && w.state != WorkerState::Dead {
            let _ = stop_worker(core, &w.id).await;
        } else if let Some(addr) = w.h2_agent.as_deref()
            && let Some(aid) = crate::agents::parse_id(addr)
            && core.lock().reg.agents.get(&aid).is_some_and(|a| a.is_live())
        {
            core.kill(&[aid]).await;
        }
    }
}

/// Reaps stale workers and lingering `h2_agent` sessions left behind across a daemon restart/hot-update.
pub async fn reconcile_startup_workers(core: &Arc<Core>) {
    let workers = core.drove.store.list_workers(None);
    for w in workers {
        let has_tok = core.drove.has_worker_token(&w.id);
        let agent_live = w
            .h2_agent
            .as_deref()
            .and_then(crate::agents::parse_id)
            .is_some_and(|aid| core.lock().reg.agents.get(&aid).is_some_and(|a| a.is_live()));
        if !has_tok || !agent_live || matches!(w.state, WorkerState::Closed | WorkerState::Dead) {
            if agent_live
                && let Some(addr) = w.h2_agent.as_deref()
                && let Some(aid) = crate::agents::parse_id(addr)
            {
                core.kill(&[aid]).await;
            }
            let run = core.drove.store.get_run(&w.run_id);
            if let Some(wt) = w.worktree.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                if let Some(ref r) = run {
                    super::integrate::cleanup_worktree_dir(&r.cwd, std::path::Path::new(wt));
                } else {
                    let _ = std::fs::remove_dir_all(wt);
                }
            }
            if !matches!(w.state, WorkerState::Closed | WorkerState::Dead)
                || w.worktree.is_some()
                || w.current_attempt_id.is_some()
                || w.pid.is_some()
            {
                let _ = core.drove.store.update_worker(&w.id, |upd| {
                    if upd.state != WorkerState::Dead {
                        upd.state = WorkerState::Closed;
                    }
                    upd.current_attempt_id = None;
                    upd.worktree = None;
                    upd.pid = None;
                });
            }
            core.drove.remove_worker_token(&w.id);
        }
    }
    for att in core.drove.store.list_attempts(None, None, None) {
        if att.outcome.is_none() && att.ended_at.is_none() && !core.drove.has_worker_token(&att.worker_id) {
            let _ = core.drove.store.update_attempt(&att.id, |a| {
                a.outcome = Some(Outcome::Failed);
                a.ended_at = Some(now_iso());
                a.failure = Some(AttemptFailure {
                    kind: FailureKind::ProcessExited,
                    message: "Daemon restarted before attempt finished".into(),
                });
            });
        }
    }
}

pub(crate) fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        #[cfg(target_os = "linux")]
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            && let Some((_, rest)) = stat.rsplit_once(") ")
            && rest.trim_start().starts_with('Z')
        {
            return false;
        }
        let rc = unsafe { libc::kill(pid as i32, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let raw = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if raw.is_null() {
                return false;
            }
            let _process = OwnedHandle::from_raw_handle(raw);
            let mut started: FILETIME = std::mem::zeroed();
            let mut exited: FILETIME = std::mem::zeroed();
            let mut kernel: FILETIME = std::mem::zeroed();
            let mut user: FILETIME = std::mem::zeroed();
            if GetProcessTimes(raw, &mut started, &mut exited, &mut kernel, &mut user) == 0 {
                return false;
            }
            exited.dwLowDateTime == 0 && exited.dwHighDateTime == 0
        }
    }
}

pub(super) fn live_untrashed_agent_id(core: &Core, addr: Option<&str>) -> Option<u32> {
    let aid = addr.and_then(crate::agents::parse_id)?;
    let st = core.lock();
    st.reg
        .agents
        .get(&aid)
        .is_some_and(|a| a.is_live() && a.labels.get("herdr.trashed").is_none_or(String::is_empty))
        .then_some(aid)
}

pub(super) fn local_session_part<'a>(core: &Core, sess: &'a str) -> Option<&'a str> {
    match sess.split_once('/') {
        Some((prefix, rest)) => (prefix == core.info.id || prefix == core.info.name).then_some(rest),
        None => Some(sess),
    }
}

/// Returns `true` when the Run's Coordinator session (`a*`, `t*`, shell foreground, or `hostPid`) has closed or been trashed.
pub fn is_coordinator_closed(core: &Core, run: &Run) -> bool {
    let sess = run.coordinator.host_session.trim();
    if sess.is_empty() || sess == "coordinator" {
        return run.coordinator.host_pid.is_some_and(|pid| !pid_is_alive(pid));
    }
    let Some(local_part) = local_session_part(core, sess) else {
        return false;
    };
    let Some(tk) = TargetKey::parse(local_part) else {
        return run.coordinator.host_pid.is_some_and(|pid| !pid_is_alive(pid));
    };
    match tk {
        TargetKey::Agent(aid) => {
            let st = core.lock();
            match st.reg.agents.get(&aid) {
                Some(a) => !a.is_live() || a.labels.get("herdr.trashed").is_some_and(|s| !s.is_empty()),
                None => run.coordinator.session_bound == Some(true),
            }
        }
        TargetKey::Terminal(tid) => {
            let term_info = {
                let st = core.lock();
                st.pty_reg.terminals.get(&tid).map(|t| {
                    let live = t.is_live();
                    let trashed = t.labels.get("herdr.trashed").is_some_and(|s| !s.is_empty());
                    let at_shell_root = t.pid.is_some_and(|tpid| {
                        run.coordinator.host_pid.is_some_and(|hpid| hpid != tpid)
                            && t.foreground.as_ref().is_some_and(|fg| {
                                Some(fg.pid as u32) == t.pid && crate::foreground::is_shell_root(fg)
                            })
                    });
                    (live, trashed, at_shell_root)
                })
            };
            match term_info {
                Some((live, trashed, at_shell_root)) => {
                    if !live || trashed {
                        return true;
                    }
                    run.coordinator.agent.is_some()
                        && at_shell_root
                        && run.coordinator.host_pid.is_some_and(|hpid| !pid_is_alive(hpid))
                }
                None => run.coordinator.session_bound == Some(true),
            }
        }
    }
}

/// Stops all workers of `run` after its Coordinator exits or is trashed, and transitions an Active Run out of Active.
pub async fn close_run_for_coordinator(core: &Arc<Core>, run: &Run) {
    let workers = core.drove.store.list_workers(Some(&run.id));
    stop_run_workers(core, &run.id).await;
    for w in workers {
        core.drove.remove_worker_token(&w.id);
    }
    if let Some(cur) = core.drove.store.get_run(&run.id)
        && cur.status == RunStatus::Active
    {
        let tasks = core.drove.store.list_tasks(&run.id);
        let all_terminal = !tasks.is_empty()
            && tasks
                .iter()
                .filter(|t| t.template != Some(true))
                .all(|t| super::scheduler::is_terminal_status(t.status));
        let _ = core.drove.store.update_run(&run.id, |r| {
            if all_terminal {
                r.status = RunStatus::Done;
            } else {
                r.status = RunStatus::Paused;
                r.pause_reason = Some("coordinator_closed".into());
            }
        });
    }
    core.drove.wake_scheduler();
}

async fn maybe_detect_spinning_loop(core: &Arc<Core>, worker: &Worker, attempt_id: &str) {
    let cfg = core.drove.config_snapshot();
    if !cfg.ai.enabled || cfg.ai.loop_detect == "off" {
        return;
    }
    let Some(att) = core.drove.store.get_attempt(attempt_id) else {
        return;
    };
    if att.outcome.is_some() || att.ended_at.is_some() || att.spinning_detected_at.is_some() {
        return;
    }
    let items = core.drove.store.list_items(&worker.id, Some(attempt_id), None, 32);
    let recent_tools: Vec<(&str, &str)> = items
        .iter()
        .filter(|it| it.kind == "tool")
        .filter_map(|it| it.tool.as_ref())
        .map(|tl| (tl.kind.as_deref().unwrap_or(""), tl.title.as_deref().unwrap_or("").trim()))
        .filter(|(_, title)| !title.is_empty())
        .collect();

    // Stage 1 (0ms exact rule): 5 consecutive identical `(kind, title)` tool calls.
    if recent_tools.len() >= 5 {
        let tail = &recent_tools[recent_tools.len() - 5..];
        let first = tail[0];
        if tail.iter().all(|pair| *pair == first) {
            trigger_loop_detection(
                core,
                worker,
                &att,
                &cfg.ai.loop_detect,
                format!("Repeated tool `{}` 5 times", first.1),
                format!(
                    "[drove loop-detect] You repeated identical tool call `{}` 5 times. \
                     Stop repeating it, verify current workspace state, and take a different concrete step \
                     or call `h2 drove worker ask` / `h2 drove worker done --outcome failed`.",
                    first.1
                ),
            )
            .await;
            return;
        }
    }

    // Stage 2 (incremental multi-turn semantic oscillation audit via AiSessionPool):
    // Trigger only when >= 8 tools ran, >= 4 new tools arrived since watermark, and oscillation signals appear.
    if recent_tools.len() < 8 || !core.ai.is_configured() || core.ai.is_circuit_open() {
        return;
    }
    let prev_wm = core.drove.get_loop_watermark(attempt_id);
    let max_id = items.iter().map(|it| it.id).max().unwrap_or(0);
    if max_id <= prev_wm {
        return;
    }
    let delta_items: Vec<Item> = items.iter().filter(|it| it.id > prev_wm).cloned().collect();
    let delta_tool_count = delta_items.iter().filter(|it| it.kind == "tool").count();
    if prev_wm > 0 && delta_tool_count < 4 {
        return;
    }

    let tail10 = &recent_tools[recent_tools.len().saturating_sub(10)..];
    let unique_tools: std::collections::HashSet<(&str, &str)> = tail10.iter().copied().collect();
    let oscillation_signal = unique_tools.len() <= 3
        || has_repeated_edit_failure_pattern(&items)
        || (recent_tools.len() >= 12 && recent_tools.len() % 12 < 4);
    if !oscillation_signal {
        return;
    }

    let Some(task) = core.drove.store.get_task(&att.task_id) else {
        return;
    };
    core.drove.set_loop_watermark(attempt_id, max_id);

    let Some(verdict) =
        super::ai::detect_semantic_loop(core, &att.run_id, attempt_id, &task, &delta_items).await
    else {
        return;
    };
    if !verdict.looping || verdict.confidence < 0.80 {
        return;
    }

    let detail = if verdict.root_cause.is_empty() {
        format!("{} (conf {:.2})", verdict.pattern, verdict.confidence)
    } else {
        format!("{}: {} (conf {:.2})", verdict.pattern, verdict.root_cause, verdict.confidence)
    };
    let steer_msg = if verdict.steer_hint.is_empty() {
        format!(
            "[drove loop-detect] Detected semantic oscillation (`{}`). \
             Stop repeating this cycle, re-check the root cause, or call `h2 drove worker ask`.",
            verdict.pattern
        )
    } else {
        format!("[drove loop-detect] {} — {}", verdict.pattern, verdict.steer_hint)
    };
    trigger_loop_detection(core, worker, &att, &cfg.ai.loop_detect, detail, steer_msg).await;
}

fn has_repeated_edit_failure_pattern(items: &[Item]) -> bool {
    let mut edit_paths: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut failed_cmds = 0usize;
    for it in items.iter().rev().take(16) {
        if let Some(tl) = &it.tool {
            let k = tl.kind.as_deref().unwrap_or("").to_ascii_lowercase();
            let st = tl.status.as_deref().unwrap_or("").to_ascii_lowercase();
            if st == "failed" || st == "error" {
                failed_cmds += 1;
            }
            if matches!(k.as_str(), "edit" | "write" | "modify") {
                if let Some(locs) = &tl.locations {
                    for loc in locs {
                        *edit_paths.entry(loc.as_str()).or_default() += 1;
                    }
                } else if let Some(title) = tl.title.as_deref() {
                    *edit_paths.entry(title).or_default() += 1;
                }
            }
        }
    }
    edit_paths.values().any(|&n| n >= 3) && failed_cmds >= 2
}

async fn trigger_loop_detection(
    core: &Arc<Core>,
    worker: &Worker,
    att: &Attempt,
    loop_detect_policy: &str,
    detail: String,
    steer_msg: String,
) {
    let ts = now_iso();
    let _ = core.drove.store.update_attempt(&att.id, |a| {
        a.spinning_detected_at = Some(ts.clone());
    });
    core.drove.store.append_daemon_event(NewDaemonEvent {
        run_id: Some(att.run_id.clone()),
        task_id: Some(att.task_id.clone()),
        attempt_id: Some(att.id.clone()),
        worker_id: Some(worker.id.clone()),
        message_id: None,
        category: "ai".into(),
        kind: "loop_detect".into(),
        status: "warn".into(),
        title: format!("Potential tool loop on `{}`", worker.id),
        detail: Some(detail),
        duration_ms: None,
        tokens: None,
        cost: None,
    });
    // Per user-confirmed decision: `steer_once` delivers queued guidance (`steer = false`).
    if loop_detect_policy == "steer_once"
        && let Some(addr) = worker.h2_agent.as_deref()
        && let Some(aid) = crate::agents::parse_id(addr)
        && core.lock().reg.agents.get(&aid).is_some_and(|a| a.is_live())
    {
        let _ = core.send(aid, &steer_msg, false, None, Some("drove")).await;
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

pub(super) async fn apply_warm_config(core: &Arc<Core>, aid: u32, cfg_id: &str, val: Value) {
    let (sid, effective_id, wire_val_initial, src, tr, opts_snapshot) = {
        let st = core.lock();
        let Some(a) = st.reg.agents.get(&aid) else { return };
        let Some(sid) = a.acp_session_id.clone() else { return };
        let opts = &a.config_options;
        let Some((eff_id, resolved_val, resolved_src)) = acp::resolve_config_override(opts, cfg_id, &val) else {
            return;
        };
        let Some(tr) = st.sessions.get(&aid).map(|h| h.transport.clone()) else { return };
        (sid, eff_id, resolved_val, resolved_src, tr, opts.clone())
    };
    let (wire_val, derived) = if effective_id == "model"
        && let Some(raw) = wire_val_initial.as_str()
    {
        let split = acp::split_model_variant_for_options(raw, &opts_snapshot);
        let mut items: Vec<(&'static str, Value)> = Vec::new();
        if let Some(eff) = split.effort {
            items.push(("reasoning_effort", json!(eff)));
        }
        if let Some(fast) = split.fast {
            items.push(("fast-mode", json!(fast)));
        }
        if let Some(ctx) = split.context {
            items.push(("context", json!(ctx)));
        }
        (Value::String(split.model), items)
    } else {
        (wire_val_initial, Vec::new())
    };
    if let Ok(mut maybe_opts) = acp::apply_config_option(&tr, &sid, &effective_id, &wire_val, src.as_deref()).await {
        let mut applied_extra: Vec<(String, Value)> = Vec::new();
        for (cid, cval) in derived {
            let resolved = {
                let base_opts = maybe_opts.as_deref().unwrap_or(&opts_snapshot);
                acp::resolve_config_override(base_opts, cid, &cval)
            };
            let Some((sub_id, sub_val, sub_src)) = resolved else { continue };
            if let Ok(sub_opts) = acp::apply_config_option(&tr, &sid, &sub_id, &sub_val, sub_src.as_deref()).await {
                if let Some(opts) = sub_opts {
                    maybe_opts = Some(opts);
                }
                applied_extra.push((sub_id, sub_val));
            }
        }
        let mut st = core.lock();
        if let Some(a) = st.reg.agents.get_mut(&aid) {
            a.config_overrides.insert(effective_id.clone(), wire_val.clone());
            for (sub_id, sub_val) in &applied_extra {
                a.config_overrides.insert(sub_id.clone(), sub_val.clone());
            }
            if let Some(opts) = maybe_opts {
                a.config_options = opts;
            } else {
                acp::patch_config_value(&mut a.config_options, &effective_id, &wire_val);
                for (sub_id, sub_val) in &applied_extra {
                    acp::patch_config_value(&mut a.config_options, sub_id, sub_val);
                }
            }
            acp::sync_config_to_agent(a);
            let data = acp::config_event_data(a);
            core.emit(&mut st, "config", aid, data);
            core.save(&st);
        }
    }
}

async fn build_attempt_handover(
    core: &Arc<Core>,
    retry_of: Option<&str>,
    effective_cwd: &str,
) -> (Option<PriorAttemptHandover>, Option<String>) {
    let Some(prev_id) = retry_of.map(str::trim).filter(|s| !s.is_empty()) else {
        return (None, None);
    };
    let Some(prev) = core.drove.store.get_attempt(prev_id) else {
        return (None, None);
    };
    let prev_worker = core.drove.store.get_worker(&prev.worker_id);
    let forked_from = prev_worker.as_ref().and_then(|w| w.h2_agent.clone());

    let context_markdown = forked_from.as_deref().and_then(|addr| {
        let aid = crate::agents::parse_id(addr)?;
        let mut entries = {
            let st = core.lock();
            st.transcripts.get(&aid).map(|t| t.entries().to_vec())
        }?;
        entries.retain(|e| !matches!(e, Entry::User { .. }));
        if entries.is_empty() {
            return None;
        }
        let max_turn = entries.iter().filter_map(Entry::turn).max().unwrap_or(0);
        let from_turn = (max_turn > 4).then_some(max_turn - 3);
        let pkg = crate::handoff::build_handoff_package(&crate::handoff::HandoffBuildInput {
            kind: crate::handoff::LineageKind::Handoff,
            source_address: addr,
            source_provider: Some(&prev.agent),
            source_session_id: None,
            source_model: prev.model.as_deref(),
            cwd: None,
            entries: &entries,
            from_turn,
            to_turn: None,
            max_bytes: 12 * 1024,
            note: None,
            is_zh: false,
            worktree_origin: None,
            omitted_summary: None,
        });
        (!pkg.markdown.trim().is_empty()).then_some(pkg.markdown)
    });

    let git_status = tokio::process::Command::new("git")
        .args(["-C", effective_cwd, "status", "--short", "--untracked-files=normal"])
        .output()
        .await
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .filter(|s| !s.trim().is_empty());

    let failure_reason = prev
        .failure
        .as_ref()
        .map(|f| format!("{:?}: {}", f.kind, f.message))
        .or_else(|| {
            prev.result
                .as_ref()
                .and_then(|r| r.blockers.as_ref())
                .filter(|b| !b.is_empty())
                .map(|b| b.join("; "))
        });

    (
        Some(PriorAttemptHandover {
            attempt_id: prev.id,
            agent: prev.agent,
            model: prev.model,
            failure_reason,
            context_markdown,
            git_status,
        }),
        forked_from,
    )
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
        fast: decision.fast.then_some(true),
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
    let mut out = Vec::new();
    let mut push_unique = |cand: &Task| {
        if !out.iter().any(|existing: &Task| existing.id == cand.id) {
            out.push(cand.clone());
        }
    };
    for dep in &task.deps {
        if let Some(target) = all.iter().find(|c| &c.id == dep || &c.key == dep) {
            if target.template == Some(true) {
                if task.expanded_from.as_deref() == Some(target.id.as_str()) {
                    for tpl_dep in &target.deps {
                        if let Some(up) = all.iter().find(|c| &c.id == tpl_dep || &c.key == tpl_dep) {
                            push_unique(up);
                        }
                    }
                } else {
                    for child in all.iter().filter(|c| c.expanded_from.as_deref() == Some(target.id.as_str())) {
                        push_unique(child);
                    }
                }
            } else {
                push_unique(target);
            }
        }
    }
    out
}

pub(super) fn resolve_local_parent(core: &Core, host_session: &str) -> Option<String> {
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

/// Ensures Drove worker/reviewer child processes inherit a complete `PATH` (including `~/.nvm` Node
/// shims, `~/.local/bin`, and `~/.cargo/bin`) even when `herdr2 hold` was spawned by a desktop GUI.
pub(super) fn inject_drove_path_env(env: &mut BTreeMap<String, String>) {
    let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH")
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        let mut extras = vec![home.join(".local/bin"), home.join(".cargo/bin")];
        if let Ok(rd) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            let mut vers: Vec<std::path::PathBuf> =
                rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
            vers.sort();
            if let Some(latest) = vers.pop() {
                extras.insert(0, latest.join("bin"));
            }
        }
        for ex in extras.into_iter().rev() {
            if ex.is_dir() && !dirs.iter().any(|d| d == &ex) {
                dirs.insert(0, ex);
            }
        }
    }
    if let Ok(joined) = std::env::join_paths(&dirs)
        && let Some(s) = joined.to_str()
        && !s.is_empty()
    {
        env.insert("PATH".into(), s.to_string());
    }
}

/// Simple token store helpers used across worker endpoints.
pub fn build_token_map() -> HashMap<String, String> {
    HashMap::new()
}
