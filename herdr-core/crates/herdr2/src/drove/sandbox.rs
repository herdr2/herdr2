//! Process launch sandboxing for Drove workers using Linux `bwrap` when available.

use super::config::DrovePaths;
use super::types::RoleAccessMode;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

static BWRAP_USABLE: OnceLock<bool> = OnceLock::new();

/// Probes once whether `bwrap` exists in `PATH` and user namespaces permit mounting.
pub fn bwrap_available() -> bool {
    *BWRAP_USABLE.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return false;
        }
        Command::new("bwrap")
            .args(["--dev-bind", "/", "/", "--", "true"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Wraps worker ACP argv with `bwrap` to mask Drove secrets (`ai.key`, `coordinator.token`)
/// and enforce read-only workspace mounts when `access == RoleAccessMode::Readonly`.
pub fn wrap_worker_argv(
    argv: &[String],
    paths: &DrovePaths,
    cwd: &str,
    access: RoleAccessMode,
    isolate_secrets: bool,
) -> (Vec<String>, &'static str) {
    if argv.is_empty() || !isolate_secrets || !bwrap_available() {
        return (argv.to_vec(), "none");
    }

    let _ = paths.ensure_dirs();
    let state_dir = canonical_or_raw(&paths.state_dir);
    let secrets_dir = canonical_or_raw(&paths.secrets_dir);
    let cfg_file = canonical_or_raw(&paths.config_file);
    let cwd_path = canonical_or_raw(Path::new(cwd));

    // Avoid binding root `/` as tmpfs if something went wrong with paths.
    if secrets_dir == "/" || state_dir == "/" {
        return (argv.to_vec(), "none");
    }

    let mut out = vec![
        "bwrap".to_string(),
        "--dev-bind".into(),
        "/".into(),
        "/".into(),
        "--ro-bind".into(),
        state_dir.clone(),
        state_dir.clone(),
    ];

    if Path::new(&cfg_file).exists() {
        out.extend(["--ro-bind".into(), cfg_file.clone(), cfg_file]);
    }

    if cwd_path != "/" && Path::new(&cwd_path).is_dir() {
        if access == RoleAccessMode::Readonly {
            out.extend(["--ro-bind".into(), cwd_path.clone(), cwd_path]);
        } else if cwd_path.starts_with(&state_dir) {
            // Worktrees live inside state_dir/worktrees; re-bind writable after state_dir --ro-bind.
            out.extend(["--bind".into(), cwd_path.clone(), cwd_path]);
        }
    }

    // Mask secrets last so neither --dev-bind / nor writable cwd can reveal coordinator/AI secrets.
    out.extend([
        "--tmpfs".into(),
        secrets_dir,
        "--unshare-pid".into(),
        "--proc".into(),
        "/proc".into(),
        "--die-with-parent".into(),
        "--".into(),
    ]);
    out.extend(argv.iter().cloned());
    (out, "bwrap")
}

fn canonical_or_raw(p: &Path) -> String {
    p.canonicalize()
        .unwrap_or_else(|_| p.to_path_buf())
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;

    #[test]
    fn wrap_falls_back_cleanly_when_isolation_disabled() {
        let base = std::env::temp_dir().join("drove-test-sb");
        let p = Paths {
            socket: base.join("s.sock"),
            holder_socket: base.join("h.sock"),
            state: base.join("state"),
            config: base.join("config/config.toml"),
        };
        let paths = DrovePaths::from_paths(&p);
        let argv = vec!["python3".into(), "agent.py".into()];
        let (out, mode) = wrap_worker_argv(&argv, &paths, "/tmp", RoleAccessMode::Writable, false);
        assert_eq!(out, argv);
        assert_eq!(mode, "none");
    }
}
