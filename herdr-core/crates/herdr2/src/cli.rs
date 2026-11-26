//! `h2`: one API call per command. Tables for people, JSON when piped or `--json`.

use crate::api::{
    AgentParams, ApproveParams, AttachParams, ConfigParams, ExecParams, HandoffParams, InboxEditParams,
    InboxRemoveParams, InboxSendParams, InterruptParams, KeepParams, KeysParams, LabelParams, ListParams, PutParams,
    ReadParams, RunParams, SendParams, SubscribeParams, WaitParams,
};
use crate::client;
use crate::paths::Paths;
use crate::rpc::RpcError;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::io::{IsTerminal, Read};

mod checkpoint;
mod file;
mod machine;
mod output;
mod run;
mod server;
mod skill;
mod update;
use file::*;
use machine::*;
use output::*;
use run::*;
use server::*;
use update::*;

#[derive(Subcommand)]
enum InboxCmd {
    /// List queued prompts waiting in an agent's inbox.
    Ls(AgentParams),
    /// Edit a queued prompt before its turn begins.
    Edit(InboxEditParams),
    /// Remove a queued prompt before its turn begins.
    Rm(InboxRemoveParams),
    /// Interrupt the active turn if needed and immediately send a queued prompt.
    Send(InboxSendParams),
    /// Re-enqueue paused prompts preserved across a restart/reboot and resume delivery.
    Resume(AgentParams),
    /// Discard paused prompts preserved across a restart/reboot.
    ClearPaused(AgentParams),
}

