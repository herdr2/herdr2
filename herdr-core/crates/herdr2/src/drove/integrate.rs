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

/// Allocates (or resets and reuses) an isolated worktree for `task` when `task.isolation == Isolation::Worktree`.
pub async fn prepare_task_worktree(
    core: &Arc<Core>,
    run: &Run,
    task: &Task,
    reuse_worktree: Option<&str>,
) -> Result<(String, Option<String>), String> {
    if task.isolation != Isolation::Worktree {
        return Ok((run.cwd.clone(), None));
    }

    let run_cwd = run.cwd.clone();
    let reuse_wt = reuse_worktree
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let repo_name: String = Path::new(&run_cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|name| {
            name.chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "repo".into());
    let wt_dir = core
        .drove
        .paths
        .state_dir
        .join("worktrees")
        .join(&run.id)
        .join(format!("{repo_name}-{}", super::crypto::rand_id("wt")));

    if let Some(parent) = wt_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir worktree parent: {e}"))?;
    }

    let lock = core.drove.run_merge_lock(&run.id);
    let _guard = lock.lock().await;
    let wt_parent = wt_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from(&run_cwd));

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

        let base_commit = snapshot_working_tree_commit(&run_cwd, &wt_parent)?;
        if let Some(existing) = reuse_wt.as_deref()
            && Path::new(existing).is_dir()
        {
            let reset_ok = reset_worktree_to_commit(existing, &base_commit);
            if reset_ok {
                return Ok((existing.to_string(), Some(existing.to_string())));
            }
            cleanup_worktree_dir(&run_cwd, Path::new(existing));
        }
        let wt_str = wt_dir.display().to_string();
        let out = super::bg_std_command("git")
            .args(["-C", &run_cwd, "worktree", "add", "--detach", &wt_str, &base_commit])
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

struct TempIndexGuard(std::path::PathBuf);

