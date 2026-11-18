use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::checkpoint::{RepoInfo, canonicalize_path, probe_repo};
use crate::runtime::State;

/// Another live process/agent sharing or nesting the workspace that prevents destructive file restore.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceBlocker {
    pub addr: String,
    pub kind: String,
    pub status: String,
    pub detail: String,
    pub cwd: String,
}

/// Three-tier workspace safety assessment for checkpoint file restoration.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum WorkspaceIsolation {
    /// Dedicated Git worktree with no co-located agents or shells: restore files directly.
    IsolatedWorktree,
    /// Main checkout or shared with idle shell panes only: allow restore with confirmation.
    ExclusiveConfirm { is_main_checkout: bool, idle_shells: Vec<String> },
    /// Shared or nested with another live agent or non-shell process (e.g. nvim, cargo, claude): hard-block file restore.
    SharedBlocked { blockers: Vec<WorkspaceBlocker> },
}

/// Snapshot of a live target captured while briefly holding `Core`'s mutex.
#[derive(Clone, Debug)]
pub struct LiveTargetSnapshot {
    pub addr: String,
    pub kind: &'static str,
    pub cwd: String,
    pub parent: Option<String>,
    pub status_state: Option<String>,
    pub foreground_name: Option<String>,
    pub argv0: Option<String>,
}

/// Collects live ACP agents and PTY terminals from `State` for lock-free safety evaluation.
pub fn snapshot_live_targets(st: &State) -> Vec<LiveTargetSnapshot> {
    let mut out = Vec::new();
    for a in st.reg.agents.values() {
        if !a.is_live() {
            continue;
        }
        out.push(LiveTargetSnapshot {
            addr: a.address.clone(),
            kind: "agent",
            cwd: a.cwd.clone(),
            parent: a.parent.clone(),
            status_state: a.status.as_ref().map(|s| s.state.clone()),
            foreground_name: None,
            argv0: a.argv.first().cloned(),
        });
    }
    for t in st.pty_reg.terminals.values() {
        if !t.is_live() {
            continue;
        }
        let cwd = t
            .foreground
            .as_ref()
            .map(|f| f.cwd.as_str())
            .filter(|s| !s.is_empty())
            .or(t.last_cwd.as_deref())
            .unwrap_or(&t.cwd)
            .to_string();
        out.push(LiveTargetSnapshot {
            addr: t.address.clone(),
            kind: "terminal",
            cwd,
            parent: t.parent.clone(),
            status_state: t.status.as_ref().map(|s| s.state.clone()),
            foreground_name: t.foreground.as_ref().map(|f| f.name.clone()),
            argv0: t.argv.first().cloned(),
        });
    }
    out
}

/// Evaluates whether restoring `repo`'s working tree on behalf of `target_addr` is safe.
pub fn evaluate_isolation(
    target_addr: &str,
    repo: &RepoInfo,
    live_targets: &[LiveTargetSnapshot],
) -> WorkspaceIsolation {
    let target_root = canonicalize_path(&repo.toplevel);
    let mut blockers = Vec::new();
    let mut idle_shells = Vec::new();

    for other in live_targets {
        if other.addr == target_addr {
            continue;
        }
        let other_cwd = canonicalize_path(Path::new(&other.cwd));
        if !paths_share_working_tree(&target_root, repo, &other_cwd) {
            continue;
        }

        if other.kind == "agent" {
            let state = other.status_state.as_deref().unwrap_or("idle");
            let is_direct_child = other.parent.as_deref() == Some(target_addr);
            if !matches!(state, "working" | "blocked") {
                if !is_direct_child {
                    idle_shells.push(other.addr.clone());
                }
                continue;
            }
            blockers.push(WorkspaceBlocker {
                addr: other.addr.clone(),
                kind: other.kind.to_string(),
                status: state.to_string(),
                detail: format!("active ACP agent {} ({state})", other.addr),
                cwd: other.cwd.clone(),
            });
            continue;
        }

        // Terminal classification: distinguish idle shells from active CLIs, editors, and foreground commands.
        let status_state = other.status_state.as_deref();
        let active_status = matches!(status_state, Some("working" | "blocked"));
        if let Some(fg_name) = other.foreground_name.as_deref() {
            let stem = exe_stem(fg_name);
            if is_shell_name(&stem) && !active_status {
                idle_shells.push(other.addr.clone());
            } else {
                let status = if is_editor_name(&stem) { "editor" } else { status_state.unwrap_or("active") };
                blockers.push(WorkspaceBlocker {
                    addr: other.addr.clone(),
                    kind: other.kind.to_string(),
                    status: status.to_string(),
                    detail: format!("{} running {fg_name}", other.addr),
                    cwd: other.cwd.clone(),
                });
            }
        } else {
            let cmd_stem = other.argv0.as_deref().map(exe_stem).unwrap_or_default();
            let is_shell_launch = cmd_stem.is_empty() || is_shell_name(&cmd_stem);
            if is_shell_launch && !active_status {
                idle_shells.push(other.addr.clone());
            } else {
                let detail_cmd = other.argv0.as_deref().unwrap_or("terminal");
                blockers.push(WorkspaceBlocker {
                    addr: other.addr.clone(),
                    kind: other.kind.to_string(),
                    status: status_state.unwrap_or("active").to_string(),
                    detail: format!("{} ({detail_cmd})", other.addr),
                    cwd: other.cwd.clone(),
                });
            }
        }
    }

    if !blockers.is_empty() {
        WorkspaceIsolation::SharedBlocked { blockers }
    } else if repo.is_worktree && idle_shells.is_empty() {
        WorkspaceIsolation::IsolatedWorktree
    } else {
        WorkspaceIsolation::ExclusiveConfirm { is_main_checkout: !repo.is_worktree, idle_shells }
    }
}