#[derive(Subcommand)]
enum AiCmd {
    /// Show shared Core AI assistant status, configuration, and KV-cache metrics.
    Status {
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Update shared Core AI assistant settings from a JSON file.
    SettingsSet {
        #[arg(long, value_name = "FILE")]
        file: std::path::PathBuf,
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
        /// Delete the input JSON file right after reading it (safe for API keys).
        #[arg(long)]
        consume: bool,
    },
    /// Fetch available models from an AI endpoint (/models, OpenAI or Claude protocol).
    Models {
        #[arg(long, value_name = "FILE")]
        file: Option<std::path::PathBuf>,
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
        /// Delete `--file` right after reading it.
        #[arg(long)]
        consume: bool,
    },
    /// Test the shared Core AI assistant URL and API key.
    Test {
        #[arg(long, value_name = "FILE")]
        file: Option<std::path::PathBuf>,
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
        /// Delete `--file` right after reading it.
        #[arg(long)]
        consume: bool,
    },
    /// Clear prior prompt-cache detection records, set prompt mode to auto, and re-detect shared-prefix vs compact mode.
    ProbePromptMode {
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Recommend a complementary target provider, handoff mode, and note for an upstream session.
    HandoffRecommend {
        /// Source agent or terminal (`a1`, `t1`, name, or `machine/a1`).
        source: String,
        /// Installed presets available on the target machine (repeatable or comma-separated).
        #[arg(long = "installed", value_delimiter = ',')]
        installed: Vec<String>,
        /// Caller UI language (`zh` / `en`).
        #[arg(long)]
        lang: Option<String>,
    },
    /// Aggregate local & remote agents and Drove runs into a priority catch-up briefing.
    Briefing {
        /// Target remote machine (`h2 machine ls`).
        #[arg(long)]
        on: Option<String>,
        /// Preferred language (`zh-CN` / `en`).
        #[arg(long)]
        lang: Option<String>,
        /// Away duration in minutes (optional context for executive headline).
        #[arg(long)]
        away_minutes: Option<u32>,
        /// Return deterministic aggregation only without calling Core AI.
        #[arg(long)]
        no_ai: bool,
    },
    /// Generate or refresh a concise semantic title and progress micro-summary for an agent or terminal.
    SummarizeAgent {
        /// Target agent or terminal (`a1`, `t1`, name, or `machine/a1`).
        target: String,
        /// Preferred language (`zh-CN` / `en`).
        #[arg(long)]
        lang: Option<String>,
        /// Return deterministic summary only without calling Core AI.
        #[arg(long)]
        no_ai: bool,
    },
    /// Generate a concise Git commit message for uncommitted changes using Core AI.
    CommitMessage {
        /// Git repository directory (defaults to current working directory).
        #[arg(long)]
        cwd: Option<String>,
        /// Target remote machine (`h2 machine ls`).
        #[arg(long)]
        on: Option<String>,
        /// Preferred language (`zh-CN` / `en`).
        #[arg(long)]
        lang: Option<String>,
        /// Return deterministic fallback without calling Core AI.
        #[arg(long)]
        no_ai: bool,
    },
    /// Polish a rough user prompt into a clearer, more structured prompt for an ACP or CLI agent.
    PolishPrompt {
        /// Draft prompt text to polish.
        prompt: String,
        /// Optional target agent or terminal (`a1`, `t1`, name, or `machine/a1`).
        #[arg(long)]
        target: Option<String>,
        /// Target agent kind (`acp` or `cli`).
        #[arg(long)]
        kind: Option<String>,
        /// Optional extra context hint (workspace or task summary).
        #[arg(long)]
        context: Option<String>,
        /// Preferred language (`zh-CN` / `en`).
        #[arg(long)]
        lang: Option<String>,
        /// Target remote machine (`h2 machine ls`).
        #[arg(long)]
        on: Option<String>,
    },
}

#[derive(Parser)]
#[command(name = "h2", version = env!("H2_LONG_VERSION"), about = "herdr2: headless ACP orchestration runtime for coding agents")]
struct Cli {
    /// JSON output even in a terminal.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start an ACP agent preset, command, or template: `h2 run claude "fix tests"`.
    Run(RunParams),
    /// List live agents.
    Ls(ListParams),
    /// Show one agent.
    Get(AgentParams),
    /// End an agent process (HUP, then escalate).
    Kill(AgentParams),
    /// Remove a finished agent.
    Rm(AgentParams),
    /// Set labels (key=value; key= removes).
    Label(LabelParams),
    /// Keep a live agent recoverable via `session/load` (--off disables).
    Keep(KeepParams),
    /// Queue a prompt into an agent's ACP inbox (or --steer to cancel & deliver next).
    Send(SendParams),
    /// Inspect, edit, or remove prompts queued in an ACP agent's serial inbox.
    Inbox {
        #[command(subcommand)]
        action: InboxCmd,
    },
    /// Wait for children (--children any) or one agent; a message to you also ends it.
    Wait(WaitParams),
    /// Send ACP `session/cancel` to stop the current turn cleanly.
    Interrupt(InterruptParams),
    /// Approve a blocked agent's pending ACP permission request.
    Approve(ApproveParams),
    /// Deny a blocked agent's pending ACP permission request.
    Deny(ApproveParams),
    /// Configure a live ACP session option (model, reasoning effort, fast mode, session mode).
    Config(ConfigParams),
    /// Print an agent's transcript or final assistant text (`--last`).
    Read(ReadParams),
    /// Hand off, fork, open a side chat, or merge back context across ACP/PTY agents.
    Handoff(HandoffParams),
    /// Stage a local file beside a live agent (on its machine); prints its path.
    Put(PutParams),
    /// Run a non-agent command, here or on a machine: exact stdout/stderr and exit code.
    Exec(ExecParams),
    /// Read, write or edit a file here or on a machine.
    File {
        #[command(subcommand)]
        action: FileCmd,
    },
    /// Start, stop or inspect the core.
    Server {
        #[command(subcommand)]
        action: ServerCmd,
    },
    /// Other machines, reached over ssh.
    Machine {
        #[command(subcommand)]
        action: MachineCmd,
    },
    /// Follow fact changes as they happen (one JSON line each when piped).
    Events(SubscribeParams),
    /// Templates you can run by name (~/.config/herdr2/templates).
    Templates,
    /// Run the built-in stdio MCP server (`h2 mcp`).
    Mcp,
    /// Manage multi-agent Drove orchestration (`drove status`, `drove web`, `drove settings`, ...).
    Drove(crate::drove::cli::DroveCli),
    /// Detect coding agents and install/update bundled Herdr2 companion skills (`status`, `install`, `ai-install`).
    Skill(skill::SkillCli),
    /// Inspect or configure the shared Core AI assistant (`ai status`, `ai settings-set`, `ai test`).
    Ai {
        #[command(subcommand)]
        action: AiCmd,
    },
    /// Hidden Git ref checkpoints, pure turn diffs, and session/workspace rollback (`ls`, `snap`, `diff`, `rollback`).
    Checkpoint(checkpoint::CheckpointCli),
    /// Swap this h2 for another build and restart core.
    Update {
        /// A herdr2 binary to use (default: download the latest release).
        #[arg(long, value_name = "FILE", conflicts_with = "version")]
        from: Option<std::path::PathBuf>,
        /// At least this release (`0.1.4`): nothing changes when this h2 is
        /// already that or newer.
        #[arg(long, value_name = "X.Y.Z", value_parser = release_version)]
        version: Option<String>,
        /// Allow an older version (saved state only moves forward), or a
        /// release of another protocol generation.
        #[arg(long)]
        force: bool,
    },
    /// Every API method with its JSON Schema, for app builders.
    Schema,
    /// Speak the protocol over stdin/stdout (`ssh box herdr2 connect`).
    Connect {
        /// The h2 session to reach, like H2_SESSION (one command that reads
        /// the same in sh, cmd and PowerShell).
        #[arg(long)]
        session: Option<String>,
    },
    /// Follow live output of an interactive PTY terminal (Ctrl-\ then d detaches).
    Attach(AttachParams),
    /// Send named keys (enter, esc, tab, ctrl+c, ...) to a live PTY terminal.
    Keys(KeysParams),
    #[command(hide = true)]
    Hold {
        #[arg(long)]
        socket: std::path::PathBuf,
    },
    #[command(hide = true)]
    Serve,
}

const HELP: &str = include_str!("help.txt");
const SKILL: &str = include_str!("SKILL.md");

/// Every command a person or agent can type (for checking the help names them all).
pub fn command_names() -> Vec<String> {
    use clap::CommandFactory;
    Cli::command()
        .get_subcommands()
        .filter(|c| !c.is_hide_set() && c.get_name() != "help")
        .map(|c| c.get_name().to_string())
        .collect()
}

pub fn main() -> i32 {
    let daemon = crate::platform::daemon_startup();
    client::remember_exe();
    if !daemon {
        let raw_args: Vec<_> = std::env::args_os().skip(1).collect();
        if raw_args.first().and_then(|a| a.to_str()) == Some(crate::drove::sandbox::WINDOWS_SANDBOX_SUBCOMMAND) {
            return crate::drove::sandbox::run_windows_sandbox_entry(&raw_args[1..]);
        }
    }
    if !daemon
        && let Some(arg0) = std::env::args_os().next()
        && let Some(stem) = std::path::Path::new(&arg0).file_stem().and_then(|s| s.to_str())
        && stem.eq_ignore_ascii_case("drove")
    {
        return crate::drove::cli::main_standalone();
    }
    let args: Vec<_> = if daemon { vec!["serve".into()] } else { std::env::args_os().skip(1).collect() };
    match args.iter().map(|a| a.to_str()).collect::<Vec<_>>().as_slice() {
        [Some("--help" | "-h" | "help")] => {
            write_stdout_raw(HELP);
            return 0;
        }
        [Some("--skill")] => {
            write_stdout_raw(SKILL);
            return 0;
        }
        [Some("--licenses")] => {
            write_stdout_raw(include_str!("../THIRD-PARTY-NOTICES"));
            return 0;
        }
        _ => {}
    }
    let piped = !std::io::stdout().is_terminal();
    let cli = match Cli::try_parse_from(std::env::args_os().take(1).chain(args.iter().cloned())) {
        Ok(cli) => cli,
        Err(e) if !e.use_stderr() || !(piped || args.iter().any(|a| a == "--json")) => e.exit(),
        Err(e) => return fail(&RpcError::new("usage", usage_message(&e), "see `h2 --help`"), true),
    };
    let (cmd, asked) = match cli.cmd {
        Some(Cmd::Checkpoint(ref c)) if matches!(c.sub, checkpoint::CheckpointSub::Hook { .. }) => {
            if let Some((code, msg)) = checkpoint::worktree_guard_hook(c) {
                if let Some(m) = msg {
                    if code == 0 {
                        write_stdout_line(&m);
                    } else {
                        eprintln!("{m}");
                    }
                }
                return code;
            }
            if let Some(noop) = checkpoint::fast_noop_hook(c) {
                write_stdout_line(&noop);
                return 0;
            }
            (cli.cmd, cli.json)
        }
        Some(Cmd::Run(p)) => match Paths::from_env().and_then(|paths| flags_after_task(&paths, p)) {
            Ok((p, trailing)) => (Some(Cmd::Run(p)), cli.json || trailing),
            Err(err) => return fail(&err, cli.json || piped),
        },
        cmd => (cmd, cli.json),
    };
    let json = asked || piped;
    match run(cmd, json, asked) {
        Ok(Some(out)) => {
            print(&out, json);
            0
        }
        Ok(None) => 0,
        Err(err) => fail(&err, json),
    }
}

/// Clap's diagnostic without its usage footer, on one line.
fn usage_message(e: &clap::Error) -> String {
    let text = e.to_string();
    let lines = text.lines().take_while(|l| !l.starts_with("Usage:") && !l.starts_with("For more information"));
    let words: Vec<&str> = lines.map(str::trim).filter(|l| !l.is_empty()).collect();
    words.join(" ").trim_start_matches("error: ").to_string()
}

/// What a command printed: its JSON result and the line a person reads.
struct Out {
    json: Value,
    human: String,
}

/// Writes `text` to stdout without panicking on `BrokenPipe` (e.g. `| head -n 20`).
/// Returns `false` if stdout closed so streaming callers can stop cleanly.
pub(crate) fn write_stdout_raw(text: &str) -> bool {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|_| out.flush()) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => false,
        Err(_) => false,
    }
}

