//! Historical CLI agent session discovery, structured transcript loading, and
//! full-access resume command generation scoped to a workspace and CLI agent.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::checkpoint::TargetCheckpoints;
use crate::drove::crypto::sha256_hex;
use crate::pty_transcript::{
    agy_cid_from_path, claude_root, claude_slug, clean_cursor_user_text, codex_root, created_ms, cursor_project_slug,
    cwd_matches, detect_agent_hint, is_claude_user_noise, is_valid_agy_cid, mtime_ms, normalize_cwd, parse_agy_jsonl,
    parse_claude_jsonl, parse_codex_jsonl, parse_cursor_jsonl, parse_gemini_json, parse_pi_jsonl, parse_ts_ms,
    pick_agy_log_file, sorted_subdirs_desc, strip_system_reminders, strip_user_request_wrapper,
    terminal_known_session_id, user_home_dir,
};
use crate::terminals::Terminal;
use crate::transcript::{Entry, cap_tool_detail, strip_h2_envelope};

const DEFAULT_SESSION_LIMIT: usize = 60;
const MAX_SESSION_LIMIT: usize = 200;
const DEFAULT_ENTRY_LIMIT: usize = 500;
const MAX_ENTRY_LIMIT: usize = 2000;
const HEADER_SCAN_LINES: usize = 80;
const TAIL_SCAN_BYTES: u64 = 48 * 1024;
const FULL_SCAN_MAX_BYTES: u64 = 768 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CliSessionSummary {
    pub session_id: String,
    pub agent: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    pub turns: u64,
    pub created_ms: u64,
    pub updated_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// Ready-to-run CLI command that resumes this session with full-access permissions.
    pub resume_command: String,
    #[serde(default)]
    pub current: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CliSessionListResult {
    pub cwd: String,
    pub agent: String,
    pub agent_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_session_id: Option<String>,
    pub sessions: Vec<CliSessionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CliSessionReadResult {
    pub session_id: String,
    pub agent: String,
    pub agent_name: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub turns: u64,
    pub updated_ms: u64,
    pub resume_command: String,
    pub entries: Vec<Entry>,
}

/// Maps aliases, binary stems, or OSC 7501 app names to a canonical CLI agent key.
pub fn canonical_cli_agent(raw: &str) -> Option<&'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return None;
    }
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed).to_ascii_lowercase();
    let no_ext = [".exe", ".cmd", ".bat", ".ps1", ".js", ".mjs", ".cjs", ".ts", ".py", ".rb"]
        .iter()
        .find_map(|ext| base.strip_suffix(ext))
        .unwrap_or(&base);
    let stem = no_ext.strip_suffix(".real").unwrap_or(no_ext);
    match stem {
        "claude" | "claude-code" | "claude code" => Some("claude"),
        "codex" => Some("codex"),
        "agy" | "antigravity" | "antigravity-cli" => Some("agy"),
        "cursor" | "cursor-agent" | "cursor agent" => Some("cursor"),
        "zcode" | "zcode-cli" if !trimmed.replace('\\', "/").to_ascii_lowercase().contains("/opt/zcode/") => {
            Some("zcode")
        }
        "opencode" | "opencode-ai" => Some("opencode"),
        "qwen" | "qwen-code" | "qwen code" => Some("qwen"),
        "gemini" | "gemini-cli" | "gemini cli" => Some("gemini"),
        "pi" => Some("pi"),
        "omp" | "oh-my-pi" => Some("omp"),
        "hermes" => Some("hermes"),
        "kimi" | "kimi-code" | "kimi-cli" | "kimicodecli" => Some("kimi"),
        "copilot" | "github-copilot" => Some("copilot"),
        "qoder" | "qodercli" => Some("qoder"),
        "kiro" | "kiro-cli" => Some("kiro"),
        "amp" => Some("amp"),
        "crush" => Some("crush"),
        "trae" | "traecli" | "traex" | "trae-cli" => Some("trae"),
        _ => None,
    }
}

pub fn cli_agent_display_name(agent: &str) -> &'static str {
    match canonical_cli_agent(agent).unwrap_or(agent) {
        "claude" => "Claude Code",
        "codex" => "Codex",
        "agy" => "Antigravity",
        "cursor" => "Cursor",
        "zcode" => "ZCode",
        "opencode" => "OpenCode",
        "qwen" => "Qwen Code",
        "gemini" => "Gemini CLI",
        "pi" => "Pi",
        "omp" => "oh-my-pi",
        "hermes" => "Hermes",
        "kimi" => "Kimi",
        "copilot" => "GitHub Copilot",
        "qoder" => "Qoder",
        "kiro" => "Kiro",
        "amp" => "Amp",
        "crush" => "Crush",
        "trae" => "TRAE CLI",
        _ => "CLI Agent",
    }
}

/// Detects the CLI agent key and active session ID (when known) for a PTY terminal.
pub fn detect_terminal_cli_agent(t: &Terminal, tc: Option<&TargetCheckpoints>) -> (Option<String>, Option<String>) {
    let mut detected: Option<String> = None;

    if let Some(fg) = &t.foreground {
        let unwrapped = crate::pty_adapters::unwrap_shell_cli_argv(&fg.argv);
        for tok in unwrapped.iter().map(String::as_str).chain(std::iter::once(fg.name.as_str())) {
            if let Some(k) = canonical_cli_agent(tok) {
                detected = Some(k.to_string());
                break;
            }
        }
    }
    if detected.is_none()
        && let Some(ref s) = t.status
        && let Some(k) = canonical_cli_agent(&s.app)
    {
        detected = Some(k.to_string());
    }
    if detected.is_none()
        && let Some(ref saved) = t.saved_cli
        && let Some(k) = canonical_cli_agent(&saved.kind)
    {
        detected = Some(k.to_string());
    }
    if detected.is_none()
        && let Some(tc_kind) = tc.and_then(|c| c.cli_kind.as_deref())
        && let Some(k) = canonical_cli_agent(tc_kind)
    {
        detected = Some(k.to_string());
    }
    if detected.is_none() {
        let unwrapped = crate::pty_adapters::unwrap_shell_cli_argv(&t.argv);
        for tok in &unwrapped {
            if let Some(k) = canonical_cli_agent(tok) {
                detected = Some(k.to_string());
                break;
            }
        }
    }
    if detected.is_none()
        && let Some(hint) = detect_agent_hint(t)
        && let Some(k) = canonical_cli_agent(&hint)
    {
        detected = Some(k.to_string());
    }

    let is_agy = detected.as_deref() == Some("agy");
    let sid = tc
        .and_then(|c| c.cli_session_id.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty() && (!is_agy || is_valid_agy_cid(s)))
        .map(str::to_string)
        .or_else(|| terminal_known_session_id(t, detected.as_deref()));

    (detected, sid)
}

fn quote_cli_arg(arg: &str) -> String {
    let trimmed = arg.trim();
    if !trimmed.is_empty()
        && trimmed.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/'))
    {
        return trimmed.to_string();
    }
    if cfg!(windows) {
        format!("\"{}\"", trimmed.replace('"', "\\\""))
    } else {
        format!("'{}'", trimmed.replace('\'', "'\\''"))
    }
}

/// Builds a shell-ready CLI command that resumes `session_id` with full-access permissions.
pub fn build_full_access_resume_command(agent: &str, session_id: &str) -> String {
    let key = canonical_cli_agent(agent).unwrap_or(agent.trim());
    let sid = quote_cli_arg(session_id);
    match key {
        "claude" => format!("claude --dangerously-skip-permissions --resume {sid}"),
        "codex" => format!("codex resume --dangerously-bypass-approvals-and-sandbox {sid}"),
        "agy" => format!("agy --dangerously-skip-permissions --conversation {sid}"),
        "cursor" => format!("cursor-agent --yolo --trust --resume {sid}"),
        "zcode" => format!("zcode --mode yolo --resume {sid}"),
        "opencode" => format!("opencode --auto --session {sid}"),
        "qwen" => format!("qwen --yolo --resume {sid}"),
        "gemini" => format!("gemini --yolo --resume {sid}"),
        "hermes" => format!("hermes --yolo --accept-hooks --resume {sid}"),
        "pi" => format!("pi --session {sid}"),
        "omp" => format!("omp --session {sid}"),
        "kimi" => format!("kimi --auto --session {sid}"),
        "copilot" => format!("copilot --allow-all --resume {sid}"),
        "qoder" => format!("qodercli --dangerously-skip-permissions --resume {sid}"),
        "amp" => format!("amp --dangerously-allow-all threads continue {sid}"),
        "kiro" => "kiro-cli chat --trust-all-tools --resume".to_string(),
        "crush" => "crush --yolo".to_string(),
        other => format!("{other} --dangerously-skip-permissions --resume {sid}"),
    }
}

fn clean_single_line_title(raw: &str, max_chars: usize) -> String {
    let stripped = strip_h2_envelope(raw);
    let mut cleaned = String::with_capacity(stripped.len());
    let mut in_pasted_tag = false;
    let mut chars = stripped.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '<' {
            let rest: String = chars.clone().take(24).collect();
            let lower = rest.to_ascii_lowercase();
            if lower.starts_with("pasted_content") || lower.starts_with("/pasted_content") {
                in_pasted_tag = true;
                continue;
            }
        }
        if in_pasted_tag {
            if ch == '>' {
                in_pasted_tag = false;
                cleaned.push(' ');
            }
            continue;
        }
        if matches!(ch, '\r' | '\n' | '\t') {
            cleaned.push(' ');
        } else {
            cleaned.push(ch);
        }
    }
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        collapsed
    } else {
        let mut out: String = collapsed.chars().take(max_chars).collect();
        out.push('…');
        out
    }
}

fn workspace_candidates(cwd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let norm = normalize_cwd(cwd);
    if !norm.is_empty() {
        out.push(norm.clone());
    }
    if !norm.is_empty()
        && let Ok(canon) = fs::canonicalize(&norm)
    {
        let cn = normalize_cwd(&canon.to_string_lossy());
        if !cn.is_empty() && !out.iter().any(|x| cwd_matches(x, &cn)) {
            out.push(cn);
        }
    }
    if !norm.is_empty()
        && let Some(repo) = crate::checkpoint::probe_repo(Path::new(&norm))
    {
        let rn = normalize_cwd(&repo.toplevel.to_string_lossy());
        if !rn.is_empty() && !out.iter().any(|x| cwd_matches(x, &rn)) {
            out.push(rn);
        }
    }
    out
}

