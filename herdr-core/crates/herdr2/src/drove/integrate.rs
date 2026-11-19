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
/// touching the user's `HEAD` or git index. Also supports unborn Git repositories (no `HEAD` commit yet)
/// by creating a detached root snapshot commit object from the working tree.
fn snapshot_working_tree_commit(run_cwd: &str, wt_parent: &Path) -> Result<String, String> {
    let head_sha = git_stdout_trimmed(run_cwd, &["rev-parse", "--verify", "HEAD"], None).ok();
    let head_tree = if head_sha.is_some() {
        git_stdout_trimmed(run_cwd, &["rev-parse", "--verify", "HEAD^{tree}"], None).ok()
    } else {
        None
    };

    let tmp_idx = wt_parent.join(format!(".drove-idx-{}", super::crypto::rand_id("idx")));
    let _guard = TempIndexGuard(tmp_idx.clone());

    if head_sha.is_some() {
        git_stdout_trimmed(run_cwd, &["read-tree", "HEAD"], Some(&tmp_idx))?;
    }
    git_stdout_trimmed(run_cwd, &["add", "-A"], Some(&tmp_idx))?;

    if head_sha.is_some() {
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
    } else {
        let listed_out = super::bg_std_command("git")
            .args(["-C", run_cwd, "ls-files", "-z"])
            .env("GIT_INDEX_FILE", &tmp_idx)
            .output()
            .map_err(|e| format!("spawn git ls-files: {e}"))?;
        if listed_out.status.success() {
            let ephemeral: Vec<String> = listed_out
                .stdout
                .split(|b| *b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).to_string())
                .filter(|rel| super::roles::is_ephemeral_cache_path(rel))
                .collect();
            for chunk in ephemeral.chunks(200) {
                let mut rm_cmd = super::bg_std_command("git");
                rm_cmd
                    .args(["-C", run_cwd, "rm", "--cached", "-q", "--ignore-unmatch", "--"])
                    .args(chunk)
                    .env("GIT_INDEX_FILE", &tmp_idx);
                let _ = rm_cmd.output();
            }
        }
    }

    let tree_sha = git_stdout_trimmed(run_cwd, &["write-tree"], Some(&tmp_idx))?;
    if let (Some(h_sha), Some(h_tree)) = (&head_sha, &head_tree)
        && tree_sha == *h_tree
    {
        return Ok(h_sha.clone());
    }

    let mut commit_cmd = super::bg_std_command("git");
    commit_cmd.args(["-C", run_cwd, "commit-tree", &tree_sha]);
    if let Some(h_sha) = &head_sha {
        commit_cmd.args(["-p", h_sha]);
    }
    let commit_out = commit_cmd
        .args(["-m", "drove: worktree base snapshot"])
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

        let run_apply = |three_way: bool| -> Result<std::process::Output, ApiErr> {
            let mut args = vec!["-C", run_cwd, "apply"];
            if three_way {
                args.push("--3way");
            }
            args.push("--whitespace=nowarn");
            let mut child = super::bg_std_command("git")
                .args(&args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| ApiErr::new(500, "internal", format!("git apply spawn: {e}")))?;
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(&patch_out.stdout);
            }
            child
                .wait_with_output()
                .map_err(|e| ApiErr::new(500, "internal", format!("git apply wait: {e}")))
        };
        let mut apply_out = run_apply(true)?;
        if !apply_out.status.success()
            && String::from_utf8_lossy(&apply_out.stderr).contains("does not exist in index")
        {
            apply_out = run_apply(false)?;
        }
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

pub(crate) fn cleanup_worktree_dir(run_cwd: &str, wt_dir: &Path) {
    let _ = super::bg_std_command("git")
        .args(["-C", run_cwd, "worktree", "remove", "--force", &wt_dir.display().to_string()])
        .output();
    if wt_dir.exists() {
        let _ = std::fs::remove_dir_all(wt_dir);
    }
    let _ = super::bg_std_command("git")
        .args(["-C", run_cwd, "worktree", "prune"])
        .output();
}

/// Permanently removes a forked session's worktree directory AND deletes its dedicated Git branch.
pub(crate) fn cleanup_fork_worktree(origin_cwd: &str, wt_dir: &Path, branch: Option<&str>) {
    cleanup_worktree_dir(origin_cwd, wt_dir);
    if let Some(b) = branch.map(str::trim).filter(|s| !s.is_empty() && *s != "HEAD") {
        let _ = super::bg_std_command("git")
            .args(["-C", origin_cwd, "branch", "-D", b])
            .output();
    }
}

