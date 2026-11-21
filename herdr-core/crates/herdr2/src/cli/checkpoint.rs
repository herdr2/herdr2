use std::io::{IsTerminal, Read as _};
use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::paths::Paths;
use crate::rpc::RpcError;

use super::Out;

#[derive(Args)]
pub struct CheckpointCli {
    #[command(subcommand)]
    pub sub: CheckpointSub,
}

#[derive(Subcommand)]
pub enum CheckpointSub {
    /// List hidden Git ref checkpoints and workspace isolation state.
    Ls {
        /// Agent (`a1`), terminal (`t1`), remote (`machine/t1`), or unique name.
        target: String,
    },
    /// Manually capture a checkpoint for an agent or terminal (defaults to current pane/agent).
    Snap {
        /// Agent (`a1`), terminal (`t1`), remote (`machine/t1`), or unique name.
        target: Option<String>,
        /// Optional bookmark label recorded alongside this checkpoint.
        #[arg(long)]
        label: Option<String>,
    },
    /// Show the pure authored diff between two checkpoints.
    Diff {
        /// Agent (`a1`), terminal (`t1`), remote (`machine/t1`), or unique name.
        target: String,
        /// Starting checkpoint turn (defaults to highest snapshot before `to`).
        #[arg(long)]
        from: Option<u64>,
        /// Ending checkpoint turn (defaults to latest checkpoint).
        #[arg(long)]
        to: Option<u64>,
    },
    /// Roll back session state (and optionally workspace files) to an earlier turn.
    Rollback {
        /// Agent (`a1`), terminal (`t1`), remote (`machine/t1`), or unique name.
        target: String,
        /// Target checkpoint turn (`0` = baseline before turn 1).
        #[arg(long)]
        to: u64,
        /// Rewind conversation memory only; keep current workspace files intact.
        #[arg(long = "no-files")]
        no_files: bool,
        /// Confirm destructive file restore on a main checkout or with co-located idle shells.
        #[arg(long)]
        confirm: bool,
    },
    /// Locate which checkpoint turn most likely introduced a bug or regression.
    Bisect {
        /// Agent (`a1`), terminal (`t1`), remote (`machine/t1`), or unique name.
        target: String,
        /// Symptom or regression query (e.g. "which turn broke auth tests?").
        #[arg(long)]
        query: String,
        /// Caller UI language (`zh` / `en`).
        #[arg(long)]
        lang: Option<String>,
    },
    /// Internal CLI hook bridge (`claude`, `codex-notify`, `agy`).
    #[command(hide = true)]
    Hook {
        #[arg(long)]
        source: String,
        #[arg(long)]
        phase: Option<String>,
        #[arg(long)]
        event: Option<String>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },
}

/// Fast zero-RPC PreToolUse guard for forked worktree sessions (`--phase guard`).
///
/// Returns `Some((exit_code, message))` whenever `phase == "guard"`:
/// - `exit_code == 0`: tool call allowed (or not a worktree session).
/// - `exit_code == 2`: tool call targets `H2_WORKTREE_ORIGIN` outside `H2_WORKTREE_PATH`; Claude Code
///   blocks the call and feeds `stderr` back to the model so it retries inside `H2_WORKTREE_PATH`.
pub fn worktree_guard_hook(cli: &CheckpointCli) -> Option<(i32, Option<String>)> {
    let CheckpointSub::Hook { source, phase, extra, .. } = &cli.sub else {
        return None;
    };
    if phase.as_deref() != Some("guard") {
        return None;
    }
    let origin = std::env::var("H2_WORKTREE_ORIGIN").ok().map(|s| s.trim().to_string()).unwrap_or_default();
    let wt_path = std::env::var("H2_WORKTREE_PATH").ok().map(|s| s.trim().to_string()).unwrap_or_default();
    if origin.is_empty() || wt_path.is_empty() || normalize_path(&origin) == normalize_path(&wt_path) {
        return Some((0, Some("{}".to_string())));
    }
    let payload = read_hook_payload(source, extra);
    if let Some(reason) = check_worktree_guard(&payload, &origin, &wt_path) {
        return Some((2, Some(reason)));
    }
    Some((0, Some("{}".to_string())))
}