fn matches_any_workspace(candidate_cwd: &str, targets: &[String]) -> bool {
    targets.iter().any(|t| cwd_matches(candidate_cwd, t))
}

pub fn list_cli_sessions(
    state_dir: &Path,
    cwd: &str,
    agent: &str,
    active_session_id: Option<&str>,
    limit: Option<usize>,
) -> CliSessionListResult {
    let Some(home) = user_home_dir() else {
        let canon_agent = canonical_cli_agent(agent).unwrap_or(agent).to_string();
        return CliSessionListResult {
            cwd: normalize_cwd(cwd),
            agent_name: cli_agent_display_name(&canon_agent).to_string(),
            agent: canon_agent,
            active_session_id: active_session_id.map(str::to_string),
            sessions: Vec::new(),
        };
    };
    list_cli_sessions_in(&home, state_dir, cwd, agent, active_session_id, limit)
}

pub fn list_cli_sessions_in(
    home: &Path,
    state_dir: &Path,
    cwd: &str,
    agent: &str,
    active_session_id: Option<&str>,
    limit: Option<usize>,
) -> CliSessionListResult {
    let canon_agent = canonical_cli_agent(agent).unwrap_or(agent.trim()).to_string();
    let agent_name = cli_agent_display_name(&canon_agent).to_string();
    let norm_cwd = normalize_cwd(cwd);
    let max_items = limit.unwrap_or(DEFAULT_SESSION_LIMIT).clamp(1, MAX_SESSION_LIMIT);
    let active_sid = active_session_id.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);

    if norm_cwd.is_empty() || canon_agent.is_empty() {
        return CliSessionListResult {
            cwd: norm_cwd,
            agent: canon_agent,
            agent_name,
            active_session_id: active_sid,
            sessions: Vec::new(),
        };
    }

    let targets = workspace_candidates(&norm_cwd);
    let mut sessions = match canon_agent.as_str() {
        "claude" => discover_claude_sessions(home, &targets, active_sid.as_deref()),
        "codex" => discover_codex_sessions(home, &targets, active_sid.as_deref()),
        "agy" => discover_agy_sessions(home, state_dir, &targets, active_sid.as_deref()),
        "cursor" => discover_cursor_sessions(home, &targets, active_sid.as_deref()),
        "zcode" => discover_zcode_sessions(home, state_dir, &targets, active_sid.as_deref()),
        "opencode" => discover_opencode_sessions(home, &targets, active_sid.as_deref()),
        "qwen" => discover_qwen_or_gemini_sessions(home, "qwen", &targets, active_sid.as_deref()),
        "gemini" => discover_qwen_or_gemini_sessions(home, "gemini", &targets, active_sid.as_deref()),
        "pi" => discover_pi_or_omp_sessions(home, "pi", &targets, active_sid.as_deref()),
        "omp" => discover_pi_or_omp_sessions(home, "omp", &targets, active_sid.as_deref()),
        "hermes" => discover_hermes_sessions(home, &targets, active_sid.as_deref()),
        _ => discover_archived_checkpoint_sessions(state_dir, &canon_agent, &targets, active_sid.as_deref()),
    };

    // Deduplicate by session_id keeping newest updated_ms.
    sessions.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms).then_with(|| b.created_ms.cmp(&a.created_ms)));
    let mut seen = HashSet::new();
    sessions.retain(|s| seen.insert(s.session_id.clone()));

    for s in &mut sessions {
        s.current = active_sid.as_deref() == Some(s.session_id.as_str());
    }
    // Keep current session visible at top if present, otherwise strictly newest-first.
    sessions.sort_by(|a, b| {
        b.current
            .cmp(&a.current)
            .then_with(|| b.updated_ms.cmp(&a.updated_ms))
            .then_with(|| b.created_ms.cmp(&a.created_ms))
    });
    sessions.truncate(max_items);

    CliSessionListResult { cwd: norm_cwd, agent: canon_agent, agent_name, active_session_id: active_sid, sessions }
}

pub fn read_cli_session(
    state_dir: &Path,
    cwd: Option<&str>,
    agent: &str,
    session_id: &str,
    limit: Option<usize>,
) -> Result<CliSessionReadResult, String> {
    let home = user_home_dir().ok_or_else(|| "cannot resolve user home directory".to_string())?;
    read_cli_session_in(&home, state_dir, cwd, agent, session_id, limit)
}

pub fn read_cli_session_in(
    home: &Path,
    state_dir: &Path,
    cwd: Option<&str>,
    agent: &str,
    session_id: &str,
    limit: Option<usize>,
) -> Result<CliSessionReadResult, String> {
    let canon_agent = canonical_cli_agent(agent).unwrap_or(agent.trim()).to_string();
    let agent_name = cli_agent_display_name(&canon_agent).to_string();
    let sid = session_id.trim();
    if sid.is_empty() {
        return Err("session_id is required".into());
    }
    let max_entries = limit.unwrap_or(DEFAULT_ENTRY_LIMIT).clamp(1, MAX_ENTRY_LIMIT);
    let norm_cwd = cwd.map(normalize_cwd).filter(|s| !s.is_empty());

    let (mut entries, title_hint, model, updated_ms, resume_target) = match canon_agent.as_str() {
        "claude" => {
            let path = locate_claude_file(home, norm_cwd.as_deref(), sid)
                .ok_or_else(|| format!("Claude session `{sid}` not found"))?;
            let mt = mtime_ms(&path);
            let custom = extract_claude_custom_title(&path);
            let (entries, meta) = parse_claude_jsonl(&path);
            (entries, custom, meta.model, mt, sid.to_string())
        }
        "codex" => {
            let path = locate_codex_file(home, norm_cwd.as_deref(), sid)
                .ok_or_else(|| format!("Codex session `{sid}` not found"))?;
            let mt = mtime_ms(&path);
            let idx_titles = load_codex_session_index(home);
            let custom = idx_titles.get(sid).cloned();
            let (entries, meta) = parse_codex_jsonl(&path);
            (entries, custom, meta.model, mt, sid.to_string())
        }
        "agy" => {
            if !is_valid_agy_cid(sid) {
                return Err(format!("invalid Antigravity conversation ID `{sid}`"));
            }
            let logs = home.join(".gemini/antigravity-cli/brain").join(sid).join(".system_generated/logs");
            let path = pick_agy_log_file(&logs).ok_or_else(|| format!("Antigravity session `{sid}` not found"))?;
            let mt = mtime_ms(&path);
            let db_title = lookup_agy_db_title(home, sid);
            let (entries, meta) = parse_agy_jsonl(&path);
            (entries, db_title, meta.model, mt, sid.to_string())
        }
        "cursor" => {
            let path = locate_cursor_file(home, norm_cwd.as_deref(), sid)
                .ok_or_else(|| format!("Cursor session `{sid}` not found"))?;
            let mt = mtime_ms(&path);
            let (entries, meta) = parse_cursor_jsonl(&path);
            (entries, None, meta.model, mt, sid.to_string())
        }
        "zcode" => read_zcode_transcript(home, state_dir, sid)?,
        "opencode" => read_opencode_transcript(home, sid)?,
        "qwen" => {
            let path = locate_qwen_or_gemini_file(home, "qwen", norm_cwd.as_deref(), sid)
                .ok_or_else(|| format!("Qwen session `{sid}` not found"))?;
            let mt = mtime_ms(&path);
            let (entries, meta) = if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                parse_claude_jsonl(&path)
            } else {
                parse_gemini_json(&path)
            };
            (entries, None, meta.model, mt, sid.to_string())
        }
        "gemini" => {
            let path = locate_qwen_or_gemini_file(home, "gemini", norm_cwd.as_deref(), sid)
                .ok_or_else(|| format!("Gemini session `{sid}` not found"))?;
            let mt = mtime_ms(&path);
            let (entries, meta) = parse_gemini_json(&path);
            (entries, None, meta.model, mt, sid.to_string())
        }
        "pi" | "omp" => {
            let path = locate_pi_or_omp_file(home, &canon_agent, norm_cwd.as_deref(), sid)
                .ok_or_else(|| format!("{agent_name} session `{sid}` not found"))?;
            let mt = mtime_ms(&path);
            let resume_arg = path.to_string_lossy().to_string();
            let (entries, meta) = parse_pi_jsonl(&path);
            (entries, None, meta.model, mt, resume_arg)
        }
        "hermes" => read_hermes_transcript(home, sid)?,
        _ => read_archived_checkpoint_session(state_dir, &canon_agent, sid)?,
    };

    let turns = entries.iter().filter_map(Entry::turn).max().unwrap_or(0);
    let fallback_title = entries
        .iter()
        .find_map(|e| match e {
            Entry::User { text, .. } => {
                let c = clean_single_line_title(text, 100);
                (!c.is_empty()).then_some(c)
            }
            _ => None,
        })
        .unwrap_or_else(|| sid.to_string());
    let title =
        title_hint.map(|t| clean_single_line_title(&t, 100)).filter(|t| !t.is_empty()).unwrap_or(fallback_title);

    if entries.len() > max_entries {
        let drop_n = entries.len() - max_entries;
        entries.drain(0..drop_n);
    }

    Ok(CliSessionReadResult {
        session_id: sid.to_string(),
        agent_name,
        resume_command: build_full_access_resume_command(&canon_agent, &resume_target),
        agent: canon_agent,
        title,
        cwd: norm_cwd,
        model,
        turns,
        updated_ms,
        entries,
    })
}

// ---------------------------------------------------------------------------
// Claude Code
// ---------------------------------------------------------------------------

fn read_head_and_tail_lines(path: &Path) -> (Vec<String>, Vec<String>, bool) {
    let Ok(mut file) = File::open(path) else {
        return (Vec::new(), Vec::new(), false);
    };
    let len = file.metadata().map_or(0, |m| m.len());
    if len == 0 {
        return (Vec::new(), Vec::new(), false);
    }
    if len <= FULL_SCAN_MAX_BYTES {
        let mut raw = String::new();
        if file.read_to_string(&mut raw).is_ok() {
            return (raw.lines().map(str::to_string).collect(), Vec::new(), true);
        }
    }
    let mut head = Vec::new();
    {
        let reader = BufReader::new(&mut file);
        for line in reader.lines().take(HEADER_SCAN_LINES).flatten() {
            head.push(line);
        }
    }
    let mut tail = Vec::new();
    if len > TAIL_SCAN_BYTES
        && let Ok(mut tail_file) = File::open(path)
        && tail_file.seek(SeekFrom::Start(len - TAIL_SCAN_BYTES)).is_ok()
    {
        let mut buf = Vec::new();
        if tail_file.read_to_end(&mut buf).is_ok() {
            let text = String::from_utf8_lossy(&buf);
            let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
            if !lines.is_empty() {
                lines.remove(0);
            }
            tail = lines;
        }
    }
    (head, tail, false)
}

