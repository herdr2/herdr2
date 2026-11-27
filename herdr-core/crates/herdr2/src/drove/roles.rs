//! Role access guardrails: dirty baseline snapshotting, file-scope matching, and violation checks.

use super::crypto::sha256_hex;
use super::types::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

const PLAIN_DIR_MAX_FILES: usize = 32_768;

pub(super) const DIFF_CACHE_EXCLUDES: [&str; 6] = [
    ":(exclude)**/__pycache__/**",
    ":(exclude)**/*.pyc",
    ":(exclude)**/*.pyo",
    ":(exclude)**/.pytest_cache/**",
    ":(exclude)**/.mypy_cache/**",
    ":(exclude)**/.ruff_cache/**",
];

#[derive(Clone, Debug, Default)]
pub struct DirtyBaseline {
    pub is_git: bool,
    pub hashes: BTreeMap<String, String>,
    pub tree_sha: Option<String>,
    pub truncated: bool,
}

/// Captures pre-existing dirty/untracked file hashes and an index-free Git working-tree SHA
/// before a worker starts executing an attempt.
pub fn capture_dirty_baseline(cwd: &str) -> DirtyBaseline {
    if is_git_repo(cwd) {
        let mut hashes = BTreeMap::new();
        for rel in git_status_paths(cwd) {
            hashes.insert(rel.clone(), file_digest_or_tombstone(Path::new(cwd).join(&rel)));
        }
        let tree_sha = compute_git_working_tree_sha(cwd);
        DirtyBaseline { is_git: true, hashes, tree_sha, truncated: false }
    } else {
        let (hashes, truncated) = scan_plain_dir_hashes(Path::new(cwd));
        DirtyBaseline { is_git: false, hashes, tree_sha: None, truncated }
    }
}

