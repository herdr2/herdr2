use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::checkpoint::{capture_target_checkpoint, random_uuid_v4};
pub(crate) use crate::pty_transcript::{find_claude_session_file, find_codex_rollout_by_sid};
use crate::pty_transcript::{
    clean_claude_text, discover_cli_transcript, find_agy_transcript_by_cid, is_claude_user_noise, parse_cli_transcript,
    user_home_dir,
};
use crate::runtime::{Core, TargetKey};
use crate::transcript::{Entry, build_portable_handoff, strip_h2_envelope};

/// First-class interactive CLI backends supported in PTY terminals (`t*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PtyCliKind {
    Claude,
    Codex,
    Agy,
    Generic,
}

impl PtyCliKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Agy => "agy",
            Self::Generic => "generic",
        }
    }

    pub fn from_str_name(raw: &str) -> Self {
        match raw {
            "claude" => Self::Claude,
            "codex" => Self::Codex,
            "agy" => Self::Agy,
            _ => Self::Generic,
        }
    }

    fn detect_cmd_token(raw: &str) -> Self {
        match exe_stem(raw).as_str() {
            "claude" | "claude-code" => Self::Claude,
            "codex" => Self::Codex,
            "agy" => Self::Agy,
            _ => Self::Generic,
        }
    }

    /// Detects the CLI kind from `argv`, skipping `env VAR=val` and Herdr's POSIX login-shell wrapper.
    pub fn detect(argv: &[String]) -> Self {
        let Some(cmd_idx) = pty_command_offset(argv) else {
            return Self::Generic;
        };
        let kind = Self::detect_cmd_token(&argv[cmd_idx]);
        if kind != Self::Generic {
            return kind;
        }
        // Fallback for `sh -c "<inline command...>"` without separate `$@` arguments.
        if argv.len() == cmd_idx + 3
            && is_shell_stem(&exe_stem(&argv[cmd_idx]))
            && matches!(argv[cmd_idx + 1].as_str(), "-c" | "-lc" | "-lic" | "-ic")
        {
            for tok in argv[cmd_idx + 2].split_whitespace() {
                if tok == "exec" || (tok.contains('=') && !tok.starts_with('-')) {
                    continue;
                }
                return Self::detect_cmd_token(tok);
            }
        }
        Self::Generic
    }
}

fn is_shell_stem(stem: &str) -> bool {
    matches!(stem, "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "ash" | "fish")
}

/// Returns the index in `argv` where the actual program (and appendable CLI flags) start,
/// unwrapping leading `env VAR=val` and Herdr's `sh -c '... "$@" ...' sh <cmd> ...` wrapper.
pub fn pty_command_offset(argv: &[String]) -> Option<usize> {
    let mut idx = 0usize;
    if let Some(first) = argv.first()
        && exe_stem(first) == "env"
    {
        idx += 1;
        while idx < argv.len() && argv[idx].contains('=') && !argv[idx].starts_with('-') {
            idx += 1;
        }
    }
    if let Some(sh) = argv.get(idx)
        && is_shell_stem(&exe_stem(sh))
        && argv.get(idx + 1).is_some_and(|f| matches!(f.as_str(), "-c" | "-lc" | "-lic" | "-ic"))
        && argv.len() >= idx + 5
        && argv.get(idx + 2).is_some_and(|script| script.contains("\"$@\"") || script.contains("$argv"))
    {
        return Some(idx + 4);
    }
    (idx < argv.len()).then_some(idx)
}

/// Resolves a live PTY terminal's CLI backend from saved state, `argv` (including wrappers),
/// foreground process metadata, and OSC 7501 `app` identity.
pub fn detect_terminal_cli_kind(t: &crate::terminals::Terminal, saved_kind: Option<&str>) -> PtyCliKind {
    if let Some(saved) = saved_kind {
        let parsed = PtyCliKind::from_str_name(saved);
        if parsed != PtyCliKind::Generic {
            return parsed;
        }
    }
    let from_argv = PtyCliKind::detect(&t.argv);
    if from_argv != PtyCliKind::Generic {
        return from_argv;
    }
    if let Some(ref fg) = t.foreground {
        let from_fg_argv = PtyCliKind::detect(&fg.argv);
        if from_fg_argv != PtyCliKind::Generic {
            return from_fg_argv;
        }
        let from_fg_name = PtyCliKind::detect_cmd_token(&fg.name);
        if from_fg_name != PtyCliKind::Generic {
            return from_fg_name;
        }
    }
    if let Some(ref s) = t.status {
        match s.app.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claude code" => return PtyCliKind::Claude,
            "codex" => return PtyCliKind::Codex,
            "agy" | "antigravity" | "antigravity-cli" => return PtyCliKind::Agy,
            _ => {}
        }
    }
    PtyCliKind::Generic
}

fn exe_stem(raw: &str) -> String {
    let base =
        PathBuf::from(raw).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| raw.to_string());
    let lower = base.to_ascii_lowercase();
    let no_exe = lower.strip_suffix(".exe").unwrap_or(&lower);
    no_exe.strip_suffix(".real").unwrap_or(no_exe).to_string()
}

fn shell_quote(raw: &str) -> String {
    #[cfg(windows)]
    {
        format!("\"{}\"", raw.replace('"', "\\\""))
    }
    #[cfg(not(windows))]
    {
        format!("'{}'", raw.replace('\'', "'\\''"))
    }
}

pub(crate) fn h2_exe_string() -> String {
    crate::client::exe_path().map(|p| p.display().to_string()).unwrap_or_else(|| "h2".into())
}

/// Ensures global `~/.gemini/config/plugins/herdr2-checkpoints/{plugin.json,hooks.json}` exists for `agy`.
///
/// Outside `herdr2`-managed panes (`H2_TERMINAL_ID` unset), `h2 checkpoint hook` immediately outputs `{}`
/// in <1ms without connecting to the daemon, leaving standalone `agy` sessions completely unaffected.
pub fn ensure_agy_plugin_installed(h2_exe: &str) -> Result<PathBuf, String> {
    let Some(home) = user_home_dir() else {
        return Err("cannot resolve user home directory for agy plugin".into());
    };
    ensure_agy_plugin_at(&home, h2_exe)
}

pub fn ensure_agy_plugin_at(home: &Path, h2_exe: &str) -> Result<PathBuf, String> {
    let dir = home.join(".gemini/config/plugins/herdr2-checkpoints");
    std::fs::create_dir_all(&dir).map_err(|e| format!("failed creating {}: {e}", dir.display()))?;

    let plugin_json = serde_json::to_vec_pretty(&json!({
        "name": "herdr2-checkpoints",
        "description": "Herdr2 per-turn checkpoint capture and rollback context handoff"
    }))
    .map_err(|e| e.to_string())?;
    crate::paths::write_atomic(&dir.join("plugin.json"), &plugin_json).map_err(|e| e.to_string())?;

    let quoted = shell_quote(h2_exe);
    let hooks_json = serde_json::to_vec_pretty(&json!({
        "herdr2-checkpoints": {
            "SessionStart": [{
                "type": "command",
                "command": format!("{quoted} checkpoint hook --source agy --event SessionStart"),
                "timeout": 10
            }],
            "PreInvocation": [{
                "type": "command",
                "command": format!("{quoted} checkpoint hook --source agy --event PreInvocation"),
                "timeout": 10
            }],
            "Stop": [{
                "type": "command",
                "command": format!("{quoted} checkpoint hook --source agy --event Stop"),
                "timeout": 15
            }]
        }
    }))
    .map_err(|e| e.to_string())?;
    crate::paths::write_atomic(&dir.join("hooks.json"), &hooks_json).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Ensures `<hooks_dir>/claude-hooks.json` exists with `UserPromptSubmit` + `Stop` handlers.
pub fn ensure_claude_hooks_file(hooks_dir: &Path, h2_exe: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(hooks_dir).map_err(|e| format!("failed creating {}: {e}", hooks_dir.display()))?;
    let path = hooks_dir.join("claude-hooks.json");
    let quoted = shell_quote(h2_exe);
    let payload = serde_json::to_vec_pretty(&json!({
        "hooks": {
            "UserPromptSubmit": [{
                "hooks": [{
                    "type": "command",
                    "command": format!("{quoted} checkpoint hook --source claude --phase pre")
                }]
            }],
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": format!("{quoted} checkpoint hook --source claude --phase post")
                }]
            }]
        }
    }))
    .map_err(|e| e.to_string())?;
    crate::paths::write_atomic(&path, &payload).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Result of launch-time CLI instrumentation for a PTY terminal.