fn extract_claude_user_text(v: &Value) -> Option<String> {
    if v.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let content = v.pointer("/message/content")?;
    match content {
        Value::String(s) => {
            if is_claude_user_noise(s) {
                return None;
            }
            let clean = strip_system_reminders(s);
            let line = clean_single_line_title(&clean, 140);
            (!line.is_empty()).then_some(line)
        }
        Value::Array(blocks) => {
            if blocks.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")) {
                return None;
            }
            for b in blocks {
                if b.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(raw) = b.get("text").and_then(Value::as_str)
                    && !is_claude_user_noise(raw)
                {
                    let clean = strip_system_reminders(raw);
                    let line = clean_single_line_title(&clean, 140);
                    if !line.is_empty() {
                        return Some(line);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn summarize_claude_file(
    path: &Path,
    targets: &[String],
    require_cwd_match: bool,
    active_sid: Option<&str>,
) -> Option<CliSessionSummary> {
    let stem = path.file_stem()?.to_str()?.trim();
    if stem.is_empty() || stem.starts_with("agent-") {
        return None;
    }
    let mt = mtime_ms(path);
    let (head, tail, full_scan) = read_head_and_tail_lines(path);
    if head.is_empty() {
        return None;
    }

    let mut sid: Option<String> = None;
    let mut found_cwd: Option<String> = None;
    let mut git_branch: Option<String> = None;
    let mut model: Option<String> = None;
    let mut first_ts = 0u64;
    let mut first_prompt: Option<String> = None;
    let mut last_preview: Option<String> = None;
    let mut custom_title: Option<String> = None;
    let mut summary_title: Option<String> = None;
    let mut turns = 0u64;

    for line in head.iter().chain(tail.iter()) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || v.get("isMeta").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        if sid.is_none()
            && let Some(s) = v.get("sessionId").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
        {
            sid = Some(s.to_string());
        }
        if found_cwd.is_none()
            && let Some(c) = v.get("cwd").and_then(Value::as_str).filter(|s| !s.trim().is_empty())
        {
            found_cwd = Some(c.to_string());
        }
        if git_branch.is_none()
            && let Some(b) = v.get("gitBranch").and_then(Value::as_str).filter(|s| !s.trim().is_empty() && *s != "HEAD")
        {
            git_branch = Some(b.to_string());
        }
        if first_ts == 0
            && let Some(ts) = v.get("timestamp").and_then(Value::as_str)
        {
            first_ts = parse_ts_ms(ts);
        }
        match v.get("type").and_then(Value::as_str) {
            Some("custom-title") => {
                if let Some(ct) = v.get("customTitle").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                    custom_title = Some(clean_single_line_title(ct, 100));
                }
            }
            Some("summary") => {
                if let Some(sm) = v.get("summary").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                    summary_title = Some(clean_single_line_title(sm, 100));
                }
            }
            Some("user") => {
                if let Some(u) = extract_claude_user_text(&v) {
                    if full_scan {
                        turns += 1;
                    } else if first_prompt.is_none() {
                        turns = 1;
                    }
                    if first_prompt.is_none() {
                        first_prompt = Some(u.clone());
                    }
                    last_preview = Some(u);
                }
            }
            Some("assistant") => {
                if let Some(m) = v.pointer("/message/model").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                    model = Some(m.to_string());
                }
            }
            _ => {}
        }
    }

    if require_cwd_match && !found_cwd.as_deref().is_some_and(|c| matches_any_workspace(c, targets)) {
        return None;
    }
    if let Some(ref c) = found_cwd
        && !matches_any_workspace(c, targets)
    {
        return None;
    }

    let final_sid = sid.unwrap_or_else(|| stem.to_string());
    let is_active = active_sid == Some(final_sid.as_str());
    let title = custom_title.or(summary_title).or_else(|| first_prompt.clone());
    if title.is_none() && !is_active {
        return None;
    }
    let final_title = title.unwrap_or_else(|| final_sid.clone());
    let preview = last_preview.filter(|p| p != &final_title);

    Some(CliSessionSummary {
        resume_command: build_full_access_resume_command("claude", &final_sid),
        session_id: final_sid,
        agent: "claude".into(),
        title: final_title,
        preview,
        turns,
        created_ms: if first_ts > 0 { first_ts } else { mt },
        updated_ms: mt.max(first_ts),
        model,
        git_branch,
        current: is_active,
    })
}

fn extract_claude_custom_title(path: &Path) -> Option<String> {
    let (head, tail, _) = read_head_and_tail_lines(path);
    let mut custom = None;
    let mut summary = None;
    for line in head.iter().chain(tail.iter()) {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
        match v.get("type").and_then(Value::as_str) {
            Some("custom-title") => {
                if let Some(ct) = v.get("customTitle").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                    custom = Some(ct.to_string());
                }
            }
            Some("summary") => {
                if let Some(sm) = v.get("summary").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                    summary = Some(sm.to_string());
                }
            }
            _ => {}
        }
    }
    custom.or(summary)
}

fn discover_claude_sessions(home: &Path, targets: &[String], active_sid: Option<&str>) -> Vec<CliSessionSummary> {
    let projects_dir = claude_root(home, None).join("projects");
    if !projects_dir.is_dir() {
        return Vec::new();
    }
    let mut visited_dirs = HashSet::new();
    let mut out = Vec::new();

    for t in targets {
        let slug_dir = projects_dir.join(claude_slug(t));
        if slug_dir.is_dir()
            && visited_dirs.insert(slug_dir.clone())
            && let Ok(entries) = fs::read_dir(&slug_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("jsonl")
                    && path.is_file()
                    && let Some(s) = summarize_claude_file(&path, targets, false, active_sid)
                {
                    out.push(s);
                }
            }
        }
    }

    if out.is_empty()
        && let Ok(entries) = fs::read_dir(&projects_dir)
    {
        let mut dirs: Vec<(u64, PathBuf)> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && !visited_dirs.contains(p))
            .map(|p| (mtime_ms(&p), p))
            .collect();
        dirs.sort_by_key(|b| std::cmp::Reverse(b.0));
        for (_, dir) in dirs.into_iter().take(16) {
            let Ok(files) = fs::read_dir(&dir) else { continue };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|s| s.to_str()) == Some("jsonl")
                    && path.is_file()
                    && let Some(s) = summarize_claude_file(&path, targets, true, active_sid)
                {
                    out.push(s);
                }
            }
        }
    }
    out
}

