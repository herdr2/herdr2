//! `h2`: one API call per command. Tables for people, JSON when piped or `--json`.

use crate::api::{
    AgentParams, ApproveParams, ExecParams, InterruptParams, KeepParams, LabelParams, ListParams, PutParams,
    ReadParams, RunParams, SendParams, SubscribeParams, WaitParams,
};
use crate::client;
use crate::paths::Paths;
use crate::rpc::RpcError;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::io::{IsTerminal, Read};

mod file;
mod machine;
mod output;
mod run;
mod server;
mod update;
use file::*;
use machine::*;
use output::*;
use run::*;
use server::*;
use update::*;

#[derive(Parser)]
#[command(name = "h2", version, about = "herdr2: headless ACP orchestration runtime for coding agents")]
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
    /// Wait for children (--children any) or one agent; a message to you also ends it.
    Wait(WaitParams),
    /// Send ACP `session/cancel` to stop the current turn cleanly.
    Interrupt(InterruptParams),
    /// Approve a blocked agent's pending ACP permission request.
    Approve(ApproveParams),
    /// Deny a blocked agent's pending ACP permission request.
    Deny(ApproveParams),
    /// Print an agent's transcript or final assistant text (`--last`).
    Read(ReadParams),
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
    let args: Vec<_> = if daemon { vec!["serve".into()] } else { std::env::args_os().skip(1).collect() };
    match args.iter().map(|a| a.to_str()).collect::<Vec<_>>().as_slice() {
        [Some("--help" | "-h" | "help")] => {
            print!("{HELP}");
            return 0;
        }
        [Some("--skill")] => {
            print!("{SKILL}");
            return 0;
        }
        [Some("--licenses")] => {
            print!("{}", include_str!("../THIRD-PARTY-NOTICES"));
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

fn print(out: &Out, json: bool) {
    if json {
        println!("{}", out.json);
    } else {
        println!("{}", out.human);
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
    client::call(&stream, method, params).map_err(|e| client::explain_closed(paths, e))
}

/// Requires an explicit target or fails with candidate agent addresses/names.
fn target(paths: &Paths, given: Option<String>, finished: bool) -> Result<String, RpcError> {
    if let Some(a) = given {
        return Ok(a);
    }
    let list = call(paths, "agent.list", json!({"all": finished}))?;
    let state = if finished { "finished" } else { "live" };
    let candidates: Vec<&Value> =
        list["agents"].as_array().into_iter().flatten().filter(|a| a["state"] == state).collect();
    let names: Vec<String> = candidates
        .iter()
        .map(|a| match a["name"].as_str() {
            Some(name) => format!("{} ({name})", a["address"].as_str().unwrap_or("?")),
            None => a["address"].as_str().unwrap_or("?").to_string(),
        })
        .collect();
    let message = if names.is_empty() {
        format!("no agent given, and none are {state}")
    } else {
        format!("which agent? {}", names.join(", "))
    };
    Err(RpcError::new("needs_target", message, "pass an address like a7 or a name"))
}

/// Bare `h2`: print the live agents overview (or JSON when piped/`--json`).
fn bare(paths: &Paths) -> Result<Option<Out>, RpcError> {
    let mut r = call(paths, "agent.list", json!({}))?;
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
    if !matches!(&cmd, Some(Cmd::Serve | Cmd::Connect { .. } | Cmd::Mcp)) {
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
        Cmd::Mcp => {
            crate::mcp::serve_stdio()?;
            return Ok(None);
        }
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
        Cmd::Interrupt(mut p) => {
            p.agent = pick(p.agent)?;
            let r = call("agent.interrupt", to(&p))?;
            let human = format!("sent {}: {}", r["method"].as_str().unwrap_or("?"), r["outcome"].as_str().unwrap_or("?"));
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
            let r = call("agent.list", to(&p))?;
            let human = table(&r);
            let rows: Vec<Value> = r["agents"].as_array().into_iter().flatten().map(short).collect();
            let mut json = json!({"agents": rows, "hidden_finished": r["hidden_finished"]});
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
            (p.agent, p.caller) = (pick(p.agent)?, Some(caller()));
            let r = call("agent.get", to(&p))?;
            let human = describe(&r);
            Out { json: r, human }
        }
        Cmd::Kill(mut p) => {
            (p.agent, p.caller) = (pick(p.agent)?, Some(caller()));
            let r = call("agent.kill", to(&p))?;
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
            let r = call("agent.keep", to(&p))?;
            let human = describe(&r);
            Out { json: r, human }
        }
        Cmd::Label(p) => {
            let r = call("agent.label", to(&p))?;
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
                println!("{human}");
                return Ok(None);
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

/// Who is asking: this h2 agent, else the shell or script that ran us.
fn caller() -> String {
    client::agent_id().unwrap_or_else(|| format!("ppid:{}", crate::platform::parent_pid()))
}

/// Prints the snapshot, then every notification until the core goes away.
fn follow_events(paths: &Paths, params: Value, json: bool) -> Result<(), RpcError> {
    use std::io::BufRead;
    let stream = client::connect_or_start(paths)?;
    let snapshot = client::call(&stream, "events.subscribe", params)?;
    if json {
        println!("{snapshot}");
    } else {
        println!("{}", table(&json!({"agents": snapshot["agents"]})));
    }
    for line in std::io::BufReader::new(&stream).lines() {
        let Ok(line) = line else { break };
        if json {
            println!("{line}");
            continue;
        }
        let msg: Value = serde_json::from_str(&line).unwrap_or_default();
        let p = &msg["params"];
        match msg["method"].as_str() {
            Some("event") if p["type"] == "doc" => {
                let name = p["data"]["name"].as_str().unwrap_or("?");
                let machine = p["machine"].as_str().map(|m| format!("{m}/")).unwrap_or_default();
                println!("{machine}{name}  doc  version {}", p["data"]["version"]);
            }
            Some("event") => println!(
                "{:<6} {:<10} {}",
                p["agent"].as_str().unwrap_or("?"),
                p["type"].as_str().unwrap_or("?"),
                p["data"]
            ),
            Some("missed") => {
                println!("[missed some events; {} agents now]", p["agents"].as_array().map_or(0, Vec::len))
            }
            _ => {}
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
    ("wait", "agent.wait"),
    ("interrupt", "agent.interrupt"),
    ("approve", "agent.approve"),
    ("deny", "agent.deny"),
    ("read", "agent.read"),
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
];
/// Client-only methods: handshake and opaque document storage.
pub const API_ONLY: &[&str] = &["hello", "doc.get", "doc.set"];

#[cfg(test)]
mod tests;
