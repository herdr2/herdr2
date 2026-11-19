//! DAG scheduler, foreachFrom expansion, exec task runner, and stall watchdog.

use super::notify::notify_coordinator;
use super::pool::dispatch_agent_task;
use super::protocol::when_matches;
use super::router::{candidate_pool_for_ai, route_task_with_preference};
use super::types::*;
use super::worktree_for_task;
use crate::runtime::Core;
use serde_json::Value;
use std::collections::HashSet;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

const WATCHDOG_INTERVAL: Duration = Duration::from_secs(2);

/// Event-driven scheduler loop waking on `Store::changed`, `DroveRuntime::scheduler_wake`, or 2s tick.
pub async fn run_scheduler_loop(core: Arc<Core>) {
    super::pool::reconcile_startup_workers(&core).await;
    let mut tick = tokio::time::interval(WATCHDOG_INTERVAL);
    tick.tick().await;
    loop {
        tokio::select! {
            _ = core.drove.store.changed.notified() => {}
            _ = core.drove.scheduler_wake.notified() => {}
            _ = tick.tick() => {}
            _ = core.drove.http_stop.notified() => break,
        }
        if core.lock().stopping {
            break;
        }
        tick_once(&core).await;
    }
}

pub async fn tick_once(core: &Arc<Core>) {
    let runs = core.drove.store.list_runs();
    let cfg = core.drove.config_snapshot();
    let probed = core.drove.cached_probe();

    for run in runs {
        let has_open_workers = core
            .drove
            .store
            .list_workers(Some(&run.id))
            .iter()
            .any(|w| !matches!(w.state, WorkerState::Closed | WorkerState::Dead));
        if run.trashed_at.is_some() {
            if has_open_workers {
                super::pool::stop_run_workers(core, &run.id).await;
            }
            continue;
        }
        if (has_open_workers || run.status == RunStatus::Active)
            && super::pool::is_coordinator_closed(core, &run)
        {
            super::pool::close_run_for_coordinator(core, &run).await;
            continue;
        }
        if run.status != RunStatus::Active {
            continue;
        }
        let mut tasks = core.drove.store.list_tasks(&run.id);
        // 1. Expand any `foreachFrom` templates whose upstream dependency just succeeded.
        expand_foreach_from(core, &run, &tasks);
        tasks = core.drove.store.list_tasks(&run.id);

        // 2. Promote Pending tasks whose deps + triggerRule + when are satisfied.
        promote_pending_tasks(core, &tasks);
        tasks = core.drove.store.list_tasks(&run.id);

        // 3. Check silence / stall / timeout watchdog for Running tasks.
        check_running_timeouts_and_stalls(core, &run, &tasks).await;
        tasks = core.drove.store.list_tasks(&run.id);

        if !tasks.is_empty()
            && tasks
                .iter()
                .filter(|t| t.template != Some(true))
                .all(|t| is_terminal_status(t.status))
        {
            continue;
        }

        // 4. Recompute concurrency & dispatch Ready tasks (`exec` or `agent`).
        let workers = core.drove.store.list_workers(Some(&run.id));
        let busy_agents = workers.iter().filter(|w| w.state == WorkerState::Busy).count() as u32;
        let running_execs = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Running && t.kind.as_deref() == Some("exec"))
            .count() as u32;
        let mut slots = run.max_concurrency.saturating_sub(busy_agents + running_execs);
        if slots == 0 {
            continue;
        }

        let ready_tasks: Vec<Task> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Ready && t.template != Some(true))
            .cloned()
            .collect();
        let quotas = if !ready_tasks.is_empty() && cfg.magpie.enabled {
            core.drove.magpie.fetch_if_enabled(&cfg.magpie, false).await.0
        } else {
            None
        };

        let repo_supports_wt = super::integrate::is_git_repo(&run.cwd)
            && !super::integrate::is_aosp_harness(&run.cwd);
        let running_shared_writers = tasks
            .iter()
            .filter(|t| {
                t.status == TaskStatus::Running
                    && t.isolation == Isolation::None
                    && task_writes_workspace(&cfg, t)
            })
            .count();
        let mut shared_writers_started_this_tick = 0usize;
        let mut promoted_shared_writers_in_tick = 0usize;

        for (idx, mut task) in ready_tasks.iter().cloned().enumerate() {
            if slots == 0 {
                break;
            }
            if !core.drove.try_reserve_task(&task.id) {
                continue;
            }
            slots -= 1;

            if task.kind.as_deref() == Some("exec") {
                let c = core.clone();
                let r = run.clone();
                tokio::spawn(async move {
                    run_exec_task(c, r, task).await;
                });
                continue;
            }

            if task.isolation == Isolation::None && task_writes_workspace(&cfg, &task) {
                let remaining_ready_writers = ready_tasks[idx + 1..]
                    .iter()
                    .filter(|other| other.isolation == Isolation::None && task_writes_workspace(&cfg, other))
                    .count();
                let can_overlap_in_tick = slots > 0 && remaining_ready_writers > 0;
                let conflict_risk = running_shared_writers > 0
                    || shared_writers_started_this_tick > 0
                    || promoted_shared_writers_in_tick > 0
                    || can_overlap_in_tick;
                if conflict_risk {
                    match cfg.ai.conflict_policy.as_str() {
                        "auto_worktree" if repo_supports_wt => {
                            let _ = core.drove.store.update_task(&task.id, |t| {
                                t.isolation = Isolation::Worktree;
                            });
                            task.isolation = Isolation::Worktree;
                            promoted_shared_writers_in_tick += 1;
                            core.drove.store.append_daemon_event(super::store::NewDaemonEvent {
                                run_id: Some(run.id.clone()),
                                task_id: Some(task.id.clone()),
                                attempt_id: None,
                                worker_id: None,
                                message_id: None,
                                category: "schedule".into(),
                                kind: "auto_worktree".into(),
                                status: "ok".into(),
                                title: format!("Auto-isolated `{}` into worktree", task.key),
                                detail: Some(
                                    "Concurrent writable/scoped_write tasks detected under conflictPolicy=auto_worktree"
                                        .into(),
                                ),
                                duration_ms: None,
                                tokens: None,
                                cost: None,
                            });
                        }
                        "warn" => {
                            shared_writers_started_this_tick += 1;
                            core.drove.store.append_daemon_event(super::store::NewDaemonEvent {
                                run_id: Some(run.id.clone()),
                                task_id: Some(task.id.clone()),
                                attempt_id: None,
                                worker_id: None,
                                message_id: None,
                                category: "schedule".into(),
                                kind: "write_conflict_warn".into(),
                                status: "warn".into(),
                                title: format!("Concurrent shared-cwd write on `{}`", task.key),
                                detail: Some(
                                    "Parallel writable tasks share run.cwd under conflictPolicy=warn".into(),
                                ),
                                duration_ms: None,
                                tokens: None,
                                cost: None,
                            });
                        }
                        _ => {
                            shared_writers_started_this_tick += 1;
                        }
                    }
                } else {
                    shared_writers_started_this_tick += 1;
                }
            }

            let current_workers = core.drove.store.list_workers(Some(&run.id));
            let ai_pref = if cfg.ai.enabled {
                if let Some((rname, pool)) =
                    candidate_pool_for_ai(&cfg, &task, &current_workers, &probed, quotas.as_deref())
                {
                    super::ai::pick_role_candidate(core, &run.id, &task, &rname, &pool).await
                } else {
                    None
                }
            } else {
                None
            };
            let mut decision = route_task_with_preference(
                &cfg,
                &task,
                &run.cwd,
                &current_workers,
                &probed,
                quotas.as_deref(),
                ai_pref.clone(),
            );
            if decision.warm_worker.is_none()
                && task.isolation == Isolation::None
                && repo_supports_wt
                && task_writes_workspace(&cfg, &task)
            {
                let explicit_worker = (task.assign.as_ref().and_then(|a| a.source.as_deref()) != Some("router"))
                    .then(|| task.assign.as_ref().and_then(|a| a.worker.as_deref()))
                    .flatten();
                let has_warm_wt = current_workers.iter().any(|w| {
                    w.state == WorkerState::Idle
                        && w.read_only_dir.is_none()
                        && w.agent == decision.agent
                        && w.role.as_deref().unwrap_or("coder") == decision.role
                        && explicit_worker.is_none_or(|eid| w.id == eid)
                        && w.worktree
                            .as_deref()
                            .is_some_and(|p| !p.trim().is_empty() && std::path::Path::new(p).is_dir())
                        && super::pool::live_untrashed_agent_id(core, w.h2_agent.as_deref()).is_some()
                });
                if has_warm_wt {
                    let _ = core.drove.store.update_task(&task.id, |t| {
                        t.isolation = Isolation::Worktree;
                    });
                    task.isolation = Isolation::Worktree;
                    decision = route_task_with_preference(
                        &cfg,
                        &task,
                        &run.cwd,
                        &current_workers,
                        &probed,
                        quotas.as_deref(),
                        ai_pref,
                    );
                }
            }
            if let Some(ref warm) = decision.warm_worker {
                let warm_live = super::pool::live_untrashed_agent_id(core, warm.h2_agent.as_deref()).is_some();
                if !warm_live {
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
                    decision.warm_worker = None;
                }
            }
            let reuse_wt = decision
                .warm_worker
                .as_ref()
                .and_then(|w| w.worktree.clone());
            let effective_cwd = match worktree_for_task(core, &run, &task, reuse_wt.as_deref()).await {
                Ok((cwd, wt)) => (cwd, wt),
                Err(err) => {
                    core.drove.release_task_reservation(&task.id);
                    super::fail_or_retry_task(
                        core,
                        &run.id,
                        &task.id,
                        "",
                        FailureKind::AgentError,
                        &format!("Worktree setup failed: {err}"),
                        0,
                    )
                    .await;
                    continue;
                }
            };
            if let Some(ref warm) = decision.warm_worker
                && warm.worktree.as_deref() != effective_cwd.1.as_deref()
            {
                decision.warm_worker = None;
            }

            let retry_of = core
                .drove
                .store
                .list_attempts(None, Some(&task.id), None)
                .into_iter()
                .rev()
                .find(|a| a.kind.as_deref() != Some("review"))
                .map(|a| a.id);
            let hint = core.drove.take_task_hint(&task.id);

            let res = dispatch_agent_task(
                core,
                &run,
                &task,
                decision,
                effective_cwd.0,
                effective_cwd.1,
                retry_of,
                hint,
            )
            .await;
            core.drove.release_task_reservation(&task.id);

            if let Err(err_msg) = res {
                super::fail_or_retry_task(
                    core,
                    &run.id,
                    &task.id,
                    "",
                    FailureKind::AgentError,
                    &err_msg,
                    super::MAX_AUTO_RETRIES,
                )
                .await;
            }
        }
    }
}

