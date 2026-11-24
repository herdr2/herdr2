//! Task `spec.verify` and Run-level `run.verify` gates with artifact logs and stagnant-failure caps.

use super::crypto::sha256_hex;
use super::http::ApiErr;
use super::notify::notify_coordinator;
use super::pool::mark_worker_idle;
use super::store::NewDaemonEvent;
use super::types::*;
use crate::runtime::Core;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug)]
pub struct CommandRunOutput {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub combined_output: String,
    pub output_hash: String,
}

/// Executes a shell verification command with timeout and returns combined output + SHA-256 hash.
pub async fn run_verify_command(cwd: &str, cmd_str: &str, timeout_sec: u64) -> CommandRunOutput {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = super::bg_tokio_command("cmd");
        c.args(["/C", cmd_str]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = super::bg_tokio_command("sh");
        c.args(["-c", cmd_str]);
        c
    };
    cmd.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());

    match tokio::time::timeout(Duration::from_secs(timeout_sec.clamp(1, 3600)), cmd.output()).await {
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let combined = match (stdout.trim(), stderr.trim()) {
                ("", "") => String::new(),
                (o, "") => o.to_string(),
                ("", e) => e.to_string(),
                (o, e) => format!("{o}\n{e}"),
            };
            let output_hash = sha256_hex(combined.as_bytes());
            CommandRunOutput {
                success: out.status.success(),
                exit_code: out.status.code(),
                timed_out: false,
                combined_output: combined,
                output_hash,
            }
        }
        Ok(Err(e)) => {
            let msg = format!("failed to spawn verify command: {e}");
            let output_hash = sha256_hex(msg.as_bytes());
            CommandRunOutput { success: false, exit_code: None, timed_out: false, combined_output: msg, output_hash }
        }
        Err(_) => {
            let msg = format!("verify command timed out after {timeout_sec}s");
            let output_hash = sha256_hex(msg.as_bytes());
            CommandRunOutput { success: false, exit_code: None, timed_out: true, combined_output: msg, output_hash }
        }
    }
}

fn tail_lines(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let mut idx = text.len() - max_chars;
    while !text.is_char_boundary(idx) {
        idx += 1;
    }
    format!("...\n{}", &text[idx..])
}

/// Runs task-level `spec.verify` inside `work_cwd`, writing full output to artifacts.
pub(super) async fn run_task_verify_gate(
    core: &Arc<Core>,
    run: &Run,
    task: &Task,
    worker: &Worker,
    attempt: &Attempt,
    work_cwd: &str,
) -> Result<Option<String>, ApiErr> {
    let Some(vspec) = &task.spec.verify else {
        return Ok(None);
    };
    let (cmd_str, timeout_sec, max_rej, max_stag) = match vspec {
        VerifySpec::Cmd(s) => (s.as_str(), 300u64, 5u32, 3u32),
        VerifySpec::Obj(o) => (
            o.cmd.as_str(),
            o.timeout_sec.unwrap_or(300),
            o.max_rejections.unwrap_or(5),
            o.max_stagnant_rejections.unwrap_or(3),
        ),
    };
    if cmd_str.trim().is_empty() {
        return Ok(None);
    }

    let _ = core.drove.store.update_task(&task.id, |t| t.exec_phase = Some(TaskPhase::Verifying));
    let next_n = attempt.verify_rejections.unwrap_or(0) + 1;
    let artifact_id = format!("verify-{}-{next_n}", attempt.id);
    let res = run_verify_command(work_cwd, cmd_str, timeout_sec).await;
    let log_body = format!(
        "$ {cmd_str}\ncwd: {work_cwd}\nexit: {:?} timedOut: {}\n\n{}\n",
        res.exit_code, res.timed_out, res.combined_output
    );
    let _ = core.drove.store.write_artifact(&run.id, &artifact_id, log_body.as_bytes());

    if res.success {
        let _ = core.drove.store.update_attempt(&attempt.id, |a| {
            a.verify_log_artifact_id = Some(artifact_id.clone());
        });
        return Ok(Some(artifact_id));
    }

    let prev_hash = attempt.last_verify_hash.as_deref();
    let next_stag =
        if prev_hash == Some(res.output_hash.as_str()) { attempt.stagnant_rejections.unwrap_or(0) + 1 } else { 1 };
    let tail = tail_lines(&res.combined_output, 1800);
    let _ = core.drove.store.update_attempt(&attempt.id, |a| {
        a.verify_rejections = Some(next_n);
        a.stagnant_rejections = Some(next_stag);
        a.last_verify_hash = Some(res.output_hash.clone());
        a.verify_log_artifact_id = Some(artifact_id.clone());
    });
    let _ = core.drove.store.update_task(&task.id, |t| t.exec_phase = Some(TaskPhase::Rework));
    core.drove.store.append_daemon_event(NewDaemonEvent {
        run_id: Some(run.id.clone()),
        task_id: Some(task.id.clone()),
        attempt_id: Some(attempt.id.clone()),
        worker_id: Some(worker.id.clone()),
        message_id: None,
        category: "verify".into(),
        kind: "task_verify".into(),
        status: "failed".into(),
        title: format!("Task `{}` verify failed (#{next_n})", task.key),
        detail: Some(tail.clone()),
        duration_ms: None,
        tokens: None,
        cost: None,
    });
    super::ai::spawn_verify_digest(core.clone(), run.id.clone(), attempt.id.clone(), cmd_str.to_string(), tail.clone());

    let msg = format!(
        "Verification failed (attempt {next_n}/{max_rej}, stagnant {next_stag}/{max_stag}): `{cmd_str}`\n{tail}"
    );
    if next_n >= max_rej || next_stag >= max_stag {
        let _ = core.drove.store.update_attempt(&attempt.id, |a| {
            a.outcome = Some(Outcome::Failed);
            a.ended_at = Some(now_iso());
            a.failure = Some(AttemptFailure { kind: FailureKind::VerificationFailed, message: msg.clone() });
        });
        mark_worker_idle(core, &worker.id);
        super::fail_or_retry_task(core, &run.id, &task.id, &attempt.id, FailureKind::VerificationFailed, &msg, 0).await;
        return Err(ApiErr::new(422, "verify_failed_exhausted", msg));
    }

    Err(ApiErr::new(422, "verify_failed", msg))
}