/// Validates restore safety for `target_addr` against live peers, mapping denials into typed RPC errors.
pub fn enforce_restore_safety(
    target_addr: &str,
    repo: &RepoInfo,
    live_targets: &[LiveTargetSnapshot],
    confirm: bool,
) -> Result<(), crate::rpc::RpcError> {
    match evaluate_isolation(target_addr, repo, live_targets) {
        WorkspaceIsolation::IsolatedWorktree => Ok(()),
        WorkspaceIsolation::ExclusiveConfirm { is_main_checkout, idle_shells } => {
            if confirm {
                return Ok(());
            }
            let why = if is_main_checkout {
                "workspace is the main repository checkout"
            } else if idle_shells.is_empty() {
                "workspace requires confirmation"
            } else {
                "workspace has other open shell panes"
            };
            Err(crate::rpc::RpcError::new(
                "confirm_required",
                format!("{why}; pass confirm=true to restore files for {target_addr}"),
                "pass --confirm (or --no-files for conversation-only rollback)",
            ))
        }
        WorkspaceIsolation::SharedBlocked { blockers } => {
            let names: Vec<String> = blockers.iter().map(|b| b.detail.clone()).collect();
            Err(crate::rpc::RpcError::new(
                "shared_workspace",
                format!("cannot restore files for {target_addr}; workspace is shared with {}", names.join(", ")),
                "stop co-located agents/editors or pass --no-files",
            ))
        }
    }
}

fn paths_share_working_tree(target_root: &Path, target_repo: &RepoInfo, other_cwd: &Path) -> bool {
    if other_cwd == target_root {
        return true;
    }
    if other_cwd.starts_with(target_root) {
        // A separate linked worktree located under `<main>/.worktrees/<wt>` has its own git_dir.
        if let Some(other_repo) = probe_repo(other_cwd)
            && other_repo.git_dir != target_repo.git_dir
        {
            return false;
        }
        return true;
    }
    if target_root.starts_with(other_cwd) {
        // When target IS a linked worktree, another pane sitting at the outer parent checkout does not collide.
        return !target_repo.is_worktree;
    }
    false
}

fn exe_stem(raw: &str) -> String {
    let base =
        PathBuf::from(raw).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| raw.to_string());
    let lower = base.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

fn is_shell_name(stem: &str) -> bool {
    matches!(
        stem,
        "sh" | "bash"
            | "zsh"
            | "fish"
            | "dash"
            | "ash"
            | "ksh"
            | "mksh"
            | "tcsh"
            | "csh"
            | "nu"
            | "nushell"
            | "elvish"
            | "xonsh"
            | "pwsh"
            | "powershell"
            | "cmd"
    )
}