#[derive(Clone, Debug)]
pub struct PreparedPtyLaunch {
    pub kind: PtyCliKind,
    pub session_id: Option<String>,
}

/// Injects zero-pollution hooks/session IDs into `argv` before spawning inside `holder`.
pub fn prepare_pty_launch(
    core: &Core,
    _id: u32,
    argv: &mut Vec<String>,
    _env: &mut BTreeMap<String, String>,
) -> PreparedPtyLaunch {
    let kind = PtyCliKind::detect(argv);
    let h2_exe = h2_exe_string();
    let Some(cmd_idx) = pty_command_offset(argv) else {
        return PreparedPtyLaunch { kind, session_id: None };
    };
    let can_append_flags = PtyCliKind::detect_cmd_token(&argv[cmd_idx]) == kind;
    let cmd_slice = &argv[cmd_idx..];
    let mut session_id = extract_flag_value(cmd_slice, &["--session-id", "--resume", "-r"]);

    match kind {
        PtyCliKind::Claude => {
            if let Ok(settings_path) = ensure_claude_hooks_file(&core.hooks_dir(), &h2_exe)
                && can_append_flags
                && !cmd_slice.iter().any(|a| a == "--settings" || a.starts_with("--settings="))
            {
                argv.push("--settings".into());
                argv.push(settings_path.to_string_lossy().to_string());
            }
            let cmd_slice = &argv[cmd_idx..];
            let has_session_flag = cmd_slice.iter().any(|a| {
                a == "--session-id"
                    || a.starts_with("--session-id=")
                    || a == "--resume"
                    || a.starts_with("--resume=")
                    || a == "-r"
                    || a == "--continue"
                    || a == "-c"
            });
            if !has_session_flag && can_append_flags {
                let sid = random_uuid_v4();
                argv.push("--session-id".into());
                argv.push(sid.clone());
                session_id = Some(sid);
            }
        }
        PtyCliKind::Codex => {
            if session_id.is_none() {
                session_id = extract_codex_resume_id(cmd_slice);
            }
            let has_notify = cmd_slice.windows(2).any(|w| w[0] == "-c" && w[1].starts_with("notify="))
                || cmd_slice.iter().any(|a| a.starts_with("-cnotify=") || a.contains("notify=["));
            if !has_notify && can_append_flags {
                let notify_arr = serde_json::to_string(&vec![
                    h2_exe,
                    "checkpoint".into(),
                    "hook".into(),
                    "--source".into(),
                    "codex-notify".into(),
                ])
                .unwrap_or_default();
                argv.push("-c".into());
                argv.push(format!("notify={notify_arr}"));
            }
        }
        PtyCliKind::Agy => {
            let _ = ensure_agy_plugin_installed(&h2_exe);
        }
        PtyCliKind::Generic => {}
    }

    PreparedPtyLaunch { kind, session_id }
}

fn extract_flag_value(argv: &[String], flags: &[&str]) -> Option<String> {
    for (idx, arg) in argv.iter().enumerate() {
        for flag in flags {
            if arg == flag {
                return argv.get(idx + 1).cloned();
            }
            if let Some(rest) = arg.strip_prefix(&format!("{flag}=")) {
                return Some(rest.to_string());
            }
        }
    }
    None
}

fn extract_codex_resume_id(argv: &[String]) -> Option<String> {
    let pos = argv.iter().position(|a| a == "resume")?;
    argv.get(pos + 1).filter(|s| !s.starts_with('-')).cloned()
}

/// Builds replacement `argv` to re-launch a CLI inside the same PTY pane after rollback,
/// preserving Herdr's login-shell `"$@"` wrapper when present.
pub fn build_rollback_argv(
    kind: PtyCliKind,
    orig_argv: &[String],
    hooks_dir: &Path,
    h2_exe: &str,
    to_turn: u64,
    fork_session_id: Option<&str>,
) -> Vec<String> {
    if let Some(cmd_idx) = pty_command_offset(orig_argv)
        && cmd_idx > 0
        && PtyCliKind::detect_cmd_token(&orig_argv[cmd_idx]) == kind
    {
        let mut out = orig_argv[..cmd_idx].to_vec();
        out.extend(build_direct_cli_argv(
            kind,
            &orig_argv[cmd_idx..],
            hooks_dir,
            h2_exe,
            to_turn,
            fork_session_id,
        ));
        return out;
    }
    build_direct_cli_argv(kind, orig_argv, hooks_dir, h2_exe, to_turn, fork_session_id)
}

/// Builds replacement `argv` for PTY rollback, wrapping foreground-started CLIs (typed inside an
/// interactive shell like `/usr/bin/zsh`) back into a login-shell wrapper so the shell stays open on exit.
pub fn build_rollback_argv_for_terminal(
    kind: PtyCliKind,
    orig_argv: &[String],
    fg_argv: Option<&[String]>,
    hooks_dir: &Path,
    h2_exe: &str,
    to_turn: u64,
    fork_session_id: Option<&str>,
) -> Vec<String> {
    if let Some(cmd_idx) = pty_command_offset(orig_argv)
        && PtyCliKind::detect_cmd_token(&orig_argv[cmd_idx]) == kind
    {
        return build_rollback_argv(kind, orig_argv, hooks_dir, h2_exe, to_turn, fork_session_id);
    }
    if kind != PtyCliKind::Generic
        && orig_argv.len() == 1
        && is_shell_stem(&exe_stem(&orig_argv[0]))
    {
        let cli_base: Vec<String> = fg_argv
            .filter(|a| !a.is_empty() && PtyCliKind::detect(a) == kind)
            .map(<[String]>::to_vec)
            .unwrap_or_else(|| vec![kind.as_str().to_string()]);
        let rebuilt_cli = build_direct_cli_argv(kind, &cli_base, hooks_dir, h2_exe, to_turn, fork_session_id);
        let sh = orig_argv[0].clone();
        let mut out = vec![sh.clone(), "-lic".into(), "\"$@\"; exec \"$0\" -li".into(), sh];
        out.extend(rebuilt_cli);
        return out;
    }
    build_rollback_argv(kind, orig_argv, hooks_dir, h2_exe, to_turn, fork_session_id)
}

fn build_direct_cli_argv(
    kind: PtyCliKind,
    orig_argv: &[String],
    hooks_dir: &Path,
    h2_exe: &str,
    to_turn: u64,
    fork_session_id: Option<&str>,
) -> Vec<String> {
    match kind {
        PtyCliKind::Claude => {
            let bin = orig_argv.first().cloned().unwrap_or_else(|| "claude".into());
            let mut out = vec![bin];
            let mut i = 1usize;
            while i < orig_argv.len() {
                let arg = &orig_argv[i];
                if matches!(arg.as_str(), "--session-id" | "--resume" | "-r" | "--settings") {
                    i += 2;
                    continue;
                }
                if arg.starts_with("--session-id=")
                    || arg.starts_with("--resume=")
                    || arg.starts_with("--settings=")
                    || matches!(arg.as_str(), "-c" | "--continue" | "--fork-session")
                {
                    i += 1;
                    continue;
                }
                out.push(arg.clone());
                i += 1;
            }
            if let Ok(settings_path) = ensure_claude_hooks_file(hooks_dir, h2_exe) {
                out.push("--settings".into());
                out.push(settings_path.to_string_lossy().to_string());
            }
            if to_turn > 0
                && let Some(sid) = fork_session_id
            {
                out.push("--resume".into());
                out.push(sid.to_string());
            } else {
                out.push("--session-id".into());
                out.push(random_uuid_v4());
            }
            out
        }
        PtyCliKind::Codex => {
            let bin = orig_argv.first().cloned().unwrap_or_else(|| "codex".into());
            let mut flags = Vec::new();
            let mut i = 1usize;
            while i < orig_argv.len() {
                let arg = &orig_argv[i];
                if arg == "resume" {
                    i += 2;
                    continue;
                }
                if arg == "-c" && orig_argv.get(i + 1).is_some_and(|v| v.starts_with("notify=")) {
                    i += 2;
                    continue;
                }
                flags.push(arg.clone());
                i += 1;
            }
            let mut out = vec![bin];
            if to_turn > 0
                && let Some(sid) = fork_session_id
            {
                out.push("resume".into());
                out.push(sid.to_string());
            }
            out.extend(flags);
            if !out.iter().any(|a| a == "--no-daemon") {
                out.push("--no-daemon".into());
            }
            let notify_arr = serde_json::to_string(&vec![
                h2_exe.to_string(),
                "checkpoint".into(),
                "hook".into(),
                "--source".into(),
                "codex-notify".into(),
            ])
            .unwrap_or_default();
            out.push("-c".into());
            out.push(format!("notify={notify_arr}"));
            out
        }
        PtyCliKind::Agy => {
            let _ = ensure_agy_plugin_installed(h2_exe);
            let mut out = orig_argv.to_vec();
            if out.is_empty() {
                out.push("agy".into());
            }
            out
        }
        PtyCliKind::Generic => orig_argv.to_vec(),
    }
}

