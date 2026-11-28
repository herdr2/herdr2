//! Structured transcript extraction for terminal-hosted coding agents (Claude Code,
//! Codex CLI, Cursor CLI, Qwen Code, Pi / oh-my-pi, and Antigravity CLI)
//! plus optimistic merging of prompts sent via `h2 send`.

use crate::acp::proto::AcpContextUsage;
use crate::api::ReadSessionMeta;
use crate::drove::crypto::sha256_hex;
use crate::terminals::Terminal;
use crate::transcript::{Entry, cap_tool_detail};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 2000;
const MAX_BYTES: usize = 2 << 20;
const MTIME_SLACK_MS: u64 = 3_000;
const START_SLACK_MS: u64 = 2_000;

#[derive(Clone, Debug, Default)]
pub struct PtySnapshot {
    pub cwd: String,
    pub started_ms: u64,
    pub agent_hint: Option<String>,
    pub sent_prompts: Vec<(u64, u64, String)>,
    pub turn: u64,
    pub resumed: bool,
    pub fg_pid: Option<u32>,
    /// Session ID pinned to this terminal (from checkpoint/hook capture), when known.
    pub pinned_session_id: Option<String>,
    /// Session IDs owned by other agents/terminals (e.g. ACP side-chat/handoff agents in the same cwd)
    /// that must never be attributed to this PTY terminal.
    pub excluded_session_ids: HashSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    Claude,
    Codex,
    Cursor,
    Qwen,
    Pi,
    Agy,
}

impl Provider {
    fn as_str(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Cursor => "cursor",
            Provider::Qwen => "qwen",
            Provider::Pi => "pi",
            Provider::Agy => "agy",
        }
    }
}

pub(crate) fn is_valid_agy_cid(raw: &str) -> bool {
    let s = raw.trim();
    !s.is_empty()
        && !matches!(s, "transcript" | "transcript_full" | "logs" | ".system_generated")
        && !s.contains('/')
        && !s.contains('\\')
        && !s.contains("..")
}

pub(crate) fn agy_cid_from_path(path: &Path) -> Option<String> {
    let logs = path.parent()?;
    if logs.file_name().and_then(|s| s.to_str()) != Some("logs") {
        return None;
    }
    let sys = logs.parent()?;
    if sys.file_name().and_then(|s| s.to_str()) != Some(".system_generated") {
        return None;
    }
    sys.parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map(str::trim)
        .filter(|s| is_valid_agy_cid(s))
        .map(str::to_string)
}

fn codex_sid_from_header(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    for line in BufReader::new(file).lines().take(12).flatten() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) == Some("session_meta")
            && let Some(sid) = v.pointer("/payload/id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
        {
            return Some(sid.to_string());
        }
    }
    None
}

struct Candidate {
    path: PathBuf,
    provider: Provider,
    mtime_ms: u64,
    first_ts_ms: u64,
}

fn candidate_session_id(c: &Candidate) -> Option<String> {
    match c.provider {
        Provider::Claude | Provider::Cursor | Provider::Qwen => c
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.starts_with("agent-"))
            .map(str::to_string),
        Provider::Codex => codex_sid_from_header(&c.path),
        Provider::Agy => agy_cid_from_path(&c.path),
        Provider::Pi => None,
    }
}

pub(crate) fn argv_has_resume(argv: &[String]) -> bool {
    let has_flag = argv.iter().any(|arg| {
        matches!(arg.as_str(), "--resume" | "-r" | "--continue" | "-c" | "--conversation")
            || arg.starts_with("--resume=")
            || arg.starts_with("--conversation=")
    });
    if has_flag {
        return true;
    }
    let is_codex = argv.iter().any(|a| {
        let base = a.rsplit(['/', '\\']).next().unwrap_or(a).to_ascii_lowercase();
        base == "codex" || base == "codex.exe"
    });
    is_codex && argv.iter().any(|a| a == "resume")
}

pub fn snapshot_from_terminal(t: &Terminal, sent_prompts: Option<&Vec<(u64, u64, String)>>) -> PtySnapshot {
    let raw_cwd = t
        .foreground
        .as_ref()
        .map(|f| f.cwd.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| t.last_cwd.as_deref().filter(|s| !s.is_empty()))
        .unwrap_or(t.cwd.as_str());
    let cwd = normalize_cwd(raw_cwd);
    let started_ms = t.program_started_ms.unwrap_or(t.created_ms);
    let agent_hint = detect_agent_hint(t);
    let turn = t.turn;
    let fg_pid = t.foreground.as_ref().and_then(|f| u32::try_from(f.pid).ok());
    let resumed = crate::pty_adapters::terminal_active_resume_argv(t).is_some();
    PtySnapshot {
        cwd,
        started_ms,
        agent_hint,
        sent_prompts: sent_prompts.cloned().unwrap_or_default(),
        turn,
        resumed,
        fg_pid,
        pinned_session_id: None,
        excluded_session_ids: HashSet::new(),
    }
}

fn explicit_cli_session_id_from_argv(argv: &[String], agent_hint: Option<&str>) -> Option<String> {
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        for flag in ["--conversation", "--session-id", "--resume", "-r"] {
            if arg == flag {
                if let Some(val) = argv.get(i + 1).map(String::as_str).map(str::trim)
                    && !val.is_empty()
                    && !val.starts_with('-')
                    && (agent_hint != Some("agy") || is_valid_agy_cid(val))
                {
                    return Some(val.to_string());
                }
            } else if let Some(val) = arg.strip_prefix(&format!("{flag}=")).map(str::trim)
                && !val.is_empty()
                && !val.starts_with('-')
                && (agent_hint != Some("agy") || is_valid_agy_cid(val))
            {
                return Some(val.to_string());
            }
        }
        i += 1;
    }
    if agent_hint == Some("codex")
        && let Some(pos) = argv.iter().position(|a| a == "resume")
        && let Some(val) = argv.get(pos + 1).map(String::as_str).map(str::trim)
        && !val.is_empty()
        && !val.starts_with('-')
    {
        return Some(val.to_string());
    }
    None
}

fn terminal_known_session_id(t: &Terminal, hint: Option<&str>) -> Option<String> {
    if let Some(fg) = &t.foreground
        && let Some(sid) = explicit_cli_session_id_from_argv(&fg.argv, hint)
    {
        return Some(sid);
    }
    if let Some(saved) = &t.saved_cli
        && let Some(sid) = saved.session_id.as_deref().map(str::trim).filter(|s| !s.is_empty())
        && (hint != Some("agy") || is_valid_agy_cid(sid))
    {
        return Some(sid.to_string());
    }
    if hint == Some("agy")
        && let Some(pid) = t.foreground.as_ref().and_then(|f| u32::try_from(f.pid).ok())
        && let ProcAgyProbe::Locked(cid) = probe_agy_proc(pid)
    {
        return Some(cid);
    }
    None
}

/// Populates `pinned_session_id` and `excluded_session_ids` so ACP agents (e.g. side-chat /
/// handoff / fork agents running in the same cwd) and other terminals never hijack this PTY
/// terminal's discovered session JSONL.
pub fn populate_session_affinity(
    snap: &mut PtySnapshot,
    target_addr: &str,
    core: &crate::runtime::Core,
    st: &mut crate::runtime::State,
) {
    let mut acp_sids: HashSet<String> = HashSet::new();
    for a in st.reg.agents.values() {
        if let Some(sid) = a.acp_session_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            acp_sids.insert(sid.to_string());
            snap.excluded_session_ids.insert(sid.to_string());
        }
    }
    let is_agy = snap.agent_hint.as_deref() == Some("agy");
    let own_tc = core.target_checkpoints(st, target_addr);
    if let Some(sid) = own_tc
        .cli_session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && (!is_agy || is_valid_agy_cid(s)))
        && !acp_sids.contains(sid)
    {
        snap.pinned_session_id = Some(sid.to_string());
    }
    if snap.pinned_session_id.is_none()
        && let Some(target) = st.pty_reg.terminals.values().find(|t| t.address == target_addr)
        && let Some(sid) = terminal_known_session_id(target, snap.agent_hint.as_deref())
        && !acp_sids.contains(&sid)
    {
        snap.pinned_session_id = Some(sid);
    }
    for (addr, tc) in &st.checkpoints {
        if addr == target_addr {
            continue;
        }
        if let Some(sid) = tc
            .cli_session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && (!is_agy || is_valid_agy_cid(s)))
            && snap.pinned_session_id.as_deref() != Some(sid)
        {
            snap.excluded_session_ids.insert(sid.to_string());
        }
    }
    for other in st.pty_reg.terminals.values() {
        if other.address == target_addr || !other.is_live() {
            continue;
        }
        let other_hint = detect_agent_hint(other);
        if let Some(sid) = terminal_known_session_id(other, other_hint.as_deref())
            && snap.pinned_session_id.as_deref() != Some(sid.as_str())
        {
            snap.excluded_session_ids.insert(sid);
        }
    }
}

pub fn extract_entries(snap: &PtySnapshot) -> Vec<Entry> {
    extract_transcript(snap).0
}

pub fn extract_transcript(snap: &PtySnapshot) -> (Vec<Entry>, Option<ReadSessionMeta>) {
    let (mut entries, meta) = home_dir()
        .map(|home| extract_transcript_with_home(snap, &home, None, None))
        .unwrap_or_else(|| (Vec::new(), None));
    merge_sent_prompts(&mut entries, &snap.sent_prompts);
    cap_entries(&mut entries);
    (entries, meta)
}

#[cfg(test)]
fn extract_entries_with_home(
    snap: &PtySnapshot,
    home: &Path,
    claude_dir_override: Option<&Path>,
    codex_dir_override: Option<&Path>,
) -> Vec<Entry> {
    extract_transcript_with_home(snap, home, claude_dir_override, codex_dir_override).0
}

fn extract_transcript_with_home(
    snap: &PtySnapshot,
    home: &Path,
    claude_dir_override: Option<&Path>,
    codex_dir_override: Option<&Path>,
) -> (Vec<Entry>, Option<ReadSessionMeta>) {
    let norm_cwd = normalize_cwd(&snap.cwd);
    if norm_cwd.is_empty() {
        return (Vec::new(), None);
    }
    let allow_older_fallback = snap.resumed || snap.turn > 0;
    let fg_started_ms = snap.fg_pid.and_then(proc_started_ms);
    let effective_start = if snap.resumed {
        0
    } else {
        fg_started_ms.map_or(snap.started_ms, |f| snap.started_ms.max(f))
    };
    let min_mtime = if snap.resumed { 0 } else { effective_start.saturating_sub(MTIME_SLACK_MS) };
    let providers = ordered_providers(snap.agent_hint.as_deref());

    for provider in providers {
        let mut candidates = match provider {
            Provider::Claude => discover_claude(
                home,
                claude_dir_override,
                &norm_cwd,
                if snap.pinned_session_id.is_some() { 0 } else { min_mtime },
            ),
            Provider::Codex => discover_codex(
                home,
                codex_dir_override,
                &norm_cwd,
                if snap.pinned_session_id.is_some() { 0 } else { min_mtime },
            ),
            Provider::Cursor => discover_cursor(
                home,
                &norm_cwd,
                if snap.pinned_session_id.is_some() { 0 } else { min_mtime },
            ),
            Provider::Qwen => discover_qwen(home, &norm_cwd, min_mtime),
            Provider::Pi => discover_pi(home, &norm_cwd, min_mtime),
            Provider::Agy => {
                if let Some(cid) = snap.pinned_session_id.as_deref().filter(|s| is_valid_agy_cid(s)) {
                    let logs = home.join(".gemini/antigravity-cli/brain").join(cid).join(".system_generated/logs");
                    match pick_agy_log_file(&logs) {
                        Some(path) => vec![Candidate {
                            mtime_ms: mtime_ms(&path),
                            first_ts_ms: agy_first_ts(&path),
                            path,
                            provider: Provider::Agy,
                        }],
                        None => Vec::new(),
                    }
                } else {
                    discover_agy(home, min_mtime, snap.fg_pid)
                }
            }
        };
        if !snap.excluded_session_ids.is_empty() {
            candidates.retain(|c| {
                candidate_session_id(c).as_deref().is_none_or(|sid| !snap.excluded_session_ids.contains(sid))
            });
        }
        let pinned_hit = snap.pinned_session_id.as_deref().and_then(|wanted| {
            candidates
                .iter()
                .position(|c| candidate_session_id(c).as_deref() == Some(wanted))
                .map(|idx| candidates.remove(idx))
        });
        let picked = if snap.pinned_session_id.is_some() {
            pinned_hit
        } else {
            pick_best_candidate(candidates, effective_start, fg_started_ms, allow_older_fallback)
        };
        if let Some(best) = picked {
            let (entries, mut meta) = match best.provider {
                Provider::Claude => parse_claude_jsonl(&best.path),
                Provider::Codex => parse_codex_jsonl(&best.path),
                Provider::Cursor => parse_cursor_jsonl(&best.path),
                Provider::Qwen => {
                    if best.path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                        parse_claude_jsonl(&best.path)
                    } else {
                        parse_gemini_json(&best.path)
                    }
                }
                Provider::Pi => parse_pi_jsonl(&best.path),
                Provider::Agy => parse_agy_jsonl(&best.path),
            };
            if !entries.is_empty() || !meta.is_empty() {
                meta.provider = Some(best.provider.as_str().to_string());
                return (entries, Some(meta));
            }
        }
    }
    (Vec::new(), None)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).filter(|s| !s.is_empty()).map(PathBuf::from)
}

fn ordered_providers(hint: Option<&str>) -> Vec<Provider> {
    match hint {
        Some("claude") => vec![Provider::Claude],
        Some("codex") => vec![Provider::Codex],
        Some("cursor") => vec![Provider::Cursor],
        Some("qwen") => vec![Provider::Qwen],
        Some("pi") => vec![Provider::Pi],
        Some("agy") => vec![Provider::Agy],
        _ => Vec::new(),
    }
}

fn hint_from_label(raw: &str) -> Option<&'static str> {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    if lower == "claude" || lower == "claude-code" || lower == "claude code" {
        return Some("claude");
    }
    if lower == "codex" {
        return Some("codex");
    }
    if lower == "cursor" || lower == "cursor-agent" || lower == "cursor agent" {
        return Some("cursor");
    }
    if lower == "qwen" || lower == "qwen-code" {
        return Some("qwen");
    }
    if lower == "pi" || lower == "omp" || lower == "oh-my-pi" {
        return Some("pi");
    }
    if lower == "agy" || lower == "antigravity" || lower == "antigravity-cli" {
        return Some("agy");
    }
    // ZCode keeps its history in a database, not a transcript: the hint names the program only.
    if lower == "zcode" || lower == "zcode-cli" {
        return Some("zcode");
    }
    None
}

fn detect_agent_hint(t: &Terminal) -> Option<String> {
    if let Some(fg) = &t.foreground {
        if let Some(h) = hint_from_words(std::iter::once(fg.name.as_str()).chain(fg.argv.iter().map(String::as_str))) {
            return Some(h.to_string());
        }
    }
    if let Some(h) = hint_from_words(t.argv.iter().map(String::as_str)) {
        return Some(h.to_string());
    }
    if let Some(h) = t.status.as_ref().and_then(|s| hint_from_label(&s.app)) {
        return Some(h.to_string());
    }
    let title = t.title.trim();
    if !title.contains('/')
        && !title.contains('\\')
        && let Some(h) = hint_from_label(title)
    {
        return Some(h.to_string());
    }
    None
}

fn strip_known_token_ext(s: &str) -> &str {
    for ext in [".exe", ".cmd", ".bat", ".ps1", ".js", ".mjs", ".cjs", ".ts", ".py", ".rb"] {
        if let Some(prefix) = s.strip_suffix(ext) {
            return prefix;
        }
    }
    s
}

fn hint_from_single_word(raw: &str) -> Option<&'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return None;
    }
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed).to_ascii_lowercase();
    let stem = strip_known_token_ext(&base);
    let clean = stem.strip_suffix(".real").unwrap_or(stem);
    if let Some(h) = hint_from_label(clean) {
        return Some(h);
    }
    // Check bundled versioned runtime paths (e.g. `...\cursor-agent\versions\<ver>\node.exe`,
    // `...\cursor-agent\versions\<ver>\index.js`, `/home/u/.local/share/claude/versions/2.1.325`)
    // and `node_modules` package layouts.
    if trimmed.contains('/') || trimmed.contains('\\') {
        let segs: Vec<&str> = trimmed.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        for i in (1..segs.len().saturating_sub(1)).rev() {
            if segs[i].eq_ignore_ascii_case("versions") && segs.len().saturating_sub(i) <= 4 {
                let pkg = segs[i - 1].to_ascii_lowercase();
                if let Some(h) = hint_from_label(&pkg) {
                    return Some(h);
                }
            }
        }
        if let Some(nm_idx) = segs.iter().rposition(|s| s.eq_ignore_ascii_case("node_modules")) {
            for seg in segs[nm_idx + 1..segs.len().saturating_sub(1)].iter().rev() {
                if seg.starts_with('@') {
                    continue;
                }
                let pkg = seg.to_ascii_lowercase();
                if let Some(h) = hint_from_label(&pkg) {
                    return Some(h);
                }
            }
        }
    }
    None
}