fn expand_foreach_from(core: &Arc<Core>, run: &Run, tasks: &[Task]) {
    for tpl in tasks {
        if tpl.template != Some(true) || tpl.expansion_state.as_deref() == Some("done") {
            continue;
        }
        let Some(ff) = &tpl.foreach_from else { continue };
        let Some(src) = tasks.iter().find(|t| t.key == ff.task || t.id == ff.task) else {
            continue;
        };
        if !is_terminal_status(src.status) {
            continue;
        }
        if src.status != TaskStatus::Succeeded {
            let _ = core.drove.store.update_task(&tpl.id, |t| {
                t.expansion_state = Some("done".into());
                t.status = TaskStatus::Skipped;
            });
            continue;
        }
        let arr = extract_foreach_array(src, ff.path.as_deref()).unwrap_or_default();
        if arr.is_empty() {
            let _ = core.drove.store.update_task(&tpl.id, |t| {
                t.expansion_state = Some("done".into());
                t.status = TaskStatus::Skipped;
            });
            continue;
        }
        let existing: HashSet<String> = tasks.iter().map(|t| t.key.clone()).collect();
        for (idx, item) in arr.into_iter().enumerate() {
            let child_key = format!("{}#{}", tpl.key, idx + 1);
            if existing.contains(&child_key) {
                continue;
            }
            let title = format!("{} [{}]", tpl.title, idx + 1);
            let mut spec = tpl.spec.clone();
            let item_str = match &item {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            spec.target = spec.target.replace("{{item}}", &item_str);
            spec.change = spec.change.replace("{{item}}", &item_str);
            spec.acceptance = spec.acceptance.replace("{{item}}", &item_str);

            core.drove.store.create_task(super::store::CreateTaskInput {
                run_id: run.id.clone(),
                key: child_key,
                title,
                phase: tpl.phase.clone(),
                spec,
                tags: tpl.tags.clone(),
                files: tpl.files.clone(),
                deps: vec![tpl.id.clone()],
                output_schema: tpl.output_schema.clone(),
                isolation: tpl.isolation,
                timeout_sec: tpl.timeout_sec,
                stall_warn_sec: tpl.stall_warn_sec,
                stall_kill_sec: tpl.stall_kill_sec,
                when: tpl.when.clone(),
                assign: tpl.assign.clone(),
                status: TaskStatus::Pending,
                result: None,
                kind: tpl.kind.clone(),
                trigger_rule: tpl.trigger_rule,
                context: Some(item),
                exec: tpl.exec.clone(),
                foreach_from: None,
                template: None,
                expanded_from: Some(tpl.id.clone()),
                review: tpl.review.clone(),
                cache: tpl.cache.clone(),
                cached_from: None,
            });
        }
        let _ = core.drove.store.update_task(&tpl.id, |t| {
            t.expansion_state = Some("done".into());
            t.status = TaskStatus::Succeeded;
        });
    }
}

fn extract_foreach_array(task: &Task, path: Option<&str>) -> Option<Vec<Value>> {
    let data = task.result.as_ref()?.data.as_ref()?;
    let target = if let Some(p) = path.map(str::trim).filter(|s| !s.is_empty()) {
        let mut cur = data;
        for seg in p.strip_prefix("data.").unwrap_or(p).split('.').filter(|s| !s.is_empty()) {
            cur = cur.get(seg)?;
        }
        cur
    } else {
        data
    };
    target.as_array().cloned()
}

fn promote_pending_tasks(core: &Arc<Core>, tasks: &[Task]) {
    for task in tasks {
        if task.status != TaskStatus::Pending || task.template == Some(true) {
            continue;
        }
        let rule = task.trigger_rule.unwrap_or(TriggerRule::AllSuccess);
        let mut dep_statuses = Vec::with_capacity(task.deps.len());
        let mut unresolved = false;
        for dep_ref in &task.deps {
            if let Some(d) = tasks.iter().find(|t| &t.id == dep_ref || &t.key == dep_ref) {
                if d.template == Some(true) && task.expanded_from.as_deref() != Some(d.id.as_str()) {
                    if d.expansion_state.as_deref() != Some("done") {
                        unresolved = true;
                    }
                    let mut child_count = 0usize;
                    for child in tasks
                        .iter()
                        .filter(|c| c.expanded_from.as_deref() == Some(d.id.as_str()))
                    {
                        child_count += 1;
                        if !is_terminal_status(child.status) {
                            unresolved = true;
                        }
                        dep_statuses.push(child.status);
                    }
                    if child_count == 0 {
                        if !is_terminal_status(d.status) {
                            unresolved = true;
                        }
                        dep_statuses.push(d.status);
                    }
                } else {
                    if !is_terminal_status(d.status) {
                        unresolved = true;
                    }
                    dep_statuses.push(d.status);
                }
            } else {
                for child in tasks.iter().filter(|c| {
                    c.key
                        .strip_prefix(dep_ref.as_str())
                        .is_some_and(|r| r.starts_with('#'))
                }) {
                    if !is_terminal_status(child.status) {
                        unresolved = true;
                    }
                    dep_statuses.push(child.status);
                }
            }
        }
        let (should_skip, should_ready) = match rule {
            TriggerRule::AllSuccess => {
                if dep_statuses
                    .iter()
                    .any(|s| matches!(s, TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Skipped))
                {
                    (true, false)
                } else if !unresolved {
                    if when_matches(task.when.as_ref(), tasks) {
                        (false, true)
                    } else {
                        (true, false)
                    }
                } else {
                    (false, false)
                }
            }
            TriggerRule::AnySuccess => {
                if dep_statuses.contains(&TaskStatus::Succeeded) {
                    if when_matches(task.when.as_ref(), tasks) {
                        (false, true)
                    } else {
                        (true, false)
                    }
                } else if !unresolved {
                    (true, false)
                } else {
                    (false, false)
                }
            }
        };
        if should_skip {
            let _ = core.drove.store.update_task(&task.id, |t| t.status = TaskStatus::Skipped);
        } else if should_ready {
            let _ = core.drove.store.update_task(&task.id, |t| t.status = TaskStatus::Ready);
        }
    }
}

async fn check_running_timeouts_and_stalls(core: &Arc<Core>, run: &Run, tasks: &[Task]) {
    let now_ms = super::now_ms_u64();
    for task in tasks {
        if task.status != TaskStatus::Running || task.template == Some(true) {
            continue;
        }
        let Some(att) = core
            .drove
            .store
            .list_attempts(None, Some(&task.id), None)
            .into_iter()
            .rev()
            .find(|a| a.outcome.is_none())
        else {
            continue;
        };
        let Some(worker) = core.drove.store.get_worker(&att.worker_id) else {
            continue;
        };
        if core.drove.has_active_ask(&att.id) {
            continue;
        }
        let last_ms = parse_iso_ms(&worker.last_active_at)
            .or_else(|| parse_iso_ms(&att.started_at))
            .unwrap_or(now_ms);
        let idle_sec = now_ms.saturating_sub(last_ms) / 1000;

        if let Some(kill_sec) = task.stall_kill_sec
            && idle_sec >= kill_sec
        {
            let _ = super::pool::stop_worker(core, &worker.id).await;
            let _ = core.drove.store.update_attempt(&att.id, |a| {
                a.outcome = Some(Outcome::Failed);
                a.ended_at = Some(now_iso());
                a.failure = Some(AttemptFailure {
                    kind: FailureKind::Timeout,
                    message: format!("Killed after {idle_sec}s inactivity"),
                });
            });
            super::fail_or_retry_task(
                core,
                &run.id,
                &task.id,
                &att.id,
                FailureKind::Timeout,
                &format!("Stall limit {kill_sec}s exceeded"),
                super::MAX_AUTO_RETRIES,
            )
            .await;
            continue;
        }

        if let Some(warn_sec) = task.stall_warn_sec
            && idle_sec >= warn_sec
            && core.drove.mark_stall_warned(&att.id)
        {
            notify_coordinator(
                core,
                run,
                "stalled",
                &format!("Task `{}` quiet for {idle_sec}s", task.key),
                &format!("[drove] Task `{}` worker quiet for {idle_sec}s", task.key),
                Some(&task.id),
                Some(&att.id),
                Some(&worker.id),
                None,
            )
            .await;
        }
    }
}

async fn run_exec_task(core: Arc<Core>, run: Run, task: Task) {
    let attempt_id = super::crypto::rand_id("att");
    let attempt = Attempt {
        id: attempt_id.clone(),
        task_id: task.id.clone(),
        run_id: run.id.clone(),
        worker_id: "exec".into(),
        agent: "exec".into(),
        model: None,
        effort: None,
        fast: None,
        route_reason: "exec".into(),
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
        kind: Some("exec".into()),
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
    core.drove.store.insert_attempt(attempt);
    core.drove.store.update_task(&task.id, |t| {
        t.status = TaskStatus::Running;
        t.exec_phase = Some(TaskPhase::Author);
    });
    core.drove.release_task_reservation(&task.id);

    let spec = task.exec.clone().unwrap_or(ExecSpec {
        cmd: Some(task.spec.change.clone()),
        program: None,
        args: None,
        env: None,
        timeout_sec: task.timeout_sec,
        parse_json: None,
    });
    let timeout = Duration::from_secs(spec.timeout_sec.or(task.timeout_sec).unwrap_or(300));
    let mut cmd = if let Some(prog) = &spec.program {
        let mut c = super::bg_tokio_command(prog);
        if let Some(args) = &spec.args {
            c.args(args);
        }
        c
    } else {
        #[cfg(windows)]
        let c = {
            let mut cmd = super::bg_tokio_command("cmd");
            cmd.args(["/C", spec.cmd.as_deref().unwrap_or("")]);
            cmd
        };
        #[cfg(not(windows))]
        let c = {
            let mut cmd = super::bg_tokio_command("sh");
            cmd.args(["-c", spec.cmd.as_deref().unwrap_or("")]);
            cmd
        };
        c
    };
    cmd.current_dir(&run.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(env_map) = &spec.env {
        cmd.envs(env_map);
    }

    let out = tokio::time::timeout(timeout, cmd.output()).await;
    match out {
        Ok(Ok(o)) => {
            let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if o.status.success() {
                let parsed_data = if spec.parse_json.unwrap_or(true) && !stdout.is_empty() {
                    serde_json::from_str::<Value>(&stdout).ok()
                } else {
                    None
                };
                let summary = if stdout.is_empty() {
                    "exec succeeded".into()
                } else {
                    stdout.chars().take(400).collect()
                };
                let tr = TaskResult {
                    summary,
                    compact_summary: None,
                    data: parsed_data,
                    files_modified: None,
                    blockers: None,
                    output_file_hashes: None,
                };
                let _ = core.drove.store.update_attempt(&attempt_id, |a| {
                    a.outcome = Some(Outcome::Succeeded);
                    a.ended_at = Some(now_iso());
                    a.result = Some(tr.clone());
                });
                let _ = core.drove.store.update_task(&task.id, |t| {
                    t.status = TaskStatus::Succeeded;
                    t.result = Some(tr);
                });
            } else {
                let msg = format!(
                    "exec exited {:?}: {}",
                    o.status.code(),
                    if stderr.is_empty() { stdout } else { stderr }
                );
                let _ = core.drove.store.update_attempt(&attempt_id, |a| {
                    a.outcome = Some(Outcome::Failed);
                    a.ended_at = Some(now_iso());
                    a.failure = Some(AttemptFailure { kind: FailureKind::AgentError, message: msg.clone() });
                });
                super::fail_or_retry_task(
                    &core,
                    &run.id,
                    &task.id,
                    &attempt_id,
                    FailureKind::AgentError,
                    &msg,
                    0,
                )
                .await;
            }
        }
        Ok(Err(e)) => {
            let msg = format!("exec spawn failed: {e}");
            let _ = core.drove.store.update_attempt(&attempt_id, |a| {
                a.outcome = Some(Outcome::Failed);
                a.ended_at = Some(now_iso());
                a.failure = Some(AttemptFailure { kind: FailureKind::AgentError, message: msg.clone() });
            });
            super::fail_or_retry_task(&core, &run.id, &task.id, &attempt_id, FailureKind::AgentError, &msg, 0).await;
        }
        Err(_) => {
            let msg = format!("exec timed out after {}s", timeout.as_secs());
            let _ = core.drove.store.update_attempt(&attempt_id, |a| {
                a.outcome = Some(Outcome::Failed);
                a.ended_at = Some(now_iso());
                a.failure = Some(AttemptFailure { kind: FailureKind::Timeout, message: msg.clone() });
            });
            super::fail_or_retry_task(&core, &run.id, &task.id, &attempt_id, FailureKind::Timeout, &msg, 0).await;
        }
    }
    core.drove.wake_scheduler();
}

pub fn is_terminal_status(s: TaskStatus) -> bool {
    matches!(
        s,
        TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Skipped
    )
}

fn task_writes_workspace(cfg: &DroveConfig, task: &Task) -> bool {
    if task.kind.as_deref() == Some("exec") || task.template == Some(true) {
        return false;
    }
    let role_name = task
        .assign
        .as_ref()
        .and_then(|a| a.role.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("coder");
    let mode = super::config::resolve_role_config(cfg, role_name)
        .and_then(|r| r.access)
        .unwrap_or(RoleAccessMode::Writable);
    mode != RoleAccessMode::Readonly
}