/// Forks a Claude Code `.jsonl` session up to turn `to_turn` into `<fork_uuid>.jsonl`, returning
/// `(forked_path, restored_prompt)` without touching the original file.
pub fn fork_claude_jsonl(src: &Path, to_turn: u64, fork_uuid: &str) -> Result<(PathBuf, Option<String>), String> {
    let raw = std::fs::read_to_string(src).map_err(|e| format!("failed reading {}: {e}", src.display()))?;
    let mut kept_lines = Vec::new();
    let mut current_turn = 0u64;
    let mut restored_prompt: Option<String> = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(mut val) = serde_json::from_str::<Value>(trimmed) else {
            kept_lines.push(trimmed.to_string());
            continue;
        };

        let is_side = val.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || val.get("isMeta").and_then(Value::as_bool) == Some(true);

        if !is_side && let Some(user_prompt) = extract_claude_user_turn_prompt(&val) {
            let next_turn = current_turn + 1;
            if next_turn == to_turn + 1 && restored_prompt.is_none() {
                restored_prompt = Some(user_prompt);
            }
            if next_turn > to_turn {
                break;
            }
            current_turn = next_turn;
        }

        if let Some(obj) = val.as_object_mut()
            && obj.contains_key("sessionId")
        {
            obj.insert("sessionId".into(), Value::String(fork_uuid.to_string()));
        }
        kept_lines.push(serde_json::to_string(&val).unwrap_or_else(|_| trimmed.to_string()));
    }

    // Trim trailing queue-operation events belonging to a discarded queued turn.
    while kept_lines.last().is_some_and(|line| {
        serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|v| v.get("type").and_then(Value::as_str).map(|s| s == "queue-operation"))
            == Some(true)
    }) {
        kept_lines.pop();
    }

    let parent = src.parent().ok_or_else(|| format!("{} has no parent directory", src.display()))?;
    let dest = parent.join(format!("{fork_uuid}.jsonl"));
    let mut out = kept_lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    crate::paths::write_atomic(&dest, out.as_bytes()).map_err(|e| format!("failed writing {}: {e}", dest.display()))?;
    Ok((dest, restored_prompt))
}

fn extract_claude_user_turn_prompt(v: &Value) -> Option<String> {
    if v.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let content = v.pointer("/message/content")?;
    match content {
        Value::String(s) => {
            if is_claude_user_noise(s) {
                return None;
            }
            let clean = clean_claude_text(s);
            (!clean.is_empty()).then_some(clean)
        }
        Value::Array(blocks) => {
            if blocks.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")) {
                return None;
            }
            let mut parts = Vec::new();
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(raw) = block.get("text").and_then(Value::as_str)
                    && !is_claude_user_noise(raw)
                {
                    let clean = clean_claude_text(raw);
                    if !clean.is_empty() {
                        parts.push(clean);
                    }
                }
            }
            (!parts.is_empty()).then(|| parts.join("\n\n"))
        }
        _ => None,
    }
}

/// Forks a Codex CLI `rollout-*.jsonl` up to `to_turn` user turns with a fresh `session_meta.payload.id`,
/// returning `(forked_path, restored_prompt)` without mutating `src`.
pub fn fork_codex_jsonl(src: &Path, to_turn: u64, fork_uuid: &str) -> Result<(PathBuf, Option<String>), String> {
    let raw = std::fs::read_to_string(src).map_err(|e| format!("failed reading {}: {e}", src.display()))?;
    let mut parsed_lines: Vec<(String, Option<Value>)> = Vec::new();
    let mut has_event_user_msg = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let val = serde_json::from_str::<Value>(trimmed).ok();
        if let Some(ref v) = val
            && v.get("type").and_then(Value::as_str) == Some("event_msg")
            && v.pointer("/payload/type").and_then(Value::as_str) == Some("user_message")
        {
            has_event_user_msg = true;
        }
        parsed_lines.push((trimmed.to_string(), val));
    }

    let mut cut_idx = parsed_lines.len();
    let mut current_turn = 0u64;
    let mut restored_prompt: Option<String> = None;

    for (idx, (_, opt_v)) in parsed_lines.iter().enumerate() {
        let Some(v) = opt_v else { continue };
        let user_prompt = if has_event_user_msg {
            if v.get("type").and_then(Value::as_str) == Some("event_msg")
                && v.pointer("/payload/type").and_then(Value::as_str) == Some("user_message")
            {
                v.pointer("/payload/message")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            } else {
                None
            }
        } else {
            extract_codex_response_user_prompt(v)
        };

        if let Some(prompt_text) = user_prompt {
            let next_turn = current_turn + 1;
            if next_turn == to_turn + 1 && restored_prompt.is_none() {
                restored_prompt = Some(prompt_text);
            }
            if next_turn > to_turn {
                // Step backward past preceding per-turn wrapper lines belonging to the dropped turn.
                let mut back = idx;
                while back > 0 {
                    let Some((_, Some(prev))) = parsed_lines.get(back - 1) else { break };
                    if is_codex_turn_preamble(prev) {
                        back -= 1;
                    } else {
                        break;
                    }
                }
                cut_idx = back;
                break;
            }
            current_turn = next_turn;
        }
    }

    let mut out_lines = Vec::new();
    for (raw_line, opt_v) in parsed_lines.into_iter().take(cut_idx) {
        if let Some(mut v) = opt_v {
            if v.get("type").and_then(Value::as_str) == Some("session_meta")
                && let Some(payload) = v.get_mut("payload").and_then(Value::as_object_mut)
            {
                payload.insert("id".into(), Value::String(fork_uuid.to_string()));
            }
            out_lines.push(serde_json::to_string(&v).unwrap_or(raw_line));
        } else {
            out_lines.push(raw_line);
        }
    }

    let parent = src.parent().ok_or_else(|| format!("{} has no parent directory", src.display()))?;
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("rollout-fork");
    // Preserve `rollout-YYYY-MM-DDTHH-MM-SS` prefix when present; strip trailing UUID if possible.
    let prefix =
        if let Some(pos) = stem.match_indices('-').nth(3).map(|(i, _)| i) { &stem[..pos] } else { "rollout-fork" };
    let dest = parent.join(format!("{prefix}-{fork_uuid}.jsonl"));
    let mut out = out_lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    crate::paths::write_atomic(&dest, out.as_bytes()).map_err(|e| format!("failed writing {}: {e}", dest.display()))?;
    Ok((dest, restored_prompt))
}