/// Creates a Git worktree on a dedicated local branch (never detached HEAD) for a forked session,
/// capturing uncommitted working-tree modifications into a base commit so the forked agent can
/// freely commit changes on its own branch without mutating the user's `HEAD` or index.
/// Returns `(worktree_path, branch_name)`.
pub(crate) fn create_fork_worktree(state_dir: &Path, origin_cwd: &str, tag: &str) -> Result<(String, String), String> {
    if is_aosp_harness(origin_cwd) {
        return Err("Worktree isolation is disabled for AOSP harness workspaces; use shared directory fork instead.".into());
    }
    if !is_git_repo(origin_cwd) {
        return Err(format!("Worktree isolation requires a Git repository (`{origin_cwd}` is not a git work tree)."));
    }
    let repo_name: String = Path::new(origin_cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|name| {
            name.chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "repo".into());
    let wt_parent = state_dir.join("worktrees").join("forks");
    std::fs::create_dir_all(&wt_parent).map_err(|e| format!("mkdir fork worktree parent: {e}"))?;
    let wt_dir = wt_parent.join(format!("{repo_name}-{tag}"));
    let base_commit = snapshot_working_tree_commit(origin_cwd, &wt_parent)?;
    let wt_str = wt_dir.display().to_string();
    let branch = format!("herdr/fork-{tag}");
    let out = super::bg_std_command("git")
        .args(["-C", origin_cwd, "worktree", "add", "-b", &branch, &wt_str, &base_commit])
        .output()
        .map_err(|e| format!("spawn git worktree add: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok((wt_str, branch))
}

/// Read-only snapshot of a fork worktree's branch, commits ahead of origin `HEAD`, and uncommitted files.
#[derive(Clone, Debug, Default)]
pub(crate) struct ForkWorktreeSummary {
    pub branch: String,
    pub ahead_commits: Vec<String>,
    pub dirty_files: Vec<String>,
}

pub(crate) fn inspect_fork_worktree(
    origin_cwd: &str,
    wt_dir: &str,
    fallback_branch: Option<&str>,
) -> ForkWorktreeSummary {
    let mut summary = ForkWorktreeSummary {
        branch: fallback_branch.unwrap_or_default().trim().to_string(),
        ..Default::default()
    };
    if !Path::new(wt_dir).is_dir() {
        return summary;
    }
    if let Ok(b) = git_stdout_trimmed(wt_dir, &["rev-parse", "--abbrev-ref", "HEAD"], None)
        && !b.is_empty()
        && b != "HEAD"
    {
        summary.branch = b;
    }
    if let Ok(origin_head) = git_stdout_trimmed(origin_cwd, &["rev-parse", "--verify", "HEAD"], None)
        && !origin_head.is_empty()
    {
        let range = format!("{origin_head}..HEAD");
        if let Ok(out) = super::bg_std_command("git")
            .args(["-C", wt_dir, "log", "--oneline", "-n", "20", &range])
            .output()
            && out.status.success()
        {
            summary.ahead_commits = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    if let Ok(out) = super::bg_std_command("git")
        .args(["-C", wt_dir, "status", "--short"])
        .output()
        && out.status.success()
    {
        summary.dirty_files = String::from_utf8_lossy(&out.stdout)
            .lines()
            .take(40)
            .map(|l| l.trim_end().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    summary
}

/// Applies edits from a forked session's worktree back into `origin_cwd` via `--3way` patch,
/// optionally removing the worktree directory once merged cleanly.
#[allow(dead_code)]
pub(crate) fn merge_fork_worktree(origin_cwd: &str, wt_dir: &str, cleanup: bool) -> Result<Vec<String>, String> {
    if !Path::new(wt_dir).is_dir() {
        return Ok(Vec::new());
    }
    merge_worktree_blocking(origin_cwd, wt_dir, !cleanup).map_err(|e| e.message)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_worktree_works_on_unborn_git_repo_and_merges_back() {
        let base = std::env::temp_dir().join(format!("herdr2-unborn-wt-{}", super::super::crypto::rand_id("t")));
        let repo = base.join("unborn-repo");
        let state = base.join("state");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();

        let init = super::super::bg_std_command("git")
            .args(["-C", repo.to_str().unwrap(), "init", "-q"])
            .output()
            .unwrap();
        assert!(init.status.success(), "git init failed: {:?}", init);
        std::fs::write(repo.join("calc.py"), "print(1)\n").unwrap();

        let (wt, branch) = create_fork_worktree(&state, repo.to_str().unwrap(), "fk1").unwrap();
        assert_eq!(branch, "herdr/fork-fk1");
        assert!(Path::new(&wt).join("calc.py").is_file());
        assert_eq!(std::fs::read_to_string(Path::new(&wt).join("calc.py")).unwrap(), "print(1)\n");

        // Edit existing untracked file and add a new file inside the fork worktree.
        std::fs::write(Path::new(&wt).join("calc.py"), "print(1)\nprint(2)\n").unwrap();
        std::fs::write(Path::new(&wt).join("helper.py"), "ok = True\n").unwrap();

        let merged = merge_fork_worktree(repo.to_str().unwrap(), &wt, true).unwrap();
        assert!(merged.iter().any(|f| f == "calc.py"), "merged={merged:?}");
        assert!(merged.iter().any(|f| f == "helper.py"), "merged={merged:?}");
        assert_eq!(std::fs::read_to_string(repo.join("calc.py")).unwrap(), "print(1)\nprint(2)\n");
        assert_eq!(std::fs::read_to_string(repo.join("helper.py")).unwrap(), "ok = True\n");
        assert!(!Path::new(&wt).exists(), "worktree dir should be cleaned up");
        let _ = std::fs::remove_dir_all(&base);
    }
}