fn normalize_path(p: &str) -> String {
    let s = p.trim().replace('\\', "/");
    let trimmed = s.trim_end_matches('/');
    if trimmed.is_empty() && s.starts_with('/') { "/".to_string() } else { trimmed.to_string() }
}

fn eq_path_prefix(a: &str, b: &str) -> bool {
    if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b }
}

fn is_under_dir(path_norm: &str, dir_norm: &str) -> bool {
    if dir_norm.is_empty() {
        return false;
    }
    if path_norm.len() < dir_norm.len() || !path_norm.is_char_boundary(dir_norm.len()) {
        return false;
    }
    if !eq_path_prefix(&path_norm[..dir_norm.len()], dir_norm) {
        return false;
    }
    path_norm.len() == dir_norm.len() || path_norm.as_bytes()[dir_norm.len()] == b'/'
}

fn resolve_lexical(raw: &str, base_norm: &str) -> String {
    let s = raw.trim().replace('\\', "/");
    let is_win_abs = s.len() >= 3 && s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1] == b':' && s.as_bytes()[2] == b'/';
    let full = if s.starts_with('/') || is_win_abs {
        s
    } else if base_norm.is_empty() {
        s
    } else {
        format!("{base_norm}/{s}")
    };
    let (prefix, rest) = if full.len() >= 3 && full.as_bytes()[0].is_ascii_alphabetic() && full.as_bytes()[1] == b':' && full.as_bytes()[2] == b'/' {
        (&full[..3], &full[3..])
    } else if let Some(stripped) = full.strip_prefix('/') {
        ("/", stripped)
    } else {
        ("", full.as_str())
    };
    let mut stack: Vec<&str> = Vec::new();
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    format!("{prefix}{}", stack.join("/"))
}

fn path_hits_origin(raw: &str, origin_norm: &str, wt_norm: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }
    let resolved = resolve_lexical(trimmed, wt_norm);
    if is_under_dir(&resolved, wt_norm) {
        return false;
    }
    is_under_dir(&resolved, origin_norm)
}

fn is_path_word_char(b: u8) -> bool {
    matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.')
}

