//! Worktree isolation & per-run serialized merge-back (Git + AOSP `./bin/harness`).

use super::http::ApiErr;
use super::types::*;
use crate::runtime::Core;
use std::path::Path;
use std::sync::Arc;

pub fn is_aosp_harness(cwd: &str) -> bool {
    Path::new(cwd).join("bin/harness").is_file()
}

pub fn is_git_repo(cwd: &str) -> bool {
    super::bg_std_command("git")
        .args(["-C", cwd, "rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Allocates an isolated worktree for `task` when `task.isolation == Isolation::Worktree`.
pub async fn prepare_task_worktree(
    core: &Arc<Core>,
    run: &Run,
    task: &Task,
) -> Result<(String, Option<String>), String> {
    if task.isolation != Isolation::Worktree {
        return Ok((run.cwd.clone(), None));
    }

    let run_cwd = run.cwd.clone();
    let safe_key: String = task
        .key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let wt_dir = core
        .drove
        .paths
        .state_dir
        .join("worktrees")
        .join(&run.id)
        .join(format!("{safe_key}-{}", super::crypto::rand_id("wt")));

    if let Some(parent) = wt_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir worktree parent: {e}"))?;
    }

    tokio::task::spawn_blocking(move || {
        if is_aosp_harness(&run_cwd) {
            let bin = Path::new(&run_cwd).join("bin/harness");
            let out = super::bg_std_command(bin)
                .args(["worktree", "add", &wt_dir.display().to_string()])
                .current_dir(&run_cwd)
                .output()
                .map_err(|e| format!("spawn harness worktree add: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "harness worktree add failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            let s = wt_dir.display().to_string();
            return Ok((s.clone(), Some(s)));
        }

        if !is_git_repo(&run_cwd) {
            return Err(format!("worktree isolation requested, but {} is neither a git repo nor an AOSP harness workspace", run_cwd));
        }
        let head_ok = super::bg_std_command("git")
            .args(["-C", &run_cwd, "rev-parse", "--verify", "HEAD"])
            .output()
            .is_ok_and(|o| o.status.success());
        if !head_ok {
            return Err("git repository has no HEAD commit yet for worktree creation".into());
        }

        let wt_str = wt_dir.display().to_string();
        let out = super::bg_std_command("git")
            .args(["-C", &run_cwd, "worktree", "add", "--detach", &wt_str, "HEAD"])
            .output()
            .map_err(|e| format!("spawn git worktree add: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git worktree add failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok((wt_str.clone(), Some(wt_str)))
    })
    .await
    .map_err(|e| format!("worktree task join error: {e}"))?
}

/// Serializes merging worktree changes back into `run.cwd` and removes the temporary worktree.
pub(super) async fn merge_and_cleanup_worktree(
    core: &Arc<Core>,
    run: &Run,
    worktree_dir: Option<&str>,
) -> Result<Vec<String>, ApiErr> {
    let Some(wt) = worktree_dir.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(Vec::new());
    };
    let lock = core.drove.run_merge_lock(&run.id);
    let _guard = lock.lock().await;
    let run_cwd = run.cwd.clone();
    let wt_str = wt.to_string();

    tokio::task::spawn_blocking(move || merge_worktree_blocking(&run_cwd, &wt_str))
        .await
        .map_err(|e| ApiErr::new(500, "internal", e.to_string()))?
}

fn merge_worktree_blocking(run_cwd: &str, wt_dir: &str) -> Result<Vec<String>, ApiErr> {
    if is_aosp_harness(run_cwd) {
        let bin = Path::new(run_cwd).join("bin/harness");
        let merge_out = super::bg_std_command(&bin)
            .args(["worktree", "merge", wt_dir])
            .current_dir(run_cwd)
            .output()
            .map_err(|e| ApiErr::new(500, "internal", format!("harness merge failed: {e}")))?;
        if !merge_out.status.success() {
            return Err(ApiErr::new(
                422,
                "merge_conflict",
                format!(
                    "Harness worktree merge failed: {}",
                    String::from_utf8_lossy(&merge_out.stderr).trim()
                ),
            ));
        }
        let _ = super::bg_std_command(bin)
            .args(["worktree", "remove", wt_dir])
            .current_dir(run_cwd)
            .output();
        return Ok(Vec::new());
    }

    // Stage all edits + new files inside worktree index (sharing object store with run_cwd)
    // so `git diff --cached --binary HEAD` preserves exact blob SHAs for clean `--3way` apply.
    let add_out = super::bg_std_command("git")
        .args(["-C", wt_dir, "add", "-A"])
        .output()
        .map_err(|e| ApiErr::new(500, "internal", format!("git add in worktree: {e}")))?;
    if !add_out.status.success() {
        return Err(ApiErr::new(
            500,
            "internal",
            format!("git add failed: {}", String::from_utf8_lossy(&add_out.stderr).trim()),
        ));
    }

    let names_out = super::bg_std_command("git")
        .args(["-C", wt_dir, "diff", "--cached", "--name-only", "HEAD"])
        .output()
        .map_err(|e| ApiErr::new(500, "internal", e.to_string()))?;
    let changed_files: Vec<String> = String::from_utf8_lossy(&names_out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if !changed_files.is_empty() {
        let patch_out = super::bg_std_command("git")
            .args(["-C", wt_dir, "diff", "--cached", "--binary", "HEAD"])
            .output()
            .map_err(|e| ApiErr::new(500, "internal", format!("git diff in worktree: {e}")))?;
        if !patch_out.status.success() {
            return Err(ApiErr::new(
                500,
                "internal",
                format!("git diff failed: {}", String::from_utf8_lossy(&patch_out.stderr).trim()),
            ));
        }

        let mut apply_child = super::bg_std_command("git")
            .args(["-C", run_cwd, "apply", "--3way", "--whitespace=nowarn"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| ApiErr::new(500, "internal", format!("git apply spawn: {e}")))?;
        if let Some(mut stdin) = apply_child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(&patch_out.stdout);
        }
        let apply_out = apply_child
            .wait_with_output()
            .map_err(|e| ApiErr::new(500, "internal", format!("git apply wait: {e}")))?;
        if !apply_out.status.success() {
            return Err(ApiErr::new(
                422,
                "merge_conflict",
                format!(
                    "Merge conflict integrating worktree `{wt_dir}` into `{run_cwd}`: {}",
                    String::from_utf8_lossy(&apply_out.stderr).trim()
                ),
            ));
        }
    }

    cleanup_worktree_dir(run_cwd, Path::new(wt_dir));
    Ok(changed_files)
}

fn cleanup_worktree_dir(run_cwd: &str, wt_dir: &Path) {
    let _ = super::bg_std_command("git")
        .args(["-C", run_cwd, "worktree", "remove", "--force", &wt_dir.display().to_string()])
        .output();
    if wt_dir.exists() {
        let _ = std::fs::remove_dir_all(wt_dir);
    }
}