fn is_editor_name(stem: &str) -> bool {
    matches!(stem, "nvim" | "vim" | "vi" | "hx" | "helix" | "emacs" | "nano" | "kak" | "micro" | "pico")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    fn init_repo_with_worktree(name: &str) -> (PathBuf, PathBuf) {
        let root = crate::test_tmp().join(format!("h2-safe-{name}-{}", crate::platform::random_hex()));
        let main_dir = root.join("main");
        let wt_dir = root.join("wt1");
        std::fs::create_dir_all(&main_dir).unwrap();
        git(&main_dir, &["init", "-b", "main"]);
        std::fs::write(main_dir.join("README.md"), "hi\n").unwrap();
        git(&main_dir, &["add", "."]);
        git(&main_dir, &["commit", "-m", "init"]);
        git(&main_dir, &["worktree", "add", "-b", "feat/wt1", &wt_dir.to_string_lossy()]);
        (root, wt_dir)
    }

    #[test]
    fn three_tier_isolation_levels_work_across_worktree_main_and_shared_agents() {
        let (root, wt_dir) = init_repo_with_worktree("tiers");
        let main_dir = root.join("main");
        let main_repo = probe_repo(&main_dir).unwrap();
        let wt_repo = probe_repo(&wt_dir).unwrap();
        assert!(!main_repo.is_worktree);
        assert!(wt_repo.is_worktree);

        // 1. Isolated worktree with no overlapping targets -> IsolatedWorktree.
        let main_agent = LiveTargetSnapshot {
            addr: "t1".into(),
            kind: "terminal",
            cwd: main_dir.to_string_lossy().into(),
            parent: None,
            status_state: Some("working".into()),
            foreground_name: Some("claude".into()),
            argv0: Some("claude".into()),
        };
        assert_eq!(
            evaluate_isolation("t2", &wt_repo, std::slice::from_ref(&main_agent)),
            WorkspaceIsolation::IsolatedWorktree
        );

        // 2. Main checkout shared only with an idle zsh shell -> ExclusiveConfirm.
        let idle_shell = LiveTargetSnapshot {
            addr: "t3".into(),
            kind: "terminal",
            cwd: main_dir.join("sub").to_string_lossy().into(),
            parent: None,
            status_state: Some("idle".into()),
            foreground_name: Some("/bin/zsh".into()),
            argv0: Some("zsh".into()),
        };
        assert_eq!(
            evaluate_isolation("t1", &main_repo, std::slice::from_ref(&idle_shell)),
            WorkspaceIsolation::ExclusiveConfirm { is_main_checkout: true, idle_shells: vec!["t3".into()] }
        );

        // 3. Same directory shared with active nvim / another claude agent -> SharedBlocked.
        let editor_pane = LiveTargetSnapshot {
            addr: "t4".into(),
            kind: "terminal",
            cwd: main_dir.to_string_lossy().into(),
            parent: None,
            status_state: None,
            foreground_name: Some("nvim".into()),
            argv0: Some("zsh".into()),
        };
        let blocked = evaluate_isolation("t1", &main_repo, &[idle_shell, editor_pane]);
        assert!(
            matches!(blocked, WorkspaceIsolation::SharedBlocked { blockers } if blockers.len() == 1 && blockers[0].addr == "t4" && blockers[0].status == "editor")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn done_child_subagent_does_not_block_parent_while_working_child_does() {
        let (root, wt_dir) = init_repo_with_worktree("child");
        let wt_repo = probe_repo(&wt_dir).unwrap();

        let done_child = LiveTargetSnapshot {
            addr: "a1".into(),
            kind: "agent",
            cwd: wt_dir.to_string_lossy().into(),
            parent: Some("t1".into()),
            status_state: Some("done".into()),
            foreground_name: None,
            argv0: Some("claude-code-acp".into()),
        };
        assert_eq!(evaluate_isolation("t1", &wt_repo, &[done_child]), WorkspaceIsolation::IsolatedWorktree);

        let working_child = LiveTargetSnapshot {
            addr: "a2".into(),
            kind: "agent",
            cwd: wt_dir.to_string_lossy().into(),
            parent: Some("t1".into()),
            status_state: Some("working".into()),
            foreground_name: None,
            argv0: Some("claude-code-acp".into()),
        };
        assert!(matches!(
            evaluate_isolation("t1", &wt_repo, &[working_child]),
            WorkspaceIsolation::SharedBlocked { blockers } if blockers.len() == 1 && blockers[0].addr == "a2"
        ));

        // Idle sibling ACP agent allows rollback with confirmation, while working sibling blocks.
        let idle_sibling = LiveTargetSnapshot {
            addr: "a3".into(),
            kind: "agent",
            cwd: wt_dir.to_string_lossy().into(),
            parent: None,
            status_state: Some("idle".into()),
            foreground_name: None,
            argv0: Some("claude-code-acp".into()),
        };
        assert_eq!(
            evaluate_isolation("t1", &wt_repo, &[idle_sibling]),
            WorkspaceIsolation::ExclusiveConfirm { is_main_checkout: false, idle_shells: vec!["a3".into()] }
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
