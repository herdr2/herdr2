use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const MAX_PATCH_BYTES: usize = 512 * 1024;
const MAX_PATHSPEC_BATCH_BYTES: usize = 24_000;
const GIT_AUTHOR_NAME: &str = "Herdr2";
const GIT_AUTHOR_EMAIL: &str = "herdr2@local";

/// Per-file diff summary between two checkpoints.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct CheckpointFileStat {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub added: u64,
    pub deleted: u64,
    #[serde(default)]
    pub binary: bool,
}

/// Persisted metadata for a single turn/manual checkpoint.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct CheckpointMeta {
    pub turn: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_turn: Option<u64>,
    pub ref_name: String,
    pub commit_oid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Origin of `label`: `"manual"` (user bookmark) or `"ai"` (async semantic milestone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_source: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<CheckpointFileStat>,
    pub ts_ms: u64,
}

impl CheckpointMeta {
    /// Returns true for user-created manual bookmarks (explicit `--label` or legacy labeled
    /// snapshots where `label_source` is not `"ai"`).
    pub fn is_manual_bookmark(&self) -> bool {
        self.label.is_some() && self.label_source.as_deref() != Some("ai")
    }
}

/// On-disk per-target checkpoint registry (`<state>/checkpoints/<addr>.json`).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TargetCheckpoints {
    pub addr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_transcript_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_fg_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_handoff: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_rollback_turn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_num: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pre_turn_refs: BTreeMap<u64, String>,
    #[serde(default)]
    pub checkpoints: Vec<CheckpointMeta>,
}

/// Response returned by `checkpoint.rollback`.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct CheckpointRollbackResult {
    pub target: String,
    pub turn: u64,
    pub restored_files: bool,
    pub rewound_session: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_prompt: Option<String>,
}

/// Unified diff payload between two checkpoints.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct CheckpointDiff {
    pub from_turn: u64,
    pub to_turn: u64,
    pub from_ref: String,
    pub to_ref: String,
    pub files: Vec<CheckpointFileStat>,
    pub patch: String,
    pub truncated: bool,
}

/// Discovered Git repository coordinates for a workspace directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoInfo {
    pub toplevel: PathBuf,
    pub common_dir: PathBuf,
    pub index_path: PathBuf,
    pub git_dir: PathBuf,
    pub head_oid: Option<String>,
    pub is_worktree: bool,
}

/// Generates a lowercase RFC 4122 v4 UUID using the platform CSPRNG.
pub fn random_uuid_v4() -> String {
    let hex = crate::platform::random_hex();
    let mut bytes = [0u8; 16];
    for (idx, chunk) in hex.as_bytes().chunks_exact(2).take(16).enumerate() {
        let s = std::str::from_utf8(chunk).unwrap_or("00");
        bytes[idx] = u8::from_str_radix(s, 16).unwrap_or(0);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

/// Sanitizes an arbitrary identifier into a single safe git-ref component.
pub fn slug_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches(['_', '-']);
    if trimmed.is_empty() { "default".to_string() } else { trimmed.to_string() }
}

/// Builds the hidden checkpoint ref name for `<session_slug>/<addr>/<turn>`.
pub fn checkpoint_ref(session_slug: &str, addr: &str, turn: u64) -> String {
    format!("refs/herdr2/checkpoints/{}/{}/{}", slug_segment(session_slug), slug_segment(addr), turn)
}

/// Builds the hidden pre-turn baseline ref name for `<session_slug>/<addr>/pre-<turn>`.
pub fn pre_turn_checkpoint_ref(session_slug: &str, addr: &str, turn: u64) -> String {
    format!("refs/herdr2/checkpoints/{}/{}/pre-{}", slug_segment(session_slug), slug_segment(addr), turn)
}

/// Prefix under `refs/herdr2/checkpoints/` owning all snapshots for a target.
pub fn checkpoint_prefix(session_slug: &str, addr: &str) -> String {
    format!("refs/herdr2/checkpoints/{}/{}/", slug_segment(session_slug), slug_segment(addr))
}

/// Normalizes a filesystem path, stripping Windows `\\?\` verbatim prefixes.
pub fn canonicalize_path(path: &Path) -> PathBuf {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    strip_verbatim_prefix(canon)
}

#[cfg(windows)]
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path
    }
}

#[cfg(not(windows))]
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    path
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn apply_safe_git_flags(cmd: &mut Command) {
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
}

fn run_git_bytes(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    stdin_bytes: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let mut cmd = crate::platform::bg_command("git");
    apply_safe_git_flags(&mut cmd);
    cmd.args(args).current_dir(cwd).stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin_bytes.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|e| format!("failed to spawn git {}: {e}", args.join(" ")))?;
    if let Some(input) = stdin_bytes
        && let Some(mut stdin) = child.stdin.take()
    {
        use std::io::Write as _;
        stdin.write_all(input).map_err(|e| format!("failed writing stdin to git {}: {e}", args.join(" ")))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("failed waiting for git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("git {} failed with status {}", args.join(" "), out.status)
        } else {
            format!("git {} failed: {stderr}", args.join(" "))
        });
    }
    Ok(out.stdout)
}

