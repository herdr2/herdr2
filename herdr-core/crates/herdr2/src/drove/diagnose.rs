//! Structured task/attempt diagnostics (`drove inspect` & `/api/tasks/:id/inspect`).

use super::store::Store;
use super::types::*;
use std::collections::{BTreeSet, HashMap};

/// Inspects a task (or specific attempt id) cleanly without altering workspace state.
pub fn diagnose_task(store: &Store, id_or_attempt: &str) -> Option<Diagnosis> {
    diagnose_task_with_tail(store, id_or_attempt, 30)
}

/// Inspects a task (or specific attempt id) returning up to `tail` coalesced recent activity items.
pub fn diagnose_task_with_tail(store: &Store, id_or_attempt: &str, tail: usize) -> Option<Diagnosis> {
    let tail = tail.clamp(1, 200);
    let selected = store.get_attempt(id_or_attempt);
    let task = match &selected {
        Some(a) => store.get_task(&a.task_id)?,
        None => store.get_task(id_or_attempt)?,
    };
    let attempts = store.list_attempts(None, Some(&task.id), None);
    let latest = attempts.last().cloned();
    let attempt = selected.or_else(|| {
        if latest.as_ref().is_some_and(|a| a.failure.is_some()) || task.status != TaskStatus::Failed {
            latest.clone()
        } else {
            attempts.iter().rev().find(|a| a.failure.is_some()).cloned().or(latest)
        }
    });

    let worker = attempt.as_ref().and_then(|a| store.get_worker(&a.worker_id));
    let run_cwd = store.get_run(&task.run_id).map(|r| r.cwd);
    let git_cwd = worker.as_ref().and_then(|w| w.worktree.as_ref().or(Some(&w.cwd))).or(run_cwd.as_ref());

    let git_status = match git_cwd {
        Some(dir) => match super::bg_std_command("git").args(["status", "--porcelain"]).current_dir(dir).output() {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim_end().to_string(),
            Ok(o) => format!("git status exited {:?}: {}", o.status.code(), String::from_utf8_lossy(&o.stderr).trim()),
            Err(e) => format!("unable to run git status: {e}"),
        },
        None => "workspace unavailable".into(),
    };

    let fetch_limit = tail.saturating_mul(4).clamp(60, 400);
    let raw_items = match &attempt {
        Some(a) => store.list_items(&a.worker_id, Some(&a.id), None, fetch_limit),
        None => Vec::new(),
    };
    let last_items = coalesce_inspect_items(raw_items, tail);

    let mut declared_set = BTreeSet::new();
    if let Some(files) = task.result.as_ref().and_then(|r| r.files_modified.as_ref()) {
        for f in files {
            if !f.trim().is_empty() {
                declared_set.insert(f.trim().to_string());
            }
        }
    }
    let declared_files_modified: Vec<String> = declared_set.into_iter().collect();
    let live_dirty_files = parse_porcelain_paths(&git_status);
    let files_modified = if !declared_files_modified.is_empty() {
        declared_files_modified.clone()
    } else {
        live_dirty_files.clone()
    };

    let attempt_tokens = attempt.as_ref().map(|a| a.tokens.clone());
    let worker_total_tokens = worker.as_ref().map(|w| w.totals.tokens);

    let failure = attempt.as_ref().and_then(|a| a.failure.clone());
    let suggestion = if let Some(rc) = &task.review_circuit {
        match rc.stage {
            ReviewCircuitStage::HandoffToCoordinator => format!(
                "Review circuit breaker handed off to Coordinator after {} non-passing review round(s) (limit {}). Coordinator must take over and fix directly, then trigger ONLY ONE post-handoff final review via `h2 drove retry {}`.",
                rc.review_rounds, rc.max_normal_rounds, task.key
            ),
            ReviewCircuitStage::PostHandoffReview => format!(
                "Single post-handoff final review (1/1) is active after Coordinator takeover (prior non-passing rounds: {}). If this review still fails, Drove will engage the hard circuit breaker and require user decision.",
                rc.review_rounds
            ),
            ReviewCircuitStage::AwaitingUserDecision => format!(
                "HARD REVIEW CIRCUIT BREAKER OPEN ({} non-passing review rounds, post-handoff review failed): STOP retrying. Immediately report current status, unresolved divergence, impact, and your concrete recommendation to the user. After user approval, either run `h2 drove retry {} --user-override` or close the run with `h2 drove complete --force`.",
                rc.review_rounds, task.key
            ),
            ReviewCircuitStage::Normal | ReviewCircuitStage::Cleared => default_failure_suggestion(failure.as_ref()),
        }
    } else {
        default_failure_suggestion(failure.as_ref())
    };

    Some(Diagnosis {
        task,
        attempts: attempts.clone(),
        route_reason: attempt.as_ref().map(|a| a.route_reason.clone()),
        ai: attempt.as_ref().and_then(|a| a.ai_diagnosis.clone()),
        attempt,
        failure,
        last_items,
        files_modified,
        declared_files_modified,
        live_dirty_files,
        attempt_tokens,
        worker_total_tokens,
        git_status,
        suggestion,
    })
}

