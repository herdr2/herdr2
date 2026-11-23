//! Adversarial read-only reviewer loop (Full Diff -> R1, Delta Diff -> R2..Rn).

use super::MAX_AUTO_RETRIES;
use super::http::ApiErr;
use super::notify::notify_coordinator;
use super::pool::mark_worker_idle;
use super::roles::diff_touched_paths;
use super::store::NewDaemonEvent;
use super::types::*;
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
    let patch_text =
        capture_candidate_patch(work_cwd, &core.drove.get_attempt_baseline(&attempt.id).unwrap_or_default());
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
                    isolate_secrets: true,
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
        return Ok(());
    }

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
        super::fail_or_retry_task(core, &run.id, &task.id, &attempt.id, FailureKind::ReviewRejected, &reject_msg, 0)
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

const DIFF_CACHE_EXCLUDES: [&str; 6] = [
    ":(exclude)**/__pycache__/**",
    ":(exclude)**/*.pyc",
    ":(exclude)**/*.pyo",
    ":(exclude)**/.pytest_cache/**",
    ":(exclude)**/.mypy_cache/**",
    ":(exclude)**/.ruff_cache/**",
];

fn capture_candidate_patch(work_cwd: &str, baseline: &super::roles::DirtyBaseline) -> String {
    if Command::new("git")
        .args(["-C", work_cwd, "rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|o| o.status.success())
    {
        let mut add_cmd = Command::new("git");
        add_cmd.args(["-C", work_cwd, "add", "-N", "--", "."]);
        add_cmd.args(DIFF_CACHE_EXCLUDES);
        let _ = add_cmd.output();

        let mut diff_cmd = Command::new("git");
        diff_cmd.args(["-C", work_cwd, "diff", "HEAD", "--", "."]);
        diff_cmd.args(DIFF_CACHE_EXCLUDES);
        if let Ok(out) = diff_cmd.output()
            && out.status.success()
        {
            return String::from_utf8_lossy(&out.stdout).to_string();
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
    let git_dir_out = Command::new("git").args(["-C", work_cwd, "rev-parse", "--git-dir"]).output().ok()?;
    if !git_dir_out.status.success() {
        return None;
    }
    let idx_path = std::env::temp_dir().join(format!("drove-idx-{}", super::crypto::rand_id("t")));
    let _ = std::fs::remove_file(&idx_path);
    let idx_str = idx_path.display().to_string();
    let _ = Command::new("git").args(["-C", work_cwd, "read-tree", "HEAD"]).env("GIT_INDEX_FILE", &idx_str).output();
    let mut add_cmd = Command::new("git");
    add_cmd.args(["-C", work_cwd, "add", "-A", "--", "."]);
    add_cmd.args(DIFF_CACHE_EXCLUDES);
    add_cmd.env("GIT_INDEX_FILE", &idx_str);
    let _ = add_cmd.output();
    let out = Command::new("git").args(["-C", work_cwd, "write-tree"]).env("GIT_INDEX_FILE", &idx_str).output().ok();
    let _ = std::fs::remove_file(&idx_path);
    out.filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn git_diff_trees(work_cwd: &str, a: &str, b: &str) -> Option<String> {
    let mut diff_cmd = Command::new("git");
    diff_cmd.args(["-C", work_cwd, "diff", a, b, "--", "."]);
    diff_cmd.args(DIFF_CACHE_EXCLUDES);
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
            agent: None,
            model: None,
            effort: None,
            fast: None,
            different_agent: Some(true),
            criteria: None,
            max_rounds: Some(1),
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
            &ReviewSpec {
                agent: None,
                model: None,
                effort: None,
                fast: None,
                different_agent: None,
                criteria: None,
                max_rounds: Some(1),
            },
            "claude",
            None,
            &[],
            &[failed_att],
        );
        assert_eq!(cands_after_fail[0].agent, "agy");
        assert_eq!(cands_after_fail[1].agent, "codex");
    }
}