/// Runs Run-level `run.verify` during `complete_run_http`. Returns `Ok(None)` when skipped/passed,
/// `Ok(Some(repair_task))` when `goal.autoRepair` spawned a repair task, or `Err(ApiErr)` on hard failure.
pub(super) async fn run_level_verify_gate(
    core: &Arc<Core>,
    run: &Run,
) -> Result<(Option<String>, Option<Task>), ApiErr> {
    let Some(vspec) = &run.verify else {
        return Ok((None, None));
    };
    let (cmd_str, timeout_sec) = match vspec {
        VerifySpec::Cmd(s) => (s.as_str(), 300u64),
        VerifySpec::Obj(o) => (o.cmd.as_str(), o.timeout_sec.unwrap_or(300)),
    };
    if cmd_str.trim().is_empty() {
        return Ok((None, None));
    }

    let generation = run.finalization.as_ref().map(|f| f.generation + 1).unwrap_or(1);
    let artifact_id = format!("verify-run-{}-{generation}", run.id);
    let res = run_verify_command(&run.cwd, cmd_str, timeout_sec).await;
    let log_body = format!(
        "$ {cmd_str}\ncwd: {}\nexit: {:?} timedOut: {}\n\n{}\n",
        run.cwd, res.exit_code, res.timed_out, res.combined_output
    );
    let _ = core.drove.store.write_artifact(&run.id, &artifact_id, log_body.as_bytes());

    if res.success {
        return Ok((Some(artifact_id), None));
    }

    let tail = tail_lines(&res.combined_output, 1800);
    let used = run.finalization.as_ref().map(|f| f.repairs_used).unwrap_or(0);
    let auto = run.goal.as_ref().and_then(|g| g.auto_repair).unwrap_or(false);
    let max_repairs = run.goal.as_ref().and_then(|g| g.max_rounds).unwrap_or(2);

    if auto && used < max_repairs {
        let next_repairs = used + 1;
        let repair_task = core.drove.store.create_task(super::store::CreateTaskInput {
            run_id: run.id.clone(),
            key: format!("repair-r{next_repairs}"),
            title: format!("Auto-repair run verify failure (round {next_repairs})"),
            phase: Some("repair".into()),
            spec: Spec {
                target: run.cwd.clone(),
                change: format!(
                    "The run-level verify command `{cmd_str}` failed during completion. \
                     Diagnose and fix the root cause shown below, then re-verify cleanly.\n\n{tail}"
                ),
                constraints: None,
                ownership: None,
                acceptance: format!("`{cmd_str}` passes cleanly"),
                verify: Some(VerifySpec::Cmd(cmd_str.to_string())),
            },
            tags: vec!["auto-repair".into()],
            files: Vec::new(),
            deps: Vec::new(),
            output_schema: None,
            isolation: Isolation::None,
            timeout_sec: Some(timeout_sec),
            stall_warn_sec: None,
            stall_kill_sec: None,
            when: None,
            assign: Some(Assign { role: Some("coder".into()), ..Default::default() }),
            status: TaskStatus::Ready,
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
        let _ = core.drove.store.update_run(&run.id, |r| {
            r.status = RunStatus::Active;
            r.finalization = Some(RunFinalization {
                state: "repairing".into(),
                generation,
                tree_sha: None,
                verified_tree_sha: None,
                command_id: None,
                repairs_used: next_repairs,
                last_failure: Some(tail.clone()),
                log_artifact_id: Some(artifact_id.clone()),
            });
        });
        core.drove.wake_scheduler();
        notify_coordinator(
            core,
            run,
            "auto_repair",
            "Run verify failed — auto-repair spawned",
            &format!("[drove] Run verify failed; auto-repair task `{}` dispatched", repair_task.key),
            Some(&repair_task.id),
            None,
            None,
            None,
        )
        .await;
        return Ok((Some(artifact_id), Some(repair_task)));
    }

    let _ = core.drove.store.update_run(&run.id, |r| {
        r.finalization = Some(RunFinalization {
            state: "verify_failed".into(),
            generation,
            tree_sha: None,
            verified_tree_sha: None,
            command_id: None,
            repairs_used: used,
            last_failure: Some(tail.clone()),
            log_artifact_id: Some(artifact_id.clone()),
        });
    });
    Err(ApiErr::new(422, "run_verify_failed", format!("Run verification failed: `{cmd_str}`\n{tail}")))
}
