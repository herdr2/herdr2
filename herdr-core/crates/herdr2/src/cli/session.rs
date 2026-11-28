use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::paths::Paths;
use crate::rpc::RpcError;

use super::Out;

#[derive(Args)]
pub struct SessionCli {
    #[command(subcommand)]
    pub sub: SessionSub,
}

#[derive(Subcommand)]
pub enum SessionSub {
    /// List historical CLI agent sessions for a workspace and CLI agent, with full-access resume commands.
    Ls {
        /// Optional live terminal (`t1` or `machine/t1`) to infer workspace and CLI agent.
        target: Option<String>,
        /// Workspace directory path (overrides `target`'s cwd when given).
        #[arg(long)]
        cwd: Option<String>,
        /// CLI agent kind (`claude`, `codex`, `agy`, `cursor`, `zcode`, `opencode`, `qwen`, `gemini`, `pi`, `hermes`, ...).
        #[arg(long)]
        agent: Option<String>,
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
        /// Maximum number of sessions to return (default 60).
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Read structured conversation entries of a historical CLI agent session.
    Read {
        /// Session ID to inspect.
        session_id: String,
        /// CLI agent kind (`claude`, `codex`, `agy`, `cursor`, `zcode`, `opencode`, ...).
        #[arg(long)]
        agent: Option<String>,
        /// Optional terminal (`t1` or `machine/t1`) to infer agent, workspace, or remote machine.
        #[arg(long)]
        target: Option<String>,
        /// Workspace directory hint.
        #[arg(long)]
        cwd: Option<String>,
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
        /// Maximum entries to return (default 500).
        #[arg(long)]
        limit: Option<usize>,
    },
}

pub fn execute(paths: &Paths, cli: SessionCli) -> Result<Out, RpcError> {
    match cli.sub {
        SessionSub::Ls { target, cwd, agent, on, limit } => {
            let mut params = json!({});
            if let Some(t) = target.filter(|s| !s.trim().is_empty()) {
                params["target"] = json!(t);
            }
            if let Some(c) = cwd.filter(|s| !s.trim().is_empty()) {
                params["cwd"] = json!(c);
            } else if params.get("target").is_none()
                && on.is_none()
                && let Ok(cur) = std::env::current_dir()
            {
                params["cwd"] = json!(cur.to_string_lossy());
            }
            if let Some(a) = agent.filter(|s| !s.trim().is_empty()) {
                params["agent"] = json!(a);
            }
            if let Some(m) = on.filter(|s| !s.trim().is_empty()) {
                params["on"] = json!(m);
            }
            if let Some(l) = limit {
                params["limit"] = json!(l);
            }
            let r = super::call(paths, "session.list", params)?;
            let human = render_list(&r);
            Ok(Out { json: r, human })
        }
        SessionSub::Read { session_id, agent, target, cwd, on, limit } => {
            let mut params = json!({ "session_id": session_id });
            if let Some(a) = agent.filter(|s| !s.trim().is_empty()) {
                params["agent"] = json!(a);
            }
            if let Some(t) = target.filter(|s| !s.trim().is_empty()) {
                params["target"] = json!(t);
            }
            if let Some(c) = cwd.filter(|s| !s.trim().is_empty()) {
                params["cwd"] = json!(c);
            } else if params.get("target").is_none()
                && on.is_none()
                && let Ok(cur) = std::env::current_dir()
            {
                params["cwd"] = json!(cur.to_string_lossy());
            }
            if let Some(m) = on.filter(|s| !s.trim().is_empty()) {
                params["on"] = json!(m);
            }
            if let Some(l) = limit {
                params["limit"] = json!(l);
            }
            let r = super::call(paths, "session.read", params)?;
            let human = render_read(&r);
            Ok(Out { json: r, human })
        }
    }
}

fn render_list(r: &Value) -> String {
    let agent = r["agent_name"].as_str().unwrap_or("-");
    let cwd = r["cwd"].as_str().unwrap_or("-");
    let Some(arr) = r["sessions"].as_array() else {
        return format!("no sessions ({agent} · {cwd})");
    };
    if arr.is_empty() {
        return format!("no historical sessions for {agent} in {cwd}");
    }
    let mut lines = vec![format!("{agent} sessions in {cwd} ({}):", arr.len())];
    for s in arr {
        let sid = s["session_id"].as_str().unwrap_or("?");
        let title = s["title"].as_str().unwrap_or("");
        let cmd = s["resume_command"].as_str().unwrap_or("");
        let cur = if s["current"].as_bool() == Some(true) { " [current]" } else { "" };
        lines.push(format!("  {sid}{cur}  {title}\n    resume: {cmd}"));
    }
    lines.join("\n")
}

fn render_read(r: &Value) -> String {
    let sid = r["session_id"].as_str().unwrap_or("?");
    let title = r["title"].as_str().unwrap_or("");
    let cmd = r["resume_command"].as_str().unwrap_or("");
    let mut out = format!("# {title} ({sid})\nresume: {cmd}\n");
    if let Some(entries) = r["entries"].as_array() {
        for e in entries {
            match e["kind"].as_str().or_else(|| e["type"].as_str()) {
                Some("user") => {
                    let turn = e["turn"].as_u64().unwrap_or(0);
                    let text = e["text"].as_str().unwrap_or("");
                    out.push_str(&format!("\n[Turn {turn} · User]\n{text}\n"));
                }
                Some("assistant") => {
                    let text = e["text"].as_str().unwrap_or("");
                    out.push_str(&format!("\n[Assistant]\n{text}\n"));
                }
                _ => {}
            }
        }
    }
    out.trim_end().to_string()
}
