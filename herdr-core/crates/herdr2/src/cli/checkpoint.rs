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
