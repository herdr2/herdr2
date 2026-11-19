//! Structured transcript extraction for terminal-hosted coding agents (Claude Code,
//! Codex CLI, Gemini CLI, Qwen Code, Pi / oh-my-pi, and Antigravity CLI) plus
//! optimistic merging of prompts sent via `h2 send`.

use crate::acp::proto::AcpContextUsage;
use crate::api::ReadSessionMeta;
use crate::drove::crypto::sha256_hex;
use crate::terminals::Terminal;
use crate::transcript::{Entry, cap_tool_detail};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 2000;
const MAX_BYTES: usize = 2 << 20;
const MTIME_SLACK_MS: u64 = 15_000;
const START_SLACK_MS: u64 = 10_000;

#[derive(Clone, Debug)]
pub struct PtySnapshot {
    pub cwd: String,
    pub started_ms: u64,
    pub agent_hint: Option<String>,
    pub sent_prompts: Vec<(u64, u64, String)>,
    pub turn: u64,
    pub resumed: bool,
    pub fg_pid: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    Claude,
    Codex,
    Gemini,
    Qwen,
    Pi,
    Agy,
}

impl Provider {
    fn as_str(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Gemini => "gemini",
            Provider::Qwen => "qwen",
            Provider::Pi => "pi",
            Provider::Agy => "agy",
        }
    }
}

