use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::checkpoint::{capture_target_checkpoint, capture_target_checkpoint_with_session_turn, random_uuid_v4};
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
            "claude" | "claude-code" => return Self::Claude,
            "codex" => return Self::Codex,
            "agy" | "antigravity" | "antigravity-cli" => return Self::Agy,
            _ => {}
        }
        let lower = raw.replace('\\', "/").to_ascii_lowercase();
        if lower.contains("/claude/versions/") || lower.contains("/claude-code/") {
            return Self::Claude;
        }
        if lower.contains("/codex/") {
            return Self::Codex;
        }
        if lower.contains("/antigravity/") {
            return Self::Agy;
        }
        Self::Generic
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
    matches!(
        stem,
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "ash" | "fish" | "nu" | "tcsh" | "pwsh" | "powershell" | "cmd"
    )
}

/// Returns the shell executable if `argv` launches an interactive shell (with or without login/interactive flags,
/// e.g. `["/usr/bin/zsh"]`, `["/usr/bin/zsh", "-l"]`, `["/bin/bash", "-li"]`, `["pwsh.exe", "-NoLogo"]`).
fn extract_interactive_shell(argv: &[String]) -> Option<String> {
    let first = argv.first()?;
    if !is_shell_stem(&exe_stem(first)) {
        return None;
    }
    let has_command = argv.iter().skip(1).any(|arg| {
        (!arg.starts_with('-') && !arg.starts_with('+'))
            || matches!(arg.split('=').next(), Some("--command" | "--commands" | "-Command" | "/c" | "/C" | "/k" | "/K"))
            || (arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains('c'))
    });
    (!has_command).then(|| first.clone())
}

/// Returns the outer shell executable if `argv` is a Herdr login-shell wrapper around a CLI command.
fn extract_wrapper_shell(argv: &[String]) -> Option<String> {
    let cmd_idx = pty_command_offset(argv)?;
    if cmd_idx < 4 {
        return None;
    }
    let sh = argv.get(cmd_idx.saturating_sub(4))?;
    is_shell_stem(&exe_stem(sh)).then(|| sh.clone())
}