fn first_powershell_command_token(raw: &str) -> Option<&str> {
    let rest = raw.trim_start().strip_prefix('&')?.trim_start();
    if let Some(after) = rest.strip_prefix('\'') {
        let end = after.find('\'')?;
        Some(&after[..end])
    } else if let Some(after) = rest.strip_prefix('"') {
        let end = after.find('"')?;
        Some(&after[..end])
    } else {
        rest.split_whitespace().next()
    }
}

pub(crate) fn hint_from_words<'a>(words: impl Iterator<Item = &'a str>) -> Option<&'static str> {
    for raw in words {
        if raw.starts_with('-') {
            continue;
        }
        if let Some(h) = hint_from_single_word(raw) {
            return Some(h);
        }
        if let Some(ps_cmd) = first_powershell_command_token(raw)
            && let Some(h) = hint_from_single_word(ps_cmd)
        {
            return Some(h);
        }
    }
    None
}

pub(crate) fn normalize_cwd(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed == "/" {
        return "/".to_string();
    }
    if trimmed.len() == 3 && trimmed.as_bytes()[1] == b':' && (trimmed.ends_with('\\') || trimmed.ends_with('/')) {
        return trimmed.to_string();
    }
    let stripped = trimmed.trim_end_matches(['/', '\\']);
    if stripped.is_empty() { "/".to_string() } else { stripped.to_string() }
}

fn cwd_matches(a: &str, b: &str) -> bool {
    let na = normalize_cwd(a);
    let nb = normalize_cwd(b);
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    #[cfg(windows)]
    {
        na.replace('\\', "/").eq_ignore_ascii_case(&nb.replace('\\', "/"))
    }
    #[cfg(not(windows))]
    {
        na == nb
    }
}

fn mtime_ms(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

fn created_ms(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.created())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(target_os = "linux")]
fn proc_started_ms(pid: u32) -> Option<u64> {
    let ms = mtime_ms(Path::new(&format!("/proc/{pid}")));
    (ms > 0).then_some(ms)
}

#[cfg(not(target_os = "linux"))]
fn proc_started_ms(_pid: u32) -> Option<u64> {
    None
}

fn pick_best_candidate(
    candidates: Vec<Candidate>,
    started_ms: u64,
    fg_started_ms: Option<u64>,
    allow_older_fallback: bool,
) -> Option<Candidate> {
    if candidates.is_empty() {
        return None;
    }
    let effective_start = if allow_older_fallback {
        started_ms
    } else {
        fg_started_ms.map_or(started_ms, |f| started_ms.max(f))
    };
    let start_floor = effective_start.saturating_sub(START_SLACK_MS);
    let fg_floor = fg_started_ms.map(|ms| ms.saturating_sub(START_SLACK_MS));
    let mut after_start: Vec<Candidate> = Vec::new();
    let mut older: Vec<Candidate> = Vec::new();
    for c in candidates {
        let qualifies = if start_floor == 0 {
            true
        } else if c.first_ts_ms > 0 {
            c.first_ts_ms >= start_floor
        } else {
            // Never let a timestamp-less old session bypass start_floor for a brand-new 0-turn CLI.
            allow_older_fallback
        };
        if qualifies {
            after_start.push(c);
        } else {
            older.push(c);
        }
    }
    if !after_start.is_empty() {
        after_start.sort_by(|a, b| {
            let a_fg = fg_floor.is_some_and(|f| a.first_ts_ms > 0 && a.first_ts_ms >= f);
            let b_fg = fg_floor.is_some_and(|f| b.first_ts_ms > 0 && b.first_ts_ms >= f);
            b_fg.cmp(&a_fg).then_with(|| b.mtime_ms.cmp(&a.mtime_ms)).then_with(|| b.first_ts_ms.cmp(&a.first_ts_ms))
        });
        after_start.into_iter().next()
    } else if allow_older_fallback {
        older.sort_by(|a, b| b.mtime_ms.cmp(&a.mtime_ms));
        older.into_iter().next()
    } else {
        None
    }
}

fn claude_slug(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn claude_root(home: &Path, override_dir: Option<&Path>) -> PathBuf {
    if let Some(dir) = override_dir {
        return dir.to_path_buf();
    }
    if let Some(env_dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|s| !s.is_empty()) {
        return PathBuf::from(env_dir);
    }
    home.join(".claude")
}

fn discover_claude(home: &Path, override_dir: Option<&Path>, cwd: &str, min_mtime: u64) -> Vec<Candidate> {
    let projects_dir = claude_root(home, override_dir).join("projects");
    if !projects_dir.is_dir() {
        return Vec::new();
    }
    let slug_dir = projects_dir.join(claude_slug(cwd));
    let mut out = Vec::new();
    if slug_dir.is_dir() {
        collect_claude_dir(&slug_dir, cwd, min_mtime, false, &mut out);
    }
    if !out.is_empty() {
        return out;
    }

    // Fallback: scan up to the 12 most recent project directories and check JSONL `"cwd"`.
    let Ok(entries) = fs::read_dir(&projects_dir) else {
        return out;
    };
    let mut dirs: Vec<(u64, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && *p != slug_dir)
        .map(|p| (mtime_ms(&p), p))
        .filter(|(m, _)| *m == 0 || *m >= min_mtime)
        .collect();
    dirs.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, dir) in dirs.into_iter().take(12) {
        collect_claude_dir(&dir, cwd, min_mtime, true, &mut out);
    }
    out
}

fn collect_claude_dir(dir: &Path, cwd: &str, min_mtime: u64, require_cwd: bool, out: &mut Vec<Candidate>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") || !path.is_file() {
            continue;
        }
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.starts_with("agent-") {
            continue;
        }
        let mt = mtime_ms(&path);
        if mt > 0 && mt < min_mtime {
            continue;
        }
        let (matched_cwd, first_ts) = inspect_claude_header(&path);
        if require_cwd && !matched_cwd.as_deref().is_some_and(|c| cwd_matches(c, cwd)) {
            continue;
        }
        if let Some(c) = matched_cwd.as_deref()
            && !cwd_matches(c, cwd)
        {
            continue;
        }
        out.push(Candidate { path, provider: Provider::Claude, mtime_ms: mt, first_ts_ms: first_ts });
    }
}

fn inspect_claude_header(path: &Path) -> (Option<String>, u64) {
    let Ok(file) = File::open(path) else {
        return (None, 0);
    };
    let reader = BufReader::new(file);
    let mut found_cwd = None;
    let mut first_ts = 0u64;
    for line in reader.lines().take(24).flatten() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if found_cwd.is_none()
            && let Some(c) = v.get("cwd").and_then(Value::as_str)
        {
            found_cwd = Some(c.to_string());
        }
        if first_ts == 0
            && let Some(ts) = v.get("timestamp").and_then(Value::as_str)
        {
            first_ts = parse_ts_ms(ts);
        }
        if found_cwd.is_some() && first_ts > 0 {
            break;
        }
    }
    (found_cwd, first_ts)
}

fn codex_root(home: &Path, override_dir: Option<&Path>) -> PathBuf {
    if let Some(dir) = override_dir {
        return dir.to_path_buf();
    }
    if let Some(env_dir) = std::env::var_os("CODEX_HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(env_dir);
    }
    home.join(".codex")
}

fn discover_codex(home: &Path, override_dir: Option<&Path>, cwd: &str, min_mtime: u64) -> Vec<Candidate> {
    let sessions_dir = codex_root(home, override_dir).join("sessions");
    if !sessions_dir.is_dir() {
        return Vec::new();
    }
    // Collect YYYY/MM/DD directories (sorted descending, taking up to 4 most recent).
    let mut day_dirs = Vec::new();
    for year in sorted_subdirs_desc(&sessions_dir) {
        for month in sorted_subdirs_desc(&year) {
            for day in sorted_subdirs_desc(&month) {
                day_dirs.push(day);
                if day_dirs.len() >= 4 {
                    break;
                }
            }
            if day_dirs.len() >= 4 {
                break;
            }
        }
        if day_dirs.len() >= 4 {
            break;
        }
    }
    if day_dirs.is_empty() {
        day_dirs.push(sessions_dir);
    }

    let mut out = Vec::new();
    for dir in day_dirs {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !name.starts_with("rollout-")
                || path.extension().and_then(|s| s.to_str()) != Some("jsonl")
                || !path.is_file()
            {
                continue;
            }
            let mt = mtime_ms(&path);
            if mt > 0 && mt < min_mtime {
                continue;
            }
            if let Some(first_ts) = inspect_codex_header(&path, cwd) {
                out.push(Candidate { path, provider: Provider::Codex, mtime_ms: mt, first_ts_ms: first_ts });
            }
        }
    }
    out
}

fn sorted_subdirs_desc(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    dirs
}

fn inspect_codex_header(path: &Path, cwd: &str) -> Option<u64> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut matched = false;
    let mut first_ts = 0u64;
    for line in reader.lines().take(12).flatten() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if first_ts == 0
            && let Some(ts) = v.get("timestamp").and_then(Value::as_str)
        {
            first_ts = parse_ts_ms(ts);
        }
        if v.get("type").and_then(Value::as_str) == Some("session_meta") {
            let meta_cwd = v.pointer("/payload/cwd").and_then(Value::as_str).unwrap_or("");
            if cwd_matches(meta_cwd, cwd) {
                matched = true;
            } else {
                return None;
            }
            if first_ts == 0
                && let Some(ts) = v.pointer("/payload/timestamp").and_then(Value::as_str)
            {
                first_ts = parse_ts_ms(ts);
            }
            break;
        }
    }
    matched.then_some(first_ts)
}

fn discover_qwen(home: &Path, cwd: &str, min_mtime: u64) -> Vec<Candidate> {
    let mut out = discover_gemini_or_qwen(home, ".qwen", Provider::Qwen, cwd, min_mtime);
    let slug_chats = home.join(".qwen/projects").join(claude_slug(cwd)).join("chats");
    if slug_chats.is_dir() {
        collect_claude_dir(&slug_chats, cwd, min_mtime, false, &mut out);
        for c in &mut out {
            if c.path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                c.provider = Provider::Qwen;
            }
        }
    }
    out
}

fn discover_gemini_or_qwen(
    home: &Path,
    dot_dir: &str,
    provider: Provider,
    cwd: &str,
    min_mtime: u64,
) -> Vec<Candidate> {
    let tmp_dir = home.join(dot_dir).join("tmp");
    if !tmp_dir.is_dir() {
        return Vec::new();
    }
    let cwd_hash = sha256_hex(cwd.as_bytes());
    let Ok(project_dirs) = fs::read_dir(&tmp_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in project_dirs.flatten() {
        let proj = entry.path();
        if !proj.is_dir() {
            continue;
        }
        let dir_name = proj.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let root_file = proj.join(".project_root");
        let root_matches = fs::read_to_string(&root_file).ok().is_some_and(|s| cwd_matches(&s, cwd));
        let hash_matches = dir_name == cwd_hash;

        let chats = proj.join("chats");
        if !chats.is_dir() {
            continue;
        }
        let Ok(chat_files) = fs::read_dir(&chats) else {
            continue;
        };
        for cf in chat_files.flatten() {
            let path = cf.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !name.starts_with("session-")
                || path.extension().and_then(|s| s.to_str()) != Some("json")
                || !path.is_file()
            {
                continue;
            }
            let mt = mtime_ms(&path);
            if mt > 0 && mt < min_mtime {
                continue;
            }
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let Ok(raw) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let json_hash_matches = v.get("projectHash").and_then(Value::as_str) == Some(cwd_hash.as_str());
            if !(root_matches || hash_matches || json_hash_matches) {
                continue;
            }
            let first_ts = v
                .get("startTime")
                .and_then(Value::as_str)
                .map(parse_ts_ms)
                .filter(|t| *t > 0)
                .or_else(|| {
                    v.get("messages")
                        .and_then(Value::as_array)
                        .and_then(|m| m.first())
                        .and_then(|m| m.get("timestamp"))
                        .and_then(Value::as_str)
                        .map(parse_ts_ms)
                })
                .unwrap_or(0);
            out.push(Candidate { path, provider, mtime_ms: mt, first_ts_ms: first_ts });
        }
    }
    out
}

fn discover_pi(home: &Path, cwd: &str, min_mtime: u64) -> Vec<Candidate> {
    let mut out = Vec::new();
    for rel in [".pi/agent/sessions", ".omp/agent/sessions"] {
        let base = home.join(rel);
        if !base.is_dir() {
            continue;
        }
        let Ok(dirs) = fs::read_dir(&base) else {
            continue;
        };
        for dir in dirs.flatten() {
            let pdir = dir.path();
            if !pdir.is_dir() {
                continue;
            }
            let Ok(files) = fs::read_dir(&pdir) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(|s| s.to_str()) != Some("jsonl") || !path.is_file() {
                    continue;
                }
                let mt = mtime_ms(&path);
                if mt > 0 && mt < min_mtime {
                    continue;
                }
                if let Some(first_ts) = inspect_pi_header(&path, cwd) {
                    out.push(Candidate { path, provider: Provider::Pi, mtime_ms: mt, first_ts_ms: first_ts });
                }
            }
        }
    }
    out
}

fn inspect_pi_header(path: &Path, cwd: &str) -> Option<u64> {
    let file = File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line).ok()?;
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }
    let scwd = v.get("cwd").and_then(Value::as_str)?;
    if !cwd_matches(scwd, cwd) {
        return None;
    }
    let ts = v.get("timestamp").and_then(Value::as_str).map(parse_ts_ms).unwrap_or(0);
    Some(ts)
}

fn pick_agy_log_file(logs_dir: &Path) -> Option<PathBuf> {
    let full = logs_dir.join("transcript_full.jsonl");
    if full.is_file() && fs::metadata(&full).is_ok_and(|m| m.len() > 0) {
        return Some(full);
    }
    let compact = logs_dir.join("transcript.jsonl");
    compact.is_file().then_some(compact)
}

fn agy_first_ts(path: &Path) -> u64 {
    File::open(path)
        .ok()
        .and_then(|f| {
            let mut line = String::new();
            BufReader::new(f).read_line(&mut line).ok()?;
            let v: Value = serde_json::from_str(line.trim()).ok()?;
            v.get("created_at").and_then(Value::as_str).map(parse_ts_ms)
        })
        .unwrap_or(0)
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) enum ProcAgyProbe {
    /// Process (or its direct child) holds an open `presence/<uuid>.lock`.
    Locked(String),
    /// Process is a live `agy` process whose `/proc/<pid>/fd` was inspected and has no lock yet
    /// (i.e. brand-new idle session before Turn 1 — must never fall back to global brain scan).
    IdleNoLock,
    /// Platform or PID does not support authoritative `/proc/<pid>/fd` inspection.
    Unavailable,
}

#[cfg(target_os = "linux")]
fn scan_proc_fd_for_agy_lock(pid: u32) -> Option<Option<String>> {
    let entries = fs::read_dir(PathBuf::from(format!("/proc/{pid}/fd"))).ok()?;
    for entry in entries.flatten() {
        let Ok(target) = fs::read_link(entry.path()) else {
            continue;
        };
        let target_str = target.to_string_lossy();
        if !target_str.contains("/antigravity-cli/presence/") || !target_str.ends_with(".lock") {
            continue;
        }
        if let Some(uuid) = target.file_stem().and_then(|s| s.to_str()).filter(|s| is_valid_agy_cid(s)) {
            return Some(Some(uuid.to_string()));
        }
    }
    Some(None)
}

#[cfg(target_os = "linux")]
fn proc_looks_like_agy(pid: u32) -> bool {
    if let Ok(comm) = fs::read_to_string(format!("/proc/{pid}/comm")) {
        let lower = comm.trim().to_ascii_lowercase();
        let clean = lower.strip_suffix(".real").unwrap_or(&lower);
        if matches!(clean, "agy" | "antigravity" | "antigravity-cli") {
            return true;
        }
    }
    if let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) {
        for part in cmdline.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let s = String::from_utf8_lossy(part);
            if hint_from_words(std::iter::once(s.as_ref())) == Some("agy") {
                return true;
            }
        }
    }
    false
}