/// Writes `line` + `\n` to stdout without panicking on `BrokenPipe`.
pub(crate) fn write_stdout_line(line: &str) -> bool {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    match out.write_all(line.as_bytes()).and_then(|_| out.write_all(b"\n")).and_then(|_| out.flush()) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => false,
        Err(_) => false,
    }
}

fn print(out: &Out, json: bool) {
    if json {
        let _ = write_stdout_line(&out.json.to_string());
    } else {
        let _ = write_stdout_line(&out.human);
    }
}

fn fail(err: &RpcError, json: bool) -> i32 {
    if err.kind == "cancelled" {
    } else if json {
        let body = json!({"error": {"kind": err.kind, "message": err.message, "hint": err.hint}});
        eprintln!("{body}");
    } else {
        eprintln!("error: {}", err.message);
        if !err.hint.is_empty() {
            eprintln!("hint: {}", err.hint);
        }
    }
    err.exit_code()
}

fn call(paths: &Paths, method: &str, params: Value) -> Result<Value, RpcError> {
    let stream = client::connect_or_start(paths)?;
    match client::call(&stream, method, params.clone()) {
        Ok(val) => Ok(val),
        Err(err) if err.kind == "unknown_method" && crate::api::METHOD_NAMES.contains(&method) => {
            // If an older background daemon is running without newer RPC methods, restart it once and retry.
            let _ = client::call(&stream, "server.restart", json!({}));
            std::thread::sleep(std::time::Duration::from_millis(150));
            let stream = client::connect_or_start(paths)?;
            client::call(&stream, method, params).map_err(|e| client::explain_closed(paths, e))
        }
        Err(err) => Err(client::explain_closed(paths, err)),
    }
}