fn wrap_cli_in_shell(shell: &str, cli_argv: Vec<String>) -> Vec<String> {
    let stem = exe_stem(shell);
    if stem == "fish" {
        let mut out = vec![
            shell.to_string(),
            "-lic".into(),
            "command $argv[2..]; exec $argv[1] -li".into(),
            shell.to_string(),
        ];
        out.extend(cli_argv);
        return out;
    }
    if matches!(stem.as_str(), "pwsh" | "powershell") {
        let joined = cli_argv.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ");
        return vec![shell.to_string(), "-NoExit".into(), "-Command".into(), format!("& {joined}")];
    }
    let mut out = vec![shell.to_string(), "-lic".into(), "\"$@\"; exec \"$0\" -li".into(), shell.to_string()];
    out.extend(cli_argv);
    out
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
    let from_argv = PtyCliKind::detect(&t.argv);
    if from_argv != PtyCliKind::Generic {
        return from_argv;
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

/// Extracts a stable CLI session ID from a discovered transcript file path.
pub(crate) fn session_id_from_transcript(cli_kind: &str, path: &Path) -> Option<String> {
    match cli_kind {
        "claude" => {
            let stem = path.file_stem()?.to_string_lossy().trim().to_string();
            (!stem.is_empty()).then_some(stem)
        }
        "codex" => {
            if let Ok(raw) = std::fs::read_to_string(path) {
                for line in raw.lines().take(12) {
                    if let Ok(v) = serde_json::from_str::<Value>(line.trim())
                        && v.get("type").and_then(Value::as_str) == Some("session_meta")
                        && let Some(sid) = v.pointer("/payload/id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
                    {
                        return Some(sid.to_string());
                    }
                }
            }
            let stem = path.file_stem()?.to_string_lossy();
            if stem.len() >= 36 {
                let tail = &stem[stem.len() - 36..];
                if tail.chars().filter(|&c| c == '-').count() == 4 {
                    return Some(tail.to_string());
                }
            }
            (!stem.is_empty()).then(|| stem.to_string())
        }
        "agy" => {
            let stem = path.file_stem()?.to_string_lossy().trim().to_string();
            if !stem.is_empty() && stem != "transcript" {
                return Some(stem);
            }
            path.parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().trim().to_string())
                .filter(|s| !s.is_empty())
        }
        _ => None,
    }
}

/// Returns the active command argv if the terminal is currently running a resumed CLI
/// (`-c`, `--continue`, `-r`, `--resume`, `codex resume`).
///
/// When OS foreground tracking knows the live foreground process (`t.foreground`), that process is
/// authoritative: we never fall back to a historical `t.argv` (e.g. a post-rollback login-shell
/// wrapper containing `--resume <old-sid>` after the CLI has exited back to `zsh -li` or been
/// replaced by a fresh non-resumed `claude`).
pub(crate) fn terminal_active_resume_argv(t: &crate::terminals::Terminal) -> Option<&[String]> {
    if let Some(ref fg) = t.foreground
        && !fg.argv.is_empty()
    {
        if crate::foreground::is_shell_root(fg) {
            return None;
        }
        return crate::pty_transcript::argv_has_resume(&fg.argv).then_some(fg.argv.as_slice());
    }
    crate::pty_transcript::argv_has_resume(&t.argv).then_some(t.argv.as_slice())
}

/// Returns true when a terminal running a resumed CLI (`-c`, `--continue`, `--resume`, `codex resume`)
/// should attempt to re-attach archived session checkpoints.
pub(crate) fn needs_resumed_reattach(tc: &crate::checkpoint::TargetCheckpoints, fg_pid: Option<u32>) -> bool {
    let only_anonymous_baseline =
        tc.cli_session_id.is_none() && tc.checkpoints.iter().all(|c| c.turn == 0);
    if tc.checkpoints.is_empty() || only_anonymous_baseline {
        return true;
    }
    fg_pid.is_some() && tc.cli_fg_pid != fg_pid
}

/// Re-attaches archived checkpoints when a user resumes a previous CLI conversation
/// (e.g. `claude --dangerously-skip-permissions -c`, `claude --resume <id>`, `codex resume`).
pub async fn reattach_resumed_cli_checkpoints(core: &Arc<Core>, id: u32) -> bool {
    let (addr, cwd, fg_pid, cli_kind, argv, only_anonymous_or_empty, current_sid) = {
        let mut st = core.lock();
        if st.pty_respawning.contains(&id) {
            return false;
        }
        let Some(t) = st.pty_reg.terminals.get(&id).cloned() else {
            return false;
        };
        if !t.is_live() {
            return false;
        }
        let Some(active_argv) = terminal_active_resume_argv(&t).map(<[String]>::to_vec) else {
            return false;
        };
        let cli_kind = detect_terminal_cli_kind(&t, None);
        if cli_kind == PtyCliKind::Generic {
            return false;
        }
        let cwd = t
            .foreground
            .as_ref()
            .map(|f| f.cwd.as_str())
            .filter(|s| !s.is_empty())
            .or(t.last_cwd.as_deref())
            .unwrap_or(&t.cwd)
            .to_string();
        let fg_pid = t.foreground.as_ref().and_then(|f| u32::try_from(f.pid).ok());
        let tc = core.target_checkpoints(&mut st, &t.address);
        if !needs_resumed_reattach(&tc, fg_pid) {
            return false;
        }
        let only_anonymous_or_empty = tc.checkpoints.is_empty()
            || (tc.cli_session_id.is_none() && tc.checkpoints.iter().all(|c| c.turn == 0));
        (
            t.address,
            cwd,
            fg_pid,
            cli_kind,
            active_argv,
            only_anonymous_or_empty,
            tc.cli_session_id,
        )
    };

    let kind_str = cli_kind.as_str().to_string();
    let explicit_sid = match cli_kind {
        PtyCliKind::Claude | PtyCliKind::Agy => extract_flag_value(&argv, &["--resume", "-r", "--conversation"]),
        PtyCliKind::Codex => extract_codex_resume_id(&argv),
        PtyCliKind::Generic => None,
    };

    let cwd_c = cwd.clone();
    let kind_c = kind_str.clone();
    let explicit_c = explicit_sid.clone();
    let core_c = Arc::clone(core);
    let resolved = tokio::task::spawn_blocking(move || {
        // Resolve transcript file for explicit sid or `-c` / `--continue` (allow_older_fallback = true).
        let transcript_path = match (&kind_c[..], explicit_c.as_deref()) {
            ("claude", Some(s)) => find_claude_session_file(&cwd_c, s),
            ("codex", Some(s)) => find_codex_rollout_by_sid(&cwd_c, s),
            ("agy", Some(s)) => find_agy_transcript_by_cid(s),
            _ => discover_cli_transcript(&kind_c, &cwd_c, 0, None, true),
        };
        let resolved_sid = explicit_c.or_else(|| {
            transcript_path
                .as_deref()
                .and_then(|p| session_id_from_transcript(&kind_c, p))
        });

        let archived = core_c
            .load_archived_cli_session(&kind_c, resolved_sid.as_deref(), Some(&cwd_c))
            .and_then(|mut arch| {
                let repo_dir = arch
                    .repo_root
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&cwd_c);
                if let Some(repo) = crate::checkpoint::probe_repo(Path::new(repo_dir)) {
                    arch.checkpoints
                        .retain(|c| crate::checkpoint::ref_exists(&repo, &c.ref_name));
                    if arch.repo_root.is_none() {
                        arch.repo_root = Some(repo.toplevel.to_string_lossy().to_string());
                    }
                }
                if arch.checkpoints.is_empty() {
                    return None;
                }
                if arch.cli_transcript_path.is_none() {
                    arch.cli_transcript_path = transcript_path.as_ref().map(|p| p.to_string_lossy().to_string());
                }
                Some(arch)
            });

        (archived, resolved_sid, transcript_path)
    })
    .await
    .ok();

    let Some((archived_opt, resolved_sid, transcript_path)) = resolved else {
        return false;
    };

    let Some(mut restored) = archived_opt else {
        // No archived checkpoints exist for this resumed session, but bind its discovered
        // `cli_session_id` & `cli_transcript_path` when the target is blank/anonymous so we don't
        // re-scan on every poll and future turns archive under the right session ID.
        if only_anonymous_or_empty && (resolved_sid.is_some() || transcript_path.is_some()) {
            let mut st = core.lock();
            if st.pty_reg.terminals.get(&id).is_some_and(|t| t.is_live()) {
                let mut tc = core.target_checkpoints(&mut st, &addr);
                tc.cli_kind = Some(kind_str);
                if let Some(s) = resolved_sid {
                    tc.cli_session_id = Some(s);
                }
                if let Some(p) = transcript_path {
                    tc.cli_transcript_path = Some(p.to_string_lossy().to_string());
                }
                if let Some(pid) = fg_pid {
                    tc.cli_fg_pid = Some(pid);
                }
                core.save_target_checkpoints(&mut st, tc);
            }
        }
        return false;
    };

    let sid_final = resolved_sid
        .or_else(|| restored.cli_session_id.clone())
        .unwrap_or_default();
    if !only_anonymous_or_empty && !sid_final.is_empty() && current_sid.as_deref() == Some(sid_final.as_str()) {
        // Same session already attached; just record the new foreground PID so we don't re-scan.
        let mut st = core.lock();
        let mut tc = core.target_checkpoints(&mut st, &addr);
        tc.cli_fg_pid = fg_pid;
        core.save_target_checkpoints(&mut st, tc);
        return false;
    }

    let max_turn = restored.checkpoints.iter().map(|c| c.turn).max().unwrap_or(0);
    restored.addr = addr.clone();
    restored.cli_kind = Some(kind_str);
    if !sid_final.is_empty() {
        restored.cli_session_id = Some(sid_final.clone());
    }
    restored.cli_fg_pid = fg_pid;
    restored.pending_handoff = None;
    restored.pending_rollback_turn = None;
    restored.active_turn_num = None;
    restored.active_turn_prompt = None;

    let mut st = core.lock();
    if !st.pty_reg.terminals.get(&id).is_some_and(|t| t.is_live()) {
        return false;
    }
    core.save_target_checkpoints(&mut st, restored);
    let mut turn_changed = false;
    if let Some(t) = st.pty_reg.terminals.get_mut(&id)
        && t.turn != max_turn
    {
        t.turn = max_turn;
        turn_changed = true;
        core.save_pty_soon();
    }
    if turn_changed {
        core.emit_pty(&mut st, "turn", id, json!({ "turn": max_turn }));
    }
    core.emit_pty(&mut st, "checkpoint", id, json!({ "restored_session": sid_final }));
    true
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
    if kind != PtyCliKind::Generic
        && let Some(cmd_idx) = pty_command_offset(orig_argv)
        && PtyCliKind::detect_cmd_token(&orig_argv[cmd_idx]) == kind
    {
        return build_rollback_argv(kind, orig_argv, hooks_dir, h2_exe, to_turn, fork_session_id);
    }
    if let Some(sh) = extract_interactive_shell(orig_argv).or_else(|| extract_wrapper_shell(orig_argv)) {
        if kind != PtyCliKind::Generic {
            let cli_base: Vec<String> = fg_argv
                .filter(|a| !a.is_empty() && PtyCliKind::detect(a) == kind)
                .map(<[String]>::to_vec)
                .unwrap_or_else(|| vec![kind.as_str().to_string()]);
            let rebuilt_cli = build_direct_cli_argv(kind, &cli_base, hooks_dir, h2_exe, to_turn, fork_session_id);
            return wrap_cli_in_shell(&sh, rebuilt_cli);
        }
        if let Some(fg) = fg_argv.filter(|a| !a.is_empty() && extract_interactive_shell(a).is_none()) {
            return wrap_cli_in_shell(&sh, fg.to_vec());
        }
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
            let raw_bin = orig_argv.first().cloned().unwrap_or_else(|| "claude".into());
            let bin = if raw_bin.replace('\\', "/").to_ascii_lowercase().contains("/claude/versions/") {
                "claude".to_string()
            } else {
                raw_bin
            };
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
                    || matches!(arg.as_str(), "-c" | "--continue" | "--fork-session" | "--reply-on-resume")
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
                let sid = fork_session_id
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(random_uuid_v4);
                out.push("--session-id".into());
                out.push(sid);
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
            let raw_bin = orig_argv.first().cloned().unwrap_or_else(|| "agy".into());
            let bin = if raw_bin.ends_with(".real") || raw_bin.ends_with(".real.exe") {
                "agy".to_string()
            } else {
                raw_bin
            };
            let mut out = vec![bin];
            let mut i = 1usize;
            while i < orig_argv.len() {
                let arg = &orig_argv[i];
                if matches!(arg.as_str(), "--conversation" | "-c" | "--resume" | "-r") {
                    i += 2;
                    continue;
                }
                if arg.starts_with("--conversation=") || arg.starts_with("--resume=") {
                    i += 1;
                    continue;
                }
                out.push(arg.clone());
                i += 1;
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
    let mut kept_uuids = std::collections::HashSet::new();
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

        if let Some(uuid) = val.get("uuid").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            kept_uuids.insert(uuid.to_string());
        }
        if let Some(obj) = val.as_object_mut()
            && obj.contains_key("sessionId")
        {
            obj.insert("sessionId".into(), Value::String(fork_uuid.to_string()));
        }
        kept_lines.push(serde_json::to_string(&val).unwrap_or_else(|_| trimmed.to_string()));
    }

    // Trim trailing queue-operation / preamble events belonging to a discarded turn
    // (e.g. a `file-history-snapshot` whose `messageId` references the truncated user turn).
    while kept_lines.last().is_some_and(|line| {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("queue-operation") => true,
            Some("file-history-snapshot") => v
                .get("messageId")
                .and_then(Value::as_str)
                .is_some_and(|mid| !kept_uuids.contains(mid)),
            _ => false,
        }
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

fn terminal_cli_fg_pid(st: &crate::runtime::State, id: u32) -> Option<u32> {
    let t = st.pty_reg.terminals.get(&id)?;
    let fg = t.foreground.as_ref()?;
    let pid = u32::try_from(fg.pid).ok()?;
    (Some(pid) != t.pid).then_some(pid)
}

/// Clears previous CLI session checkpoints when a hook arrives for a different CLI kind or session ID
/// (e.g. exiting Agent A and starting Agent B, or running `/clear` / `/new` inside the same CLI).
fn reset_if_new_cli_session(
    core: &Core,
    st: &mut crate::runtime::State,
    id: u32,
    addr: &str,
    cwd: &str,
    new_kind: &str,
    new_sid: Option<&str>,
) {
    let tc = core.target_checkpoints(st, addr);
    if tc.pending_rollback_turn.is_some() || tc.pending_handoff.is_some() {
        return;
    }
    let kind_changed = tc
        .cli_kind
        .as_deref()
        .is_some_and(|old| !old.is_empty() && old != "generic" && old != new_kind);
    let sid_changed = match (tc.cli_session_id.as_deref(), new_sid) {
        (Some(old), Some(new)) if !old.is_empty() && !new.is_empty() && old != new => true,
        _ => false,
    };
    if !kind_changed && !sid_changed {
        // If current target has no checkpoints yet but the incoming hook belongs to an archived CLI session, restore it.
        if tc.checkpoints.is_empty()
            && let Some(sid) = new_sid.filter(|s| !s.is_empty())
            && let Some(mut archived) = core.load_archived_cli_session(new_kind, Some(sid), Some(cwd))
        {
            archived.addr = addr.to_string();
            archived.cli_kind = Some(new_kind.to_string());
            archived.cli_session_id = Some(sid.to_string());
            archived.cli_fg_pid = terminal_cli_fg_pid(st, id);
            let max_turn = archived.checkpoints.iter().map(|c| c.turn).max().unwrap_or(0);
            core.save_target_checkpoints(st, archived);
            if let Some(t) = st.pty_reg.terminals.get_mut(&id)
                && t.turn != max_turn
            {
                t.turn = max_turn;
                core.emit_pty(st, "turn", id, json!({ "turn": max_turn }));
            }
            core.emit_pty(st, "checkpoint", id, json!({ "restored_session": sid }));
        }
        return;
    }
    core.detach_target_checkpoints(st, addr, cwd);
    st.pty_prompts.remove(&id);
    if let Some(t) = st.pty_reg.terminals.get_mut(&id) {
        t.turn = 0;
    }
    if let Some(sid) = new_sid.filter(|s| !s.is_empty())
        && let Some(mut archived) = core.load_archived_cli_session(new_kind, Some(sid), Some(cwd))
    {
        archived.addr = addr.to_string();
        archived.cli_kind = Some(new_kind.to_string());
        archived.cli_session_id = Some(sid.to_string());
        archived.cli_fg_pid = terminal_cli_fg_pid(st, id);
        let max_turn = archived.checkpoints.iter().map(|c| c.turn).max().unwrap_or(0);
        core.save_target_checkpoints(st, archived);
        if let Some(t) = st.pty_reg.terminals.get_mut(&id) {
            t.turn = max_turn;
        }
        core.emit_pty(st, "turn", id, json!({ "turn": max_turn }));
        core.emit_pty(st, "checkpoint", id, json!({ "restored_session": sid }));
    } else {
        core.emit_pty(st, "turn", id, json!({ "turn": 0 }));
        core.emit_pty(st, "checkpoint", id, json!({ "cleared": true }));
    }
}

/// Extracts `(1-based user turn number, prompt text)` of the latest user turn in a CLI transcript.
fn latest_transcript_user_turn(kind: &str, path: &Path) -> (Option<u64>, Option<String>) {
    let entries = parse_cli_transcript(kind, path);
    entries
        .iter()
        .rev()
        .find_map(|e| match e {
            Entry::User { turn, text, .. } => {
                let clean = strip_h2_envelope(text);
                let prompt = (!clean.is_empty()).then_some(clean);
                Some(((*turn > 0).then_some(*turn), prompt))
            }
            _ => None,
        })
        .unwrap_or((None, None))
}

/// Resolves the 1-based user-turn index in the CLI's JSONL transcript corresponding to `to_turn`.
fn resolve_rollback_transcript_turn(checkpoints: &[crate::checkpoint::CheckpointMeta], to_turn: u64) -> u64 {
    if to_turn == 0 {
        return 0;
    }
    if let Some(st) = checkpoints.iter().find(|c| c.turn == to_turn).and_then(|c| c.session_turn) {
        return st;
    }
    let ordinal = checkpoints
        .iter()
        .filter(|c| c.turn > 0 && c.turn <= to_turn && c.label.is_none())
        .count() as u64;
    if ordinal > 0 { ordinal } else { to_turn }
}

/// Returns true for Codex CLI's internal background helper LLM calls (such as automatic
/// single-line thread-title generation) that also trigger `codex-notify` hooks.
fn is_codex_internal_helper_prompt(prompt: &str) -> bool {
    let lower = prompt.trim_start().to_ascii_lowercase();
    lower.starts_with("generate a concise, single-line task title")
        || (lower.starts_with("generate a ")
            && lower.contains("title")
            && lower.contains("do not answer the request"))
        || (lower.contains("do not answer the request.") && lower.contains("\nuser prompt:\n"))
}

/// Returns true when `checkpoints` already contains an automatic turn snapshot for `session_turn`,
/// preventing duplicate snapshots when both a CLI hook (`codex-notify`) and OSC `turn_ended` fire
/// for the same turn, or when `codex resume` / `claude -c` pulses `working -> idle` on load.
fn has_captured_session_turn(
    checkpoints: &[crate::checkpoint::CheckpointMeta],
    session_turn: Option<u64>,
    prompt: Option<&str>,
) -> bool {
    let Some(sturn) = session_turn.filter(|&n| n > 0) else {
        return false;
    };
    checkpoints.iter().any(|c| {
        if c.turn == 0 || c.label.is_some() {
            return false;
        }
        if c.session_turn == Some(sturn) {
            return true;
        }
        c.session_turn.is_none()
            && prompt.is_some_and(|p| !p.is_empty() && c.prompt.as_deref() == Some(p))
    })
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
                    reset_if_new_cli_session(core, &mut st, id, &addr, &cwd, "claude", sid.as_deref());
                    let fg_pid = terminal_cli_fg_pid(&st, id);
                    let mut tc = core.target_checkpoints(&mut st, &addr);
                    tc.cli_kind = Some("claude".into());
                    if let Some(pid) = fg_pid {
                        tc.cli_fg_pid = Some(pid);
                    }
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
                let (turn_num, prompt_val, resolved_tpath) = {
                    let mut st = core.lock();
                    reset_if_new_cli_session(core, &mut st, id, &addr, &cwd, "claude", sid.as_deref());
                    let fg_pid = terminal_cli_fg_pid(&st, id);
                    let mut tc = core.target_checkpoints(&mut st, &addr);
                    tc.cli_kind = Some("claude".into());
                    if let Some(pid) = fg_pid {
                        tc.cli_fg_pid = Some(pid);
                    }
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
                    let resolved_tpath = tc.cli_transcript_path.clone();
                    core.save_target_checkpoints(&mut st, tc);
                    (turn_num, pr, resolved_tpath)
                };
                let (session_turn, parsed_prompt) = match resolved_tpath.as_deref().map(PathBuf::from).filter(|p| p.is_file()) {
                    Some(p) => tokio::task::spawn_blocking(move || latest_transcript_user_turn("claude", &p))
                        .await
                        .unwrap_or((None, None)),
                    None => (None, None),
                };
                let _ = capture_target_checkpoint_with_session_turn(
                    core,
                    TargetKey::Terminal(id),
                    Some(turn_num),
                    session_turn,
                    prompt_val.or(parsed_prompt),
                    None,
                    true,
                    false,
                )
                .await;
            }
            json!({})
        }
        "codex-notify" => {
            let input_msgs = payload.get("input-messages").and_then(Value::as_array);
            if input_msgs.is_some_and(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .any(is_codex_internal_helper_prompt)
            }) {
                return json!({});
            }
            let sid = payload
                .get("thread-id")
                .or_else(|| payload.get("session-id"))
                .or_else(|| payload.get("session_id"))
                .or_else(|| payload.get("conversation-id"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let turn_prompt = input_msgs
                .and_then(|arr| arr.last())
                .and_then(Value::as_str)
                .map(strip_h2_envelope)
                .filter(|s| !s.is_empty());

            let (needs_baseline, turn_num, effective_prompt, resolved_tpath) = {
                let mut st = core.lock();
                reset_if_new_cli_session(core, &mut st, id, &addr, &cwd, "codex", sid.as_deref());
                let fg_pid = terminal_cli_fg_pid(&st, id);
                let (started_ms, resumed) = st
                    .pty_reg
                    .terminals
                    .get(&id)
                    .map(|t| (t.program_started_ms.unwrap_or(0), terminal_active_resume_argv(t).is_some()))
                    .unwrap_or((0, false));
                let mut tc = core.target_checkpoints(&mut st, &addr);
                tc.cli_kind = Some("codex".into());
                if let Some(pid) = fg_pid {
                    tc.cli_fg_pid = Some(pid);
                }
                if let Some(ref s) = sid {
                    tc.cli_session_id = Some(s.clone());
                    if tc.cli_transcript_path.is_none()
                        && let Some(found) = find_codex_rollout_by_sid(&cwd, s)
                    {
                        tc.cli_transcript_path = Some(found.to_string_lossy().to_string());
                    }
                }
                if tc.cli_transcript_path.is_none()
                    && let Some(found) = discover_cli_transcript(
                        "codex",
                        &cwd,
                        if resumed { 0 } else { started_ms },
                        if resumed { None } else { fg_pid },
                        resumed,
                    )
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
                let resolved_tpath = tc.cli_transcript_path.clone();
                core.save_target_checkpoints(&mut st, tc);
                (needs_baseline, turn_num, effective_prompt, resolved_tpath)
            };
            if needs_baseline {
                let _ =
                    capture_target_checkpoint(core, TargetKey::Terminal(id), Some(0), None, None, true, false).await;
            }
            let (session_turn, parsed_prompt) = match resolved_tpath.as_deref().map(PathBuf::from).filter(|p| p.is_file()) {
                Some(p) => tokio::task::spawn_blocking(move || latest_transcript_user_turn("codex", &p))
                    .await
                    .unwrap_or((None, None)),
                None => (None, None),
            };
            let final_prompt = effective_prompt.or(parsed_prompt);
            let already_captured = {
                let mut st = core.lock();
                let tc = core.target_checkpoints(&mut st, &addr);
                has_captured_session_turn(&tc.checkpoints, session_turn, final_prompt.as_deref())
            };
            if !already_captured {
                let _ = capture_target_checkpoint_with_session_turn(
                    core,
                    TargetKey::Terminal(id),
                    Some(turn_num),
                    session_turn,
                    final_prompt,
                    None,
                    true,
                    false,
                )
                .await;
            }
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
                        reset_if_new_cli_session(core, &mut st, id, &addr, &cwd, "agy", cid.as_deref());
                        let fg_pid = terminal_cli_fg_pid(&st, id);
                        let mut tc = core.target_checkpoints(&mut st, &addr);
                        tc.cli_kind = Some("agy".into());
                        if let Some(pid) = fg_pid {
                            tc.cli_fg_pid = Some(pid);
                        }
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
                        reset_if_new_cli_session(core, &mut st, id, &addr, &cwd, "agy", cid.as_deref());
                        let fg_pid = terminal_cli_fg_pid(&st, id);
                        let mut tc = core.target_checkpoints(&mut st, &addr);
                        tc.cli_kind = Some("agy".into());
                        if let Some(pid) = fg_pid {
                            tc.cli_fg_pid = Some(pid);
                        }
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
                    let (turn_num, prompt_val, resolved_tpath) = {
                        let mut st = core.lock();
                        reset_if_new_cli_session(core, &mut st, id, &addr, &cwd, "agy", cid.as_deref());
                        let fg_pid = terminal_cli_fg_pid(&st, id);
                        let mut tc = core.target_checkpoints(&mut st, &addr);
                        tc.cli_kind = Some("agy".into());
                        if let Some(pid) = fg_pid {
                            tc.cli_fg_pid = Some(pid);
                        }
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
                        let resolved_tpath = tc.cli_transcript_path.clone();
                        core.save_target_checkpoints(&mut st, tc);
                        (turn_num, pr, resolved_tpath)
                    };
                    let (session_turn, parsed_prompt) = match resolved_tpath.as_deref().map(PathBuf::from).filter(|p| p.is_file()) {
                        Some(p) => tokio::task::spawn_blocking(move || latest_transcript_user_turn("agy", &p))
                            .await
                            .unwrap_or((None, None)),
                        None => (None, None),
                    };
                    let _ = capture_target_checkpoint_with_session_turn(
                        core,
                        TargetKey::Terminal(id),
                        Some(turn_num),
                        session_turn,
                        prompt_val.or(parsed_prompt),
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
    // If this terminal is running a resumed CLI (`claude -c`, `claude --resume`, `codex resume`)
    // and hasn't re-attached its archived checkpoints yet, do so BEFORE capturing a blank T0 baseline!
    let should_reattach_first = {
        let mut st = core.lock();
        if st.pty_respawning.contains(&id) {
            false
        } else if let Some(t) = st.pty_reg.terminals.get(&id).cloned()
            && t.is_live()
        {
            if terminal_active_resume_argv(&t).is_some() {
                let fg_pid = t.foreground.as_ref().and_then(|f| u32::try_from(f.pid).ok());
                let tc = core.target_checkpoints(&mut st, &t.address);
                needs_resumed_reattach(&tc, fg_pid)
            } else {
                false
            }
        } else {
            false
        }
    };
    if should_reattach_first {
        let _ = reattach_resumed_cli_checkpoints(core, id).await;
    }

    let (
        addr,
        cwd,
        fg_pid,
        started_ms,
        cli_kind,
        resumed,
        expected_turn,
        needs_baseline,
        needs_discover,
        hook_already_done,
    ) = {
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
        let fg_pid = t.foreground.as_ref().and_then(|f| u32::try_from(f.pid).ok());
        let started_ms = t.program_started_ms.unwrap_or(0);
        let resumed = terminal_active_resume_argv(&t).is_some();
        let mut tc = core.target_checkpoints(&mut st, &t.address);
        let cli_kind = detect_terminal_cli_kind(&t, tc.cli_kind.as_deref());
        tc.cli_kind = Some(cli_kind.as_str().into());
        if let Some(pid) = fg_pid.filter(|&p| Some(p) != t.pid) {
            tc.cli_fg_pid = Some(pid);
        }
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
        // For clean (non-resumed) CLI sessions, defer transcript discovery until `turn_ended` so we
        // never race ahead of the new session's JSONL creation and accidentally bind an older session.
        let needs_discover = resumed && tc.cli_transcript_path.is_none() && cli_kind != PtyCliKind::Generic;
        core.save_target_checkpoints(&mut st, tc);
        (
            t.address,
            cwd,
            fg_pid,
            started_ms,
            cli_kind,
            resumed,
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
                discover_cli_transcript(&kind_str, &cwd_c, 0, None, true)
            })
            .await
            .ok()
            .flatten()
            {
                let mut st = core.lock();
                let mut tc = core.target_checkpoints(&mut st, &addr);
                let mut changed = false;
                if tc.cli_transcript_path.is_none() {
                    tc.cli_transcript_path = Some(found.to_string_lossy().to_string());
                    changed = true;
                }
                if tc.cli_session_id.is_none()
                    && let Some(sid) = session_id_from_transcript(cli_kind.as_str(), &found)
                {
                    tc.cli_session_id = Some(sid);
                    changed = true;
                }
                if changed {
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
    let mut session_turn: Option<u64> = None;
    if cli_kind != PtyCliKind::Generic {
        let kind_str = cli_kind.as_str().to_string();
        let cwd_c = cwd.clone();
        let tpath_c = cached_tpath.clone();
        let res = tokio::task::spawn_blocking(move || -> (Option<PathBuf>, Option<u64>, Option<String>) {
            // For non-resumed sessions, verify against the foreground process start time so an older
            // session's transcript can never hijack this turn's prompt/session_id.
            let discovered = discover_cli_transcript(
                &kind_str,
                &cwd_c,
                if resumed { 0 } else { started_ms },
                if resumed { None } else { fg_pid },
                resumed,
            );
            let path = if !resumed && discovered.is_some() {
                discovered
            } else {
                tpath_c
                    .as_deref()
                    .map(PathBuf::from)
                    .filter(|p| p.is_file())
                    .or(discovered)
            };
            let Some(ref p) = path else {
                return (None, None, None);
            };
            // On `turn_ended`, the turn that just completed is the latest user turn in the transcript.
            let (sturn, turn_prompt) = latest_transcript_user_turn(&kind_str, p);
            (path, sturn, turn_prompt)
        })
        .await
        .ok();
        if let Some((found_path, parsed_sturn, parsed_prompt)) = res {
            let had_transcript = found_path.is_some();
            if let Some(p) = found_path {
                let p_str = p.to_string_lossy().to_string();
                let mut st = core.lock();
                let mut tc = core.target_checkpoints(&mut st, &addr);
                let mut changed = false;
                if tc.cli_transcript_path.as_deref() != Some(p_str.as_str()) {
                    tc.cli_transcript_path = Some(p_str);
                    changed = true;
                }
                if let Some(sid) = session_id_from_transcript(cli_kind.as_str(), &p)
                    && tc.cli_session_id.as_deref() != Some(sid.as_str())
                {
                    tc.cli_session_id = Some(sid);
                    changed = true;
                }
                if changed {
                    core.save_target_checkpoints(&mut st, tc);
                }
            }
            session_turn = parsed_sturn;
            if prompt.is_none() {
                prompt = parsed_prompt;
            }
            // Ignore spurious startup `working -> idle` OSC transitions when the discovered CLI
            // transcript still has zero user turns.
            if had_transcript && session_turn.is_none() && prompt.is_none() {
                return;
            }
        }
        // Skip duplicate fallback capture when a hook (`codex-notify`, `claude` Stop) or prior
        // session turn already recorded this exact `session_turn` (e.g. `codex resume` startup pulse).
        let already_captured = {
            let mut st = core.lock();
            let tc = core.target_checkpoints(&mut st, &addr);
            has_captured_session_turn(&tc.checkpoints, session_turn, prompt.as_deref())
        };
        if already_captured {
            return;
        }
    }

    let _ = capture_target_checkpoint_with_session_turn(
        core,
        TargetKey::Terminal(id),
        Some(expected_turn),
        session_turn,
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
        transcript_turn,
        ref_owner,
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
        let transcript_turn = resolve_rollback_transcript_turn(&tc.checkpoints, to_turn);
        let ref_owner = tc.ref_namespace.clone().unwrap_or_else(|| t.address.clone());
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
            transcript_turn,
            ref_owner,
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
                .or_else(|| discover_cli_transcript("claude", &orig_cwd, started_ms, fg_pid, true)),
            PtyCliKind::Codex => sid
                .as_deref()
                .and_then(|s| find_codex_rollout_by_sid(&orig_cwd, s))
                .or_else(|| discover_cli_transcript("codex", &orig_cwd, started_ms, fg_pid, true)),
            PtyCliKind::Agy => sid
                .as_deref()
                .and_then(find_agy_transcript_by_cid)
                .or_else(|| discover_cli_transcript("agy", &orig_cwd, started_ms, fg_pid, true)),
            PtyCliKind::Generic => None,
        });

    let mut new_sid: Option<String> = None;
    let mut new_tpath: Option<String> = None;
    let mut new_handoff: Option<String> = None;
    let mut restored_prompt: Option<String> = None;
    let mut rewound_session = false;

    if to_turn == 0 {
        if cli_kind == PtyCliKind::Claude {
            new_sid = Some(random_uuid_v4());
        }
        // Still extract turn 1's user prompt if transcript exists so UI can pre-fill composer.
        if let Some(ref src) = transcript_src {
            match cli_kind {
                PtyCliKind::Claude => {
                    if let Ok((tmp_dest, prompt)) = fork_claude_jsonl(src, 0, &random_uuid_v4()) {
                        let _ = std::fs::remove_file(&tmp_dest);
                        restored_prompt = prompt;
                    }
                }
                PtyCliKind::Codex => {
                    if let Ok((tmp_dest, prompt)) = fork_codex_jsonl(src, 0, &random_uuid_v4()) {
                        let _ = std::fs::remove_file(&tmp_dest);
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
                let (dest, prompt) =
                    fork_claude_jsonl(src, transcript_turn, &fork_uuid).map_err(RpcError::internal)?;
                restored_prompt = prompt;
                if std::fs::metadata(&dest).is_ok_and(|m| m.len() > 0) {
                    new_sid = Some(fork_uuid);
                    new_tpath = Some(dest.to_string_lossy().to_string());
                    rewound_session = true;
                } else {
                    let _ = std::fs::remove_file(&dest);
                }
            }
            PtyCliKind::Codex => {
                let fork_uuid = random_uuid_v4();
                let (dest, prompt) =
                    fork_codex_jsonl(src, transcript_turn, &fork_uuid).map_err(RpcError::internal)?;
                restored_prompt = prompt;
                if std::fs::metadata(&dest).is_ok_and(|m| m.len() > 0) {
                    new_sid = Some(fork_uuid);
                    new_tpath = Some(dest.to_string_lossy().to_string());
                    rewound_session = true;
                } else {
                    let _ = std::fs::remove_file(&dest);
                }
            }
            PtyCliKind::Agy => {
                let (handoff, prompt) = prepare_agy_rollback(src, transcript_turn);
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
        let kill_deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2000);
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
        let dropped_refs: Vec<String> = {
            let mut st = core.lock();
            core.target_checkpoints(&mut st, &addr)
                .checkpoints
                .into_iter()
                .filter(|c| c.turn > to_turn)
                .map(|c| c.ref_name)
                .collect()
        };
        let _ckpt_guard = core.repo_ckpt_lock(&r.toplevel).lock_owned().await;
        let r_clone = r.clone();
        let ref_to_restore = target_ref.clone();
        let cwd_path = PathBuf::from(&orig_cwd);
        let ref_owner_clone = ref_owner.clone();
        let slug_clone = core.session_slug();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            if restore_files && let Some(ref ref_name) = ref_to_restore {
                crate::checkpoint::restore_checkpoint(&r_clone, ref_name, Some(&cwd_path))?;
            }
            let _ = crate::checkpoint::delete_checkpoint_refs_after(&r_clone, &slug_clone, &ref_owner_clone, to_turn);
            let _ = crate::checkpoint::delete_checkpoint_ref_names(&r_clone, &dropped_refs);
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

    // Ensure a live, non-draining current holder exists so terminals created before a core
    // restart migrate cleanly instead of failing with `holder is draining` on rollback respawn.
    core.ensure_holder()
        .await
        .map_err(|e| RpcError::new("unavailable", format!("failed to start current holder for {addr}: {e}"), ""))?;

    let spawn_rx = {
        let mut st = core.lock();
        let target_instance = st
            .current()
            .or_else(|| st.link(id))
            .map(|l| l.instance.clone())
            .ok_or_else(|| RpcError::new("unavailable", format!("{addr} holder disconnected"), ""))?;
        let mut screen = ghostty_vt::Terminal::new(cols, rows, settings.history_bytes());
        screen.set_xtversion(&identity);
        st.screens.insert(id, screen);
        st.relays.remove(&id);
        st.pty_prompts.remove(&id);
        core.output(&mut st, id, b"\x1bc");

        let mut tc = core.target_checkpoints(&mut st, &addr);
        tc.cli_kind = Some(cli_kind.as_str().into());
        if let Some(sid_val) = new_sid {
            tc.cli_session_id = Some(sid_val);
        } else if to_turn == 0 {
            tc.cli_session_id = None;
        }
        tc.cli_transcript_path = if to_turn == 0 { None } else { new_tpath };
        tc.cli_fg_pid = None;
        tc.pending_rollback_turn = new_handoff.as_ref().map(|_| to_turn);
        tc.pending_handoff = new_handoff;
        tc.active_turn_prompt = None;
        tc.active_turn_num = None;
        tc.checkpoints.retain(|c| c.turn <= to_turn);
        core.save_target_checkpoints(&mut st, tc);

        if let Some(t) = st.pty_reg.terminals.get_mut(&id) {
            t.holder = Some(target_instance);
            t.argv = new_argv.clone();
            t.cwd = orig_cwd.clone();
            t.last_cwd = Some(orig_cwd.clone());
            t.scope = Some(scope);
            t.foreground = None;
            t.restart_after_ms = 0;
            t.restart_delay_ms = 0;
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
            st.pty_respawning.remove(&id);
            respawn_guard.armed = false;
            if let Some(t) = st.pty_reg.terminals.get_mut(&id) {
                t.turn = to_turn;
            }
            core.emit_pty(&mut st, "turn", id, json!({ "turn": to_turn }));
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
            json!({"type":"user","uuid":"u1","sessionId":"orig-sid","message":{"role":"user","content":"turn 1 task"}}),
            json!({"type":"assistant","uuid":"a1","sessionId":"orig-sid","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Edit","input":{}}]}}),
            json!({"type":"user","uuid":"u1r","sessionId":"orig-sid","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"},{"type":"text","text":"<system-reminder>keep inside turn 1</system-reminder>"}]}}),
            json!({"type":"assistant","uuid":"a2","sessionId":"orig-sid","message":{"role":"assistant","content":[{"type":"text","text":"done turn 1"}]}}),
            json!({"type":"file-history-snapshot","messageId":"u2","snapshot":{}}),
            json!({"type":"queue-operation","sessionId":"orig-sid","operation":"enqueue"}),
            json!({"type":"user","uuid":"u2","sessionId":"orig-sid","message":{"role":"user","content":"turn 2 mistake"}}),
            json!({"type":"assistant","uuid":"a3","sessionId":"orig-sid","message":{"role":"assistant","content":[{"type":"text","text":"broke code"}]}}),
        ];
        std::fs::write(&src, lines.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n") + "\n").unwrap();

        let (forked, prompt) = fork_claude_jsonl(&src, 1, "fork-uuid-456").unwrap();
        assert_eq!(prompt.as_deref(), Some("turn 2 mistake"));
        let out_raw = std::fs::read_to_string(&forked).unwrap();
        let out_lines: Vec<Value> = out_raw.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(out_lines.len(), 4, "mixed tool_result stays in turn 1; orphaned file-history-snapshot, queue-operation and turn 2 trimmed");
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

    #[test]
    fn reset_if_new_cli_session_clears_on_session_change_but_keeps_pending_rollback() {
        use crate::checkpoint::{CheckpointMeta, TargetCheckpoints};
        use crate::paths::Paths;
        use crate::terminals::NewTerminal;

        let root = crate::test_tmp().join(format!("h2-hook-reset-{}-{}", std::process::id(), crate::platform::random_hex()));
        let paths = Paths {
            socket: root.join("core.sock"),
            holder_socket: root.join("holder.sock"),
            state: root.clone(),
            config: root.join("config.toml"),
        };
        paths.create_dirs().unwrap();
        let core = Core::start(paths, crate::api::CoreInfo { id: "here".into(), name: "here".into() }).unwrap();

        let id = {
            let mut st = core.lock();
            st.pty_reg
                .create(
                    NewTerminal {
                        argv: vec!["zsh".into()],
                        cwd: root.to_string_lossy().to_string(),
                        cols: 80,
                        rows: 24,
                        ..Default::default()
                    },
                    1,
                )
                .unwrap()
        };
        let addr = crate::terminals::address(id);
        let cwd = root.to_string_lossy().to_string();

        // Different session_id clears previous checkpoints and resets terminal turn.
        {
            let mut st = core.lock();
            st.pty_reg.terminals.get_mut(&id).unwrap().turn = 3;
            core.save_target_checkpoints(
                &mut st,
                TargetCheckpoints {
                    addr: addr.clone(),
                    cli_kind: Some("claude".into()),
                    cli_session_id: Some("sess-old".into()),
                    checkpoints: vec![CheckpointMeta {
                        turn: 1,
                        session_turn: Some(1),
                        ref_name: "refs/herdr2/checkpoints/test/t1/turn-1".into(),
                        commit_oid: "deadbeef".into(),
                        head_oid: None,
                        prompt: Some("old".into()),
                        label: None,
                        files: vec![],
                        ts_ms: 1,
                    }],
                    ..Default::default()
                },
            );
            reset_if_new_cli_session(&core, &mut st, id, &addr, &cwd, "claude", Some("sess-new"));
            let after = core.target_checkpoints(&mut st, &addr);
            assert!(after.checkpoints.is_empty());
            assert_eq!(st.pty_reg.terminals[&id].turn, 0);
        }

        // Pending rollback protects checkpoints even when a new agy conversationId arrives.
        {
            let mut st = core.lock();
            core.save_target_checkpoints(
                &mut st,
                TargetCheckpoints {
                    addr: addr.clone(),
                    cli_kind: Some("agy".into()),
                    cli_session_id: Some("conv-old".into()),
                    pending_rollback_turn: Some(1),
                    checkpoints: vec![CheckpointMeta {
                        turn: 1,
                        session_turn: Some(1),
                        ref_name: "refs/herdr2/checkpoints/test/t1/turn-1".into(),
                        commit_oid: "deadbeef".into(),
                        head_oid: None,
                        prompt: Some("kept".into()),
                        label: None,
                        files: vec![],
                        ts_ms: 1,
                    }],
                    ..Default::default()
                },
            );
            reset_if_new_cli_session(&core, &mut st, id, &addr, &cwd, "agy", Some("conv-new"));
            let after = core.target_checkpoints(&mut st, &addr);
            assert_eq!(after.checkpoints.len(), 1);
        }

        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn build_rollback_argv_for_terminal_reenters_cli_across_shells_wrappers_and_generic_agents() {
        let dir = crate::test_tmp().join(format!("h2-rb-argv-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();

        // 1. Login shell with `-l` flag running versioned Claude binary preserves flags & re-enters claude.
        let orig_shell = vec!["/usr/bin/zsh".into(), "-l".into()];
        let fg_claude = vec![
            "/home/u/.local/share/claude/versions/2.1.328".into(),
            "--dangerously-skip-permissions".into(),
            "--reply-on-resume".into(),
            "--resume".into(),
            "old-sid".into(),
        ];
        let rebuilt = build_rollback_argv_for_terminal(
            PtyCliKind::Claude,
            &orig_shell,
            Some(&fg_claude),
            &dir,
            "h2",
            1,
            Some("new-sid"),
        );
        assert_eq!(rebuilt[0], "/usr/bin/zsh");
        assert_eq!(rebuilt[1], "-lic");
        assert_eq!(rebuilt[4], "claude");
        assert!(rebuilt.iter().any(|a| a == "--dangerously-skip-permissions"), "{rebuilt:?}");
        assert!(rebuilt.windows(2).any(|w| w == ["--resume", "new-sid"]), "{rebuilt:?}");
        assert!(!rebuilt.iter().any(|a| a == "--reply-on-resume"), "{rebuilt:?}");

        // 2. Previous Claude wrapper now running Codex replaces wrapped command with Codex (never `zsh resume`).
        let prev_wrapper = vec![
            "/usr/bin/zsh".into(),
            "-lic".into(),
            "\"$@\"; exec \"$0\" -li".into(),
            "/usr/bin/zsh".into(),
            "claude".into(),
            "--resume".into(),
            "old-claude".into(),
        ];
        let fg_codex = vec!["codex".into(), "--dangerously-bypass-approvals-and-sandbox".into()];
        let rebuilt_codex = build_rollback_argv_for_terminal(
            PtyCliKind::Codex,
            &prev_wrapper,
            Some(&fg_codex),
            &dir,
            "h2",
            2,
            Some("codex-sid"),
        );
        assert_eq!(rebuilt_codex[0], "/usr/bin/zsh");
        assert_eq!(rebuilt_codex[4], "codex");
        assert_eq!(rebuilt_codex[5], "resume");
        assert_eq!(rebuilt_codex[6], "codex-sid");

        // 3. Shell running `agy.real --conversation old` normalizes to `agy` without stale `--conversation`.
        let fg_agy = vec![
            "/home/u/.local/bin/agy.real".into(),
            "--dangerously-skip-permissions".into(),
            "--conversation".into(),
            "old-conv".into(),
        ];
        let rebuilt_agy =
            build_rollback_argv_for_terminal(PtyCliKind::Agy, &orig_shell, Some(&fg_agy), &dir, "h2", 1, None);
        assert_eq!(rebuilt_agy[4], "agy");
        assert!(rebuilt_agy.iter().any(|a| a == "--dangerously-skip-permissions"), "{rebuilt_agy:?}");
        assert!(!rebuilt_agy.iter().any(|a| a == "--conversation" || a == "old-conv"), "{rebuilt_agy:?}");

        // 4. Shell running Generic CLI (e.g. `gemini`) re-launches it inside shell wrapper instead of bare zsh.
        let fg_gemini = vec!["gemini".into(), "--yolo".into()];
        let rebuilt_gen =
            build_rollback_argv_for_terminal(PtyCliKind::Generic, &orig_shell, Some(&fg_gemini), &dir, "h2", 0, None);
        assert_eq!(rebuilt_gen[0], "/usr/bin/zsh");
        assert_eq!(rebuilt_gen[4], "gemini");
        assert_eq!(rebuilt_gen[5], "--yolo");

        // 5. Rolling back Claude to T0 passes the caller's exact fresh session_id to `--session-id`.
        let reset_claude = build_rollback_argv_for_terminal(
            PtyCliKind::Claude,
            &orig_shell,
            Some(&fg_claude),
            &dir,
            "h2",
            0,
            Some("fresh-t0-uuid"),
        );
        assert!(reset_claude.windows(2).any(|w| w == ["--session-id", "fresh-t0-uuid"]), "{reset_claude:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_rollback_transcript_turn_handles_session_turn_and_offset_turns() {
        use crate::checkpoint::CheckpointMeta;
        let mk = |turn: u64, session_turn: Option<u64>, label: Option<&str>| CheckpointMeta {
            turn,
            session_turn,
            ref_name: format!("refs/herdr2/checkpoints/default/t1/{turn}"),
            commit_oid: "abc".into(),
            head_oid: None,
            prompt: None,
            label: label.map(str::to_string),
            files: vec![],
            ts_ms: turn,
        };
        // Explicit session_turn wins (e.g. resumed session had 3 turns before checkpointing turn 4 as T1).
        let resumed_ckpts = vec![mk(0, None, None), mk(1, Some(4), None), mk(2, Some(5), None)];
        assert_eq!(resolve_rollback_transcript_turn(&resumed_ckpts, 0), 0);
        assert_eq!(resolve_rollback_transcript_turn(&resumed_ckpts, 1), 4);
        assert_eq!(resolve_rollback_transcript_turn(&resumed_ckpts, 2), 5);

        // Legacy offset checkpoints without session_turn (T0, T5, T6, manual snap, T7) map by ordinal turn count.
        let offset_ckpts = vec![
            mk(0, None, None),
            mk(5, None, None),
            mk(6, None, None),
            mk(7, None, Some("manual")),
            mk(8, None, None),
        ];
        assert_eq!(resolve_rollback_transcript_turn(&offset_ckpts, 5), 1);
        assert_eq!(resolve_rollback_transcript_turn(&offset_ckpts, 6), 2);
        assert_eq!(resolve_rollback_transcript_turn(&offset_ckpts, 8), 3);
    }

    #[test]
    fn codex_internal_title_helper_prompt_is_ignored_by_checkpoint_hook() {
        let helper = "Generate a concise, single-line task title of at most 36 characters and under five words where possible. Start with an imperative verb. Do not answer the request.\n\nUser prompt:\n[Handoff from t2t] Read the full context handoff";
        assert!(is_codex_internal_helper_prompt(helper));
        assert!(!is_codex_internal_helper_prompt("[Handoff from t2t] Read the full context handoff"));
        assert!(!is_codex_internal_helper_prompt("Generate a title bar component for the app"));

        let root = crate::test_tmp().join(format!("h2-codex-helper-{}-{}", std::process::id(), crate::platform::random_hex()));
        let paths = crate::paths::Paths {
            socket: root.join("core.sock"),
            holder_socket: root.join("holder.sock"),
            state: root.clone(),
            config: root.join("config.toml"),
        };
        paths.create_dirs().unwrap();
        let core = Arc::new(
            Core::start(paths, crate::api::CoreInfo { id: "here".into(), name: "here".into() }).unwrap(),
        );
        let id = {
            let mut st = core.lock();
            st.pty_reg
                .create(
                    crate::terminals::NewTerminal {
                        argv: vec!["codex".into()],
                        cwd: root.to_string_lossy().to_string(),
                        cols: 80,
                        rows: 24,
                        ..Default::default()
                    },
                    1,
                )
                .unwrap()
        };
        let addr = crate::terminals::address(id);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(handle_checkpoint_hook(
            &core,
            id,
            "codex-notify",
            None,
            None,
            &json!({
                "type": "agent-turn-complete",
                "thread-id": "codex-title-helper-thread",
                "input-messages": [helper]
            }),
        ));
        {
            let mut st = core.lock();
            let tc = core.target_checkpoints(&mut st, &addr);
            assert!(tc.checkpoints.is_empty(), "internal title helper must not capture a checkpoint");
            assert!(tc.cli_session_id.is_none(), "internal title helper must not bind session_id");
        }
        // Duplicate session_turn deduplication prevents codex-notify + OSC turn_ended double snapshots.
        let existing = vec![
            crate::checkpoint::CheckpointMeta {
                turn: 0,
                session_turn: None,
                ref_name: "refs/herdr2/checkpoints/default/t1/0".into(),
                commit_oid: "aaa".into(),
                head_oid: None,
                prompt: None,
                label: None,
                files: vec![],
                ts_ms: 1,
            },
            crate::checkpoint::CheckpointMeta {
                turn: 1,
                session_turn: Some(1),
                ref_name: "refs/herdr2/checkpoints/default/t1/1".into(),
                commit_oid: "bbb".into(),
                head_oid: None,
                prompt: Some("[Handoff from t2t] continue".into()),
                label: None,
                files: vec![],
                ts_ms: 2,
            },
        ];
        assert!(has_captured_session_turn(&existing, Some(1), Some("[Handoff from t2t] continue")));
        assert!(!has_captured_session_turn(&existing, Some(2), Some("next question")));
        assert!(!has_captured_session_turn(&existing, None, Some("anything")));

        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }
}