#[cfg(target_os = "linux")]
pub(crate) fn probe_agy_proc(fg_pid: u32) -> ProcAgyProbe {
    let Some(direct) = scan_proc_fd_for_agy_lock(fg_pid) else {
        return ProcAgyProbe::Unavailable;
    };
    if let Some(uuid) = direct {
        return ProcAgyProbe::Locked(uuid);
    }
    let mut child_is_agy = false;
    if let Ok(children_raw) = fs::read_to_string(format!("/proc/{fg_pid}/task/{fg_pid}/children")) {
        for tok in children_raw.split_whitespace() {
            let Ok(cpid) = tok.parse::<u32>() else { continue };
            if let Some(Some(uuid)) = scan_proc_fd_for_agy_lock(cpid) {
                return ProcAgyProbe::Locked(uuid);
            }
            if proc_looks_like_agy(cpid) {
                child_is_agy = true;
            }
        }
    }
    if proc_looks_like_agy(fg_pid) || child_is_agy {
        ProcAgyProbe::IdleNoLock
    } else {
        ProcAgyProbe::Unavailable
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn probe_agy_proc(_fg_pid: u32) -> ProcAgyProbe {
    ProcAgyProbe::Unavailable
}

fn discover_agy(home: &Path, min_mtime: u64, fg_pid: Option<u32>) -> Vec<Candidate> {
    if let Some(pid) = fg_pid {
        match probe_agy_proc(pid) {
            ProcAgyProbe::Locked(uuid) => {
                let logs = home.join(".gemini/antigravity-cli/brain").join(&uuid).join(".system_generated/logs");
                return match pick_agy_log_file(&logs) {
                    Some(path) => vec![Candidate {
                        mtime_ms: mtime_ms(&path),
                        first_ts_ms: agy_first_ts(&path),
                        path,
                        provider: Provider::Agy,
                    }],
                    None => Vec::new(),
                };
            }
            ProcAgyProbe::IdleNoLock => {
                // Live agy process has not opened a conversation lock yet; never hijack another session.
                return Vec::new();
            }
            ProcAgyProbe::Unavailable => {}
        }
    }
    let brain = home.join(".gemini/antigravity-cli/brain");
    if !brain.is_dir() {
        return Vec::new();
    }
    let Ok(dirs) = fs::read_dir(&brain) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for dir in dirs.flatten() {
        let logs = dir.path().join(".system_generated/logs");
        let Some(path) = pick_agy_log_file(&logs) else {
            continue;
        };
        let mt = mtime_ms(&path);
        if mt > 0 && mt < min_mtime {
            continue;
        }
        let first_ts = agy_first_ts(&path);
        out.push(Candidate { path, provider: Provider::Agy, mtime_ms: mt, first_ts_ms: first_ts });
    }
    out
}

fn read_bounded_lines(path: &Path) -> Vec<String> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let Ok(meta) = file.metadata() else {
        return Vec::new();
    };
    let len = meta.len();
    let mut buf = Vec::new();
    let truncated = len > MAX_FILE_BYTES;
    if truncated {
        if file.seek(SeekFrom::Start(len - MAX_FILE_BYTES)).is_err() {
            return Vec::new();
        }
    }
    if file.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if truncated && !lines.is_empty() {
        lines.remove(0);
    }
    lines
}

fn merge_tool_output(entries: &mut [Entry], idx: usize, new_status: &str, output_snippet: Option<String>) {
    let Some(Entry::ToolCall { status, detail, .. }) = entries.get_mut(idx) else {
        return;
    };
    *status = new_status.to_string();
    let Some(out) = output_snippet.as_deref().and_then(cap_tool_detail) else {
        return;
    };
    match detail {
        Some(prev) if !prev.contains(&out) => {
            *detail = cap_tool_detail(&format!("{prev}\n\n{out}"));
        }
        None => *detail = Some(out),
        _ => {}
    }
}

fn value_text_snippet(val: Option<&Value>) -> Option<String> {
    let val = val?;
    match val {
        Value::String(s) => cap_tool_detail(s),
        Value::Array(arr) => {
            let parts: Vec<&str> = arr
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .or_else(|| item.get("text").and_then(Value::as_str))
                        .or_else(|| item.get("output").and_then(Value::as_str))
                })
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if parts.is_empty() { None } else { cap_tool_detail(&parts.join("\n")) }
        }
        Value::Object(obj) => obj
            .get("output")
            .or_else(|| obj.get("stdout"))
            .or_else(|| obj.get("text"))
            .or_else(|| obj.get("stderr"))
            .and_then(Value::as_str)
            .and_then(cap_tool_detail),
        _ => None,
    }
}

fn claude_tool_detail(name: &str, input: &Value) -> Option<String> {
    match name {
        "Edit" | "MultiEdit" => {
            let path = input.get("file_path").and_then(Value::as_str).unwrap_or("file");
            let old_t = input.get("old_string").and_then(Value::as_str).unwrap_or("");
            let new_t = input.get("new_string").and_then(Value::as_str).unwrap_or("");
            if old_t.is_empty() && new_t.is_empty() {
                return None;
            }
            let mut diff = format!("--- {path}\n+++ {path}");
            for line in old_t.lines().take(25) {
                diff.push_str("\n- ");
                diff.push_str(line);
            }
            for line in new_t.lines().take(25) {
                diff.push_str("\n+ ");
                diff.push_str(line);
            }
            cap_tool_detail(&diff)
        }
        "Write" => {
            let content = input.get("content").and_then(Value::as_str)?;
            cap_tool_detail(content)
        }
        "Bash" => {
            let cmd = input.get("command").and_then(Value::as_str)?.trim();
            if cmd.contains('\n') || cmd.len() > 120 { cap_tool_detail(cmd) } else { None }
        }
        _ => None,
    }
}

fn parse_claude_jsonl(path: &Path) -> (Vec<Entry>, ReadSessionMeta) {
    let settings = load_claude_settings();
    parse_claude_jsonl_with_settings(path, settings.as_ref())
}