fn locate_claude_file(home: &Path, cwd: Option<&str>, session_id: &str) -> Option<PathBuf> {
    let projects_dir = claude_root(home, None).join("projects");
    if let Some(c) = cwd {
        for t in workspace_candidates(c) {
            let direct = projects_dir.join(claude_slug(&t)).join(format!("{session_id}.jsonl"));
            if direct.is_file() {
                return Some(direct);
            }
        }
    }
    let entries = fs::read_dir(&projects_dir).ok()?;
    for entry in entries.flatten() {
        let cand = entry.path().join(format!("{session_id}.jsonl"));
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Codex CLI
// ---------------------------------------------------------------------------

fn load_codex_session_index(home: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let idx_path = codex_root(home, None).join("session_index.jsonl");
    let Ok(file) = File::open(&idx_path) else {
        return map;
    };
    for line in BufReader::new(file).lines().take(5000).flatten() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
        let Some(id) = v.get("id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        if let Some(name) = v.get("thread_name").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
            map.insert(id.to_string(), name.to_string());
        }
    }
    map
}

fn collect_codex_rollout_dirs(home: &Path, max_days: usize) -> Vec<PathBuf> {
    let root = codex_root(home, None);
    let mut dirs = Vec::new();
    let sessions_dir = root.join("sessions");
    if sessions_dir.is_dir() {
        for year in sorted_subdirs_desc(&sessions_dir) {
            for month in sorted_subdirs_desc(&year) {
                for day in sorted_subdirs_desc(&month) {
                    dirs.push(day);
                    if dirs.len() >= max_days {
                        break;
                    }
                }
                if dirs.len() >= max_days {
                    break;
                }
            }
            if dirs.len() >= max_days {
                break;
            }
        }
        if dirs.is_empty() {
            dirs.push(sessions_dir);
        }
    }
    let archived = root.join("archived_sessions");
    if archived.is_dir() {
        dirs.push(archived);
    }
    dirs
}

fn summarize_codex_file(
    path: &Path,
    targets: &[String],
    idx_titles: &HashMap<String, String>,
    active_sid: Option<&str>,
) -> Option<CliSessionSummary> {
    let mt = mtime_ms(path);
    let (head, tail, full_scan) = read_head_and_tail_lines(path);
    if head.is_empty() {
        return None;
    }

    let mut sid: Option<String> = None;
    let mut meta_cwd: Option<String> = None;
    let mut git_branch: Option<String> = None;
    let mut model: Option<String> = None;
    let mut first_ts = 0u64;
    let mut first_prompt: Option<String> = None;
    let mut turns = 0u64;

    for line in head.iter().chain(tail.iter()) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else { continue };
        if first_ts == 0
            && let Some(ts) = v.get("timestamp").and_then(Value::as_str)
        {
            first_ts = parse_ts_ms(ts);
        }
        match v.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if let Some(id) =
                    v.pointer("/payload/id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
                {
                    sid = Some(id.to_string());
                }
                if let Some(c) = v.pointer("/payload/cwd").and_then(Value::as_str) {
                    meta_cwd = Some(c.to_string());
                }
                if let Some(b) =
                    v.pointer("/payload/git/branch").and_then(Value::as_str).filter(|s| !s.trim().is_empty())
                {
                    git_branch = Some(b.to_string());
                }
                if let Some(m) = v.pointer("/payload/model").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                    model = Some(m.to_string());
                }
                if first_ts == 0
                    && let Some(ts) = v.pointer("/payload/timestamp").and_then(Value::as_str)
                {
                    first_ts = parse_ts_ms(ts);
                }
            }
            Some("turn_context") => {
                if let Some(m) = v.pointer("/payload/model").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                    model = Some(m.to_string());
                }
            }
            Some("event_msg") => {
                if v.pointer("/payload/type").and_then(Value::as_str) == Some("user_message")
                    && let Some(msg) = v.pointer("/payload/message").and_then(Value::as_str)
                {
                    let clean = clean_single_line_title(msg, 120);
                    if !clean.is_empty() {
                        if full_scan {
                            turns += 1;
                        } else if first_prompt.is_none() {
                            turns = 1;
                        }
                        if first_prompt.is_none() {
                            first_prompt = Some(clean);
                        }
                    }
                }
            }
            Some("response_item") if first_prompt.is_none() => {
                if v.pointer("/payload/type").and_then(Value::as_str) == Some("message")
                    && v.pointer("/payload/role").and_then(Value::as_str) == Some("user")
                    && let Some(arr) = v.pointer("/payload/content").and_then(Value::as_array)
                {
                    for b in arr {
                        if b.get("type").and_then(Value::as_str) == Some("input_text")
                            && let Some(t) = b.get("text").and_then(Value::as_str).map(str::trim)
                            && !t.is_empty()
                            && !t.starts_with("<environment_context>")
                            && !t.starts_with("<permissions instructions>")
                            && !t.starts_with("# AGENTS.md")
                            && !t.starts_with("<user_instructions>")
                        {
                            let clean = clean_single_line_title(t, 120);
                            if !clean.is_empty() {
                                first_prompt = Some(clean);
                                turns = turns.max(1);
                                break;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if !meta_cwd.as_deref().is_some_and(|c| matches_any_workspace(c, targets)) {
        return None;
    }
    let final_sid = sid?;
    let is_active = active_sid == Some(final_sid.as_str());
    let indexed = idx_titles.get(&final_sid).map(|s| clean_single_line_title(s, 100)).filter(|s| !s.is_empty());
    let title = indexed.or_else(|| first_prompt.clone());
    if title.is_none() && !is_active {
        return None;
    }
    let final_title = title.unwrap_or_else(|| final_sid.clone());
    let preview = first_prompt.filter(|p| p != &final_title);

    Some(CliSessionSummary {
        resume_command: build_full_access_resume_command("codex", &final_sid),
        session_id: final_sid,
        agent: "codex".into(),
        title: final_title,
        preview,
        turns,
        created_ms: if first_ts > 0 { first_ts } else { mt },
        updated_ms: mt.max(first_ts),
        model,
        git_branch,
        current: is_active,
    })
}

fn discover_codex_sessions(home: &Path, targets: &[String], active_sid: Option<&str>) -> Vec<CliSessionSummary> {
    let idx_titles = load_codex_session_index(home);
    let dirs = collect_codex_rollout_dirs(home, 120);
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !name.starts_with("rollout-")
                || path.extension().and_then(|s| s.to_str()) != Some("jsonl")
                || !path.is_file()
            {
                continue;
            }
            if let Some(s) = summarize_codex_file(&path, targets, &idx_titles, active_sid) {
                out.push(s);
            }
        }
    }
    out
}

fn locate_codex_file(home: &Path, _cwd: Option<&str>, session_id: &str) -> Option<PathBuf> {
    let suffix = format!("-{session_id}.jsonl");
    let mut hits: Vec<(u64, PathBuf)> = Vec::new();
    for dir in collect_codex_rollout_dirs(home, 365) {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.ends_with(&suffix) && path.is_file() {
                hits.push((mtime_ms(&path), path));
            }
        }
    }
    hits.sort_by_key(|b| std::cmp::Reverse(b.0));
    hits.into_iter().next().map(|(_, p)| p)
}

// ---------------------------------------------------------------------------
// Antigravity (`agy`)
// ---------------------------------------------------------------------------

fn decode_file_uri(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let rest = trimmed.strip_prefix("file://")?;
    // Percent-decode UTF-8 sequences (`%20`, Chinese UTF-8 bytes, etc.).
    let bytes = rest.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(val) = u8::from_str_radix(hex, 16)
        {
            decoded.push(val);
            i += 3;
            continue;
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    let mut path = String::from_utf8_lossy(&decoded).into_owned();
    // Windows `file:///C:/Users/...` -> `C:/Users/...`.
    if path.len() >= 4
        && path.as_bytes()[0] == b'/'
        && path.as_bytes()[1].is_ascii_alphabetic()
        && path.as_bytes()[2] == b':'
    {
        path.remove(0);
    }
    Some(path)
}

fn workspace_uris_match(raw_json: &str, targets: &[String]) -> bool {
    if let Ok(arr) = serde_json::from_str::<Vec<String>>(raw_json) {
        for uri in arr {
            if let Some(decoded) = decode_file_uri(&uri) {
                if matches_any_workspace(&decoded, targets) {
                    return true;
                }
            } else if matches_any_workspace(&uri, targets) {
                return true;
            }
        }
    }
    false
}

/// Runs a read-only SQLite query returning a JSON array of objects via `python3`/`python` or `sqlite3 -json`.
fn query_sqlite_json(db_path: &Path, sql: &str, params: &[&str]) -> Option<Vec<Value>> {
    if !db_path.is_file() {
        return None;
    }
    let py_script = r#"import json, sqlite3, sys
db = sys.argv[1]
sql = sys.argv[2]
params = sys.argv[3:]
con = sqlite3.connect("file:" + db + "?mode=ro", uri=True, timeout=1.5)
con.row_factory = sqlite3.Row
rows = [dict(r) for r in con.execute(sql, params).fetchall()]
con.close()
sys.stdout.write(json.dumps(rows))
"#;
    let db_str = db_path.to_string_lossy().to_string();
    for py in ["python3", "python"] {
        let mut cmd = crate::platform::bg_command(py);
        cmd.arg("-c").arg(py_script).arg(&db_str).arg(sql);
        for p in params {
            cmd.arg(p);
        }
        if let Ok(out) = cmd.output()
            && out.status.success()
            && let Ok(rows) = serde_json::from_slice::<Vec<Value>>(&out.stdout)
        {
            return Some(rows);
        }
    }
    if params.is_empty() {
        let mut cmd = crate::platform::bg_command("sqlite3");
        cmd.args(["-readonly", "-json"]).arg(&db_str).arg(sql);
        if let Ok(out) = cmd.output()
            && out.status.success()
        {
            if out.stdout.iter().all(u8::is_ascii_whitespace) {
                return Some(Vec::new());
            }
            if let Ok(rows) = serde_json::from_slice::<Vec<Value>>(&out.stdout) {
                return Some(rows);
            }
        }
    }
    None
}

fn summarize_agy_log(path: &Path) -> (Option<String>, Option<String>, u64, u64, u64) {
    let mt = mtime_ms(path);
    let (head, tail, full_scan) = read_head_and_tail_lines(path);
    let mut first_prompt = None;
    let mut model = None;
    let mut first_ts = 0u64;
    let mut turns = 0u64;
    for line in head.iter().chain(tail.iter()) {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
        if first_ts == 0
            && let Some(ts) = v.get("created_at").and_then(Value::as_str)
        {
            first_ts = parse_ts_ms(ts);
        }
        if let Some(m) = v.get("model").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
            model = Some(m.to_string());
        }
        if v.get("type").and_then(Value::as_str) == Some("USER_INPUT")
            && let Some(raw) = v.get("content").and_then(Value::as_str)
        {
            let clean = clean_single_line_title(&strip_user_request_wrapper(raw), 120);
            if !clean.is_empty() {
                if full_scan {
                    turns += 1;
                } else if first_prompt.is_none() {
                    turns = 1;
                }
                if first_prompt.is_none() {
                    first_prompt = Some(clean);
                }
            }
        }
    }
    (first_prompt, model, turns, first_ts, mt)
}

fn lookup_agy_db_title(home: &Path, cid: &str) -> Option<String> {
    let db = home.join(".gemini/antigravity-cli/conversation_summaries.db");
    let rows =
        query_sqlite_json(&db, "SELECT title FROM conversation_summaries WHERE conversation_id = ? LIMIT 1", &[cid])?;
    rows.first()
        .and_then(|r| r.get("title"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn agy_log_mentions_workspace(path: &Path, targets: &[String]) -> bool {
    let Ok(file) = File::open(path) else { return false };
    for line in BufReader::new(file).lines().take(30).flatten() {
        for t in targets {
            if line.contains(t.as_str()) {
                return true;
            }
        }
    }
    false
}

fn discover_agy_sessions(
    home: &Path,
    state_dir: &Path,
    targets: &[String],
    active_sid: Option<&str>,
) -> Vec<CliSessionSummary> {
    let agy_root = home.join(".gemini/antigravity-cli");
    let brain = agy_root.join("brain");
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    // 1. Authoritative workspace index in conversation_summaries.db.
    let db = agy_root.join("conversation_summaries.db");
    if let Some(rows) = query_sqlite_json(
        &db,
        "SELECT conversation_id, title, preview, step_count, last_modified_time, workspace_uris \
         FROM conversation_summaries \
         WHERE nesting_depth = 0 AND (parent_conversation_id = '' OR parent_conversation_id IS NULL) AND killed = 0 \
         ORDER BY last_modified_time DESC LIMIT 300",
        &[],
    ) {
        for r in rows {
            let Some(cid) =
                r.get("conversation_id").and_then(Value::as_str).map(str::trim).filter(|s| is_valid_agy_cid(s))
            else {
                continue;
            };
            let uris = r.get("workspace_uris").and_then(Value::as_str).unwrap_or("[]");
            if !workspace_uris_match(uris, targets) && active_sid != Some(cid) {
                continue;
            }
            let logs = brain.join(cid).join(".system_generated/logs");
            let Some(log_path) = pick_agy_log_file(&logs) else {
                continue;
            };
            let (first_prompt, model, log_turns, first_ts, mt) = summarize_agy_log(&log_path);
            let db_title = r
                .get("title")
                .and_then(Value::as_str)
                .map(|s| clean_single_line_title(s, 100))
                .filter(|s| !s.is_empty());
            let db_preview = r
                .get("preview")
                .and_then(Value::as_str)
                .map(|s| clean_single_line_title(&strip_user_request_wrapper(s), 120))
                .filter(|s| !s.is_empty());
            let title = db_title.or_else(|| first_prompt.clone()).or_else(|| db_preview.clone());
            let is_active = active_sid == Some(cid);
            if title.is_none() && !is_active {
                continue;
            }
            let final_title = title.unwrap_or_else(|| cid.to_string());
            let preview = first_prompt.or(db_preview).filter(|p| p != &final_title);
            seen.insert(cid.to_string());
            out.push(CliSessionSummary {
                resume_command: build_full_access_resume_command("agy", cid),
                session_id: cid.to_string(),
                agent: "agy".into(),
                title: final_title,
                preview,
                turns: log_turns,
                created_ms: if first_ts > 0 { first_ts } else { mt },
                updated_ms: mt.max(first_ts),
                model,
                git_branch: None,
                current: is_active,
            });
        }
    }

    // 2. Supplement from Herdr2 archived checkpoints + pure-Rust brain scan when needed.
    for arch in discover_archived_checkpoint_sessions(state_dir, "agy", targets, active_sid) {
        if seen.insert(arch.session_id.clone()) {
            out.push(arch);
        }
    }

    if out.is_empty()
        && brain.is_dir()
        && let Ok(dirs) = fs::read_dir(&brain)
    {
        for dir in dirs.flatten() {
            let logs = dir.path().join(".system_generated/logs");
            let Some(log_path) = pick_agy_log_file(&logs) else { continue };
            let Some(cid) = agy_cid_from_path(&log_path) else { continue };
            if seen.contains(&cid) {
                continue;
            }
            let is_active = active_sid == Some(cid.as_str());
            if !is_active && !agy_log_mentions_workspace(&log_path, targets) {
                continue;
            }
            let (first_prompt, model, turns, first_ts, mt) = summarize_agy_log(&log_path);
            if first_prompt.is_none() && !is_active {
                continue;
            }
            let final_title = first_prompt.unwrap_or_else(|| cid.clone());
            seen.insert(cid.clone());
            out.push(CliSessionSummary {
                resume_command: build_full_access_resume_command("agy", &cid),
                session_id: cid,
                agent: "agy".into(),
                title: final_title,
                preview: None,
                turns,
                created_ms: if first_ts > 0 { first_ts } else { mt },
                updated_ms: mt.max(first_ts),
                model,
                git_branch: None,
                current: is_active,
            });
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Cursor (`cursor-agent`)
// ---------------------------------------------------------------------------

fn summarize_cursor_file(path: &Path, active_sid: Option<&str>) -> Option<CliSessionSummary> {
    let sid = path.file_stem()?.to_str()?.trim().to_string();
    if sid.is_empty() {
        return None;
    }
    let mt = mtime_ms(path);
    let ct = created_ms(path);
    let (head, tail, full_scan) = read_head_and_tail_lines(path);
    let mut first_prompt = None;
    let mut turns = 0u64;
    for line in head.iter().chain(tail.iter()) {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
        if v.get("role").and_then(Value::as_str) == Some("user")
            && let Some(content) = v.pointer("/message/content")
        {
            let text = match content {
                Value::String(s) => clean_cursor_user_text(s),
                Value::Array(arr) => arr
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .map(clean_cursor_user_text)
                    .find(|s| !s.is_empty())
                    .unwrap_or_default(),
                _ => String::new(),
            };
            let clean = clean_single_line_title(&text, 120);
            if !clean.is_empty() {
                if full_scan {
                    turns += 1;
                } else if first_prompt.is_none() {
                    turns = 1;
                }
                if first_prompt.is_none() {
                    first_prompt = Some(clean);
                }
            }
        }
    }
    let is_active = active_sid == Some(sid.as_str());
    if first_prompt.is_none() && !is_active {
        return None;
    }
    let title = first_prompt.unwrap_or_else(|| sid.clone());
    Some(CliSessionSummary {
        resume_command: build_full_access_resume_command("cursor", &sid),
        session_id: sid,
        agent: "cursor".into(),
        title,
        preview: None,
        turns,
        created_ms: if ct > 0 { ct } else { mt },
        updated_ms: mt.max(ct),
        model: None,
        git_branch: None,
        current: is_active,
    })
}

fn discover_cursor_sessions(home: &Path, targets: &[String], active_sid: Option<&str>) -> Vec<CliSessionSummary> {
    let projects_dir = home.join(".cursor").join("projects");
    if !projects_dir.is_dir() {
        return Vec::new();
    }
    let mut slug_dirs = Vec::new();
    for t in targets {
        let slug = cursor_project_slug(t);
        if slug.is_empty() {
            continue;
        }
        let direct = projects_dir.join(&slug);
        if direct.is_dir() {
            if !slug_dirs.contains(&direct) {
                slug_dirs.push(direct);
            }
        } else if let Ok(entries) = fs::read_dir(&projects_dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir()
                    && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case(&slug))
                    && !slug_dirs.contains(&p)
                {
                    slug_dirs.push(p);
                }
            }
        }
    }
    let mut out = Vec::new();
    for slug_dir in slug_dirs {
        let transcripts_dir = slug_dir.join("agent-transcripts");
        let Ok(entries) = fs::read_dir(&transcripts_dir) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            let jsonl = if p.is_dir() {
                let Some(name) = p.file_name().and_then(|s| s.to_str()) else { continue };
                let nested = p.join(format!("{name}.jsonl"));
                if nested.is_file() {
                    nested
                } else {
                    let Some(found) = fs::read_dir(&p)
                        .ok()
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|e| e.path())
                        .find(|f| f.is_file() && f.extension().and_then(|s| s.to_str()) == Some("jsonl"))
                    else {
                        continue;
                    };
                    found
                }
            } else if p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                p
            } else {
                continue;
            };
            if let Some(s) = summarize_cursor_file(&jsonl, active_sid) {
                out.push(s);
            }
        }
    }
    out
}

fn locate_cursor_file(home: &Path, cwd: Option<&str>, session_id: &str) -> Option<PathBuf> {
    let projects_dir = home.join(".cursor").join("projects");
    let mut search_dirs = Vec::new();
    if let Some(c) = cwd {
        for t in workspace_candidates(c) {
            let slug = cursor_project_slug(&t);
            if !slug.is_empty() {
                search_dirs.push(projects_dir.join(slug).join("agent-transcripts"));
            }
        }
    }
    if let Ok(entries) = fs::read_dir(&projects_dir) {
        for e in entries.flatten() {
            let td = e.path().join("agent-transcripts");
            if td.is_dir() && !search_dirs.contains(&td) {
                search_dirs.push(td);
            }
        }
    }
    for td in search_dirs {
        let nested = td.join(session_id).join(format!("{session_id}.jsonl"));
        if nested.is_file() {
            return Some(nested);
        }
        let flat = td.join(format!("{session_id}.jsonl"));
        if flat.is_file() {
            return Some(flat);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// ZCode & OpenCode (SQLite session + message + part)
// ---------------------------------------------------------------------------

fn zcode_db_path(home: &Path) -> PathBuf {
    let base = std::env::var("ZCODE_DATA_BASE_DIR")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map_or_else(|| home.to_path_buf(), PathBuf::from);
    base.join(".zcode/cli/db/db.sqlite")
}

fn opencode_db_path(home: &Path) -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|s| !s.is_empty()) {
        let p = PathBuf::from(xdg).join("opencode/opencode.db");
        if p.is_file() {
            return p;
        }
    }
    if cfg!(windows)
        && let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|s| !s.is_empty())
    {
        let p = PathBuf::from(local).join("opencode/opencode.db");
        if p.is_file() {
            return p;
        }
    }
    home.join(".local/share/opencode/opencode.db")
}

fn discover_zcode_sessions(
    home: &Path,
    state_dir: &Path,
    targets: &[String],
    active_sid: Option<&str>,
) -> Vec<CliSessionSummary> {
    let db = zcode_db_path(home);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    if let Some(rows) = query_sqlite_json(
        &db,
        "SELECT id, directory, title, time_created, time_updated \
         FROM session WHERE parent_id IS NULL AND time_deleted IS NULL \
         ORDER BY time_updated DESC LIMIT 200",
        &[],
    ) {
        for r in rows {
            let Some(sid) = r.get("id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
                continue;
            };
            let dir = r.get("directory").and_then(Value::as_str).unwrap_or("");
            let is_active = active_sid == Some(sid);
            if !matches_any_workspace(dir, targets) && !is_active {
                continue;
            }
            let title = r
                .get("title")
                .and_then(Value::as_str)
                .map(|s| clean_single_line_title(s, 100))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| sid.to_string());
            let created = r.get("time_created").and_then(Value::as_u64).unwrap_or(0);
            let updated = r.get("time_updated").and_then(Value::as_u64).unwrap_or(created);
            seen.insert(sid.to_string());
            out.push(CliSessionSummary {
                resume_command: build_full_access_resume_command("zcode", sid),
                session_id: sid.to_string(),
                agent: "zcode".into(),
                title,
                preview: None,
                turns: 0,
                created_ms: created,
                updated_ms: updated,
                model: None,
                git_branch: None,
                current: is_active,
            });
        }
    }
    for arch in discover_archived_checkpoint_sessions(state_dir, "zcode", targets, active_sid) {
        if seen.insert(arch.session_id.clone()) {
            out.push(arch);
        }
    }
    out
}

fn discover_opencode_sessions(home: &Path, targets: &[String], active_sid: Option<&str>) -> Vec<CliSessionSummary> {
    let db = opencode_db_path(home);
    let rows = query_sqlite_json(
        &db,
        "SELECT id, directory, title, model, time_created, time_updated \
         FROM session_v2 WHERE parent_id IS NULL \
         ORDER BY time_updated DESC LIMIT 200",
        &[],
    )
    .or_else(|| {
        query_sqlite_json(
            &db,
            "SELECT id, directory, title, NULL as model, time_created, time_updated \
             FROM session WHERE parent_id IS NULL \
             ORDER BY time_updated DESC LIMIT 200",
            &[],
        )
    })
    .unwrap_or_default();

    let mut out = Vec::new();
    for r in rows {
        let Some(sid) = r.get("id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let dir = r.get("directory").and_then(Value::as_str).unwrap_or("");
        let is_active = active_sid == Some(sid);
        if !matches_any_workspace(dir, targets) && !is_active {
            continue;
        }
        let title = r
            .get("title")
            .and_then(Value::as_str)
            .map(|s| clean_single_line_title(s, 100))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| sid.to_string());
        let model = r.get("model").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).map(str::to_string);
        let created = r.get("time_created").and_then(Value::as_u64).unwrap_or(0);
        let updated = r.get("time_updated").and_then(Value::as_u64).unwrap_or(created);
        out.push(CliSessionSummary {
            resume_command: build_full_access_resume_command("opencode", sid),
            session_id: sid.to_string(),
            agent: "opencode".into(),
            title,
            preview: None,
            turns: 0,
            created_ms: created,
            updated_ms: updated,
            model,
            git_branch: None,
            current: is_active,
        });
    }
    out
}