/// Computes a side-effect-free Git tree SHA of `cwd` (including unstaged edits and non-ignored
/// untracked files, excluding ephemeral bytecode caches) using a temporary `GIT_INDEX_FILE` so
/// the repository's real `.git/index` is never mutated.
pub(crate) fn compute_git_working_tree_sha(cwd: &str) -> Option<String> {
    let git_dir_out = super::bg_std_command("git").args(["-C", cwd, "rev-parse", "--git-dir"]).output().ok()?;
    if !git_dir_out.status.success() {
        return None;
    }
    struct TempIdx(std::path::PathBuf);
    impl Drop for TempIdx {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let idx_path = std::env::temp_dir().join(format!("drove-idx-{}", super::crypto::rand_id("t")));
    let _ = std::fs::remove_file(&idx_path);
    let _guard = TempIdx(idx_path.clone());

    let has_head = super::bg_std_command("git")
        .args(["-C", cwd, "rev-parse", "--verify", "HEAD"])
        .output()
        .is_ok_and(|o| o.status.success());
    if has_head {
        let _ = super::bg_std_command("git")
            .args(["-C", cwd, "read-tree", "HEAD"])
            .env("GIT_INDEX_FILE", &idx_path)
            .output();
    }
    let mut add_cmd = super::bg_std_command("git");
    add_cmd.args(["-C", cwd, "add", "-A", "--", "."]);
    add_cmd.args(DIFF_CACHE_EXCLUDES);
    add_cmd.env("GIT_INDEX_FILE", &idx_path);
    if !add_cmd.output().is_ok_and(|o| o.status.success()) {
        return None;
    }
    let out =
        super::bg_std_command("git").args(["-C", cwd, "write-tree"]).env("GIT_INDEX_FILE", &idx_path).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Returns workspace-relative paths touched since `baseline`.
pub fn diff_touched_paths(cwd: &str, baseline: &DirtyBaseline) -> Vec<String> {
    if baseline.is_git || is_git_repo(cwd) {
        let current_paths = git_status_paths(cwd);
        let mut all: BTreeSet<String> = current_paths.into_iter().collect();
        for prev in baseline.hashes.keys() {
            all.insert(prev.clone());
        }
        let mut touched = Vec::new();
        for rel in all {
            let now_hash = file_digest_or_tombstone(Path::new(cwd).join(&rel));
            let in_git_now = Path::new(cwd).join(&rel).exists() || baseline.hashes.contains_key(&rel);
            match baseline.hashes.get(&rel) {
                None => {
                    if in_git_now {
                        touched.push(rel);
                    }
                }
                Some(prev_hash) => {
                    if prev_hash != &now_hash {
                        touched.push(rel);
                    }
                }
            }
        }
        touched
    } else {
        let (current, cur_truncated) = scan_plain_dir_hashes(Path::new(cwd));
        let any_truncated = baseline.truncated || cur_truncated;
        let mut touched = Vec::new();
        for (rel, cur_hash) in &current {
            match baseline.hashes.get(rel) {
                Some(prev_hash) if prev_hash == cur_hash => {}
                None if baseline.truncated => {
                    // File was beyond the baseline scan cap; compare against current disk state only if newly modified is unknown.
                    // Conservatively skip only if baseline was truncated and file existed before; otherwise check directly.
                    touched.push(rel.clone());
                }
                _ => touched.push(rel.clone()),
            }
        }
        for (rel, prev_hash) in &baseline.hashes {
            if !current.contains_key(rel) {
                if any_truncated {
                    // When scan hit the cap, check the actual file on disk instead of falsely assuming deletion.
                    let disk_hash = file_digest_or_tombstone(Path::new(cwd).join(rel));
                    if &disk_hash != prev_hash {
                        touched.push(rel.clone());
                    }
                } else {
                    touched.push(rel.clone());
                }
            }
        }
        touched.sort();
        touched.dedup();
        touched
    }
}

/// Checks whether `touched` paths violate `role_cfg.access` (`Readonly` or `ScopedWrite`).
/// Returns offending file paths when violated.
pub fn find_scope_violations(role_cfg: &RoleConfig, touched: &[String]) -> Vec<String> {
    let mode = role_cfg.access.unwrap_or(RoleAccessMode::Writable);
    match mode {
        RoleAccessMode::Writable => Vec::new(),
        RoleAccessMode::Readonly => touched.to_vec(),
        RoleAccessMode::ScopedWrite => {
            if role_cfg.allowed_paths.is_empty() {
                return touched.to_vec();
            }
            touched
                .iter()
                .filter(|rel| !role_cfg.allowed_paths.iter().any(|pat| matches_glob(pat, rel)))
                .cloned()
                .collect()
        }
    }
}

/// Evaluates candidate target file paths from a pre-execution tool call (`ACP request_permission`
/// or CLI `PreToolUse` hook) against `access` and `allowed_paths` anchored at `workspace_cwd`.
/// Returns `Err(reason)` immediately if any target path is out-of-bounds.
pub fn check_pre_tool_paths(
    workspace_cwd: &str,
    access: RoleAccessMode,
    allowed_paths: &[String],
    candidate_paths: &[String],
) -> Result<(), String> {
    match access {
        RoleAccessMode::Writable => Ok(()),
        RoleAccessMode::Readonly => {
            if candidate_paths.is_empty() {
                Err("Blocked by Drove readonly role guardrail: workspace mutations are prohibited.".into())
            } else {
                Err(format!(
                    "Blocked by Drove readonly role guardrail: cannot modify {}.",
                    candidate_paths.join(", ")
                ))
            }
        }
        RoleAccessMode::ScopedWrite => {
            if candidate_paths.is_empty() {
                return Ok(());
            }
            if allowed_paths.is_empty() {
                return Err("Blocked by Drove scoped_write guardrail: allowedPaths is empty.".into());
            }
            let root = Path::new(workspace_cwd);
            let mut violations = Vec::new();
            for raw in candidate_paths {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let p = Path::new(trimmed);
                let rel_opt = if p.is_absolute() {
                    p.strip_prefix(root).ok().map(|r| r.to_string_lossy().replace('\\', "/"))
                } else {
                    let norm = trimmed.trim_start_matches("./").replace('\\', "/");
                    if norm.starts_with("../") || norm == ".." {
                        None
                    } else {
                        Some(norm)
                    }
                };
                match rel_opt {
                    None => violations.push(trimmed.to_string()),
                    Some(rel) => {
                        if !allowed_paths.iter().any(|pat| matches_glob(pat, &rel)) {
                            violations.push(rel);
                        }
                    }
                }
            }
            if violations.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "Blocked by Drove scoped_write guardrail: [{}] is outside allowedPaths [{}]. Only edit files matching allowedPaths.",
                    violations.join(", "),
                    allowed_paths.join(", ")
                ))
            }
        }
    }
}

/// Extracts candidate target file paths from an ACP `session/request_permission` or CLI `PreToolUse` JSON payload.
pub fn extract_tool_target_paths(payload: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: &str| {
        let t = s.trim();
        if !t.is_empty() && !out.iter().any(|e| e == t) {
            out.push(t.to_string());
        }
    };

    if let Some(tc) = payload.get("toolCall") {
        if let Some(locs) = tc.get("locations").and_then(serde_json::Value::as_array) {
            for loc in locs {
                if let Some(s) = loc.as_str() {
                    push(s);
                } else if let Some(p) = loc.get("path").or_else(|| loc.get("uri")).and_then(serde_json::Value::as_str) {
                    push(p.strip_prefix("file://").unwrap_or(p));
                }
            }
        }
        if let Some(inp) = tc.get("rawInput").or_else(|| tc.get("input")) {
            collect_input_paths(inp, &mut push);
        }
    }
    if let Some(inp) = payload.get("tool_input").or_else(|| payload.get("input")) {
        collect_input_paths(inp, &mut push);
    }
    out
}