fn parse_claude_jsonl_with_settings(path: &Path, settings: Option<&Value>) -> (Vec<Entry>, ReadSessionMeta) {
    let lines = read_bounded_lines(path);
    let mut entries: Vec<Entry> = Vec::new();
    let mut meta = ReadSessionMeta::default();
    let mut tool_indices: HashMap<String, usize> = HashMap::new();
    let mut turn: u64 = 0;
    let mut active_context_window: Option<u64> = None;
    let mut model_windows: HashMap<String, u64> = HashMap::new();
    let mut last_used: Option<u64> = None;

    let apply_model_switch = |raw_spec: &str,
                              meta: &mut ReadSessionMeta,
                              active_context_window: &mut Option<u64>,
                              model_windows: &mut HashMap<String, u64>| {
        let (resolved_model, resolved_window) = resolve_claude_model_spec(raw_spec, settings);
        let prev_base = meta.model.as_deref().map(|m| strip_model_context_suffix(m).to_ascii_lowercase());
        let next_base = strip_model_context_suffix(&resolved_model).to_ascii_lowercase();
        if prev_base.as_deref() != Some(next_base.as_str()) {
            *active_context_window = None;
        }
        if let Some(sz) = resolved_window {
            if !next_base.is_empty() {
                model_windows.insert(next_base, sz);
            }
            *active_context_window = Some(sz);
        }
        meta.model = Some(resolved_model);
    };

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if meta.session_id.is_none()
            && let Some(sid) = v.get("sessionId").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
        {
            meta.session_id = Some(sid.to_string());
        }
        if let Some(pm) = v.get("permissionMode").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
            meta.mode =
                Some(if pm == "bypassPermissions" { "dangerously-skip-permissions".into() } else { pm.to_string() });
        }
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || v.get("isMeta").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let ts_ms = v.get("timestamp").and_then(Value::as_str).map(parse_ts_ms).unwrap_or(0);
        match v.get("type").and_then(Value::as_str) {
            Some("user") => {
                let Some(content) = v.pointer("/message/content") else {
                    continue;
                };
                match content {
                    Value::String(s) => {
                        if let Some(raw_spec) = parse_claude_model_switch_stdout(s) {
                            apply_model_switch(&raw_spec, &mut meta, &mut active_context_window, &mut model_windows);
                        }
                        if is_claude_command_noise(s) {
                            continue;
                        }
                        let clean = strip_system_reminders(s);
                        if !clean.is_empty() {
                            turn += 1;
                            entries.push(Entry::User { turn, from: None, text: clean, attachments: vec![], ts_ms });
                        }
                    }
                    Value::Array(blocks) => {
                        let mut text_parts = Vec::new();
                        for block in blocks {
                            match block.get("type").and_then(Value::as_str) {
                                Some("tool_result") => {
                                    if let Some(id) = block.get("tool_use_id").and_then(Value::as_str)
                                        && let Some(&idx) = tool_indices.get(id)
                                    {
                                        let is_err = block.get("is_error").and_then(Value::as_bool) == Some(true);
                                        let new_status = if is_err { "failed" } else { "completed" };
                                        let out = value_text_snippet(block.get("content"));
                                        merge_tool_output(&mut entries, idx, new_status, out);
                                    }
                                }
                                Some("text") => {
                                    if let Some(raw) = block.get("text").and_then(Value::as_str) {
                                        if let Some(raw_spec) = parse_claude_model_switch_stdout(raw) {
                                            apply_model_switch(
                                                &raw_spec,
                                                &mut meta,
                                                &mut active_context_window,
                                                &mut model_windows,
                                            );
                                        }
                                        if is_claude_command_noise(raw)
                                            || raw.trim_start().starts_with("[Request interrupted by user")
                                        {
                                            continue;
                                        }
                                        let clean = strip_system_reminders(raw);
                                        if !clean.is_empty() {
                                            text_parts.push(clean);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        if !text_parts.is_empty() {
                            turn += 1;
                            entries.push(Entry::User {
                                turn,
                                from: None,
                                text: text_parts.join("\n\n"),
                                attachments: vec![],
                                ts_ms,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Some("api-request") => {
                let req_model =
                    v.pointer("/params/model").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
                let beta_1m = v.pointer("/params/betas").and_then(Value::as_array).is_some_and(|betas| {
                    betas.iter().filter_map(Value::as_str).any(|b| b.to_ascii_lowercase().starts_with("context-1m-"))
                });
                let req_window = req_model.and_then(parse_context_window_from_model_name).or(if beta_1m {
                    Some(1_000_000)
                } else {
                    None
                });
                if let Some(m) = req_model {
                    let prev_base =
                        meta.model.as_deref().map(|cur| strip_model_context_suffix(cur).to_ascii_lowercase());
                    let next_base = strip_model_context_suffix(m).to_ascii_lowercase();
                    if prev_base.as_deref() != Some(next_base.as_str()) {
                        active_context_window = None;
                        meta.model = Some(m.to_string());
                    }
                    if let Some(sz) = req_window {
                        if !next_base.is_empty() {
                            model_windows.insert(next_base, sz);
                        }
                        active_context_window = Some(sz);
                    }
                } else if let Some(sz) = req_window {
                    active_context_window = Some(sz);
                }
            }
            Some("cost-state") => {
                if let Some(map) = v.get("modelUsage").and_then(Value::as_object) {
                    let cur_base = meta.model.as_deref().map(|m| strip_model_context_suffix(m).to_ascii_lowercase());
                    for (model_key, entry) in map {
                        let sz = entry
                            .get("contextWindow")
                            .or_else(|| entry.get("context_window"))
                            .and_then(Value::as_u64)
                            .filter(|&n| n > 0)
                            .or_else(|| parse_context_window_from_model_name(model_key));
                        if let Some(sz) = sz {
                            let key_base = strip_model_context_suffix(model_key).to_ascii_lowercase();
                            if !key_base.is_empty() {
                                model_windows.insert(key_base.clone(), sz);
                            }
                            if cur_base.is_none() || cur_base.as_deref() == Some(key_base.as_str()) {
                                active_context_window = Some(sz);
                            }
                        }
                    }
                }
            }
            Some("assistant") => {
                if let Some(model) =
                    v.pointer("/message/model").and_then(Value::as_str).filter(|s| !s.trim().is_empty())
                {
                    let prev_base = meta.model.as_deref().map(|m| strip_model_context_suffix(m).to_ascii_lowercase());
                    let next_base = strip_model_context_suffix(model).to_ascii_lowercase();
                    if prev_base.as_deref() != Some(next_base.as_str()) {
                        active_context_window = None;
                        meta.model = Some(model.to_string());
                    } else if parse_context_window_from_model_name(model).is_some()
                        || meta.model.as_deref().and_then(parse_context_window_from_model_name).is_none()
                    {
                        meta.model = Some(model.to_string());
                    }
                    if let Some(sz) = parse_context_window_from_model_name(model) {
                        if !next_base.is_empty() {
                            model_windows.insert(next_base, sz);
                        }
                        active_context_window = Some(sz);
                    }
                }
                if let Some(usage) = v.pointer("/message/usage") {
                    let input_tok = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let cache_create = usage.get("cache_creation_input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let cache_read = usage.get("cache_read_input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let used = input_tok + cache_create + cache_read;
                    if used > 0 {
                        last_used = Some(used);
                    }
                }
                let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else {
                    continue;
                };
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("thinking") => {
                            if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                                let clean = text.trim();
                                if !clean.is_empty() {
                                    entries.push(Entry::Thought { turn, text: clean.to_string(), ts_ms });
                                }
                            }
                        }
                        Some("tool_use") => {
                            let id = block.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                            let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                            let input = block.get("input").unwrap_or(&Value::Null);
                            if name == "TodoWrite"
                                && let Some(todos) = input.get("todos").and_then(Value::as_array)
                            {
                                let items = format_todos(todos);
                                if !items.is_empty() {
                                    entries.push(Entry::Plan { turn, items, ts_ms });
                                }
                                continue;
                            }
                            let (tool_kind, title) = summarize_claude_tool(name, input);
                            let detail = claude_tool_detail(name, input);
                            let idx = entries.len();
                            entries.push(Entry::ToolCall {
                                turn,
                                id: id.clone(),
                                title,
                                tool_kind,
                                status: "in_progress".to_string(),
                                detail,
                                attachments: vec![],
                                ts_ms,
                            });
                            if !id.is_empty() {
                                tool_indices.insert(id, idx);
                            }
                        }
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                let clean = text.trim();
                                if !clean.is_empty() {
                                    entries.push(Entry::Assistant {
                                        turn,
                                        text: clean.to_string(),
                                        attachments: vec![],
                                        ts_ms,
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(used) = last_used {
        let model_base = meta.model.as_deref().map(|m| strip_model_context_suffix(m).to_ascii_lowercase());
        let size = active_context_window
            .or_else(|| meta.model.as_deref().and_then(parse_context_window_from_model_name))
            .or_else(|| model_base.as_deref().and_then(|b| model_windows.get(b).copied()))
            .or_else(|| meta.model.as_deref().and_then(|m| lookup_claude_settings_context_window_in(m, settings)))
            .unwrap_or(if used > 200_000 { 1_000_000 } else { 200_000 });
        meta.context_usage = Some(AcpContextUsage { used, size });
    }
    if !entries.is_empty() || !meta.is_empty() {
        if meta.session_id.is_none()
            && let Some(stem) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::trim)
                .filter(|s| !s.is_empty() && !s.starts_with("agent-"))
        {
            meta.session_id = Some(stem.to_string());
        }
        meta.provider.get_or_insert_with(|| "claude".to_string());
    }
    (entries, meta)
}

fn load_claude_settings() -> Option<Value> {
    let home = home_dir()?;
    let settings_path = claude_root(&home, None).join("settings.json");
    let raw = fs::read_to_string(settings_path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn strip_ansi_csi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for ch in chars.by_ref() {
                if ('@'..='~').contains(&ch) {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn parse_claude_model_switch_stdout(raw: &str) -> Option<String> {
    let inner = if let Some(start) = raw.find("<local-command-stdout>") {
        let after = &raw[start + "<local-command-stdout>".len()..];
        let end = after.find("</local-command-stdout>").unwrap_or(after.len());
        &after[..end]
    } else {
        return None;
    };
    let clean = strip_ansi_csi(inner);
    let lower = clean.to_ascii_lowercase();
    let prefix = "set model to ";
    let idx = lower.find(prefix)?;
    let rest = clean[idx + prefix.len()..].trim();
    if rest.is_empty() {
        return None;
    }
    if let Some(after_tick) = rest.strip_prefix('`') {
        if let Some(end_tick) = after_tick.find('`') {
            let spec = after_tick[..end_tick].trim();
            if !spec.is_empty() {
                return Some(spec.to_string());
            }
        }
    }
    let first_line = rest.lines().next().unwrap_or(rest).trim();
    let lower_line = first_line.to_ascii_lowercase();
    let cut = [" and saved as your default", " for this session", " (saved as default"]
        .iter()
        .filter_map(|marker| lower_line.find(marker))
        .min()
        .unwrap_or(first_line.len());
    let spec = first_line[..cut].trim().trim_end_matches('.').trim();
    (!spec.is_empty()).then(|| spec.to_string())
}

fn resolve_claude_model_spec(raw_spec: &str, settings: Option<&Value>) -> (String, Option<u64>) {
    let spec = raw_spec.trim();
    if let Some(v) = settings
        && let Some(opts) = v.pointer("/modelPicker/options").and_then(Value::as_array)
    {
        for opt in opts {
            let label = opt.get("label").and_then(Value::as_str).map(str::trim).unwrap_or("");
            let model = opt.get("model").and_then(Value::as_str).map(str::trim).unwrap_or("");
            if model.is_empty() {
                continue;
            }
            if (!label.is_empty() && label.eq_ignore_ascii_case(spec))
                || model.eq_ignore_ascii_case(spec)
                || strip_model_context_suffix(model).eq_ignore_ascii_case(spec)
            {
                let sz =
                    parse_context_window_from_model_name(model).or_else(|| parse_context_window_from_model_name(spec));
                return (model.to_string(), sz);
            }
        }
    }
    // Official Claude Code output like `Default (recommended) (claude-sonnet-4-6)` or `Opus (claude-opus-4-6[1m])`.
    if let Some(open) = spec.rfind('(')
        && let Some(inside) = spec[open + 1..].strip_suffix(')')
    {
        let cand = inside.trim();
        if !cand.is_empty()
            && !cand.contains(char::is_whitespace)
            && (cand.contains('-') || cand.contains('/') || cand.contains('[') || cand.contains('@'))
        {
            let sz = parse_context_window_from_model_name(cand)
                .or_else(|| parse_context_window_from_model_name(spec))
                .or_else(|| lookup_claude_settings_context_window_in(cand, settings));
            return (cand.to_string(), sz);
        }
    }
    let sz =
        parse_context_window_from_model_name(spec).or_else(|| lookup_claude_settings_context_window_in(spec, settings));
    (spec.to_string(), sz)
}

fn parse_token_size_suffix(raw: &str) -> Option<u64> {
    let s = raw.trim().to_ascii_lowercase();
    if let Some(num) = s.strip_suffix('m') {
        let v: f64 = num.parse().ok()?;
        if v.is_finite() && v > 0.0 {
            return Some((v * 1_000_000.0).round() as u64);
        }
    } else if let Some(num) = s.strip_suffix('k') {
        let v: f64 = num.parse().ok()?;
        if v.is_finite() && v > 0.0 {
            return Some((v * 1_000.0).round() as u64);
        }
    } else if let Ok(v) = s.parse::<u64>()
        && v >= 16_000
    {
        return Some(v);
    }
    None
}

fn strip_model_context_suffix(model: &str) -> &str {
    let mut s = model.trim();
    if let Some(idx) = s.rfind('[')
        && s.ends_with(']')
    {
        s = &s[..idx];
    }
    if let Some(idx) = s.rfind('@')
        && parse_token_size_suffix(&s[idx + 1..]).is_some()
    {
        s = &s[..idx];
    }
    s.trim()
}

fn parse_context_window_from_model_name(model: &str) -> Option<u64> {
    let s = model.trim();
    // Match `[1m]`, `[500k]` anywhere in the model name/label.
    let mut rest = s;
    while let Some(start) = rest.find('[') {
        let after = &rest[start + 1..];
        let Some(end) = after.find(']') else { break };
        if let Some(sz) = parse_token_size_suffix(&after[..end]) {
            return Some(sz);
        }
        rest = &after[end + 1..];
    }
    // Match `@500k` or `@ 500k` (even when followed by ` · Cursor` or whitespace).
    let mut at_rest = s;
    while let Some(at) = at_rest.find('@') {
        let after = at_rest[at + 1..].trim_start();
        let tok_end = after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '.')).unwrap_or(after.len());
        if tok_end > 0
            && let Some(sz) = parse_token_size_suffix(&after[..tok_end])
        {
            return Some(sz);
        }
        at_rest = after;
    }
    None
}

fn lookup_claude_settings_context_window_in(model: &str, settings: Option<&Value>) -> Option<u64> {
    let v = settings?;
    let base = strip_model_context_suffix(model).to_ascii_lowercase();
    if base.is_empty() {
        return None;
    }
    if let Some(opts) = v.pointer("/modelPicker/options").and_then(Value::as_array) {
        for opt in opts {
            if let Some(m) = opt.get("model").and_then(Value::as_str)
                && strip_model_context_suffix(m).eq_ignore_ascii_case(&base)
                && let Some(sz) = parse_context_window_from_model_name(m)
            {
                return Some(sz);
            }
        }
    }
    if let Some(env) = v.get("env").and_then(Value::as_object) {
        for (k, val) in env {
            if k.ends_with("_MODEL")
                && let Some(m) = val.as_str()
                && strip_model_context_suffix(m).eq_ignore_ascii_case(&base)
                && let Some(sz) = parse_context_window_from_model_name(m)
            {
                return Some(sz);
            }
        }
    }
    None
}

fn is_claude_command_noise(s: &str) -> bool {
    s.contains("<local-command-caveat>")
        || s.contains("<command-name>")
        || s.contains("<local-command-stdout>")
        || s.contains("<local-command-stderr>")
}

fn strip_system_reminders(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("<system-reminder>") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "<system-reminder>".len()..];
        if let Some(end) = after.find("</system-reminder>") {
            rest = &after[end + "</system-reminder>".len()..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn format_todos(todos: &[Value]) -> Vec<String> {
    todos
        .iter()
        .filter_map(|t| {
            let content = t.get("content").and_then(Value::as_str)?.trim();
            if content.is_empty() {
                return None;
            }
            let prefix = match t.get("status").and_then(Value::as_str) {
                Some("completed") => "[x] ",
                Some("in_progress") => "[~] ",
                _ => "[ ] ",
            };
            Some(format!("{prefix}{content}"))
        })
        .collect()
}

fn summarize_claude_tool(name: &str, input: &Value) -> (Option<String>, String) {
    let str_field = |k: &str| input.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    match name {
        "Bash" => {
            let title = str_field("command").or_else(|| str_field("description")).unwrap_or("Bash");
            (Some("execute".into()), title.to_string())
        }
        "Read" => {
            let title = match str_field("file_path") {
                Some(p) => format!("Read {p}"),
                None => "Read".to_string(),
            };
            (Some("read".into()), title)
        }
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
            let path = str_field("file_path").or_else(|| str_field("notebook_path"));
            let title = match path {
                Some(p) => format!("{name} {p}"),
                None => name.to_string(),
            };
            (Some("edit".into()), title)
        }
        "Grep" | "Glob" | "WebSearch" | "WebFetch" => {
            let q = str_field("pattern").or_else(|| str_field("query")).or_else(|| str_field("url"));
            let title = match q {
                Some(q) => format!("{name} {q}"),
                None => name.to_string(),
            };
            (Some("search".into()), title)
        }
        _ => (None, name.to_string()),
    }
}

fn codex_tool_detail(name: &str, args: &Value) -> Option<String> {
    match name {
        "apply_patch" => args.as_str().and_then(cap_tool_detail),
        "shell" | "shell_command" | "exec_command" => {
            let cmd = args.get("command").or_else(|| args.get("cmd")).and_then(Value::as_str)?.trim();
            if cmd.contains('\n') || cmd.len() > 120 { cap_tool_detail(cmd) } else { None }
        }
        _ => None,
    }
}

fn parse_codex_jsonl(path: &Path) -> (Vec<Entry>, ReadSessionMeta) {
    let lines = read_bounded_lines(path);
    let mut parsed: Vec<Value> = Vec::new();
    let mut has_event_user_msg = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) == Some("event_msg")
            && v.pointer("/payload/type").and_then(Value::as_str) == Some("user_message")
        {
            has_event_user_msg = true;
        }
        parsed.push(v);
    }

    let mut entries: Vec<Entry> = Vec::new();
    let mut meta = ReadSessionMeta::default();
    let mut tool_indices: HashMap<String, usize> = HashMap::new();
    let mut turn: u64 = 0;

    for v in parsed {
        let ts_ms = v.get("timestamp").and_then(Value::as_str).map(parse_ts_ms).unwrap_or(0);
        match v.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if meta.session_id.is_none()
                    && let Some(sid) =
                        v.pointer("/payload/id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
                {
                    meta.session_id = Some(sid.to_string());
                }
                if let Some(m) = v.pointer("/payload/model").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                    meta.model = Some(m.to_string());
                }
            }
            Some("turn_context") => {
                if let Some(payload) = v.get("payload") {
                    if let Some(m) = payload.get("model").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                        meta.model = Some(m.to_string());
                    }
                    if let Some(e) = payload.get("effort").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                        meta.reasoning_effort = Some(e.to_string());
                    }
                    if let Some(p) =
                        payload.get("approval_policy").and_then(Value::as_str).filter(|s| !s.trim().is_empty())
                    {
                        meta.mode = Some(p.to_string());
                    }
                }
            }
            Some("event_msg") => {
                let ev_type = v.pointer("/payload/type").and_then(Value::as_str);
                if ev_type == Some("user_message")
                    && let Some(msg) = v.pointer("/payload/message").and_then(Value::as_str)
                {
                    let clean = msg.trim();
                    if !clean.is_empty() {
                        turn += 1;
                        entries.push(Entry::User {
                            turn,
                            from: None,
                            text: clean.to_string(),
                            attachments: vec![],
                            ts_ms,
                        });
                    }
                } else if ev_type == Some("token_count")
                    && let Some(info) = v.pointer("/payload/info")
                {
                    let used = info
                        .pointer("/last_token_usage/total_tokens")
                        .or_else(|| info.pointer("/total_token_usage/total_tokens"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let size = info.get("model_context_window").and_then(Value::as_u64).unwrap_or(0);
                    if size > 0 {
                        meta.context_usage = Some(AcpContextUsage { used, size });
                    }
                }
            }
            Some("response_item") => {
                let Some(payload) = v.get("payload") else {
                    continue;
                };
                match payload.get("type").and_then(Value::as_str) {
                    Some("reasoning") => {
                        let summary_parts: Vec<&str> = payload
                            .get("summary")
                            .and_then(Value::as_array)
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|item| item.get("text").and_then(Value::as_str))
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty())
                                    .collect()
                            })
                            .unwrap_or_default();
                        if !summary_parts.is_empty() {
                            entries.push(Entry::Thought { turn, text: summary_parts.join("\n\n"), ts_ms });
                        }
                    }
                    Some("function_call") | Some("custom_tool_call") => {
                        let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("").to_string();
                        let name = payload.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let raw_args = payload.get("arguments").or_else(|| payload.get("input"));
                        let args_val = match raw_args {
                            Some(Value::String(s)) => {
                                serde_json::from_str::<Value>(s).unwrap_or(Value::String(s.clone()))
                            }
                            Some(other) => other.clone(),
                            None => Value::Null,
                        };
                        if name == "update_plan"
                            && let Some(plan) = args_val.get("plan").and_then(Value::as_array)
                        {
                            let items: Vec<String> = plan
                                .iter()
                                .filter_map(|p| {
                                    let step = p.get("step").and_then(Value::as_str)?.trim();
                                    if step.is_empty() {
                                        return None;
                                    }
                                    let prefix = match p.get("status").and_then(Value::as_str) {
                                        Some("completed") => "[x] ",
                                        Some("in_progress") => "[~] ",
                                        _ => "[ ] ",
                                    };
                                    Some(format!("{prefix}{step}"))
                                })
                                .collect();
                            if !items.is_empty() {
                                entries.push(Entry::Plan { turn, items, ts_ms });
                            }
                            continue;
                        }
                        let (tool_kind, title) = summarize_codex_tool(name, &args_val);
                        let detail = codex_tool_detail(name, &args_val);
                        let idx = entries.len();
                        entries.push(Entry::ToolCall {
                            turn,
                            id: call_id.clone(),
                            title,
                            tool_kind,
                            status: "in_progress".to_string(),
                            detail,
                            attachments: vec![],
                            ts_ms,
                        });
                        if !call_id.is_empty() {
                            tool_indices.insert(call_id, idx);
                        }
                    }
                    Some("function_call_output") | Some("custom_tool_call_output") => {
                        if let Some(call_id) = payload.get("call_id").and_then(Value::as_str)
                            && let Some(&idx) = tool_indices.get(call_id)
                        {
                            let failed = codex_output_failed(payload.get("output"));
                            let new_status = if failed { "failed" } else { "completed" };
                            let out = value_text_snippet(payload.get("output"));
                            merge_tool_output(&mut entries, idx, new_status, out);
                        }
                    }
                    Some("message") => {
                        let role = payload.get("role").and_then(Value::as_str);
                        if role == Some("assistant") {
                            let text = join_codex_message_texts(payload, &["output_text", "text"]);
                            if !text.is_empty() {
                                entries.push(Entry::Assistant { turn, text, attachments: vec![], ts_ms });
                            }
                        } else if role == Some("user") && !has_event_user_msg {
                            let parts: Vec<String> = payload
                                .get("content")
                                .and_then(Value::as_array)
                                .map(|arr| {
                                    arr.iter()
                                        .filter(|b| b.get("type").and_then(Value::as_str) == Some("input_text"))
                                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                                        .map(str::trim)
                                        .filter(|t| {
                                            !t.is_empty()
                                                && !t.starts_with("<environment_context>")
                                                && !t.starts_with("<permissions instructions>")
                                                && !t.starts_with("# AGENTS.md")
                                        })
                                        .map(str::to_string)
                                        .collect()
                                })
                                .unwrap_or_default();
                            if !parts.is_empty() {
                                turn += 1;
                                entries.push(Entry::User {
                                    turn,
                                    from: None,
                                    text: parts.join("\n\n"),
                                    attachments: vec![],
                                    ts_ms,
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if !entries.is_empty() || !meta.is_empty() {
        if meta.session_id.is_none() {
            meta.session_id = codex_sid_from_header(path);
        }
        meta.provider.get_or_insert_with(|| "codex".to_string());
    }
    (entries, meta)
}

fn join_codex_message_texts(payload: &Value, types: &[&str]) -> String {
    payload
        .get("content")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter(|b| b.get("type").and_then(Value::as_str).is_some_and(|t| types.contains(&t)))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

fn summarize_codex_tool(name: &str, args: &Value) -> (Option<String>, String) {
    let str_field = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    match name {
        "shell" | "shell_command" | "exec_command" => {
            if let Some(cmd) = str_field("command").or_else(|| str_field("cmd")) {
                return (Some("execute".into()), cmd.to_string());
            }
            if let Some(arr) = args.get("command").and_then(Value::as_array) {
                let joined: Vec<&str> = arr.iter().filter_map(Value::as_str).collect();
                if !joined.is_empty() {
                    return (Some("execute".into()), joined.join(" "));
                }
            }
            (Some("execute".into()), name.to_string())
        }
        "apply_patch" => {
            let summary = args
                .as_str()
                .and_then(|s| {
                    s.lines().find(|l| {
                        l.starts_with("*** Update File:")
                            || l.starts_with("*** Add File:")
                            || l.starts_with("*** Delete File:")
                    })
                })
                .map(|l| l.trim_start_matches('*').trim().to_string())
                .unwrap_or_else(|| "apply_patch".to_string());
            (Some("edit".into()), summary)
        }
        "read_file" | "view_file" => {
            let p = str_field("path").or_else(|| str_field("file_path"));
            (Some("read".into()), p.map_or_else(|| name.to_string(), |p| format!("{name} {p}")))
        }
        "grep" | "rg" | "find_files" | "web_search" => {
            let q = str_field("pattern").or_else(|| str_field("query")).or_else(|| str_field("q"));
            (Some("search".into()), q.map_or_else(|| name.to_string(), |q| format!("{name} {q}")))
        }
        _ => (None, name.to_string()),
    }
}

fn codex_output_failed(output: Option<&Value>) -> bool {
    let Some(output) = output else {
        return false;
    };
    let text = match output {
        Value::String(s) => s.as_str(),
        Value::Object(obj) => {
            if obj.get("is_error").and_then(Value::as_bool) == Some(true) {
                return true;
            }
            if let Some(code) = output.pointer("/metadata/exit_code").and_then(Value::as_i64)
                && code != 0
            {
                return true;
            }
            obj.get("output").and_then(Value::as_str).unwrap_or("")
        }
        _ => return false,
    };
    if let Ok(parsed) = serde_json::from_str::<Value>(text)
        && let Some(code) = parsed.pointer("/metadata/exit_code").and_then(Value::as_i64)
        && code != 0
    {
        return true;
    }
    if let Some(rest) = text.trim_start().strip_prefix("Exit code:")
        && let Some(num) = rest.trim_start().split_whitespace().next()
        && num.parse::<i64>().is_ok_and(|c| c != 0)
    {
        return true;
    }
    false
}

fn parse_gemini_json(path: &Path) -> (Vec<Entry>, ReadSessionMeta) {
    let mut meta = ReadSessionMeta::default();
    let Ok(raw) = fs::read_to_string(path) else {
        return (Vec::new(), meta);
    };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        return (Vec::new(), meta);
    };
    let Some(msgs) = v.get("messages").and_then(Value::as_array) else {
        return (Vec::new(), meta);
    };
    let mut entries = Vec::new();
    let mut turn: u64 = 0;

    for m in msgs {
        let ts_ms = m.get("timestamp").and_then(Value::as_str).map(parse_ts_ms).unwrap_or(0);
        if let Some(model) = m.get("model").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
            meta.model = Some(model.to_string());
        }
        match m.get("type").and_then(Value::as_str) {
            Some("user") => {
                let text = extract_gemini_text(m.get("content"));
                if !text.is_empty() {
                    turn += 1;
                    entries.push(Entry::User { turn, from: None, text, attachments: vec![], ts_ms });
                }
            }
            Some("gemini" | "assistant" | "model") => {
                if let Some(thoughts) = m.get("thoughts").and_then(Value::as_array) {
                    for th in thoughts {
                        let text = match th {
                            Value::String(s) => s.trim().to_string(),
                            Value::Object(obj) => {
                                let subj = obj.get("subject").and_then(Value::as_str).map(str::trim).unwrap_or("");
                                let desc = obj.get("description").and_then(Value::as_str).map(str::trim).unwrap_or("");
                                match (subj.is_empty(), desc.is_empty()) {
                                    (false, false) => format!("**{subj}**\n\n{desc}"),
                                    (false, true) => subj.to_string(),
                                    (true, false) => desc.to_string(),
                                    (true, true) => String::new(),
                                }
                            }
                            _ => String::new(),
                        };
                        if !text.is_empty() {
                            entries.push(Entry::Thought { turn, text, ts_ms });
                        }
                    }
                }
                if let Some(tcs) = m.get("toolCalls").and_then(Value::as_array) {
                    for tc in tcs {
                        let id = tc.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                        let name = tc.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let args = tc.get("args").unwrap_or(&Value::Null);
                        let (tool_kind, title) = summarize_generic_tool(name, args);
                        let status = match tc.get("status").and_then(Value::as_str) {
                            Some("error" | "failed" | "cancelled") => "failed",
                            Some("executing" | "scheduled" | "awaiting_approval" | "pending" | "in_progress") => {
                                "in_progress"
                            }
                            _ => "completed",
                        };
                        let detail = merge_tool_detail_parts(
                            generic_tool_detail(name, args),
                            value_text_snippet(tc.get("resultDisplay").or_else(|| tc.get("result"))),
                        );
                        entries.push(Entry::ToolCall {
                            turn,
                            id,
                            title,
                            tool_kind,
                            status: status.to_string(),
                            detail,
                            attachments: vec![],
                            ts_ms,
                        });
                    }
                }
                let text = extract_gemini_text(m.get("content"));
                if !text.is_empty() {
                    entries.push(Entry::Assistant { turn, text, attachments: vec![], ts_ms });
                }
            }
            _ => {}
        }
    }
    if !entries.is_empty() || !meta.is_empty() {
        if meta.session_id.is_none()
            && let Some(sid) = v.get("sessionId").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
        {
            meta.session_id = Some(sid.to_string());
        }
        meta.provider.get_or_insert_with(|| "gemini".to_string());
    }
    (entries, meta)
}

fn extract_gemini_text(val: Option<&Value>) -> String {
    match val {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| match p {
                Value::String(s) => Some(s.trim()),
                Value::Object(obj) => obj.get("text").and_then(Value::as_str).map(str::trim),
                _ => None,
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

fn generic_tool_detail(name: &str, args: &Value) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let str_field = |k: &str| args.get(k).and_then(Value::as_str);
    if lower.contains("edit") || lower.contains("replace") {
        let path = str_field("file_path")
            .or_else(|| str_field("path"))
            .or_else(|| str_field("absolute_path"))
            .or_else(|| str_field("Path"))
            .unwrap_or("file");
        let (old_s, new_s) = str_field("old_string")
            .map(|o| (o, str_field("new_string").unwrap_or("")))
            .or_else(|| str_field("oldText").map(|o| (o, str_field("newText").unwrap_or(""))))
            .or_else(|| str_field("Target").map(|o| (o, str_field("Replacement").unwrap_or(""))))
            .unwrap_or(("", ""));
        if old_s.is_empty() && new_s.is_empty() {
            return None;
        }
        let mut diff = format!("--- {path}\n+++ {path}");
        for line in old_s.lines().take(30) {
            diff.push_str("\n- ");
            diff.push_str(line);
        }
        for line in new_s.lines().take(30) {
            diff.push_str("\n+ ");
            diff.push_str(line);
        }
        cap_tool_detail(&diff)
    } else if lower.contains("write") {
        let content = str_field("content").or_else(|| str_field("Content"))?;
        cap_tool_detail(content)
    } else if lower.contains("shell") || lower.contains("bash") || lower.contains("exec") || lower.contains("command") {
        let cmd = str_field("command").or_else(|| str_field("cmd")).or_else(|| str_field("CommandLine"))?.trim();
        if cmd.contains('\n') || cmd.len() > 100 { cap_tool_detail(cmd) } else { None }
    } else {
        None
    }
}

fn merge_tool_detail_parts(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(x), Some(y)) if !x.contains(&y) => cap_tool_detail(&format!("{x}\n\n{y}")),
        (Some(x), _) | (_, Some(x)) => Some(x),
        (None, None) => None,
    }
}

fn extract_plan_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|line| {
            line.starts_with("- ")
                || line.starts_with("* ")
                || line
                    .split_once(". ")
                    .is_some_and(|(num, _)| !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(|line| {
            if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
                rest.trim().to_string()
            } else if let Some((_, rest)) = line.split_once(". ") {
                rest.trim().to_string()
            } else {
                line.to_string()
            }
        })
        .filter(|s| !s.is_empty())
        .take(40)
        .collect()
}

fn summarize_generic_tool(name: &str, args: &Value) -> (Option<String>, String) {
    let lower = name.to_ascii_lowercase();
    let str_field = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    if lower.contains("shell") || lower.contains("bash") || lower.contains("exec") || lower.contains("command") {
        let cmd =
            str_field("command").or_else(|| str_field("cmd")).or_else(|| str_field("CommandLine")).unwrap_or(name);
        (Some("execute".into()), cmd.to_string())
    } else if lower.contains("read") {
        let p = str_field("file_path")
            .or_else(|| str_field("path"))
            .or_else(|| str_field("absolute_path"))
            .or_else(|| str_field("Path"));
        (Some("read".into()), p.map_or_else(|| name.to_string(), |p| format!("{name} {p}")))
    } else if lower.contains("edit") || lower.contains("write") || lower.contains("replace") {
        let p = str_field("file_path").or_else(|| str_field("path")).or_else(|| str_field("Path"));
        (Some("edit".into()), p.map_or_else(|| name.to_string(), |p| format!("{name} {p}")))
    } else if lower.contains("search") || lower.contains("grep") || lower.contains("glob") || lower.contains("find") {
        let q = str_field("pattern").or_else(|| str_field("query")).or_else(|| str_field("Query"));
        (Some("search".into()), q.map_or_else(|| name.to_string(), |q| format!("{name} {q}")))
    } else {
        (None, name.to_string())
    }
}

fn parse_pi_jsonl(path: &Path) -> (Vec<Entry>, ReadSessionMeta) {
    let lines = read_bounded_lines(path);
    let mut entries = Vec::new();
    let mut meta = ReadSessionMeta::default();
    let mut tool_indices: HashMap<String, usize> = HashMap::new();
    let mut turn: u64 = 0;

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some(eff) = v
            .get("thinkingLevel")
            .or_else(|| v.get("reasoningEffort"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            meta.reasoning_effort = Some(eff.to_string());
        }
        if v.get("type").and_then(Value::as_str) == Some("session") {
            if meta.session_id.is_none()
                && let Some(sid) = v.get("id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
            {
                meta.session_id = Some(sid.to_string());
            }
            continue;
        }
        if v.get("type").and_then(Value::as_str) == Some("model_change")
            && let Some(m) = v.get("modelId").and_then(Value::as_str).filter(|s| !s.trim().is_empty())
        {
            meta.model = Some(m.to_string());
            continue;
        }
        if v.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let ts_ms = v.get("timestamp").and_then(Value::as_str).map(parse_ts_ms).unwrap_or(0);
        let Some(msg) = v.get("message") else {
            continue;
        };
        if let Some(eff) = msg
            .get("thinkingLevel")
            .or_else(|| msg.get("reasoningEffort"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            meta.reasoning_effort = Some(eff.to_string());
        }
        match msg.get("role").and_then(Value::as_str) {
            Some("user") => {
                let text = extract_gemini_text(msg.get("content"));
                if !text.is_empty() {
                    turn += 1;
                    entries.push(Entry::User { turn, from: None, text, attachments: vec![], ts_ms });
                }
            }
            Some("assistant") => {
                if let Some(m) = msg.get("model").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                    meta.model = Some(m.to_string());
                }
                let Some(blocks) = msg.get("content").and_then(Value::as_array) else {
                    continue;
                };
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("thinking") => {
                            if let Some(t) = block.get("thinking").and_then(Value::as_str) {
                                let clean = t.trim();
                                if !clean.is_empty() {
                                    entries.push(Entry::Thought { turn, text: clean.to_string(), ts_ms });
                                }
                            }
                        }
                        Some("toolCall") => {
                            let id = block.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                            let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                            let args = block.get("arguments").unwrap_or(&Value::Null);
                            let (tool_kind, title) = summarize_generic_tool(name, args);
                            let detail = generic_tool_detail(name, args);
                            let idx = entries.len();
                            entries.push(Entry::ToolCall {
                                turn,
                                id: id.clone(),
                                title,
                                tool_kind,
                                status: "in_progress".to_string(),
                                detail,
                                attachments: vec![],
                                ts_ms,
                            });
                            if !id.is_empty() {
                                tool_indices.insert(id, idx);
                            }
                        }
                        Some("text") => {
                            if let Some(t) = block.get("text").and_then(Value::as_str) {
                                let clean = t.trim();
                                if !clean.is_empty() {
                                    entries.push(Entry::Assistant {
                                        turn,
                                        text: clean.to_string(),
                                        attachments: vec![],
                                        ts_ms,
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("toolResult") => {
                if let Some(id) = msg.get("toolCallId").and_then(Value::as_str)
                    && let Some(&idx) = tool_indices.get(id)
                {
                    let is_err = msg.get("isError").and_then(Value::as_bool) == Some(true);
                    let new_status = if is_err { "failed" } else { "completed" };
                    let out = value_text_snippet(msg.get("content"));
                    merge_tool_output(&mut entries, idx, new_status, out);
                }
            }
            _ => {}
        }
    }
    if !entries.is_empty() || !meta.is_empty() {
        meta.provider.get_or_insert_with(|| "pi".to_string());
    }
    (entries, meta)
}

fn parse_agy_jsonl(path: &Path) -> (Vec<Entry>, ReadSessionMeta) {
    let lines = read_bounded_lines(path);
    let mut entries = Vec::new();
    let mut meta = ReadSessionMeta::default();
    let mut turn: u64 = 0;

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some(m) = v.get("model").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
            meta.model = Some(m.to_string());
        }
        let ts_ms = v.get("created_at").and_then(Value::as_str).map(parse_ts_ms).unwrap_or(0);
        match v.get("type").and_then(Value::as_str) {
            Some("USER_INPUT") => {
                let raw = v.get("content").and_then(Value::as_str).unwrap_or("");
                let clean = strip_user_request_wrapper(raw);
                if !clean.is_empty() {
                    turn += 1;
                    entries.push(Entry::User { turn, from: None, text: clean, attachments: vec![], ts_ms });
                }
            }
            Some("PLANNER_RESPONSE") => {
                if let Some(thinking) = v.get("thinking").and_then(Value::as_str) {
                    let clean = thinking.trim();
                    if !clean.is_empty() {
                        entries.push(Entry::Thought { turn, text: clean.to_string(), ts_ms });
                    }
                }
                if let Some(tcs) = v.get("tool_calls").and_then(Value::as_array) {
                    for tc in tcs {
                        let id = tc.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                        let name = tc.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let raw_args = tc.get("arguments").or_else(|| tc.get("args"));
                        let parsed_args: Value;
                        let args = match raw_args {
                            Some(Value::String(s)) => {
                                parsed_args = serde_json::from_str::<Value>(s).unwrap_or(Value::Null);
                                &parsed_args
                            }
                            Some(v) => v,
                            None => &Value::Null,
                        };
                        if name == "exit_plan_mode"
                            && let Some(plan_text) =
                                args.get("plan").and_then(Value::as_str).filter(|s| !s.trim().is_empty())
                        {
                            let items = extract_plan_lines(plan_text);
                            if !items.is_empty() {
                                entries.push(Entry::Plan { turn, items, ts_ms });
                            } else if let Some(detail) = cap_tool_detail(plan_text) {
                                entries.push(Entry::ToolCall {
                                    turn,
                                    id,
                                    title: "exit_plan_mode".to_string(),
                                    tool_kind: Some("plan".into()),
                                    status: "completed".to_string(),
                                    detail: Some(detail),
                                    attachments: vec![],
                                    ts_ms,
                                });
                            }
                            continue;
                        }
                        let (tool_kind, title) = summarize_generic_tool(name, args);
                        let detail = generic_tool_detail(name, args);
                        entries.push(Entry::ToolCall {
                            turn,
                            id,
                            title,
                            tool_kind,
                            status: "completed".to_string(),
                            detail,
                            attachments: vec![],
                            ts_ms,
                        });
                    }
                }
                if let Some(content) = v.get("content").and_then(Value::as_str) {
                    let clean = content.trim();
                    if !clean.is_empty() {
                        entries.push(Entry::Assistant { turn, text: clean.to_string(), attachments: vec![], ts_ms });
                    }
                }
            }
            _ => {}
        }
    }
    if !entries.is_empty() || !meta.is_empty() {
        if meta.session_id.is_none() {
            meta.session_id = agy_cid_from_path(path);
        }
        meta.provider.get_or_insert_with(|| "agy".to_string());
    }
    (entries, meta)
}

fn strip_xml_tag_blocks(raw: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find(&open) {
        out.push_str(&rest[..start]);
        let after = &rest[start + open.len()..];
        if let Some(end) = after.find(&close) {
            rest = &after[end + close.len()..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

fn strip_user_request_wrapper(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("[plan review]") {
        return String::new();
    }
    let extracted = if let Some(start) = trimmed.find("<USER_REQUEST>") {
        let after = &trimmed[start + "<USER_REQUEST>".len()..];
        if let Some(end) = after.find("</USER_REQUEST>") {
            after[..end].to_string()
        } else {
            let cut = ["<ADDITIONAL_METADATA>", "<SYSTEM_MESSAGE>"]
                .iter()
                .filter_map(|tag| after.find(tag))
                .min()
                .unwrap_or(after.len());
            after[..cut].to_string()
        }
    } else {
        trimmed.to_string()
    };
    let no_meta = strip_xml_tag_blocks(&extracted, "ADDITIONAL_METADATA");
    let no_sys = strip_xml_tag_blocks(&no_meta, "SYSTEM_MESSAGE");
    no_sys.trim().to_string()
}

pub(crate) fn merge_sent_prompts(entries: &mut Vec<Entry>, sent_prompts: &[(u64, u64, String)]) {
    if sent_prompts.is_empty() {
        return;
    }
    let last_disk_user_ts = entries
        .iter()
        .filter_map(|e| match e {
            Entry::User { ts_ms, .. } => Some(*ts_ms),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let mut recent_user_texts: Vec<String> = entries
        .iter()
        .rev()
        .filter_map(|e| match e {
            Entry::User { text, .. } => Some(text.trim().to_string()),
            _ => None,
        })
        .take(8)
        .collect();
    for (turn, ts_ms, text) in sent_prompts {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if (last_disk_user_ts == 0 || *ts_ms > last_disk_user_ts) && !recent_user_texts.iter().any(|u| u == trimmed) {
            entries.push(Entry::User {
                turn: (*turn).max(1),
                from: None,
                text: text.clone(),
                attachments: vec![],
                ts_ms: *ts_ms,
            });
            recent_user_texts.push(trimmed.to_string());
        }
    }
}

fn cap_entries(entries: &mut Vec<Entry>) {
    if entries.len() > MAX_ENTRIES {
        let drop_n = entries.len() - MAX_ENTRIES;
        entries.drain(0..drop_n);
    }
    let mut total_bytes: usize = entries.iter().map(entry_approx_bytes).sum();
    let mut drop_n = 0usize;
    while drop_n < entries.len() && total_bytes > MAX_BYTES {
        total_bytes -= entry_approx_bytes(&entries[drop_n]);
        drop_n += 1;
    }
    if drop_n > 0 {
        entries.drain(0..drop_n);
    }
}

fn entry_approx_bytes(e: &Entry) -> usize {
    e.approx_bytes()
}

pub(crate) fn user_home_dir() -> Option<PathBuf> {
    home_dir()
}

pub(crate) fn is_claude_user_noise(raw: &str) -> bool {
    is_claude_command_noise(raw) || raw.trim_start().starts_with("[Request interrupted by user")
}

pub(crate) fn clean_claude_text(raw: &str) -> String {
    strip_system_reminders(raw)
}

pub(crate) fn find_claude_session_file(cwd: &str, session_id: &str) -> Option<PathBuf> {
    if session_id.trim().is_empty() {
        return None;
    }
    let home = home_dir()?;
    let projects_dir = claude_root(&home, None).join("projects");
    let direct = projects_dir.join(claude_slug(cwd)).join(format!("{session_id}.jsonl"));
    if direct.is_file() {
        return Some(direct);
    }
    let entries = fs::read_dir(&projects_dir).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join(format!("{session_id}.jsonl"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub(crate) fn find_codex_rollout_by_sid(cwd: &str, sid: &str) -> Option<PathBuf> {
    let home = home_dir()?;
    let suffix = format!("-{sid}.jsonl");
    let mut matches = Vec::new();
    for c in discover_codex(&home, None, cwd, 0) {
        let name = c.path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.ends_with(&suffix) {
            matches.push(c);
        }
    }
    matches.sort_by(|a, b| b.mtime_ms.cmp(&a.mtime_ms));
    matches.into_iter().next().map(|c| c.path)
}

pub(crate) fn find_agy_transcript_by_cid(cid: &str) -> Option<PathBuf> {
    if cid.trim().is_empty() {
        return None;
    }
    let home = home_dir()?;
    let logs = home.join(".gemini/antigravity-cli/brain").join(cid).join(".system_generated/logs");
    pick_agy_log_file(&logs)
}

pub(crate) fn discover_cli_transcript(
    kind: &str,
    cwd: &str,
    min_mtime_ms: u64,
    fg_pid: Option<u32>,
    allow_older_fallback: bool,
) -> Option<PathBuf> {
    let home = home_dir()?;
    let fg_started = (!allow_older_fallback).then(|| fg_pid.and_then(proc_started_ms)).flatten();
    let effective_start =
        if allow_older_fallback { 0 } else { fg_started.map_or(min_mtime_ms, |f| min_mtime_ms.max(f)) };
    let scan_min_mtime = if allow_older_fallback { 0 } else { effective_start.saturating_sub(MTIME_SLACK_MS) };
    let mut candidates = match kind {
        "claude" => discover_claude(&home, None, cwd, scan_min_mtime),
        "codex" => discover_codex(&home, None, cwd, scan_min_mtime),
        "cursor" => discover_cursor(&home, cwd, scan_min_mtime),
        "agy" => discover_agy(&home, scan_min_mtime, fg_pid),
        _ => return None,
    };
    if !allow_older_fallback && let Some(fg_ms) = fg_started {
        // A clean non-resumed CLI session can only own transcripts created once that foreground
        // CLI process started (2s slack for filesystem/clock jitter), never a previous session.
        let strict_floor = fg_ms.saturating_sub(2_000);
        candidates.retain(|c| c.first_ts_ms > 0 && c.first_ts_ms >= strict_floor);
    }
    pick_best_candidate(candidates, effective_start, fg_started, allow_older_fallback).map(|c| c.path)
}

pub(crate) fn parse_cli_transcript(kind: &str, path: &Path) -> Vec<Entry> {
    match kind {
        "claude" => parse_claude_jsonl(path).0,
        "codex" => parse_codex_jsonl(path).0,
        "cursor" => parse_cursor_jsonl(path).0,
        "agy" => parse_agy_jsonl(path).0,
        _ => Vec::new(),
    }
}

fn cursor_project_slug(cwd: &str) -> String {
    let mut out = String::with_capacity(cwd.len());
    let mut prev_dash = true;
    for ch in cwd.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn discover_cursor(home: &Path, cwd: &str, min_mtime: u64) -> Vec<Candidate> {
    let projects_dir = home.join(".cursor").join("projects");
    if !projects_dir.is_dir() {
        return Vec::new();
    }
    let slug = cursor_project_slug(cwd);
    if slug.is_empty() {
        return Vec::new();
    }
    let direct_dir = projects_dir.join(&slug);
    let slug_dir = if direct_dir.is_dir() {
        Some(direct_dir)
    } else {
        // On Windows, drive letter casing can differ (`d-Users-...` vs `D-Users-...`).
        fs::read_dir(&projects_dir)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.is_dir()
                    && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case(&slug))
            })
    };
    let Some(slug_dir) = slug_dir else {
        return Vec::new();
    };
    let transcripts_dir = slug_dir.join("agent-transcripts");
    let Ok(entries) = fs::read_dir(&transcripts_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let jsonl = if path.is_dir() {
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let nested = path.join(format!("{name}.jsonl"));
            if nested.is_file() {
                nested
            } else {
                let Some(found) = fs::read_dir(&path)
                    .ok()
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .find(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
                else {
                    continue;
                };
                found
            }
        } else if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            path
        } else {
            continue;
        };
        let mt = mtime_ms(&jsonl);
        if mt > 0 && mt < min_mtime {
            continue;
        }
        let first_ts = cursor_first_ts_ms(&jsonl, mt);
        out.push(Candidate { path: jsonl, provider: Provider::Cursor, mtime_ms: mt, first_ts_ms: first_ts });
    }
    out
}

fn cursor_first_ts_ms(path: &Path, mtime: u64) -> u64 {
    // Prefer exact filesystem creation time (NTFS/APFS/ext4 statx) because Cursor's inline
    // `<timestamp>` tag has minute granularity (`11:19 AM`), which can precede `started_ms` by up to 59s.
    let ctime = created_ms(path);
    if ctime > 0 {
        return ctime;
    }
    if let Ok(file) = File::open(path) {
        for line in BufReader::new(file).lines().take(8).flatten() {
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if let Some(ts) = extract_cursor_message_ts_ms(&v) {
                // Minute-granular timestamp: clamp upper bound to mtime if written within the same minute.
                return if mtime >= ts { mtime.min(ts.saturating_add(59_999)) } else { ts };
            }
        }
    }
    0
}

fn extract_cursor_message_ts_ms(v: &Value) -> Option<u64> {
    if let Some(ts) = v.get("timestamp").and_then(Value::as_str).map(parse_ts_ms).filter(|&t| t > 0) {
        return Some(ts);
    }
    let content = v.pointer("/message/content")?;
    match content {
        Value::String(s) => parse_cursor_timestamp_tag(s),
        Value::Array(blocks) => blocks.iter().find_map(|b| {
            b.get("text").and_then(Value::as_str).and_then(parse_cursor_timestamp_tag)
        }),
        _ => None,
    }
}

/// Parses Cursor's inline `<timestamp>Thursday, Nov 26, 2026, 11:19 AM (UTC+8)</timestamp>` into UTC ms.
fn parse_cursor_timestamp_tag(raw: &str) -> Option<u64> {
    let start = raw.find("<timestamp>")?;
    let after = &raw[start + "<timestamp>".len()..];
    let end = after.find("</timestamp>")?;
    parse_cursor_human_ts(&after[..end])
}

fn parse_cursor_human_ts(raw: &str) -> Option<u64> {
    let mut s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let mut tz_offset_secs: i64 = 0;
    if let Some(open) = s.rfind('(')
        && let Some(inside) = s[open + 1..].strip_suffix(')')
    {
        let tz = inside.trim();
        if let Some(rest) = tz.strip_prefix("UTC").or_else(|| tz.strip_prefix("GMT")) {
            let rest = rest.trim();
            if !rest.is_empty() && (rest.starts_with('+') || rest.starts_with('-')) {
                let sign = if rest.starts_with('+') { 1i64 } else { -1i64 };
                let off = &rest[1..];
                let (hh, mm) = if let Some((h, m)) = off.split_once(':') {
                    (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?)
                } else {
                    (off.parse::<i64>().ok()?, 0)
                };
                tz_offset_secs = sign * (hh * 3600 + mm * 60);
            }
        }
        s = s[..open].trim();
    }
    // Strip optional leading weekday (`Thursday, `).
    let parts: Vec<&str> = s.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
    let (month_day, year_str, time_str) = match parts.as_slice() {
        [_, md, yr, tm] => (*md, *yr, *tm),
        [md, yr, tm] => (*md, *yr, *tm),
        _ => return None,
    };
    let mut md_it = month_day.split_whitespace();
    let mon_str = md_it.next()?;
    let day: u32 = md_it.next()?.parse().ok()?;
    let month: u32 = match mon_str.to_ascii_lowercase().as_str() {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "sept" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => return None,
    };
    let year: i64 = year_str.parse().ok()?;
    let mut tm_it = time_str.split_whitespace();
    let clock = tm_it.next()?;
    let ampm = tm_it.next().map(|a| a.to_ascii_uppercase());
    let mut clock_it = clock.split(':');
    let mut hour: i64 = clock_it.next()?.parse().ok()?;
    let min: i64 = clock_it.next()?.parse().ok()?;
    let sec: i64 = clock_it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    if let Some(mer) = ampm.as_deref() {
        match mer {
            "AM" => {
                if hour == 12 {
                    hour = 0;
                }
            }
            "PM" => {
                if hour < 12 {
                    hour += 12;
                }
            }
            _ => {}
        }
    }
    let days = days_from_civil(year, month, day)?;
    let secs = days * 86_400 + hour * 3600 + min * 60 + sec - tz_offset_secs;
    (secs >= 0).then_some((secs as u64) * 1000)
}

fn clean_cursor_user_text(raw: &str) -> String {
    let mut s = raw.to_string();
    while let Some(start) = s.find("<timestamp>") {
        let after_start = start + "<timestamp>".len();
        if let Some(rel_end) = s[after_start..].find("</timestamp>") {
            let end = after_start + rel_end + "</timestamp>".len();
            s.replace_range(start..end, "");
        } else {
            break;
        }
    }
    if let Some(start) = s.find("<user_query>") {
        let after = &s[start + "<user_query>".len()..];
        let inner = if let Some(end) = after.find("</user_query>") { &after[..end] } else { after };
        return strip_system_reminders(inner);
    }
    strip_system_reminders(&s)
}

fn summarize_cursor_tool(name: &str, input: &Value) -> (Option<String>, String) {
    let str_field = |k: &str| input.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    match name {
        "Shell" | "Bash" | "RunCommand" => {
            let title = str_field("command").or_else(|| str_field("description")).unwrap_or(name);
            (Some("execute".into()), title.to_string())
        }
        "AwaitShell" => {
            let title = match str_field("shell_id") {
                Some(id) => format!("AwaitShell {id}"),
                None => "AwaitShell".to_string(),
            };
            (Some("execute".into()), title)
        }
        "Read" | "ReadFile" => {
            let path = str_field("path").or_else(|| str_field("file_path"));
            let title = match path {
                Some(p) => format!("Read {p}"),
                None => "Read".to_string(),
            };
            (Some("read".into()), title)
        }
        "ReadLints" => {
            let first_path = input
                .get("paths")
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let title = match first_path {
                Some(p) => format!("ReadLints {p}"),
                None => "ReadLints".to_string(),
            };
            (Some("read".into()), title)
        }
        "Write" | "StrReplace" | "Edit" | "EditFile" | "ApplyPatch" | "Delete" => {
            let path = str_field("path").or_else(|| str_field("file_path"));
            let title = match path {
                Some(p) => format!("{name} {p}"),
                None => name.to_string(),
            };
            (Some("edit".into()), title)
        }
        "Glob" | "Grep" | "SemanticSearch" | "WebSearch" | "WebFetch" => {
            let q = str_field("glob_pattern")
                .or_else(|| str_field("pattern"))
                .or_else(|| str_field("query"))
                .or_else(|| str_field("search_term"))
                .or_else(|| str_field("url"));
            let title = match q {
                Some(q) => format!("{name} {q}"),
                None => name.to_string(),
            };
            (Some("search".into()), title)
        }
        _ => (None, name.to_string()),
    }
}

fn cursor_tool_detail(name: &str, input: &Value) -> Option<String> {
    match name {
        "StrReplace" | "Edit" | "EditFile" => {
            let path = input
                .get("path")
                .or_else(|| input.get("file_path"))
                .and_then(Value::as_str)
                .unwrap_or("file");
            let old_t = input.get("old_string").and_then(Value::as_str).unwrap_or("");
            let new_t = input.get("new_string").and_then(Value::as_str).unwrap_or("");
            if old_t.is_empty() && new_t.is_empty() {
                return None;
            }
            let mut diff = format!("--- {path}\n+++ {path}");
            for line in old_t.lines().take(25) {
                diff.push_str("\n- ");
                diff.push_str(line);
            }
            for line in new_t.lines().take(25) {
                diff.push_str("\n+ ");
                diff.push_str(line);
            }
            cap_tool_detail(&diff)
        }
        "Write" => input
            .get("contents")
            .or_else(|| input.get("content"))
            .and_then(Value::as_str)
            .and_then(cap_tool_detail),
        "ApplyPatch" => input
            .as_str()
            .or_else(|| input.get("patch").and_then(Value::as_str))
            .and_then(cap_tool_detail),
        "Shell" | "Bash" | "RunCommand" => {
            let cmd = input.get("command").and_then(Value::as_str)?.trim();
            if cmd.contains('\n') || cmd.len() > 120 { cap_tool_detail(cmd) } else { None }
        }
        _ => None,
    }
}

fn complete_open_tools(entries: &mut [Entry], open_tools: &mut Vec<usize>, new_status: &str) {
    for idx in open_tools.drain(..) {
        if let Some(Entry::ToolCall { status, .. }) = entries.get_mut(idx)
            && status == "in_progress"
        {
            *status = new_status.to_string();
        }
    }
}

fn parse_cursor_jsonl(path: &Path) -> (Vec<Entry>, ReadSessionMeta) {
    let lines = read_bounded_lines(path);
    let mut entries: Vec<Entry> = Vec::new();
    let mut meta = ReadSessionMeta::default();
    let mut open_tools: Vec<usize> = Vec::new();
    let mut turn: u64 = 0;
    let mut tool_seq: usize = 0;
    let mut last_ts_ms: u64 = created_ms(path);

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) == Some("turn_ended") {
            let status_str = v.get("status").and_then(Value::as_str).unwrap_or("success");
            let is_err = matches!(status_str, "error" | "failed");
            complete_open_tools(&mut entries, &mut open_tools, if is_err { "failed" } else { "completed" });
            if is_err
                && let Some(err_msg) = v
                    .get("error")
                    .and_then(|e| e.as_str().or_else(|| e.get("message").and_then(Value::as_str)))
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            {
                entries.push(Entry::Error { turn, message: err_msg.to_string(), ts_ms: last_ts_ms });
            }
            continue;
        }
        let role = v.get("role").and_then(Value::as_str);
        match role {
            Some("user") => {
                complete_open_tools(&mut entries, &mut open_tools, "completed");
                if let Some(ts) = extract_cursor_message_ts_ms(&v) {
                    last_ts_ms = ts;
                }
                let Some(content) = v.pointer("/message/content") else {
                    continue;
                };
                let mut text_parts = Vec::new();
                match content {
                    Value::String(s) => {
                        let clean = clean_cursor_user_text(s);
                        if !clean.is_empty() {
                            text_parts.push(clean);
                        }
                    }
                    Value::Array(blocks) => {
                        for block in blocks {
                            if block.get("type").and_then(Value::as_str) == Some("text")
                                && let Some(raw) = block.get("text").and_then(Value::as_str)
                            {
                                let clean = clean_cursor_user_text(raw);
                                if !clean.is_empty() {
                                    text_parts.push(clean);
                                }
                            }
                        }
                    }
                    _ => {}
                }
                if !text_parts.is_empty() {
                    turn += 1;
                    entries.push(Entry::User {
                        turn,
                        from: None,
                        text: text_parts.join("\n\n"),
                        attachments: vec![],
                        ts_ms: last_ts_ms,
                    });
                }
            }
            Some("assistant") => {
                // Each subsequent assistant message implies tools from the prior message finished.
                complete_open_tools(&mut entries, &mut open_tools, "completed");
                if let Some(m) = v.pointer("/message/model").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
                    meta.model = Some(m.to_string());
                }
                let Some(content) = v.pointer("/message/content") else {
                    continue;
                };
                match content {
                    Value::String(s) => {
                        let clean = s.trim();
                        if !clean.is_empty() {
                            entries.push(Entry::Assistant {
                                turn,
                                text: clean.to_string(),
                                attachments: vec![],
                                ts_ms: last_ts_ms,
                            });
                        }
                    }
                    Value::Array(blocks) => {
                        for block in blocks {
                            match block.get("type").and_then(Value::as_str) {
                                Some("thinking") => {
                                    if let Some(text) = block
                                        .get("thinking")
                                        .or_else(|| block.get("text"))
                                        .and_then(Value::as_str)
                                    {
                                        let clean = text.trim();
                                        if !clean.is_empty() {
                                            entries.push(Entry::Thought {
                                                turn,
                                                text: clean.to_string(),
                                                ts_ms: last_ts_ms,
                                            });
                                        }
                                    }
                                }
                                Some("tool_use") => {
                                    let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                                    let input = block.get("input").unwrap_or(&Value::Null);
                                    if matches!(name, "TodoWrite" | "CreatePlan")
                                        && let Some(todos) = input.get("todos").and_then(Value::as_array)
                                    {
                                        let items = format_todos(todos);
                                        if !items.is_empty() {
                                            entries.push(Entry::Plan { turn, items, ts_ms: last_ts_ms });
                                            continue;
                                        }
                                    }
                                    tool_seq += 1;
                                    let id = block
                                        .get("id")
                                        .and_then(Value::as_str)
                                        .filter(|s| !s.is_empty())
                                        .map(str::to_string)
                                        .unwrap_or_else(|| format!("cursor-tool-{turn}-{tool_seq}"));
                                    let (tool_kind, title) = summarize_cursor_tool(name, input);
                                    let detail = cursor_tool_detail(name, input);
                                    let idx = entries.len();
                                    entries.push(Entry::ToolCall {
                                        turn,
                                        id,
                                        title,
                                        tool_kind,
                                        status: "in_progress".to_string(),
                                        detail,
                                        attachments: vec![],
                                        ts_ms: last_ts_ms,
                                    });
                                    open_tools.push(idx);
                                }
                                Some("text") => {
                                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                                        let clean = text.trim();
                                        if !clean.is_empty() {
                                            entries.push(Entry::Assistant {
                                                turn,
                                                text: clean.to_string(),
                                                attachments: vec![],
                                                ts_ms: last_ts_ms,
                                            });
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    if !entries.is_empty() || !meta.is_empty() {
        if meta.session_id.is_none()
            && let Some(stem) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        {
            meta.session_id = Some(stem.to_string());
        }
        meta.provider.get_or_insert_with(|| "cursor".to_string());
    }
    (entries, meta)
}

/// Parses RFC3339 / ISO-8601 timestamps or numeric epoch milliseconds into UTC ms.
pub(crate) fn parse_ts_ms(raw: &str) -> u64 {
    let s = raw.trim();
    if s.is_empty() {
        return 0;
    }
    if s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse::<u64>().unwrap_or(0);
    }
    let bytes = s.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || (bytes[10] != b'T' && bytes[10] != b' ') {
        return 0;
    }
    let Some(year) = s[0..4].parse::<i64>().ok() else { return 0 };
    let Some(month) = s[5..7].parse::<u32>().ok() else { return 0 };
    let Some(day) = s[8..10].parse::<u32>().ok() else { return 0 };
    if bytes[13] != b':' || bytes[16] != b':' {
        return 0;
    }
    let Some(hour) = s[11..13].parse::<i64>().ok() else { return 0 };
    let Some(min) = s[14..16].parse::<i64>().ok() else { return 0 };
    let Some(sec) = s[17..19].parse::<i64>().ok() else { return 0 };

    let mut pos = 19;
    let mut ms: i64 = 0;
    if pos < bytes.len() && bytes[pos] == b'.' {
        pos += 1;
        let frac_start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
        let frac = &s[frac_start..pos];
        if !frac.is_empty() {
            let mut digits = [b'0'; 3];
            for (i, b) in frac.bytes().take(3).enumerate() {
                digits[i] = b;
            }
            ms = std::str::from_utf8(&digits).ok().and_then(|d| d.parse::<i64>().ok()).unwrap_or(0);
        }
    }

    let mut tz_offset_secs: i64 = 0;
    if pos < bytes.len() {
        if bytes[pos] == b'Z' || bytes[pos] == b'z' {
            tz_offset_secs = 0;
        } else if (bytes[pos] == b'+' || bytes[pos] == b'-') && pos + 3 <= bytes.len() {
            let sign = if bytes[pos] == b'+' { 1i64 } else { -1i64 };
            let tz_rest = &s[pos + 1..];
            let (hh, mm) = if let Some((h, m)) = tz_rest.split_once(':') {
                (h.parse::<i64>().ok().unwrap_or(0), m.get(..2).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0))
            } else if tz_rest.len() >= 4 {
                (tz_rest[..2].parse::<i64>().ok().unwrap_or(0), tz_rest[2..4].parse::<i64>().ok().unwrap_or(0))
            } else if tz_rest.len() >= 2 {
                (tz_rest[..2].parse::<i64>().ok().unwrap_or(0), 0)
            } else {
                (0, 0)
            };
            tz_offset_secs = sign * (hh * 3600 + mm * 60);
        }
    }

    let Some(days) = days_from_civil(year, month, day) else {
        return 0;
    };
    let secs = days * 86_400 + hour * 3600 + min * 60 + sec - tz_offset_secs;
    if secs < 0 { 0 } else { (secs as u64) * 1000 + (ms as u64) }
}

fn days_from_civil(y: i64, m: u32, d: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(m);
    let d = i64::from(d);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = crate::test_tmp().join(format!("herdr2-pty-transcript-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_iso_timestamps() {
        assert_eq!(parse_ts_ms("2026-11-17T10:00:00.250Z"), 1_794_909_600_250);
        assert_eq!(parse_ts_ms("2026-11-17T18:00:00+08:00"), 1_794_909_600_000);
        assert_eq!(parse_ts_ms("1700000000123"), 1_700_000_000_123);
    }

    #[test]
    fn extracts_claude_code_jsonl_session() {
        let root = tmp_dir("claude");
        let project = "/tmp/my/repo";
        let proj_dir = root.join("projects").join(claude_slug(project));
        fs::create_dir_all(&proj_dir).unwrap();
        let session = proj_dir.join("11111111-2222-3333-4444-555555555555.jsonl");
        let lines = [
            serde_json::json!({
                "type": "user",
                "cwd": project,
                "permissionMode": "bypassPermissions",
                "timestamp": "2026-11-17T10:00:00.000Z",
                "message": { "role": "user", "content": "<local-command-caveat>ignore</local-command-caveat>" }
            }),
            serde_json::json!({
                "type": "user",
                "cwd": project,
                "timestamp": "2026-11-17T10:00:01.000Z",
                "message": { "role": "user", "content": "<system-reminder>ctx</system-reminder>Inspect main.rs" }
            }),
            serde_json::json!({
                "type": "assistant",
                "cwd": project,
                "timestamp": "2026-11-17T10:00:02.000Z",
                "message": {
                    "role": "assistant",
                    "model": "claude-sonnet-4-5",
                    "usage": { "input_tokens": 1200, "cache_read_input_tokens": 26800 },
                    "content": [
                        { "type": "thinking", "thinking": "Let me read the file." },
                        { "type": "tool_use", "id": "tu-1", "name": "Read", "input": { "file_path": "src/main.rs" } },
                        { "type": "tool_use", "id": "tu-2", "name": "TodoWrite", "input": { "todos": [{"content": "Check code", "status": "completed"}] } }
                    ]
                }
            }),
            serde_json::json!({
                "type": "user",
                "cwd": project,
                "timestamp": "2026-11-17T10:00:03.000Z",
                "message": {
                    "role": "user",
                    "content": [
                        { "type": "tool_result", "tool_use_id": "tu-1", "is_error": false, "content": "fn main() {}" }
                    ]
                }
            }),
            serde_json::json!({
                "type": "assistant",
                "cwd": project,
                "timestamp": "2026-11-17T10:00:04.000Z",
                "message": {
                    "role": "assistant",
                    "model": "claude-sonnet-4-5",
                    "content": [{ "type": "text", "text": "All good." }]
                }
            }),
        ];
        fs::write(&session, lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();

        let snap = PtySnapshot {
            cwd: project.to_string(),
            started_ms: parse_ts_ms("2026-11-17T10:00:00.000Z"),
            agent_hint: Some("claude".into()),
            sent_prompts: Vec::new(),
            turn: 1,
            resumed: false,
            fg_pid: None,
            ..Default::default()
        };
        let (entries, meta) = extract_transcript_with_home(&snap, &root, Some(&root), None);
        assert_eq!(entries.len(), 5);
        assert!(matches!(&entries[0], Entry::User { text, turn: 1, .. } if text == "Inspect main.rs"));
        assert!(matches!(&entries[1], Entry::Thought { text, .. } if text == "Let me read the file."));
        assert!(
            matches!(&entries[2], Entry::ToolCall { title, tool_kind, status, detail, .. } if title == "Read src/main.rs" && tool_kind.as_deref() == Some("read") && status == "completed" && detail.as_deref() == Some("fn main() {}"))
        );
        assert!(matches!(&entries[3], Entry::Plan { items, .. } if items == &vec!["[x] Check code".to_string()]));
        assert!(matches!(&entries[4], Entry::Assistant { text, .. } if text == "All good."));
        let meta = meta.expect("expected Claude session metadata");
        assert_eq!(meta.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(meta.mode.as_deref(), Some("dangerously-skip-permissions"));
        assert_eq!(meta.context_usage, Some(AcpContextUsage { used: 28000, size: 200_000 }));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn claude_jsonl_detects_1m_and_500k_context_window_from_betas_and_cost_state() {
        assert_eq!(parse_context_window_from_model_name("group/deepseek-v4.1-flash[1m]"), Some(1_000_000));
        assert_eq!(parse_context_window_from_model_name("cursor/grok-4.7@500k"), Some(500_000));
        assert_eq!(parse_context_window_from_model_name("claude-sonnet-4-5"), None);

        let root = tmp_dir("claude-1m");
        let file = root.join("sess.jsonl");
        let lines = [
            serde_json::json!({
                "type": "api-request",
                "params": { "model": "group/deepseek-v4.1-flash", "betas": ["claude-code-20250219", "context-1m-2025-08-07"] }
            }),
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "model": "group/deepseek-v4.1-flash",
                    "usage": { "input_tokens": 109527, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0 },
                    "content": [{ "type": "text", "text": "ok" }]
                }
            }),
        ];
        fs::write(&file, lines.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n") + "\n").unwrap();
        let (_, meta) = parse_claude_jsonl(&file);
        assert_eq!(meta.context_usage, Some(AcpContextUsage { used: 109_527, size: 1_000_000 }));

        // Also verify `cost-state` modelUsage `@500k` suffix overrides default 200k.
        let file2 = root.join("sess500k.jsonl");
        let lines2 = [
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "model": "cursor/grok-4.7",
                    "usage": { "input_tokens": 50000 },
                    "content": [{ "type": "text", "text": "hi" }]
                }
            }),
            serde_json::json!({
                "type": "cost-state",
                "modelUsage": { "cursor/grok-4.7@500k": { "inputTokens": 50000 } }
            }),
        ];
        fs::write(&file2, lines2.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n") + "\n").unwrap();
        let (_, meta2) = parse_claude_jsonl(&file2);
        assert_eq!(meta2.context_usage, Some(AcpContextUsage { used: 50_000, size: 500_000 }));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn claude_jsonl_updates_model_and_context_window_after_slash_model_switch() {
        let settings = serde_json::json!({
            "modelPicker": {
                "options": [
                    { "model": "group/deepseek-v4.1-flash[1m]", "label": "deepseek-v4.1-flash · routing group" },
                    { "model": "codex/gpt-6.1-sol", "label": "GPT-6.1-Sol · Codex" },
                    { "model": "cursor/grok-4.7@500k", "label": "Grok 4.7 @ 500k · Cursor" }
                ]
            }
        });

        let root = tmp_dir("claude-model-switch");
        let file = root.join("sess.jsonl");

        // Turn 1 used a 1M model, then user switched via `/model` to Grok 4.7 @ 500k.
        let lines_500k = [
            serde_json::json!({
                "type": "api-request",
                "params": { "model": "group/deepseek-v4.1-flash", "betas": ["context-1m-2025-08-07"] }
            }),
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "model": "group/deepseek-v4.1-flash",
                    "usage": { "input_tokens": 33307 },
                    "content": [{ "type": "text", "text": "hello" }]
                }
            }),
            serde_json::json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": "<local-command-stdout>Set model to `Grok 4.7 @ 500k · Cursor` and saved as your default for new sessions</local-command-stdout>"
                }
            }),
        ];
        fs::write(&file, lines_500k.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n") + "\n").unwrap();
        let (entries, meta) = parse_claude_jsonl_with_settings(&file, Some(&settings));
        assert_eq!(entries.len(), 1, "local-command-stdout must not appear as a user chat bubble");
        assert_eq!(meta.model.as_deref(), Some("cursor/grok-4.7@500k"));
        assert_eq!(meta.context_usage, Some(AcpContextUsage { used: 33_307, size: 500_000 }));

        // Switching next to a 200k default model must drop the stale 1M/500k window.
        let mut lines_200k = lines_500k.to_vec();
        lines_200k.push(serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": "<local-command-stdout>Set model to `GPT-6.1-Sol · Codex` and saved as your default for new sessions</local-command-stdout>"
            }
        }));
        fs::write(&file, lines_200k.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n") + "\n").unwrap();
        let (_, meta2) = parse_claude_jsonl_with_settings(&file, Some(&settings));
        assert_eq!(meta2.model.as_deref(), Some("codex/gpt-6.1-sol"));
        assert_eq!(meta2.context_usage, Some(AcpContextUsage { used: 33_307, size: 200_000 }));

        // Official Claude Code format without backticks works too.
        assert_eq!(
            parse_claude_model_switch_stdout(
                "<local-command-stdout>Set model to Opus (claude-opus-4-6[1m]) for this session</local-command-stdout>"
            ),
            Some("Opus (claude-opus-4-6[1m])".to_string())
        );
        assert_eq!(
            resolve_claude_model_spec("Opus (claude-opus-4-6[1m])", None),
            ("claude-opus-4-6[1m]".to_string(), Some(1_000_000))
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn extracts_codex_rollout_jsonl_session() {
        let root = tmp_dir("codex");
        let project = "/tmp/codex/repo";
        let day_dir = root.join("sessions/2026/11/17");
        fs::create_dir_all(&day_dir).unwrap();
        let session = day_dir.join("rollout-2026-11-17T10-00-00-abc.jsonl");
        let lines = [
            serde_json::json!({
                "timestamp": "2026-11-17T10:00:00.000Z",
                "type": "session_meta",
                "payload": { "cwd": project, "timestamp": "2026-11-17T10:00:00.000Z" }
            }),
            serde_json::json!({
                "timestamp": "2026-11-17T10:00:00.500Z",
                "type": "turn_context",
                "payload": { "model": "gpt-5.4", "effort": "high", "approval_policy": "never" }
            }),
            serde_json::json!({
                "timestamp": "2026-11-17T10:00:01.000Z",
                "type": "event_msg",
                "payload": { "type": "user_message", "message": "Run tests" }
            }),
            serde_json::json!({
                "timestamp": "2026-11-17T10:00:02.000Z",
                "type": "response_item",
                "payload": { "type": "reasoning", "summary": [{ "text": "Checking cargo test" }] }
            }),
            serde_json::json!({
                "timestamp": "2026-11-17T10:00:03.000Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "call_id": "call-1",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"cargo test\"}"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-11-17T10:00:04.000Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "call-1",
                    "output": "Exit code: 1\nfailed"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-11-17T10:00:04.500Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "last_token_usage": { "total_tokens": 34000 },
                        "model_context_window": 272000
                    }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-11-17T10:00:05.000Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "Tests failed." }]
                }
            }),
        ];
        fs::write(&session, lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();

        let snap = PtySnapshot {
            cwd: project.to_string(),
            started_ms: parse_ts_ms("2026-11-17T10:00:00.000Z"),
            agent_hint: Some("codex".into()),
            sent_prompts: Vec::new(),
            turn: 1,
            resumed: false,
            fg_pid: None,
            ..Default::default()
        };
        let (entries, meta) = extract_transcript_with_home(&snap, &root, None, Some(&root));
        assert_eq!(entries.len(), 4);
        assert!(matches!(&entries[0], Entry::User { text, .. } if text == "Run tests"));
        assert!(matches!(&entries[1], Entry::Thought { text, .. } if text == "Checking cargo test"));
        assert!(
            matches!(&entries[2], Entry::ToolCall { title, tool_kind, status, detail, .. } if title == "cargo test" && tool_kind.as_deref() == Some("execute") && status == "failed" && detail.as_deref() == Some("Exit code: 1\nfailed"))
        );
        assert!(matches!(&entries[3], Entry::Assistant { text, .. } if text == "Tests failed."));
        let meta = meta.expect("expected Codex session metadata");
        assert_eq!(meta.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(meta.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(meta.mode.as_deref(), Some("never"));
        assert_eq!(meta.context_usage, Some(AcpContextUsage { used: 34000, size: 272000 }));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn extracts_qwen_legacy_session_json() {
        let root = tmp_dir("qwen-legacy");
        let project = "/tmp/qwen-legacy/proj";
        let chats = root.join(".qwen/tmp/hash123/chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(root.join(".qwen/tmp/hash123/.project_root"), project).unwrap();
        let session = chats.join("session-2026-11-17T10-00-00.json");
        let payload = serde_json::json!({
            "startTime": "2026-11-17T10:00:00.000Z",
            "messages": [
                { "type": "user", "timestamp": "2026-11-17T10:00:01.000Z", "content": "Explain lib.rs" },
                {
                    "type": "assistant",
                    "timestamp": "2026-11-17T10:00:02.000Z",
                    "thoughts": [{ "subject": "Reading", "description": "Check exports" }],
                    "toolCalls": [{ "id": "g1", "name": "read_file", "args": { "path": "src/lib.rs" }, "status": "success" }],
                    "content": "It exports modules."
                }
            ]
        });
        fs::write(&session, payload.to_string()).unwrap();

        let snap = PtySnapshot {
            cwd: project.to_string(),
            started_ms: parse_ts_ms("2026-11-17T10:00:00.000Z"),
            agent_hint: Some("qwen".into()),
            sent_prompts: Vec::new(),
            turn: 1,
            resumed: false,
            fg_pid: None,
            ..Default::default()
        };
        let entries = extract_entries_with_home(&snap, &root, None, None);
        assert_eq!(entries.len(), 4);
        assert!(matches!(&entries[0], Entry::User { text, .. } if text == "Explain lib.rs"));
        assert!(
            matches!(&entries[1], Entry::Thought { text, .. } if text.contains("Reading") && text.contains("Check exports"))
        );
        assert!(
            matches!(&entries[2], Entry::ToolCall { status, tool_kind, .. } if status == "completed" && tool_kind.as_deref() == Some("read"))
        );
        assert!(matches!(&entries[3], Entry::Assistant { text, .. } if text == "It exports modules."));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn extracts_pi_jsonl_session() {
        let root = tmp_dir("pi");
        let project = "/tmp/pi/proj";
        let sess_dir = root.join(".pi/agent/sessions/--tmp-pi-proj--");
        fs::create_dir_all(&sess_dir).unwrap();
        let session = sess_dir.join("2026-11-17.jsonl");
        let lines = [
            serde_json::json!({ "type": "session", "cwd": project, "timestamp": "2026-11-17T10:00:00.000Z" }),
            serde_json::json!({ "type": "message", "timestamp": "2026-11-17T10:00:01.000Z", "message": { "role": "user", "content": [{ "type": "text", "text": "Hello Pi" }] } }),
            serde_json::json!({
                "type": "message",
                "timestamp": "2026-11-17T10:00:02.000Z",
                "message": {
                    "role": "assistant",
                    "content": [
                        { "type": "thinking", "thinking": "Thinking..." },
                        { "type": "toolCall", "id": "p1", "name": "bash", "arguments": { "command": "ls" } },
                        { "type": "text", "text": "Done!" }
                    ]
                }
            }),
            serde_json::json!({
                "type": "message",
                "timestamp": "2026-11-17T10:00:03.000Z",
                "message": { "role": "toolResult", "toolCallId": "p1", "isError": false }
            }),
        ];
        fs::write(&session, lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();

        let snap = PtySnapshot {
            cwd: project.to_string(),
            started_ms: parse_ts_ms("2026-11-17T10:00:00.000Z"),
            agent_hint: Some("pi".into()),
            sent_prompts: Vec::new(),
            turn: 1,
            resumed: false,
            fg_pid: None,
            ..Default::default()
        };
        let entries = extract_entries_with_home(&snap, &root, None, None);
        assert_eq!(entries.len(), 4);
        assert!(matches!(&entries[0], Entry::User { text, .. } if text == "Hello Pi"));
        assert!(matches!(&entries[1], Entry::Thought { text, .. } if text == "Thinking..."));
        assert!(
            matches!(&entries[2], Entry::ToolCall { status, tool_kind, .. } if status == "completed" && tool_kind.as_deref() == Some("execute"))
        );
        assert!(matches!(&entries[3], Entry::Assistant { text, .. } if text == "Done!"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn plain_shell_does_not_hijack_provider_sessions() {
        let root = tmp_dir("plain-shell");
        let project = "/tmp/shared/repo";
        let proj_dir = root.join("projects").join(claude_slug(project));
        fs::create_dir_all(&proj_dir).unwrap();
        let session = proj_dir.join("sess-1.jsonl");
        let lines = [
            serde_json::json!({
                "type": "user",
                "cwd": project,
                "timestamp": "2026-11-17T10:00:01.000Z",
                "message": { "role": "user", "content": "Secret prompt" }
            }),
            serde_json::json!({
                "type": "assistant",
                "cwd": project,
                "timestamp": "2026-11-17T10:00:02.000Z",
                "message": { "role": "assistant", "content": [{ "type": "text", "text": "Reply" }] }
            }),
        ];
        fs::write(&session, lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();

        let snap = PtySnapshot {
            cwd: project.to_string(),
            started_ms: parse_ts_ms("2026-11-17T10:00:00.000Z"),
            agent_hint: None,
            sent_prompts: Vec::new(),
            turn: 1,
            resumed: false,
            fg_pid: None,
            ..Default::default()
        };
        let entries = extract_entries_with_home(&snap, &root, Some(&root), None);
        assert!(entries.is_empty(), "plain shell must not hijack .claude sessions");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn fresh_claude_ignores_older_session_unless_resumed() {
        let root = tmp_dir("fresh-claude");
        let project = "/tmp/fresh/repo";
        let proj_dir = root.join("projects").join(claude_slug(project));
        fs::create_dir_all(&proj_dir).unwrap();
        let session = proj_dir.join("older-sess.jsonl");
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        let old_first_ms = now_ms.saturating_sub(120_000);
        let lines = [
            serde_json::json!({
                "type": "user",
                "cwd": project,
                "timestamp": old_first_ms.to_string(),
                "message": { "role": "user", "content": "Old task" }
            }),
            serde_json::json!({
                "type": "assistant",
                "cwd": project,
                "timestamp": (old_first_ms + 1000).to_string(),
                "message": { "role": "assistant", "content": [{ "type": "text", "text": "Old reply" }] }
            }),
        ];
        fs::write(&session, lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();

        let fresh = PtySnapshot {
            cwd: project.to_string(),
            started_ms: now_ms.saturating_sub(500),
            agent_hint: Some("claude".into()),
            sent_prompts: Vec::new(),
            turn: 0,
            resumed: false,
            fg_pid: None,
            ..Default::default()
        };
        assert!(extract_entries_with_home(&fresh, &root, Some(&root), None).is_empty());

        let resumed = PtySnapshot { resumed: true, ..fresh };
        let entries = extract_entries_with_home(&resumed, &root, Some(&root), None);
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[0], Entry::User { text, .. } if text == "Old task"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn fresh_cli_ignores_timestampless_old_session_and_missing_pinned_session() {
        let root = tmp_dir("fresh-cli-no-hijack");
        let project = "/tmp/fresh-no-hijack/repo";
        let proj_dir = root.join("projects").join(claude_slug(project));
        fs::create_dir_all(&proj_dir).unwrap();
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;

        // A session JSONL with NO timestamp field in its header (`first_ts_ms == 0`) must never
        // be stolen by a brand-new 0-turn CLI agent.
        let no_ts_session = proj_dir.join("old-no-ts.jsonl");
        fs::write(
            &no_ts_session,
            serde_json::json!({
                "type": "user",
                "cwd": project,
                "message": { "role": "user", "content": "Previous agent task" }
            })
            .to_string()
                + "\n",
        )
        .unwrap();

        let fresh = PtySnapshot {
            cwd: project.to_string(),
            started_ms: now_ms,
            agent_hint: Some("claude".into()),
            turn: 0,
            resumed: false,
            fg_pid: None,
            ..Default::default()
        };
        assert!(
            extract_entries_with_home(&fresh, &root, Some(&root), None).is_empty(),
            "fresh 0-turn CLI must not hijack timestamp-less session"
        );

        // A session with an explicit pinned_session_id whose file hasn't been written yet must not
        // fall back to a different session file.
        let other_recent = proj_dir.join("other-recent.jsonl");
        fs::write(
            &other_recent,
            serde_json::json!({
                "type": "user",
                "cwd": project,
                "timestamp": (now_ms + 500).to_string(),
                "message": { "role": "user", "content": "Concurrent other agent" }
            })
            .to_string()
                + "\n",
        )
        .unwrap();
        let pinned_unwritten = PtySnapshot {
            pinned_session_id: Some("brand-new-pinned-uuid".into()),
            ..fresh
        };
        assert!(
            extract_entries_with_home(&pinned_unwritten, &root, Some(&root), None).is_empty(),
            "missing pinned session must not fall back to another session"
        );

        assert!(is_valid_agy_cid("a6ebbc27-5c43-4353-bb78-651a0223bf01"));
        assert!(!is_valid_agy_cid("transcript_full"));
        assert!(!is_valid_agy_cid("transcript"));
        assert!(!is_valid_agy_cid("logs"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn claude_discovery_excludes_acp_side_chat_sessions_and_honors_pinned_sid() {
        let root = tmp_dir("claude-exclude-acp");
        let project = "/tmp/shared/repo";
        let slug_dir = root.join("projects").join(claude_slug(project));
        fs::create_dir_all(&slug_dir).unwrap();

        let now_ms = std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;

        let main_file = slug_dir.join("main-sess-111.jsonl");
        let acp_file = slug_dir.join("acp-side-222.jsonl");
        fs::write(
            &main_file,
            serde_json::json!({
                "type": "user", "cwd": project, "sessionId": "main-sess-111",
                "message": { "role": "user", "content": "Real main task" }
            })
            .to_string()
                + "\n",
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(
            &acp_file,
            serde_json::json!({
                "type": "user", "cwd": project, "sessionId": "acp-side-222",
                "message": { "role": "user", "content": "Side chat message" }
            })
            .to_string()
                + "\n",
        )
        .unwrap();

        let mut excluded = HashSet::new();
        excluded.insert("acp-side-222".to_string());
        let snap = PtySnapshot {
            cwd: project.to_string(),
            started_ms: now_ms.saturating_sub(5_000),
            agent_hint: Some("claude".into()),
            turn: 1,
            excluded_session_ids: excluded,
            ..Default::default()
        };
        let (entries, meta) = extract_transcript_with_home(&snap, &root, Some(&root), None);
        assert_eq!(meta.and_then(|m| m.session_id).as_deref(), Some("main-sess-111"));
        assert!(matches!(&entries[0], Entry::User { text, .. } if text == "Real main task"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn agy_prefers_transcript_full_and_strips_metadata_and_plan_review() {
        let root = tmp_dir("agy-full");
        let logs = root.join(".gemini/antigravity-cli/brain/brain-1/.system_generated/logs");
        fs::create_dir_all(&logs).unwrap();
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        let ts = now_ms + 1000;

        let truncated_lines = [
            serde_json::json!({
                "type": "USER_INPUT",
                "created_at": ts.to_string(),
                "content": "<USER_REQUEST>\nCheck\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nclock\n</ADDITIONAL_METADATA>"
            }),
            serde_json::json!({
                "type": "PLANNER_RESPONSE",
                "created_at": (ts + 1000).to_string(),
                "content": "truncated..."
            }),
        ];
        fs::write(
            logs.join("transcript.jsonl"),
            truncated_lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n"),
        )
        .unwrap();

        let full_lines = [
            serde_json::json!({
                "type": "USER_INPUT",
                "created_at": ts.to_string(),
                "content": "<USER_REQUEST>\nSummarize rankings\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nlocal time\n</ADDITIONAL_METADATA>"
            }),
            serde_json::json!({
                "type": "USER_INPUT",
                "created_at": (ts + 500).to_string(),
                "content": "[plan review] ignore me"
            }),
            serde_json::json!({
                "type": "PLANNER_RESPONSE",
                "created_at": (ts + 1000).to_string(),
                "tool_calls": [{ "name": "run_command", "args": { "CommandLine": "h2 ls" } }],
                "content": "Full untruncated reply\n\n| A | B |\n|---|---|\n| 1 | 2 |"
            }),
        ];
        fs::write(
            logs.join("transcript_full.jsonl"),
            full_lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n"),
        )
        .unwrap();

        let snap = PtySnapshot {
            cwd: "/tmp/repo".to_string(),
            started_ms: now_ms,
            agent_hint: Some("agy".into()),
            sent_prompts: Vec::new(),
            turn: 1,
            resumed: false,
            fg_pid: None,
            ..Default::default()
        };
        let entries = extract_entries_with_home(&snap, &root, None, None);
        assert_eq!(entries.len(), 3);
        assert!(matches!(&entries[0], Entry::User { text, .. } if text == "Summarize rankings"));
        assert!(
            matches!(&entries[1], Entry::ToolCall { title, tool_kind, .. } if title == "h2 ls" && tool_kind.as_deref() == Some("execute"))
        );
        assert!(matches!(&entries[2], Entry::Assistant { text, .. } if text.contains("| A | B |")));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn optimistic_prompt_dedup_works() {
        let mut entries =
            vec![Entry::User { turn: 1, from: None, text: "first prompt".into(), attachments: vec![], ts_ms: 1000 }];
        let prompts = vec![
            (1, 1050, "first prompt".into()),
            (1, 900, "older prompt".into()),
            (2, 2000, "second prompt".into()),
            (2, 2010, "  second prompt  ".into()),
        ];
        merge_sent_prompts(&mut entries, &prompts);
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[1], Entry::User { text, ts_ms: 2000, .. } if text == "second prompt"));
    }

    #[test]
    fn pi_and_agy_populate_diff_details_and_plan_entries() {
        let root = tmp_dir("pi-agy-diff");
        let pi_path = root.join("pi.jsonl");
        let pi_lines = [
            serde_json::json!({
                "type": "message",
                "thinkingLevel": "high",
                "timestamp": "2026-11-17T10:00:01.000Z",
                "message": { "role": "user", "content": [{ "type": "text", "text": "Fix foo" }] }
            }),
            serde_json::json!({
                "type": "message",
                "timestamp": "2026-11-17T10:00:02.000Z",
                "message": {
                    "role": "assistant",
                    "model": "gpt-5.4",
                    "content": [
                        {
                            "type": "toolCall",
                            "id": "p-edit",
                            "name": "Edit",
                            "arguments": { "path": "src/foo.rs", "oldText": "fn a() {}", "newText": "fn b() {}" }
                        }
                    ]
                }
            }),
            serde_json::json!({
                "type": "message",
                "timestamp": "2026-11-17T10:00:03.000Z",
                "message": { "role": "toolResult", "toolCallId": "p-edit", "isError": false, "content": "Applied" }
            }),
        ];
        fs::write(&pi_path, pi_lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
        let (pi_entries, pi_meta) = parse_pi_jsonl(&pi_path);
        assert_eq!(pi_meta.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(pi_meta.reasoning_effort.as_deref(), Some("high"));
        assert!(matches!(
            &pi_entries[1],
            Entry::ToolCall { tool_kind, detail: Some(d), .. }
            if tool_kind.as_deref() == Some("edit") && d.contains("--- src/foo.rs") && d.contains("- fn a() {}") && d.contains("+ fn b() {}")
        ));

        let agy_path = root.join("agy.jsonl");
        let agy_lines = [
            serde_json::json!({
                "type": "USER_INPUT",
                "created_at": "1700000000000",
                "content": "<USER_REQUEST>\nUpdate bar\n</USER_REQUEST>"
            }),
            serde_json::json!({
                "type": "PLANNER_RESPONSE",
                "created_at": "1700000001000",
                "model": "claude-opus-4",
                "tool_calls": [
                    {
                        "id": "a-edit",
                        "name": "edit_file",
                        "args": { "Path": "src/bar.rs", "Target": "old", "Replacement": "new" }
                    },
                    {
                        "id": "a-plan",
                        "name": "exit_plan_mode",
                        "args": { "plan": "- Step one\n- Step two" }
                    }
                ],
                "content": "Updated bar."
            }),
        ];
        fs::write(&agy_path, agy_lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
        let (agy_entries, agy_meta) = parse_agy_jsonl(&agy_path);
        assert_eq!(agy_meta.model.as_deref(), Some("claude-opus-4"));
        assert!(matches!(
            &agy_entries[1],
            Entry::ToolCall { tool_kind, detail: Some(d), .. }
            if tool_kind.as_deref() == Some("edit") && d.contains("--- src/bar.rs") && d.contains("- old") && d.contains("+ new")
        ));
        assert!(
            matches!(&agy_entries[2], Entry::Plan { items, .. } if items == &vec!["Step one".to_string(), "Step two".to_string()])
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn cursor_hint_and_structured_transcript_discovery_and_parsing_work() {
        assert_eq!(
            hint_from_words(
                [
                    "node.exe",
                    r"D:\Users\zhihao.zou\AppData\Local\cursor-agent\versions\2026.10.22-27dcf5f\node.exe",
                    r"D:\Users\zhihao.zou\AppData\Local\cursor-agent\versions\2026.10.22-27dcf5f\index.js",
                    "--yolo",
                    "--trust",
                ]
                .into_iter()
            ),
            Some("cursor")
        );
        assert_eq!(
            hint_from_words(
                [
                    "powershell.exe",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-NoExit",
                    "-Command",
                    "& 'cursor-agent' '--yolo' '--trust'",
                ]
                .into_iter()
            ),
            Some("cursor")
        );
        assert_eq!(cursor_project_slug(r"D:\Users\zhihao.zou"), "D-Users-zhihao-zou");
        assert_eq!(cursor_project_slug("/home/u/Work/co_agent"), "home-u-Work-co-agent");
        assert!(parse_cursor_human_ts("Thursday, Nov 26, 2026, 11:19 AM (UTC+8)").is_some());

        let root = tmp_dir("cursor-transcript");
        let sid = "c28c6c39-c785-4904-b150-e5b421191e0a";
        let tx_dir = root.join(".cursor/projects/D-Users-zhihao-zou/agent-transcripts").join(sid);
        fs::create_dir_all(&tx_dir).unwrap();
        let tx_path = tx_dir.join(format!("{sid}.jsonl"));
        let lines = [
            serde_json::json!({
                "role": "user",
                "message": {
                    "content": [{
                        "type": "text",
                        "text": "<timestamp>Thursday, Nov 26, 2026, 11:19 AM (UTC+8)</timestamp>\n<user_query>\n检查构建脚本并修改端口\n</user_query>"
                    }]
                }
            }),
            serde_json::json!({
                "role": "assistant",
                "message": {
                    "content": [
                        { "type": "tool_use", "name": "Read", "input": { "path": "src/main.rs" } },
                        { "type": "tool_use", "name": "StrReplace", "input": { "path": "src/main.rs", "old_string": "8080", "new_string": "9090" } },
                        { "type": "tool_use", "name": "Shell", "input": { "command": "cargo test", "description": "Run tests" } }
                    ]
                }
            }),
            serde_json::json!({
                "role": "assistant",
                "message": {
                    "content": [{ "type": "text", "text": "已将端口更新为 9090 并通过测试。" }]
                }
            }),
            serde_json::json!({ "type": "turn_ended", "status": "success" }),
        ];
        fs::write(&tx_path, lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();

        let now_ms = mtime_ms(&tx_path);
        let snap = PtySnapshot {
            cwd: r"d:\Users\zhihao.zou".into(),
            started_ms: now_ms.saturating_sub(1_000),
            agent_hint: Some("cursor".into()),
            sent_prompts: Vec::new(),
            turn: 0,
            resumed: false,
            fg_pid: None,
            ..Default::default()
        };
        let (entries, meta) = extract_transcript_with_home(&snap, &root, None, None);
        let meta = meta.expect("cursor session meta");
        assert_eq!(meta.provider.as_deref(), Some("cursor"));
        assert_eq!(meta.session_id.as_deref(), Some(sid));
        assert_eq!(entries.len(), 5);
        assert!(matches!(&entries[0], Entry::User { turn: 1, text, .. } if text == "检查构建脚本并修改端口"));
        assert!(matches!(
            &entries[1],
            Entry::ToolCall { title, tool_kind, status, .. }
            if title == "Read src/main.rs" && tool_kind.as_deref() == Some("read") && status == "completed"
        ));
        assert!(matches!(
            &entries[2],
            Entry::ToolCall { title, tool_kind, status, detail: Some(d), .. }
            if title == "StrReplace src/main.rs" && tool_kind.as_deref() == Some("edit") && status == "completed" && d.contains("- 8080") && d.contains("+ 9090")
        ));
        assert!(matches!(
            &entries[3],
            Entry::ToolCall { title, tool_kind, status, .. }
            if title == "cargo test" && tool_kind.as_deref() == Some("execute") && status == "completed"
        ));
        assert!(matches!(&entries[4], Entry::Assistant { turn: 1, text, .. } if text == "已将端口更新为 9090 并通过测试。"));
        fs::remove_dir_all(root).ok();
    }
}