fn parse_message_part_rows(msg_rows: &[Value], part_rows: &[Value]) -> (Vec<Entry>, Option<String>) {
    let mut parts_by_msg: HashMap<String, Vec<&Value>> = HashMap::new();
    for p in part_rows {
        if let Some(mid) = p.get("message_id").and_then(Value::as_str) {
            parts_by_msg.entry(mid.to_string()).or_default().push(p);
        }
    }
    let mut entries = Vec::new();
    let mut model = None;
    let mut turn = 0u64;

    for m in msg_rows {
        let mid = m.get("id").and_then(Value::as_str).unwrap_or("");
        let ts_ms = m.get("time_created").and_then(Value::as_u64).unwrap_or(0);
        let Ok(mdata) = serde_json::from_str::<Value>(m.get("data").and_then(Value::as_str).unwrap_or("{}")) else {
            continue;
        };
        if mdata.get("synthetic").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if let Some(mod_str) = mdata
            .get("modelID")
            .or_else(|| mdata.pointer("/model/modelID"))
            .or_else(|| mdata.get("model"))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            model = Some(mod_str.to_string());
        }
        let role =
            mdata.get("role").and_then(Value::as_str).or_else(|| m.get("type").and_then(Value::as_str)).unwrap_or("");
        let mparts = parts_by_msg.get(mid).cloned().unwrap_or_default();

        if role == "user" {
            let mut texts = Vec::new();
            for pr in mparts {
                let Ok(pdata) = serde_json::from_str::<Value>(pr.get("data").and_then(Value::as_str).unwrap_or("{}"))
                else {
                    continue;
                };
                if pdata.get("synthetic").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                if pdata.get("type").and_then(Value::as_str).or_else(|| pr.get("type").and_then(Value::as_str))
                    == Some("text")
                    && let Some(t) = pdata.get("text").and_then(Value::as_str)
                {
                    let clean = strip_h2_envelope(t);
                    if !clean.is_empty() {
                        texts.push(clean);
                    }
                }
            }
            if !texts.is_empty() {
                turn += 1;
                entries.push(Entry::User { turn, from: None, text: texts.join("\n\n"), attachments: vec![], ts_ms });
            }
        } else if role == "assistant" {
            for pr in mparts {
                let Ok(pdata) = serde_json::from_str::<Value>(pr.get("data").and_then(Value::as_str).unwrap_or("{}"))
                else {
                    continue;
                };
                let ptype = pdata
                    .get("type")
                    .and_then(Value::as_str)
                    .or_else(|| pr.get("type").and_then(Value::as_str))
                    .unwrap_or("");
                match ptype {
                    "reasoning" | "thinking" => {
                        if let Some(t) =
                            pdata.get("text").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
                        {
                            entries.push(Entry::Thought { turn, text: t.to_string(), ts_ms });
                        }
                    }
                    "tool" | "tool-invocation" => {
                        let tool = pdata.get("tool").and_then(Value::as_str).unwrap_or("tool");
                        let state = pdata.pointer("/state/status").and_then(Value::as_str).unwrap_or("completed");
                        let status = if matches!(state, "error" | "failed") { "failed" } else { "completed" };
                        let title = pdata
                            .pointer("/state/title")
                            .and_then(Value::as_str)
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or(tool)
                            .to_string();
                        let detail = pdata.pointer("/state/output").and_then(Value::as_str).and_then(cap_tool_detail);
                        let call_id = pdata.get("callID").and_then(Value::as_str).unwrap_or("").to_string();
                        entries.push(Entry::ToolCall {
                            turn,
                            id: call_id,
                            title,
                            tool_kind: None,
                            status: status.into(),
                            detail,
                            attachments: vec![],
                            ts_ms,
                        });
                    }
                    "text" => {
                        if let Some(t) =
                            pdata.get("text").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
                        {
                            entries.push(Entry::Assistant { turn, text: t.to_string(), attachments: vec![], ts_ms });
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    (entries, model)
}

type ParsedTranscriptResult = Result<(Vec<Entry>, Option<String>, Option<String>, u64, String), String>;

fn read_zcode_transcript(home: &Path, state_dir: &Path, sid: &str) -> ParsedTranscriptResult {
    let db = zcode_db_path(home);
    if let Some(sess_rows) =
        query_sqlite_json(&db, "SELECT title, time_updated FROM session WHERE id = ? LIMIT 1", &[sid])
        && let Some(srow) = sess_rows.first()
    {
        let title = srow.get("title").and_then(Value::as_str).map(str::to_string);
        let updated = srow.get("time_updated").and_then(Value::as_u64).unwrap_or(0);
        let msgs = query_sqlite_json(
            &db,
            "SELECT id, time_created, data FROM message WHERE session_id = ? ORDER BY time_created ASC, id ASC LIMIT 800",
            &[sid],
        )
        .unwrap_or_default();
        let parts = query_sqlite_json(
            &db,
            "SELECT message_id, time_created, data FROM part WHERE session_id = ? ORDER BY time_created ASC, id ASC LIMIT 2400",
            &[sid],
        )
        .unwrap_or_default();
        let (entries, model) = parse_message_part_rows(&msgs, &parts);
        return Ok((entries, title, model, updated, sid.to_string()));
    }
    read_archived_checkpoint_session(state_dir, "zcode", sid)
}

fn read_opencode_transcript(home: &Path, sid: &str) -> ParsedTranscriptResult {
    let db = opencode_db_path(home);
    if let Some(sess_rows) =
        query_sqlite_json(&db, "SELECT title, model, time_updated FROM session_v2 WHERE id = ? LIMIT 1", &[sid])
        && let Some(srow) = sess_rows.first()
    {
        let title = srow.get("title").and_then(Value::as_str).map(str::to_string);
        let s_model = srow.get("model").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
        let updated = srow.get("time_updated").and_then(Value::as_u64).unwrap_or(0);
        let msgs = query_sqlite_json(
            &db,
            "SELECT id, type, time_created, data FROM session_message WHERE session_id = ? ORDER BY seq ASC, time_created ASC LIMIT 800",
            &[sid],
        )
        .unwrap_or_default();
        let parts = query_sqlite_json(
            &db,
            "SELECT message_id, type, time_created, data FROM session_part WHERE session_id = ? ORDER BY ordinal ASC, time_created ASC LIMIT 2400",
            &[sid],
        )
        .unwrap_or_default();
        let (entries, parsed_model) = parse_message_part_rows(&msgs, &parts);
        return Ok((entries, title, s_model.or(parsed_model), updated, sid.to_string()));
    }
    if let Some(sess_rows) =
        query_sqlite_json(&db, "SELECT title, time_updated FROM session WHERE id = ? LIMIT 1", &[sid])
        && let Some(srow) = sess_rows.first()
    {
        let title = srow.get("title").and_then(Value::as_str).map(str::to_string);
        let updated = srow.get("time_updated").and_then(Value::as_u64).unwrap_or(0);
        let msgs = query_sqlite_json(
            &db,
            "SELECT id, time_created, data FROM message WHERE session_id = ? ORDER BY time_created ASC, id ASC LIMIT 800",
            &[sid],
        )
        .unwrap_or_default();
        let parts = query_sqlite_json(
            &db,
            "SELECT message_id, time_created, data FROM part WHERE session_id = ? ORDER BY time_created ASC, id ASC LIMIT 2400",
            &[sid],
        )
        .unwrap_or_default();
        let (entries, model) = parse_message_part_rows(&msgs, &parts);
        return Ok((entries, title, model, updated, sid.to_string()));
    }
    Err(format!("OpenCode session `{sid}` not found"))
}

// ---------------------------------------------------------------------------
// Qwen Code & Gemini CLI
// ---------------------------------------------------------------------------

fn discover_qwen_or_gemini_sessions(
    home: &Path,
    kind: &str,
    targets: &[String],
    active_sid: Option<&str>,
) -> Vec<CliSessionSummary> {
    let dot_dir = if kind == "qwen" { ".qwen" } else { ".gemini" };
    let tmp_dir = home.join(dot_dir).join("tmp");
    let mut out = Vec::new();
    let hashes: HashSet<String> = targets.iter().map(|t| sha256_hex(t.as_bytes())).collect();

    if tmp_dir.is_dir()
        && let Ok(project_dirs) = fs::read_dir(&tmp_dir)
    {
        for entry in project_dirs.flatten() {
            let proj = entry.path();
            if !proj.is_dir() {
                continue;
            }
            let dir_name = proj.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let root_matches =
                fs::read_to_string(proj.join(".project_root")).ok().is_some_and(|s| matches_any_workspace(&s, targets));
            let hash_matches = hashes.contains(dir_name);
            let chats = proj.join("chats");
            let Ok(chat_files) = fs::read_dir(&chats) else { continue };
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
                let Ok(raw) = fs::read_to_string(&path) else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&raw) else { continue };
                let json_hash = v.get("projectHash").and_then(Value::as_str).unwrap_or("");
                if !(root_matches || hash_matches || hashes.contains(json_hash)) {
                    continue;
                }
                let sid = v
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_string));
                let Some(sid) = sid else { continue };
                let (entries, meta) = parse_gemini_json(&path);
                let turns = entries.iter().filter_map(Entry::turn).max().unwrap_or(0);
                let first_prompt = entries.iter().find_map(|e| match e {
                    Entry::User { text, .. } => {
                        let c = clean_single_line_title(text, 120);
                        (!c.is_empty()).then_some(c)
                    }
                    _ => None,
                });
                let is_active = active_sid == Some(sid.as_str());
                if first_prompt.is_none() && !is_active {
                    continue;
                }
                out.push(CliSessionSummary {
                    resume_command: build_full_access_resume_command(kind, &sid),
                    session_id: sid.clone(),
                    agent: kind.into(),
                    title: first_prompt.unwrap_or(sid),
                    preview: None,
                    turns,
                    created_ms: mt,
                    updated_ms: mt,
                    model: meta.model,
                    git_branch: None,
                    current: is_active,
                });
            }
        }
    }

    if kind == "qwen" {
        for t in targets {
            let slug_chats = home.join(".qwen/projects").join(claude_slug(t)).join("chats");
            let Ok(files) = fs::read_dir(&slug_chats) else { continue };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|s| s.to_str()) == Some("jsonl")
                    && path.is_file()
                    && let Some(mut s) = summarize_claude_file(&path, targets, false, active_sid)
                {
                    s.agent = "qwen".into();
                    s.resume_command = build_full_access_resume_command("qwen", &s.session_id);
                    out.push(s);
                }
            }
        }
    }
    out
}