fn collect_input_paths(inp: &serde_json::Value, push: &mut impl FnMut(&str)) {
    let Some(obj) = inp.as_object() else { return };
    for key in ["file_path", "filePath", "path", "Path", "target_file", "TargetFile", "notebook_path"] {
        if let Some(s) = obj.get(key).and_then(serde_json::Value::as_str) {
            push(s);
        }
    }
    if let Some(files) = obj.get("files").and_then(serde_json::Value::as_array) {
        for f in files {
            if let Some(s) = f.as_str() {
                push(s);
            } else if let Some(s) = f.get("path").or_else(|| f.get("file_path")).and_then(serde_json::Value::as_str) {
                push(s);
            }
        }
    }
}

/// Matches a relative path (`a/b/c.rs`) against a gitignore-like glob (`src/**`, `tests/*.rs`, `*.md`).
pub fn matches_glob(pattern: &str, path: &str) -> bool {
    let pat = pattern.trim().trim_start_matches("./").replace('\\', "/");
    let p = path.trim().trim_start_matches("./").replace('\\', "/");
    if pat.is_empty() {
        return false;
    }
    let pat_chars: Vec<char> = pat.chars().collect();
    let path_chars: Vec<char> = p.chars().collect();
    glob_match_chars(&pat_chars, &path_chars)
}

fn glob_match_chars(pat: &[char], path: &[char]) -> bool {
    if pat.is_empty() {
        return path.is_empty();
    }
    if pat.len() >= 2 && pat[0] == '*' && pat[1] == '*' {
        // Consume `**/` or `**`.
        let mut next_pat = &pat[2..];
        if next_pat.first() == Some(&'/') {
            next_pat = &next_pat[1..];
        }
        if glob_match_chars(next_pat, path) {
            return true;
        }
        for i in 0..path.len() {
            if glob_match_chars(next_pat, &path[i + 1..]) {
                return true;
            }
        }
        return false;
    }
    match pat[0] {
        '*' => {
            if glob_match_chars(&pat[1..], path) {
                return true;
            }
            let mut i = 0;
            while i < path.len() && path[i] != '/' {
                if glob_match_chars(&pat[1..], &path[i + 1..]) {
                    return true;
                }
                i += 1;
            }
            false
        }
        '?' => !path.is_empty() && path[0] != '/' && glob_match_chars(&pat[1..], &path[1..]),
        c => !path.is_empty() && path[0] == c && glob_match_chars(&pat[1..], &path[1..]),
    }
}

/// Returns true for ephemeral bytecode/test cache files and directories that must not
/// trigger role scope violations or pollute worktree snapshots/patches.
pub(super) fn is_ephemeral_cache_path(rel: &str) -> bool {
    let norm = rel.trim_start_matches("./").replace('\\', "/");
    if norm.ends_with(".pyc") || norm.ends_with(".pyo") {
        return true;
    }
    norm.split('/').any(|seg| matches!(seg, "__pycache__" | ".pytest_cache" | ".mypy_cache" | ".ruff_cache"))
}

fn is_git_repo(cwd: &str) -> bool {
    super::bg_std_command("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn git_status_paths(cwd: &str) -> Vec<String> {
    let Ok(out) =
        super::bg_std_command("git").args(["status", "--porcelain=v1", "-z", "-uall"]).current_dir(cwd).output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let mut paths = Vec::new();
    let mut it = out.stdout.split(|b| *b == 0);
    while let Some(rec) = it.next() {
        if rec.len() < 4 {
            continue;
        }
        let code = &rec[..2];
        let rel = String::from_utf8_lossy(&rec[3..]).replace('\\', "/");
        if !rel.is_empty() && !(code == b"??" && is_ephemeral_cache_path(&rel)) {
            paths.push(rel);
        }
        if code[0] == b'R' || code[0] == b'C' {
            if let Some(orig) = it.next() {
                let o = String::from_utf8_lossy(orig).replace('\\', "/");
                if !o.is_empty() {
                    paths.push(o);
                }
            }
        }
    }
    paths
}

fn file_digest_or_tombstone(path: impl AsRef<Path>) -> String {
    match std::fs::read(path) {
        Ok(bytes) => sha256_hex(&bytes),
        Err(_) => "<deleted>".into(),
    }
}

fn is_ignored_plain_dir_name(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | "out"
            | ".next"
            | ".nuxt"
            | ".venv"
            | "venv"
            | "vendor"
            | ".gradle"
            | ".idea"
            | ".vscode"
            | "__pycache__"
            | ".pytest_cache"
            | ".mypy_cache"
            | ".ruff_cache"
            | ".tox"
            | "coverage"
    )
}

