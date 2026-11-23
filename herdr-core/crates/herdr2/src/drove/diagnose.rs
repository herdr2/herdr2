//! Structured task/attempt diagnostics (`drove inspect` & `/api/tasks/:id/inspect`).

use super::store::Store;
use super::types::*;
use std::collections::BTreeSet;
use std::process::Command;

/// Inspects a task (or specific attempt id) cleanly without altering workspace state.
pub fn diagnose_task(store: &Store, id_or_attempt: &str) -> Option<Diagnosis> {
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
        Some(dir) => match Command::new("git").args(["status", "--porcelain"]).current_dir(dir).output() {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim_end().to_string(),
            Ok(o) => format!("git status exited {:?}: {}", o.status.code(), String::from_utf8_lossy(&o.stderr).trim()),
            Err(e) => format!("unable to run git status: {e}"),
        },
        None => "workspace unavailable".into(),
    };

    let last_items = match &attempt {
        Some(a) => store.list_items(&a.worker_id, Some(&a.id), None, 30),
        None => Vec::new(),
    };

    let mut files_set = BTreeSet::new();
    if let Some(files) = task.result.as_ref().and_then(|r| r.files_modified.as_ref()) {
        for f in files {
            files_set.insert(f.clone());
        }
    }

    let failure = attempt.as_ref().and_then(|a| a.failure.clone());
    let suggestion = match failure.as_ref().map(|f| f.kind) {
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
    .to_string();

    Some(Diagnosis {
        task,
        attempts: attempts.clone(),
        route_reason: attempt.as_ref().map(|a| a.route_reason.clone()),
        ai: attempt.as_ref().and_then(|a| a.ai_diagnosis.clone()),
        attempt,
        failure,
        last_items,
        files_modified: files_set.into_iter().collect(),
        git_status,
        suggestion,
    })
}
