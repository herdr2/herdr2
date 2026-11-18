//! Role access guardrails: dirty baseline snapshotting, file-scope matching, and violation checks.

use super::crypto::sha256_hex;
use super::types::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct DirtyBaseline {
    pub is_git: bool,
    pub hashes: BTreeMap<String, String>,
}

/// Captures pre-existing dirty/untracked file hashes in Git repos (or file hashes in plain dirs)
/// before a worker starts executing an attempt.
pub fn capture_dirty_baseline(cwd: &str) -> DirtyBaseline {
    if is_git_repo(cwd) {
        let mut hashes = BTreeMap::new();
        for rel in git_status_paths(cwd) {
            hashes.insert(rel.clone(), file_digest_or_tombstone(Path::new(cwd).join(&rel)));
        }
        DirtyBaseline { is_git: true, hashes }
    } else {
        DirtyBaseline {
            is_git: false,
            hashes: scan_plain_dir_hashes(Path::new(cwd)),
        }
    }
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
        let current = scan_plain_dir_hashes(Path::new(cwd));
        let mut touched = Vec::new();
        for (rel, cur_hash) in &current {
            if baseline.hashes.get(rel) != Some(cur_hash) {
                touched.push(rel.clone());
            }
        }
        for rel in baseline.hashes.keys() {
            if !current.contains_key(rel) {
                touched.push(rel.clone());
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
    norm.split('/').any(|seg| {
        matches!(
            seg,
            "__pycache__" | ".pytest_cache" | ".mypy_cache" | ".ruff_cache"
        )
    })
}

fn is_git_repo(cwd: &str) -> bool {
    super::bg_std_command("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn git_status_paths(cwd: &str) -> Vec<String> {
    let Ok(out) = super::bg_std_command("git")
        .args(["status", "--porcelain=v1", "-z", "-uall"])
        .current_dir(cwd)
        .output()
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

fn scan_plain_dir_hashes(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            let name = entry.file_name();
            let ns = name.to_string_lossy();
            if matches!(
                ns.as_ref(),
                ".git" | "node_modules" | "target" | "__pycache__" | ".pytest_cache" | ".mypy_cache" | ".ruff_cache"
            ) {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if p.is_file()
                && let Ok(rel) = p.strip_prefix(root)
            {
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                if is_ephemeral_cache_path(&rel_str) {
                    continue;
                }
                out.insert(rel_str, file_digest_or_tombstone(&p));
                if out.len() >= 4096 {
                    return out;
                }
            }
        }
    }
    out
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
}
