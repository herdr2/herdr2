//! `h2 drove` and multicall `drove` CLI commands (settings, coordinator DAG, and worker protocol).

use super::types::RoleTarget;
use super::yaml::parse_yaml_or_json;
use crate::client;
use crate::paths::Paths;
use crate::rpc::RpcError;
use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use std::io::{IsTerminal, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "drove", version, about = "Drove multi-agent orchestration for herdr2")]
struct StandaloneCli {
    /// JSON output even in a terminal.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Option<DroveAction>,
}

#[derive(Args)]
pub struct DroveCli {
    #[command(subcommand)]
    pub action: Option<DroveAction>,
}

#[derive(Subcommand)]
pub enum DroveAction {
    /// Show Drove web server, configured roles, AI status, and active runs.
    Status {
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Print or open the Drove Web UI URL in the browser.
    Web {
        /// Pre-select a run ID (`run_...` or `machine/run_...`).
        #[arg(long)]
        run: Option<String>,
        /// Target remote machine.
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
        /// Print URL only without launching a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Read Drove configuration, resolved roles, and AI connection status.
    Settings {
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Update Drove configuration from a JSON file (removes temp file if requested).
    SettingsSet {
        #[arg(long, value_name = "FILE")]
        file: PathBuf,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
        /// Delete the input JSON file right after reading it (safe for API keys).
        #[arg(long)]
        consume: bool,
    },
    /// Probe installed ACP agents and discover their live model & effort options.
    Probe {
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
        /// Run live ACP initialize+session/new probing instead of quick cache/PATH scan.
        #[arg(long)]
        refresh: bool,
    },
    /// Test the lightweight AI assistant URL and API key.
    AiTest {
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
        /// Delete `--file` right after reading it.
        #[arg(long)]
        consume: bool,
    },
    /// Use the configured AI assistant model to auto-configure Drove roles, models, and candidate strengths.
    RolesAiConfig {
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
        /// Configuration scope: `models`, `strengths`, or `all`.
        #[arg(long)]
        mode: Option<String>,
        /// Optional preference instruction for how roles should be assigned.
        #[arg(long)]
        instruction: Option<String>,
        /// Preview without saving to drove.json.
        #[arg(long)]
        dry_run: bool,
        /// Preferred language for strength tags and rationale (e.g. zh-CN or en).
        #[arg(long)]
        language: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
        /// Delete `--file` right after reading it.
        #[arg(long)]
        consume: bool,
    },
    /// Test the optional Magpie quota gateway.
    MagpieTest {
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Set the active default Run ID for subsequent CLI commands.
    Use {
        run_id: String,
    },
    /// Create a Drove Run from a YAML/JSON plan file or stdin and launch its DAG.
    Plan {
        /// Plan YAML or JSON file (`{ objective?, maxConcurrency?, verify?, tasks: [...] }` or task list).
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
        /// Read plan YAML/JSON from stdin.
        #[arg(long)]
        stdin: bool,
        /// Run objective (defaults to plan `objective` or directory name).
        #[arg(long)]
        objective: Option<String>,
        /// Workspace root (defaults to current working directory).
        #[arg(long)]
        cwd: Option<String>,
        /// Maximum parallel workers (1..16).
        #[arg(long)]
        max_concurrency: Option<u32>,
        /// Run-level verification command executed on `complete`.
        #[arg(long)]
        verify: Option<String>,
        /// Override coordinator session address (defaults to `$H2_AGENT_ID` or `$H2_TERMINAL_ID`).
        #[arg(long)]
        session: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Append one or more tasks to an existing Run.
    Add {
        /// Run ID (defaults to `drove use` or latest active run).
        #[arg(long)]
        run: Option<String>,
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
        #[arg(long)]
        stdin: bool,
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        change: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long, value_delimiter = ',')]
        deps: Vec<String>,
        #[arg(long)]
        verify: Option<String>,
        #[arg(long)]
        worktree: bool,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Quickly launch a single-task Run from the command line.
    Quick {
        prompt: String,
        #[arg(long, default_value = "coder")]
        role: String,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        verify: Option<String>,
        #[arg(long)]
        worktree: bool,
        #[arg(long)]
        wait: bool,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Wait for pending coordinator messages, task failures, or Run completion.
    Wait {
        #[arg(long)]
        run: Option<String>,
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        #[arg(long, default_value_t = 0)]
        since_seq: u64,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// List unread coordinator inbox messages for a Run.
    Inbox {
        #[arg(long)]
        run: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Inspect a Run, Task, or Message snapshot by ID.
    Show {
        id: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Acknowledge a coordinator inbox message.
    Ack {
        message_id: String,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Reply to a worker question or escalation message.
    Reply {
        message_id: String,
        body: String,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Send guidance to a running task worker (or interrupt current turn).
    Send {
        task: String,
        text: String,
        #[arg(long)]
        interrupt: bool,
        #[arg(long)]
        run: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Inspect structured diagnostics and recent items for a task or attempt.
    Inspect {
        target: String,
        #[arg(long)]
        run: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Retry a task with optional hint or new role/agent/model.
    Retry {
        task: String,
        #[arg(long)]
        hint: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        review_agent: Option<String>,
        #[arg(long)]
        review_model: Option<String>,
        #[arg(long)]
        no_review: bool,
        #[arg(long)]
        fresh: bool,
        #[arg(long)]
        run: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Override role/agent/model/effort assignment on a pending or ready task.
    Route {
        task: String,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        worker: Option<String>,
        #[arg(long)]
        run: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Cancel a specific task (by ID/key) or an entire Run.
    Cancel {
        target: Option<String>,
        #[arg(long)]
        run: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Finalize a Run (runs Run-level verify gate if configured).
    Complete {
        run: Option<String>,
        #[arg(long)]
        summary: Option<String>,
        #[arg(long)]
        force: bool,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Pause scheduling new tasks in a Run.
    Pause {
        run: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Resume a paused Run.
    Resume {
        run: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Worker protocol subcommands invoked by spawned worker ACP sessions.
    Worker {
        #[command(subcommand)]
        action: WorkerAction,
    },
    /// Install Drove agent skills (`drove`, `drove-add`, `drove-setup`).
    Install {
        #[arg(long, value_name = "DIR")]
        target: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum WorkerAction {
    /// Print current worker & active attempt context from the daemon.
    Context,
    /// Drain unread notes sent to this worker attempt.
    Inbox,
    /// Ask the coordinator a blocking question and print the reply.
    Ask {
        question: String,
        #[arg(long, value_delimiter = ',')]
        options: Vec<String>,
        #[arg(long)]
        recommend: Option<String>,
        #[arg(long)]
        why: Option<String>,
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
    /// Post a non-blocking progress note to the coordinator.
    Note {
        text: String,
        #[arg(long)]
        subject: Option<String>,
    },
    /// Read a dependency task result, verify log, or review patch slice.
    Read {
        target: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 65536)]
        limit: usize,
        #[arg(long)]
        grep: Option<String>,
    },
    /// Submit task attempt completion or explicit failure to the Drove daemon gates.
    Done {
        #[arg(long, default_value = "done")]
        outcome: String,
        #[arg(long)]
        summary: Option<String>,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        data: Option<String>,
        #[arg(long, value_delimiter = ',')]
        files: Vec<String>,
        #[arg(long)]
        no_retry: bool,
    },
}

/// Entrypoint when invoked via `drove` binary symlink/argv[0].
pub fn main_standalone() -> i32 {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let piped = !std::io::stdout().is_terminal();
    let cli = match StandaloneCli::try_parse_from(std::env::args_os().take(1).chain(args.iter().cloned())) {
        Ok(c) => c,
        Err(e) if !e.use_stderr() || !(piped || args.iter().any(|a| a == "--json")) => e.exit(),
        Err(e) => {
            let msg = e.to_string();
            let err = RpcError::new("usage", msg.trim(), "see `drove --help`");
            eprintln!("{}", json!({"error": {"kind": err.kind, "message": err.message, "hint": err.hint}}));
            return 2;
        }
    };
    let json_mode = cli.json || piped;
    let paths = match Paths::from_env() {
        Ok(p) => p,
        Err(e) => return print_err(&e, json_mode),
    };
    match execute(&paths, DroveCli { action: cli.cmd }) {
        Ok((val, human)) => {
            if json_mode {
                println!("{val}");
            } else {
                println!("{human}");
            }
            0
        }
        Err(e) => print_err(&e, json_mode),
    }
}

fn print_err(err: &RpcError, json_mode: bool) -> i32 {
    if json_mode {
        eprintln!("{}", json!({"error": {"kind": err.kind, "message": err.message, "hint": err.hint}}));
    } else {
        eprintln!("error: {}", err.message);
        if !err.hint.is_empty() {
            eprintln!("hint: {}", err.hint);
        }
    }
    err.exit_code()
}

fn call_core(paths: &Paths, method: &str, params: Value) -> Result<Value, RpcError> {
    let stream = client::connect_or_start(paths)?;
    client::call(&stream, method, params).map_err(|e| client::explain_closed(paths, e))
}

fn proxy_call(
    paths: &Paths,
    on: Option<&str>,
    method: &str,
    path: &str,
    query: &str,
    body: Option<Value>,
) -> Result<Value, RpcError> {
    let mut params = json!({
        "method": method,
        "path": path,
        "query": query,
        "headers": {},
    });
    if let Some(m) = on {
        params["on"] = Value::String(m.to_string());
    }
    if let Some(b) = body {
        params["body"] = b;
    }
    let resp = call_core(paths, "drove.http_proxy", params)?;
    let status = resp["status"].as_u64().unwrap_or(500);
    let resp_body = resp["body"].clone();
    if status >= 400 {
        let code = resp_body["error"].as_str().unwrap_or("drove_error");
        let msg = resp_body["message"].as_str().unwrap_or("request failed");
        return Err(RpcError::new(code, msg, ""));
    }
    Ok(resp_body)
}

fn current_run_file(paths: &Paths) -> PathBuf {
    paths.state.join("drove").join("current_run")
}

fn save_current_run(paths: &Paths, run_id: &str) {
    let p = current_run_file(paths);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, run_id);
}

fn resolve_run_id(paths: &Paths, on: Option<&str>, explicit: Option<String>) -> Result<String, RpcError> {
    if let Some(r) = explicit.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        return Ok(r);
    }
    if on.is_none()
        && let Ok(text) = std::fs::read_to_string(current_run_file(paths))
    {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let list = proxy_call(paths, on, "GET", "/api/runs", "local=1", None)?;
    if let Some(arr) = list["runs"].as_array() {
        if let Some(active) = arr.iter().find(|r| r["status"] == "active").or_else(|| arr.first())
            && let Some(id) = active["id"].as_str()
        {
            return Ok(id.to_string());
        }
    }
    Err(RpcError::invalid_params("no run found; pass --run <RUN_ID> or create one with `drove plan`"))
}

fn resolve_task_id(
    paths: &Paths,
    on: Option<&str>,
    run_opt: Option<String>,
    task_id_or_key: &str,
) -> Result<String, RpcError> {
    if task_id_or_key.starts_with("tsk_")
        || task_id_or_key.starts_with("att_")
        || task_id_or_key.contains('/')
    {
        return Ok(task_id_or_key.to_string());
    }
    let run_id = resolve_run_id(paths, on, run_opt)?;
    let snap = proxy_call(paths, on, "GET", &format!("/api/runs/{run_id}"), "", None)?;
    if let Some(arr) = snap["tasks"].as_array()
        && let Some(t) = arr
            .iter()
            .find(|t| t["key"].as_str() == Some(task_id_or_key) || t["id"].as_str() == Some(task_id_or_key))
        && let Some(id) = t["id"].as_str()
    {
        return Ok(id.to_string());
    }
    Err(RpcError::invalid_params(format!("task {task_id_or_key:?} not found in run {run_id}")))
}

fn read_plan_input(file: Option<PathBuf>, use_stdin: bool) -> Result<Value, RpcError> {
    let raw = if let Some(p) = file {
        std::fs::read_to_string(&p)
            .map_err(|e| RpcError::invalid_params(format!("cannot read {}: {e}", p.display())))?
    } else if use_stdin || !std::io::stdin().is_terminal() {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| RpcError::invalid_params(format!("cannot read stdin: {e}")))?;
        s
    } else {
        return Err(RpcError::invalid_params("provide --file <PATH> or --stdin"));
    };
    parse_yaml_or_json(&raw).map_err(|e| RpcError::invalid_params(format!("invalid YAML/JSON plan: {e}")))
}

fn default_host_session(explicit: Option<String>) -> String {
    explicit
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("H2_AGENT_ID").ok().filter(|s| !s.trim().is_empty()))
        .or_else(|| std::env::var("H2_TERMINAL_ID").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "coordinator".into())
}

fn read_json_file(path: &PathBuf, consume: bool) -> Result<Value, RpcError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| RpcError::invalid_params(format!("cannot read {}: {e}", path.display())))?;
    if consume {
        let _ = std::fs::remove_file(path);
    }
    serde_json::from_str(&text).map_err(|e| RpcError::invalid_params(format!("invalid JSON in {}: {e}", path.display())))
}

/// Executes `h2 drove ...` or `drove ...` and returns `(json, human_summary)`.
pub fn execute(paths: &Paths, cli: DroveCli) -> Result<(Value, String), RpcError> {
    let action = cli.action.unwrap_or(DroveAction::Status { on: None });
    match action {
        DroveAction::Status { on } => {
            let mut p = json!({});
            if let Some(m) = &on {
                p["on"] = Value::String(m.clone());
            }
            let val = call_core(paths, "drove.settings.get", p)?;
            let runs = proxy_call(paths, on.as_deref(), "GET", "/api/runs", "local=1", None)
                .ok()
                .and_then(|v| v["runs"].as_array().map(|a| a.len()))
                .unwrap_or(0);
            let port = val["webPort"].as_u64().unwrap_or(0);
            let machine = val["machine"].as_str().unwrap_or("local");
            let ai_ok = val["aiStatus"]["configured"].as_bool().unwrap_or(false);
            let magpie_on = val["config"]["magpie"]["enabled"].as_bool().unwrap_or(false);
            let roles = val["config"]["roles"]
                .as_object()
                .map(|m| m.keys().cloned().collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            let human = format!(
                "drove on {machine}\nweb: http://127.0.0.1:{port}/\nroles: {roles}\nruns: {runs} | ai configured: {ai_ok} | magpie quota: {}",
                if magpie_on { "on" } else { "off" }
            );
            Ok((val, human))
        }
        DroveAction::Web { run, on, no_open } => {
            let mut p = json!({ "no_open": no_open });
            if let Some(r) = run {
                p["run_id"] = Value::String(r);
            }
            if let Some(m) = on {
                p["on"] = Value::String(m);
            }
            let res = call_core(paths, "drove.web.open", p)?;
            let url = res["url"].as_str().unwrap_or("http://127.0.0.1:7799/").to_string();
            Ok((res, url))
        }
        DroveAction::Settings { on } => {
            let mut p = json!({});
            if let Some(m) = on {
                p["on"] = Value::String(m);
            }
            let val = call_core(paths, "drove.settings.get", p)?;
            let human = serde_json::to_string_pretty(&val).unwrap_or_else(|_| val.to_string());
            Ok((val, human))
        }
        DroveAction::SettingsSet { file, on, consume } => {
            let mut p = read_json_file(&file, consume)?;
            if let Some(m) = on
                && let Some(obj) = p.as_object_mut()
            {
                obj.insert("on".into(), Value::String(m));
            }
            let val = call_core(paths, "drove.settings.set", p)?;
            Ok((val, "drove settings updated".into()))
        }
        DroveAction::Probe { on, refresh } => {
            let mut p = json!({ "refresh": refresh });
            if let Some(m) = on {
                p["on"] = Value::String(m);
            }
            let val = call_core(paths, "drove.probe", p)?;
            let mut lines = Vec::new();
            if let Some(arr) = val["agents"].as_array() {
                for a in arr {
                    let id = a["id"].as_str().unwrap_or("?");
                    let inst = a["installed"].as_bool().unwrap_or(false);
                    let models = a["models"].as_array().map(|m| m.len()).unwrap_or(0);
                    lines.push(format!("{id:<14} installed={inst:<5} models={models}"));
                }
            }
            Ok((val, lines.join("\n")))
        }
        DroveAction::AiTest { file, url, model, on, consume } => {
            let mut p = match file {
                Some(f) => read_json_file(&f, consume)?,
                None => json!({}),
            };
            if let Some(u) = url {
                p["baseUrl"] = Value::String(u);
            }
            if let Some(m) = model {
                p["model"] = Value::String(m);
            }
            if let Some(m) = on {
                p["on"] = Value::String(m);
            }
            let val = call_core(paths, "drove.ai.test", p)?;
            let ok = val["ok"].as_bool().unwrap_or(false);
            let human = if ok {
                format!(
                    "AI OK ({} ms) model={}",
                    val["latencyMs"].as_u64().unwrap_or(0),
                    val["discoveredModel"].as_str().unwrap_or("?")
                )
            } else {
                format!("AI FAILED: {}", val["error"].as_str().unwrap_or("unknown"))
            };
            Ok((val, human))
        }
        DroveAction::RolesAiConfig { file, mode, instruction, dry_run, language, on, consume } => {
            let mut p = match file {
                Some(f) => read_json_file(&f, consume)?,
                None => json!({}),
            };
            if let Some(md) = mode {
                p["mode"] = Value::String(md);
            }
            if let Some(inst) = instruction {
                p["instruction"] = Value::String(inst);
            }
            if dry_run {
                p["apply"] = Value::Bool(false);
            }
            if let Some(lang) = language {
                p["language"] = Value::String(lang);
            }
            if let Some(m) = on {
                p["on"] = Value::String(m);
            }
            let val = call_core(paths, "drove.roles.ai_configure", p)?;
            let ok = val["ok"].as_bool().unwrap_or(false);
            let human = if ok {
                format!(
                    "AI roles configured ({} ms, model={}): {}",
                    val["latencyMs"].as_u64().unwrap_or(0),
                    val["modelUsed"].as_str().unwrap_or("?"),
                    val["rationale"].as_str().unwrap_or("done")
                )
            } else {
                format!("AI roles config failed: {}", val["error"].as_str().unwrap_or("unknown"))
            };
            Ok((val, human))
        }
        DroveAction::MagpieTest { url, on } => {
            let mut p = json!({});
            if let Some(u) = url {
                p["gatewayUrl"] = Value::String(u);
            }
            if let Some(m) = on {
                p["on"] = Value::String(m);
            }
            let val = call_core(paths, "drove.magpie.test", p)?;
            let ok = val["ok"].as_bool().unwrap_or(false);
            let human = if ok {
                format!("Magpie OK at {} ({} cards)", val["gatewayUrl"].as_str().unwrap_or("?"), val["cardsCount"].as_u64().unwrap_or(0))
            } else {
                format!("Magpie FAILED at {}: {}", val["gatewayUrl"].as_str().unwrap_or("?"), val["error"].as_str().unwrap_or("unknown"))
            };
            Ok((val, human))
        }
        DroveAction::Use { run_id } => {
            save_current_run(paths, &run_id);
            Ok((json!({ "ok": true, "runId": run_id }), format!("using run {run_id}")))
        }
        DroveAction::Plan { file, stdin, objective, cwd, max_concurrency, verify, session, on } => {
            let parsed = read_plan_input(file, stdin)?;
            let cwd_resolved = cwd.unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| ".".into())
            });
            let obj = objective
                .or_else(|| parsed.get("objective").and_then(Value::as_str).map(String::from))
                .unwrap_or_else(|| format!("Drove run in {cwd_resolved}"));
            let mc = max_concurrency
                .or_else(|| parsed.get("maxConcurrency").and_then(Value::as_u64).map(|v| v as u32))
                .unwrap_or(4);
            let tasks_val = if parsed.is_array() {
                json!({ "tasks": parsed })
            } else if let Some(t) = parsed.get("tasks") {
                json!({ "tasks": t })
            } else {
                return Err(RpcError::invalid_params("plan YAML/JSON must contain a `tasks` array"));
            };

            let mut run_body = json!({
                "hostSession": default_host_session(session),
                "objective": obj,
                "cwd": cwd_resolved,
                "maxConcurrency": mc,
            });
            if let Some(v) = verify.map(Value::String).or_else(|| parsed.get("verify").cloned()) {
                run_body["verify"] = v;
            }
            if let Some(g) = parsed.get("goal") {
                run_body["goal"] = g.clone();
            }
            if let Some(b) = parsed.get("budget") {
                run_body["budget"] = b.clone();
            }

            let preflight = proxy_call(
                paths,
                on.as_deref(),
                "POST",
                "/api/preflight/worktree",
                "",
                Some(json!({
                    "cwd": cwd_resolved,
                    "tasks": tasks_val["tasks"],
                })),
            )?;
            let wt_warn = preflight["dirtyWarning"].as_str().map(String::from);

            let run = proxy_call(paths, on.as_deref(), "POST", "/api/runs", "", Some(run_body))?;
            let run_id = run["id"].as_str().unwrap_or("").to_string();
            if on.is_none() && !run_id.is_empty() {
                save_current_run(paths, &run_id);
            }
            let tasks = proxy_call(
                paths,
                on.as_deref(),
                "POST",
                &format!("/api/runs/{run_id}/tasks"),
                "",
                Some(tasks_val),
            )?;
            let n = tasks["tasks"].as_array().map(|a| a.len()).unwrap_or(0);
            let mut out = json!({ "run": run, "tasks": tasks["tasks"] });
            let mut human = format!("started run {run_id} with {n} task(s)");
            if let Some(w) = wt_warn.or_else(|| tasks["worktreeWarning"].as_str().map(String::from)) {
                out["worktreeWarning"] = Value::String(w.clone());
                human.push_str(&format!("\n{w}"));
            }
            Ok((out, human))
        }
        DroveAction::Add { run, file, stdin, key, title, change, role, deps, verify, worktree, on } => {
            let run_id = resolve_run_id(paths, on.as_deref(), run)?;
            let mut body = if file.is_some() || stdin {
                let parsed = read_plan_input(file, stdin)?;
                if parsed.is_array() {
                    json!({ "tasks": parsed })
                } else if parsed.get("tasks").is_some() {
                    parsed
                } else {
                    json!({ "tasks": [parsed] })
                }
            } else {
                let k = key.ok_or_else(|| RpcError::invalid_params("provide --file/--stdin or --key + --change"))?;
                let ch = change.unwrap_or_else(|| title.clone().unwrap_or_else(|| k.clone()));
                let mut t = json!({
                    "key": k,
                    "title": title.unwrap_or_else(|| k.clone()),
                    "spec": {
                        "target": "workspace",
                        "change": ch,
                        "acceptance": "Complete task cleanly",
                        "verify": verify,
                    },
                    "deps": deps,
                    "isolation": if worktree { "worktree" } else { "none" },
                });
                if let Some(r) = role {
                    t["assign"] = json!({ "role": r });
                }
                json!({ "tasks": [t] })
            };
            if let Some(obj) = body.as_object_mut()
                && !obj.contains_key("hostSession")
            {
                obj.insert("hostSession".into(), Value::String(default_host_session(None)));
            }
            let res = proxy_call(paths, on.as_deref(), "POST", &format!("/api/runs/{run_id}/tasks"), "", Some(body))?;
            let n = res["tasks"].as_array().map(|a| a.len()).unwrap_or(0);
            let mut human = format!("added {n} task(s) to {run_id}");
            if let Some(w) = res["worktreeWarning"].as_str() {
                human.push_str(&format!("\n{w}"));
            }
            Ok((res, human))
        }
        DroveAction::Quick { prompt, role, cwd, verify, worktree, wait, on } => {
            let cwd_resolved = cwd.unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| ".".into())
            });
            let task = json!({
                "tasks": [{
                    "key": "task",
                    "title": prompt.chars().take(60).collect::<String>(),
                    "spec": {
                        "target": "workspace",
                        "change": prompt,
                        "acceptance": "Complete task cleanly",
                        "verify": verify,
                    },
                    "assign": { "role": role },
                    "isolation": if worktree { "worktree" } else { "none" },
                }]
            });
            let preflight = proxy_call(
                paths,
                on.as_deref(),
                "POST",
                "/api/preflight/worktree",
                "",
                Some(json!({
                    "cwd": cwd_resolved,
                    "tasks": task["tasks"],
                })),
            )?;
            let wt_warn = preflight["dirtyWarning"].as_str().map(String::from);

            let mut run_body = json!({
                "hostSession": default_host_session(None),
                "objective": prompt,
                "cwd": cwd_resolved,
                "maxConcurrency": 2,
            });
            if let Some(v) = &verify {
                run_body["verify"] = Value::String(v.clone());
            }
            let run = proxy_call(paths, on.as_deref(), "POST", "/api/runs", "", Some(run_body))?;
            let run_id = run["id"].as_str().unwrap_or("").to_string();
            if on.is_none() && !run_id.is_empty() {
                save_current_run(paths, &run_id);
            }
            let added = proxy_call(paths, on.as_deref(), "POST", &format!("/api/runs/{run_id}/tasks"), "", Some(task))?;
            if wait {
                let waited = wait_loop(paths, on.as_deref(), &run_id, 600, 0)?;
                return Ok((waited.clone(), format!("run {run_id} settled={}", waited["settled"])));
            }
            let mut out = json!({ "run": run, "tasks": added["tasks"] });
            let mut human = format!("started quick run {run_id}");
            if let Some(w) = wt_warn.or_else(|| added["worktreeWarning"].as_str().map(String::from)) {
                out["worktreeWarning"] = Value::String(w.clone());
                human.push_str(&format!("\n{w}"));
            }
            Ok((out, human))
        }
        DroveAction::Wait { run, timeout, since_seq, on } => {
            let run_id = resolve_run_id(paths, on.as_deref(), run)?;
            let res = wait_loop(paths, on.as_deref(), &run_id, timeout, since_seq)?;
            let msgs = res["messages"].as_array().map(|a| a.len()).unwrap_or(0);
            Ok((res.clone(), format!("run {run_id}: settled={} messages={msgs}", res["settled"])))
        }
        DroveAction::Inbox { run, on } => {
            let run_id = resolve_run_id(paths, on.as_deref(), run)?;
            let res = proxy_call(paths, on.as_deref(), "GET", &format!("/api/runs/{run_id}/inbox"), "", None)?;
            let n = res["messages"].as_array().map(|a| a.len()).unwrap_or(0);
            Ok((res, format!("{n} pending message(s) in {run_id}")))
        }
        DroveAction::Show { id, on } => {
            let target = match id {
                Some(x) => x,
                None => resolve_run_id(paths, on.as_deref(), None)?,
            };
            let res = if target.starts_with("msg_") {
                proxy_call(paths, on.as_deref(), "GET", &format!("/api/messages/{target}"), "", None)?
            } else if target.starts_with("tsk_") {
                proxy_call(paths, on.as_deref(), "GET", &format!("/api/tasks/{target}/inspect"), "", None)?
            } else {
                proxy_call(paths, on.as_deref(), "GET", &format!("/api/runs/{target}"), "", None)?
            };
            let human = serde_json::to_string_pretty(&res).unwrap_or_else(|_| res.to_string());
            Ok((res, human))
        }
        DroveAction::Ack { message_id, on } => {
            let res = proxy_call(paths, on.as_deref(), "POST", &format!("/api/messages/{message_id}/ack"), "", Some(json!({})))?;
            Ok((res, format!("acknowledged {message_id}")))
        }
        DroveAction::Reply { message_id, body, on } => {
            let res = proxy_call(
                paths,
                on.as_deref(),
                "POST",
                &format!("/api/messages/{message_id}/reply"),
                "",
                Some(json!({ "body": body })),
            )?;
            Ok((res, format!("replied to {message_id}")))
        }
        DroveAction::Send { task, text, interrupt, run, on } => {
            let tid = resolve_task_id(paths, on.as_deref(), run, &task)?;
            let res = proxy_call(
                paths,
                on.as_deref(),
                "POST",
                &format!("/api/tasks/{tid}/send"),
                "",
                Some(json!({ "text": text, "interrupt": interrupt })),
            )?;
            Ok((res, format!("sent message to {tid}")))
        }
        DroveAction::Inspect { target, run, on } => {
            let tid = resolve_task_id(paths, on.as_deref(), run, &target)?;
            let res = proxy_call(paths, on.as_deref(), "GET", &format!("/api/tasks/{tid}/inspect"), "", None)?;
            let human = serde_json::to_string_pretty(&res).unwrap_or_else(|_| res.to_string());
            Ok((res, human))
        }
        DroveAction::Retry {
            task,
            hint,
            role,
            agent,
            model,
            effort,
            review_agent,
            review_model,
            no_review,
            fresh,
            run,
            on,
        } => {
            let tid = resolve_task_id(paths, on.as_deref(), run, &task)?;
            let res = proxy_call(
                paths,
                on.as_deref(),
                "POST",
                &format!("/api/tasks/{tid}/retry"),
                "",
                Some(json!({
                    "hint": hint,
                    "role": role,
                    "agent": agent,
                    "model": model,
                    "effort": effort,
                    "reviewAgent": review_agent,
                    "reviewModel": review_model,
                    "noReview": no_review,
                    "fresh": fresh,
                })),
            )?;
            Ok((res, format!("retried {tid}")))
        }
        DroveAction::Route { task, role, agent, model, effort, worker, run, on } => {
            let tid = resolve_task_id(paths, on.as_deref(), run, &task)?;
            let res = proxy_call(
                paths,
                on.as_deref(),
                "PUT",
                &format!("/api/tasks/{tid}/assign"),
                "",
                Some(json!({
                    "role": role,
                    "agent": agent,
                    "model": model,
                    "effort": effort,
                    "worker": worker,
                })),
            )?;
            Ok((res, format!("updated assignment on {tid}")))
        }
        DroveAction::Cancel { target, run, on } => {
            if let Some(tgt) = target {
                if tgt.starts_with("run_") {
                    let res = proxy_call(paths, on.as_deref(), "POST", &format!("/api/runs/{tgt}/cancel"), "", Some(json!({})))?;
                    return Ok((res, format!("cancelled run {tgt}")));
                }
                let tid = resolve_task_id(paths, on.as_deref(), run, &tgt)?;
                let res = proxy_call(paths, on.as_deref(), "POST", &format!("/api/tasks/{tid}/cancel"), "", Some(json!({})))?;
                return Ok((res, format!("cancelled task {tid}")));
            }
            let run_id = resolve_run_id(paths, on.as_deref(), run)?;
            let res = proxy_call(paths, on.as_deref(), "POST", &format!("/api/runs/{run_id}/cancel"), "", Some(json!({})))?;
            Ok((res, format!("cancelled run {run_id}")))
        }
        DroveAction::Complete { run, summary, force, on } => {
            let run_id = resolve_run_id(paths, on.as_deref(), run)?;
            let res = proxy_call(
                paths,
                on.as_deref(),
                "POST",
                &format!("/api/runs/{run_id}/complete"),
                "",
                Some(json!({ "summary": summary, "force": force })),
            )?;
            Ok((res, format!("completed run {run_id}")))
        }
        DroveAction::Pause { run, on } => {
            let run_id = resolve_run_id(paths, on.as_deref(), run)?;
            let res = proxy_call(paths, on.as_deref(), "POST", &format!("/api/runs/{run_id}/pause"), "", Some(json!({})))?;
            Ok((res, format!("paused {run_id}")))
        }
        DroveAction::Resume { run, on } => {
            let run_id = resolve_run_id(paths, on.as_deref(), run)?;
            let res = proxy_call(paths, on.as_deref(), "POST", &format!("/api/runs/{run_id}/resume"), "", Some(json!({})))?;
            Ok((res, format!("resumed {run_id}")))
        }
        DroveAction::Worker { action } => execute_worker_action(action),
        DroveAction::Install { target } => install_skills(target),
    }
}

fn wait_loop(
    paths: &Paths,
    on: Option<&str>,
    run_id: &str,
    timeout_sec: u64,
    since_seq: u64,
) -> Result<Value, RpcError> {
    let deadline = Instant::now() + Duration::from_secs(timeout_sec.max(1));
    loop {
        let remain = deadline.saturating_duration_since(Instant::now()).as_secs().clamp(1, 20);
        let q = format!("timeoutSec={remain}&sinceSeq={since_seq}");
        let res = proxy_call(paths, on, "GET", &format!("/api/runs/{run_id}/wait"), &q, None)?;
        if !res["timedOut"].as_bool().unwrap_or(false) || Instant::now() >= deadline {
            return Ok(res);
        }
    }
}

fn execute_worker_action(action: WorkerAction) -> Result<(Value, String), RpcError> {
    let url = std::env::var("H2_DROVE_URL")
        .map_err(|_| RpcError::invalid_params("H2_DROVE_URL is not set (must be run inside a Drove worker)"))?;
    let tok = std::env::var("H2_DROVE_WORKER_TOKEN")
        .or_else(|_| std::env::var("H2_DROVE_WORKER_TICKET"))
        .map_err(|_| RpcError::invalid_params("H2_DROVE_WORKER_TOKEN is not set"))?;
    let wid = std::env::var("H2_DROVE_WORKER_ID").unwrap_or_default();

    let resolve_attempt = || -> Result<String, RpcError> {
        if !wid.is_empty() {
            let ctx = worker_http_post(&url, &tok, "/api/worker/context", &json!({ "workerId": wid }))?;
            if let Some(a) = ctx["currentAttempt"].as_str()
                && !a.is_empty()
            {
                return Ok(a.to_string());
            }
        }
        Ok(std::env::var("H2_DROVE_ATTEMPT_ID").unwrap_or_default())
    };

    match action {
        WorkerAction::Context => {
            let val = worker_http_post(&url, &tok, "/api/worker/context", &json!({ "workerId": wid }))?;
            Ok((val.clone(), serde_json::to_string_pretty(&val).unwrap_or_default()))
        }
        WorkerAction::Inbox => {
            let att_id = resolve_attempt()?;
            let val = worker_http_post(&url, &tok, "/api/worker/inbox", &json!({ "attemptId": att_id }))?;
            Ok((val.clone(), serde_json::to_string_pretty(&val).unwrap_or_default()))
        }
        WorkerAction::Ask { question, options, recommend, why, timeout } => {
            let att_id = resolve_attempt()?;
            let mut body = json!({
                "attemptId": att_id,
                "question": question,
                "timeoutSec": timeout,
            });
            if !options.is_empty() {
                body["options"] = json!(options);
            }
            if let Some(r) = recommend {
                body["recommend"] = Value::String(r);
            }
            if let Some(w) = why {
                body["why"] = Value::String(w);
            }
            let val = worker_http_post(&url, &tok, "/api/worker/ask", &body)?;
            let reply_txt = val["reply"]["body"].as_str().unwrap_or("").to_string();
            Ok((val, reply_txt))
        }
        WorkerAction::Note { text, subject } => {
            let att_id = resolve_attempt()?;
            let val = worker_http_post(
                &url,
                &tok,
                "/api/worker/note",
                &json!({ "attemptId": att_id, "body": text, "subject": subject }),
            )?;
            Ok((val, "note recorded".into()))
        }
        WorkerAction::Read { target, offset, limit, grep } => {
            let att_id = resolve_attempt()?;
            let val = worker_http_post(
                &url,
                &tok,
                "/api/worker/read",
                &json!({
                    "attemptId": att_id,
                    "target": target,
                    "offset": offset,
                    "limit": limit,
                    "grep": grep,
                }),
            )?;
            Ok((val.clone(), serde_json::to_string_pretty(&val).unwrap_or_default()))
        }
        WorkerAction::Done { outcome, summary, reason, data, files, no_retry } => {
            let att_id = resolve_attempt()?;
            let parsed_data = match data {
                Some(raw) if !raw.trim().is_empty() => Some(
                    serde_json::from_str::<Value>(&raw)
                        .map_err(|e| RpcError::invalid_params(format!("--data is not valid JSON: {e}")))?,
                ),
                _ => None,
            };
            let mut body = json!({
                "attemptId": att_id,
                "outcome": outcome,
                "noRetry": no_retry,
            });
            if let Some(s) = summary {
                body["summary"] = Value::String(s);
            }
            if let Some(r) = reason {
                body["reason"] = Value::String(r);
            }
            if let Some(d) = parsed_data {
                body["data"] = d;
            }
            if !files.is_empty() {
                body["filesModified"] = json!(files);
            }
            let val = worker_http_post(&url, &tok, "/api/worker/done", &body)?;
            Ok((val, "reported worker result".into()))
        }
    }
}

fn worker_http_post(base_url: &str, token: &str, path: &str, body: &Value) -> Result<Value, RpcError> {
    let host_port = base_url
        .trim()
        .trim_end_matches('/')
        .strip_prefix("http://")
        .ok_or_else(|| RpcError::invalid_params(format!("invalid H2_DROVE_URL {base_url:?}")))?;
    let mut stream = TcpStream::connect(host_port)
        .map_err(|e| RpcError::new("unavailable", format!("cannot connect to Drove at {host_port}: {e}"), ""))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(600)));
    let payload = serde_json::to_vec(body).map_err(|e| RpcError::internal(e.to_string()))?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\
         x-drove-worker-token: {token}\r\n\r\n",
        payload.len()
    );
    stream.write_all(req.as_bytes()).map_err(|e| RpcError::internal(e.to_string()))?;
    stream.write_all(&payload).map_err(|e| RpcError::internal(e.to_string()))?;
    stream.flush().map_err(|e| RpcError::internal(e.to_string()))?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|e| RpcError::internal(e.to_string()))?;
    let text = String::from_utf8_lossy(&raw);
    let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(500);
    let parsed: Value = serde_json::from_str(rest).unwrap_or_else(|_| json!({ "raw": rest }));
    if status >= 400 {
        let code = parsed["error"].as_str().unwrap_or("worker_error");
        let msg = parsed["message"].as_str().unwrap_or(rest.trim());
        return Err(RpcError::new(code, msg, ""));
    }
    Ok(parsed)
}

const SKILL_DROVE: &str = include_str!("../../../../../skills/drove/SKILL.md");
const SKILL_DROVE_ADD: &str = include_str!("../../../../../skills/drove-add/SKILL.md");
const SKILL_DROVE_SETUP: &str = include_str!("../../../../../skills/drove-setup/SKILL.md");

fn install_skills(target: Option<PathBuf>) -> Result<(Value, String), RpcError> {
    let base = match target {
        Some(dir) => dir,
        None => {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .ok_or_else(|| RpcError::invalid_params("HOME/USERPROFILE is unset; pass --target <DIR>"))?;
            home.join(".gemini").join("skills")
        }
    };
    let mut written = Vec::new();
    for (name, content) in [
        ("drove", SKILL_DROVE),
        ("drove-add", SKILL_DROVE_ADD),
        ("drove-setup", SKILL_DROVE_SETUP),
    ] {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir)
            .map_err(|e| RpcError::internal(format!("mkdir {}: {e}", dir.display())))?;
        let path = dir.join("SKILL.md");
        std::fs::write(&path, content)
            .map_err(|e| RpcError::internal(format!("write {}: {e}", path.display())))?;
        written.push(path.display().to_string());
    }
    let out = json!({ "ok": true, "target": base.display().to_string(), "files": written });
    Ok((out, format!("installed Drove skills into {}", base.display())))
}

pub(crate) fn _silence_role_target(_t: Option<RoleTarget>) {}