fn default_failure_suggestion(failure: Option<&AttemptFailure>) -> String {
    match failure.map(|f| f.kind) {
        Some(FailureKind::AgentError) => {
            "Inspect worker stderr/output; retry with --same-worker or switch agent/model via `drove route`."
        }
        Some(FailureKind::NoDone) => {
            "Worker finished turn without reporting completion; retry with `--hint \"Call drove worker done\"`."
        }
        Some(FailureKind::Timeout) => "Task stalled or timed out; increase timeoutSec or split into smaller tasks.",
        Some(FailureKind::Schema) => "Result JSON did not satisfy outputSchema; retry with schema hint.",
        Some(FailureKind::VerificationFailed) => {
            "Verification gate failed; check `artifacts/verify-*` or task logs before retrying."
        }
        Some(FailureKind::ReviewRejected) => {
            "Reviewer requested changes across max rounds; review findings and complete manually or adjust spec."
        }
        Some(FailureKind::MergeConflict) => {
            "Worktree merge conflicted; resolve conflicts in the workspace before continuing."
        }
        Some(FailureKind::ProcessExited) => "Worker process exited externally; retry cleanly.",
        Some(FailureKind::WorkerReported) => {
            "Worker reported explicit failure; check reason and adjust task spec or hint."
        }
        Some(FailureKind::Cancelled) => "Task was cancelled.",
        Some(FailureKind::QuotaExhausted) => {
            "Model quota/rate limit reached; switch role model or wait before retrying."
        }
        None => "No failure recorded; inspect worker transcript and task spec.",
    }
    .to_string()
}

/// Parses `git status --porcelain` lines into workspace-relative paths (skipping ephemeral caches).
pub(crate) fn parse_porcelain_paths(porcelain: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    for line in porcelain.lines() {
        if line.len() < 4 {
            continue;
        }
        let b = line.as_bytes();
        // Skip error strings like "git status exited ..." or "workspace unavailable".
        if !matches!(b[0], b' ' | b'M' | b'A' | b'D' | b'R' | b'C' | b'U' | b'?')
            || !matches!(b[1], b' ' | b'M' | b'A' | b'D' | b'R' | b'C' | b'U' | b'?')
            || b[2] != b' '
        {
            continue;
        }
        let raw_path = line[3..].trim();
        let target = raw_path.rsplit_once(" -> ").map(|(_, dst)| dst).unwrap_or(raw_path);
        let unquoted = target.trim_matches('"').trim();
        if unquoted.is_empty() || super::roles::is_ephemeral_cache_path(unquoted.trim_end_matches('/')) {
            continue;
        }
        out.insert(unquoted.to_string());
    }
    out.into_iter().collect()
}

