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
    let reuse_wt = reuse_worktree.map(str::trim).filter(|s| !s.is_empty()).map(String::from);
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
    let wt_parent = wt_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| std::path::PathBuf::from(&run_cwd));

    tokio::task::spawn_blocking(move || {
        if is_aosp_harness(&run_cwd) {
            let bin = Path::new(&run_cwd).join("bin/harness");
            let out = super::bg_std_command(bin)
                .args(["worktree", "add", &wt_dir.display().to_string()])
                .current_dir(&run_cwd)
                .output()
                .map_err(|e| format!("spawn harness worktree add: {e}"))?;
            if !out.status.success() {
                return Err(format!("harness worktree add failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            let s = wt_dir.display().to_string();
            return Ok((s.clone(), Some(s)));
        }

        if !is_git_repo(&run_cwd) {
            return Err(format!(
                "worktree isolation requested, but {} is neither a git repo nor an AOSP harness workspace",
                run_cwd
            ));
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
        let _ = git_cmd(&run_cwd).args(["worktree", "prune"]).output();
        let out = git_cmd(&run_cwd)
            .args(["worktree", "add", "--detach", &wt_str, &base_commit])
            .output()
            .map_err(|e| format!("spawn git worktree add: {e}"))?;
        if !out.status.success() {
            return Err(format!("git worktree add failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok((wt_str.clone(), Some(wt_str)))
    })
    .await
    .map_err(|e| format!("worktree task join error: {e}"))?
}

fn git_cmd(run_cwd: &str) -> std::process::Command {
    let mut cmd = super::bg_std_command("git");
    cmd.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.splitIndex=false",
        "-c",
        "splitIndex.sharedIndexExpire=never",
        "-c",
        "core.safecrlf=false",
    ]);
    #[cfg(windows)]
    cmd.args(["-c", "core.longpaths=true"]);
    cmd.args(["-C", run_cwd]);
    cmd
}

struct TempIndexGuard(std::path::PathBuf);

impl Drop for TempIndexGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn git_stdout_trimmed(run_cwd: &str, args: &[&str], idx_env: Option<&Path>) -> Result<String, String> {
    let mut cmd = git_cmd(run_cwd);
    cmd.args(args);
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
        return Err(format!("git commit-tree failed: {}", String::from_utf8_lossy(&commit_out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&commit_out.stdout).trim().to_string())
}

/// Outcome of merging an isolated worktree back into `run.cwd`.
#[derive(Clone, Debug, Default)]
pub(super) struct MergeOutcome {
    pub changed_files: Vec<String>,
    /// `true` when `run.cwd`'s working tree had advanced since this worktree branched (e.g. a sibling
    /// parallel worktree merged while this task was running), indicating post-merge semantic `verify`
    /// should re-run on the combined `run.cwd` tree if `task.spec.verify` is configured.
    pub base_diverged: bool,
}

#[derive(Clone)]
struct FileBackup {
    rel: String,
    bytes: Option<Vec<u8>>,
    executable: bool,
}

fn capture_file_backups(run_cwd: &str, files: &[String]) -> Vec<FileBackup> {
    files
        .iter()
        .map(|rel| {
            let p = Path::new(run_cwd).join(rel);
            let bytes = std::fs::read(&p).ok();
            let executable = bytes.is_some() && is_executable(&p);
            FileBackup { rel: rel.clone(), bytes, executable }
        })
        .collect()
}

fn restore_file_backups(run_cwd: &str, backups: &[FileBackup]) {
    for b in backups {
        let p = Path::new(run_cwd).join(&b.rel);
        match &b.bytes {
            Some(bytes) => {
                if let Some(parent) = p.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&p, bytes);
                #[cfg(unix)]
                if b.executable {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = std::fs::metadata(&p) {
                        let mut perm = meta.permissions();
                        perm.set_mode(perm.mode() | 0o111);
                        let _ = std::fs::set_permissions(&p, perm);
                    }
                }
            }
            None => {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

/// Serializes merging worktree changes back into `run.cwd` (and re-verifying on `run.cwd` when a
/// concurrent sibling task merged after this worktree branched), optionally keeping the reset worktree for warm reuse.
pub(super) async fn merge_and_finalize_worktree(
    core: &Arc<Core>,
    run: &Run,
    task: &Task,
    worktree_dir: Option<&str>,
    keep_for_reuse: bool,
) -> Result<MergeOutcome, ApiErr> {
    let Some(wt) = worktree_dir.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(MergeOutcome::default());
    };
    let lock = core.drove.run_merge_lock(&run.id);
    let _guard = lock.lock().await;
    let run_cwd = run.cwd.clone();
    let wt_str = wt.to_string();
    let post_verify_cmd = task.spec.verify.as_ref().and_then(|v| {
        let (cmd, timeout) = match v {
            VerifySpec::Cmd(s) => (s.trim().to_string(), 300u64),
            VerifySpec::Obj(o) => (o.cmd.trim().to_string(), o.timeout_sec.unwrap_or(300)),
        };
        (!cmd.is_empty()).then_some((cmd, timeout))
    });

    let (outcome, backups, wt_patch) = tokio::task::spawn_blocking({
        let run_cwd = run_cwd.clone();
        let wt_str = wt_str.clone();
        move || merge_worktree_stage_one(&run_cwd, &wt_str)
    })
    .await
    .map_err(|e| ApiErr::new(500, "internal", e.to_string()))??;

    // When another concurrent worktree merged into `run_cwd` after this worktree branched (`base_diverged`),
    // re-run `task.spec.verify` against the combined `run_cwd` before finalizing so cross-worktree
    // signature/semantic breakages are caught immediately instead of silently corrupting `run_cwd`.
    if outcome.base_diverged
        && !outcome.changed_files.is_empty()
        && let Some((cmd_str, timeout_sec)) = post_verify_cmd
    {
        let vres = super::verify::run_verify_command(&run_cwd, &cmd_str, timeout_sec).await;
        if !vres.success {
            let run_cwd_rb = run_cwd.clone();
            let wt_rb = wt_str.clone();
            let files_rb = outcome.changed_files.clone();
            let _ = tokio::task::spawn_blocking(move || {
                restore_file_backups(&run_cwd_rb, &backups);
                refresh_worktree_with_latest_main(&run_cwd_rb, &wt_rb, &files_rb, &wt_patch);
            })
            .await;
            let tail: String = vres.combined_output.chars().rev().take(1600).collect::<String>().chars().rev().collect();
            core.drove.store.append_daemon_event(super::store::NewDaemonEvent {
                run_id: Some(run.id.clone()),
                task_id: Some(task.id.clone()),
                attempt_id: None,
                worker_id: None,
                message_id: None,
                category: "verify".into(),
                kind: "post_merge_verify".into(),
                status: "failed".into(),
                title: format!("Post-merge verify failed on `{}` after concurrent worktree updates", task.key),
                detail: Some(tail.clone()),
                duration_ms: None,
                tokens: None,
                cost: None,
            });
            return Err(ApiErr::new(
                422,
                "post_merge_verify_failed",
                format!(
                    "Post-merge verification failed after combining with concurrent workspace changes (`{cmd_str}`). \
                     Your worktree has been refreshed onto the latest workspace state — fix the integration issue and re-run `drove worker done`:\n{tail}"
                ),
            ));
        }
    }

    tokio::task::spawn_blocking(move || {
        if !is_aosp_harness(&run_cwd) {
            if keep_for_reuse {
                reset_worktree_clean(&wt_str);
            } else {
                cleanup_worktree_dir(&run_cwd, Path::new(&wt_str));
            }
        }
    })
    .await
    .map_err(|e| ApiErr::new(500, "internal", e.to_string()))?;

    Ok(outcome)
}

/// Best-effort sync of candidate worktree changes into `run.cwd` when a review circuit trips into
/// `HandoffToCoordinator` / `AwaitingUserDecision`, so the Coordinator immediately sees the author's
/// candidate files in `run.cwd` and subsequent `retry` / worker reaping never wipes unmerged work.
pub(super) async fn sync_worktree_for_coordinator_handoff(
    core: &Arc<Core>,
    run: &Run,
    worktree_dir: Option<&str>,
) -> Vec<String> {
    let Some(wt) = worktree_dir.map(str::trim).filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    if wt == run.cwd || is_aosp_harness(&run.cwd) || !Path::new(wt).is_dir() {
        return Vec::new();
    }
    let lock = core.drove.run_merge_lock(&run.id);
    let _guard = lock.lock().await;
    let run_cwd = run.cwd.clone();
    let wt_str = wt.to_string();
    match tokio::task::spawn_blocking(move || merge_worktree_stage_one(&run_cwd, &wt_str)).await {
        Ok(Ok((outcome, _, _))) => outcome.changed_files,
        _ => Vec::new(),
    }
}

fn refresh_worktree_with_latest_main(run_cwd: &str, wt_dir: &str, changed_files: &[String], wt_patch: &[u8]) {
    let wt_parent = Path::new(wt_dir).parent().unwrap_or_else(|| Path::new(run_cwd));
    let Ok(new_base) = snapshot_working_tree_commit(run_cwd, wt_parent) else {
        return;
    };
    let their_blobs: Vec<(String, Option<Vec<u8>>, bool)> = changed_files
        .iter()
        .map(|rel| {
            let p = Path::new(wt_dir).join(rel);
            let bytes = std::fs::read(&p).ok();
            let exec = bytes.is_some() && is_executable(&p);
            (rel.clone(), bytes, exec)
        })
        .collect();
    let old_base_blobs: Vec<(String, Option<Vec<u8>>)> =
        changed_files.iter().map(|rel| (rel.clone(), git_show_blob(wt_dir, &format!("HEAD:{rel}")))).collect();

    if !reset_worktree_to_commit(wt_dir, &new_base) {
        return;
    }
    if !wt_patch.is_empty() {
        let mut child = match super::bg_std_command("git")
            .args(["-C", wt_dir, "apply", "--whitespace=nowarn"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(wt_patch);
        }
        if child.wait().is_ok_and(|s| s.success()) {
            return;
        }
    }
    // Best-effort 3-way replay of author edits onto the refreshed worktree base.
    let tmp_dir = std::env::temp_dir().join(format!("herdr2-wt-replay-{}", super::crypto::rand_id("r")));
    let _ = std::fs::create_dir_all(&tmp_dir);
    for (idx, (rel, theirs_opt, exec)) in their_blobs.into_iter().enumerate() {
        let dst = Path::new(wt_dir).join(&rel);
        let ours_opt = std::fs::read(&dst).ok();
        let base_opt = old_base_blobs.get(idx).and_then(|(_, b)| b.clone());
        match (base_opt, ours_opt, theirs_opt) {
            (_, _, None) => {
                let _ = std::fs::remove_file(&dst);
            }
            (Some(base), Some(ours), Some(theirs))
                if ours != base && !base.contains(&0) && !ours.contains(&0) && !theirs.contains(&0) =>
            {
                let f_o = tmp_dir.join(format!("o-{idx}"));
                let f_b = tmp_dir.join(format!("b-{idx}"));
                let f_t = tmp_dir.join(format!("t-{idx}"));
                if std::fs::write(&f_o, &ours).is_ok()
                    && std::fs::write(&f_b, &base).is_ok()
                    && std::fs::write(&f_t, &theirs).is_ok()
                    && let Ok(out) = super::bg_std_command("git")
                        .args(["merge-file", "-p"])
                        .arg(&f_o)
                        .arg(&f_b)
                        .arg(&f_t)
                        .output()
                {
                    let _ = std::fs::write(&dst, out.stdout);
                }
            }
            (_, _, Some(theirs)) => {
                if let Some(parent) = dst.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&dst, theirs);
                #[cfg(unix)]
                if exec {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = std::fs::metadata(&dst) {
                        let mut perm = meta.permissions();
                        perm.set_mode(perm.mode() | 0o111);
                        let _ = std::fs::set_permissions(&dst, perm);
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

fn merge_worktree_stage_one(run_cwd: &str, wt_dir: &str) -> Result<(MergeOutcome, Vec<FileBackup>, Vec<u8>), ApiErr> {
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
                format!("Harness worktree merge failed: {}", String::from_utf8_lossy(&merge_out.stderr).trim()),
            ));
        }
        let _ = super::bg_std_command(bin).args(["worktree", "remove", wt_dir]).current_dir(run_cwd).output();
        return Ok((MergeOutcome::default(), Vec::new(), Vec::new()));
    }

    let wt_base_tree = git_stdout_trimmed(wt_dir, &["rev-parse", "--verify", "HEAD^{tree}"], None).ok();
    let pre_main_tree = super::roles::compute_git_working_tree_sha(run_cwd);
    let base_diverged = match (&wt_base_tree, &pre_main_tree) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    };

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

    let mut backups = Vec::new();
    let mut patch_bytes = Vec::new();

    if !changed_files.is_empty() {
        backups = capture_file_backups(run_cwd, &changed_files);
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
        patch_bytes = patch_out.stdout.clone();

        // Fast path: apply directly to the working tree (NO `--3way` / `--index`).
        // 1) Handles dirty pre-existing unstaged files whose working-tree blob still matches the
        //    worktree's base snapshot (`HEAD`), avoiding false `does not match index` deadlocks.
        // 2) Never mutates `run_cwd`'s `.git/index`, preserving the user's staged/unstaged state.
        let run_plain_apply = |check_only: bool| -> Result<std::process::Output, ApiErr> {
            let mut args = vec!["-C", run_cwd, "apply"];
            if check_only {
                args.push("--check");
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
            child.wait_with_output().map_err(|e| ApiErr::new(500, "internal", format!("git apply wait: {e}")))
        };

        let check_out = run_plain_apply(true)?;
        if check_out.status.success() {
            let apply_out = run_plain_apply(false)?;
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
        } else {
            // Fallback for true concurrent edits in `run_cwd`: perform base-blob-aware per-file
            // 3-way merge (`git merge-file -p`) in memory and commit to working tree only if ALL
            // files merge cleanly — still never touching `run_cwd`'s `.git/index`.
            let fallback_err = String::from_utf8_lossy(&check_out.stderr).trim().to_string();
            merge_worktree_files_three_way(run_cwd, wt_dir, &changed_files, &fallback_err)?;
        }
    }

    Ok((MergeOutcome { changed_files, base_diverged }, backups, patch_bytes))
}

fn merge_worktree_blocking(run_cwd: &str, wt_dir: &str, keep_for_reuse: bool) -> Result<Vec<String>, ApiErr> {
    let (outcome, _, _) = merge_worktree_stage_one(run_cwd, wt_dir)?;
    if !is_aosp_harness(run_cwd) {
        if keep_for_reuse {
            reset_worktree_clean(wt_dir);
        } else {
            cleanup_worktree_dir(run_cwd, Path::new(wt_dir));
        }
    }
    Ok(outcome.changed_files)
}

/// Base-blob-aware 3-way working-tree merge fallback used only when plain `git apply --check` fails
/// (i.e. concurrent edits happened in `run_cwd` after the worktree branched). Never touches `run_cwd`'s
/// `.git/index`, and writes nothing to disk if any file has a real textual/binary conflict.
fn merge_worktree_files_three_way(
    run_cwd: &str,
    wt_dir: &str,
    changed_files: &[String],
    apply_err: &str,
) -> Result<(), ApiErr> {
    enum PlannedOp {
        Write { rel: String, bytes: Vec<u8>, executable: bool },
        Delete { rel: String },
    }

    let tmp_dir = std::env::temp_dir().join(format!("herdr2-3way-{}", super::crypto::rand_id("m")));
    let _ = std::fs::create_dir_all(&tmp_dir);
    struct DirGuard(std::path::PathBuf);
    impl Drop for DirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _guard = DirGuard(tmp_dir.clone());

    let mut ops: Vec<PlannedOp> = Vec::with_capacity(changed_files.len());
    let mut conflicts: Vec<String> = Vec::new();

    for (idx, rel) in changed_files.iter().enumerate() {
        let base_bytes = git_show_blob(wt_dir, &format!("HEAD:{rel}"));
        let wt_path = Path::new(wt_dir).join(rel);
        let main_path = Path::new(run_cwd).join(rel);
        let theirs_bytes = std::fs::read(&wt_path).ok();
        let ours_bytes = std::fs::read(&main_path).ok();
        let wt_exec = is_executable(&wt_path);

        if ours_bytes == base_bytes {
            // Main working tree file is byte-identical to the worktree's base snapshot (even if
            // unstaged relative to `.git/index`): safe to take `theirs` directly.
            match theirs_bytes {
                Some(bytes) => ops.push(PlannedOp::Write { rel: rel.clone(), bytes, executable: wt_exec }),
                None => ops.push(PlannedOp::Delete { rel: rel.clone() }),
            }
            continue;
        }
        if ours_bytes == theirs_bytes {
            continue;
        }

        match (base_bytes, ours_bytes, theirs_bytes) {
            (Some(base), Some(ours), Some(theirs))
                if !base.contains(&0) && !ours.contains(&0) && !theirs.contains(&0) =>
            {
                let f_ours = tmp_dir.join(format!("ours-{idx}"));
                let f_base = tmp_dir.join(format!("base-{idx}"));
                let f_theirs = tmp_dir.join(format!("theirs-{idx}"));
                if std::fs::write(&f_ours, &ours).is_err()
                    || std::fs::write(&f_base, &base).is_err()
                    || std::fs::write(&f_theirs, &theirs).is_err()
                {
                    conflicts.push(format!("{rel} (temp file write error)"));
                    continue;
                }
                let out = super::bg_std_command("git")
                    .args(["merge-file", "-p"])
                    .arg(&f_ours)
                    .arg(&f_base)
                    .arg(&f_theirs)
                    .output();
                match out {
                    Ok(o) if o.status.success() => {
                        ops.push(PlannedOp::Write { rel: rel.clone(), bytes: o.stdout, executable: wt_exec });
                    }
                    _ => conflicts.push(rel.clone()),
                }
            }
            _ => conflicts.push(rel.clone()),
        }
    }

    if !conflicts.is_empty() {
        return Err(ApiErr::new(
            422,
            "merge_conflict",
            format!(
                "Merge conflict integrating worktree `{wt_dir}` into `{run_cwd}` on [{}]: {apply_err}",
                conflicts.join(", ")
            ),
        ));
    }

    for op in ops {
        match op {
            PlannedOp::Write { rel, bytes, executable } => {
                let dst = Path::new(run_cwd).join(&rel);
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| ApiErr::new(500, "internal", format!("mkdir for {rel}: {e}")))?;
                }
                std::fs::write(&dst, bytes)
                    .map_err(|e| ApiErr::new(500, "internal", format!("write merged {rel}: {e}")))?;
                #[cfg(unix)]
                if executable {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = std::fs::metadata(&dst) {
                        let mut perm = meta.permissions();
                        perm.set_mode(perm.mode() | 0o111);
                        let _ = std::fs::set_permissions(&dst, perm);
                    }
                }
                #[cfg(not(unix))]
                let _ = executable;
            }
            PlannedOp::Delete { rel } => {
                let dst = Path::new(run_cwd).join(&rel);
                if dst.exists() {
                    std::fs::remove_file(&dst)
                        .map_err(|e| ApiErr::new(500, "internal", format!("remove deleted {rel}: {e}")))?;
                }
            }
        }
    }
    Ok(())
}

fn git_show_blob(cwd: &str, rev_path: &str) -> Option<Vec<u8>> {
    let out = super::bg_std_command("git").args(["-C", cwd, "show", rev_path]).output().ok()?;
    out.status.success().then_some(out.stdout)
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

pub(super) fn reset_worktree_clean(wt_dir: &str) {
    let _ = super::bg_std_command("git").args(["-C", wt_dir, "reset", "-q", "HEAD"]).output();
    let _ = super::bg_std_command("git").args(["-C", wt_dir, "checkout", "-f", "--", "."]).output();
    let _ = super::bg_std_command("git").args(["-C", wt_dir, "clean", "-fdx"]).output();
}

fn reset_worktree_to_commit(wt_dir: &str, commit: &str) -> bool {
    reset_worktree_clean(wt_dir);
    super::bg_std_command("git")
        .args(["-C", wt_dir, "checkout", "-f", "--detach", commit])
        .output()
        .is_ok_and(|o| o.status.success())
        && super::bg_std_command("git").args(["-C", wt_dir, "clean", "-fdx"]).output().is_ok_and(|o| o.status.success())
}

pub(crate) fn cleanup_worktree_dir(run_cwd: &str, wt_dir: &Path) {
    let _ = super::bg_std_command("git")
        .args(["-C", run_cwd, "worktree", "remove", "--force", &wt_dir.display().to_string()])
        .output();
    if wt_dir.exists() {
        let _ = std::fs::remove_dir_all(wt_dir);
    }
    let _ = super::bg_std_command("git").args(["-C", run_cwd, "worktree", "prune"]).output();
}

/// Creates a detached throwaway Git worktree cloned from `source_cwd`'s exact working tree
/// (including uncommitted candidate edits) so an adversarial `Breaker` reviewer can write and
/// execute falsifying tests without touching `source_cwd` or `run.cwd`.
pub(super) async fn create_breaker_worktree(
    core: &Arc<Core>,
    run: &Run,
    source_cwd: &str,
    tag: &str,
) -> Result<String, String> {
    if is_aosp_harness(&run.cwd) || !is_git_repo(source_cwd) {
        return Err(format!("breaker worktree requires a standard Git repository ({source_cwd})"));
    }
    let run_cwd = run.cwd.clone();
    let src_cwd = source_cwd.to_string();
    let safe_tag: String = tag
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let wt_dir = core
        .drove
        .paths
        .state_dir
        .join("worktrees")
        .join(&run.id)
        .join(format!("breaker-{safe_tag}-{}", super::crypto::rand_id("brk")));
    if let Some(parent) = wt_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir breaker worktree parent: {e}"))?;
    }
    let wt_parent = wt_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| std::path::PathBuf::from(&run_cwd));

    let lock = core.drove.run_merge_lock(&run.id);
    let _guard = lock.lock().await;

    tokio::task::spawn_blocking(move || {
        let head_ok = super::bg_std_command("git")
            .args(["-C", &src_cwd, "rev-parse", "--verify", "HEAD"])
            .output()
            .is_ok_and(|o| o.status.success());
        if !head_ok {
            return Err("git repository has no HEAD commit yet for breaker worktree".into());
        }
        let snap_commit = snapshot_working_tree_commit(&src_cwd, &wt_parent)?;
        let wt_str = wt_dir.display().to_string();
        let _ = git_cmd(&run_cwd).args(["worktree", "prune"]).output();
        let out = git_cmd(&run_cwd)
            .args(["worktree", "add", "--detach", &wt_str, &snap_commit])
            .output()
            .map_err(|e| format!("spawn git worktree add (breaker): {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git worktree add (breaker) failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(wt_str)
    })
    .await
    .map_err(|e| format!("breaker worktree join error: {e}"))?
}

/// Copies passing regression test edits from `brk_cwd` into `candidate_cwd` when
/// `review.keepRegressionTests` is enabled, without mutating `.git/index`.
pub(super) fn copy_breaker_tests_into_candidate(
    brk_cwd: &str,
    candidate_cwd: &str,
    touched_rel_paths: &[String],
) {
    for rel in touched_rel_paths {
        if super::roles::is_ephemeral_cache_path(rel) {
            continue;
        }
        let src = Path::new(brk_cwd).join(rel);
        let dst = Path::new(candidate_cwd).join(rel);
        if let Ok(bytes) = std::fs::read(&src) {
            if let Some(parent) = dst.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::write(&dst, bytes).is_ok() {
                #[cfg(unix)]
                if is_executable(&src) {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = std::fs::metadata(&dst) {
                        let mut perm = meta.permissions();
                        perm.set_mode(perm.mode() | 0o111);
                        let _ = std::fs::set_permissions(&dst, perm);
                    }
                }
            }
        } else if dst.exists() {
            let _ = std::fs::remove_file(&dst);
        }
    }
}

/// Permanently removes a forked session's worktree directory AND deletes its dedicated Git branch.
pub(crate) fn cleanup_fork_worktree(origin_cwd: &str, wt_dir: &Path, branch: Option<&str>) {
    cleanup_worktree_dir(origin_cwd, wt_dir);
    if let Some(b) = branch.map(str::trim).filter(|s| !s.is_empty() && *s != "HEAD") {
        let _ = super::bg_std_command("git").args(["-C", origin_cwd, "branch", "-D", b]).output();
    }
}

/// Structured preflight blocker returned when a repository is not ready for a forked Git worktree.
#[derive(Clone, Debug)]
pub(crate) struct WorktreeBlocker {
    pub code: &'static str,
    pub cwd: String,
    pub op: Option<String>,
    pub files: Vec<String>,
    pub total_dirty: usize,
}

impl WorktreeBlocker {
    pub(crate) fn wire_message(&self) -> String {
        let payload = serde_json::json!({
            "code": self.code,
            "cwd": self.cwd,
            "op": self.op,
            "files": self.files,
            "total_dirty": self.total_dirty,
        });
        format!("WORKTREE_BLOCKED:{payload}")
    }
}

fn detect_in_progress_git_op(origin_cwd: &str) -> Option<String> {
    let git_dir_str = git_stdout_trimmed(origin_cwd, &["rev-parse", "--git-dir"], None).ok()?;
    let git_dir_path = Path::new(&git_dir_str);
    let git_dir =
        if git_dir_path.is_absolute() { git_dir_path.to_path_buf() } else { Path::new(origin_cwd).join(git_dir_path) };
    if git_dir.join("MERGE_HEAD").exists() {
        return Some("merge".into());
    }
    if git_dir.join("rebase-merge").exists()
        || git_dir.join("rebase-apply").exists()
        || git_dir.join("REBASE_HEAD").exists()
    {
        return Some("rebase".into());
    }
    if git_dir.join("CHERRY_PICK_HEAD").exists() {
        return Some("cherry-pick".into());
    }
    if git_dir.join("REVERT_HEAD").exists() {
        return Some("revert".into());
    }
    if git_dir.join("BISECT_LOG").exists() {
        return Some("bisect".into());
    }
    None
}

/// Structured preflight blocker returned before starting Drove orchestration when the workspace
/// cannot safely create Git worktrees.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DroveWorktreeBlocker {
    pub code: String,
    pub cwd: String,
    pub message: String,
    pub fix_hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

/// Non-blocking informational status returned when Drove worktree preflight passes.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DroveWorktreePreflightOk {
    pub is_aosp: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirty_files: Vec<String>,
    pub total_dirty: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty_warning: Option<String>,
}

fn find_nested_unborn_git_dirs(origin_cwd: &str) -> Vec<String> {
    let Ok(out) =
        git_cmd(origin_cwd).args(["ls-files", "--others", "--exclude-standard", "--directory", "-z"]).output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let root = Path::new(origin_cwd);
    let mut nested = Vec::new();
    for rec in out.stdout.split(|&b| b == 0) {
        if rec.is_empty() {
            continue;
        }
        let rel = String::from_utf8_lossy(rec).trim_end_matches('/').to_string();
        if rel.is_empty() {
            continue;
        }
        let dot_git = root.join(&rel).join(".git");
        if dot_git.is_dir() && !dot_git.join("HEAD").exists() {
            nested.push(rel);
            if nested.len() >= 8 {
                break;
            }
        }
    }
    nested
}

/// Pre-flight check run before starting Drove orchestration whenever tasks require or may trigger
/// Git worktree isolation. Fails fast with actionable guidance instead of creating failing tasks.
pub fn check_drove_worktree_ready(origin_cwd: &str) -> Result<DroveWorktreePreflightOk, DroveWorktreeBlocker> {
    if is_aosp_harness(origin_cwd) {
        return Ok(DroveWorktreePreflightOk { is_aosp: true, ..Default::default() });
    }
    if !Path::new(origin_cwd).is_dir() {
        return Err(DroveWorktreeBlocker {
            code: "missing_cwd".into(),
            cwd: origin_cwd.to_string(),
            message: format!("工作目录不存在：{origin_cwd}"),
            fix_hint: "请确认 --cwd 指向一个有效的本地目录。".into(),
            op: None,
            files: Vec::new(),
        });
    }

    let probe_out = git_cmd(origin_cwd).args(["rev-parse", "--is-inside-work-tree"]).output();
    match probe_out {
        Err(e) => {
            return Err(DroveWorktreeBlocker {
                code: "git_missing".into(),
                cwd: origin_cwd.to_string(),
                message: format!("无法执行 git：{e}"),
                fix_hint: "请确认已安装 git 且位于 PATH 中。".into(),
                op: None,
                files: Vec::new(),
            });
        }
        Ok(out) if !out.status.success() => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let lower = stderr.to_ascii_lowercase();
            if lower.contains("dubious ownership") {
                return Err(DroveWorktreeBlocker {
                    code: "dubious_ownership".into(),
                    cwd: origin_cwd.to_string(),
                    message: format!("Git 拒绝访问目录（dubious ownership）：{origin_cwd}"),
                    fix_hint: format!("请先执行：git config --global --add safe.directory {origin_cwd}"),
                    op: None,
                    files: Vec::new(),
                });
            }
            let is_bare = git_cmd(origin_cwd)
                .args(["rev-parse", "--is-bare-repository"])
                .output()
                .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true");
            if is_bare {
                return Err(DroveWorktreeBlocker {
                    code: "bare_repo".into(),
                    cwd: origin_cwd.to_string(),
                    message: format!(
                        "当前目录是裸仓库（bare repository），无法直接用于 Drove Worktree 编排：{origin_cwd}"
                    ),
                    fix_hint: "请切换到非裸工作树目录后再启动编排。".into(),
                    op: None,
                    files: Vec::new(),
                });
            }
            return Err(DroveWorktreeBlocker {
                code: "not_git_repo".into(),
                cwd: origin_cwd.to_string(),
                message: format!("当前目录不是 Git 仓库，无法创建并行 Worktree：{origin_cwd}"),
                fix_hint: format!(
                    "请先初始化 Git 仓库并完成首次提交：cd {origin_cwd} && git init && git add -A && git commit -m \"chore: initial commit\"（或取消 --worktree / 关闭 auto_worktree）"
                ),
                op: None,
                files: Vec::new(),
            });
        }
        Ok(_) => {}
    }

    if git_stdout_trimmed(origin_cwd, &["rev-parse", "--verify", "HEAD"], None).is_err() {
        return Err(DroveWorktreeBlocker {
            code: "no_commits".into(),
            cwd: origin_cwd.to_string(),
            message: format!("当前 Git 仓库尚未有任何提交（无 HEAD），无法创建 Worktree：{origin_cwd}"),
            fix_hint: format!(
                "请先创建一次初始提交：cd {origin_cwd} && git add -A && git commit -m \"chore: initial commit\""
            ),
            op: None,
            files: Vec::new(),
        });
    }

    let git_dir_raw = git_stdout_trimmed(origin_cwd, &["rev-parse", "--git-dir"], None).unwrap_or_default();
    let common_dir_raw = git_stdout_trimmed(origin_cwd, &["rev-parse", "--git-common-dir"], None).unwrap_or_default();
    let resolve_dir = |raw: &str| {
        let p = Path::new(raw);
        if p.is_absolute() { p.to_path_buf() } else { Path::new(origin_cwd).join(p) }
    };
    if !git_dir_raw.is_empty() {
        let lock_file = resolve_dir(&git_dir_raw).join("index.lock");
        if lock_file.exists() {
            return Err(DroveWorktreeBlocker {
                code: "git_locked".into(),
                cwd: origin_cwd.to_string(),
                message: format!("检测到 Git 锁文件残留（{}），无法创建 Worktree", lock_file.display()),
                fix_hint: format!("请确认无其他 git 进程运行后清理锁文件：rm -f {}", lock_file.display()),
                op: None,
                files: vec![lock_file.display().to_string()],
            });
        }
    }
    if !common_dir_raw.is_empty() {
        let wt_meta_dir = resolve_dir(&common_dir_raw).join("worktrees");
        let probe_dir = if wt_meta_dir.is_dir() { wt_meta_dir } else { resolve_dir(&common_dir_raw) };
        let probe_file = probe_dir.join(format!(".herdr2-wt-probe-{}", super::crypto::rand_id("p")));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe_file) {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe_file);
            }
            Err(e) => {
                return Err(DroveWorktreeBlocker {
                    code: "readonly_git_dir".into(),
                    cwd: origin_cwd.to_string(),
                    message: format!("Git 元数据目录不可写（{}）：{e}", probe_dir.display()),
                    fix_hint: "请检查目录权限或磁盘空间（ENOSPC / 只读挂载）后重试。".into(),
                    op: None,
                    files: Vec::new(),
                });
            }
        }
    }

    let mut op = detect_in_progress_git_op(origin_cwd);
    let mut dirty: Vec<String> = Vec::new();
    if let Ok(out) = git_cmd(origin_cwd).args(["status", "--porcelain=v1", "-z", "-unormal"]).output()
        && out.status.success()
    {
        let mut iter = out.stdout.split(|b| *b == 0);
        while let Some(raw) = iter.next() {
            if raw.len() < 4 {
                continue;
            }
            let x = raw[0] as char;
            let y = raw[1] as char;
            let xy = [x, y];
            let path = String::from_utf8_lossy(&raw[3..]).to_string();
            if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
                let _ = iter.next();
            }
            if matches!(xy, ['D', 'D'] | ['A', 'U'] | ['U', 'D'] | ['U', 'A'] | ['D', 'U'] | ['A', 'A'] | ['U', 'U'])
                && op.is_none()
            {
                op = Some("conflict".into());
            }
            if xy == ['?', '?'] && super::roles::is_ephemeral_cache_path(path.trim_end_matches('/')) {
                continue;
            }
            dirty.push(format!("{x}{y} {path}"));
        }
    }
    if let Some(in_prog) = op {
        return Err(DroveWorktreeBlocker {
            code: "in_progress_op".into(),
            cwd: origin_cwd.to_string(),
            message: format!("仓库正在进行 `{in_prog}` 操作或存在未解决冲突，无法创建 Worktree：{origin_cwd}"),
            fix_hint: format!(
                "请先完成或中止当前 `{in_prog}`（如 `git {in_prog} --abort` 或解决冲突提交）后再开始编排。"
            ),
            op: Some(in_prog),
            files: dirty.into_iter().take(8).collect(),
        });
    }

    let nested = find_nested_unborn_git_dirs(origin_cwd);
    if !nested.is_empty() {
        return Err(DroveWorktreeBlocker {
            code: "nested_unborn_git".into(),
            cwd: origin_cwd.to_string(),
            message: format!("工作区包含未初始化的嵌套 .git 目录（{}），会导致 Worktree 快照失败", nested.join(", ")),
            fix_hint: "请移除这些子目录中的空 .git 目录、提交它们或将其加入 .gitignore 后重试。".into(),
            op: None,
            files: nested,
        });
    }

    let total_dirty = dirty.len();
    let dirty_files: Vec<String> = dirty.into_iter().take(8).collect();
    let dirty_warning = (total_dirty > 0).then(|| {
        format!(
            "提示：当前仓库有 {total_dirty} 处未提交修改（{}），Worktree 将基于临时快照继承这些改动，建议先 commit/stash 以降低合回冲突风险。",
            dirty_files.join(", ")
        )
    });
    Ok(DroveWorktreePreflightOk { is_aosp: false, dirty_files, total_dirty, dirty_warning })
}

/// Validates that `origin_cwd` is a clean Git repository with at least one commit and no in-progress
/// merge/rebase/conflict before creating a forked worktree. Returns `HEAD` SHA when ready.
pub(crate) fn check_fork_worktree_ready(origin_cwd: &str) -> Result<String, WorktreeBlocker> {
    if is_aosp_harness(origin_cwd) {
        return Err(WorktreeBlocker {
            code: "aosp_harness",
            cwd: origin_cwd.to_string(),
            op: None,
            files: Vec::new(),
            total_dirty: 0,
        });
    }
    if !is_git_repo(origin_cwd) {
        return Err(WorktreeBlocker {
            code: "not_git_repo",
            cwd: origin_cwd.to_string(),
            op: None,
            files: Vec::new(),
            total_dirty: 0,
        });
    }
    let Ok(head_sha) = git_stdout_trimmed(origin_cwd, &["rev-parse", "--verify", "HEAD"], None) else {
        return Err(WorktreeBlocker {
            code: "no_commits",
            cwd: origin_cwd.to_string(),
            op: None,
            files: Vec::new(),
            total_dirty: 0,
        });
    };

    let mut op = detect_in_progress_git_op(origin_cwd);
    let mut dirty: Vec<String> = Vec::new();
    if let Ok(out) =
        super::bg_std_command("git").args(["-C", origin_cwd, "status", "--porcelain=v1", "-z", "-unormal"]).output()
        && out.status.success()
    {
        let mut iter = out.stdout.split(|b| *b == 0);
        while let Some(raw) = iter.next() {
            if raw.len() < 4 {
                continue;
            }
            let x = raw[0] as char;
            let y = raw[1] as char;
            let xy = [x, y];
            let path = String::from_utf8_lossy(&raw[3..]).to_string();
            if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
                let _ = iter.next(); // consume original path for rename/copy
            }
            if matches!(xy, ['D', 'D'] | ['A', 'U'] | ['U', 'D'] | ['U', 'A'] | ['D', 'U'] | ['A', 'A'] | ['U', 'U'])
                && op.is_none()
            {
                op = Some("conflict".into());
            }
            if xy == ['?', '?'] && super::roles::is_ephemeral_cache_path(path.trim_end_matches('/')) {
                continue;
            }
            dirty.push(format!("{x}{y} {path}"));
        }
    }

    if let Some(in_prog) = op {
        let total_dirty = dirty.len();
        return Err(WorktreeBlocker {
            code: "in_progress_op",
            cwd: origin_cwd.to_string(),
            op: Some(in_prog),
            files: dirty.into_iter().take(8).collect(),
            total_dirty,
        });
    }
    if !dirty.is_empty() {
        let total_dirty = dirty.len();
        return Err(WorktreeBlocker {
            code: "dirty_worktree",
            cwd: origin_cwd.to_string(),
            op: None,
            files: dirty.into_iter().take(8).collect(),
            total_dirty,
        });
    }
    Ok(head_sha)
}

/// Creates a Git worktree on a dedicated local branch (never detached HEAD) for a forked session
/// directly from the repository's clean `HEAD` commit so main and fork share exact Git history.
/// Returns `(worktree_path, branch_name)`.
pub(crate) fn create_fork_worktree(state_dir: &Path, origin_cwd: &str, tag: &str) -> Result<(String, String), String> {
    let head_sha = check_fork_worktree_ready(origin_cwd).map_err(|b| b.wire_message())?;
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
    let wt_str = wt_dir.display().to_string();
    let branch = format!("herdr/fork-{tag}");
    let out = super::bg_std_command("git")
        .args(["-C", origin_cwd, "worktree", "add", "-b", &branch, &wt_str, &head_sha])
        .output()
        .map_err(|e| format!("spawn git worktree add: {e}"))?;
    if !out.status.success() {
        return Err(format!("git worktree add failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
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
    let mut summary =
        ForkWorktreeSummary { branch: fallback_branch.unwrap_or_default().trim().to_string(), ..Default::default() };
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
        if let Ok(out) =
            super::bg_std_command("git").args(["-C", wt_dir, "log", "--oneline", "-n", "20", &range]).output()
            && out.status.success()
        {
            summary.ahead_commits = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    if let Ok(out) = super::bg_std_command("git").args(["-C", wt_dir, "status", "--short"]).output()
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
    let Ok(out) = super::bg_std_command("git").args(["-C", wt_dir, "status", "--porcelain=v1", "-z", "-uall"]).output()
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
    fn fork_worktree_preflight_blocks_unready_repos_and_succeeds_when_clean() {
        let base = std::env::temp_dir().join(format!("herdr2-preflight-wt-{}", super::super::crypto::rand_id("t")));
        let repo = base.join("repo");
        let state = base.join("state");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();

        // 1. Not a git repo -> not_git_repo
        let err1 = create_fork_worktree(&state, repo.to_str().unwrap(), "fk0").unwrap_err();
        assert!(err1.contains("\"code\":\"not_git_repo\""), "err1={err1}");

        // 2. Initialized with git init, no commits yet -> no_commits
        let run_git = |args: &[&str]| {
            let out = super::super::bg_std_command("git")
                .args(["-C", repo.to_str().unwrap()])
                .args(args)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@local")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@local")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        };
        run_git(&["init", "-q"]);
        std::fs::write(repo.join("calc.py"), "print(1)\n").unwrap();
        let err2 = create_fork_worktree(&state, repo.to_str().unwrap(), "fk0").unwrap_err();
        assert!(err2.contains("\"code\":\"no_commits\""), "err2={err2}");

        // 3. Committed, then dirty working tree -> dirty_worktree
        run_git(&["add", "calc.py"]);
        run_git(&["commit", "-q", "-m", "init"]);
        std::fs::write(repo.join("calc.py"), "print(1)\nprint(2)\n").unwrap();
        let err3 = create_fork_worktree(&state, repo.to_str().unwrap(), "fk0").unwrap_err();
        assert!(err3.contains("\"code\":\"dirty_worktree\""), "err3={err3}");
        assert!(err3.contains("calc.py"), "err3={err3}");

        // 4. Clean working tree (ephemeral __pycache__ ignored) + simulated MERGE_HEAD -> in_progress_op
        run_git(&["checkout", "--", "calc.py"]);
        std::fs::create_dir_all(repo.join("__pycache__")).unwrap();
        std::fs::write(repo.join("__pycache__/calc.cpython-312.pyc"), "bin").unwrap();
        std::fs::write(repo.join(".git/MERGE_HEAD"), "0000000000000000000000000000000000000000\n").unwrap();
        let err4 = create_fork_worktree(&state, repo.to_str().unwrap(), "fk0").unwrap_err();
        assert!(err4.contains("\"code\":\"in_progress_op\""), "err4={err4}");
        assert!(err4.contains("\"op\":\"merge\""), "err4={err4}");
        std::fs::remove_file(repo.join(".git/MERGE_HEAD")).unwrap();

        // 5. Clean working tree -> succeeds directly off HEAD and merges back cleanly!
        let (wt, branch) = create_fork_worktree(&state, repo.to_str().unwrap(), "fk1").unwrap();
        assert_eq!(branch, "herdr/fork-fk1");
        assert_eq!(std::fs::read_to_string(Path::new(&wt).join("calc.py")).unwrap(), "print(1)\n");

        std::fs::write(Path::new(&wt).join("calc.py"), "print(1)\nprint(2)\n").unwrap();
        std::fs::write(Path::new(&wt).join("helper.py"), "ok = True\n").unwrap();
        let merged = merge_fork_worktree(repo.to_str().unwrap(), &wt, true).unwrap();
        assert!(merged.iter().any(|f| f == "calc.py"), "merged={merged:?}");
        assert!(merged.iter().any(|f| f == "helper.py"), "merged={merged:?}");
        assert_eq!(std::fs::read_to_string(repo.join("calc.py")).unwrap(), "print(1)\nprint(2)\n");
        assert!(!Path::new(&wt).exists(), "worktree dir should be cleaned up");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn drove_worktree_preflight_catches_unready_states_and_warns_on_dirty() {
        let base = std::env::temp_dir().join(format!("herdr2-drove-preflight-{}", super::super::crypto::rand_id("t")));
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let cwd = repo.to_str().unwrap();

        // 1. Non-git dir -> not_git_repo
        let b1 = check_drove_worktree_ready(cwd).unwrap_err();
        assert_eq!(b1.code, "not_git_repo");
        assert!(b1.fix_hint.contains("git init"));

        // 2. git init without commits -> no_commits
        let run_git = |args: &[&str]| {
            let out = super::super::bg_std_command("git")
                .args(["-C", cwd])
                .args(args)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@local")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@local")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run_git(&["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "hi\n").unwrap();
        let b2 = check_drove_worktree_ready(cwd).unwrap_err();
        assert_eq!(b2.code, "no_commits");

        // 3. After first commit -> clean OK
        run_git(&["add", "a.txt"]);
        run_git(&["commit", "-q", "-m", "init"]);
        let ok_clean = check_drove_worktree_ready(cwd).unwrap();
        assert_eq!(ok_clean.total_dirty, 0);
        assert!(ok_clean.dirty_warning.is_none());

        // 4. index.lock present -> git_locked
        std::fs::write(repo.join(".git/index.lock"), "").unwrap();
        let b3 = check_drove_worktree_ready(cwd).unwrap_err();
        assert_eq!(b3.code, "git_locked");
        std::fs::remove_file(repo.join(".git/index.lock")).unwrap();

        // 5. Nested unborn .git directory -> nested_unborn_git
        std::fs::create_dir_all(repo.join("sub/.git")).unwrap();
        let b4 = check_drove_worktree_ready(cwd).unwrap_err();
        assert_eq!(b4.code, "nested_unborn_git");
        std::fs::remove_dir_all(repo.join("sub")).unwrap();

        // 6. Dirty working tree -> OK with non-blocking dirty_warning
        std::fs::write(repo.join("a.txt"), "modified\n").unwrap();
        let ok_dirty = check_drove_worktree_ready(cwd).unwrap();
        assert_eq!(ok_dirty.total_dirty, 1);
        assert!(ok_dirty.dirty_warning.is_some());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn merge_worktree_handles_unstaged_dirty_base_and_preserves_index() {
        let base = std::env::temp_dir().join(format!("herdr2-wt-dirty-merge-{}", super::super::crypto::rand_id("t")));
        let repo = base.join("repo");
        let wt_parent = base.join("wts");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt_parent).unwrap();
        let cwd = repo.to_str().unwrap();

        let run_git = |args: &[&str]| -> String {
            let out = super::super::bg_std_command("git")
                .args(["-C", cwd])
                .args(args)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@local")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@local")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).to_string()
        };

        run_git(&["init", "-q"]);
        std::fs::write(repo.join("test_unified.py"), "line1\nline2\nline3\n").unwrap();
        std::fs::write(repo.join("other.py"), "a = 1\n").unwrap();
        run_git(&["add", "-A"]);
        run_git(&["commit", "-q", "-m", "init"]);

        // Simulate prior task editing `test_unified.py` in main workspace WITHOUT staging it
        // (index stays at old blob while working tree has new content).
        std::fs::write(repo.join("test_unified.py"), "line1\nline2_mlbb\nline3\n").unwrap();

        // Fork worktree from working-tree snapshot (same as prepare_task_worktree).
        let snap_commit = snapshot_working_tree_commit(cwd, &wt_parent).unwrap();
        let wt_dir = wt_parent.join("wt1");
        let wt_str = wt_dir.to_str().unwrap();
        run_git(&["worktree", "add", "--detach", wt_str, &snap_commit]);

        // Worker edits the already-dirty file and adds a new file.
        std::fs::write(wt_dir.join("test_unified.py"), "line1\nline2_mlbb\nline3\nline4_unit_test\n").unwrap();
        std::fs::write(wt_dir.join("new_test.py"), "def test_ok(): pass\n").unwrap();

        // Merge back MUST succeed (no `does not match index` deadlock) AND keep `.git/index` untouched!
        let merged = merge_worktree_blocking(cwd, wt_str, false).unwrap();
        assert!(merged.iter().any(|f| f == "test_unified.py"));
        assert!(merged.iter().any(|f| f == "new_test.py"));
        assert_eq!(
            std::fs::read_to_string(repo.join("test_unified.py")).unwrap(),
            "line1\nline2_mlbb\nline3\nline4_unit_test\n"
        );
        let status = run_git(&["status", "--porcelain"]);
        assert!(status.contains(" M test_unified.py"), "index must remain unstaged: {status:?}");
        assert!(!status.contains("M  test_unified.py") && !status.contains("MM test_unified.py"), "index polluted: {status:?}");
        assert!(status.contains("?? new_test.py"), "new file should stay untracked, not staged: {status:?}");

        // Now test non-overlapping concurrent edits in BOTH main working tree and a second worktree.
        let snap2 = snapshot_working_tree_commit(cwd, &wt_parent).unwrap();
        let wt2_dir = wt_parent.join("wt2");
        let wt2_str = wt2_dir.to_str().unwrap();
        run_git(&["worktree", "add", "--detach", wt2_str, &snap2]);

        // Concurrent edit to top of file in main workspace, bottom of file in worktree.
        std::fs::write(repo.join("test_unified.py"), "line1_main_concurrent\nline2_mlbb\nline3\nline4_unit_test\n").unwrap();
        std::fs::write(wt2_dir.join("test_unified.py"), "line1\nline2_mlbb\nline3\nline4_unit_test\nline5_wt2\n").unwrap();
        let merged2 = merge_worktree_blocking(cwd, wt2_str, false).unwrap();
        assert_eq!(merged2, vec!["test_unified.py".to_string()]);
        assert_eq!(
            std::fs::read_to_string(repo.join("test_unified.py")).unwrap(),
            "line1_main_concurrent\nline2_mlbb\nline3\nline4_unit_test\nline5_wt2\n"
        );
        let status2 = run_git(&["status", "--porcelain"]);
        assert!(status2.contains(" M test_unified.py") && !status2.contains("MM "), "3-way merge must not touch index: {status2:?}");

        // True overlapping conflict must fail cleanly with 422 without leaving conflict markers in main.
        let snap3 = snapshot_working_tree_commit(cwd, &wt_parent).unwrap();
        let wt3_dir = wt_parent.join("wt3");
        let wt3_str = wt3_dir.to_str().unwrap();
        run_git(&["worktree", "add", "--detach", wt3_str, &snap3]);
        std::fs::write(repo.join("other.py"), "a = 200\n").unwrap();
        std::fs::write(wt3_dir.join("other.py"), "a = 300\n").unwrap();
        let err = merge_worktree_blocking(cwd, wt3_str, false).unwrap_err();
        assert_eq!(err.status, 422);
        assert_eq!(err.code, "merge_conflict");
        assert_eq!(std::fs::read_to_string(repo.join("other.py")).unwrap(), "a = 200\n");

        let _ = std::fs::remove_dir_all(&base);
    }
}