fn run_git_text(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String, String> {
    let bytes = run_git_bytes(cwd, args, env, None)?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

fn resolve_git_path(cwd: &Path, raw: &str) -> PathBuf {
    let p = PathBuf::from(raw.trim());
    let joined = if p.is_absolute() { p } else { cwd.join(p) };
    canonicalize_path(&joined)
}

/// Probes whether `cwd` resides inside a Git working tree and resolves key paths.
pub fn probe_repo(cwd: &Path) -> Option<RepoInfo> {
    if !cwd.exists() {
        return None;
    }
    let out = crate::platform::bg_command("git")
        .args([
            "-C",
            &cwd.to_string_lossy(),
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
            "--git-path",
            "index",
            "--git-dir",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let toplevel = resolve_git_path(cwd, lines.next()?);
    let common_dir = resolve_git_path(cwd, lines.next()?);
    let index_path = resolve_git_path(cwd, lines.next()?);
    let git_dir = resolve_git_path(cwd, lines.next()?);
    let head_oid =
        run_git_text(&toplevel, &["rev-parse", "--verify", "-q", "HEAD"], &[]).ok().filter(|s| !s.is_empty());
    let is_worktree = git_dir != common_dir;
    Some(RepoInfo { toplevel, common_dir, index_path, git_dir, head_oid, is_worktree })
}

struct TempIndexGuard {
    index_path: PathBuf,
    lock_path: PathBuf,
}

impl TempIndexGuard {
    fn new(common_dir: &Path) -> Self {
        let name = format!("herdr2-ckpt-index-{}", random_uuid_v4());
        let index_path = common_dir.join(&name);
        let lock_path = common_dir.join(format!("{name}.lock"));
        Self { index_path, lock_path }
    }
}

impl Drop for TempIndexGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.index_path);
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

fn is_sparse_checkout(repo: &RepoInfo) -> bool {
    run_git_text(&repo.toplevel, &["config", "--bool", "core.sparseCheckout"], &[]).ok().is_some_and(|v| v == "true")
}

/// Clears `assume-unchanged` / `skip-worktree` flags in `temp_index` so snapshots never miss edited files.
fn clear_cached_index_flags(repo: &RepoInfo, temp_index_str: &str, sparse: bool) {
    let Ok(bytes) = run_git_bytes(
        &repo.toplevel,
        &["ls-files", "--stage", "-v", "-z"],
        &[("GIT_INDEX_FILE", temp_index_str)],
        None,
    ) else {
        return;
    };
    let mut updates = Vec::new();
    for rec in bytes.split(|&b| b == 0) {
        if rec.len() < 3 || rec[1] != b' ' {
            continue;
        }
        let tag = rec[0];
        // Normal tracked files have tag 'H'; 'h' = assume-unchanged, 'S'/'s' = skip-worktree.
        if !matches!(tag, b'h' | b'S' | b's') {
            continue;
        }
        let rest = &rec[2..];
        if sparse && matches!(tag, b'S' | b's') {
            // Preserve sparse-checkout entries that intentionally do not exist on disk.
            if let Some(tab_pos) = rest.iter().position(|&b| b == b'\t') {
                let rel = String::from_utf8_lossy(&rest[tab_pos + 1..]);
                if !repo.toplevel.join(rel.as_ref()).exists() {
                    continue;
                }
            }
        }
        updates.extend_from_slice(rest);
        updates.push(0);
    }
    if !updates.is_empty() {
        let _ = run_git_bytes(
            &repo.toplevel,
            &["update-index", "-z", "--index-info"],
            &[("GIT_INDEX_FILE", temp_index_str)],
            Some(&updates),
        );
    }
}

/// Scans untracked directories for empty/unborn nested `.git` directories that make `git add -A` fail.
fn find_nested_empty_git_dirs(repo: &RepoInfo, temp_index_str: &str) -> Vec<String> {
    let Ok(bytes) = run_git_bytes(
        &repo.toplevel,
        &["ls-files", "--others", "--exclude-standard", "--directory", "-z"],
        &[("GIT_INDEX_FILE", temp_index_str)],
        None,
    ) else {
        return Vec::new();
    };
    let mut excludes = Vec::new();
    for rec in bytes.split(|&b| b == 0) {
        if rec.is_empty() {
            continue;
        }
        let rel = String::from_utf8_lossy(rec);
        let clean = rel.trim_end_matches('/');
        if clean.is_empty() {
            continue;
        }
        let candidate = repo.toplevel.join(clean);
        let dot_git = candidate.join(".git");
        if dot_git.is_dir() && !dot_git.join("HEAD").exists() {
            excludes.push(clean.to_string());
        }
    }
    excludes
}

/// Captures an exact working-tree snapshot into a hidden Git ref without touching `.git/index` or `HEAD`.
pub fn capture_checkpoint(repo: &RepoInfo, ref_name: &str) -> Result<(String, Option<String>, u64), String> {
    let head_oid =
        run_git_text(&repo.toplevel, &["rev-parse", "--verify", "-q", "HEAD"], &[]).ok().filter(|s| !s.is_empty());
    let guard = TempIndexGuard::new(&repo.common_dir);
    let temp_index_str = guard.index_path.to_string_lossy().to_string();
    let sparse = is_sparse_checkout(repo);

    if head_oid.is_some() && repo.index_path.is_file() {
        let real_mtime = std::fs::metadata(&repo.index_path).ok().and_then(|m| m.modified().ok());
        if std::fs::copy(&repo.index_path, &guard.index_path).is_ok() {
            run_git_bytes(
                &repo.toplevel,
                &["read-tree", "--reset", "HEAD"],
                &[("GIT_INDEX_FILE", &temp_index_str)],
                None,
            )?;
            // Backdate temp index mtime by 1s so racy-git entries written in the same second as real_index are still re-hashed.
            if let Some(target_mtime) = real_mtime.and_then(|t| t.checked_sub(Duration::from_secs(1)))
                && let Ok(f) = std::fs::OpenOptions::new().write(true).open(&guard.index_path)
            {
                let _ = f.set_modified(target_mtime);
            }
            clear_cached_index_flags(repo, &temp_index_str, sparse);
        } else {
            run_git_bytes(
                &repo.toplevel,
                &["read-tree", "--reset", "HEAD"],
                &[("GIT_INDEX_FILE", &temp_index_str)],
                None,
            )?;
        }
    } else if head_oid.is_some() {
        run_git_bytes(&repo.toplevel, &["read-tree", "--reset", "HEAD"], &[("GIT_INDEX_FILE", &temp_index_str)], None)?;
    }

    let add_res = if sparse {
        run_git_bytes(
            &repo.toplevel,
            &["add", "-A", "--sparse", "--", "."],
            &[("GIT_INDEX_FILE", &temp_index_str)],
            None,
        )
    } else {
        run_git_bytes(&repo.toplevel, &["add", "-A", "--", "."], &[("GIT_INDEX_FILE", &temp_index_str)], None)
    };
    if let Err(first_err) = add_res {
        let excludes = find_nested_empty_git_dirs(repo, &temp_index_str);
        if excludes.is_empty() {
            return Err(first_err);
        }
        let mut owned_args: Vec<String> = vec!["add".into(), "-A".into()];
        if sparse {
            owned_args.push("--sparse".into());
        }
        owned_args.push("--".into());
        owned_args.push(".".into());
        for dir in excludes {
            owned_args.push(format!(":(exclude,literal){dir}"));
        }
        let arg_refs: Vec<&str> = owned_args.iter().map(String::as_str).collect();
        run_git_bytes(&repo.toplevel, &arg_refs, &[("GIT_INDEX_FILE", &temp_index_str)], None)?;
    }

    let tree_oid = run_git_text(&repo.toplevel, &["write-tree"], &[("GIT_INDEX_FILE", &temp_index_str)])?;
    let ts_ms = now_ms();
    let msg = format!("herdr2 checkpoint ref={ref_name}");
    let mut commit_args = vec!["commit-tree", tree_oid.as_str()];
    if let Some(parent) = head_oid.as_deref() {
        commit_args.push("-p");
        commit_args.push(parent);
    }
    commit_args.push("-m");
    commit_args.push(&msg);
    let author_env = [
        ("GIT_AUTHOR_NAME", GIT_AUTHOR_NAME),
        ("GIT_AUTHOR_EMAIL", GIT_AUTHOR_EMAIL),
        ("GIT_COMMITTER_NAME", GIT_AUTHOR_NAME),
        ("GIT_COMMITTER_EMAIL", GIT_AUTHOR_EMAIL),
    ];
    let commit_oid = run_git_text(&repo.toplevel, &commit_args, &author_env)?;
    run_git_bytes(&repo.toplevel, &["update-ref", ref_name, &commit_oid], &[], None)?;
    Ok((commit_oid, head_oid, ts_ms))
}

fn resolve_commit(repo: &RepoInfo, ref_or_oid: &str) -> Result<String, String> {
    let spec = format!("{ref_or_oid}^{{commit}}");
    run_git_text(&repo.toplevel, &["rev-parse", "--verify", "-q", &spec], &[])
}

fn commit_parent_and_time(repo: &RepoInfo, commit_oid: &str) -> (Option<String>, u64) {
    let parent_spec = format!("{commit_oid}^");
    let parent = run_git_text(&repo.toplevel, &["rev-parse", "--verify", "-q", &parent_spec], &[])
        .ok()
        .filter(|s| !s.is_empty());
    let secs = run_git_text(&repo.toplevel, &["show", "-s", "--format=%ct", commit_oid], &[])
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    (parent, secs)
}

fn collect_nul_paths(bytes: &[u8], out: &mut BTreeSet<String>) {
    for slice in bytes.split(|&b| b == 0) {
        if !slice.is_empty() {
            out.insert(String::from_utf8_lossy(slice).to_string());
        }
    }
}

/// Computes paths touched by the agent (or conflict resolutions) when `HEAD` changed during a turn.
///
/// Returns `Ok(None)` when `start_head == end_head` (every path in the diff was authored in-turn).
pub fn list_authored_paths(
    repo: &RepoInfo,
    from_commit: &str,
    to_commit: &str,
) -> Result<Option<BTreeSet<String>>, String> {
    let (start_head, captured_at_secs) = commit_parent_and_time(repo, from_commit);
    let (end_head, _) = commit_parent_and_time(repo, to_commit);
    if start_head == end_head {
        return Ok(None);
    }

    let mut authored = BTreeSet::new();
    match start_head.as_deref() {
        Some(sh) => {
            let b = run_git_bytes(
                &repo.toplevel,
                &["-c", "diff.renames=false", "diff", "--name-only", "-z", sh, from_commit],
                &[],
                None,
            )?;
            collect_nul_paths(&b, &mut authored);
        }
        None => {
            let b = run_git_bytes(
                &repo.toplevel,
                &[
                    "-c",
                    "diff.renames=false",
                    "diff-tree",
                    "--root",
                    "--no-commit-id",
                    "--name-only",
                    "-r",
                    "-z",
                    from_commit,
                ],
                &[],
                None,
            )?;
            collect_nul_paths(&b, &mut authored);
        }
    }

    match end_head.as_deref() {
        Some(eh) => {
            let b = run_git_bytes(
                &repo.toplevel,
                &["-c", "diff.renames=false", "diff", "--name-only", "-z", eh, to_commit],
                &[],
                None,
            )?;
            collect_nul_paths(&b, &mut authored);
        }
        None => {
            let b = run_git_bytes(
                &repo.toplevel,
                &[
                    "-c",
                    "diff.renames=false",
                    "diff-tree",
                    "--root",
                    "--no-commit-id",
                    "--name-only",
                    "-r",
                    "-z",
                    to_commit,
                ],
                &[],
                None,
            )?;
            collect_nul_paths(&b, &mut authored);
        }
    }

    let since_arg = format!("--since=@{captured_at_secs}");
    match (start_head.as_deref(), end_head.as_deref()) {
        (Some(sh), Some(eh)) => {
            let rev_range = format!("{eh}..{sh}");
            if let Ok(b) = run_git_bytes(
                &repo.toplevel,
                &[
                    "-c",
                    "diff.renames=false",
                    "log",
                    "--format=",
                    "--diff-merges=remerge",
                    "--name-only",
                    "-z",
                    &rev_range,
                ],
                &[],
                None,
            ) {
                collect_nul_paths(&b, &mut authored);
            }
            let fwd_range = format!("{sh}..{eh}");
            if let Ok(b) = run_git_bytes(
                &repo.toplevel,
                &[
                    "-c",
                    "diff.renames=false",
                    "log",
                    "--format=",
                    "--diff-merges=remerge",
                    &since_arg,
                    "--name-only",
                    "-z",
                    &fwd_range,
                ],
                &[],
                None,
            ) {
                collect_nul_paths(&b, &mut authored);
            }
        }
        (None, Some(eh)) => {
            if let Ok(b) = run_git_bytes(
                &repo.toplevel,
                &[
                    "-c",
                    "diff.renames=false",
                    "log",
                    "--format=",
                    "--diff-merges=remerge",
                    &since_arg,
                    "--name-only",
                    "-z",
                    eh,
                ],
                &[],
                None,
            ) {
                collect_nul_paths(&b, &mut authored);
            }
        }
        (Some(sh), None) => {
            if let Ok(b) = run_git_bytes(
                &repo.toplevel,
                &["-c", "diff.renames=false", "log", "--format=", "--diff-merges=remerge", "--name-only", "-z", sh],
                &[],
                None,
            ) {
                collect_nul_paths(&b, &mut authored);
            }
        }
        (None, None) => {}
    }

    Ok(Some(authored))
}

fn parse_numstat_z(bytes: &[u8]) -> Vec<CheckpointFileStat> {
    let mut out = Vec::new();
    let mut parts = bytes.split(|&b| b == 0).peekable();
    while let Some(part) = parts.next() {
        if part.is_empty() {
            continue;
        }
        let line = String::from_utf8_lossy(part);
        let mut cols = line.splitn(3, '\t');
        let Some(add_s) = cols.next() else { continue };
        let Some(del_s) = cols.next() else { continue };
        let Some(rest) = cols.next() else { continue };
        let binary = add_s == "-" || del_s == "-";
        let added = add_s.parse::<u64>().unwrap_or(0);
        let deleted = del_s.parse::<u64>().unwrap_or(0);
        let path = if rest.is_empty() {
            // Rename record emits `\t\0old\0new\0`.
            let _old = parts.next();
            match parts.next() {
                Some(new_p) if !new_p.is_empty() => String::from_utf8_lossy(new_p).to_string(),
                _ => continue,
            }
        } else {
            rest.to_string()
        };
        out.push(CheckpointFileStat { path, status: None, added, deleted, binary });
    }
    out
}

fn compute_filtered_stats(
    repo: &RepoInfo,
    from_commit: &str,
    to_commit: &str,
) -> Result<(Option<BTreeSet<String>>, Vec<CheckpointFileStat>), String> {
    let authored = list_authored_paths(repo, from_commit, to_commit)?;
    let numstat_bytes = run_git_bytes(
        &repo.toplevel,
        &["-c", "diff.renames=false", "diff", "--numstat", "-z", "--no-color", "--no-ext-diff", from_commit, to_commit],
        &[],
        None,
    )?;
    let mut files = parse_numstat_z(&numstat_bytes);
    if let Some(filter) = &authored {
        files.retain(|f| filter.contains(&f.path));
    }
    Ok((authored, files))
}

/// Computes per-file stats between two checkpoint refs, excluding un-conflicted upstream imports.
pub fn summarize_turn_files(repo: &RepoInfo, from_ref: &str, to_ref: &str) -> Result<Vec<CheckpointFileStat>, String> {
    let from_commit = resolve_commit(repo, from_ref)?;
    let to_commit = resolve_commit(repo, to_ref)?;
    let (_, files) = compute_filtered_stats(repo, &from_commit, &to_commit)?;
    Ok(files)
}

/// Computes the pure authored Unified Diff and file stats between `from_ref` and `to_ref`.
pub fn diff_checkpoints(
    repo: &RepoInfo,
    from_turn: u64,
    to_turn: u64,
    from_ref: &str,
    to_ref: &str,
) -> Result<CheckpointDiff, String> {
    let from_commit = resolve_commit(repo, from_ref)?;
    let to_commit = resolve_commit(repo, to_ref)?;
    let (authored, files) = compute_filtered_stats(repo, &from_commit, &to_commit)?;
    if files.is_empty() {
        return Ok(CheckpointDiff {
            from_turn,
            to_turn,
            from_ref: from_ref.to_string(),
            to_ref: to_ref.to_string(),
            files,
            patch: String::new(),
            truncated: false,
        });
    }

    let mut patch = String::new();
    let mut truncated = false;

    if authored.is_none() {
        let raw = run_git_bytes(
            &repo.toplevel,
            &["-c", "diff.renames=false", "diff", "--patch", "--no-color", "--no-ext-diff", &from_commit, &to_commit],
            &[],
            None,
        )?;
        let text = String::from_utf8_lossy(&raw);
        truncated = append_capped(&mut patch, &text, MAX_PATCH_BYTES);
    } else {
        // Batch paths so argv length stays well under Windows CreateProcess 32 KiB limit.
        let mut idx = 0usize;
        while idx < files.len() && !truncated {
            let mut batch = Vec::new();
            let mut batch_bytes = 0usize;
            while idx < files.len() {
                let spec = format!(":(top,literal){}", files[idx].path);
                if !batch.is_empty() && batch_bytes + spec.len() + 1 > MAX_PATHSPEC_BATCH_BYTES {
                    break;
                }
                batch_bytes += spec.len() + 1;
                batch.push(spec);
                idx += 1;
            }
            let mut args = vec![
                "-c",
                "diff.renames=false",
                "diff",
                "--patch",
                "--no-color",
                "--no-ext-diff",
                &from_commit,
                &to_commit,
                "--",
            ];
            for spec in &batch {
                args.push(spec.as_str());
            }
            let raw = run_git_bytes(&repo.toplevel, &args, &[], None)?;
            let text = String::from_utf8_lossy(&raw);
            if append_capped(&mut patch, &text, MAX_PATCH_BYTES) {
                truncated = true;
            }
        }
    }

    Ok(CheckpointDiff {
        from_turn,
        to_turn,
        from_ref: from_ref.to_string(),
        to_ref: to_ref.to_string(),
        files,
        patch,
        truncated,
    })
}

fn append_capped(dst: &mut String, chunk: &str, max_bytes: usize) -> bool {
    let remaining = max_bytes.saturating_sub(dst.len());
    if chunk.len() <= remaining {
        dst.push_str(chunk);
        false
    } else {
        let mut end = remaining;
        while end > 0 && !chunk.is_char_boundary(end) {
            end -= 1;
        }
        dst.push_str(&chunk[..end]);
        true
    }
}

/// Restores working tree files back to `ref_name`, then aligns index back to real `HEAD`.
pub fn restore_checkpoint(repo: &RepoInfo, ref_name: &str, ensure_dir: Option<&Path>) -> Result<(), String> {
    let commit_oid = resolve_commit(repo, ref_name)?;
    let tree_files = run_git_bytes(
        &repo.toplevel,
        &["ls-files", "--cached", "--with-tree", &commit_oid, "-z", "--", "."],
        &[],
        None,
    )?;

    if !tree_files.is_empty() {
        run_git_bytes(
            &repo.toplevel,
            &["restore", "--source", &commit_oid, "--worktree", "--staged", "--", "."],
            &[],
            None,
        )?;
    } else {
        // Empty target snapshot: unstage tracked files so `git clean -fd` can clear the worktree.
        let _ = run_git_bytes(
            &repo.toplevel,
            &["rm", "-rf", "--cached", "--quiet", "--ignore-unmatch", "--", "."],
            &[],
            None,
        );
    }

    let _ = std::fs::create_dir_all(&repo.toplevel);
    if let Some(dir) = ensure_dir {
        let _ = std::fs::create_dir_all(dir);
    }

    // Up to 4 passes: removing a turn-added `.gitignore` exposes ignored files created in that directory.
    for _ in 0..4 {
        let cleaned = run_git_bytes(&repo.toplevel, &["clean", "-fd", "--", "."], &[("LC_ALL", "C")], None)?;
        if String::from_utf8_lossy(&cleaned).trim().is_empty() {
            break;
        }
    }

    let _ = std::fs::create_dir_all(&repo.toplevel);
    if let Some(dir) = ensure_dir {
        let _ = std::fs::create_dir_all(dir);
    }

    let head_exists =
        run_git_text(&repo.toplevel, &["rev-parse", "--verify", "-q", "HEAD"], &[]).ok().is_some_and(|s| !s.is_empty());
    if head_exists {
        run_git_bytes(&repo.toplevel, &["reset", "--quiet", "--", "."], &[], None)?;
    } else {
        let _ = run_git_bytes(&repo.toplevel, &["read-tree", "--empty"], &[], None);
    }

    Ok(())
}

/// Returns true if `ref_name` currently resolves to a valid object in `repo`.
pub fn ref_exists(repo: &RepoInfo, ref_name: &str) -> bool {
    !ref_name.trim().is_empty()
        && run_git_text(&repo.toplevel, &["rev-parse", "--verify", "-q", ref_name], &[])
            .ok()
            .is_some_and(|s| !s.is_empty())
}

/// Deletes an explicit list of hidden checkpoint ref names in `repo`.
pub fn delete_checkpoint_ref_names(repo: &RepoInfo, ref_names: &[String]) -> Result<(), String> {
    let mut stdin = String::new();
    for r in ref_names {
        let clean = r.trim();
        if clean.starts_with("refs/herdr2/checkpoints/") {
            stdin.push_str("delete ");
            stdin.push_str(clean);
            stdin.push('\n');
        }
    }
    if stdin.is_empty() {
        return Ok(());
    }
    run_git_bytes(&repo.toplevel, &["update-ref", "--stdin"], &[], Some(stdin.as_bytes()))?;
    Ok(())
}

/// Deletes hidden checkpoint refs under `<session_slug>/<addr>/` with `turn > after_turn`.
pub fn delete_checkpoint_refs_after(
    repo: &RepoInfo,
    session_slug: &str,
    addr: &str,
    after_turn: u64,
) -> Result<(), String> {
    delete_refs_matching(repo, session_slug, addr, |turn| turn > after_turn)
}

/// Deletes every hidden checkpoint ref under `<session_slug>/<addr>/`.
pub fn delete_all_checkpoint_refs(repo: &RepoInfo, session_slug: &str, addr: &str) -> Result<(), String> {
    delete_refs_matching(repo, session_slug, addr, |_| true)
}

fn delete_refs_matching<F>(repo: &RepoInfo, session_slug: &str, addr: &str, pred: F) -> Result<(), String>
where
    F: Fn(u64) -> bool,
{
    let prefix = checkpoint_prefix(session_slug, addr);
    let out = run_git_bytes(&repo.toplevel, &["for-each-ref", "--format=%(refname)", &prefix], &[], None)?;
    let text = String::from_utf8_lossy(&out);
    let mut stdin = String::new();
    for line in text.lines() {
        let r = line.trim();
        if let Some(turn_str) = r.strip_prefix(&prefix)
            && let Ok(turn) = turn_str.strip_prefix("pre-").unwrap_or(turn_str).parse::<u64>()
            && pred(turn)
        {
            stdin.push_str("delete ");
            stdin.push_str(r);
            stdin.push('\n');
        }
    }
    if stdin.is_empty() {
        return Ok(());
    }
    run_git_bytes(&repo.toplevel, &["update-ref", "--stdin"], &[], Some(stdin.as_bytes()))?;
    Ok(())
}

/// Captures a baseline (`turn = Some(0)`), explicit turn (`turn = Some(n)`), or manual (`turn = None`)
/// checkpoint for a live ACP agent or PTY terminal, persisting metadata and emitting events.
pub async fn capture_target_checkpoint(
    core: &std::sync::Arc<crate::runtime::Core>,
    target: crate::runtime::TargetKey,
    turn: Option<u64>,
    prompt: Option<String>,
    label: Option<String>,
    skip_if_present: bool,
    record_transcript_entry: bool,
) -> Result<Option<CheckpointMeta>, String> {
    capture_target_checkpoint_with_session_turn(
        core,
        target,
        turn,
        None,
        prompt,
        label,
        skip_if_present,
        record_transcript_entry,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn capture_target_checkpoint_with_session_turn(
    core: &std::sync::Arc<crate::runtime::Core>,
    target: crate::runtime::TargetKey,
    turn: Option<u64>,
    session_turn: Option<u64>,
    prompt: Option<String>,
    label: Option<String>,
    skip_if_present: bool,
    record_transcript_entry: bool,
) -> Result<Option<CheckpointMeta>, String> {
    use crate::runtime::TargetKey;

    let (addr, cwd, ckpt_turn, prev_ref, effective_prompt, session_slug, ref_owner) = {
        let mut st = core.lock();
        let (addr, cwd, target_turn) = match target {
            TargetKey::Agent(id) => {
                let Some(a) = st.reg.agents.get(&id) else {
                    return Ok(None);
                };
                (a.address.clone(), a.cwd.clone(), a.turn)
            }
            TargetKey::Terminal(id) => {
                let Some(t) = st.pty_reg.terminals.get(&id) else {
                    return Ok(None);
                };
                let cwd = t
                    .foreground
                    .as_ref()
                    .map(|f| f.cwd.as_str())
                    .filter(|s| !s.is_empty())
                    .or(t.last_cwd.as_deref())
                    .unwrap_or(&t.cwd)
                    .to_string();
                (t.address.clone(), cwd, t.turn)
            }
        };
        let mut tc = core.target_checkpoints(&mut st, &addr);
        let ckpt_turn = match turn {
            Some(t) => t,
            None => {
                let max_ckpt = tc.checkpoints.iter().map(|c| c.turn).max();
                if tc.checkpoints.iter().any(|c| c.turn == target_turn) {
                    max_ckpt.map_or(target_turn + 1, |m| m.max(target_turn) + 1)
                } else {
                    target_turn
                }
            }
        };
        if skip_if_present && tc.checkpoints.iter().any(|c| c.turn == ckpt_turn) {
            return Ok(None);
        }
        // Isolate CLI session Git refs per session so a fresh CLI session in the same terminal
        // never overwrites or deletes an archived earlier session's hidden refs.
        if matches!(target, TargetKey::Terminal(_))
            && tc.ref_namespace.is_none()
            && tc.checkpoints.is_empty()
            && tc.cli_kind.as_deref().is_some_and(|k| !k.is_empty() && k != "generic")
        {
            let suffix = &random_uuid_v4()[..8];
            tc.ref_namespace = Some(format!("{addr}-{suffix}"));
            core.save_target_checkpoints(&mut st, tc.clone());
        }
        let ref_owner = tc.ref_namespace.clone().unwrap_or_else(|| addr.clone());
        let prev_ref = tc.pre_turn_refs.get(&ckpt_turn).cloned().or_else(|| {
            tc.checkpoints.iter().filter(|c| c.turn < ckpt_turn).max_by_key(|c| c.turn).map(|c| c.ref_name.clone())
        });
        let effective_prompt = prompt.or_else(|| (ckpt_turn > 0).then(|| tc.active_turn_prompt.clone()).flatten());
        (addr, cwd, ckpt_turn, prev_ref, effective_prompt, core.session_slug(), ref_owner)
    };

    let Some(repo) = probe_repo(Path::new(&cwd)) else {
        return Ok(None);
    };

    let ref_name = checkpoint_ref(&session_slug, &ref_owner, ckpt_turn);
    let _ckpt_guard = core.repo_ckpt_lock(&repo.toplevel).lock_owned().await;

    let repo_clone = repo.clone();
    let ref_clone = ref_name.clone();
    let prev_ref_for_bg = prev_ref.clone();
    let (commit_oid, head_oid, files, ts_ms) = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let (commit_oid, head_oid, ts_ms) = capture_checkpoint(&repo_clone, &ref_clone)?;
        let files = if ckpt_turn > 0
            && let Some(prev) = prev_ref.as_deref()
        {
            summarize_turn_files(&repo_clone, prev, &ref_clone).unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok((commit_oid, head_oid, files, ts_ms))
    })
    .await
    .map_err(|e| format!("checkpoint task panicked: {e}"))??;

    let clean_label = label.and_then(|s| {
        let t = s.trim().to_string();
        (!t.is_empty()).then_some(t)
    });
    let label_source = clean_label.as_ref().map(|_| "manual".to_string());

    let meta = {
        let mut st = core.lock();
        let still_exists = match target {
            TargetKey::Agent(id) => st.reg.agents.contains_key(&id),
            TargetKey::Terminal(id) => st.pty_reg.terminals.contains_key(&id),
        };
        if !still_exists {
            return Ok(None);
        }

        let mut tc = core.target_checkpoints(&mut st, &addr);
        if skip_if_present && tc.checkpoints.iter().any(|c| c.turn == ckpt_turn) {
            return Ok(None);
        }

        let prompt_final = if ckpt_turn > 0 {
            let p = effective_prompt.or_else(|| tc.active_turn_prompt.clone());
            tc.active_turn_prompt = None;
            if tc.active_turn_num == Some(ckpt_turn) {
                tc.active_turn_num = None;
            }
            p
        } else {
            None
        };

        let meta = CheckpointMeta {
            turn: ckpt_turn,
            session_turn: (ckpt_turn > 0).then_some(session_turn).flatten(),
            ref_name: ref_name.clone(),
            commit_oid: commit_oid.clone(),
            head_oid: head_oid.clone(),
            prompt: prompt_final,
            label: clean_label,
            label_source,
            files: files.clone(),
            ts_ms,
        };

        tc.repo_root = Some(repo.toplevel.to_string_lossy().to_string());
        if let TargetKey::Terminal(id) = target
            && let Some(t) = st.pty_reg.terminals.get(&id)
            && let Some(fg) = &t.foreground
            && fg.pid > 0
            && Some(fg.pid as u32) != t.pid
        {
            tc.cli_fg_pid = Some(fg.pid as u32);
        }
        tc.checkpoints.retain(|c| c.turn != ckpt_turn);
        tc.checkpoints.push(meta.clone());
        tc.checkpoints.sort_by_key(|c| c.turn);
        core.save_target_checkpoints(&mut st, tc);

        match target {
            TargetKey::Agent(id) => {
                if turn.is_none()
                    && let Some(a) = st.reg.agents.get_mut(&id)
                    && ckpt_turn > a.turn
                {
                    a.turn = ckpt_turn;
                    core.save(&st);
                }
                if record_transcript_entry && ckpt_turn > 0 {
                    core.record_transcript(
                        &mut st,
                        id,
                        crate::transcript::Entry::Checkpoint {
                            turn: ckpt_turn,
                            ref_name: ref_name.clone(),
                            commit_oid: commit_oid.clone(),
                            head_oid,
                            files: files.clone(),
                            ts_ms,
                        },
                    );
                }
                core.emit(&mut st, "checkpoint", id, serde_json::json!({ "meta": &meta }));
            }
            TargetKey::Terminal(id) => {
                if turn.is_none()
                    && let Some(t) = st.pty_reg.terminals.get_mut(&id)
                    && ckpt_turn > t.turn
                {
                    t.turn = ckpt_turn;
                    core.save_pty(&st);
                }
                core.emit_pty(&mut st, "checkpoint", id, serde_json::json!({ "meta": &meta }));
            }
        }
        meta
    };

    // Out-of-lock async semantic milestone labeling for non-manual, non-empty turn checkpoints.
    if ckpt_turn > 0
        && meta.label.is_none()
        && !meta.files.is_empty()
        && let Some(prev_ref_bg) = prev_ref_for_bg
        && core.ai.checkpoint_labeling_enabled()
    {
        let core_bg = std::sync::Arc::clone(core);
        let addr_bg = addr.clone();
        let repo_bg = repo.clone();
        let ref_bg = ref_name.clone();
        let oid_bg = meta.commit_oid.clone();
        let prompt_bg = meta.prompt.clone();
        let files_bg = meta.files.clone();
        tokio::spawn(async move {
            let patch = {
                let _guard = core_bg.repo_ckpt_lock(&repo_bg.toplevel).lock_owned().await;
                let r_bg = repo_bg.clone();
                let from_r = prev_ref_bg;
                let to_r = ref_bg;
                tokio::task::spawn_blocking(move || {
                    diff_checkpoints(&r_bg, ckpt_turn.saturating_sub(1), ckpt_turn, &from_r, &to_r)
                        .map(|d| d.patch)
                        .unwrap_or_default()
                })
                .await
                .unwrap_or_default()
            };

            let Some(ai_label) = crate::ai::generate_checkpoint_label(
                &core_bg.ai,
                &oid_bg,
                ckpt_turn,
                prompt_bg.as_deref(),
                &files_bg,
                &patch,
            )
            .await
            else {
                return;
            };

            let mut st = core_bg.lock();
            let alive = match target {
                TargetKey::Agent(id) => st.reg.agents.contains_key(&id),
                TargetKey::Terminal(id) => st.pty_reg.terminals.contains_key(&id),
            };
            if !alive {
                return;
            }
            let mut tc = core_bg.target_checkpoints(&mut st, &addr_bg);
            let mut updated_meta = None;
            if let Some(entry) = tc.checkpoints.iter_mut().find(|c| c.turn == ckpt_turn)
                && entry.commit_oid == oid_bg
                && entry.label.is_none()
            {
                entry.label = Some(ai_label);
                entry.label_source = Some("ai".into());
                updated_meta = Some(entry.clone());
            }
            if let Some(m) = updated_meta {
                core_bg.save_target_checkpoints(&mut st, tc);
                match target {
                    TargetKey::Agent(id) => {
                        core_bg.emit(&mut st, "checkpoint", id, serde_json::json!({ "meta": &m }));
                    }
                    TargetKey::Terminal(id) => {
                        core_bg.emit_pty(&mut st, "checkpoint", id, serde_json::json!({ "meta": &m }));
                    }
                }
            }
        });
    }

    Ok(Some(meta))
}

/// Captures a lightweight pre-turn Git snapshot right before an ACP turn starts so idle-gap edits
/// made by other sessions or editors are never attributed to the upcoming turn's diff.
pub async fn capture_pre_turn_baseline(
    core: &std::sync::Arc<crate::runtime::Core>,
    target: crate::runtime::TargetKey,
    turn: u64,
) -> Result<Option<String>, String> {
    use crate::runtime::TargetKey;
    if turn == 0 {
        return Ok(None);
    }
    let (addr, cwd, session_slug, ref_owner) = {
        let mut st = core.lock();
        let (addr, cwd) = match target {
            TargetKey::Agent(id) => {
                let Some(a) = st.reg.agents.get(&id) else {
                    return Ok(None);
                };
                (a.address.clone(), a.cwd.clone())
            }
            TargetKey::Terminal(id) => {
                let Some(t) = st.pty_reg.terminals.get(&id) else {
                    return Ok(None);
                };
                let cwd = t
                    .foreground
                    .as_ref()
                    .map(|f| f.cwd.as_str())
                    .filter(|s| !s.is_empty())
                    .or(t.last_cwd.as_deref())
                    .unwrap_or(&t.cwd)
                    .to_string();
                (t.address.clone(), cwd)
            }
        };
        let tc = core.target_checkpoints(&mut st, &addr);
        let ref_owner = tc.ref_namespace.clone().unwrap_or_else(|| addr.clone());
        (addr, cwd, core.session_slug(), ref_owner)
    };

    let Some(repo) = probe_repo(Path::new(&cwd)) else {
        return Ok(None);
    };
    let pre_ref = pre_turn_checkpoint_ref(&session_slug, &ref_owner, turn);
    let _ckpt_guard = core.repo_ckpt_lock(&repo.toplevel).lock_owned().await;
    let repo_clone = repo.clone();
    let ref_clone = pre_ref.clone();
    tokio::task::spawn_blocking(move || capture_checkpoint(&repo_clone, &ref_clone))
        .await
        .map_err(|e| format!("pre-turn checkpoint task panicked: {e}"))??;

    let mut st = core.lock();
    let still_exists = match target {
        TargetKey::Agent(id) => st.reg.agents.contains_key(&id),
        TargetKey::Terminal(id) => st.pty_reg.terminals.contains_key(&id),
    };
    if !still_exists {
        return Ok(None);
    }
    let mut tc = core.target_checkpoints(&mut st, &addr);
    tc.repo_root = Some(repo.toplevel.to_string_lossy().to_string());
    tc.pre_turn_refs.insert(turn, pre_ref.clone());
    core.save_target_checkpoints(&mut st, tc);
    Ok(Some(pre_ref))
}

/// Records a no-op turn checkpoint for an ACP turn that executed no file-mutating tools.
/// Reuses the previous checkpoint commit (when present) with an empty file list so concurrent
/// edits from other agents or editors in a shared repo are never falsely attributed to this turn.
pub async fn capture_noop_target_checkpoint(
    core: &std::sync::Arc<crate::runtime::Core>,
    target: crate::runtime::TargetKey,
    turn: u64,
    prompt: Option<String>,
    skip_if_present: bool,
    record_transcript_entry: bool,
) -> Result<Option<CheckpointMeta>, String> {
    use crate::runtime::TargetKey;

    let (addr, cwd, prev_meta, stale_pre_ref, effective_prompt, session_slug, ref_owner) = {
        let mut st = core.lock();
        let (addr, cwd) = match target {
            TargetKey::Agent(id) => {
                let Some(a) = st.reg.agents.get(&id) else {
                    return Ok(None);
                };
                (a.address.clone(), a.cwd.clone())
            }
            TargetKey::Terminal(id) => {
                let Some(t) = st.pty_reg.terminals.get(&id) else {
                    return Ok(None);
                };
                let cwd = t
                    .foreground
                    .as_ref()
                    .map(|f| f.cwd.as_str())
                    .filter(|s| !s.is_empty())
                    .or(t.last_cwd.as_deref())
                    .unwrap_or(&t.cwd)
                    .to_string();
                (t.address.clone(), cwd)
            }
        };
        let tc = core.target_checkpoints(&mut st, &addr);
        if skip_if_present && tc.checkpoints.iter().any(|c| c.turn == turn) {
            return Ok(None);
        }
        let ref_owner = tc.ref_namespace.clone().unwrap_or_else(|| addr.clone());
        let prev_meta = tc.checkpoints.iter().filter(|c| c.turn < turn).max_by_key(|c| c.turn).cloned();
        let stale_pre_ref = tc.pre_turn_refs.get(&turn).cloned();
        let effective_prompt = prompt.or_else(|| (turn > 0).then(|| tc.active_turn_prompt.clone()).flatten());
        (addr, cwd, prev_meta, stale_pre_ref, effective_prompt, core.session_slug(), ref_owner)
    };

    let Some(repo) = probe_repo(Path::new(&cwd)) else {
        return Ok(None);
    };

    let ref_name = checkpoint_ref(&session_slug, &ref_owner, turn);
    let _ckpt_guard = core.repo_ckpt_lock(&repo.toplevel).lock_owned().await;

    let repo_clone = repo.clone();
    let ref_clone = ref_name.clone();
    let (commit_oid, head_oid, ts_ms) = tokio::task::spawn_blocking(move || -> Result<_, String> {
        if let Some(pre) = stale_pre_ref.as_deref() {
            let _ = delete_checkpoint_ref_names(&repo_clone, &[pre.to_string()]);
        }
        if let Some(prev) = prev_meta
            && let Ok(oid) = resolve_commit(&repo_clone, &prev.ref_name)
        {
            run_git_bytes(&repo_clone.toplevel, &["update-ref", &ref_clone, &oid], &[], None)?;
            return Ok((oid, prev.head_oid, now_ms()));
        }
        capture_checkpoint(&repo_clone, &ref_clone)
    })
    .await
    .map_err(|e| format!("noop checkpoint task panicked: {e}"))??;

    let mut st = core.lock();
    let still_exists = match target {
        TargetKey::Agent(id) => st.reg.agents.contains_key(&id),
        TargetKey::Terminal(id) => st.pty_reg.terminals.contains_key(&id),
    };
    if !still_exists {
        return Ok(None);
    }

    let mut tc = core.target_checkpoints(&mut st, &addr);
    if skip_if_present && tc.checkpoints.iter().any(|c| c.turn == turn) {
        return Ok(None);
    }
    tc.pre_turn_refs.remove(&turn);

    let prompt_final = if turn > 0 {
        let p = effective_prompt.or_else(|| tc.active_turn_prompt.clone());
        tc.active_turn_prompt = None;
        if tc.active_turn_num == Some(turn) {
            tc.active_turn_num = None;
        }
        p
    } else {
        None
    };

    let meta = CheckpointMeta {
        turn,
        session_turn: None,
        ref_name: ref_name.clone(),
        commit_oid: commit_oid.clone(),
        head_oid: head_oid.clone(),
        prompt: prompt_final,
        label: None,
        label_source: None,
        files: Vec::new(),
        ts_ms,
    };

    tc.repo_root = Some(repo.toplevel.to_string_lossy().to_string());
    tc.checkpoints.retain(|c| c.turn != turn);
    tc.checkpoints.push(meta.clone());
    tc.checkpoints.sort_by_key(|c| c.turn);
    core.save_target_checkpoints(&mut st, tc);

    match target {
        TargetKey::Agent(id) => {
            if record_transcript_entry && turn > 0 {
                core.record_transcript(
                    &mut st,
                    id,
                    crate::transcript::Entry::Checkpoint {
                        turn,
                        ref_name,
                        commit_oid,
                        head_oid,
                        files: Vec::new(),
                        ts_ms,
                    },
                );
            }
            core.emit(&mut st, "checkpoint", id, serde_json::json!({ "meta": &meta }));
        }
        TargetKey::Terminal(id) => {
            core.emit_pty(&mut st, "checkpoint", id, serde_json::json!({ "meta": &meta }));
        }
    }

    Ok(Some(meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        git_env(dir, args, &[])
    }

    fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn init_repo(name: &str) -> PathBuf {
        let root = crate::test_tmp().join(format!("h2-ckpt-{name}-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        std::fs::write(root.join("README.md"), "base\n").unwrap();
        git(&root, &["add", "."]);
        git_env(
            &root,
            &["commit", "-m", "init"],
            &[("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z"), ("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")],
        );
        root
    }

    fn sha256_or_bytes(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap()
    }

    #[test]
    fn capture_leaves_real_index_branches_and_status_untouched() {
        let root = init_repo("zero-pollution");
        let repo = probe_repo(&root).unwrap();

        // Modify tracked file + stage one file + leave untracked file.
        std::fs::write(root.join("README.md"), "staged\n").unwrap();
        git(&root, &["add", "README.md"]);
        std::fs::write(root.join("README.md"), "staged\nunstaged\n").unwrap();
        std::fs::write(root.join("notes.txt"), "untracked note\n").unwrap();

        let status_before = git(&root, &["status", "--porcelain"]);
        let branches_before = git(&root, &["branch", "-a"]);
        let index_before = sha256_or_bytes(&repo.index_path);
        let mtime_before = std::fs::metadata(&repo.index_path).unwrap().modified().unwrap();

        let r0 = checkpoint_ref("default", "a1", 0);
        let (c0, head0, _) = capture_checkpoint(&repo, &r0).unwrap();
        assert!(!c0.is_empty());
        assert_eq!(head0, repo.head_oid);

        assert_eq!(sha256_or_bytes(&repo.index_path), index_before);
        assert_eq!(std::fs::metadata(&repo.index_path).unwrap().modified().unwrap(), mtime_before);
        assert_eq!(git(&root, &["status", "--porcelain"]), status_before);
        assert_eq!(git(&root, &["branch", "-a"]), branches_before);

        // Nested empty .git dir should be excluded gracefully rather than failing capture.
        let nested = root.join("nested_empty/.git");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("notes.txt"), "updated note\n").unwrap();
        let r1 = checkpoint_ref("default", "a1", 1);
        capture_checkpoint(&repo, &r1).unwrap();

        let diff = diff_checkpoints(&repo, 0, 1, &r0, &r1).unwrap();
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].path, "notes.txt");
        assert!(diff.patch.contains("+updated note"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remerge_filtering_excludes_upstream_pull_noise() {
        let root = init_repo("remerge");
        std::fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();
        git(&root, &["add", "a.rs"]);
        git_env(
            &root,
            &["commit", "-m", "add a.rs"],
            &[("GIT_AUTHOR_DATE", "2026-01-01T00:01:00Z"), ("GIT_COMMITTER_DATE", "2026-01-01T00:01:00Z")],
        );

        // Create upstream branch with 25 commits/files well before turn starts.
        git(&root, &["checkout", "-b", "upstream"]);
        for i in 0..25 {
            std::fs::write(root.join(format!("up_{i}.txt")), format!("upstream {i}\n")).unwrap();
        }
        git(&root, &["add", "."]);
        git_env(
            &root,
            &["commit", "-m", "upstream noise"],
            &[("GIT_AUTHOR_DATE", "2026-01-01T00:02:00Z"), ("GIT_COMMITTER_DATE", "2026-01-01T00:02:00Z")],
        );

        git(&root, &["checkout", "main"]);
        let repo = probe_repo(&root).unwrap();
        let r0 = checkpoint_ref("default", "t1", 0);
        capture_checkpoint(&repo, &r0).unwrap();

        // Turn edits a.rs AND merges upstream (pulling 25 upstream files).
        std::fs::write(root.join("a.rs"), "fn a() { println!(\"authored\"); }\n").unwrap();
        git(&root, &["merge", "--no-ff", "-m", "merge upstream", "upstream"]);
        let r1 = checkpoint_ref("default", "t1", 1);
        capture_checkpoint(&repo, &r1).unwrap();

        let diff = diff_checkpoints(&repo, 0, 1, &r0, &r1).unwrap();
        let paths: Vec<&str> = diff.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.rs"]);
        assert!(!diff.patch.contains("up_0.txt"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_runs_multipass_clean_for_nested_gitignore_and_prunes_refs() {
        let root = init_repo("restore");
        let work_sub = root.join("sub");
        std::fs::create_dir_all(&work_sub).unwrap();
        std::fs::write(root.join("keep.txt"), "v1\n").unwrap();
        let repo = probe_repo(&root).unwrap();

        let r0 = checkpoint_ref("default", "a2", 0);
        capture_checkpoint(&repo, &r0).unwrap();

        // Turn 1: edits keep.txt, creates ignored output inside turn-created `gen/.gitignore`.
        std::fs::write(root.join("keep.txt"), "v2\n").unwrap();
        let gen_dir = root.join("gen");
        std::fs::create_dir_all(&gen_dir).unwrap();
        std::fs::write(gen_dir.join(".gitignore"), "out.txt\n").unwrap();
        std::fs::write(gen_dir.join("out.txt"), "generated garbage\n").unwrap();
        // Also remove `sub` directory to confirm restore recreates ensure_dir.
        std::fs::remove_dir_all(&work_sub).unwrap();

        let r1 = checkpoint_ref("default", "a2", 1);
        capture_checkpoint(&repo, &r1).unwrap();
        let r2 = checkpoint_ref("default", "a2", 2);
        capture_checkpoint(&repo, &r2).unwrap();

        restore_checkpoint(&repo, &r0, Some(&work_sub)).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("keep.txt")).unwrap(), "v1\n");
        assert!(!gen_dir.exists(), "multi-pass clean must wipe gen/.gitignore and obscured gen/out.txt");
        assert!(work_sub.is_dir());

        delete_checkpoint_refs_after(&repo, "default", "a2", 0).unwrap();
        assert!(resolve_commit(&repo, &r0).is_ok());
        assert!(resolve_commit(&repo, &r1).is_err());
        assert!(resolve_commit(&repo, &r2).is_err());

        delete_all_checkpoint_refs(&repo, "default", "a2").unwrap();
        assert!(resolve_commit(&repo, &r0).is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn random_uuid_v4_has_valid_version_and_variant_bits() {
        let u = random_uuid_v4();
        assert_eq!(u.len(), 36);
        let bytes = u.as_bytes();
        assert_eq!(bytes[14], b'4');
        assert!(matches!(bytes[19], b'8' | b'9' | b'a' | b'b'));
    }
}