fn agy_cid_from_path(path: &Path) -> Option<String> {
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
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn codex_sid_from_header(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    for line in BufReader::new(file).lines().take(12).flatten() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) == Some("session_meta")
            && let Some(sid) = v
                .pointer("/payload/id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
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

pub(crate) fn argv_has_resume(argv: &[String]) -> bool {
    let has_flag = argv.iter().any(|arg| {
        matches!(arg.as_str(), "--resume" | "-r" | "--continue" | "-c")
            || arg.starts_with("--resume=")
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
    let resumed = argv_has_resume(
        t.foreground
            .as_ref()
            .map(|f| f.argv.as_slice())
            .unwrap_or(&[]),
    ) || argv_has_resume(&t.argv);
    PtySnapshot {
        cwd,
        started_ms,
        agent_hint,
        sent_prompts: sent_prompts.cloned().unwrap_or_default(),
        turn,
        resumed,
        fg_pid,
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
    let min_mtime = if snap.resumed {
        0
    } else {
        snap.started_ms.saturating_sub(MTIME_SLACK_MS)
    };
    let fg_started_ms = snap.fg_pid.and_then(proc_started_ms);
    let providers = ordered_providers(snap.agent_hint.as_deref());

    for provider in providers {
        let candidates = match provider {
            Provider::Claude => discover_claude(home, claude_dir_override, &norm_cwd, min_mtime),
            Provider::Codex => discover_codex(home, codex_dir_override, &norm_cwd, min_mtime),
            Provider::Gemini => discover_gemini_or_qwen(home, ".gemini", Provider::Gemini, &norm_cwd, min_mtime),
            Provider::Qwen => discover_qwen(home, &norm_cwd, min_mtime),
            Provider::Pi => discover_pi(home, &norm_cwd, min_mtime),
            Provider::Agy => discover_agy(home, min_mtime, snap.fg_pid),
        };
        if let Some(best) = pick_best_candidate(candidates, snap.started_ms, fg_started_ms, allow_older_fallback) {
            let (entries, mut meta) = match best.provider {
                Provider::Claude => parse_claude_jsonl(&best.path),
                Provider::Codex => parse_codex_jsonl(&best.path),
                Provider::Gemini => parse_gemini_json(&best.path),
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
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn ordered_providers(hint: Option<&str>) -> Vec<Provider> {
    match hint {
        Some("claude") => vec![Provider::Claude],
        Some("codex") => vec![Provider::Codex],
        Some("gemini") => vec![Provider::Gemini],
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
    if lower == "gemini" || lower == "gemini-cli" {
        return Some("gemini");
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
    if !title.contains('/') && !title.contains('\\')
        && let Some(h) = hint_from_label(title)
    {
        return Some(h.to_string());
    }
    None
}

pub(crate) fn hint_from_words<'a>(words: impl Iterator<Item = &'a str>) -> Option<&'static str> {
    for raw in words {
        if raw.starts_with('-') {
            continue;
        }
        let stem = Path::new(raw)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(raw)
            .to_ascii_lowercase();
        let clean = stem.strip_suffix(".real").unwrap_or(stem.as_str());
        if let Some(h) = hint_from_label(clean) {
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
    let start_floor = started_ms.saturating_sub(START_SLACK_MS);
    let fg_floor = fg_started_ms.map(|ms| ms.saturating_sub(START_SLACK_MS));
    let mut after_start: Vec<Candidate> = Vec::new();
    let mut older: Vec<Candidate> = Vec::new();
    for c in candidates {
        if c.first_ts_ms == 0 || c.first_ts_ms >= start_floor {
            after_start.push(c);
        } else {
            older.push(c);
        }
    }
    if !after_start.is_empty() {
        after_start.sort_by(|a, b| {
            let a_fg = fg_floor.is_some_and(|f| a.first_ts_ms == 0 || a.first_ts_ms >= f);
            let b_fg = fg_floor.is_some_and(|f| b.first_ts_ms == 0 || b.first_ts_ms >= f);
            b_fg.cmp(&a_fg)
                .then_with(|| b.mtime_ms.cmp(&a.mtime_ms))
                .then_with(|| b.first_ts_ms.cmp(&a.first_ts_ms))
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
            if !name.starts_with("rollout-") || path.extension().and_then(|s| s.to_str()) != Some("jsonl") || !path.is_file() {
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
        let root_matches = fs::read_to_string(&root_file)
            .ok()
            .is_some_and(|s| cwd_matches(&s, cwd));
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
            if !name.starts_with("session-") || path.extension().and_then(|s| s.to_str()) != Some("json") || !path.is_file() {
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

#[cfg(target_os = "linux")]
fn find_agy_brain_from_proc(fg_pid: u32, home: &Path) -> Option<PathBuf> {
    let fd_dir = PathBuf::from(format!("/proc/{fg_pid}/fd"));
    let entries = fs::read_dir(fd_dir).ok()?;
    for entry in entries.flatten() {
        let Ok(target) = fs::read_link(entry.path()) else {
            continue;
        };
        let target_str = target.to_string_lossy();
        if !target_str.contains("/antigravity-cli/presence/") || !target_str.ends_with(".lock") {
            continue;
        }
        let Some(uuid) = target.file_stem().and_then(|s| s.to_str()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let logs = home
            .join(".gemini/antigravity-cli/brain")
            .join(uuid)
            .join(".system_generated/logs");
        if let Some(log) = pick_agy_log_file(&logs) {
            return Some(log);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn find_agy_brain_from_proc(_fg_pid: u32, _home: &Path) -> Option<PathBuf> {
    None
}

fn discover_agy(home: &Path, min_mtime: u64, fg_pid: Option<u32>) -> Vec<Candidate> {
    if let Some(pid) = fg_pid
        && let Some(path) = find_agy_brain_from_proc(pid, home)
    {
        return vec![Candidate {
            mtime_ms: mtime_ms(&path),
            first_ts_ms: agy_first_ts(&path),
            path,
            provider: Provider::Agy,
        }];
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
            if parts.is_empty() {
                None
            } else {
                cap_tool_detail(&parts.join("\n"))
            }
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
            if cmd.contains('\n') || cmd.len() > 120 {
                cap_tool_detail(cmd)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn parse_claude_jsonl(path: &Path) -> (Vec<Entry>, ReadSessionMeta) {
    let lines = read_bounded_lines(path);
    let mut entries: Vec<Entry> = Vec::new();
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
        if meta.session_id.is_none()
            && let Some(sid) = v.get("sessionId").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
        {
            meta.session_id = Some(sid.to_string());
        }
        if let Some(pm) = v.get("permissionMode").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
            meta.mode = Some(if pm == "bypassPermissions" {
                "dangerously-skip-permissions".into()
            } else {
                pm.to_string()
            });
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
                        if is_claude_command_noise(s) {
                            continue;
                        }
                        let clean = strip_system_reminders(s);
                        if !clean.is_empty() {
                            turn += 1;
                            entries.push(Entry::User { turn, from: None, text: clean, ts_ms });
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
                                ts_ms,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                if let Some(model) = v.pointer("/message/model").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                    meta.model = Some(model.to_string());
                }
                if let Some(usage) = v.pointer("/message/usage") {
                    let input_tok = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let cache_create = usage.get("cache_creation_input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let cache_read = usage.get("cache_read_input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let used = input_tok + cache_create + cache_read;
                    if used > 0 {
                        meta.context_usage = Some(AcpContextUsage { used, size: 200_000 });
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
                                    entries.push(Entry::Assistant { turn, text: clean.to_string(), ts_ms });
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
            let title = str_field("command")
                .or_else(|| str_field("description"))
                .unwrap_or("Bash");
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
            let q = str_field("pattern")
                .or_else(|| str_field("query"))
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

fn codex_tool_detail(name: &str, args: &Value) -> Option<String> {
    match name {
        "apply_patch" => args.as_str().and_then(cap_tool_detail),
        "shell" | "shell_command" | "exec_command" => {
            let cmd = args
                .get("command")
                .or_else(|| args.get("cmd"))
                .and_then(Value::as_str)?
                .trim();
            if cmd.contains('\n') || cmd.len() > 120 {
                cap_tool_detail(cmd)
            } else {
                None
            }
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
                    && let Some(sid) = v
                        .pointer("/payload/id")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
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
                    if let Some(p) = payload.get("approval_policy").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
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
                        entries.push(Entry::User { turn, from: None, text: clean.to_string(), ts_ms });
                    }
                } else if ev_type == Some("token_count")
                    && let Some(info) = v.pointer("/payload/info")
                {
                    let used = info
                        .pointer("/last_token_usage/total_tokens")
                        .or_else(|| info.pointer("/total_token_usage/total_tokens"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let size = info
                        .get("model_context_window")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
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
                            Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::String(s.clone())),
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
                                entries.push(Entry::Assistant { turn, text, ts_ms });
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
                                entries.push(Entry::User { turn, from: None, text: parts.join("\n\n"), ts_ms });
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
                .and_then(|s| s.lines().find(|l| l.starts_with("*** Update File:") || l.starts_with("*** Add File:") || l.starts_with("*** Delete File:")))
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
                    entries.push(Entry::User { turn, from: None, text, ts_ms });
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
                            Some("executing" | "scheduled" | "awaiting_approval" | "pending" | "in_progress") => "in_progress",
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
                            ts_ms,
                        });
                    }
                }
                let text = extract_gemini_text(m.get("content"));
                if !text.is_empty() {
                    entries.push(Entry::Assistant { turn, text, ts_ms });
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
        let cmd = str_field("command")
            .or_else(|| str_field("cmd"))
            .or_else(|| str_field("CommandLine"))?
            .trim();
        if cmd.contains('\n') || cmd.len() > 100 {
            cap_tool_detail(cmd)
        } else {
            None
        }
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
        let cmd = str_field("command")
            .or_else(|| str_field("cmd"))
            .or_else(|| str_field("CommandLine"))
            .unwrap_or(name);
        (Some("execute".into()), cmd.to_string())
    } else if lower.contains("read") {
        let p = str_field("file_path")
            .or_else(|| str_field("path"))
            .or_else(|| str_field("absolute_path"))
            .or_else(|| str_field("Path"));
        (Some("read".into()), p.map_or_else(|| name.to_string(), |p| format!("{name} {p}")))
    } else if lower.contains("edit") || lower.contains("write") || lower.contains("replace") {
        let p = str_field("file_path")
            .or_else(|| str_field("path"))
            .or_else(|| str_field("Path"));
        (Some("edit".into()), p.map_or_else(|| name.to_string(), |p| format!("{name} {p}")))
    } else if lower.contains("search") || lower.contains("grep") || lower.contains("glob") || lower.contains("find") {
        let q = str_field("pattern")
            .or_else(|| str_field("query"))
            .or_else(|| str_field("Query"));
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
                    entries.push(Entry::User { turn, from: None, text, ts_ms });
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
                                    entries.push(Entry::Assistant { turn, text: clean.to_string(), ts_ms });
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
                    entries.push(Entry::User { turn, from: None, text: clean, ts_ms });
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
                            && let Some(plan_text) = args.get("plan").and_then(Value::as_str).filter(|s| !s.trim().is_empty())
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
                            ts_ms,
                        });
                    }
                }
                if let Some(content) = v.get("content").and_then(Value::as_str) {
                    let clean = content.trim();
                    if !clean.is_empty() {
                        entries.push(Entry::Assistant { turn, text: clean.to_string(), ts_ms });
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
        if (last_disk_user_ts == 0 || *ts_ms > last_disk_user_ts)
            && !recent_user_texts.iter().any(|u| u == trimmed)
        {
            entries.push(Entry::User {
                turn: (*turn).max(1),
                from: None,
                text: text.clone(),
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
    let effective_start = if allow_older_fallback {
        0
    } else {
        fg_started.map_or(min_mtime_ms, |f| min_mtime_ms.max(f))
    };
    let scan_min_mtime = if allow_older_fallback {
        0
    } else {
        effective_start.saturating_sub(MTIME_SLACK_MS)
    };
    let mut candidates = match kind {
        "claude" => discover_claude(&home, None, cwd, scan_min_mtime),
        "codex" => discover_codex(&home, None, cwd, scan_min_mtime),
        "agy" => discover_agy(&home, scan_min_mtime, fg_pid),
        _ => return None,
    };
    if !allow_older_fallback
        && let Some(fg_ms) = fg_started
    {
        // A clean non-resumed CLI session can only own transcripts created once that foreground
        // CLI process started (2s slack for filesystem/clock jitter), never a previous session.
        let strict_floor = fg_ms.saturating_sub(2_000);
        candidates.retain(|c| c.first_ts_ms == 0 || c.first_ts_ms >= strict_floor);
    }
    pick_best_candidate(candidates, effective_start, fg_started, allow_older_fallback).map(|c| c.path)
}

pub(crate) fn parse_cli_transcript(kind: &str, path: &Path) -> Vec<Entry> {
    match kind {
        "claude" => parse_claude_jsonl(path).0,
        "codex" => parse_codex_jsonl(path).0,
        "agy" => parse_agy_jsonl(path).0,
        _ => Vec::new(),
    }
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
                (
                    tz_rest[..2].parse::<i64>().ok().unwrap_or(0),
                    tz_rest[2..4].parse::<i64>().ok().unwrap_or(0),
                )
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
        };
        let (entries, meta) = extract_transcript_with_home(&snap, &root, Some(&root), None);
        assert_eq!(entries.len(), 5);
        assert!(matches!(&entries[0], Entry::User { text, turn: 1, .. } if text == "Inspect main.rs"));
        assert!(matches!(&entries[1], Entry::Thought { text, .. } if text == "Let me read the file."));
        assert!(matches!(&entries[2], Entry::ToolCall { title, tool_kind, status, detail, .. } if title == "Read src/main.rs" && tool_kind.as_deref() == Some("read") && status == "completed" && detail.as_deref() == Some("fn main() {}")));
        assert!(matches!(&entries[3], Entry::Plan { items, .. } if items == &vec!["[x] Check code".to_string()]));
        assert!(matches!(&entries[4], Entry::Assistant { text, .. } if text == "All good."));
        let meta = meta.expect("expected Claude session metadata");
        assert_eq!(meta.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(meta.mode.as_deref(), Some("dangerously-skip-permissions"));
        assert_eq!(meta.context_usage, Some(AcpContextUsage { used: 28000, size: 200_000 }));
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
        };
        let (entries, meta) = extract_transcript_with_home(&snap, &root, None, Some(&root));
        assert_eq!(entries.len(), 4);
        assert!(matches!(&entries[0], Entry::User { text, .. } if text == "Run tests"));
        assert!(matches!(&entries[1], Entry::Thought { text, .. } if text == "Checking cargo test"));
        assert!(matches!(&entries[2], Entry::ToolCall { title, tool_kind, status, detail, .. } if title == "cargo test" && tool_kind.as_deref() == Some("execute") && status == "failed" && detail.as_deref() == Some("Exit code: 1\nfailed")));
        assert!(matches!(&entries[3], Entry::Assistant { text, .. } if text == "Tests failed."));
        let meta = meta.expect("expected Codex session metadata");
        assert_eq!(meta.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(meta.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(meta.mode.as_deref(), Some("never"));
        assert_eq!(meta.context_usage, Some(AcpContextUsage { used: 34000, size: 272000 }));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn extracts_gemini_session_json() {
        let root = tmp_dir("gemini");
        let project = "/tmp/gemini/proj";
        let chats = root.join(".gemini/tmp/hash123/chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(root.join(".gemini/tmp/hash123/.project_root"), project).unwrap();
        let session = chats.join("session-2026-11-17T10-00-00.json");
        let payload = serde_json::json!({
            "startTime": "2026-11-17T10:00:00.000Z",
            "messages": [
                { "type": "user", "timestamp": "2026-11-17T10:00:01.000Z", "content": "Explain lib.rs" },
                {
                    "type": "gemini",
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
            agent_hint: Some("gemini".into()),
            sent_prompts: Vec::new(),
            turn: 1,
            resumed: false,
            fg_pid: None,
        };
        let entries = extract_entries_with_home(&snap, &root, None, None);
        assert_eq!(entries.len(), 4);
        assert!(matches!(&entries[0], Entry::User { text, .. } if text == "Explain lib.rs"));
        assert!(matches!(&entries[1], Entry::Thought { text, .. } if text.contains("Reading") && text.contains("Check exports")));
        assert!(matches!(&entries[2], Entry::ToolCall { status, tool_kind, .. } if status == "completed" && tool_kind.as_deref() == Some("read")));
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
        };
        let entries = extract_entries_with_home(&snap, &root, None, None);
        assert_eq!(entries.len(), 4);
        assert!(matches!(&entries[0], Entry::User { text, .. } if text == "Hello Pi"));
        assert!(matches!(&entries[1], Entry::Thought { text, .. } if text == "Thinking..."));
        assert!(matches!(&entries[2], Entry::ToolCall { status, tool_kind, .. } if status == "completed" && tool_kind.as_deref() == Some("execute")));
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
        };
        assert!(extract_entries_with_home(&fresh, &root, Some(&root), None).is_empty());

        let resumed = PtySnapshot { resumed: true, ..fresh };
        let entries = extract_entries_with_home(&resumed, &root, Some(&root), None);
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[0], Entry::User { text, .. } if text == "Old task"));
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
        ).unwrap();

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
        ).unwrap();

        let snap = PtySnapshot {
            cwd: "/tmp/repo".to_string(),
            started_ms: now_ms,
            agent_hint: Some("agy".into()),
            sent_prompts: Vec::new(),
            turn: 1,
            resumed: false,
            fg_pid: None,
        };
        let entries = extract_entries_with_home(&snap, &root, None, None);
        assert_eq!(entries.len(), 3);
        assert!(matches!(&entries[0], Entry::User { text, .. } if text == "Summarize rankings"));
        assert!(matches!(&entries[1], Entry::ToolCall { title, tool_kind, .. } if title == "h2 ls" && tool_kind.as_deref() == Some("execute")));
        assert!(matches!(&entries[2], Entry::Assistant { text, .. } if text.contains("| A | B |")));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn optimistic_prompt_dedup_works() {
        let mut entries = vec![Entry::User {
            turn: 1,
            from: None,
            text: "first prompt".into(),
            ts_ms: 1000,
        }];
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
        assert!(matches!(&agy_entries[2], Entry::Plan { items, .. } if items == &vec!["Step one".to_string(), "Step two".to_string()]));
        fs::remove_dir_all(root).ok();
    }
}