fn locate_qwen_or_gemini_file(home: &Path, kind: &str, cwd: Option<&str>, session_id: &str) -> Option<PathBuf> {
    if kind == "qwen"
        && let Some(c) = cwd
    {
        for t in workspace_candidates(c) {
            let jl =
                home.join(".qwen/projects").join(claude_slug(&t)).join("chats").join(format!("{session_id}.jsonl"));
            if jl.is_file() {
                return Some(jl);
            }
        }
    }
    let dot_dir = if kind == "qwen" { ".qwen" } else { ".gemini" };
    let tmp_dir = home.join(dot_dir).join("tmp");
    let entries = fs::read_dir(&tmp_dir).ok()?;
    for entry in entries.flatten() {
        let chats = entry.path().join("chats");
        let Ok(files) = fs::read_dir(&chats) else { continue };
        for cf in files.flatten() {
            let path = cf.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") || !path.is_file() {
                continue;
            }
            if path.file_stem().and_then(|s| s.to_str()) == Some(session_id) {
                return Some(path);
            }
            if let Ok(raw) = fs::read_to_string(&path)
                && let Ok(v) = serde_json::from_str::<Value>(&raw)
                && v.get("sessionId").and_then(Value::as_str) == Some(session_id)
            {
                return Some(path);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Pi & oh-my-pi (`omp`)
// ---------------------------------------------------------------------------

fn discover_pi_or_omp_sessions(
    home: &Path,
    kind: &str,
    targets: &[String],
    active_sid: Option<&str>,
) -> Vec<CliSessionSummary> {
    let rel = if kind == "omp" { ".omp/agent/sessions" } else { ".pi/agent/sessions" };
    let base = home.join(rel);
    if !base.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let Ok(dirs) = fs::read_dir(&base) else { return out };
    for dir in dirs.flatten() {
        let pdir = dir.path();
        let Ok(files) = fs::read_dir(&pdir) else { continue };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") || !path.is_file() {
                continue;
            }
            let Ok(f) = File::open(&path) else { continue };
            let mut first_line = String::new();
            if BufReader::new(f).read_line(&mut first_line).is_err() {
                continue;
            }
            let Ok(hdr) = serde_json::from_str::<Value>(first_line.trim()) else { continue };
            if hdr.get("type").and_then(Value::as_str) != Some("session") {
                continue;
            }
            let Some(scwd) = hdr.get("cwd").and_then(Value::as_str) else { continue };
            if !matches_any_workspace(scwd, targets) {
                continue;
            }
            let mt = mtime_ms(&path);
            let sid = hdr
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_string));
            let Some(sid) = sid else { continue };
            let (entries, meta) = parse_pi_jsonl(&path);
            let turns = entries.iter().filter_map(Entry::turn).max().unwrap_or(0);
            let first_prompt = entries.iter().find_map(|e| match e {
                Entry::User { text, .. } => {
                    let c = clean_single_line_title(text, 120);
                    (!c.is_empty()).then_some(c)
                }
                _ => None,
            });
            let is_active = active_sid == Some(sid.as_str());
            if first_prompt.is_none() && !is_active {
                continue;
            }
            let path_str = path.to_string_lossy().to_string();
            out.push(CliSessionSummary {
                resume_command: build_full_access_resume_command(kind, &path_str),
                session_id: sid.clone(),
                agent: kind.into(),
                title: first_prompt.unwrap_or(sid),
                preview: None,
                turns,
                created_ms: mt,
                updated_ms: mt,
                model: meta.model,
                git_branch: None,
                current: is_active,
            });
        }
    }
    out
}