/// Filters empty streaming `tool_call_update` shells and coalesces duplicate updates by `tool.id`,
/// preserving the earliest non-empty title/kind and latest non-empty status/output.
pub(crate) fn coalesce_inspect_items(items: Vec<Item>, tail: usize) -> Vec<Item> {
    let mut out: Vec<Option<Item>> = Vec::with_capacity(items.len());
    let mut by_tool_id: HashMap<String, usize> = HashMap::new();

    for mut item in items {
        if item.kind == "tool"
            && let Some(mut tl) = item.tool.take()
        {
            let title_empty = tl.title.as_deref().map(str::trim).unwrap_or("").is_empty();
            let status_empty = tl.status.as_deref().map(str::trim).unwrap_or("").is_empty();
            let output_empty = tl.output.as_deref().map(str::trim).unwrap_or("").is_empty();
            if title_empty && status_empty && output_empty {
                continue;
            }
            if !tl.id.is_empty()
                && let Some(&prev_idx) = by_tool_id.get(&tl.id)
                && let Some(prev_item) = out[prev_idx].take()
            {
                if let Some(prev_tl) = prev_item.tool {
                    if title_empty {
                        tl.title = prev_tl.title;
                    }
                    if tl.kind.is_none() {
                        tl.kind = prev_tl.kind;
                    }
                    if status_empty {
                        tl.status = prev_tl.status;
                    }
                    if output_empty {
                        tl.output = prev_tl.output;
                    }
                    if tl.locations.is_none() {
                        tl.locations = prev_tl.locations;
                    }
                    if tl.input.is_none() {
                        tl.input = prev_tl.input;
                    }
                }
                item.tool = Some(tl);
                let new_idx = out.len();
                if let Some(tid) = item.tool.as_ref().map(|t| t.id.clone()) {
                    by_tool_id.insert(tid, new_idx);
                }
                out.push(Some(item));
                continue;
            }
            if title_empty && status_empty {
                continue;
            }
            let tid = tl.id.clone();
            item.tool = Some(tl);
            let idx = out.len();
            if !tid.is_empty() {
                by_tool_id.insert(tid, idx);
            }
            out.push(Some(item));
        } else {
            out.push(Some(item));
        }
    }

    let mut clean: Vec<Item> = out.into_iter().flatten().collect();
    if clean.len() > tail {
        clean.drain(..clean.len() - tail);
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_tool(id: u64, tid: &str, title: &str, status: &str) -> Item {
        Item {
            id,
            worker_id: "wrk_1".into(),
            attempt_id: Some("att_1".into()),
            ts: now_iso(),
            kind: "tool".into(),
            text: None,
            tool: Some(ItemTool {
                id: tid.into(),
                title: Some(title.into()),
                kind: None,
                status: Some(status.into()),
                locations: None,
                input: None,
                output: None,
            }),
            usage: None,
        }
    }

    #[test]
    fn coalesce_inspect_items_drops_codex_empty_tool_shells_and_merges_by_id() {
        let items = vec![
            mk_tool(1, "exec-1", "pytest tests/unit", "in_progress"),
            mk_tool(2, "exec-1", "", ""),
            mk_tool(3, "exec-1", "", ""),
            mk_tool(4, "exec-1", "", "completed"),
            mk_tool(5, "exec-2", "", ""),
        ];
        let res = coalesce_inspect_items(items, 10);
        assert_eq!(res.len(), 1);
        let tl = res[0].tool.as_ref().unwrap();
        assert_eq!(tl.id, "exec-1");
        assert_eq!(tl.title.as_deref(), Some("pytest tests/unit"));
        assert_eq!(tl.status.as_deref(), Some("completed"));
    }

    #[test]
    fn parse_porcelain_paths_extracts_live_dirty_files_and_skips_caches() {
        let raw = " M src/cli.py\n?? tests/test_mlbb.py\nR  old.py -> new.py\n?? __pycache__/a.pyc\n";
        assert_eq!(parse_porcelain_paths(raw), vec!["new.py", "src/cli.py", "tests/test_mlbb.py"]);
        assert!(parse_porcelain_paths("workspace unavailable").is_empty());
    }

    #[test]
    fn diagnose_task_surfaces_stage_specific_review_circuit_suggestions() {
        let dir = std::env::temp_dir().join(format!("herdr2-diag-cb-{}", super::super::crypto::rand_id("d")));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = crate::paths::Paths {
            state: dir.clone(),
            config: dir.join("config.toml"),
            socket: dir.join("core.sock"),
            holder_socket: dir.join("core.holder.sock"),
        };
        let dpaths = super::super::config::DrovePaths::from_paths(&paths);
        let store = Store::open(&dpaths).unwrap();
        let run = store.create_run(super::super::store::CreateRunInput {
            objective: "diag circuit".into(),
            cwd: dir.display().to_string(),
            host: "herdr2".into(),
            host_session: "d1".into(),
            agent: None,
            model: None,
            host_pid: None,
            session_bound: None,
            max_concurrency: 1,
            budget: None,
            verify: None,
            goal: None,
        });
        let task = store.create_task(super::super::store::CreateTaskInput {
            run_id: run.id.clone(),
            key: "review-gate".into(),
            title: "Review Gate".into(),
            phase: None,
            spec: Spec {
                target: "workspace".into(),
                change: "review".into(),
                constraints: None,
                ownership: None,
                acceptance: "pass".into(),
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
            assign: None,
            status: TaskStatus::Failed,
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
        });

        let set_stage = |stage: ReviewCircuitStage, rounds: u32| {
            store.update_task(&task.id, |t| {
                t.review_circuit = Some(ReviewCircuit {
                    stage,
                    review_rounds: rounds,
                    max_normal_rounds: 2,
                    handoff_to_coordinator: stage != ReviewCircuitStage::Normal,
                    handoff_at: None,
                    post_handoff_review_used: matches!(
                        stage,
                        ReviewCircuitStage::PostHandoffReview | ReviewCircuitStage::AwaitingUserDecision
                    ),
                    awaiting_user_decision: stage == ReviewCircuitStage::AwaitingUserDecision,
                    tripped_at: None,
                    user_overrides: 0,
                    history: Vec::new(),
                });
            });
        };

        set_stage(ReviewCircuitStage::HandoffToCoordinator, 2);
        let d_handoff = diagnose_task(&store, &task.id).unwrap();
        assert!(d_handoff.suggestion.contains("Coordinator must take over"));
        assert!(d_handoff.suggestion.contains("h2 drove retry review-gate"));

        set_stage(ReviewCircuitStage::PostHandoffReview, 2);
        let d_post = diagnose_task(&store, &task.id).unwrap();
        assert!(d_post.suggestion.contains("Single post-handoff final review (1/1)"));

        set_stage(ReviewCircuitStage::AwaitingUserDecision, 3);
        let d_hard = diagnose_task(&store, &task.id).unwrap();
        assert!(d_hard.suggestion.contains("HARD REVIEW CIRCUIT BREAKER OPEN"));
        assert!(d_hard.suggestion.contains("--user-override"));
        assert!(d_hard.suggestion.contains("h2 drove complete --force"));

        set_stage(ReviewCircuitStage::Cleared, 3);
        let d_cleared = diagnose_task(&store, &task.id).unwrap();
        assert!(d_cleared.suggestion.contains("No failure recorded"));

        let _ = std::fs::remove_dir_all(dir);
    }
}