fn scan_plain_dir_hashes(root: &Path) -> (BTreeMap<String, String>, bool) {
    let mut out = BTreeMap::new();
    let mut queue = VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let p = entry.path();
            let name = entry.file_name();
            let ns = name.to_string_lossy();
            if is_ignored_plain_dir_name(ns.as_ref()) {
                continue;
            }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                queue.push_back(p);
            } else if ft.is_file()
                && let Ok(rel) = p.strip_prefix(root)
            {
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                if is_ephemeral_cache_path(&rel_str) {
                    continue;
                }
                out.insert(rel_str, file_digest_or_tombstone(&p));
                if out.len() >= PLAIN_DIR_MAX_FILES {
                    return (out, true);
                }
            }
        }
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_tests_and_wildcard_subtrees() {
        assert!(matches_glob("tests/**", "tests/unit/foo.rs"));
        assert!(matches_glob("tests/**/*.rs", "tests/foo.rs"));
        assert!(matches_glob("*.test.ts", "foo.test.ts"));
        assert!(!matches_glob("*.test.ts", "src/foo.test.ts"));
        assert!(!matches_glob("docs/**", "src/lib.rs"));
    }

    #[test]
    fn ephemeral_cache_paths_are_detected() {
        assert!(is_ephemeral_cache_path("src/__pycache__/kvstore.cpython-312.pyc"));
        assert!(is_ephemeral_cache_path("./tests/__pycache__/"));
        assert!(is_ephemeral_cache_path(".pytest_cache/v/cache/nodeids"));
        assert!(is_ephemeral_cache_path("foo.pyo"));
        assert!(!is_ephemeral_cache_path("src/kvstore.py"));
        assert!(!is_ephemeral_cache_path("tests/test_kvstore.py"));
    }

    #[test]
    fn plain_dir_scan_ignores_build_and_venv_dirs() {
        let tmp = std::env::temp_dir().join(format!("herdr2-plain-scan-{}", super::super::crypto::rand_id("t")));
        std::fs::create_dir_all(tmp.join(".venv/lib")).unwrap();
        std::fs::create_dir_all(tmp.join("dist")).unwrap();
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("src/main.rs"), "fn main() {}\n").unwrap();
        let baseline = capture_dirty_baseline(tmp.to_str().unwrap());
        assert!(!baseline.is_git);

        // Creating files inside ignored `.venv/` or `dist/` must not register as touched workspace files.
        std::fs::write(tmp.join(".venv/lib/site.py"), "x = 1\n").unwrap();
        std::fs::write(tmp.join("dist/bundle.js"), "console.log(1);\n").unwrap();
        assert!(diff_touched_paths(tmp.to_str().unwrap(), &baseline).is_empty());

        std::fs::write(tmp.join("src/main.rs"), "fn main() { println!(\"hi\"); }\n").unwrap();
        assert_eq!(diff_touched_paths(tmp.to_str().unwrap(), &baseline), vec!["src/main.rs"]);
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn pre_tool_path_guard_blocks_out_of_scope_and_allows_scoped_paths() {
        let allowed = vec!["tests/**".to_string(), "**/*_test.rs".to_string()];
        assert!(
            check_pre_tool_paths("/repo", RoleAccessMode::ScopedWrite, &allowed, &["/repo/tests/api.rs".into()])
                .is_ok()
        );
        assert!(
            check_pre_tool_paths("/repo", RoleAccessMode::ScopedWrite, &allowed, &["src/foo_test.rs".into()]).is_ok()
        );
        let err =
            check_pre_tool_paths("/repo", RoleAccessMode::ScopedWrite, &allowed, &["/repo/src/lib.rs".into()])
                .unwrap_err();
        assert!(err.contains("outside allowedPaths"));

        let ro_err =
            check_pre_tool_paths("/repo", RoleAccessMode::Readonly, &[], &["tests/api.rs".into()]).unwrap_err();
        assert!(ro_err.contains("readonly"));

        let payload = serde_json::json!({
            "toolCall": {
                "kind": "edit",
                "locations": [{ "path": "/repo/src/lib.rs" }],
                "rawInput": { "file_path": "/repo/src/main.rs" }
            }
        });
        assert_eq!(extract_tool_target_paths(&payload), vec!["/repo/src/lib.rs", "/repo/src/main.rs"]);
    }
}
