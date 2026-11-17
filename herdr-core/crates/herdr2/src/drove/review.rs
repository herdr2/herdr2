//! Adversarial read-only reviewer loop (Full Diff -> R1, Delta Diff -> R2..Rn).

use super::http::ApiErr;
use super::notify::notify_coordinator;
use super::pool::mark_worker_idle;
use super::roles::diff_touched_paths;
use super::store::NewDaemonEvent;
use super::types::*;
use super::MAX_AUTO_RETRIES;
use crate::api::run::{DroveSpawnSpec, spawn_drove_agent};
use crate::runtime::Core;
use crate::transcript::Entry;
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    let max_rounds = spec.max_rounds.unwrap_or(2).clamp(1, 5);
    let round = attempt.review_round.as_ref().map(|r| r.round + 1).unwrap_or(1);

    let _ = core.drove.store.update_task(&task.id, |t| t.exec_phase = Some(TaskPhase::Review));

    // 1. Capture candidate patch (`<artifact>.patch`) and tree SHA.
    let tree_sha = git_tree_sha(work_cwd);
    let patch_text = capture_candidate_patch(work_cwd, &core.drove.get_attempt_baseline(&attempt.id).unwrap_or_default());
    let artifact_id = format!("review-{}-r{round}", attempt.id);
    let _ = core
        .drove
        .store
        .write_artifact(&run.id, &format!("{artifact_id}.patch"), patch_text.as_bytes());

    let prev_round = attempt.review_round.clone();
    let delta_artifact_id = if round > 1 {
        let delta_id = format!("review-{}-delta-r{round}", attempt.id);
        let delta_text = match (prev_round.as_ref().and_then(|r| r.tree_sha.as_deref()), tree_sha.as_deref()) {
            (Some(old_t), Some(new_t)) if old_t != new_t => git_diff_trees(work_cwd, old_t, new_t).unwrap_or_else(|| patch_text.clone()),
            _ => patch_text.clone(),
        };
        let _ = core
            .drove
            .store
            .write_artifact(&run.id, &format!("{delta_id}.patch"), delta_text.as_bytes());
        Some(delta_id)
    } else {
        None
    };

    // 2. Resolve reviewer agent/model/effort (`reviewer` role default + optional overrides).
    let cfg = core.drove.config_snapshot();
    let role_cfg = super::config::resolve_role_config(&cfg, "reviewer")
        .unwrap_or_else(|| super::config::default_roles().remove("reviewer").unwrap());
    let mut rev_agent = spec.agent.clone().unwrap_or_else(|| role_cfg.primary.agent.clone());
    if spec.different_agent == Some(true)
        && rev_agent == attempt.agent
        && let Some(fb) = role_cfg.fallbacks.iter().find(|f| f.agent != attempt.agent)
    {
        rev_agent = fb.agent.clone();
    }
    let rev_model = spec.model.clone().or_else(|| role_cfg.primary.model.clone());
    let rev_effort = spec.effort.unwrap_or(role_cfg.primary.effort);

    let rev_worker_id = super::crypto::rand_id("wrk");
    let rev_tok = core.drove.register_worker_token(&rev_worker_id);
    let rev_att_id = super::crypto::rand_id("att");
    let display = format!("reviewer · {}", task.key);

    let mut labels = BTreeMap::new();
    labels.insert("drove.run".into(), run.id.clone());
    labels.insert("drove.worker".into(), rev_worker_id.clone());
    labels.insert("drove.role".into(), "reviewer".into());
    labels.insert("drove.task".into(), task.key.clone());
    labels.insert("drove.display".into(), display.clone());

    let port = core.drove.web_port.load(std::sync::atomic::Ordering::SeqCst);
    let mut extra_env = BTreeMap::new();
    extra_env.insert("H2_DROVE_URL".into(), format!("http://127.0.0.1:{port}"));
    extra_env.insert("H2_DROVE_WORKER_TOKEN".into(), rev_tok);
    extra_env.insert("H2_DROVE_WORKER_ID".into(), rev_worker_id.clone());
    extra_env.insert("H2_DROVE_RUN_ID".into(), run.id.clone());
    extra_env.insert("H2_DROVE_ATTEMPT_ID".into(), rev_att_id.clone());

    let rev_worker = Worker {
        id: rev_worker_id.clone(),
        run_id: run.id.clone(),
        agent: rev_agent.clone(),
        model: rev_model.clone(),
        effort: Some(rev_effort),
        session_key: format!("{}:{rev_worker_id}", run.id),
        h2_agent: None,
        cwd: work_cwd.to_string(),
        worktree: None,
        state: WorkerState::Starting,
        current_attempt_id: Some(rev_att_id.clone()),
        pid: None,
        fingerprint: Fingerprint::default(),
        context: WorkerContext::default(),
        totals: WorkerTotals::default(),
        created_at: now_iso(),
        last_active_at: now_iso(),
        role: Some("reviewer".into()),
        forked_from: None,
        protocol_gen: Some(1),
        read_only_dir: Some(work_cwd.to_string()),
    };
    core.drove.store.insert_worker(rev_worker);

    let rev_attempt = Attempt {
        id: rev_att_id.clone(),
        task_id: task.id.clone(),
        run_id: run.id.clone(),
        worker_id: rev_worker_id.clone(),
        agent: rev_agent.clone(),
        model: rev_model.clone(),
        effort: Some(rev_effort),
        route_reason: format!("review round={round}"),
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

    let spawn = spawn_drove_agent(
        core,
        DroveSpawnSpec {
            agent: rev_agent.clone(),
            cwd: work_cwd.to_string(),
            parent: resolve_local_parent_addr(core, &run.coordinator.host_session),
            title: display,
            labels,
            env: extra_env,
            model: rev_model,
            effort: Some(rev_effort.as_str().to_string()),
            access: RoleAccessMode::Readonly,
            isolate_secrets: true,
        },
    )
    .await
    .map_err(|e| ApiErr::new(500, "review_spawn_failed", format!("Failed spawning reviewer: {}", e.message)))?;

    core.drove.store.update_worker(&rev_worker_id, |w| {
        w.state = WorkerState::Busy;
        w.h2_agent = Some(spawn.address.clone());
        w.pid = spawn.pid;
    });

    let patch_preview: String = patch_text.chars().take(16_000).collect();
    let mode_title = if round == 1 { "Full Diff (Round 1)" } else { "Delta Diff" };
    let criteria_block = spec
        .criteria
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|c| format!("\nReview criteria:\n{c}\n"))
        .unwrap_or_default();
    let review_prompt = format!(
        "You are the read-only Drove Reviewer for task `{}` (`{}`).\n\
         Workspace: `{work_cwd}` (READONLY — do NOT modify files).\n\
         Target: {}\nAcceptance: {}\n{criteria_block}\n\
         {mode_title}:\n```diff\n{patch_preview}\n```\n\n\
         Reply ONLY with a single JSON object (no other prose if possible):\n\
         {{\"verdict\":\"approve\"|\"changes_requested\",\"summary\":\"...\",\"findings\":[{{\"severity\":\"high\"|\"medium\",\"file\":\"...\",\"message\":\"...\"}}]}}",
        task.key, task.title, task.spec.target, task.spec.acceptance
    );

    let prev_turn = core
        .lock()
        .reg
        .agents
        .get(&spawn.agent_id)
        .map(|a| a.turn)
        .unwrap_or(0);
    if let Err(e) = core.send(spawn.agent_id, &review_prompt, false, None, Some("drove")).await {
        core.kill(&[spawn.agent_id]).await;
        return Err(ApiErr::new(500, "review_send_failed", e.message));
    }

    // Wait for reviewer's single ACP turn to finish.
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let done = {
            let st = core.lock();
            match st.reg.agents.get(&spawn.agent_id) {
                None => true,
                Some(a) => !a.is_live() || (a.turn > prev_turn && a.status.as_ref().is_none_or(|s| s.state != "working")),
            }
        };
        if done || Instant::now() >= deadline {
            break;
        }
        let notified = core.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let _ = tokio::time::timeout(Duration::from_millis(400), notified).await;
    }

    // Collect assistant output from reviewer transcript & close ephemeral reviewer session.
    let assistant_text = collect_assistant_text(core, spawn.agent_id, prev_turn + 1);
    let _ = core.drove.store.update_worker(&rev_worker_id, |w| w.state = WorkerState::Closed);
    core.kill(&[spawn.agent_id]).await;

    let parsed = parse_review_verdict(&assistant_text);
    let _ = core.drove.store.update_attempt(&rev_att_id, |a| {
        a.outcome = Some(Outcome::Succeeded);
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
        return Ok(());
    }

    let findings_txt = parsed
        .findings
        .as_ref()
        .map(|fs| {
            fs.iter()
                .map(|f| format!("- [{}] {}: {}", f.severity.as_deref().unwrap_or("info"), f.file.as_deref().unwrap_or("*"), f.message))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let reject_msg = if findings_txt.is_empty() {
        format!("Review round {round}/{max_rounds} requested changes: {}", parsed.summary)
    } else {
        format!("Review round {round}/{max_rounds} requested changes: {}\n{findings_txt}", parsed.summary)
    };

    if round >= max_rounds {
        let _ = core.drove.store.update_attempt(&attempt.id, |a| {
            a.outcome = Some(Outcome::Failed);
            a.ended_at = Some(now_iso());
            a.failure = Some(AttemptFailure { kind: FailureKind::ReviewRejected, message: reject_msg.clone() });
        });
        mark_worker_idle(core, &author_worker.id);
        super::fail_or_retry_task(
            core,
            &run.id,
            &task.id,
            &attempt.id,
            FailureKind::ReviewRejected,
            &reject_msg,
            0,
        )
        .await;
        let _ = MAX_AUTO_RETRIES;
        notify_coordinator(
            core,
            run,
            "review_rejected",
            &format!("Task `{}` review rejected ({round}/{max_rounds})", task.key),
            &format!("[drove] Review rejected for `{}`: {}", task.key, parsed.summary),
            Some(&task.id),
            Some(&attempt.id),
            Some(&author_worker.id),
            None,
        )
        .await;
        return Err(ApiErr::new(422, "review_rejected", reject_msg));
    }

    Err(ApiErr::new(422, "review_changes_requested", reject_msg))
}

fn collect_assistant_text(core: &Core, agent_id: u32, turn: u64) -> String {
    let Ok(entries) = core.read_transcript_entries(agent_id) else {
        return String::new();
    };
    let mut parts = Vec::new();
    for e in entries {
        if let Entry::Assistant { turn: t, text, .. } = e
            && t >= turn
        {
            parts.push(text);
        }
    }
    parts.join("")
}

/// Parses a `ReviewVerdict` JSON object even when surrounded by markdown fences or prose.
pub fn parse_review_verdict(raw: &str) -> ReviewVerdict {
    if let Ok(v) = serde_json::from_str::<ReviewVerdict>(raw.trim()) {
        return v;
    }
    for (idx, _) in raw.match_indices('{') {
        let mut de = serde_json::Deserializer::from_str(&raw[idx..]).into_iter::<ReviewVerdict>();
        if let Some(Ok(v)) = de.next()
            && matches!(v.verdict.as_str(), "approve" | "approved" | "changes_requested" | "reject" | "rejected")
            && v.summary != "..."
        {
            return v;
        }
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
    if Command::new("git")
        .args(["-C", work_cwd, "rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|o| o.status.success())
    {
        let _ = Command::new("git").args(["-C", work_cwd, "add", "-N", "."]).output();
        if let Ok(out) = Command::new("git").args(["-C", work_cwd, "diff", "HEAD"]).output()
            && out.status.success()
        {
            return String::from_utf8_lossy(&out.stdout).to_string();
        }
    }
    let touched = diff_touched_paths(work_cwd, baseline);
    if touched.is_empty() {
        "No workspace changes\n".into()
    } else {
        format!("Modified files:\n{}\n", touched.join("\n"))
    }
}

fn git_tree_sha(work_cwd: &str) -> Option<String> {
    let git_dir_out = Command::new("git")
        .args(["-C", work_cwd, "rev-parse", "--git-dir"])
        .output()
        .ok()?;
    if !git_dir_out.status.success() {
        return None;
    }
    let idx_path = std::env::temp_dir().join(format!("drove-idx-{}", super::crypto::rand_id("t")));
    let _ = std::fs::remove_file(&idx_path);
    let idx_str = idx_path.display().to_string();
    let _ = Command::new("git")
        .args(["-C", work_cwd, "read-tree", "HEAD"])
        .env("GIT_INDEX_FILE", &idx_str)
        .output();
    let _ = Command::new("git")
        .args(["-C", work_cwd, "add", "-A"])
        .env("GIT_INDEX_FILE", &idx_str)
        .output();
    let out = Command::new("git")
        .args(["-C", work_cwd, "write-tree"])
        .env("GIT_INDEX_FILE", &idx_str)
        .output()
        .ok();
    let _ = std::fs::remove_file(&idx_path);
    out.filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn git_diff_trees(work_cwd: &str, a: &str, b: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", work_cwd, "diff", a, b])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn resolve_local_parent_addr(core: &Core, host_session: &str) -> Option<String> {
    let local = host_session.rsplit_once('/').map(|(_, r)| r).unwrap_or(host_session);
    let tk = crate::runtime::TargetKey::parse(local)?;
    let st = core.lock();
    match tk {
        crate::runtime::TargetKey::Terminal(id) => st.pty_reg.terminals.get(&id).filter(|t| t.is_live()).map(|_| crate::terminals::address(id)),
        crate::runtime::TargetKey::Agent(id) => st.reg.agents.get(&id).filter(|a| a.is_live()).map(|_| crate::agents::address(id)),
    }
}
