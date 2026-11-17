//! Coordinator wakeups (`pty_send` / `Core::send`) and Drove daemon event emission.

use super::store::NewDaemonEvent;
use super::types::*;
use crate::runtime::{Core, TargetKey};
use std::sync::Arc;

/// Sends a concise, non-spammy wakeup notification to the Run's coordinator session (`t*` or `a*`)
/// and records a matching `DaemonEvent`.
pub async fn notify_coordinator(
    core: &Arc<Core>,
    run: &Run,
    kind: &str,
    title: &str,
    summary_line: &str,
    task_id: Option<&str>,
    attempt_id: Option<&str>,
    worker_id: Option<&str>,
    message_id: Option<&str>,
) {
    let status = deliver_to_target(core, &run.coordinator.host_session, summary_line).await;
    core.drove.store.append_daemon_event(NewDaemonEvent {
        run_id: Some(run.id.clone()),
        task_id: task_id.map(String::from),
        attempt_id: attempt_id.map(String::from),
        worker_id: worker_id.map(String::from),
        message_id: message_id.map(String::from),
        category: "notify".into(),
        kind: kind.into(),
        status,
        title: title.into(),
        detail: Some(summary_line.into()),
        duration_ms: None,
        tokens: None,
        cost: None,
    });
}

async fn deliver_to_target(core: &Arc<Core>, session: &str, line: &str) -> String {
    let local_part = match session.rsplit_once('/') {
        Some((_, rest)) => rest,
        None => session,
    };
    let Some(target) = TargetKey::parse(local_part) else {
        return "skipped".into();
    };
    match target {
        TargetKey::Terminal(tid) => {
            let live = core
                .lock()
                .pty_reg
                .terminals
                .get(&tid)
                .is_some_and(|t| t.is_live());
            if !live {
                return "skipped".into();
            }
            match core.pty_send(tid, line, false, Some("idle")).await {
                Ok(_) => "ok".into(),
                Err(e) if matches!(e.kind.as_str(), "status_mismatch" | "blocked") => {
                    // Retry unconditionally without idle filter only when safe? Better: leave to next wait/inbox check.
                    "deferred".into()
                }
                Err(_) => "failed".into(),
            }
        }
        TargetKey::Agent(aid) => {
            let live = core
                .lock()
                .reg
                .agents
                .get(&aid)
                .is_some_and(|a| a.is_live());
            if !live {
                return "skipped".into();
            }
            match core.send(aid, line, false, None, Some("drove")).await {
                Ok(_) => "ok".into(),
                Err(_) => "failed".into(),
            }
        }
    }
}