fn extract_codex_response_user_prompt(v: &Value) -> Option<String> {
    if v.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let payload = v.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("message")
        || payload.get("role").and_then(Value::as_str) != Some("user")
    {
        return None;
    }
    let arr = payload.get("content").and_then(Value::as_array)?;
    let mut parts = Vec::new();
    for b in arr {
        if b.get("type").and_then(Value::as_str) == Some("input_text")
            && let Some(t) = b.get("text").and_then(Value::as_str).map(str::trim)
            && !t.is_empty()
            && !t.starts_with("<environment_context>")
            && !t.starts_with("# AGENTS.md instructions")
            && !t.starts_with("<user_instructions>")
        {
            parts.push(t.to_string());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn is_codex_turn_preamble(v: &Value) -> bool {
    match v.get("type").and_then(Value::as_str) {
        Some("turn_context") => true,
        Some("event_msg") => matches!(v.pointer("/payload/type").and_then(Value::as_str), Some("task_started")),
        Some("response_item") => extract_codex_response_user_prompt(v).is_some(),
        _ => false,
    }
}

/// Builds a bounded portable handoff + extracts the `(to_turn + 1)` prompt from an Antigravity `agy` log.
pub fn prepare_agy_rollback(src: &Path, to_turn: u64) -> (Option<String>, Option<String>) {
    let entries = parse_cli_transcript("agy", src);
    let restored_prompt = entries.iter().find_map(|e| match e {
        Entry::User { turn, text, .. } if *turn == to_turn + 1 => {
            let clean = strip_h2_envelope(text);
            (!clean.is_empty()).then_some(clean)
        }
        _ => None,
    });
    let handoff = build_portable_handoff(&entries, to_turn, 24 * 1024);
    (handoff, restored_prompt)
}

/// Processes a synchronous CLI hook invocation (`claude`, `codex-notify`, `agy`) and returns JSON output for stdout.
pub async fn handle_checkpoint_hook(
    core: &Arc<Core>,
    id: u32,
    source: &str,
    phase: Option<&str>,
    event: Option<&str>,
    payload: &Value,
) -> Value {
    let (addr, cwd) = {
        let st = core.lock();
        let Some(t) = st.pty_reg.terminals.get(&id) else {
            return json!({});
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
    };

    match source {
        "claude" => {
            let sid = payload.get("session_id").and_then(Value::as_str).map(str::to_string);
            let tpath = payload.get("transcript_path").and_then(Value::as_str).map(str::to_string);
            let prompt = payload.get("prompt").and_then(Value::as_str).map(strip_h2_envelope).filter(|s| !s.is_empty());

            if phase == Some("pre") {
                let needs_baseline = {
                    let mut st = core.lock();
                    let mut tc = core.target_checkpoints(&mut st, &addr);
                    tc.cli_kind = Some("claude".into());
                    if let Some(s) = sid {
                        tc.cli_session_id = Some(s);
                    }
                    if let Some(p) = tpath {
                        tc.cli_transcript_path = Some(p);
                    }
                    if let Some(ref pr) = prompt {
                        tc.active_turn_prompt = Some(pr.clone());
                    }
                    if tc.active_turn_num.is_none() {
                        let max_turn = tc.checkpoints.iter().map(|c| c.turn).max().unwrap_or(0);
                        let t_turn = st.pty_reg.terminals.get(&id).map_or(0, |t| t.turn);
                        tc.active_turn_num = Some((t_turn + 1).max(max_turn + 1));
                    }
                    let needs = !tc.checkpoints.iter().any(|c| c.turn == 0);
                    core.save_target_checkpoints(&mut st, tc);
                    needs
                };
                if needs_baseline {
                    let _ = capture_target_checkpoint(core, TargetKey::Terminal(id), Some(0), None, None, true, false)
                        .await;
                }
            } else if phase == Some("post") {
                let (turn_num, prompt_val) = {
                    let mut st = core.lock();
                    let mut tc = core.target_checkpoints(&mut st, &addr);
                    tc.cli_kind = Some("claude".into());
                    if let Some(s) = sid.clone() {
                        tc.cli_session_id = Some(s);
                    }
                    if let Some(p) = tpath.clone() {
                        tc.cli_transcript_path = Some(p);
                    }
                    let turn_num = tc
                        .active_turn_num
                        .take()
                        .unwrap_or_else(|| tc.checkpoints.iter().map(|c| c.turn).max().unwrap_or(0) + 1);
                    let pr = tc.active_turn_prompt.take();
                    if tc.cli_transcript_path.is_none()
                        && let Some(ref s) = tc.cli_session_id
                        && let Some(found) = find_claude_session_file(&cwd, s)
                    {
                        tc.cli_transcript_path = Some(found.to_string_lossy().to_string());
                    }
                    core.save_target_checkpoints(&mut st, tc);
                    (turn_num, pr)
                };
                let _ = capture_target_checkpoint(
                    core,
                    TargetKey::Terminal(id),
                    Some(turn_num),
                    prompt_val,
                    None,
                    true,
                    false,
                )
                .await;
            }
            json!({})
        }
        "codex-notify" => {
            let sid = payload
                .get("thread-id")
                .or_else(|| payload.get("session-id"))
                .or_else(|| payload.get("session_id"))
                .or_else(|| payload.get("conversation-id"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let turn_prompt = payload
                .get("input-messages")
                .and_then(Value::as_array)
                .and_then(|arr| arr.last())
                .and_then(Value::as_str)
                .map(strip_h2_envelope)
                .filter(|s| !s.is_empty());

            let (needs_baseline, turn_num, effective_prompt) = {
                let mut st = core.lock();
                let mut tc = core.target_checkpoints(&mut st, &addr);
                tc.cli_kind = Some("codex".into());
                if let Some(ref s) = sid {
                    tc.cli_session_id = Some(s.clone());
                    if tc.cli_transcript_path.is_none()
                        && let Some(found) = find_codex_rollout_by_sid(&cwd, s)
                    {
                        tc.cli_transcript_path = Some(found.to_string_lossy().to_string());
                    }
                }
                if tc.cli_transcript_path.is_none()
                    && let Some(found) = discover_cli_transcript("codex", &cwd, 0, None)
                {
                    tc.cli_transcript_path = Some(found.to_string_lossy().to_string());
                }
                let needs_baseline = !tc.checkpoints.iter().any(|c| c.turn == 0);
                let t_turn = st.pty_reg.terminals.get(&id).map_or(0, |t| t.turn);
                let turn_num = tc
                    .active_turn_num
                    .take()
                    .unwrap_or_else(|| {
                        let max_ckpt = tc.checkpoints.iter().map(|c| c.turn).max().unwrap_or(0);
                        (t_turn + 1).max(max_ckpt + 1)
                    });
                let effective_prompt = turn_prompt.or_else(|| tc.active_turn_prompt.take());
                core.save_target_checkpoints(&mut st, tc);
                (needs_baseline, turn_num, effective_prompt)
            };
            if needs_baseline {
                let _ =
                    capture_target_checkpoint(core, TargetKey::Terminal(id), Some(0), None, None, true, false).await;
            }
            let _ = capture_target_checkpoint(
                core,
                TargetKey::Terminal(id),
                Some(turn_num),
                effective_prompt,
                None,
                true,
                false,
            )
            .await;
            json!({})
        }
        "agy" => {
            let cid = payload.get("conversationId").and_then(Value::as_str).map(str::to_string);
            let tpath = payload.get("transcriptPath").and_then(Value::as_str).map(str::to_string);
            let user_input =
                payload.get("lastUserInput").and_then(Value::as_str).map(strip_h2_envelope).filter(|s| !s.is_empty());
            let evt = event.or_else(|| payload.get("hookEventName").and_then(Value::as_str)).unwrap_or("");

            match evt {
                "SessionStart" => {
                    let (handoff, rollback_turn) = {
                        let mut st = core.lock();
                        let mut tc = core.target_checkpoints(&mut st, &addr);
                        tc.cli_kind = Some("agy".into());
                        if let Some(ref c) = cid {
                            tc.cli_session_id = Some(c.clone());
                        }
                        if let Some(ref p) = tpath {
                            tc.cli_transcript_path = Some(p.clone());
                        } else if let Some(ref c) = tc.cli_session_id
                            && let Some(found) = find_agy_transcript_by_cid(c)
                        {
                            tc.cli_transcript_path = Some(found.to_string_lossy().to_string());
                        }
                        let handoff = tc.pending_handoff.take();
                        let rb_turn = tc.pending_rollback_turn.take();
                        core.save_target_checkpoints(&mut st, tc);
                        (handoff, rb_turn)
                    };
                    if let Some(h) = handoff {
                        let k = rollback_turn.unwrap_or(0);
                        return json!({
                            "injectSteps": [{
                                "systemMessage": {
                                    "systemMessage": h,
                                    "metadata": {
                                        "user_markdown": format!("⏪ **Rolled back to turn {k}** (workspace files and turns 1..{k} context restored)")
                                    }
                                }
                            }]
                        });
                    }
                    json!({})
                }
                "PreInvocation" => {
                    let needs_baseline = {
                        let mut st = core.lock();
                        let mut tc = core.target_checkpoints(&mut st, &addr);
                        tc.cli_kind = Some("agy".into());
                        if let Some(ref c) = cid {
                            tc.cli_session_id = Some(c.clone());
                        }
                        if let Some(ref p) = tpath {
                            tc.cli_transcript_path = Some(p.clone());
                        } else if let Some(ref c) = tc.cli_session_id
                            && let Some(found) = find_agy_transcript_by_cid(c)
                        {
                            tc.cli_transcript_path = Some(found.to_string_lossy().to_string());
                        }
                        if tc.active_turn_num.is_none() {
                            let max_turn = tc.checkpoints.iter().map(|c| c.turn).max().unwrap_or(0);
                            let t_turn = st.pty_reg.terminals.get(&id).map_or(0, |t| t.turn);
                            tc.active_turn_num = Some((t_turn + 1).max(max_turn + 1));
                        }
                        if tc.active_turn_prompt.is_none() && user_input.is_some() {
                            tc.active_turn_prompt = user_input;
                        }
                        let needs = !tc.checkpoints.iter().any(|c| c.turn == 0);
                        core.save_target_checkpoints(&mut st, tc);
                        needs
                    };
                    if needs_baseline {
                        let _ =
                            capture_target_checkpoint(core, TargetKey::Terminal(id), Some(0), None, None, true, false)
                                .await;
                    }
                    json!({})
                }
                "Stop" => {
                    let fully_idle = payload.get("fullyIdle").and_then(Value::as_bool);
                    let continuation_active = payload.get("continuationActive").and_then(Value::as_bool);
                    if fully_idle == Some(false) || continuation_active == Some(true) {
                        return json!({});
                    }
                    let (turn_num, prompt_val) = {
                        let mut st = core.lock();
                        let mut tc = core.target_checkpoints(&mut st, &addr);
                        tc.cli_kind = Some("agy".into());
                        if let Some(ref c) = cid {
                            tc.cli_session_id = Some(c.clone());
                        }
                        if let Some(ref p) = tpath {
                            tc.cli_transcript_path = Some(p.clone());
                        } else if tc.cli_transcript_path.is_none()
                            && let Some(ref c) = tc.cli_session_id
                            && let Some(found) = find_agy_transcript_by_cid(c)
                        {
                            tc.cli_transcript_path = Some(found.to_string_lossy().to_string());
                        }
                        let turn_num = tc
                            .active_turn_num
                            .take()
                            .unwrap_or_else(|| tc.checkpoints.iter().map(|c| c.turn).max().unwrap_or(0) + 1);
                        let pr = tc.active_turn_prompt.take().or(user_input);
                        core.save_target_checkpoints(&mut st, tc);
                        (turn_num, pr)
                    };
                    let _ = capture_target_checkpoint(
                        core,
                        TargetKey::Terminal(id),
                        Some(turn_num),
                        prompt_val,
                        None,
                        true,
                        false,
                    )
                    .await;
                    json!({})
                }
                _ => json!({}),
            }
        }
        _ => json!({}),
    }
}

/// Dispatches PTY checkpoint capture/discovery to `Core::jobs` on Tokio so calls from
/// `read_holder` OS threads work reliably.
pub fn on_pty_status_transition(core: &Core, st: &mut crate::runtime::State, id: u32, change: &crate::status::Change) {
    if st.pty_respawning.contains(&id) {
        return;
    }
    let Some(t) = st.pty_reg.terminals.get(&id) else {
        return;
    };
    if !t.is_live() {
        return;
    }
    let entered_working = t.status.as_ref().is_some_and(|s| s.state == "working") && change.status;
    if !entered_working && !change.turn_ended {
        return;
    }
    let _ = core.jobs.send(crate::notes::Job::PtyCheckpoint {
        id,
        entered_working,
        turn_ended: change.turn_ended,
        turn: t.turn,
    });
}

/// Captures baseline (`T0`) on entering `working`, discovers CLI transcripts, and captures fallback
/// per-turn checkpoints on `turn_ended` (immediately for `Generic`, or after 750ms if unclaimed by hooks).
pub async fn process_pty_checkpoint_job(
    core: &Arc<Core>,
    id: u32,
    entered_working: bool,
    turn_ended: bool,
    reported_turn: u64,
) {
    let (addr, cwd, fg_pid, started_ms, cli_kind, expected_turn, needs_baseline, needs_discover, hook_already_done) = {
        let mut st = core.lock();
        if st.pty_respawning.contains(&id) {
            return;
        }
        let Some(t) = st.pty_reg.terminals.get(&id).cloned() else {
            return;
        };
        if !t.is_live() {
            return;
        }
        let cwd = t
            .foreground
            .as_ref()
            .map(|f| f.cwd.as_str())
            .filter(|s| !s.is_empty())
            .or(t.last_cwd.as_deref())
            .unwrap_or(&t.cwd)
            .to_string();
        let fg_pid = t.foreground.as_ref().map(|f| f.pid as u32);
        let started_ms = t.program_started_ms.unwrap_or(0);
        let mut tc = core.target_checkpoints(&mut st, &t.address);
        let cli_kind = detect_terminal_cli_kind(&t, tc.cli_kind.as_deref());
        tc.cli_kind = Some(cli_kind.as_str().into());
        let max_ckpt_turn = tc.checkpoints.iter().map(|c| c.turn).max().unwrap_or(0);
        if entered_working && tc.active_turn_num.is_none() {
            tc.active_turn_num = Some((t.turn + 1).max(max_ckpt_turn + 1));
        }
        let hook_already_done = turn_ended
            && tc.active_turn_num.is_none()
            && cli_kind != PtyCliKind::Generic
            && ((reported_turn > 0 && tc.checkpoints.iter().any(|c| c.turn == reported_turn))
                || tc
                    .checkpoints
                    .last()
                    .is_some_and(|c| c.turn > 0 && c.ts_ms.saturating_add(5_000) >= crate::runtime::now_ms()));
        let expected_turn = tc
            .active_turn_num
            .unwrap_or_else(|| reported_turn.max(max_ckpt_turn + 1))
            .max(1);
        let needs_baseline = !tc.checkpoints.iter().any(|c| c.turn == 0);
        let needs_discover = tc.cli_transcript_path.is_none() && cli_kind != PtyCliKind::Generic;
        core.save_target_checkpoints(&mut st, tc);
        (
            t.address,
            cwd,
            fg_pid,
            started_ms,
            cli_kind,
            expected_turn,
            needs_baseline,
            needs_discover,
            hook_already_done,
        )
    };

    if entered_working {
        if needs_baseline {
            let _ = capture_target_checkpoint(core, TargetKey::Terminal(id), Some(0), None, None, true, false).await;
        }
        if needs_discover {
            let kind_str = cli_kind.as_str().to_string();
            let cwd_c = cwd.clone();
            if let Some(found) = tokio::task::spawn_blocking(move || {
                discover_cli_transcript(&kind_str, &cwd_c, started_ms, fg_pid)
            })
            .await
            .ok()
            .flatten()
            {
                let mut st = core.lock();
                let mut tc = core.target_checkpoints(&mut st, &addr);
                if tc.cli_transcript_path.is_none() {
                    tc.cli_transcript_path = Some(found.to_string_lossy().to_string());
                    core.save_target_checkpoints(&mut st, tc);
                }
            }
        }
        if !turn_ended {
            return;
        }
    }

    if !turn_ended || hook_already_done {
        return;
    }

    if cli_kind != PtyCliKind::Generic {
        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
    }

    let (still_unclaimed, still_needs_baseline, cached_tpath, saved_prompt) = {
        let mut st = core.lock();
        if st.pty_respawning.contains(&id) {
            return;
        }
        let Some((addr_now, is_live)) = st.pty_reg.terminals.get(&id).map(|t| (t.address.clone(), t.is_live())) else {
            return;
        };
        if !is_live {
            return;
        }
        let tc = core.target_checkpoints(&mut st, &addr_now);
        let unclaimed = !tc.checkpoints.iter().any(|c| c.turn == expected_turn);
        let needs_base = !tc.checkpoints.iter().any(|c| c.turn == 0);
        let tpath = tc.cli_transcript_path.clone();
        let prompt = tc.active_turn_prompt.clone();
        (unclaimed, needs_base, tpath, prompt)
    };
    if !still_unclaimed {
        return;
    }

    if still_needs_baseline {
        let _ = capture_target_checkpoint(core, TargetKey::Terminal(id), Some(0), None, None, true, false).await;
    }

    let mut prompt = saved_prompt;
    if prompt.is_none() && cli_kind != PtyCliKind::Generic {
        let kind_str = cli_kind.as_str().to_string();
        let cwd_c = cwd.clone();
        let tpath_c = cached_tpath.clone();
        let res = tokio::task::spawn_blocking(move || -> (Option<PathBuf>, Option<String>) {
            let path = tpath_c
                .as_deref()
                .map(PathBuf::from)
                .filter(|p| p.is_file())
                .or_else(|| discover_cli_transcript(&kind_str, &cwd_c, started_ms, fg_pid));
            let Some(ref p) = path else {
                return (None, None);
            };
            let entries = parse_cli_transcript(&kind_str, p);
            let turn_prompt = entries
                .iter()
                .find_map(|e| match e {
                    Entry::User { turn, text, .. } if *turn == expected_turn => {
                        let clean = strip_h2_envelope(text);
                        (!clean.is_empty()).then_some(clean)
                    }
                    _ => None,
                })
                .or_else(|| {
                    entries.iter().rev().find_map(|e| match e {
                        Entry::User { text, .. } => {
                            let clean = strip_h2_envelope(text);
                            (!clean.is_empty()).then_some(clean)
                        }
                        _ => None,
                    })
                });
            (path, turn_prompt)
        })
        .await
        .ok();
        if let Some((found_path, parsed_prompt)) = res {
            if let Some(p) = found_path {
                let mut st = core.lock();
                let mut tc = core.target_checkpoints(&mut st, &addr);
                if tc.cli_transcript_path.is_none() {
                    tc.cli_transcript_path = Some(p.to_string_lossy().to_string());
                    core.save_target_checkpoints(&mut st, tc);
                }
            }
            if prompt.is_none() {
                prompt = parsed_prompt;
            }
        }
    }

    let _ = capture_target_checkpoint(
        core,
        TargetKey::Terminal(id),
        Some(expected_turn),
        prompt,
        None,
        true,
        false,
    )
    .await;
}

struct RespawnGuard {
    core: Arc<Core>,
    id: u32,
    armed: bool,
}

impl Drop for RespawnGuard {
    fn drop(&mut self) {
        if self.armed {
            let mut st = self.core.lock();
            self.core.remove_guard(&mut st, self.id);
        }
    }
}

trait RespawnExt {
    fn remove_guard(&self, st: &mut crate::runtime::State, id: u32);
}

impl RespawnExt for Core {
    fn remove_guard(&self, st: &mut crate::runtime::State, id: u32) {
        st.pty_respawning.remove(&id);
        if let Some(t) = st.pty_reg.terminals.get_mut(&id)
            && t.reason.as_deref() == Some("rollback")
            && t.pid.is_some()
        {
            t.reason = None;
        }
    }
}

/// Rolls back a PTY terminal (`t*`): forks/rehydrates CLI memory, kills old PTY process via `holder`,
/// optionally restores workspace files, resets VT screens/viewers, and respawns cleanly in the same pane.
pub async fn rollback_pty_terminal(
    core: &Arc<Core>,
    id: u32,
    to_turn: u64,
    restore_files: bool,
    confirm: bool,
) -> Result<crate::checkpoint::CheckpointRollbackResult, crate::rpc::RpcError> {
    use crate::holder_proto as proto;
    use crate::platform::signal;
    use crate::rpc::RpcError;

    let (
        addr,
        orig_cwd,
        orig_argv,
        fg_argv,
        restart_env,
        cols,
        rows,
        identity,
        fg_pid,
        started_ms,
        cli_kind,
        sid,
        tpath,
        target_ref,
    ) = {
        let mut st = core.lock();
        let Some(t) = st.pty_reg.terminals.get(&id).cloned() else {
            return Err(RpcError::new(
                "not_found",
                format!("terminal id {id} not found"),
                "run `h2 ps` to inspect live terminals",
            ));
        };
        if !t.is_live() {
            return Err(RpcError::new(
                "unavailable",
                format!("{} has finished; only live terminals can roll back", t.address),
                "",
            ));
        }
        if st.pty_respawning.contains(&id) {
            return Err(RpcError::new(
                "busy",
                format!("{} is already respawning from a rollback", t.address),
                "wait a moment and retry",
            ));
        }
        if t.status.as_ref().is_some_and(|s| s.state == "working") {
            return Err(RpcError::new(
                "busy",
                format!("{} is currently working; stop or interrupt it first", t.address),
                format!("interrupt {} first", t.address),
            ));
        }
        if t.holder.is_none() || st.link(id).is_none() {
            return Err(RpcError::new("unavailable", format!("{} has no active holder connection", t.address), ""));
        }
        let tc = core.target_checkpoints(&mut st, &t.address);
        let max_ckpt_turn = tc.checkpoints.iter().map(|c| c.turn).max().unwrap_or(0);
        if to_turn > t.turn.max(max_ckpt_turn) {
            return Err(RpcError::invalid_params(format!(
                "cannot roll back {} to turn {to_turn} beyond turn {}",
                t.address,
                t.turn.max(max_ckpt_turn)
            )));
        }
        let target_ref = tc.checkpoints.iter().find(|c| c.turn == to_turn).map(|c| c.ref_name.clone());
        let cwd = t
            .foreground
            .as_ref()
            .map(|f| f.cwd.as_str())
            .filter(|s| !s.is_empty())
            .or(t.last_cwd.as_deref())
            .unwrap_or(&t.cwd)
            .to_string();
        let cli_kind = detect_terminal_cli_kind(&t, tc.cli_kind.as_deref());
        let sid = tc.cli_session_id;
        let tpath = tc.cli_transcript_path;
        let fg_argv = t.foreground.as_ref().map(|f| f.argv.clone());
        let fg_pid = t.foreground.as_ref().map(|f| f.pid as u32);
        (
            t.address,
            cwd,
            t.argv,
            fg_argv,
            t.restart_env,
            t.cols,
            t.rows,
            t.identity,
            fg_pid,
            t.program_started_ms.unwrap_or(0),
            cli_kind,
            sid,
            tpath,
            target_ref,
        )
    };

    let repo = crate::checkpoint::probe_repo(Path::new(&orig_cwd));
    if restore_files {
        let Some(ref r) = repo else {
            return Err(RpcError::invalid_params(format!(
                "workspace `{orig_cwd}` is not a Git repository; pass --no-files"
            )));
        };
        if target_ref.is_none() {
            return Err(RpcError::invalid_params(format!("no checkpoint found for {addr} at turn {to_turn}")));
        }
        let live_snapshot = {
            let st = core.lock();
            crate::checkpoint_safety::snapshot_live_targets(&st)
        };
        crate::checkpoint_safety::enforce_restore_safety(&addr, r, &live_snapshot, confirm)?;
    }

    // Locate CLI transcript and prepare forked session / handoff BEFORE killing the live pane process.
    let transcript_src: Option<PathBuf> =
        tpath.as_deref().map(PathBuf::from).filter(|p| p.is_file()).or_else(|| match cli_kind {
            PtyCliKind::Claude => sid
                .as_deref()
                .and_then(|s| find_claude_session_file(&orig_cwd, s))
                .or_else(|| discover_cli_transcript("claude", &orig_cwd, started_ms, fg_pid)),
            PtyCliKind::Codex => sid
                .as_deref()
                .and_then(|s| find_codex_rollout_by_sid(&orig_cwd, s))
                .or_else(|| discover_cli_transcript("codex", &orig_cwd, started_ms, fg_pid)),
            PtyCliKind::Agy => sid
                .as_deref()
                .and_then(find_agy_transcript_by_cid)
                .or_else(|| discover_cli_transcript("agy", &orig_cwd, started_ms, fg_pid)),
            PtyCliKind::Generic => None,
        });

    let mut new_sid: Option<String> = None;
    let mut new_tpath: Option<String> = None;
    let mut new_handoff: Option<String> = None;
    let mut restored_prompt: Option<String> = None;
    let mut rewound_session = false;

    if to_turn == 0 {
        // Still extract turn 1's user prompt if transcript exists so UI can pre-fill composer.
        if let Some(ref src) = transcript_src {
            match cli_kind {
                PtyCliKind::Claude => {
                    if let Ok((_, prompt)) = fork_claude_jsonl(src, 0, &random_uuid_v4()) {
                        restored_prompt = prompt;
                    }
                    new_sid = Some(random_uuid_v4());
                }
                PtyCliKind::Codex => {
                    if let Ok((_, prompt)) = fork_codex_jsonl(src, 0, &random_uuid_v4()) {
                        restored_prompt = prompt;
                    }
                }
                PtyCliKind::Agy => {
                    let (_, prompt) = prepare_agy_rollback(src, 0);
                    restored_prompt = prompt;
                }
                PtyCliKind::Generic => {}
            }
        }
        rewound_session = cli_kind != PtyCliKind::Generic;
    } else if let Some(ref src) = transcript_src {
        match cli_kind {
            PtyCliKind::Claude => {
                let fork_uuid = random_uuid_v4();
                let (dest, prompt) = fork_claude_jsonl(src, to_turn, &fork_uuid).map_err(RpcError::internal)?;
                new_sid = Some(fork_uuid);
                new_tpath = Some(dest.to_string_lossy().to_string());
                restored_prompt = prompt;
                rewound_session = true;
            }
            PtyCliKind::Codex => {
                let fork_uuid = random_uuid_v4();
                let (dest, prompt) = fork_codex_jsonl(src, to_turn, &fork_uuid).map_err(RpcError::internal)?;
                new_sid = Some(fork_uuid);
                new_tpath = Some(dest.to_string_lossy().to_string());
                restored_prompt = prompt;
                rewound_session = true;
            }
            PtyCliKind::Agy => {
                let (handoff, prompt) = prepare_agy_rollback(src, to_turn);
                new_handoff = handoff;
                restored_prompt = prompt;
                rewound_session = true;
            }
            PtyCliKind::Generic => {}
        }
    }

    let h2_exe = h2_exe_string();
    let new_argv = build_rollback_argv_for_terminal(
        cli_kind,
        &orig_argv,
        fg_argv.as_deref(),
        &core.hooks_dir(),
        &h2_exe,
        to_turn,
        new_sid.as_deref(),
    );

    // Guard in-flight respawn so EXITED frames do not mark the terminal permanently finished.
    let mut respawn_guard = {
        let mut st = core.lock();
        if st.link(id).is_none() {
            return Err(RpcError::new("unavailable", format!("{addr} holder disconnected"), ""));
        }
        st.pty_respawning.insert(id);
        let guard = RespawnGuard { core: Arc::clone(core), id, armed: true };
        let had_pid = st.pty_reg.terminals.get(&id).is_some_and(|t| t.pid.is_some());
        if let Some(t) = st.pty_reg.terminals.get_mut(&id) {
            t.reason = Some("rollback".into());
        }
        if had_pid && let Some(link) = st.link(id) {
            link.send(&proto::json_frame(proto::SIGNAL, id, &proto::Signal { signal: signal::TERM }));
        }
        guard
    };

    let term_deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(1500);
    loop {
        let still_alive = {
            let st = core.lock();
            st.pty_reg.terminals.get(&id).is_some_and(|t| t.is_live() && t.pid.is_some())
        };
        if !still_alive || tokio::time::Instant::now() >= term_deadline {
            break;
        }
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), core.changed.notified()).await;
    }

    let still_alive = {
        let st = core.lock();
        if st.pty_reg.terminals.get(&id).is_some_and(|t| t.is_live() && t.pid.is_some()) {
            if let Some(link) = st.link(id) {
                link.send(&proto::json_frame(proto::SIGNAL, id, &proto::Signal { signal: signal::KILL }));
            }
            true
        } else {
            false
        }
    };
    if still_alive {
        let kill_deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            let alive = {
                let st = core.lock();
                st.pty_reg.terminals.get(&id).is_some_and(|t| t.is_live() && t.pid.is_some())
            };
            if !alive || tokio::time::Instant::now() >= kill_deadline {
                break;
            }
            let _ = tokio::time::timeout(std::time::Duration::from_millis(50), core.changed.notified()).await;
        }
    }

    if let Some(link) = core.lock().link(id) {
        link.send(&proto::frame(proto::RELEASE, id, &[]));
    }

    // Old CLI process is gone: safe to restore files and prune future hidden refs.
    if let Some(ref r) = repo {
        let _ckpt_guard = core.repo_ckpt_lock(&r.toplevel).lock_owned().await;
        let r_clone = r.clone();
        let ref_to_restore = target_ref.clone();
        let cwd_path = PathBuf::from(&orig_cwd);
        let addr_clone = addr.clone();
        let slug_clone = core.session_slug();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            if restore_files && let Some(ref ref_name) = ref_to_restore {
                crate::checkpoint::restore_checkpoint(&r_clone, ref_name, Some(&cwd_path))?;
            }
            let _ = crate::checkpoint::delete_checkpoint_refs_after(&r_clone, &slug_clone, &addr_clone, to_turn);
            Ok(())
        })
        .await
        .map_err(|e| RpcError::internal(format!("rollback task panicked: {e}")))?
        .map_err(RpcError::internal)?;
    }

    let settings = crate::config::load(&core.paths.config).unwrap_or_default();
    let base_env = crate::platform::program_env().await;
    let env = crate::api::pty_child_env(base_env, restart_env);
    let (env, scope) = crate::api::terminal_env(core, id, &orig_cwd, env);

    let spawn_rx = {
        let mut st = core.lock();
        if st.link(id).is_none() {
            return Err(RpcError::new("unavailable", format!("{addr} holder disconnected"), ""));
        }
        let mut screen = ghostty_vt::Terminal::new(cols, rows, settings.history_bytes());
        screen.set_xtversion(&identity);
        st.screens.insert(id, screen);
        st.relays.remove(&id);
        core.output(&mut st, id, b"\x1bc");

        let mut tc = core.target_checkpoints(&mut st, &addr);
        tc.cli_kind = Some(cli_kind.as_str().into());
        if let Some(sid_val) = new_sid {
            tc.cli_session_id = Some(sid_val);
        } else if to_turn == 0 {
            tc.cli_session_id = None;
        }
        tc.cli_transcript_path = if to_turn == 0 { None } else { new_tpath };
        tc.pending_handoff = new_handoff;
        tc.pending_rollback_turn = if to_turn == 0 { None } else { Some(to_turn) };
        tc.active_turn_prompt = None;
        tc.active_turn_num = None;
        tc.checkpoints.retain(|c| c.turn <= to_turn);
        core.save_target_checkpoints(&mut st, tc);

        st.pty_respawning.remove(&id);
        respawn_guard.armed = false;

        if let Some(t) = st.pty_reg.terminals.get_mut(&id) {
            t.argv = new_argv.clone();
            t.scope = Some(scope);
            let _ = crate::status::reset_terminal(t);
            t.turn = to_turn;
            t.records.clear();
            t.reason = Some("rollback".into());
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        st.pending.insert(id, tx);
        core.save_pty(&st);
        let spawn = proto::Spawn { argv: new_argv, env, cwd: orig_cwd, cols, rows };
        st.link(id).expect("verified holder link").send(&proto::json_frame(proto::SPAWN, id, &spawn));
        rx
    };

    let spawn_ms = std::env::var("H2_SPAWN_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(10_000);
    match tokio::time::timeout(std::time::Duration::from_millis(spawn_ms), spawn_rx).await {
        Ok(Ok(Ok(_))) => {
            let mut st = core.lock();
            if let Some(t) = st.pty_reg.terminals.get_mut(&id) {
                t.turn = to_turn;
            }
            core.emit_pty(&mut st, "checkpoint", id, json!({ "rewound_to": to_turn, "restored_files": restore_files }));
            core.save_pty(&st);
            Ok(crate::checkpoint::CheckpointRollbackResult {
                target: addr,
                turn: to_turn,
                restored_files: restore_files,
                rewound_session,
                restored_prompt,
            })
        }
        _ => {
            core.lock().pending.remove(&id);
            Err(RpcError::new(
                "spawn_failed",
                format!("failed to respawn {addr} after rollback"),
                "check CLI binary path and cwd",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cli_kinds_and_preserves_codex_no_daemon_argv() {
        assert_eq!(PtyCliKind::detect(&["claude".into(), "--dangerously-skip-permissions".into()]), PtyCliKind::Claude);
        assert_eq!(PtyCliKind::detect(&["/usr/local/bin/codex".into(), "--no-daemon".into()]), PtyCliKind::Codex);
        assert_eq!(PtyCliKind::detect(&["env".into(), "FOO=1".into(), "agy".into()]), PtyCliKind::Agy);
        assert_eq!(PtyCliKind::detect(&["/bin/zsh".into()]), PtyCliKind::Generic);

        let dir = crate::test_tmp().join(format!("h2-pty-argv-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        let rebuilt = build_rollback_argv(
            PtyCliKind::Codex,
            &[
                "codex".into(),
                "--no-daemon".into(),
                "--dangerously-bypass-approvals-and-sandbox".into(),
                "-c".into(),
                "notify=[\"old\"]".into(),
            ],
            &dir,
            "/usr/bin/h2",
            2,
            Some("fork-uuid-123"),
        );
        assert_eq!(&rebuilt[..3], ["codex", "resume", "fork-uuid-123"]);
        assert!(rebuilt.contains(&"--no-daemon".to_string()));
        assert!(rebuilt.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(!rebuilt.iter().any(|a| a.contains("\"old\"")));
        assert!(rebuilt.iter().any(|a| a.starts_with("notify=") && a.contains("codex-notify")));

        let fresh_claude = build_rollback_argv(
            PtyCliKind::Claude,
            &["claude".into(), "--dangerously-skip-permissions".into(), "--session-id".into(), "old-sid".into()],
            &dir,
            "/usr/bin/h2",
            0,
            None,
        );
        assert!(!fresh_claude.contains(&"--resume".to_string()));
        assert!(fresh_claude.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(fresh_claude.contains(&"--settings".to_string()));
        assert!(fresh_claude.contains(&"--session-id".to_string()));
        assert!(!fresh_claude.contains(&"old-sid".to_string()));

        // Login-shell wrapper (exact Herdr interactive PTY launch shape) preserves prefix & appends CLI flags.
        let wrapper_argv = vec![
            "sh".into(),
            "-c".into(),
            "exec \"$shell\" -lic '\"$@\"; exec \"$0\" -li' \"$shell\" \"$@\"".into(),
            "sh".into(),
            "claude".into(),
            "--dangerously-skip-permissions".into(),
        ];
        assert_eq!(pty_command_offset(&wrapper_argv), Some(4));
        assert_eq!(PtyCliKind::detect(&wrapper_argv), PtyCliKind::Claude);
        let wrapped_rebuilt = build_rollback_argv(
            PtyCliKind::Claude,
            &wrapper_argv,
            &dir,
            "/usr/bin/h2",
            1,
            Some("fork-wrapped-sid"),
        );
        assert_eq!(&wrapped_rebuilt[..5], ["sh", "-c", wrapper_argv[2].as_str(), "sh", "claude"]);
        assert!(wrapped_rebuilt.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(wrapped_rebuilt.contains(&"--settings".to_string()));
        assert!(wrapped_rebuilt.windows(2).any(|w| w == ["--resume", "fork-wrapped-sid"]));

        // User-started Claude inside an interactive `/usr/bin/zsh` wraps resumed CLI back into login shell.
        let manual_shell = build_rollback_argv_for_terminal(
            PtyCliKind::Claude,
            &["/usr/bin/zsh".into()],
            Some(&["claude".into(), "--dangerously-skip-permissions".into()]),
            &dir,
            "/usr/bin/h2",
            2,
            Some("fork-manual-sid"),
        );
        assert_eq!(&manual_shell[..5], ["/usr/bin/zsh", "-lic", "\"$@\"; exec \"$0\" -li", "/usr/bin/zsh", "claude"]);
        assert!(manual_shell.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(manual_shell.contains(&"--settings".to_string()));
        assert!(manual_shell.windows(2).any(|w| w == ["--resume", "fork-manual-sid"]));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fork_claude_jsonl_truncates_after_target_turn_and_rewrites_session_id() {
        let dir = crate::test_tmp().join(format!("h2-claude-fork-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("orig-sid.jsonl");
        let lines = [
            json!({"type":"user","sessionId":"orig-sid","message":{"role":"user","content":"turn 1 task"}}),
            json!({"type":"assistant","sessionId":"orig-sid","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Edit","input":{}}]}}),
            json!({"type":"user","sessionId":"orig-sid","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"},{"type":"text","text":"<system-reminder>keep inside turn 1</system-reminder>"}]}}),
            json!({"type":"assistant","sessionId":"orig-sid","message":{"role":"assistant","content":[{"type":"text","text":"done turn 1"}]}}),
            json!({"type":"queue-operation","sessionId":"orig-sid","operation":"enqueue"}),
            json!({"type":"user","sessionId":"orig-sid","message":{"role":"user","content":"turn 2 mistake"}}),
            json!({"type":"assistant","sessionId":"orig-sid","message":{"role":"assistant","content":[{"type":"text","text":"broke code"}]}}),
        ];
        std::fs::write(&src, lines.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n") + "\n").unwrap();

        let (forked, prompt) = fork_claude_jsonl(&src, 1, "fork-uuid-456").unwrap();
        assert_eq!(prompt.as_deref(), Some("turn 2 mistake"));
        let out_raw = std::fs::read_to_string(&forked).unwrap();
        let out_lines: Vec<Value> = out_raw.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(out_lines.len(), 4, "mixed tool_result stays in turn 1; queue-operation and turn 2 trimmed");
        assert!(out_lines.iter().all(|v| v["sessionId"] == "fork-uuid-456"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fork_codex_jsonl_cuts_preamble_of_dropped_turn_and_rewrites_session_meta() {
        let dir = crate::test_tmp().join(format!("h2-codex-fork-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("rollout-2026-11-18T10-00-00-orig-codex.jsonl");
        let lines = [
            json!({"type":"session_meta","payload":{"id":"orig-codex","cwd":"/repo"}}),
            json!({"type":"turn_context","payload":{"model":"gpt-5"}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"step 1"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done 1"}]}}),
            json!({"type":"event_msg","payload":{"type":"task_complete"}}),
            json!({"type":"turn_context","payload":{"model":"gpt-5"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"step 2 bad"}]}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"step 2 bad"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"bad 2"}]}}),
        ];
        std::fs::write(&src, lines.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n") + "\n").unwrap();

        let (forked, prompt) = fork_codex_jsonl(&src, 1, "new-codex-id").unwrap();
        assert_eq!(prompt.as_deref(), Some("step 2 bad"));
        let out_raw = std::fs::read_to_string(&forked).unwrap();
        let out_lines: Vec<Value> = out_raw.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(out_lines.len(), 5, "step 2 preamble + user_message + assistant must be trimmed");
        assert_eq!(out_lines[0]["payload"]["id"], "new-codex-id");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn agy_plugin_installs_valid_hooks_json_and_prepares_handoff() {
        let dir = crate::test_tmp().join(format!("h2-agy-plugin-{}", crate::platform::random_hex()));
        let plugin_dir = ensure_agy_plugin_at(&dir, "/usr/bin/h2").unwrap();
        let hooks: Value =
            serde_json::from_str(&std::fs::read_to_string(plugin_dir.join("hooks.json")).unwrap()).unwrap();
        assert!(
            hooks["herdr2-checkpoints"]["SessionStart"][0]["command"]
                .as_str()
                .unwrap()
                .contains("--event SessionStart")
        );
        assert!(
            hooks["herdr2-checkpoints"]["PreInvocation"][0]["command"]
                .as_str()
                .unwrap()
                .contains("--event PreInvocation")
        );
        assert!(hooks["herdr2-checkpoints"]["Stop"][0]["command"].as_str().unwrap().contains("--event Stop"));

        let log = dir.join("transcript.jsonl");
        let agy_lines = [
            json!({"type":"USER_INPUT","content":"fix bug A","created_at":"2026-11-18T10:00:00Z"}),
            json!({"type":"PLANNER_RESPONSE","content":"Fixed bug A.","created_at":"2026-11-18T10:00:05Z"}),
            json!({"type":"USER_INPUT","content":"rewrite wrongly","created_at":"2026-11-18T10:01:00Z"}),
            json!({"type":"PLANNER_RESPONSE","content":"Broken","created_at":"2026-11-18T10:01:05Z"}),
        ];
        std::fs::write(&log, agy_lines.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n") + "\n").unwrap();

        let (handoff, restored) = prepare_agy_rollback(&log, 1);
        assert_eq!(restored.as_deref(), Some("rewrite wrongly"));
        let h = handoff.unwrap();
        assert!(h.contains("fix bug A"));
        assert!(h.contains("Fixed bug A."));
        assert!(!h.contains("Broken"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