/// Requires an explicit target or fails with candidate agent/terminal addresses and names.
fn target(paths: &Paths, given: Option<String>, finished: bool) -> Result<String, RpcError> {
    if let Some(a) = given {
        return Ok(a);
    }
    let state = if finished { "finished" } else { "live" };
    let agents = call(paths, "agent.list", json!({"all": finished}))?;
    let terminals = call(paths, "terminal.list", json!({"all": finished})).unwrap_or_else(|_| json!({}));
    let candidates: Vec<&Value> = agents["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(terminals["terminals"].as_array().into_iter().flatten())
        .filter(|a| a["state"] == state)
        .collect();
    let names: Vec<String> = candidates
        .iter()
        .map(|a| match a["name"].as_str() {
            Some(name) => format!("{} ({name})", a["address"].as_str().unwrap_or("?")),
            None => a["address"].as_str().unwrap_or("?").to_string(),
        })
        .collect();
    let message = if names.is_empty() {
        format!("no target given, and none are {state}")
    } else {
        format!("which target? {}", names.join(", "))
    };
    Err(RpcError::new("needs_target", message, "pass an address like a7 or t1, or a name"))
}

/// Bare `h2`: print live agents & terminals overview (or JSON when piped/`--json`).
fn bare(paths: &Paths) -> Result<Option<Out>, RpcError> {
    let mut r = call(paths, "agent.list", json!({}))?;
    if let Ok(tr) = call(paths, "terminal.list", json!({}))
        && let Some(arr) = tr["terminals"].as_array().filter(|a| !a.is_empty())
    {
        r["terminals"] = Value::Array(arr.clone());
    }
    let human = table(&r);
    r["commands"] = json!([
        "h2 run --agent <preset> --prompt <task>",
        "h2 ls",
        "h2 read <a>",
        "h2 send <a> <text>",
        "h2 approve <a>",
        "h2 --skill"
    ]);
    Ok(Some(Out { json: r, human }))
}

fn run(cmd: Option<Cmd>, json: bool, asked: bool) -> Result<Option<Out>, RpcError> {
    let paths = Paths::from_env()?;
    if !matches!(
        &cmd,
        Some(
            Cmd::Serve
                | Cmd::Hold { .. }
                | Cmd::Connect { .. }
                | Cmd::Mcp
                | Cmd::Checkpoint(checkpoint::CheckpointCli { sub: checkpoint::CheckpointSub::Hook { .. } })
        )
    ) {
        crate::platform::legacy_bundle_notice(&paths.state);
    }
    let Some(cmd) = cmd else {
        return bare(&paths);
    };
    let call = |method: &str, params: Value| call(&paths, method, params);
    let target = |given: Option<String>| target(&paths, given, false);
    let pick = |given: String| target((!given.is_empty()).then_some(given));
    let pick_finished = |given: String| crate::cli::target(&paths, (!given.is_empty()).then_some(given), true);
    Ok(Some(match cmd {
        Cmd::Serve => {
            crate::platform::prepare_core();
            crate::server::serve().map_err(|e| RpcError::internal(e.to_string()))?;
            return Ok(None);
        }
        Cmd::Hold { socket } => {
            crate::holder::run(socket).map_err(|e| RpcError::internal(e.to_string()))?;
            return Ok(None);
        }
        Cmd::Attach(p) => {
            if crate::client::agent_id().is_some() {
                return Err(RpcError::invalid_params(
                    "ACP agent cannot attach to a PTY terminal; terminal agents receive messages only via `h2 wait`",
                ));
            }
            let message = crate::attach::run(&paths, &pick(p.terminal)?, p.observe, p.history)?;
            eprintln!("{message}");
            return Ok(None);
        }
        Cmd::Keys(mut p) => {
            if crate::client::agent_id().is_some() {
                return Err(RpcError::invalid_params(
                    "ACP agent cannot send keys to a PTY terminal; terminal agents receive messages only via `h2 wait`",
                ));
            }
            p.caller = Some(caller());
            let r = call("terminal.keys", to(&p))?;
            Out { json: r, human: "keys sent".into() }
        }
        Cmd::Mcp => {
            crate::mcp::serve_stdio()?;
            return Ok(None);
        }
        Cmd::Drove(p) => {
            let (json, human) = crate::drove::cli::execute(&paths, p)?;
            Out { json, human }
        }
        Cmd::Skill(p) => skill::run(&paths, p)?,
        Cmd::Ai { action } => match action {
            AiCmd::Status { on } => {
                let mut params = json!({});
                if let Some(m) = on {
                    params["on"] = json!(m);
                }
                let r = call("ai.status", params)?;
                let proto = r["protocol"].as_str().map(|p| format!(", protocol: {p}")).unwrap_or_default();
                let human = format!(
                    "AI {}: {} (model: {}{proto}, cache hit: {:.0}%, local hits: {})",
                    if r["configured"].as_bool().unwrap_or(false) { "ready" } else { "unconfigured" },
                    r["baseUrl"].as_str().filter(|s| !s.is_empty()).unwrap_or("-"),
                    r["model"].as_str().filter(|s| !s.is_empty()).unwrap_or("auto"),
                    r["usage"]["cacheHitRatio"].as_f64().unwrap_or(0.0) * 100.0,
                    r["usage"]["localCacheHits"].as_u64().unwrap_or(0),
                );
                Out { json: r, human }
            }
            AiCmd::SettingsSet { file, on, consume } => {
                let text = std::fs::read_to_string(&file)
                    .map_err(|e| RpcError::new("usage", format!("cannot read {}: {e}", file.display()), ""))?;
                if consume {
                    let _ = std::fs::remove_file(&file);
                }
                let mut params: Value = serde_json::from_str(&text)
                    .map_err(|e| RpcError::new("usage", format!("invalid JSON in {}: {e}", file.display()), ""))?;
                if let Some(m) = on
                    && let Some(obj) = params.as_object_mut()
                {
                    obj.insert("on".into(), json!(m));
                }
                let r = call("ai.settings.set", params)?;
                Out { json: r, human: "AI settings saved".into() }
            }
            AiCmd::Models { file, url, on, consume } => {
                let mut params = if let Some(path) = file {
                    let text = std::fs::read_to_string(&path)
                        .map_err(|e| RpcError::new("usage", format!("cannot read {}: {e}", path.display()), ""))?;
                    if consume {
                        let _ = std::fs::remove_file(&path);
                    }
                    serde_json::from_str::<Value>(&text)
                        .map_err(|e| RpcError::new("usage", format!("invalid JSON in {}: {e}", path.display()), ""))?
                } else {
                    json!({})
                };
                if let Some(obj) = params.as_object_mut() {
                    if let Some(u) = url {
                        obj.insert("baseUrl".into(), json!(u));
                    }
                    if let Some(o) = on {
                        obj.insert("on".into(), json!(o));
                    }
                }
                let r = call("ai.models", params)?;
                let ok = r["ok"].as_bool().unwrap_or(false);
                let human = if ok {
                    let count = r["models"].as_array().map_or(0, Vec::len);
                    format!("AI models ({}ms): {count} found", r["latencyMs"].as_u64().unwrap_or(0))
                } else {
                    format!("AI models failed: {}", r["error"].as_str().unwrap_or("unknown error"))
                };
                Out { json: r, human }
            }
            AiCmd::Test { file, url, model, on, consume } => {
                let mut params = if let Some(path) = file {
                    let text = std::fs::read_to_string(&path)
                        .map_err(|e| RpcError::new("usage", format!("cannot read {}: {e}", path.display()), ""))?;
                    if consume {
                        let _ = std::fs::remove_file(&path);
                    }
                    serde_json::from_str::<Value>(&text)
                        .map_err(|e| RpcError::new("usage", format!("invalid JSON in {}: {e}", path.display()), ""))?
                } else {
                    json!({})
                };
                if let Some(obj) = params.as_object_mut() {
                    if let Some(u) = url {
                        obj.insert("baseUrl".into(), json!(u));
                    }
                    if let Some(m) = model {
                        obj.insert("model".into(), json!(m));
                    }
                    if let Some(o) = on {
                        obj.insert("on".into(), json!(o));
                    }
                }
                let r = call("ai.test", params)?;
                let ok = r["ok"].as_bool().unwrap_or(false);
                let human = if ok {
                    let proto = r["protocol"].as_str().map(|p| format!(" protocol={p}")).unwrap_or_default();
                    format!(
                        "AI OK ({}ms) model={}{proto}",
                        r["latencyMs"].as_u64().unwrap_or(0),
                        r["discoveredModel"].as_str().unwrap_or("-")
                    )
                } else {
                    format!("AI test failed: {}", r["error"].as_str().unwrap_or("unknown error"))
                };
                Out { json: r, human }
            }
            AiCmd::ProbePromptMode { on } => {
                let mut params = json!({});
                if let Some(m) = on {
                    params["on"] = json!(m);
                }
                let r = call("ai.probe_prompt_mode", params)?;
                let mode = r["detectedMode"].as_str().unwrap_or("pending");
                let probes = r["probesRun"].as_u64().unwrap_or(0);
                let cached = r["cachedInputTokens"].as_u64().unwrap_or(0);
                let human = format!("Prompt mode re-detected: {mode} (probes: {probes}, cached tokens: {cached})");
                Out { json: r, human }
            }
            AiCmd::HandoffRecommend { source, installed, lang } => {
                let source = target(Some(source))?;
                let r = call(
                    "ai.handoff_recommend",
                    json!({
                        "source": source,
                        "installedPresets": installed,
                        "language": lang,
                    }),
                )?;
                let human = format!(
                    "recommend {} ({}) — {}",
                    r["recommendedPreset"].as_str().unwrap_or("-"),
                    r["recommendedKind"].as_str().unwrap_or("handoff"),
                    r["reason"].as_str().unwrap_or("")
                );
                Out { json: r, human }
            }
            AiCmd::Briefing { on, lang, away_minutes, no_ai } => {
                let mut params = json!({ "includeAi": !no_ai });
                if let Some(m) = on {
                    params["on"] = json!(m);
                }
                if let Some(l) = lang {
                    params["language"] = json!(l);
                }
                if let Some(mins) = away_minutes {
                    params["awayMinutes"] = json!(mins);
                }
                let r = call("ai.briefing", params)?;
                let headline = r["headline"].as_str().unwrap_or("No active items");
                let c = &r["counts"];
                let human = format!(
                    "{headline}\n  critical={} needs_you={} (batch={}) ready_review={} working={}",
                    c["critical"].as_u64().unwrap_or(0),
                    c["needsYou"].as_u64().unwrap_or(0),
                    c["batchGroups"].as_u64().unwrap_or(0),
                    c["readyReview"].as_u64().unwrap_or(0),
                    c["working"].as_u64().unwrap_or(0),
                );
                Out { json: r, human }
            }
            AiCmd::SummarizeAgent { target: raw_target, lang, no_ai } => {
                let tgt = target(Some(raw_target))?;
                let mut params = json!({ "target": tgt, "includeAi": !no_ai });
                if let Some(l) = lang {
                    params["language"] = json!(l);
                }
                let r = call("ai.summarize_agent", params)?;
                let human = format!(
                    "{}: {}",
                    r["semanticTitle"].as_str().unwrap_or("-"),
                    r["microSummary"].as_str().unwrap_or("-"),
                );
                Out { json: r, human }
            }
            AiCmd::CommitMessage { cwd, on, lang, no_ai } => {
                let mut params = json!({ "includeAi": !no_ai });
                if let Some(dir) = cwd.or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string())) {
                    params["cwd"] = json!(dir);
                }
                if let Some(m) = on {
                    params["on"] = json!(m);
                }
                if let Some(l) = lang {
                    params["language"] = json!(l);
                }
                let r = call("ai.commit_message", params)?;
                let human = r["message"].as_str().unwrap_or("").to_string();
                Out { json: r, human }
            }
            AiCmd::PolishPrompt { prompt, target: raw_target, kind, context, lang, on } => {
                let mut params = json!({ "prompt": prompt });
                if let Some(t) = raw_target {
                    params["target"] = json!(target(Some(t))?);
                }
                if let Some(k) = kind {
                    params["agentKind"] = json!(k);
                }
                if let Some(c) = context {
                    params["context"] = json!(c);
                }
                if let Some(l) = lang {
                    params["language"] = json!(l);
                }
                if let Some(m) = on {
                    params["on"] = json!(m);
                }
                let r = call("ai.polish_prompt", params)?;
                let human = r["polished"].as_str().unwrap_or("").to_string();
                Out { json: r, human }
            }
        },
        Cmd::Checkpoint(p) => checkpoint::execute(&paths, p)?,
        Cmd::Wait(mut p) => {
            if p.children.is_none() {
                p.agent = Some(target(p.agent)?);
            }
            p.caller = Some(caller());
            let r = call("agent.wait", to(&p))?;
            let human = describe_wait(&r);
            Out { json: r, human }
        }
        Cmd::Send(mut p) => {
            if !p.images.is_empty() || !p.resources.is_empty() || !p.resource_links.is_empty() {
                p.content = crate::acp::media::cli_files_to_content_blocks(&p.images, &p.resources, &p.resource_links)?;
            }
            if p.text.trim().is_empty() && p.content.is_empty() {
                return Err(RpcError::new(
                    "usage",
                    "send requires <TEXT> or at least one --image / --resource / --resource-link",
                    "example: h2 send a1 --image shot.png \"Inspect this error\"",
                ));
            }
            p.caller = Some(caller());
            let r = call("agent.send", to(&p))?;
            let started = match r["started"].as_bool() {
                Some(true) => "started or queued",
                Some(false) => "not started",
                None => "queued",
            };
            let mut human = format!(
                "sent at turn {} ({}); {started} (inbox: {})",
                r["turn"],
                r["status"].as_str().unwrap_or("no status"),
                r["inbox_len"]
            );
            if !r["wait"].is_null() {
                human = format!("{human}\n{}", describe_wait(&r["wait"]));
            }
            Out { json: r, human }
        }
        Cmd::Inbox { action } => match action {
            InboxCmd::Ls(mut p) => {
                p.agent = pick(p.agent)?;
                let r = call("agent.get", to(&p))?;
                let items = r["inbox"].as_array().cloned().unwrap_or_default();
                let human = if items.is_empty() {
                    format!("{}: inbox empty", r["address"].as_str().unwrap_or("?"))
                } else {
                    items
                        .iter()
                        .map(|item| {
                            format!("#{}: {}", item["id"], item["text"].as_str().unwrap_or("").replace('\n', " "))
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Out {
                    json: json!({
                        "address": r["address"],
                        "inbox_len": r["inbox_len"].as_u64().unwrap_or(0),
                        "inbox": items,
                    }),
                    human,
                }
            }
            InboxCmd::Edit(p) => {
                let r = call("agent.inbox_edit", to(&p))?;
                let human = format!(
                    "{}: updated queued message (inbox: {})",
                    r["address"].as_str().unwrap_or("?"),
                    r["inbox_len"].as_u64().unwrap_or(0),
                );
                Out { json: r, human }
            }
            InboxCmd::Rm(p) => {
                let r = call("agent.inbox_remove", to(&p))?;
                let human = format!(
                    "{}: removed queued message (inbox: {})",
                    r["address"].as_str().unwrap_or("?"),
                    r["inbox_len"].as_u64().unwrap_or(0),
                );
                Out { json: r, human }
            }
            InboxCmd::Send(p) => {
                let r = call("agent.inbox_send", to(&p))?;
                let human = format!(
                    "{}: sending queued message now (inbox: {})",
                    r["address"].as_str().unwrap_or("?"),
                    r["inbox_len"].as_u64().unwrap_or(0),
                );
                Out { json: r, human }
            }
            InboxCmd::Resume(mut p) => {
                p.agent = pick(p.agent)?;
                let r = call("agent.inbox_resume", to(&p))?;
                let human = format!(
                    "{}: resumed paused inbox (inbox: {})",
                    r["address"].as_str().unwrap_or("?"),
                    r["inbox_len"].as_u64().unwrap_or(0),
                );
                Out { json: r, human }
            }
            InboxCmd::ClearPaused(mut p) => {
                p.agent = pick(p.agent)?;
                let r = call("agent.inbox_clear_paused", to(&p))?;
                let human = format!("{}: cleared paused inbox", r["address"].as_str().unwrap_or("?"),);
                Out { json: r, human }
            }
        },
        Cmd::Interrupt(mut p) => {
            p.agent = pick(p.agent)?;
            let r = call("agent.interrupt", to(&p))?;
            let human =
                format!("sent {}: {}", r["method"].as_str().unwrap_or("?"), r["outcome"].as_str().unwrap_or("?"));
            Out { json: r, human }
        }
        Cmd::Approve(mut p) => {
            p.agent = pick(p.agent)?;
            p.caller = Some(caller());
            let r = call("agent.approve", to(&p))?;
            let human = format!(
                "approved {} (request {}, option {})",
                r["agent"].as_str().unwrap_or("?"),
                r["request_id"].as_str().unwrap_or("?"),
                r["option_id"].as_str().unwrap_or("-")
            );
            Out { json: r, human }
        }
        Cmd::Deny(mut p) => {
            p.agent = pick(p.agent)?;
            p.caller = Some(caller());
            let r = call("agent.deny", to(&p))?;
            let human = format!(
                "denied {} (request {})",
                r["agent"].as_str().unwrap_or("?"),
                r["request_id"].as_str().unwrap_or("?")
            );
            Out { json: r, human }
        }
        Cmd::Config(p) => {
            let r = call("agent.config", to(&p))?;
            let human = format!(
                "{}: model={} mode={}",
                r["address"].as_str().unwrap_or("?"),
                r["model"].as_str().unwrap_or("-"),
                r["mode"].as_str().unwrap_or("-"),
            );
            Out { json: r, human }
        }
        Cmd::Run(mut p) => {
            if p.on.is_none() {
                p.cwd = from_here(p.cwd);
            }
            p.caller = Some(caller());
            p.wait = !p.bg;
            p.die_with_parent = !p.outlive_parent;
            let r = call("agent.run", to(&p))?;
            if p.wait && !asked {
                return ran(&r);
            }
            let address = r["address"].as_str().unwrap_or("?");
            let mut human = format!("{address} started");
            if let Some(first) = r.get("first_message") {
                match first["sent"].as_bool() {
                    Some(true) => human.push_str("; first message sent"),
                    _ => human
                        .push_str(&format!("; first message NOT sent: {}", first["reason"].as_str().unwrap_or("?"))),
                }
            }
            let mut json = short(&r);
            for key in ["first_message", "wait", "output"] {
                if let Some(v) = r.get(key) {
                    json[key] = v.clone();
                }
            }
            Out { json, human }
        }
        Cmd::Ls(p) => {
            let mut r = call("agent.list", to(&p))?;
            let tr = call("terminal.list", to(&p)).unwrap_or_else(|_| json!({}));
            if let Some(arr) = tr["terminals"].as_array().filter(|a| !a.is_empty()) {
                r["terminals"] = Value::Array(arr.clone());
                r["hidden_finished"] =
                    json!(r["hidden_finished"].as_u64().unwrap_or(0) + tr["hidden_finished"].as_u64().unwrap_or(0));
            }
            let human = table(&r);
            let rows: Vec<Value> = r["agents"].as_array().into_iter().flatten().map(short).collect();
            let mut json = json!({"agents": rows, "hidden_finished": r["hidden_finished"]});
            if let Some(trows) = r["terminals"].as_array() {
                let trows: Vec<Value> = trows.iter().map(short).collect();
                json["terminals"] = Value::Array(trows);
            }
            if let Some(machines) = r["machines"].as_array().filter(|m| !m.is_empty()) {
                let keep = |m: &Value| -> Value {
                    let fields = ["name", "state", "since_ms", "why"].into_iter();
                    fields
                        .filter_map(|k| m.get(k).filter(|v| !v.is_null()).map(|v| (k.to_string(), v.clone())))
                        .collect::<serde_json::Map<_, _>>()
                        .into()
                };
                json["machines"] = machines.iter().map(keep).collect();
            }
            Out { json, human }
        }
        Cmd::Get(mut p) => {
            let name = pick(p.agent)?;
            p.agent = name.clone();
            p.caller = Some(caller());
            let r = match call("agent.get", to(&p)) {
                Err(e) if e.kind == "not_found" => call("terminal.get", json!({"terminal": name, "caller": p.caller}))?,
                other => other?,
            };
            let human = describe(&r);
            Out { json: r, human }
        }
        Cmd::Kill(mut p) => {
            let name = pick(p.agent)?;
            p.agent = name.clone();
            p.caller = Some(caller());
            let r = match call("agent.kill", to(&p)) {
                Err(e) if e.kind == "not_found" => {
                    call("terminal.kill", json!({"terminal": name, "caller": p.caller}))?
                }
                other => other?,
            };
            let human = describe(&r);
            Out { json: r, human }
        }
        Cmd::Rm(mut p) => {
            p.agent = pick_finished(p.agent)?;
            let r = call("agent.rm", to(&p))?;
            let human = format!("removed {}", r["removed"].as_str().unwrap_or("?"));
            Out { json: r, human }
        }
        Cmd::Keep(p) => {
            let name = p.agent.clone();
            let keep = p.keep;
            let r = match call("agent.keep", to(&p)) {
                Err(e) if e.kind == "not_found" => call("terminal.keep", json!({"terminal": name, "keep": keep}))?,
                other => other?,
            };
            let human = describe(&r);
            Out { json: r, human }
        }
        Cmd::Label(p) => {
            let name = p.agent.clone();
            let labels_val = to(&p)["labels"].clone();
            let r = match call("agent.label", to(&p)) {
                Err(e) if e.kind == "not_found" => {
                    call("terminal.label", json!({"terminal": name, "labels": labels_val}))?
                }
                other => other?,
            };
            let human = describe(&r);
            Out { json: r, human }
        }
        Cmd::Read(mut p) => {
            p.agent = pick(p.agent)?;
            p.caller = Some(caller());
            let r = call("agent.read", to(&p))?;
            let mut human = r["text"].as_str().unwrap_or_default().to_string();
            if r["complete"] == false {
                human.push_str("\n[h2] older transcript lines omitted (increase --lines or omit for full transcript)");
            }
            if !asked {
                write_stdout_line(&human);
                return Ok(None);
            }
            Out { json: r, human }
        }
        Cmd::Handoff(mut p) => {
            p.source = pick(p.source)?;
            p.cwd = p.cwd.and_then(|c| from_here(Some(c)));
            p.caller = Some(caller());
            let r = call("agent.handoff", to(&p))?;
            let target = r["target"].as_str().unwrap_or("?");
            let target_kind = r["target_kind"].as_str().unwrap_or("agent");
            let kind = r["kind"].as_str().unwrap_or("handoff");
            let source = r["source"].as_str().unwrap_or("?");
            let bytes = r["bytes"].as_u64().unwrap_or(0);
            let native = if r["native_fork"] == true { " [native-fork]" } else { "" };
            let mut human = format!("{target} ({target_kind}) <- {kind} from {source} ({bytes} bytes){native}");
            if let Some(path) = r["artifact_path"].as_str() {
                human.push_str(&format!("\n  staged: {path}"));
            }
            if !r["wait"].is_null() {
                human.push_str(&format!("\n  {}", describe_wait(&r["wait"])));
            }
            Out { json: r, human }
        }
        Cmd::Put(mut p) => {
            let bytes = std::fs::read(&p.name)
                .map_err(|e| RpcError::new("usage", format!("cannot read {}: {e}", p.name), ""))?;
            p.data = crate::put::encode(&bytes);
            let r = call("agent.put", to(&p))?;
            let human = r["path"].as_str().unwrap_or_default().to_string();
            Out { json: r, human }
        }
        Cmd::Exec(mut p) => {
            if p.on.is_none() {
                p.cwd = from_here(p.cwd);
            }
            if let Some(line) = shell_line(&p.argv) {
                (p.line, p.argv) = (Some(line), Vec::new());
            }
            if p.stdin {
                p.input = Some(crate::put::encode(&piped(crate::exec::MAX_STDIN)?));
            }
            let r = call("machine.exec", to(&p))?;
            if !asked {
                exec_out(&r);
            }
            Out { json: r, human: String::new() }
        }
        Cmd::Connect { session } => {
            let paths = match session {
                Some(s) => {
                    // SAFETY: no other thread runs yet; a core this starts inherits it.
                    unsafe { std::env::set_var("H2_SESSION", s) };
                    Paths::from_env()?
                }
                None => paths,
            };
            connect(&paths)?;
            return Ok(None);
        }
        Cmd::Events(p) => {
            follow_events(&paths, to(&p), json)?;
            return Ok(None);
        }
        Cmd::Templates => {
            let r = call("templates.list", json!({}))?;
            let rows: Vec<String> = r["templates"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| match t["error"].as_str() {
                    Some(e) => format!("{:<16} (broken: {e})", t["name"].as_str().unwrap_or("?")),
                    None => {
                        format!("{:<16} {}", t["name"].as_str().unwrap_or("?"), t["description"].as_str().unwrap_or(""))
                    }
                })
                .collect();
            let human = if rows.is_empty() {
                format!("no templates yet: add markdown files to {}", crate::templates::dir(&paths).display())
            } else {
                rows.join("\n")
            };
            Out { json: r, human }
        }
        Cmd::Schema => {
            let r = call("rpc.discover", json!({}))?;
            let human = serde_json::to_string_pretty(&r).unwrap_or_default();
            Out { json: r, human }
        }
        Cmd::Server { action: action @ (ServerCmd::Start | ServerCmd::Status | ServerCmd::Restart { .. }) } => {
            if let ServerCmd::Restart { fresh } = action {
                restart(&paths, fresh)?;
            }
            let r = if crate::platform::status_may_start_core() {
                call("server.status", json!({}))?
            } else {
                let stream = if matches!(action, ServerCmd::Status) {
                    client::require_existing(&paths)?
                } else {
                    client::connect_or_start_explicit(&paths)?
                };
                client::call(&stream, "server.status", json!({}))?
            };
            let id = r["core"]["id"].as_str().unwrap_or("?");
            let human = format!(
                "core {} ({}) running, pid {}, up {}s",
                r["core"]["name"].as_str().unwrap_or("?"),
                id.get(..8).unwrap_or(id),
                r["pid"],
                r["uptime_ms"].as_u64().unwrap_or(0) / 1000
            );
            Out { json: r, human }
        }
        Cmd::Server { action: ServerCmd::Stop } => stop(&paths)?,
        Cmd::Update { from: None, version, force } => {
            let this = format!("h2 {}", env!("CARGO_PKG_VERSION"));
            if let Some(v) = &version
                && !force
                && !older(&this, &format!("h2 {v}"))
                && !crate::platform::needs_bundle_update()
            {
                let human = format!("{this} is already {v} or newer");
                return Ok(Some(up_to_date(&paths, v, human)?));
            }
            let scratch = crate::releases::Scratch::new()?;
            let requested = version
                .as_deref()
                .map(|v| if !force && !older(&this, &format!("h2 {v}")) { env!("CARGO_PKG_VERSION") } else { v });
            let m = crate::releases::manifest(requested, &scratch.0)?;
            if m.generation != crate::api::GENERATION && !force {
                return Err(RpcError::new(
                    "refused",
                    format!(
                        "h2 {} speaks protocol generation {}, this h2 {}: apps built for it would stop working",
                        m.version,
                        m.generation,
                        crate::api::GENERATION
                    ),
                    "update the apps that use h2 first, then h2 update --force",
                ));
            }
            if version.is_none()
                && !force
                && !older(&this, &format!("h2 {}", m.version))
                && !crate::platform::needs_bundle_update()
            {
                let human = format!("{this} is up to date (latest release: {})", m.version);
                up_to_date(&paths, &m.version, human)?
            } else {
                let file = crate::releases::download(
                    &m,
                    &crate::releases::this_target(),
                    &scratch.0,
                    crate::platform::prefer_release_bundle(),
                )?;
                let mut out = update(&paths, &file, force)?;
                out.json["release"] = m.version.into();
                out
            }
        }
        Cmd::Update { from: Some(from), force, .. } => update(&paths, &from, force)?,
        Cmd::Machine { action } => machine(&paths, action, call)?,
        Cmd::File { action } => match file(action, asked, call)? {
            Some(out) => out,
            None => return Ok(None),
        },
    }))
}

fn to(params: &impl serde::Serialize) -> Value {
    serde_json::to_value(params).unwrap_or_default()
}

/// One string with spaces or shell signs is a shell line for `exec`.
fn shell_line(argv: &[String]) -> Option<String> {
    let shell_sign = |c: char| c.is_whitespace() || ";|&<>$`*?(){}~'\"".contains(c);
    match argv {
        [line] if line.contains(shell_sign) => Some(line.clone()),
        _ => None,
    }
}

fn here() -> Option<String> {
    let real = std::env::current_dir().ok()?;
    let logical = std::env::var("PWD").ok().filter(|p| std::fs::canonicalize(p).ok() == real.canonicalize().ok());
    Some(logical.unwrap_or_else(|| real.display().to_string()))
}

fn from_here(cwd: Option<String>) -> Option<String> {
    match (cwd, here()) {
        (Some(cwd), Some(here)) => Some(std::path::Path::new(&here).join(cwd).display().to_string()),
        (cwd, here) => cwd.or(here),
    }
}

/// Who is asking: this h2 agent or terminal, else the shell or script that ran us.
fn caller() -> String {
    client::agent_id().or_else(client::terminal_id).unwrap_or_else(|| format!("ppid:{}", crate::platform::parent_pid()))
}

/// Prints the snapshot, then every notification until the core goes away.
fn follow_events(paths: &Paths, params: Value, json: bool) -> Result<(), RpcError> {
    use std::io::BufRead;
    let stream = client::connect_or_start(paths)?;
    let snapshot = client::call(&stream, "events.subscribe", params)?;
    let ok = if json {
        write_stdout_line(&snapshot.to_string())
    } else {
        write_stdout_line(&table(&json!({"agents": snapshot["agents"], "terminals": snapshot["terminals"]})))
    };
    if !ok {
        return Ok(());
    }
    for line in std::io::BufReader::new(&stream).lines() {
        let Ok(line) = line else { break };
        if json {
            if !write_stdout_line(&line) {
                break;
            }
            continue;
        }
        let msg: Value = serde_json::from_str(&line).unwrap_or_default();
        let p = &msg["params"];
        let written = match msg["method"].as_str() {
            Some("event") if p["type"] == "doc" => {
                let name = p["data"]["name"].as_str().unwrap_or("?");
                let machine = p["machine"].as_str().map(|m| format!("{m}/")).unwrap_or_default();
                write_stdout_line(&format!("{machine}{name}  doc  version {}", p["data"]["version"]))
            }
            Some("event") => write_stdout_line(&format!(
                "{:<6} {:<10} {}",
                p["agent"].as_str().or_else(|| p["terminal"].as_str()).unwrap_or("?"),
                p["type"].as_str().unwrap_or("?"),
                p["data"]
            )),
            Some("missed") => write_stdout_line(&format!(
                "[missed some events; {} agents, {} terminals now]",
                p["agents"].as_array().map_or(0, Vec::len),
                p["terminals"].as_array().map_or(0, Vec::len)
            )),
            _ => true,
        };
        if !written {
            break;
        }
    }
    Ok(())
}

/// Copies bytes between stdin/stdout and the core: plain or framed, untouched.
fn connect(paths: &Paths) -> Result<(), RpcError> {
    let stream = crate::platform::connect_for_app(paths)?;
    let mut to_core = stream.try_clone().map_err(|e| RpcError::internal(e.to_string()))?;
    let owed = std::sync::Arc::new(std::sync::Mutex::new(Owed::default()));
    let input = owed.clone();
    std::thread::spawn(move || {
        forward(&mut std::io::BufReader::new(&mut *crate::platform::console::stdin()), &mut to_core, &input);
        let mut owed = input.lock().unwrap();
        owed.input_done = true;
        if cfg!(unix) || owed.replies == 0 {
            let _ = to_core.shutdown(std::net::Shutdown::Write);
        }
    });
    let mut out = crate::platform::console::stdout();
    let mut buf = [0u8; 64 * 1024];
    let mut counting = true;
    while let Ok(n @ 1..) = (&stream).read(&mut buf) {
        if std::io::Write::write_all(&mut *out, &buf[..n]).is_err() {
            break;
        }
        let mut owed = owed.lock().unwrap();
        for _ in buf[..n].iter().filter(|b| **b == b'\n') {
            if !counting {
                break;
            }
            owed.replies = owed.replies.saturating_sub(1);
            counting = !(owed.plain_ended && owed.replies == 0);
        }
        if cfg!(windows) && owed.input_done && owed.replies == 0 {
            break;
        }
    }
    Ok(())
}

#[derive(Default)]
struct Owed {
    replies: usize,
    plain_ended: bool,
    input_done: bool,
}

fn forward(from: &mut impl std::io::BufRead, to: &mut impl std::io::Write, owed: &std::sync::Mutex<Owed>) {
    let mut line = Vec::new();
    loop {
        line.clear();
        if !matches!(from.read_until(b'\n', &mut line), Ok(1..)) {
            return;
        }
        if line.last() != Some(&b'\n') {
            line.push(b'\n');
        }
        let (answered, last) = plain_line(&line);
        {
            let mut owed = owed.lock().unwrap();
            owed.replies += usize::from(answered);
            owed.plain_ended |= last;
        }
        if to.write_all(&line).is_err() {
            return;
        }
        if last {
            return pump(from, to);
        }
    }
}

fn plain_line(line: &[u8]) -> (bool, bool) {
    let text = String::from_utf8_lossy(line);
    if text.trim().is_empty() {
        return (false, false);
    }
    let Ok(request) = serde_json::from_str::<Value>(&text) else { return (true, false) };
    let method = request["method"].as_str();
    let answered = method.is_none() || request.get("id").is_some();
    let last = method == Some("events.subscribe") || (method == Some("hello") && request["params"]["framed"] == true);
    (answered, last)
}

fn pump(from: &mut impl Read, to: &mut impl std::io::Write) {
    let mut buf = [0u8; 64 * 1024];
    while let Ok(n @ 1..) = from.read(&mut buf) {
        if to.write_all(&buf[..n]).is_err() {
            return;
        }
    }
}

/// Which API method each command calls. Methods missing here are API-only.
pub const COMMANDS: &[(&str, &str)] = &[
    ("run", "agent.run"),
    ("ls", "agent.list"),
    ("get", "agent.get"),
    ("kill", "agent.kill"),
    ("rm", "agent.rm"),
    ("label", "agent.label"),
    ("keep", "agent.keep"),
    ("send", "agent.send"),
    ("inbox ls", "agent.get"),
    ("inbox edit", "agent.inbox_edit"),
    ("inbox rm", "agent.inbox_remove"),
    ("inbox send", "agent.inbox_send"),
    ("inbox resume", "agent.inbox_resume"),
    ("inbox clear-paused", "agent.inbox_clear_paused"),
    ("wait", "agent.wait"),
    ("interrupt", "agent.interrupt"),
    ("approve", "agent.approve"),
    ("deny", "agent.deny"),
    ("config", "agent.config"),
    ("read", "agent.read"),
    ("handoff", "agent.handoff"),
    ("attach", "terminal.attach"),
    ("keys", "terminal.keys"),
    ("put", "agent.put"),
    ("exec", "machine.exec"),
    ("file read", "file.read"),
    ("file write", "file.write"),
    ("file edit", "file.edit"),
    ("events", "events.subscribe"),
    ("templates", "templates.list"),
    ("schema", "rpc.discover"),
    ("server start", "server.status"),
    ("server status", "server.status"),
    ("server stop", "server.stop"),
    ("server restart", "server.restart"),
    ("machine add", "machine.add"),
    ("machine ls", "machine.list"),
    ("machine login", "machine.connect"),
    ("machine rename", "machine.rename"),
    ("machine rm", "machine.rm"),
    ("drove settings", "drove.settings.get"),
    ("drove settings-set", "drove.settings.set"),
    ("drove probe", "drove.probe"),
    ("drove probe-ai-diagnose", "drove.probe.ai_diagnose"),
    ("drove ai-test", "drove.ai.test"),
    ("drove roles-ai-config", "drove.roles.ai_configure"),
    ("drove magpie-test", "drove.magpie.test"),
    ("drove web", "drove.web.open"),
    ("ai status", "ai.status"),
    ("ai settings-set", "ai.settings.set"),
    ("ai models", "ai.models"),
    ("ai test", "ai.test"),
    ("ai probe-prompt-mode", "ai.probe_prompt_mode"),
    ("ai handoff-recommend", "ai.handoff_recommend"),
    ("ai briefing", "ai.briefing"),
    ("ai summarize-agent", "ai.summarize_agent"),
    ("ai commit-message", "ai.commit_message"),
    ("ai polish-prompt", "ai.polish_prompt"),
    ("checkpoint ls", "checkpoint.list"),
    ("checkpoint snap", "checkpoint.capture"),
    ("checkpoint diff", "checkpoint.diff"),
    ("checkpoint rollback", "checkpoint.rollback"),
    ("checkpoint bisect", "checkpoint.bisect"),
];
/// Client/app methods not mapped 1:1 from a separate top-level CLI subcommand.
pub const API_ONLY: &[&str] = &[
    "hello",
    "doc.get",
    "doc.set",
    "terminal.run",
    "terminal.list",
    "terminal.get",
    "terminal.kill",
    "terminal.rm",
    "terminal.keep",
    "terminal.label",
    "terminal.read",
    "terminal.send",
    "terminal.wait",
    "terminal.interrupt",
    "terminal.put",
    "checkpoint.hook",
    "agent.review",
    "ai.complete",
    "ai.classify_terminal_state",
    "drove.http_proxy",
    "drove.subscribe",
];

#[cfg(test)]
mod tests;