fn locate_pi_or_omp_file(home: &Path, kind: &str, _cwd: Option<&str>, session_id: &str) -> Option<PathBuf> {
    let as_path = Path::new(session_id);
    if as_path.is_file() {
        return Some(as_path.to_path_buf());
    }
    let rel = if kind == "omp" { ".omp/agent/sessions" } else { ".pi/agent/sessions" };
    let base = home.join(rel);
    let dirs = fs::read_dir(&base).ok()?;
    for dir in dirs.flatten() {
        let Ok(files) = fs::read_dir(dir.path()) else { continue };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") || !path.is_file() {
                continue;
            }
            if path.file_stem().and_then(|s| s.to_str()) == Some(session_id) {
                return Some(path);
            }
            if let Ok(f) = File::open(&path) {
                let mut line = String::new();
                if BufReader::new(f).read_line(&mut line).is_ok()
                    && let Ok(v) = serde_json::from_str::<Value>(line.trim())
                    && v.get("id").and_then(Value::as_str) == Some(session_id)
                {
                    return Some(path);
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Hermes (`~/.hermes/state.db`)
// ---------------------------------------------------------------------------

fn discover_hermes_sessions(home: &Path, targets: &[String], active_sid: Option<&str>) -> Vec<CliSessionSummary> {
    let db = home.join(".hermes/state.db");
    let Some(rows) = query_sqlite_json(
        &db,
        "SELECT id, cwd, git_repo_root, git_branch, model, title, display_name, started_at, last_activity_at, message_count \
         FROM sessions WHERE parent_session_id IS NULL \
         ORDER BY COALESCE(last_activity_at, started_at) DESC LIMIT 200",
        &[],
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for r in rows {
        let Some(sid) = r.get("id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let scwd = r.get("cwd").and_then(Value::as_str).unwrap_or("");
        let gcwd = r.get("git_repo_root").and_then(Value::as_str).unwrap_or("");
        let is_active = active_sid == Some(sid);
        if !matches_any_workspace(scwd, targets) && !matches_any_workspace(gcwd, targets) && !is_active {
            continue;
        }
        let title = r
            .get("title")
            .or_else(|| r.get("display_name"))
            .and_then(Value::as_str)
            .map(|s| clean_single_line_title(s, 100))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| sid.to_string());
        let created = r.get("started_at").and_then(Value::as_f64).map_or(0, |s| (s * 1000.0) as u64);
        let updated =
            r.get("last_activity_at").and_then(Value::as_f64).map_or(created, |s| (s * 1000.0) as u64).max(created);
        let msg_count = r.get("message_count").and_then(Value::as_u64).unwrap_or(0);
        if msg_count == 0 && !is_active {
            continue;
        }
        out.push(CliSessionSummary {
            resume_command: build_full_access_resume_command("hermes", sid),
            session_id: sid.to_string(),
            agent: "hermes".into(),
            title,
            preview: None,
            turns: msg_count / 2,
            created_ms: created,
            updated_ms: updated,
            model: r.get("model").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
            git_branch: r.get("git_branch").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
            current: is_active,
        });
    }
    out
}

fn read_hermes_transcript(home: &Path, sid: &str) -> ParsedTranscriptResult {
    let db = home.join(".hermes/state.db");
    let sess_rows = query_sqlite_json(
        &db,
        "SELECT title, display_name, model, started_at, last_activity_at FROM sessions WHERE id = ? LIMIT 1",
        &[sid],
    )
    .ok_or_else(|| format!("Hermes session `{sid}` not found"))?;
    let srow = sess_rows.first().ok_or_else(|| format!("Hermes session `{sid}` not found"))?;
    let title = srow.get("title").or_else(|| srow.get("display_name")).and_then(Value::as_str).map(str::to_string);
    let model = srow.get("model").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    let updated = srow
        .get("last_activity_at")
        .or_else(|| srow.get("started_at"))
        .and_then(Value::as_f64)
        .map_or(0, |s| (s * 1000.0) as u64);
    let msgs = query_sqlite_json(
        &db,
        "SELECT role, content, reasoning, timestamp FROM messages WHERE session_id = ? AND active = 1 ORDER BY id ASC LIMIT 800",
        &[sid],
    )
    .unwrap_or_default();
    let mut entries = Vec::new();
    let mut turn = 0u64;
    for m in msgs {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("");
        let ts_ms = m.get("timestamp").and_then(Value::as_f64).map_or(0, |s| (s * 1000.0) as u64);
        let text = m.get("content").and_then(Value::as_str).map(str::trim).unwrap_or("");
        if role == "user" && !text.is_empty() {
            turn += 1;
            entries.push(Entry::User { turn, from: None, text: text.to_string(), attachments: vec![], ts_ms });
        } else if role == "assistant" {
            if let Some(th) = m.get("reasoning").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
                entries.push(Entry::Thought { turn, text: th.to_string(), ts_ms });
            }
            if !text.is_empty() {
                entries.push(Entry::Assistant { turn, text: text.to_string(), attachments: vec![], ts_ms });
            }
        }
    }
    Ok((entries, title, model, updated, sid.to_string()))
}

// ---------------------------------------------------------------------------
// Archived Herdr2 Checkpoint Fallback (for ZCode / Agy / custom CLIs)
// ---------------------------------------------------------------------------

fn discover_archived_checkpoint_sessions(
    state_dir: &Path,
    agent: &str,
    targets: &[String],
    active_sid: Option<&str>,
) -> Vec<CliSessionSummary> {
    let dir = state_dir.join("checkpoints").join("sessions").join(agent);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") || !path.is_file() {
            continue;
        }
        let Ok(tc) = crate::paths::read_state::<TargetCheckpoints>(&path) else { continue };
        let Some(sid) = tc.cli_session_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let is_active = active_sid == Some(sid);
        let matches_repo = tc.repo_root.as_deref().is_some_and(|r| matches_any_workspace(r, targets));
        if !matches_repo && !is_active {
            continue;
        }
        let first_prompt = tc
            .checkpoints
            .iter()
            .filter(|c| c.turn > 0)
            .find_map(|c| c.label.as_deref().or(c.prompt.as_deref()))
            .map(|s| clean_single_line_title(s, 100))
            .filter(|s| !s.is_empty());
        if first_prompt.is_none() && !is_active {
            continue;
        }
        let turns = tc.checkpoints.iter().map(|c| c.turn).max().unwrap_or(0);
        let created = tc.checkpoints.iter().map(|c| c.ts_ms).filter(|&t| t > 0).min().unwrap_or(0);
        let updated = tc.checkpoints.iter().map(|c| c.ts_ms).max().unwrap_or_else(|| mtime_ms(&path));
        out.push(CliSessionSummary {
            resume_command: build_full_access_resume_command(agent, sid),
            session_id: sid.to_string(),
            agent: agent.to_string(),
            title: first_prompt.unwrap_or_else(|| sid.to_string()),
            preview: None,
            turns,
            created_ms: created,
            updated_ms: updated,
            model: None,
            git_branch: None,
            current: is_active,
        });
    }
    out
}

fn read_archived_checkpoint_session(state_dir: &Path, agent: &str, sid: &str) -> ParsedTranscriptResult {
    let dir = state_dir.join("checkpoints").join("sessions").join(agent);
    if let Ok(files) = fs::read_dir(&dir) {
        for f in files.flatten() {
            let p = f.path();
            let Ok(tc) = crate::paths::read_state::<TargetCheckpoints>(&p) else { continue };
            if tc.cli_session_id.as_deref() != Some(sid) {
                continue;
            }
            let mut entries = Vec::new();
            let mut updated = 0u64;
            let mut title = None;
            for c in tc.checkpoints.iter().filter(|c| c.turn > 0) {
                updated = updated.max(c.ts_ms);
                if let Some(pr) = c.prompt.as_deref().or(c.label.as_deref()) {
                    if title.is_none() {
                        title = Some(clean_single_line_title(pr, 100));
                    }
                    entries.push(Entry::User {
                        turn: c.turn,
                        from: None,
                        text: strip_h2_envelope(pr),
                        attachments: vec![],
                        ts_ms: c.ts_ms,
                    });
                }
            }
            return Ok((entries, title, None, updated, sid.to_string()));
        }
    }
    Err(format!("{} session `{sid}` not found", cli_agent_display_name(agent)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn full_access_resume_commands_carry_permission_bypass_flags_for_all_cli_agents() {
        assert_eq!(
            build_full_access_resume_command("claude", "550e8400-e29b-41d4-a716-446655440000"),
            "claude --dangerously-skip-permissions --resume 550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(
            build_full_access_resume_command("codex", "01a190c3-06dc-71f3-a3bb-09e1e6faafe8"),
            "codex resume --dangerously-bypass-approvals-and-sandbox 01a190c3-06dc-71f3-a3bb-09e1e6faafe8"
        );
        assert_eq!(
            build_full_access_resume_command("antigravity", "72a963a2-5709-4cfe-b7a8-4b6fa74396ed"),
            "agy --dangerously-skip-permissions --conversation 72a963a2-5709-4cfe-b7a8-4b6fa74396ed"
        );
        assert_eq!(
            build_full_access_resume_command("cursor-agent", "chat-123"),
            "cursor-agent --yolo --trust --resume chat-123"
        );
        assert_eq!(build_full_access_resume_command("zcode", "sess_abc123"), "zcode --mode yolo --resume sess_abc123");
        assert_eq!(build_full_access_resume_command("opencode", "ses_xyz"), "opencode --auto --session ses_xyz");
        assert_eq!(build_full_access_resume_command("qwen", "qwen-sid-1"), "qwen --yolo --resume qwen-sid-1");
        assert_eq!(build_full_access_resume_command("gemini", "gem-sid-1"), "gemini --yolo --resume gem-sid-1");
        assert_eq!(
            build_full_access_resume_command("hermes", "hermes-sid-1"),
            "hermes --yolo --accept-hooks --resume hermes-sid-1"
        );
    }

    #[test]
    fn discovers_and_reads_workspace_scoped_claude_codex_and_cursor_sessions() {
        let root = crate::test_tmp().join(format!("h2-cli-sess-{}", crate::platform::random_hex()));
        let home = root.join("home");
        let state = root.join("state");
        let ws_a = root.join("ws-alpha");
        let ws_b = root.join("ws-beta");
        fs::create_dir_all(&ws_a).unwrap();
        fs::create_dir_all(&ws_b).unwrap();
        let cwd_a = normalize_cwd(&ws_a.to_string_lossy());
        let cwd_b = normalize_cwd(&ws_b.to_string_lossy());

        // 1. Claude sessions: ws_a has 2 real sessions + 1 sidechain + 1 subagent; ws_b has 1 session.
        let claude_a = home.join(".claude/projects").join(claude_slug(&cwd_a));
        let claude_b = home.join(".claude/projects").join(claude_slug(&cwd_b));
        fs::create_dir_all(&claude_a).unwrap();
        fs::create_dir_all(&claude_b).unwrap();

        fs::write(
            claude_a.join("sid-alpha-1.jsonl"),
            format!(
                "{}\n{}\n",
                json!({"type":"user","sessionId":"sid-alpha-1","cwd":cwd_a,"timestamp":"2026-11-28T06:00:00Z","message":{"role":"user","content":"Implement auth middleware"}}),
                json!({"type":"assistant","sessionId":"sid-alpha-1","timestamp":"2026-11-28T06:00:05Z","message":{"model":"claude-opus-4-6","content":[{"type":"text","text":"Added auth middleware and tests."}]}}),
            ),
        )
        .unwrap();
        fs::write(
            claude_a.join("sid-alpha-2.jsonl"),
            format!(
                "{}\n{}\n{}\n",
                json!({"type":"user","sessionId":"sid-alpha-2","cwd":cwd_a,"gitBranch":"feat/sessions","timestamp":"2026-11-28T07:00:00Z","message":{"role":"user","content":"Fix token refresh race"}}),
                json!({"type":"assistant","sessionId":"sid-alpha-2","timestamp":"2026-11-28T07:00:10Z","message":{"model":"claude-sonnet-4-6","content":[{"type":"text","text":"Fixed mutex lock ordering."}]}}),
                json!({"type":"custom-title","customTitle":"Token refresh race fix"}),
            ),
        )
        .unwrap();
        fs::write(
            claude_a.join("agent-sub123.jsonl"),
            format!(
                "{}\n",
                json!({"type":"user","sessionId":"agent-sub123","cwd":cwd_a,"message":{"role":"user","content":"subagent noise"}})
            ),
        )
        .unwrap();
        fs::write(
            claude_b.join("sid-beta-1.jsonl"),
            format!(
                "{}\n",
                json!({"type":"user","sessionId":"sid-beta-1","cwd":cwd_b,"message":{"role":"user","content":"Unrelated beta repo task"}})
            ),
        )
        .unwrap();

        let listed_claude = list_cli_sessions_in(&home, &state, &cwd_a, "claude", Some("sid-alpha-1"), None);
        assert_eq!(listed_claude.agent, "claude");
        assert_eq!(listed_claude.agent_name, "Claude Code");
        assert_eq!(listed_claude.sessions.len(), 2);
        // Active session bubbles first and is marked current.
        assert_eq!(listed_claude.sessions[0].session_id, "sid-alpha-1");
        assert!(listed_claude.sessions[0].current);
        assert_eq!(
            listed_claude.sessions[0].resume_command,
            "claude --dangerously-skip-permissions --resume sid-alpha-1"
        );
        assert_eq!(listed_claude.sessions[1].session_id, "sid-alpha-2");
        assert_eq!(listed_claude.sessions[1].title, "Token refresh race fix");
        assert_eq!(listed_claude.sessions[1].git_branch.as_deref(), Some("feat/sessions"));

        let read_claude = read_cli_session_in(&home, &state, Some(&cwd_a), "claude", "sid-alpha-2", None).unwrap();
        assert_eq!(read_claude.title, "Token refresh race fix");
        assert_eq!(read_claude.turns, 1);
        assert_eq!(read_claude.entries.len(), 2);
        assert_eq!(read_claude.resume_command, "claude --dangerously-skip-permissions --resume sid-alpha-2");

        // 2. Codex sessions with session_index.jsonl title & workspace filtering.
        let codex_day = home.join(".codex/sessions/2026/11/28");
        fs::create_dir_all(&codex_day).unwrap();
        fs::write(
            home.join(".codex/session_index.jsonl"),
            format!("{}\n", json!({"id":"codex-sid-a","thread_name":"Refactor DAG scheduler"})),
        )
        .unwrap();
        fs::write(
            codex_day.join("rollout-2026-11-28T10-00-00-codex-sid-a.jsonl"),
            format!(
                "{}\n{}\n{}\n",
                json!({"type":"session_meta","payload":{"id":"codex-sid-a","cwd":cwd_a,"model":"gpt-5.4","git":{"branch":"main"}}}),
                json!({"type":"event_msg","payload":{"type":"user_message","message":"Refactor the DAG scheduler queue"}}),
                json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Scheduler refactored cleanly."}]}}),
            ),
        )
        .unwrap();
        fs::write(
            codex_day.join("rollout-2026-11-28T10-05-00-codex-sid-b.jsonl"),
            format!(
                "{}\n{}\n",
                json!({"type":"session_meta","payload":{"id":"codex-sid-b","cwd":cwd_b}}),
                json!({"type":"event_msg","payload":{"type":"user_message","message":"Other workspace"}}),
            ),
        )
        .unwrap();

        let listed_codex = list_cli_sessions_in(&home, &state, &cwd_a, "codex", None, None);
        assert_eq!(listed_codex.sessions.len(), 1);
        assert_eq!(listed_codex.sessions[0].session_id, "codex-sid-a");
        assert_eq!(listed_codex.sessions[0].title, "Refactor DAG scheduler");
        assert_eq!(
            listed_codex.sessions[0].resume_command,
            "codex resume --dangerously-bypass-approvals-and-sandbox codex-sid-a"
        );
        let read_codex = read_cli_session_in(&home, &state, Some(&cwd_a), "codex", "codex-sid-a", None).unwrap();
        assert_eq!(read_codex.entries.len(), 2);

        // 3. Cursor sessions.
        let cursor_dir =
            home.join(".cursor/projects").join(cursor_project_slug(&cwd_a)).join("agent-transcripts/cur-sid-9");
        fs::create_dir_all(&cursor_dir).unwrap();
        fs::write(
            cursor_dir.join("cur-sid-9.jsonl"),
            format!(
                "{}\n{}\n",
                json!({"role":"user","message":{"content":"<user_query>Add dark mode toggle</user_query>"}}),
                json!({"role":"assistant","message":{"content":[{"type":"text","text":"Added theme toggle."}]}}),
            ),
        )
        .unwrap();
        let listed_cursor = list_cli_sessions_in(&home, &state, &cwd_a, "cursor", None, None);
        assert_eq!(listed_cursor.sessions.len(), 1);
        assert_eq!(listed_cursor.sessions[0].title, "Add dark mode toggle");
        assert_eq!(listed_cursor.sessions[0].resume_command, "cursor-agent --yolo --trust --resume cur-sid-9");

        let _ = fs::remove_dir_all(&root);
    }
}