fn command_hits_origin(cmd: &str, origin_norm: &str, wt_norm: &str) -> bool {
    if origin_norm.is_empty() {
        return false;
    }
    let cmd_norm = cmd.replace('\\', "/");
    let haystack = if cfg!(windows) { cmd_norm.to_ascii_lowercase() } else { cmd_norm };
    let needle = if cfg!(windows) { origin_norm.to_ascii_lowercase() } else { origin_norm.to_string() };
    let wt_needle = if cfg!(windows) { wt_norm.to_ascii_lowercase() } else { wt_norm.to_string() };
    let bytes = haystack.as_bytes();
    let nlen = needle.len();
    let mut i = 0usize;
    while i + nlen <= haystack.len() {
        if haystack.is_char_boundary(i) && haystack[i..].starts_with(&needle) {
            let left_ok = i == 0 || (!is_path_word_char(bytes[i - 1]) && bytes[i - 1] != b'/');
            let end = i + nlen;
            let right_ok = end == haystack.len() || !is_path_word_char(bytes[end]);
            if left_ok && right_ok {
                let inside_wt = !wt_needle.is_empty()
                    && haystack[i..].starts_with(&wt_needle)
                    && (i + wt_needle.len() == haystack.len() || !is_path_word_char(bytes[i + wt_needle.len()]));
                if !inside_wt {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

/// Checks a Claude Code `PreToolUse` hook JSON payload against `(origin, wt_path)`.
/// Returns `Some(reason)` if the tool call attempts to access `origin` outside `wt_path`.
pub fn check_worktree_guard(payload: &Value, origin: &str, wt_path: &str) -> Option<String> {
    let origin_norm = normalize_path(origin);
    let wt_norm = normalize_path(wt_path);
    if origin_norm.is_empty() || wt_norm.is_empty() || eq_path_prefix(&origin_norm, &wt_norm) {
        return None;
    }
    let input = payload.get("tool_input")?;
    let tool_name = payload.get("tool_name").and_then(Value::as_str).unwrap_or("tool");

    let mut violation: Option<String> = None;
    for key in ["file_path", "notebook_path", "path", "target_file", "directory", "cwd"] {
        if let Some(p) = input.get(key).and_then(Value::as_str)
            && path_hits_origin(p, &origin_norm, &wt_norm)
        {
            violation = Some(format!("{key}={p}"));
            break;
        }
    }
    if violation.is_none()
        && let Some(edits) = input.get("edits").and_then(Value::as_array)
    {
        for ed in edits {
            for key in ["file_path", "path"] {
                if let Some(p) = ed.get(key).and_then(Value::as_str)
                    && path_hits_origin(p, &origin_norm, &wt_norm)
                {
                    violation = Some(format!("edits.{key}={p}"));
                    break;
                }
            }
            if violation.is_some() {
                break;
            }
        }
    }
    if violation.is_none()
        && let Some(cmd) = input.get("command").and_then(Value::as_str)
        && command_hits_origin(cmd, &origin_norm, &wt_norm)
    {
        violation = Some("command references parent workspace path".into());
    }

    violation.map(|detail| {
        format!(
            "BLOCKED by herdr2 worktree guard: `{tool_name}` targeted parent workspace `{origin}` ({detail}). \
             You are running in an isolated Git worktree `{wt_path}`. \
             Retry strictly inside `{wt_path}` and NEVER read, edit, or run commands in `{origin}`."
        )
    })
}

/// Returns `Some(json!({}))` immediately without touching IPC sockets when an external process outside `herdr2` runs `checkpoint hook`.
pub fn fast_noop_hook(cli: &CheckpointCli) -> Option<String> {
    if !matches!(cli.sub, CheckpointSub::Hook { .. }) {
        return None;
    }
    let has_tid = std::env::var("H2_TERMINAL_ID").ok().is_some_and(|s| !s.trim().is_empty());
    (!has_tid).then(|| "{}".to_string())
}

pub fn execute(paths: &Paths, cli: CheckpointCli) -> Result<Out, RpcError> {
    match cli.sub {
        CheckpointSub::Ls { target } => {
            let target = super::target(paths, Some(target), true)?;
            let r = super::call(paths, "checkpoint.list", json!({ "target": target }))?;
            let human = render_list(&r);
            Ok(Out { json: r, human })
        }
        CheckpointSub::Snap { target, label } => {
            let resolved = match target {
                Some(t) => Some(super::target(paths, Some(t), false)?),
                None => crate::client::terminal_id().or_else(crate::client::agent_id),
            };
            let r = super::call(paths, "checkpoint.capture", json!({ "target": resolved, "label": label }))?;
            let turn = r["checkpoint"]["turn"].as_u64().unwrap_or(0);
            let ref_name = r["checkpoint"]["ref_name"].as_str().unwrap_or_default();
            let tgt = r["target"].as_str().unwrap_or_default();
            Ok(Out { json: r.clone(), human: format!("captured {tgt} turn {turn} ({ref_name})") })
        }
        CheckpointSub::Diff { target, from, to } => {
            let target = super::target(paths, Some(target), true)?;
            let r = super::call(paths, "checkpoint.diff", json!({ "target": target, "from": from, "to": to }))?;
            let human = render_diff(&r);
            Ok(Out { json: r, human })
        }
        CheckpointSub::Rollback { target, to, no_files, confirm } => {
            let target = super::target(paths, Some(target), false)?;
            let r = super::call(
                paths,
                "checkpoint.rollback",
                json!({
                    "target": target,
                    "to": to,
                    "restore_files": !no_files,
                    "confirm": confirm,
                }),
            )?;
            let tgt = r["target"].as_str().unwrap_or_default();
            let files_msg =
                if r["restored_files"].as_bool() == Some(true) { "files + session" } else { "session only" };
            Ok(Out { json: r.clone(), human: format!("rolled back {tgt} to turn {to} ({files_msg})") })
        }
        CheckpointSub::Bisect { target, query, lang } => {
            let target = super::target(paths, Some(target), true)?;
            let r = super::call(
                paths,
                "checkpoint.bisect",
                json!({
                    "target": target,
                    "query": query,
                    "language": lang,
                }),
            )?;
            let suspect = r["suspectTurn"].as_u64().unwrap_or(0);
            let safe = r["recommendedRollbackTurn"].as_u64().unwrap_or(0);
            let conf = (r["confidence"].as_f64().unwrap_or(0.0) * 100.0).round() as u64;
            let reason = r["reason"].as_str().unwrap_or("");
            Ok(Out {
                json: r.clone(),
                human: format!("suspect T{suspect} ({conf}%) -> recommend rollback to T{safe}: {reason}"),
            })
        }
        CheckpointSub::Hook { source, phase, event, extra } => {
            let Some(terminal) = std::env::var("H2_TERMINAL_ID").ok().filter(|s| !s.trim().is_empty()) else {
                return Ok(Out { json: json!({}), human: "{}".into() });
            };
            let payload = read_hook_payload(&source, &extra);
            let mut hook_paths = Paths {
                socket: paths.socket.clone(),
                holder_socket: paths.holder_socket.clone(),
                state: paths.state.clone(),
                config: paths.config.clone(),
            };
            if let Ok(sock) = std::env::var("H2_TERMINAL_SOCKET")
                && !sock.trim().is_empty()
            {
                let p = PathBuf::from(sock);
                hook_paths.holder_socket = p.with_extension("holder.sock");
                hook_paths.socket = p;
            }
            let Some(stream) = crate::client::connect_existing(&hook_paths) else {
                return Ok(Out { json: json!({}), human: "{}".into() });
            };
            let resp = crate::client::call(
                &stream,
                "checkpoint.hook",
                json!({
                    "terminal": terminal,
                    "source": source,
                    "phase": phase,
                    "event": event,
                    "payload": payload,
                }),
            )
            .unwrap_or_else(|_| json!({}));
            let serialized = resp.to_string();
            Ok(Out { json: resp, human: serialized })
        }
    }
}

fn read_hook_payload(source: &str, extra: &[String]) -> Value {
    if source == "codex-notify"
        && let Some(last) = extra.last()
        && let Ok(v) = serde_json::from_str::<Value>(last)
    {
        return v;
    }
    if std::io::stdin().is_terminal() {
        return json!({});
    }
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_ok()
        && let Ok(v) = serde_json::from_str::<Value>(buf.trim())
    {
        return v;
    }
    json!({})
}

fn render_list(r: &Value) -> String {
    let target = r["target"].as_str().unwrap_or("");
    let iso = r["isolation"]["level"].as_str().unwrap_or("unknown");
    let mut lines = vec![format!("{target}  isolation={iso}")];
    if let Some(ckpts) = r["checkpoints"].as_array() {
        if ckpts.is_empty() {
            lines.push("  (no checkpoints)".into());
        } else {
            for c in ckpts {
                let turn = c["turn"].as_u64().unwrap_or(0);
                let files = c["files"].as_array().map_or(0, Vec::len);
                let add: u64 = c["files"].as_array().map_or(0, |a| a.iter().filter_map(|f| f["added"].as_u64()).sum());
                let del: u64 =
                    c["files"].as_array().map_or(0, |a| a.iter().filter_map(|f| f["deleted"].as_u64()).sum());
                let summary = c["label"].as_str().or_else(|| c["prompt"].as_str()).unwrap_or(if turn == 0 {
                    "baseline"
                } else {
                    ""
                });
                let short: String = summary.chars().take(60).collect();
                lines.push(format!("  turn {turn:<3}  {files} file(s) (+{add} -{del})  {short}"));
            }
        }
    }
    lines.join("\n")
}

fn render_diff(r: &Value) -> String {
    let from = r["from_turn"].as_u64().unwrap_or(0);
    let to = r["to_turn"].as_u64().unwrap_or(0);
    let files = r["files"].as_array().map_or(0, Vec::len);
    let patch = r["patch"].as_str().unwrap_or("");
    let mut out = format!("=== turn {from} -> {to} ({files} file(s)) ===\n{patch}");
    if r["truncated"].as_bool() == Some(true) {
        out.push_str("\n... [diff truncated at 512 KiB]");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_guard_blocks_parent_workspace_and_allows_worktree() {
        let origin = "/home/user/temp/test";
        let wt = "/home/user/.local/state/herdr2/default/worktrees/forks/test-fk1";

        // Direct Write/Edit/Read/Glob into parent workspace -> BLOCKED
        for (tool, key, path) in [
            ("Write", "file_path", "/home/user/temp/test/calc.py"),
            ("Edit", "file_path", "/home/user/temp/test/calc.py"),
            ("Read", "file_path", "/home/user/temp/test/calc.py"),
            ("Glob", "path", "/home/user/temp/test"),
            ("NotebookEdit", "notebook_path", "/home/user/temp/test/nb.ipynb"),
            ("Edit", "file_path", "../../../../../../../temp/test/calc.py"),
        ] {
            let payload = json!({ "tool_name": tool, "tool_input": { key: path } });
            assert!(
                check_worktree_guard(&payload, origin, wt).is_some(),
                "expected block for {tool} {key}={path}"
            );
        }

        // Bash referencing parent workspace -> BLOCKED
        for cmd in [
            "cd /home/user/temp/test && python3 calc.py",
            "python3 /home/user/temp/test/calc.py",
            "git -C /home/user/temp/test status",
        ] {
            let payload = json!({ "tool_name": "Bash", "tool_input": { "command": cmd } });
            assert!(check_worktree_guard(&payload, origin, wt).is_some(), "expected block for bash: {cmd}");
        }

        // Allowed: inside worktree, staged handoff.md, relative inside worktree, sibling dir
        for (tool, input) in [
            ("Edit", json!({ "file_path": format!("{wt}/calc.py") })),
            ("Write", json!({ "file_path": "calc.py" })),
            ("Read", json!({ "file_path": "/home/user/.local/state/herdr2/default/files/t3s/handoff.md" })),
            ("Edit", json!({ "file_path": "/home/user/temp/test-other/calc.py" })),
            ("Bash", json!({ "command": "python3 calc.py && git status" })),
            ("Bash", json!({ "command": format!("cd {wt} && python3 calc.py") })),
            ("Bash", json!({ "command": "ls /home/user/temp/test-other" })),
        ] {
            let payload = json!({ "tool_name": tool, "tool_input": input });
            assert!(
                check_worktree_guard(&payload, origin, wt).is_none(),
                "expected allow for {tool} {input}"
            );
        }

        // Nested worktree inside origin: worktree allowed, parent outside worktree blocked
        let nested_wt = "/home/user/temp/test/.wt/fk1";
        assert!(
            check_worktree_guard(
                &json!({ "tool_name": "Edit", "tool_input": { "file_path": "/home/user/temp/test/.wt/fk1/calc.py" } }),
                origin,
                nested_wt
            )
            .is_none()
        );
        assert!(
            check_worktree_guard(
                &json!({ "tool_name": "Bash", "tool_input": { "command": "cd /home/user/temp/test/.wt/fk1 && ls" } }),
                origin,
                nested_wt
            )
            .is_none()
        );
        assert!(
            check_worktree_guard(
                &json!({ "tool_name": "Edit", "tool_input": { "file_path": "/home/user/temp/test/calc.py" } }),
                origin,
                nested_wt
            )
            .is_some()
        );
    }
}