impl Drop for TempIndexGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn git_stdout_trimmed(run_cwd: &str, args: &[&str], idx_env: Option<&Path>) -> Result<String, String> {
    let mut cmd = super::bg_std_command("git");
    cmd.args(["-C", run_cwd]).args(args);
    if let Some(idx) = idx_env {
        cmd.env("GIT_INDEX_FILE", idx);
    }
    let out = cmd.output().map_err(|e| format!("spawn git {}: {e}", args.first().copied().unwrap_or("")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Captures uncommitted working-tree changes (excluding untracked ephemeral caches) as a dangling
/// commit parented on `HEAD` so downstream worktrees inherit upstream merged-back edits without
/// touching the user's `HEAD` or git index.
fn snapshot_working_tree_commit(run_cwd: &str, wt_parent: &Path) -> Result<String, String> {
    let head_sha = git_stdout_trimmed(run_cwd, &["rev-parse", "--verify", "HEAD"], None)?;
    let head_tree = git_stdout_trimmed(run_cwd, &["rev-parse", "--verify", "HEAD^{tree}"], None)?;

    let tmp_idx = wt_parent.join(format!(".drove-idx-{}", super::crypto::rand_id("idx")));
    let _guard = TempIndexGuard(tmp_idx.clone());

    git_stdout_trimmed(run_cwd, &["read-tree", "HEAD"], Some(&tmp_idx))?;
    git_stdout_trimmed(run_cwd, &["add", "-A"], Some(&tmp_idx))?;

    let added_out = super::bg_std_command("git")
        .args(["-C", run_cwd, "diff", "--cached", "--name-only", "--diff-filter=A", "-z", "HEAD"])
        .env("GIT_INDEX_FILE", &tmp_idx)
        .output()
        .map_err(|e| format!("spawn git diff: {e}"))?;
    if added_out.status.success() {
        let ephemeral: Vec<String> = added_out
            .stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_string())
            .filter(|rel| super::roles::is_ephemeral_cache_path(rel))
            .collect();
        for chunk in ephemeral.chunks(200) {
            let mut reset_cmd = super::bg_std_command("git");
            reset_cmd
                .args(["-C", run_cwd, "reset", "-q", "HEAD", "--"])
                .args(chunk)
                .env("GIT_INDEX_FILE", &tmp_idx);
            let _ = reset_cmd.output();
        }
    }

    let tree_sha = git_stdout_trimmed(run_cwd, &["write-tree"], Some(&tmp_idx))?;
    if tree_sha == head_tree {
        return Ok(head_sha);
    }

    let commit_out = super::bg_std_command("git")
        .args([
            "-C",
            run_cwd,
            "commit-tree",
            &tree_sha,
            "-p",
            &head_sha,
            "-m",
            "drove: worktree base snapshot",
        ])
        .env("GIT_AUTHOR_NAME", "drove")
        .env("GIT_AUTHOR_EMAIL", "drove@herdr2.local")
        .env("GIT_COMMITTER_NAME", "drove")
        .env("GIT_COMMITTER_EMAIL", "drove@herdr2.local")
        .output()
        .map_err(|e| format!("spawn git commit-tree: {e}"))?;
    if !commit_out.status.success() {
        return Err(format!(
            "git commit-tree failed: {}",
            String::from_utf8_lossy(&commit_out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&commit_out.stdout).trim().to_string())
}

/// Serializes merging worktree changes back into `run.cwd`, optionally keeping the reset worktree for warm reuse.
pub(super) async fn merge_and_finalize_worktree(
    core: &Arc<Core>,
    run: &Run,
    worktree_dir: Option<&str>,
    keep_for_reuse: bool,
) -> Result<Vec<String>, ApiErr> {
    let Some(wt) = worktree_dir.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(Vec::new());
    };
    let lock = core.drove.run_merge_lock(&run.id);
    let _guard = lock.lock().await;
    let run_cwd = run.cwd.clone();
    let wt_str = wt.to_string();

    tokio::task::spawn_blocking(move || merge_worktree_blocking(&run_cwd, &wt_str, keep_for_reuse))
        .await
        .map_err(|e| ApiErr::new(500, "internal", e.to_string()))?
}

fn merge_worktree_blocking(run_cwd: &str, wt_dir: &str, keep_for_reuse: bool) -> Result<Vec<String>, ApiErr> {
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

    // Purge untracked ephemeral caches (e.g. __pycache__, .pyc) before `git add -A` so
    // verify gates don't bundle binary caches into the merge-back patch.
    purge_untracked_ephemeral_caches(wt_dir);

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

    if keep_for_reuse {
        reset_worktree_clean(wt_dir);
    } else {
        cleanup_worktree_dir(run_cwd, Path::new(wt_dir));
    }
    Ok(changed_files)
}

pub(super) fn reset_worktree_clean(wt_dir: &str) {
    let _ = super::bg_std_command("git")
        .args(["-C", wt_dir, "reset", "-q", "HEAD"])
        .output();
    let _ = super::bg_std_command("git")
        .args(["-C", wt_dir, "checkout", "-f", "--", "."])
        .output();
    let _ = super::bg_std_command("git")
        .args(["-C", wt_dir, "clean", "-fdx"])
        .output();
}

fn reset_worktree_to_commit(wt_dir: &str, commit: &str) -> bool {
    reset_worktree_clean(wt_dir);
    super::bg_std_command("git")
        .args(["-C", wt_dir, "checkout", "-f", "--detach", commit])
        .output()
        .is_ok_and(|o| o.status.success())
        && super::bg_std_command("git")
            .args(["-C", wt_dir, "clean", "-fdx"])
            .output()
            .is_ok_and(|o| o.status.success())
}

pub(super) fn cleanup_worktree_dir(run_cwd: &str, wt_dir: &Path) {
    let _ = super::bg_std_command("git")
        .args(["-C", run_cwd, "worktree", "remove", "--force", &wt_dir.display().to_string()])
        .output();
    if wt_dir.exists() {
        let _ = std::fs::remove_dir_all(wt_dir);
    }
}

fn purge_untracked_ephemeral_caches(wt_dir: &str) {
    let Ok(out) = super::bg_std_command("git")
        .args(["-C", wt_dir, "status", "--porcelain=v1", "-z", "-uall"])
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let root = Path::new(wt_dir);
    for rec in out.stdout.split(|b| *b == 0) {
        if rec.len() < 4 || &rec[..2] != b"??" {
            continue;
        }
        let rel = String::from_utf8_lossy(&rec[3..]).replace('\\', "/");
        if rel.is_empty() || !super::roles::is_ephemeral_cache_path(&rel) {
            continue;
        }
        let full = root.join(&rel);
        if full.is_dir() {
            let _ = std::fs::remove_dir_all(&full);
        } else if full.is_file() {
            let _ = std::fs::remove_file(&full);
        }
    }
}
