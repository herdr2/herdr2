//! Native `herdr2` headless ACP worker pool lifecycle (`a*` sessions, warm reuse, transcripts, kill sync).

use super::notify::notify_coordinator;
use super::protocol::{PriorAttemptHandover, build_task_prompt_with_context};
use super::router::RouteDecision;
use super::store::NewDaemonEvent;
use super::types::*;
use super::{MAX_AUTO_RETRIES, fail_or_retry_task};
use crate::acp::{
    self,
    proto::{AcpContextUsage, AcpUsageDetail},
};
use crate::api::run::{DroveSpawnSpec, spawn_drove_agent, update_agent_display};
use crate::runtime::{Core, TargetKey};
use crate::transcript::Entry;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

const COALESCE_WINDOW: Duration = Duration::from_millis(200);
const COALESCE_MAX_BYTES: usize = 4096;
const MIN_LOOP_DETECT_ELAPSED_SEC: u64 = 90;
const MIN_SEMANTIC_LOOP_ELAPSED_SEC: u64 = 120;
const SEMANTIC_LOOP_COOLDOWN: Duration = Duration::from_secs(90);
const SEMANTIC_LOOP_MAX_CHECKS: u32 = 3;

#[derive(Clone, Debug)]
pub enum PoolSignal {
    Transcript {
        worker_id: String,
        entry: Entry,
    },
    UsageUpdated {
        worker_id: String,
        turn_num: Option<u64>,
        usage: Option<AcpContextUsage>,
        detail: Option<AcpUsageDetail>,
    },
    TurnFinished {
        worker_id: String,
        agent_id: u32,
        turn_num: u64,
        error: Option<String>,
        usage: Option<AcpContextUsage>,
        detail: Option<AcpUsageDetail>,
    },
    AgentExited {
        worker_id: String,
        agent_id: u32,
        reason: String,
    },
    SessionClosed {
        address: String,
    },
    WorkerTrashed {
        worker_id: String,
    },
}

struct PendingTextChunk {
    attempt_id: Option<String>,
    kind: &'static str,
    text: String,
    first_at: Instant,
}

fn flush_worker_chunk(core: &Core, pending: &mut HashMap<String, PendingTextChunk>, worker_id: &str) {
    if let Some(chunk) = pending.remove(worker_id)
        && !chunk.text.is_empty()
    {
        core.drove.store.append_item(worker_id, chunk.attempt_id, chunk.kind, Some(chunk.text), None, None);
    }
}

fn flush_expired_chunks(core: &Core, pending: &mut HashMap<String, PendingTextChunk>) {
    let now = Instant::now();
    let expired: Vec<String> = pending
        .iter()
        .filter(|(_, chunk)| now.saturating_duration_since(chunk.first_at) >= COALESCE_WINDOW)
        .map(|(wid, _)| wid.clone())
        .collect();
    for wid in expired {
        flush_worker_chunk(core, pending, &wid);
    }
}

/// Runs in background processing transcript, turn-completion, and external kill signals from `Core`.
pub async fn run_signal_loop(core: Arc<Core>, mut rx: mpsc::UnboundedReceiver<PoolSignal>) {
    let mut pending: HashMap<String, PendingTextChunk> = HashMap::new();
    loop {
        let next_deadline = pending.values().map(|c| c.first_at + COALESCE_WINDOW).min();
        match next_deadline {
            Some(deadline) => {
                tokio::select! {
                    maybe_sig = rx.recv() => match maybe_sig {
                        Some(sig) => handle_signal(&core, &mut pending, sig).await,
                        None => break,
                    },
                    _ = tokio::time::sleep_until(deadline) => {
                        flush_expired_chunks(&core, &mut pending);
                    }
                }
            }
            None => match rx.recv().await {
                Some(sig) => handle_signal(&core, &mut pending, sig).await,
                None => break,
            },
        }
    }
    let remaining: Vec<String> = pending.keys().cloned().collect();
    for wid in remaining {
        flush_worker_chunk(&core, &mut pending, &wid);
    }
}

fn resolve_signal_attempt_id(
    drove: &super::DroveRuntime,
    worker_id: &str,
    worker: Option<&Worker>,
    turn_num: Option<u64>,
) -> Option<String> {
    let cur = worker.and_then(|w| w.current_attempt_id.clone());
    if let (Some(cur_id), Some(tn)) = (cur.as_deref(), turn_num) {
        let min_turn = drove.get_attempt_min_turn(cur_id);
        if min_turn > 0 && tn < min_turn {
            return drove.get_worker_prev_attempt(worker_id);
        }
    }
    cur.or_else(|| drove.get_worker_last_attempt(worker_id))
}

async fn handle_signal(core: &Arc<Core>, pending: &mut HashMap<String, PendingTextChunk>, sig: PoolSignal) {
    match sig {
        PoolSignal::Transcript { worker_id, entry } => {
            let worker_snap = core.drove.store.get_worker(&worker_id);
            let attempt_id = resolve_signal_attempt_id(&core.drove, &worker_id, worker_snap.as_ref(), entry.turn());
            core.drove.store.touch_worker_active(&worker_id);

            match entry {
                Entry::Assistant { text, .. } if !text.is_empty() => {
                    if let Some(ref aid) = attempt_id {
                        let est_delta = (text.len() as u64).div_ceil(4);
                        core.drove.add_attempt_estimated_output(aid, est_delta);
                    }
                    enqueue_text_chunk(core, pending, &worker_id, attempt_id, "assistant", text);
                }
                Entry::Thought { text, .. } if !text.is_empty() => {
                    if let Some(ref aid) = attempt_id {
                        let est_delta = (text.len() as u64).div_ceil(4);
                        core.drove.add_attempt_estimated_output(aid, est_delta);
                    }
                    enqueue_text_chunk(core, pending, &worker_id, attempt_id, "thought", text);
                }
                other => {
                    flush_worker_chunk(core, pending, &worker_id);
                    let is_tool_done = matches!(
                        &other,
                        Entry::ToolCall { status, .. } if status == "completed" || status == "failed"
                    );
                    record_transcript_item(core, &worker_id, attempt_id.clone(), other);
                    if is_tool_done
                        && let (Some(w), Some(aid)) = (worker_snap, attempt_id)
                    {
                        maybe_detect_spinning_loop(core, &w, &aid).await;
                    }
                }
            }
        }
        PoolSignal::UsageUpdated { worker_id, turn_num, usage, detail } => {
            apply_worker_usage(core, &worker_id, turn_num, usage, detail);
        }
        PoolSignal::TurnFinished { worker_id, agent_id, turn_num, error, usage, detail } => {
            flush_worker_chunk(core, pending, &worker_id);
            apply_worker_usage(core, &worker_id, Some(turn_num), usage, detail);
            let Some(worker) = core.drove.store.get_worker(&worker_id) else {
                return;
            };
            let Some(attempt_id) = worker.current_attempt_id.clone() else {
                return;
            };
            let min_turn = core.drove.get_attempt_min_turn(&attempt_id);
            if turn_num < min_turn {
                return;
            }
            let Some(attempt) = core.drove.store.get_attempt(&attempt_id) else {
                return;
            };
            if attempt.kind.as_deref() == Some("review") {
                return;
            }
            if attempt.outcome.is_some() || attempt.ended_at.is_some() {
                return;
            }
            // Don't prematurely remind or fail if a blocking `drove worker ask` or synchronous
            // `drove worker done` gate (`verify` / `review` / `merge`) is currently in flight.
            if core.drove.has_active_ask(&attempt_id) || core.drove.has_active_done(&attempt_id) {
                return;
            }
            if core.drove.store.get_task(&attempt.task_id).is_some_and(|t| {
                matches!(t.exec_phase, Some(TaskPhase::Verifying | TaskPhase::Review | TaskPhase::Integrating))
            }) {
                return;
            }
            if let Some(err_msg) = error {
                if try_in_place_model_failover(core, &worker, &attempt, &err_msg).await {
                    return;
                }
                finish_unsettled_attempt(core, &worker, &attempt, FailureKind::AgentError, err_msg, true).await;
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
                if try_in_place_model_failover(core, &worker, &attempt, &prov_err).await {
                    return;
                }
                finish_unsettled_attempt(core, &worker, &attempt, FailureKind::AgentError, prov_err, false).await;
                let _ = stop_worker(core, &worker_id).await;
                let _ = core.drove.store.update_worker(&worker_id, |w| {
                    w.state = WorkerState::Dead;
                });
                return;
            }
            if !core.drove.mark_attempt_reminded(&attempt_id) {
                let msg = format!(
                    "You ended your turn without calling `drove_finish` (or `h2 drove worker done`). \
                     If you finished task `{}`, call the `drove_finish` MCP tool with `outcome: \"done\"` and `summary` \
                     (or `h2 drove worker done --outcome done --summary \"...\"`).",
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
            flush_worker_chunk(core, pending, &worker_id);
            let Some(worker) = core.drove.store.get_worker(&worker_id) else {
                return;
            };
            let coord_closed =
                core.drove.store.get_run(&worker.run_id).is_some_and(|r| is_coordinator_closed(core, &r));
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
                    let kind = if was_closed { FailureKind::Cancelled } else { FailureKind::ProcessExited };
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
            flush_worker_chunk(core, pending, &worker_id);
            let _ = stop_worker(core, &worker_id).await;
            core.drove.wake_scheduler();
        }
    }
}

fn enqueue_text_chunk(
    core: &Core,
    pending: &mut HashMap<String, PendingTextChunk>,
    worker_id: &str,
    attempt_id: Option<String>,
    kind: &'static str,
    text: String,
) {
    let should_flush_prev =
        pending.get(worker_id).is_some_and(|cur| cur.kind != kind || cur.attempt_id != attempt_id);
    if should_flush_prev {
        flush_worker_chunk(core, pending, worker_id);
    }
    let flush_now = if let Some(cur) = pending.get_mut(worker_id) {
        cur.text.push_str(&text);
        cur.text.len() >= COALESCE_MAX_BYTES || cur.first_at.elapsed() >= COALESCE_WINDOW
    } else {
        let len = text.len();
        pending.insert(
            worker_id.to_string(),
            PendingTextChunk { attempt_id, kind, text, first_at: Instant::now() },
        );
        len >= COALESCE_MAX_BYTES
    };
    if flush_now {
        flush_worker_chunk(core, pending, worker_id);
    }
}

fn apply_worker_usage(
    core: &Core,
    worker_id: &str,
    turn_num: Option<u64>,
    usage: Option<AcpContextUsage>,
    detail: Option<AcpUsageDetail>,
) {
    let ctx = usage.or_else(|| detail.and_then(|d| d.context));
    let Some(worker) = core.drove.store.get_worker(worker_id) else {
        return;
    };

    let raw_input = detail.map_or(0, |d| d.input);
    let raw_output = detail.map_or(0, |d| d.output);
    let raw_cache = detail.map_or(0, |d| d.cache_read);
    let has_raw_explicit = raw_input > 0 || raw_output > 0 || raw_cache > 0;
    let (explicit_input, explicit_output, explicit_cache) = if has_raw_explicit {
        core.drove.advance_worker_explicit_usage_watermark(worker_id, raw_input, raw_output, raw_cache)
    } else {
        (0, 0, 0)
    };
    let has_explicit_tokens = explicit_input > 0 || explicit_output > 0 || explicit_cache > 0;
    let delta_cost = detail.and_then(|d| d.cost).filter(|c| c.is_finite() && *c > 0.0);

    let ctx_delta = ctx
        .map(|u| core.drove.advance_worker_usage_watermark(worker_id, u.used))
        .unwrap_or(0);

    let target_attempt_id = resolve_signal_attempt_id(&core.drove, worker_id, Some(&worker), turn_num);
    let est_out_total = target_attempt_id
        .as_deref()
        .map(|aid| core.drove.add_attempt_estimated_output(aid, 0))
        .unwrap_or(0);

    let mut attempt_changed = false;
    if let Some(att_id) = target_attempt_id
        && (has_explicit_tokens || ctx_delta > 0 || delta_cost.is_some() || est_out_total > 0)
    {
        if core
            .drove
            .store
            .update_attempt(&att_id, |a| {
                let was_estimated = a.estimated.as_ref().and_then(|e| e.tokens) == Some(true);
                if has_explicit_tokens {
                    if was_estimated {
                        // Replace rough estimated counters cleanly when exact ACP usage arrives.
                        a.tokens.input = explicit_input;
                        a.tokens.output = explicit_output;
                        a.tokens.cache_read = explicit_cache;
                    } else {
                        a.tokens.input = a.tokens.input.saturating_add(explicit_input);
                        a.tokens.output = a.tokens.output.saturating_add(explicit_output);
                        a.tokens.cache_read = a.tokens.cache_read.saturating_add(explicit_cache);
                    }
                    if let Some(est) = a.estimated.as_mut() {
                        est.tokens = None;
                        if est.cost.is_none() {
                            a.estimated = None;
                        }
                    }
                } else if ctx_delta > 0 || (est_out_total > a.tokens.output && (was_estimated || a.tokens.output == 0)) {
                    let had_exact =
                        (a.tokens.input > 0 || a.tokens.output > 0 || a.tokens.cache_read > 0) && !was_estimated;
                    if !had_exact {
                        if ctx_delta > 0 {
                            a.tokens.input = a.tokens.input.saturating_add(ctx_delta);
                        }
                        if est_out_total > a.tokens.output {
                            a.tokens.output = est_out_total;
                        }
                        let est = a.estimated.get_or_insert_with(UsageEstimate::default);
                        est.tokens = Some(true);
                    }
                }
                if let Some(c) = delta_cost {
                    a.cost = Some(a.cost.unwrap_or(0.0) + c);
                }
            })
            .is_some()
        {
            attempt_changed = true;
        }
    }

    let ctx_changed = ctx.is_some_and(|u| u.used != worker.context.used || u.size != worker.context.size);
    if !ctx_changed && !attempt_changed {
        return;
    }

    let (total_tokens, total_cost, any_est_tokens, any_est_cost) = if attempt_changed {
        let attempts = core.drove.store.list_attempts(None, None, Some(worker_id));
        let mut toks = 0u64;
        let mut cost = 0.0f64;
        let mut est_t = false;
        let mut est_c = false;
        for a in &attempts {
            toks = toks.saturating_add(a.tokens.input + a.tokens.output + a.tokens.cache_read);
            cost += a.cost.unwrap_or(0.0);
            if a.estimated.as_ref().and_then(|e| e.tokens) == Some(true) {
                est_t = true;
            }
            if a.estimated.as_ref().and_then(|e| e.cost) == Some(true) {
                est_c = true;
            }
        }
        (Some(toks), Some(cost), est_t, est_c)
    } else {
        (None, None, false, false)
    };

    let _ = core.drove.store.update_worker(worker_id, |w| {
        if let Some(u) = ctx {
            w.context = WorkerContext { used: u.used, size: u.size };
        }
        if let Some(toks) = total_tokens {
            w.totals.tokens = toks;
        }
        if let Some(c) = total_cost {
            w.totals.cost = c;
        }
        if attempt_changed {
            w.totals.estimated = if any_est_tokens || any_est_cost {
                Some(UsageEstimate {
                    tokens: any_est_tokens.then_some(true),
                    cost: any_est_cost.then_some(true),
                })
            } else {
                None
            };
        }
    });
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
            let is_aosp = run_opt.as_ref().is_some_and(|r| super::integrate::is_aosp_harness(&r.cwd));
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
    let (prior_handover, forked_from_agent) = build_attempt_handover(core, retry_of.as_deref(), &effective_cwd).await;

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
        if is_cursor
            && decision.context != warm.context_window
            && let Some(ctx) =
                decision.context.as_deref().map(str::trim).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"))
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

        // Record the minimum ACP turn number BEFORE binding `current_attempt_id` on the worker so
        // any trailing `Transcript` / `UsageUpdated` / `TurnFinished` frames from the prior attempt's
        // turn are never misattributed to this new attempt.
        let min_turn = {
            let st = core.lock();
            let base = st.reg.agents.get(&aid).map_or(0, |a| {
                let in_flight = u64::from(a.status.as_ref().is_some_and(|s| s.state == "working"));
                a.turn + in_flight
            });
            let queued = st.inboxes.get(&aid).map_or(0, |q| q.len() as u64);
            base + queued + 1
        };
        core.drove.set_attempt_min_turn(&attempt_id, min_turn);
        core.drove.set_worker_last_attempt(&warm.id, &attempt_id);

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
        let effective_fork = forked_from_agent
            .clone()
            .filter(|src| warm.h2_agent.as_deref() != Some(src.as_str()) && src != &warm.id);
        if let Some(src_agent) = &effective_fork {
            warm.forked_from = Some(src_agent.clone());
        }
        update_fingerprint(&mut warm.fingerprint, task);
        core.drove.store.update_worker(&warm.id, |w| *w = warm.clone());

        let attempt = build_attempt_record(&attempt_id, run, task, &warm.id, &decision, true, retry_of, hint.clone());
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
                fork: effective_fork.clone(),
            });
        });

        let contracts = load_task_contracts(core, &run.id, task);
        let prompt = build_task_prompt_with_context(
            run,
            task,
            &attempt,
            &decision.role,
            &decision.role_config,
            &dep_tasks,
            &contracts,
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

    // Retire any idle non-reviewer workers of the same role/agent whose context window exceeded
    // the warm-reuse ceiling so bloated idle ACP processes don't accumulate across long runs.
    let bloated_idle: Vec<String> = core
        .drove
        .store
        .list_workers(Some(&run.id))
        .into_iter()
        .filter(|w| {
            w.state == WorkerState::Idle
                && w.read_only_dir.is_none()
                && w.agent == decision.agent
                && w.role.as_deref().unwrap_or("coder") == decision.role
                && super::router::is_worker_context_bloated(w)
        })
        .map(|w| w.id)
        .collect();
    for bloated_id in bloated_idle {
        let _ = stop_worker(core, &bloated_id).await;
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

    let fork_ctx = if prior_handover.is_none() {
        resolve_task_fork_context(core, run, task, &decision.agent, &effective_cwd).await
    } else {
        None
    };
    let effective_forked_from =
        fork_ctx.as_ref().map(|f| f.source_address.clone()).or_else(|| forked_from_agent.clone());

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
        forked_from: effective_forked_from.clone(),
        protocol_gen: Some(1),
        read_only_dir: None,
    };
    core.drove.store.insert_worker(worker);

    let attempt = build_attempt_record(&attempt_id, run, task, &worker_id, &decision, false, retry_of, hint.clone());
    core.drove.store.insert_attempt(attempt.clone());
    core.drove.set_worker_last_attempt(&worker_id, &attempt_id);
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
            fork: effective_forked_from.clone(),
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
            allowed_paths: decision.role_config.allowed_paths.clone(),
            isolate_secrets: true,
            resume_session_id: fork_ctx.as_ref().and_then(|f| f.resume_session_id.clone()),
            fallback_handoff_context: None,
            lineage: fork_ctx.as_ref().map(|f| f.lineage.clone()),
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

    if let Some(ref fctx) = fork_ctx {
        if !spawned.resumed_session && !fctx.wrapped_envelope.trim().is_empty() {
            let mut st = core.lock();
            let mut tc = core.target_checkpoints(&mut st, &spawned.address);
            tc.pending_handoff = Some(fctx.wrapped_envelope.clone());
            core.save_target_checkpoints(&mut st, tc);
            core.record_transcript(
                &mut st,
                spawned.agent_id,
                Entry::Handoff {
                    turn: 0,
                    handoff_kind: "fork".into(),
                    from_address: fctx.source_address.clone(),
                    from_provider: fctx.source_provider.clone(),
                    from_session_id: fctx.source_session_id.clone(),
                    from_turns: fctx.turns_label.clone(),
                    artifact_path: None,
                    context_markdown: (!fctx.markdown.trim().is_empty()).then(|| fctx.markdown.clone()),
                    ts_ms: crate::runtime::now_ms(),
                },
            );
            core.save(&st);
        }
    }

    let contracts = load_task_contracts(core, &run.id, task);
    let prompt = build_task_prompt_with_context(
        run,
        task,
        &attempt,
        &decision.role,
        &decision.role_config,
        &dep_tasks,
        &contracts,
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
    core.drove.clear_worker_runtime_state(worker_id);

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
        let worker_live = core.drove.store.get_worker(&att.worker_id).is_some_and(|w| {
            matches!(w.state, WorkerState::Busy | WorkerState::Starting) && core.drove.has_worker_token(&w.id)
        });
        if att.outcome.is_none() && att.ended_at.is_none() && !worker_live {
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

    // Recover orphan `TaskStatus::Running` tasks whose in-flight attempts ended across a daemon restart
    // so they never remain permanently stuck in `Running` (neither scheduled nor retryable).
    let mut recovered_any = false;
    for run in core.drove.store.list_runs() {
        for task in core.drove.store.list_tasks(&run.id) {
            if task.status != TaskStatus::Running || task.template == Some(true) {
                continue;
            }
            let has_open_att = core.drove.store.list_attempts(None, Some(&task.id), None).iter().any(|a| {
                a.outcome.is_none()
                    && a.ended_at.is_none()
                    && core.drove.store.get_worker(&a.worker_id).is_some_and(|w| {
                        matches!(w.state, WorkerState::Busy | WorkerState::Starting)
                            && core.drove.has_worker_token(&w.id)
                    })
            });
            if !has_open_att {
                recovered_any = true;
                let _ = core.drove.store.update_task(&task.id, |t| {
                    t.status = TaskStatus::Ready;
                    t.exec_phase = None;
                });
                core.drove.store.append_daemon_event(NewDaemonEvent {
                    run_id: Some(run.id.clone()),
                    task_id: Some(task.id.clone()),
                    attempt_id: None,
                    worker_id: None,
                    message_id: None,
                    category: "schedule".into(),
                    kind: "startup_task_recovered".into(),
                    status: "warn".into(),
                    title: format!("Recovered interrupted task `{}` to ready after restart", task.key),
                    detail: None,
                    duration_ms: None,
                    tokens: None,
                    cost: None,
                });
            }
        }
    }
    if recovered_any {
        core.drove.wake_scheduler();
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
        use windows_sys::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
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
                            && t.foreground
                                .as_ref()
                                .is_some_and(|fg| Some(fg.pid as u32) == t.pid && crate::foreground::is_shell_root(fg))
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

fn is_generic_tool_title(title: &str) -> bool {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return true;
    }
    let low = trimmed.to_ascii_lowercase();
    matches!(
        low.as_str(),
        "read"
            | "read_file"
            | "read file"
            | "view"
            | "view_file"
            | "cat"
            | "bash"
            | "shell"
            | "terminal"
            | "run"
            | "run_command"
            | "run command"
            | "execute"
            | "exec"
            | "edit"
            | "edit_file"
            | "edit file"
            | "write"
            | "write_file"
            | "write file"
            | "search"
            | "grep"
            | "glob"
            | "find"
            | "list"
            | "list_dir"
            | "ls"
            | "fetch"
    )
}

fn is_readonly_tool_kind(kind: &str) -> bool {
    matches!(
        kind.trim().to_ascii_lowercase().as_str(),
        "read" | "search" | "grep" | "glob" | "find" | "list" | "view" | "fetch"
    )
}

/// Deduplicates streaming `tool_call` / `tool_call_update` items by `tool.id` (preserving completion
/// order and keeping the latest status per unique tool invocation).
fn collect_distinct_finished_tools(items: &[Item]) -> Vec<&ItemTool> {
    let mut latest_idx: HashMap<&str, usize> = HashMap::new();
    let mut seq_tools: Vec<Option<&ItemTool>> = Vec::new();
    for it in items {
        if it.kind != "tool" {
            continue;
        }
        let Some(tl) = it.tool.as_ref() else {
            continue;
        };
        let status = tl.status.as_deref().unwrap_or("").to_ascii_lowercase();
        if !matches!(status.as_str(), "completed" | "failed" | "error") {
            continue;
        }
        if tl.id.is_empty() {
            seq_tools.push(Some(tl));
        } else if let Some(&prev) = latest_idx.get(tl.id.as_str()) {
            seq_tools[prev] = None;
            latest_idx.insert(tl.id.as_str(), seq_tools.len());
            seq_tools.push(Some(tl));
        } else {
            latest_idx.insert(tl.id.as_str(), seq_tools.len());
            seq_tools.push(Some(tl));
        }
    }
    seq_tools.into_iter().flatten().collect()
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
    let now_ms = super::now_ms_u64();
    let elapsed_sec = parse_iso_ms(&att.started_at).map_or(0, |s| now_ms.saturating_sub(s) / 1000);
    if elapsed_sec < MIN_LOOP_DETECT_ELAPSED_SEC {
        return;
    }

    let items = core.drove.store.list_items(&worker.id, Some(attempt_id), None, 64);
    let distinct_tools = collect_distinct_finished_tools(&items);
    let recent_tools: Vec<(&str, &str, bool)> = distinct_tools
        .iter()
        .map(|tl| {
            let kind = tl.kind.as_deref().unwrap_or("");
            let title = tl.title.as_deref().unwrap_or("").trim();
            let failed = matches!(
                tl.status.as_deref().unwrap_or("").to_ascii_lowercase().as_str(),
                "failed" | "error"
            );
            (kind, title, failed)
        })
        .filter(|(_, title, _)| !title.is_empty())
        .collect();

    // Stage 1 (0ms exact rule): 6 consecutive identical distinct tool calls with either a
    // specific non-generic title (non-readonly) or repeated failures.
    if recent_tools.len() >= 6 {
        let tail = &recent_tools[recent_tools.len() - 6..];
        let (first_kind, first_title, _) = tail[0];
        let all_same = tail.iter().all(|(k, t, _)| *k == first_kind && *t == first_title);
        let any_failed = tail.iter().any(|(_, _, f)| *f);
        let meaningful = any_failed || (!is_generic_tool_title(first_title) && !is_readonly_tool_kind(first_kind));
        if all_same && meaningful {
            trigger_loop_detection(
                core,
                worker,
                &att,
                &cfg.ai.loop_detect,
                format!("Repeated tool `{}` 6 times", first_title),
                format!(
                    "[drove loop-detect] You repeated identical tool call `{}` 6 times. \
                     Stop repeating it, verify current workspace state, and take a different concrete step \
                     or call `h2 drove worker ask` / `h2 drove worker done --outcome failed`.",
                    first_title
                ),
            )
            .await;
            return;
        }
    }

    // Stage 2 (incremental semantic oscillation audit via AiSessionPool):
    // Require >= 120s elapsed, >= 12 distinct finished tool calls, >= 8 new calls since watermark,
    // per-attempt cooldown & call cap, and concrete oscillation/failure evidence.
    if elapsed_sec < MIN_SEMANTIC_LOOP_ELAPSED_SEC
        || recent_tools.len() < 12
        || !core.ai.is_configured()
        || core.ai.is_circuit_open()
        || !core.drove.can_run_loop_audit(attempt_id, SEMANTIC_LOOP_COOLDOWN, SEMANTIC_LOOP_MAX_CHECKS)
    {
        return;
    }
    let prev_wm = core.drove.get_loop_watermark(attempt_id);
    let max_id = items.iter().map(|it| it.id).max().unwrap_or(0);
    if max_id <= prev_wm {
        return;
    }
    let delta_items: Vec<Item> = items.iter().filter(|it| it.id > prev_wm).cloned().collect();
    let delta_distinct_tools = collect_distinct_finished_tools(&delta_items).len();
    if prev_wm > 0 && delta_distinct_tools < 8 {
        return;
    }

    let tail12 = &recent_tools[recent_tools.len().saturating_sub(12)..];
    let failed_in_tail = tail12.iter().filter(|(_, _, f)| *f).count();
    let specific_tail: Vec<(&str, &str)> = tail12
        .iter()
        .filter(|(k, t, f)| *f || (!is_generic_tool_title(t) && !is_readonly_tool_kind(k)))
        .map(|(k, t, _)| (*k, *t))
        .collect();
    let unique_specific: std::collections::HashSet<(&str, &str)> = specific_tail.iter().copied().collect();
    let ping_pong_signal = specific_tail.len() >= 10 && unique_specific.len() <= 2;
    let oscillation_signal =
        failed_in_tail >= 3 || has_repeated_edit_failure_pattern(&distinct_tools) || ping_pong_signal;
    if !oscillation_signal {
        return;
    }

    let Some(task) = core.drove.store.get_task(&att.task_id) else {
        return;
    };
    core.drove.set_loop_watermark(attempt_id, max_id);
    core.drove.record_loop_audit_run(attempt_id);

    let Some(verdict) = super::ai::detect_semantic_loop(core, &att.run_id, attempt_id, &task, &delta_items).await
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

fn has_repeated_edit_failure_pattern(distinct_tools: &[&ItemTool]) -> bool {
    let mut edit_paths: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut failed_cmds = 0usize;
    for tl in distinct_tools.iter().rev().take(16) {
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
            } else if let Some(title) = tl.title.as_deref().filter(|t| !is_generic_tool_title(t)) {
                *edit_paths.entry(title).or_default() += 1;
            }
        }
    }
    edit_paths.values().any(|&n| n >= 3) && failed_cmds >= 3
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
pub async fn send_to_task_worker(core: &Arc<Core>, task: &Task, text: &str, interrupt: bool) -> Result<Value, String> {
    let attempts = core.drove.store.list_attempts(None, Some(&task.id), None);
    let latest = attempts
        .iter()
        .rev()
        .find(|a| a.outcome.is_none())
        .or_else(|| attempts.last())
        .ok_or_else(|| "Task has no worker attempt yet".to_string())?;
    let worker = core.drove.store.get_worker(&latest.worker_id).ok_or_else(|| "Worker not found".to_string())?;
    let aid = worker
        .h2_agent
        .as_deref()
        .and_then(crate::agents::parse_id)
        .ok_or_else(|| "Worker has no live herdr2 ACP agent".to_string())?;

    if interrupt {
        let _ = core.interrupt(aid, std::time::Duration::from_secs(5)).await;
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
    core.send(aid, text, interrupt, None, Some("drove")).await.map_err(|e| e.message)?;
    Ok(json!({ "ok": true, "workerId": worker.id, "agent": worker.h2_agent, "interrupted": interrupt }))
}

fn load_task_contracts(core: &Core, run_id: &str, task: &Task) -> Vec<ContractPost> {
    if task.channels.is_empty() {
        core.drove.store.list_contracts(run_id, Some(&["main".into()]))
    } else {
        core.drove.store.list_contracts(run_id, Some(&task.channels))
    }
}

async fn resolve_task_fork_context(
    core: &Arc<Core>,
    run: &Run,
    task: &Task,
    target_agent: &str,
    effective_cwd: &str,
) -> Option<crate::api::handoff::DroveForkContext> {
    let raw_fork = task.fork_from.as_deref()?.trim();
    if raw_fork.is_empty() {
        return None;
    }
    let source_addr = if raw_fork.eq_ignore_ascii_case("coordinator") {
        run.coordinator.host_session.clone()
    } else {
        let up_task = core.drove.store.find_task_in_run(&run.id, raw_fork)?;
        let attempts = core.drove.store.list_attempts(Some(&run.id), Some(&up_task.id), None);
        let src_att = attempts
            .iter()
            .rev()
            .find(|a| a.outcome == Some(Outcome::Succeeded))
            .or_else(|| attempts.last())?;
        core.drove.store.get_worker(&src_att.worker_id)?.h2_agent?
    };
    crate::api::handoff::build_drove_fork_context(
        core,
        &source_addr,
        target_agent,
        &run.cwd,
        effective_cwd,
        task.fork_mode.as_deref(),
    )
    .await
}

/// Returns `true` when `msg` indicates an upstream model rate-limit, quota, or capacity failure
/// eligible for an in-place model switch on the existing live ACP worker session.
pub fn is_recoverable_model_or_quota_error(msg: &str) -> bool {
    let low = msg.to_ascii_lowercase();
    low.contains("429")
        || low.contains("rate limit")
        || low.contains("rate_limit")
        || low.contains("too many requests")
        || low.contains("quota")
        || low.contains("insufficient_quota")
        || low.contains("resource_exhausted")
        || low.contains("overloaded")
        || low.contains("529")
        || low.contains("model_not_available")
        || low.contains("model_not_found")
        || low.contains("out of capacity")
}

pub(crate) fn same_acp_adapter_family(a: &str, b: &str) -> bool {
    let canon = |s: &str| match s.trim().to_ascii_lowercase().as_str() {
        "antigravity" | "antigravity-cli" => "agy".to_string(),
        "cursor-agent" => "cursor".to_string(),
        "claude-code" => "claude".to_string(),
        "gemini-cli" => "gemini".to_string(),
        other => other.to_string(),
    };
    canon(a) == canon(b)
}

/// Attempts an Antigravity `/swarm`-style in-place model switch on `worker`'s live ACP session
/// when a 429/quota/capacity error occurs and a healthy fallback model exists on the SAME adapter.
async fn try_in_place_model_failover(
    core: &Arc<Core>,
    worker: &Worker,
    attempt: &Attempt,
    err_msg: &str,
) -> bool {
    if !is_recoverable_model_or_quota_error(err_msg) {
        return false;
    }
    let Some(aid) = live_untrashed_agent_id(core, worker.h2_agent.as_deref()) else {
        return false;
    };
    if worker.context.size > 0 && worker.context.used.saturating_mul(100) / worker.context.size >= 75 {
        return false;
    }
    let Some(task) = core.drove.store.get_task(&attempt.task_id) else {
        return false;
    };
    let cfg = core.drove.config_snapshot();
    let role_name = worker.role.as_deref().unwrap_or("coder");
    let Some(role_cfg) = super::config::resolve_role_config(&cfg, role_name) else {
        return false;
    };
    let quotas = if cfg.magpie.enabled { core.drove.magpie.fetch_if_enabled(&cfg.magpie, false).await.0 } else { None };
    let prior_attempts = core.drove.store.list_attempts(Some(&attempt.run_id), Some(&task.id), None);

    let next_cand = std::iter::once(&role_cfg.primary)
        .chain(role_cfg.fallbacks.iter())
        .find(|cand| {
            same_acp_adapter_family(&cand.agent, &worker.agent)
                && cand.model.as_deref().is_some_and(|m| !m.trim().is_empty())
                && cand.model != attempt.model
                && !prior_attempts.iter().any(|pa| {
                    same_acp_adapter_family(&pa.agent, &cand.agent) && pa.model == cand.model
                })
                && super::magpie::quota_exhausted_reason(
                    &cand.agent,
                    cand.model.as_deref(),
                    quotas.as_deref().unwrap_or(&[]),
                )
                .is_none()
        })
        .cloned();
    let Some(cand) = next_cand else {
        return false;
    };
    let Some(next_model) = cand.model.clone() else {
        return false;
    };

    // 1. Settle the quota-hit attempt WITHOUT resetting worktree or touching task status.
    let ended_at = now_iso();
    let _ = core.drove.store.update_attempt(&attempt.id, |a| {
        if a.outcome.is_none() {
            a.outcome = Some(Outcome::Failed);
            a.ended_at = Some(ended_at);
            a.failure = Some(AttemptFailure {
                kind: FailureKind::QuotaExhausted,
                message: format!("Switched in-place to `{next_model}` after: {err_msg}"),
            });
        }
    });
    core.drove.clear_loop_watermark(&attempt.id);

    // 2. Switch model + reasoning effort in-place on the live ACP session.
    apply_warm_config(core, aid, "model", json!(&next_model)).await;
    let is_agy = matches!(worker.agent.as_str(), "agy" | "antigravity");
    let is_cursor = matches!(worker.agent.as_str(), "cursor" | "cursor-agent");
    let cursor_has_effort = is_cursor && acp::model_has_embedded_effort(&next_model);
    if !is_agy && !cursor_has_effort {
        apply_warm_config(core, aid, "reasoning_effort", json!(cand.effort.as_str())).await;
    }

    // 3. Create successor attempt inheriting the original attempt's dirty baseline so all touched
    //    files from before + after the switch are validated and merged together on `drove_finish`.
    let next_att_id = super::crypto::rand_id("att");
    if let Some(orig_base) = core.drove.get_attempt_baseline(&attempt.id) {
        core.drove.record_attempt_baseline(&next_att_id, orig_base);
    }
    let min_turn = {
        let st = core.lock();
        let base = st.reg.agents.get(&aid).map_or(0, |a| {
            let in_flight = u64::from(a.status.as_ref().is_some_and(|s| s.state == "working"));
            a.turn + in_flight
        });
        let queued = st.inboxes.get(&aid).map_or(0, |q| q.len() as u64);
        base + queued + 1
    };
    core.drove.set_attempt_min_turn(&next_att_id, min_turn);
    core.drove.set_worker_last_attempt(&worker.id, &next_att_id);

    let route_reason = format!("in_place_model_switch -> {next_model}");
    let next_attempt = Attempt {
        id: next_att_id.clone(),
        task_id: task.id.clone(),
        run_id: attempt.run_id.clone(),
        worker_id: worker.id.clone(),
        agent: worker.agent.clone(),
        model: Some(next_model.clone()),
        effort: Some(cand.effort),
        fast: cand.fast.then_some(true),
        route_reason: route_reason.clone(),
        reused: true,
        retry_of: Some(attempt.id.clone()),
        hint: attempt.hint.clone(),
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
        review_round: attempt.review_round.clone(),
        verifying: None,
        verify_rejections: attempt.verify_rejections,
        stagnant_rejections: attempt.stagnant_rejections,
        last_verify_hash: attempt.last_verify_hash.clone(),
        last_tree_sha: attempt.last_tree_sha.clone(),
        verify_log_artifact_id: None,
        verify_ai_digest: None,
        ai_diagnosis: None,
        spinning_detected_at: None,
    };
    core.drove.store.insert_attempt(next_attempt);

    let _ = core.drove.store.update_worker(&worker.id, |w| {
        w.state = WorkerState::Busy;
        w.current_attempt_id = Some(next_att_id.clone());
        w.model = Some(next_model.clone());
        w.effort = Some(cand.effort);
        w.totals.attempts += 1;
        w.last_active_at = now_iso();
    });
    let _ = core.drove.store.update_task(&task.id, |t| {
        let mut a = t.assign.clone().unwrap_or_default();
        a.model = Some(next_model.clone());
        a.effort = Some(cand.effort);
        a.reason = Some(route_reason.clone());
        t.assign = Some(a);
    });
    core.drove.store.append_daemon_event(NewDaemonEvent {
        run_id: Some(attempt.run_id.clone()),
        task_id: Some(task.id.clone()),
        attempt_id: Some(next_att_id.clone()),
        worker_id: Some(worker.id.clone()),
        message_id: None,
        category: "worker".into(),
        kind: "in_place_model_switch".into(),
        status: "ok".into(),
        title: format!("Switched `{}` in-place to `{next_model}`", worker.id),
        detail: Some(err_msg.chars().take(180).collect()),
        duration_ms: None,
        tokens: None,
        cost: None,
    });

    let cont_prompt = format!(
        "[drove in-place failover] Your previous model encountered a rate-limit/quota error ({err_msg}) and has been switched in-place to `{next_model}` on this exact session and workspace (`{}`). \
         All your uncommitted edits and prior tool history are preserved. Continue task `{}` from where you left off without repeating completed file reads, and call `drove_finish` when complete.",
        next_att_id, task.key
    );
    core.send(aid, &cont_prompt, false, None, Some("drove")).await.is_ok()
}

/// Switches a running worker's model/effort in place without restarting its active attempt (`h2 drove route --in-place`).
pub(super) async fn switch_running_task_model_in_place(
    core: &Arc<Core>,
    task: &Task,
    model: Option<String>,
    effort: Option<Effort>,
) -> Result<Value, String> {
    let attempts = core.drove.store.list_attempts(None, Some(&task.id), None);
    let active = attempts
        .iter()
        .rev()
        .find(|a| a.outcome.is_none() && a.ended_at.is_none())
        .ok_or_else(|| "Task has no active attempt".to_string())?;
    let worker = core.drove.store.get_worker(&active.worker_id).ok_or_else(|| "Worker not found".to_string())?;
    let aid = live_untrashed_agent_id(core, worker.h2_agent.as_deref())
        .ok_or_else(|| "Worker ACP session is no longer live".to_string())?;

    if let Some(ref m) = model {
        apply_warm_config(core, aid, "model", json!(m)).await;
    }
    if let Some(eff) = effort {
        let is_agy = matches!(worker.agent.as_str(), "agy" | "antigravity");
        if !is_agy {
            apply_warm_config(core, aid, "reasoning_effort", json!(eff.as_str())).await;
        }
    }
    let _ = core.drove.store.update_worker(&worker.id, |w| {
        if let Some(ref m) = model {
            w.model = Some(m.clone());
        }
        if let Some(eff) = effort {
            w.effort = Some(eff);
        }
    });
    let _ = core.drove.store.update_attempt(&active.id, |a| {
        if let Some(ref m) = model {
            a.model = Some(m.clone());
        }
        if let Some(eff) = effort {
            a.effort = Some(eff);
        }
    });
    let updated = core.drove.store.update_task(&task.id, |t| {
        let mut a = t.assign.clone().unwrap_or_default();
        if let Some(ref m) = model {
            a.model = Some(m.clone());
        }
        if let Some(eff) = effort {
            a.effort = Some(eff);
        }
        a.source = Some("user_in_place".into());
        t.assign = Some(a);
    });
    Ok(json!(updated))
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
        let from_turn = (max_turn > 4).then(|| max_turn.saturating_sub(3));
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

    let git_status = super::bg_tokio_command("git")
        .args(["-C", effective_cwd, "status", "--short", "--untracked-files=normal"])
        .output()
        .await
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .filter(|s| !s.trim().is_empty());

    let failure_reason = prev.failure.as_ref().map(|f| format!("{:?}: {}", f.kind, f.message)).or_else(|| {
        prev.result.as_ref().and_then(|r| r.blockers.as_ref()).filter(|b| !b.is_empty()).map(|b| b.join("; "))
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
        TargetKey::Terminal(id) => {
            st.pty_reg.terminals.get(&id).filter(|t| t.is_live()).map(|_| crate::terminals::address(id))
        }
        TargetKey::Agent(id) => st.reg.agents.get(&id).filter(|a| a.is_live()).map(|_| crate::agents::address(id)),
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
            core.drove.store.append_item(worker_id, attempt_id, "assistant", Some(text), None, None);
        }
        Entry::Thought { text, .. } if !text.is_empty() => {
            core.drove.store.append_item(worker_id, attempt_id, "thought", Some(text), None, None);
        }
        Entry::ToolCall { id, title, tool_kind, status, detail, .. } => {
            let clean_title = title.trim();
            let clean_status = status.trim();
            // Skip empty-shell streaming `tool_call_update` chunks (e.g. Codex stdout stream frames
            // carrying only `toolCallId` with empty `title` and `status`) so `inspect.lastItems` is
            // never flooded with meaningless duplicates.
            if clean_title.is_empty() && clean_status.is_empty() {
                return;
            }
            let output = detail
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(512).collect::<String>());
            core.drove.store.append_item(
                worker_id,
                attempt_id,
                "tool",
                None,
                Some(ItemTool {
                    id,
                    title: (!clean_title.is_empty()).then(|| clean_title.to_string()),
                    kind: tool_kind,
                    status: (!clean_status.is_empty()).then(|| clean_status.to_string()),
                    locations: None,
                    input: None,
                    output,
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
            core.drove.store.append_item(worker_id, attempt_id, "system", Some(text), None, None);
        }
        _ => {}
    }
}

/// Preserves user/plan explicit overrides while marking router-selected assignments as `"router"` so retries can step down fallback chains.
fn next_assign_source(task: &Task) -> Option<String> {
    let prev_source = task.assign.as_ref().and_then(|a| a.source.as_deref());
    let had_explicit_agent =
        task.assign.as_ref().and_then(|a| a.agent.as_deref()).is_some_and(|s| !s.trim().is_empty())
            && prev_source != Some("router");
    match prev_source {
        Some(s) => Some(s.to_string()),
        None if had_explicit_agent => Some("plan".into()),
        None => Some("router".into()),
    }
}

/// Marks a task Failed and propagates terminal failure to pending `AllSuccess` dependents
/// without posting the generic `task_failed` message (used when a dedicated circuit-breaker
/// escalation message is emitted instead).
pub fn mark_task_failed_and_propagate(core: &Arc<Core>, run_id: &str, task_id: &str) -> Option<Task> {
    let updated = core.drove.store.update_task(task_id, |t| {
        if matches!(t.status, TaskStatus::Cancelled | TaskStatus::Skipped | TaskStatus::Succeeded) {
            return;
        }
        t.failures += 1;
        t.status = TaskStatus::Failed;
    });
    for _ in 0..8 {
        let before = core.drove.store.list_tasks(run_id);
        super::scheduler::promote_pending_tasks(core, &before);
        let after = core.drove.store.list_tasks(run_id);
        if before.iter().map(|t| t.status).eq(after.iter().map(|t| t.status)) {
            break;
        }
    }
    super::ai::spawn_run_narrator(core.clone(), run_id.to_string());
    updated
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
    let effective_max_retries = if max_retries == 0 {
        0
    } else {
        core.drove
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
                let chain_len =
                    super::config::resolve_role_config(&cfg, role_name).map_or(1, |r| 1 + r.fallbacks.len());
                max_retries.max(chain_len.saturating_sub(1) as u32).min(8)
            })
            .unwrap_or(max_retries)
    };

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
    if should_notify && let (Some(run), Some(task)) = (core.drove.store.get_run(run_id), updated_task) {
        // Propagate terminal failure to pending `AllSuccess` dependents immediately so `drove wait`
        // wakes up with a consistent settled/skipped task graph.
        for _ in 0..8 {
            let before = core.drove.store.list_tasks(run_id);
            super::scheduler::promote_pending_tasks(core, &before);
            let after = core.drove.store.list_tasks(run_id);
            if before.iter().map(|t| t.status).eq(after.iter().map(|t| t.status)) {
                break;
            }
        }
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
        super::ai::spawn_run_narrator(core.clone(), run.id.clone());
    }
}

/// Ensures Drove worker/reviewer child processes inherit a complete `PATH` (including `~/.nvm` Node
/// shims, `~/.local/bin`, and `~/.cargo/bin`) even when `herdr2 hold` was spawned by a desktop GUI.
pub(super) fn inject_drove_path_env(env: &mut BTreeMap<String, String>) {
    let mut dirs: Vec<std::path::PathBuf> =
        std::env::var_os("PATH").map(|v| std::env::split_paths(&v).collect()).unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        let mut extras = vec![home.join(".local/bin"), home.join(".cargo/bin")];
        if let Ok(rd) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            let mut vers: Vec<std::path::PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_tool_item(id: u64, tool_id: &str, kind: &str, title: &str, status: &str) -> Item {
        Item {
            id,
            worker_id: "wrk_1".into(),
            attempt_id: Some("att_1".into()),
            ts: now_iso(),
            kind: "tool".into(),
            text: None,
            tool: Some(ItemTool {
                id: tool_id.into(),
                title: Some(title.into()),
                kind: Some(kind.into()),
                status: Some(status.into()),
                locations: None,
                input: None,
                output: None,
            }),
            usage: None,
        }
    }

    #[test]
    fn distinct_finished_tools_deduplicates_streaming_updates() {
        let items = vec![
            mk_tool_item(1, "tc_1", "execute", "cargo test", "pending"),
            mk_tool_item(2, "tc_1", "execute", "cargo test", "in_progress"),
            mk_tool_item(3, "tc_1", "execute", "cargo test", "completed"),
            mk_tool_item(4, "tc_2", "read", "Read", "completed"),
            mk_tool_item(5, "tc_1", "execute", "cargo test", "completed"),
        ];
        let distinct = collect_distinct_finished_tools(&items);
        assert_eq!(distinct.len(), 2);
        assert_eq!(distinct[0].id, "tc_2");
        assert_eq!(distinct[1].id, "tc_1");
        assert!(is_generic_tool_title("Read"));
        assert!(is_generic_tool_title("bash"));
        assert!(!is_generic_tool_title("cargo test --package herdr2"));
    }

    #[test]
    fn worker_usage_watermark_accumulates_deltas_without_double_counting() {
        let dir = std::env::temp_dir().join(format!("herdr2-pool-test-{}", super::super::crypto::rand_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = crate::paths::Paths {
            state: dir.clone(),
            config: dir.join("config.toml"),
            socket: dir.join("core.sock"),
            holder_socket: dir.join("core.holder.sock"),
        };
        let ai = crate::ai::AiRuntime::open(&paths).unwrap();
        let drove = super::super::DroveRuntime::open(&paths, ai).unwrap();

        assert_eq!(drove.advance_worker_usage_watermark("wrk_1", 10_000), 10_000);
        assert_eq!(drove.advance_worker_usage_watermark("wrk_1", 10_000), 0);
        assert_eq!(drove.advance_worker_usage_watermark("wrk_1", 25_000), 15_000);
        // Compaction / session reset drops used tokens below watermark: count new usage cleanly.
        assert_eq!(drove.advance_worker_usage_watermark("wrk_1", 4_000), 4_000);

        // Explicit (input, output, cache_read) usage watermark returns only incremental per-attempt deltas.
        assert_eq!(drove.advance_worker_explicit_usage_watermark("wrk_1", 1000, 200, 500), (1000, 200, 500));
        assert_eq!(drove.advance_worker_explicit_usage_watermark("wrk_1", 1600, 350, 900), (600, 150, 400));
        assert_eq!(drove.advance_worker_explicit_usage_watermark("wrk_1", 1600, 350, 900), (0, 0, 0));

        // Cross-attempt attribution routes stale pre-min_turn frames back to previous attempt.
        drove.set_worker_last_attempt("wrk_1", "att_1");
        drove.set_worker_last_attempt("wrk_1", "att_2");
        drove.set_attempt_min_turn("att_2", 3);
        let worker = Worker {
            id: "wrk_1".into(),
            run_id: "run_1".into(),
            agent: "agy".into(),
            model: None,
            context_window: None,
            effort: None,
            fast: None,
            session_key: "run_1:wrk_1".into(),
            h2_agent: Some("a1".into()),
            cwd: "/tmp".into(),
            worktree: None,
            state: WorkerState::Busy,
            current_attempt_id: Some("att_2".into()),
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
        };
        assert_eq!(resolve_signal_attempt_id(&drove, "wrk_1", Some(&worker), Some(2)).as_deref(), Some("att_1"));
        assert_eq!(resolve_signal_attempt_id(&drove, "wrk_1", Some(&worker), Some(3)).as_deref(), Some("att_2"));

        // Estimated output tokens accumulate and clear cleanly.
        assert_eq!(drove.add_attempt_estimated_output("att_2", 120), 120);
        assert_eq!(drove.add_attempt_estimated_output("att_2", 80), 200);
        drove.clear_loop_watermark("att_2");
        assert_eq!(drove.add_attempt_estimated_output("att_2", 10), 10);

        assert!(is_recoverable_model_or_quota_error("HTTP 429 Too Many Requests: rate_limit_error"));
        assert!(is_recoverable_model_or_quota_error("RESOURCE_EXHAUSTED: quota exceeded"));
        assert!(!is_recoverable_model_or_quota_error("syntax error in Cargo.toml"));
        assert!(same_acp_adapter_family("antigravity", "agy"));
        assert!(same_acp_adapter_family("cursor-agent", "cursor"));
        assert!(!same_acp_adapter_family("claude", "codex"));
    }
}
