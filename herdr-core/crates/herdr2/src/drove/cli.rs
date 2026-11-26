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
    /// List Drove runs (read-only).
    #[command(visible_alias = "ls")]
    Runs {
        /// List trashed runs instead of active/completed runs.
        #[arg(long)]
        trash: bool,
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Show configured Drove roles, models, and routing policy (read-only).
    Roles {
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Read Drove configuration, resolved roles, and AI connection status.
    Settings {
        /// Return only a specific section (`roles`, `agents`, `ai`, `magpie`, `config`).
        #[arg(long, value_name = "SECTION")]
        section: Option<String>,
        /// Return a compact summary without full probed model lists.
        #[arg(long)]
        compact: bool,
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
    /// Use the configured AI assistant model to auto-configure Drove roles, models, candidate strengths, and routing notes.
    RolesAiConfig {
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
        /// Configuration scope: `models`, `strengths`, `notes`, or `all`.
        #[arg(long)]
        mode: Option<String>,
        /// Optional preference instruction for how roles should be assigned.
        #[arg(long)]
        instruction: Option<String>,
        /// Explicitly confirm applying the AI-generated role configuration to drove.json.
        #[arg(long)]
        apply: bool,
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
    Use { run_id: String },
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
        /// Run ID (positional or via `--run`).
        #[arg(value_name = "RUN")]
        run_pos: Option<String>,
        /// Run ID (equivalent to positional `<RUN>`).
        #[arg(long = "run", value_name = "RUN", conflicts_with = "run_pos")]
        run: Option<String>,
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        #[arg(long, default_value_t = 0)]
        since_seq: u64,
        /// Return as soon as any additional task reaches a terminal state (not only full Run settlement).
        #[arg(long, visible_alias = "on-change")]
        on_task_done: bool,
        /// Return full task specs instead of compact task summaries.
        #[arg(long)]
        full: bool,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// List unread coordinator inbox messages for a Run.
    Inbox {
        /// Run ID (positional or via `--run`).
        #[arg(value_name = "RUN")]
        run_pos: Option<String>,
        /// Run ID (equivalent to positional `<RUN>`).
        #[arg(long = "run", value_name = "RUN", conflicts_with = "run_pos")]
        run: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Inspect a Run, Task, or Message snapshot by ID.
    Show {
        /// Run ID, Task ID (`tsk_...`), or Message ID (`msg_...`).
        id: Option<String>,
        /// Run ID (equivalent to positional `<RUN_ID>`).
        #[arg(long = "run", value_name = "RUN", conflicts_with = "id")]
        run: Option<String>,
        /// Include full task spec (`change`, `acceptance`, `constraints`) instead of compact summaries.
        #[arg(long)]
        full: bool,
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
        /// Number of recent transcript items to return (1..=200, default 30).
        #[arg(long, default_value_t = 30)]
        tail: usize,
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
        /// Explicit user-approved override to bypass a tripped review hard circuit breaker (`awaiting_user_decision`).
        #[arg(long)]
        user_override: bool,
        /// Optional audit note explaining the user's decision when passing `--user-override`.
        #[arg(long)]
        override_reason: Option<String>,
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
        /// Run ID (positional or via `--run`).
        #[arg(value_name = "RUN")]
        run_pos: Option<String>,
        /// Run ID (equivalent to positional `<RUN>`).
        #[arg(long = "run", value_name = "RUN", conflicts_with = "run_pos")]
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
        /// Run ID (positional or via `--run`).
        #[arg(value_name = "RUN")]
        run_pos: Option<String>,
        /// Run ID (equivalent to positional `<RUN>`).
        #[arg(long = "run", value_name = "RUN", conflicts_with = "run_pos")]
        run: Option<String>,
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Resume a paused Run.
    Resume {
        /// Run ID (positional or via `--run`).
        #[arg(value_name = "RUN")]
        run_pos: Option<String>,
        /// Run ID (equivalent to positional `<RUN>`).
        #[arg(long = "run", value_name = "RUN", conflicts_with = "run_pos")]
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
    Inbox {
        /// Optional explicit attempt ID (defaults to worker's live active attempt).
        #[arg(long)]
        attempt: Option<String>,
    },
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
        /// Optional explicit attempt ID (defaults to worker's live active attempt).
        #[arg(long)]
        attempt: Option<String>,
    },
    /// Post a non-blocking progress note to the coordinator.
    Note {
        text: String,
        #[arg(long)]
        subject: Option<String>,
        /// Optional explicit attempt ID (defaults to worker's live active attempt).
        #[arg(long)]
        attempt: Option<String>,
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
        /// Optional explicit attempt ID (defaults to worker's live active attempt).
        #[arg(long)]
        attempt: Option<String>,
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
        /// Optional explicit attempt ID (defaults to worker's live active attempt).
        #[arg(long)]
        attempt: Option<String>,
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
                let _ = crate::cli::write_stdout_line(&val.to_string());
            } else {
                let _ = crate::cli::write_stdout_line(&human);
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
    match client::call(&stream, method, params.clone()) {
        Ok(val) => Ok(val),
        Err(err) if err.kind == "unknown_method" && crate::api::METHOD_NAMES.contains(&method) => {
            // If an older local background daemon is running without newer RPC methods, restart it once and retry.
            let _ = client::call(&stream, "server.restart", json!({}));
            std::thread::sleep(std::time::Duration::from_millis(150));
            let stream = client::connect_or_start(paths)?;
            client::call(&stream, method, params).map_err(|e| client::explain_closed(paths, e))
        }
        Err(err) => Err(client::explain_closed(paths, err)),
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResolveMode {
    Read,
    Write,
}

fn current_session_id() -> Option<String> {
    std::env::var("H2_AGENT_ID")
        .ok()
        .or_else(|| std::env::var("H2_TERMINAL_ID").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn sanitize_session_filename(session: &str) -> String {
    session
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn current_run_file(paths: &Paths) -> PathBuf {
    paths.state.join("drove").join("current_run")
}

fn session_run_file(paths: &Paths, session: &str) -> PathBuf {
    paths.state.join("drove").join("sessions").join(format!("{}.run", sanitize_session_filename(session)))
}

fn save_current_run(paths: &Paths, run_id: &str) -> Option<String> {
    if let Some(session) = current_session_id() {
        let p = session_run_file(paths, &session);
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, run_id);
        return Some(session);
    }
    let p = current_run_file(paths);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, run_id);
    None
}

fn normalize_dir_str(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches(['/', '\\']);
    std::fs::canonicalize(trimmed)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| trimmed.to_string())
}

fn run_matches_cwd(run: &Value, cwd_norm: &str) -> bool {
    run["cwd"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some_and(|rcwd| normalize_dir_str(rcwd) == cwd_norm)
}

fn run_matches_session(run: &Value, session_id: &str) -> bool {
    run["coordinator"]["hostSession"].as_str().map(str::trim) == Some(session_id)
}

fn format_ambiguous_runs(runs: &[&Value]) -> String {
    runs.iter()
        .take(6)
        .map(|r| {
            let id = r["id"].as_str().unwrap_or("?");
            let cwd = r["cwd"].as_str().unwrap_or("");
            let folder = cwd.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(cwd);
            if folder.is_empty() { id.to_string() } else { format!("{id} ({folder})") }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn select_run_from_list(
    runs: &[Value],
    session_id: Option<&str>,
    cwd: Option<&str>,
    saved_session_run: Option<&str>,
    saved_global_run: Option<&str>,
    mode: ResolveMode,
) -> Result<String, RpcError> {
    let run_exists = |id: &str| runs.iter().any(|r| r["id"].as_str() == Some(id));
    let active: Vec<&Value> = runs.iter().filter(|r| r["status"] == "active").collect();

    // 1. Session-scoped `drove use` / `drove plan` binding (isolated per $H2_AGENT_ID / $H2_TERMINAL_ID).
    if let Some(sid_run) = saved_session_run.map(str::trim).filter(|s| !s.is_empty())
        && (runs.is_empty() || run_exists(sid_run))
    {
        return Ok(sid_run.to_string());
    }

    // 2. Active runs owned by this coordinator session (`run.coordinator.hostSession`).
    let cwd_norm = cwd.map(str::trim).filter(|s| !s.is_empty()).map(normalize_dir_str);
    if let Some(sid) = session_id.map(str::trim).filter(|s| !s.is_empty()) {
        let owned: Vec<&Value> = active.iter().copied().filter(|r| run_matches_session(r, sid)).collect();
        if owned.len() == 1 {
            if let Some(id) = owned[0]["id"].as_str() {
                return Ok(id.to_string());
            }
        } else if owned.len() > 1 {
            if let Some(ref cn) = cwd_norm {
                let by_cwd: Vec<&Value> = owned.iter().copied().filter(|r| run_matches_cwd(r, cn)).collect();
                if by_cwd.len() == 1
                    && let Some(id) = by_cwd[0]["id"].as_str()
                {
                    return Ok(id.to_string());
                }
            }
            if mode == ResolveMode::Write {
                return Err(RpcError::new(
                    "ambiguous_run",
                    format!("multiple active runs belong to session {sid}: {}", format_ambiguous_runs(&owned)),
                    "pass --run <RUN_ID> or bind one with `h2 drove use <RUN_ID>`",
                ));
            }
            if let Some(id) = owned[0]["id"].as_str() {
                return Ok(id.to_string());
            }
        }
    }

    // 3. Active runs matching current working directory.
    if let Some(ref cn) = cwd_norm {
        let by_cwd: Vec<&Value> = active.iter().copied().filter(|r| run_matches_cwd(r, cn)).collect();
        if by_cwd.len() == 1 {
            if let Some(id) = by_cwd[0]["id"].as_str() {
                return Ok(id.to_string());
            }
        } else if by_cwd.len() > 1 {
            if mode == ResolveMode::Write {
                return Err(RpcError::new(
                    "ambiguous_run",
                    format!("multiple active runs exist in this directory: {}", format_ambiguous_runs(&by_cwd)),
                    "pass --run <RUN_ID> or bind one with `h2 drove use <RUN_ID>`",
                ));
            }
            if let Some(id) = by_cwd[0]["id"].as_str() {
                return Ok(id.to_string());
            }
        }
    }

    // 4. Global active runs: allow unambiguous single active run; refuse ambiguous writes.
    if active.len() == 1
        && let Some(id) = active[0]["id"].as_str()
    {
        return Ok(id.to_string());
    }
    if active.len() > 1 {
        if mode == ResolveMode::Write {
            return Err(RpcError::new(
                "ambiguous_run",
                format!("multiple active runs exist: {}", format_ambiguous_runs(&active)),
                "pass --run <RUN_ID> or bind one with `h2 drove use <RUN_ID>`",
            ));
        }
        if let Some(gid) = saved_global_run.map(str::trim).filter(|s| !s.is_empty())
            && active.iter().any(|r| r["id"].as_str() == Some(gid))
        {
            return Ok(gid.to_string());
        }
        if let Some(id) = active[0]["id"].as_str() {
            return Ok(id.to_string());
        }
    }

    // 5. Fallback when no run is currently active.
    if let Some(gid) = saved_global_run.map(str::trim).filter(|s| !s.is_empty())
        && (runs.is_empty() || run_exists(gid))
    {
        return Ok(gid.to_string());
    }
    if let Some(ref cn) = cwd_norm
        && let Some(matched) = runs.iter().find(|r| run_matches_cwd(r, cn))
        && let Some(id) = matched["id"].as_str()
    {
        return Ok(id.to_string());
    }
    if let Some(first) = runs.first()
        && let Some(id) = first["id"].as_str()
    {
        return Ok(id.to_string());
    }
    Err(RpcError::invalid_params("no run found; pass --run <RUN_ID> or create one with `drove plan`"))
}

fn resolve_run_id(
    paths: &Paths,
    on: Option<&str>,
    explicit: Option<String>,
    mode: ResolveMode,
) -> Result<String, RpcError> {
    if let Some(r) = explicit.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        return Ok(r);
    }
    let session_id = current_session_id();
    let saved_session_run = if on.is_none() {
        session_id.as_deref().and_then(|sid| std::fs::read_to_string(session_run_file(paths, sid)).ok())
    } else {
        None
    };
    let saved_global_run = if on.is_none() && session_id.is_none() {
        std::fs::read_to_string(current_run_file(paths)).ok()
    } else {
        None
    };
    let cwd = if on.is_none() { std::env::current_dir().ok().map(|p| p.display().to_string()) } else { None };
    let list = proxy_call(paths, on, "GET", "/api/runs", "local=1", None)?;
    let runs = list["runs"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    select_run_from_list(
        runs,
        session_id.as_deref(),
        cwd.as_deref(),
        saved_session_run.as_deref(),
        saved_global_run.as_deref(),
        mode,
    )
}

fn resolve_task_id(
    paths: &Paths,
    on: Option<&str>,
    run_opt: Option<String>,
    task_id_or_key: &str,
    mode: ResolveMode,
) -> Result<String, RpcError> {
    if task_id_or_key.starts_with("tsk_") || task_id_or_key.starts_with("att_") || task_id_or_key.contains('/') {
        return Ok(task_id_or_key.to_string());
    }
    let run_id = resolve_run_id(paths, on, run_opt, mode)?;
    let snap = proxy_call(paths, on, "GET", &format!("/api/runs/{run_id}"), "", None)?;
    if let Some(arr) = snap["tasks"].as_array()
        && let Some(t) =
            arr.iter().find(|t| t["key"].as_str() == Some(task_id_or_key) || t["id"].as_str() == Some(task_id_or_key))
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
    serde_json::from_str(&text)
        .map_err(|e| RpcError::invalid_params(format!("invalid JSON in {}: {e}", path.display())))
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
            let note_line = val["config"]["note"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| format!("\nnote: {}", s.chars().take(120).collect::<String>()))
                .unwrap_or_default();
            let human = format!(
                "drove on {machine}\nweb: http://127.0.0.1:{port}/\nroles: {roles}{note_line}\nruns: {runs} | ai configured: {ai_ok} | magpie quota: {}",
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
        DroveAction::Runs { trash, on } => {
            let query = if trash { "local=1&trash=1" } else { "local=1" };
            let val = proxy_call(paths, on.as_deref(), "GET", "/api/runs", query, None)?;
            let mut lines = Vec::new();
            if let Some(arr) = val["runs"].as_array() {
                for r in arr {
                    let id = r["id"].as_str().unwrap_or("?");
                    let status = r["status"].as_str().unwrap_or("?");
                    let obj = r["name"]
                        .as_str()
                        .or_else(|| r["objective"].as_str())
                        .unwrap_or("")
                        .chars()
                        .take(80)
                        .collect::<String>();
                    let tc = &r["taskCounts"];
                    let done = tc["succeeded"].as_u64().unwrap_or(0);
                    let failed = tc["failed"].as_u64().unwrap_or(0);
                    let running = tc["running"].as_u64().unwrap_or(0);
                    let total: u64 = tc
                        .as_object()
                        .map(|m| m.values().filter_map(Value::as_u64).sum())
                        .unwrap_or(0);
                    lines.push(format!("{id:<18} {status:<9} {done}/{total} done (run={running} fail={failed})  {obj}"));
                }
            }
            let human = if lines.is_empty() {
                if trash { "no trashed runs".into() } else { "no runs".into() }
            } else {
                lines.join("\n")
            };
            Ok((val, human))
        }
        DroveAction::Roles { on } => {
            let mut p = json!({});
            if let Some(m) = on {
                p["on"] = Value::String(m);
            }
            let val = call_core(paths, "drove.settings.get", p)?;
            let roles_obj = val["config"]["roles"].clone();
            let note = val["config"]["note"].clone();
            let mut lines = Vec::new();
            if let Some(map) = roles_obj.as_object() {
                for (name, cfg) in map {
                    let primary = &cfg["primary"];
                    let p_agent = primary["agent"].as_str().unwrap_or("?");
                    let p_model = primary["model"].as_str().unwrap_or("default");
                    let access = cfg["access"].as_str().unwrap_or("writable");
                    let fb_count = cfg["fallbacks"].as_array().map(|a| a.len()).unwrap_or(0);
                    lines.push(format!("{name:<14} {access:<9} primary={p_agent}/{p_model} fallbacks={fb_count}"));
                }
            }
            if let Some(n) = note.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                lines.push(format!("note: {n}"));
            }
            let out = json!({ "roles": roles_obj, "note": note });
            Ok((out, lines.join("\n")))
        }
        DroveAction::Settings { section, compact, on } => {
            let mut p = json!({});
            if let Some(m) = on {
                p["on"] = Value::String(m);
            }
            let val = call_core(paths, "drove.settings.get", p)?;
            format_settings_output(val, section.as_deref(), compact)
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
                    let err = a["error"].as_str().map(str::trim).filter(|s| !s.is_empty());
                    if let Some(e) = err {
                        let summary = e.lines().next().unwrap_or(e);
                        lines.push(format!("{id:<14} installed={inst:<5} models={models:<3} error={summary}"));
                    } else {
                        lines.push(format!("{id:<14} installed={inst:<5} models={models}"));
                    }
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
        DroveAction::RolesAiConfig { file, mode, instruction, apply, dry_run, language, on, consume } => {
            if file.is_none() && !apply && !dry_run {
                return Err(RpcError::new(
                    "invalid_params",
                    "roles-ai-config invokes the AI model to reconfigure drove roles; pass --dry-run to preview or --apply to write changes",
                    "to inspect current roles read-only, run `h2 drove roles` (or `h2 drove settings`)",
                ));
            }
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
            } else if apply {
                p["apply"] = Value::Bool(true);
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
                format!(
                    "Magpie OK at {} ({} cards)",
                    val["gatewayUrl"].as_str().unwrap_or("?"),
                    val["cardsCount"].as_u64().unwrap_or(0)
                )
            } else {
                format!(
                    "Magpie FAILED at {}: {}",
                    val["gatewayUrl"].as_str().unwrap_or("?"),
                    val["error"].as_str().unwrap_or("unknown")
                )
            };
            Ok((val, human))
        }
        DroveAction::Use { run_id } => {
            let bound_session = save_current_run(paths, &run_id);
            let scope = bound_session.as_deref().unwrap_or("global");
            let human = match &bound_session {
                Some(s) => format!("using run {run_id} (session {s})"),
                None => format!("using run {run_id}"),
            };
            Ok((json!({ "ok": true, "runId": run_id, "scope": scope }), human))
        }
        DroveAction::Plan { file, stdin, objective, cwd, max_concurrency, verify, session, on } => {
            let parsed = read_plan_input(file, stdin)?;
            let cwd_resolved = cwd.unwrap_or_else(|| {
                std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_else(|_| ".".into())
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
            let tasks =
                proxy_call(paths, on.as_deref(), "POST", &format!("/api/runs/{run_id}/tasks"), "", Some(tasks_val))?;
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
            let run_id = resolve_run_id(paths, on.as_deref(), run, ResolveMode::Write)?;
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
                std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_else(|_| ".".into())
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
                let waited = wait_loop(paths, on.as_deref(), &run_id, 600, 0, true, false)?;
                return Ok((waited.clone(), format_wait_human(&run_id, &waited)));
            }
            let mut out = json!({ "run": run, "tasks": added["tasks"] });
            let mut human = format!("started quick run {run_id}");
            if let Some(w) = wt_warn.or_else(|| added["worktreeWarning"].as_str().map(String::from)) {
                out["worktreeWarning"] = Value::String(w.clone());
                human.push_str(&format!("\n{w}"));
            }
            Ok((out, human))
        }
        DroveAction::Wait { run_pos, run, timeout, since_seq, on_task_done, full, on } => {
            let run_id = resolve_run_id(paths, on.as_deref(), run.or(run_pos), ResolveMode::Read)?;
            let res = wait_loop(paths, on.as_deref(), &run_id, timeout, since_seq, !full, on_task_done)?;
            let human = format_wait_human(&run_id, &res);
            Ok((res, human))
        }
        DroveAction::Inbox { run_pos, run, on } => {
            let run_id = resolve_run_id(paths, on.as_deref(), run.or(run_pos), ResolveMode::Read)?;
            let res = proxy_call(paths, on.as_deref(), "GET", &format!("/api/runs/{run_id}/inbox"), "", None)?;
            let n = res["messages"].as_array().map(|a| a.len()).unwrap_or(0);
            Ok((res, format!("{n} pending message(s) in {run_id}")))
        }
        DroveAction::Show { id, run, full, on } => {
            let target = match run.or(id) {
                Some(x) => x,
                None => resolve_run_id(paths, on.as_deref(), None, ResolveMode::Read)?,
            };
            let res = if target.starts_with("msg_") {
                proxy_call(paths, on.as_deref(), "GET", &format!("/api/messages/{target}"), "", None)?
            } else if target.starts_with("tsk_") {
                proxy_call(paths, on.as_deref(), "GET", &format!("/api/tasks/{target}/inspect"), "", None)?
            } else {
                let raw = proxy_call(paths, on.as_deref(), "GET", &format!("/api/runs/{target}"), "", None)?;
                if full { raw } else { compact_show_snapshot(raw) }
            };
            let human = serde_json::to_string_pretty(&res).unwrap_or_else(|_| res.to_string());
            Ok((res, human))
        }
        DroveAction::Ack { message_id, on } => {
            let res = proxy_call(
                paths,
                on.as_deref(),
                "POST",
                &format!("/api/messages/{message_id}/ack"),
                "",
                Some(json!({})),
            )?;
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
            let tid = resolve_task_id(paths, on.as_deref(), run, &task, ResolveMode::Write)?;
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
        DroveAction::Inspect { target, run, tail, on } => {
            let tid = resolve_task_id(paths, on.as_deref(), run, &target, ResolveMode::Read)?;
            let query = format!("tail={}", tail.clamp(1, 200));
            let res = proxy_call(paths, on.as_deref(), "GET", &format!("/api/tasks/{tid}/inspect"), &query, None)?;
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
            user_override,
            override_reason,
            run,
            on,
        } => {
            let tid = resolve_task_id(paths, on.as_deref(), run, &task, ResolveMode::Write)?;
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
                    "userOverride": user_override,
                    "overrideReason": override_reason,
                })),
            )?;
            let stage_note = match res["reviewCircuit"]["stage"].as_str() {
                Some("post_handoff_review") if user_override => {
                    let n = res["reviewCircuit"]["userOverrides"].as_u64().unwrap_or(1);
                    format!(" [user-override #{n} · single review pass]")
                }
                Some("post_handoff_review") => " [post-handoff final review 1/1]".into(),
                _ => String::new(),
            };
            Ok((res, format!("retried {tid}{stage_note}")))
        }
        DroveAction::Route { task, role, agent, model, effort, worker, run, on } => {
            let tid = resolve_task_id(paths, on.as_deref(), run, &task, ResolveMode::Write)?;
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
                    let res = proxy_call(
                        paths,
                        on.as_deref(),
                        "POST",
                        &format!("/api/runs/{tgt}/cancel"),
                        "",
                        Some(json!({})),
                    )?;
                    return Ok((res, format!("cancelled run {tgt}")));
                }
                let tid = resolve_task_id(paths, on.as_deref(), run, &tgt, ResolveMode::Write)?;
                let res =
                    proxy_call(paths, on.as_deref(), "POST", &format!("/api/tasks/{tid}/cancel"), "", Some(json!({})))?;
                return Ok((res, format!("cancelled task {tid}")));
            }
            let run_id = resolve_run_id(paths, on.as_deref(), run, ResolveMode::Write)?;
            let res =
                proxy_call(paths, on.as_deref(), "POST", &format!("/api/runs/{run_id}/cancel"), "", Some(json!({})))?;
            Ok((res, format!("cancelled run {run_id}")))
        }
        DroveAction::Complete { run_pos, run, summary, force, on } => {
            let run_id = resolve_run_id(paths, on.as_deref(), run.or(run_pos), ResolveMode::Write)?;
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
        DroveAction::Pause { run_pos, run, on } => {
            let run_id = resolve_run_id(paths, on.as_deref(), run.or(run_pos), ResolveMode::Write)?;
            let res =
                proxy_call(paths, on.as_deref(), "POST", &format!("/api/runs/{run_id}/pause"), "", Some(json!({})))?;
            Ok((res, format!("paused {run_id}")))
        }
        DroveAction::Resume { run_pos, run, on } => {
            let run_id = resolve_run_id(paths, on.as_deref(), run.or(run_pos), ResolveMode::Write)?;
            let res =
                proxy_call(paths, on.as_deref(), "POST", &format!("/api/runs/{run_id}/resume"), "", Some(json!({})))?;
            Ok((res, format!("resumed {run_id}")))
        }
        DroveAction::Worker { action } => execute_worker_action(action),
        DroveAction::Install { target } => install_skills(target),
    }
}

const WAIT_RECONNECT_BUDGET: Duration = Duration::from_secs(15);

fn format_settings_output(val: Value, section: Option<&str>, compact: bool) -> Result<(Value, String), RpcError> {
    let summarize_agent = |a: &Value| {
        json!({
            "id": a["id"],
            "installed": a["installed"],
            "defaultModel": a["defaultModel"],
            "modelCount": a["models"].as_array().map(|m| m.len()).unwrap_or(0),
            "effortCount": a["efforts"].as_array().map(|e| e.len()).unwrap_or(0),
        })
    };
    let out = if let Some(sec) = section.map(str::trim).filter(|s| !s.is_empty()) {
        match sec.to_ascii_lowercase().as_str() {
            "roles" => json!({
                "roles": val["config"]["roles"],
                "note": val["config"]["note"],
            }),
            "agents" => {
                let agents = if compact {
                    Value::Array(
                        val["probed"]["agents"]
                            .as_array()
                            .map(|arr| arr.iter().map(summarize_agent).collect())
                            .unwrap_or_default(),
                    )
                } else {
                    val["probed"]["agents"].clone()
                };
                json!({
                    "probedAt": val["probed"]["probedAt"],
                    "fullProbe": val["probed"]["fullProbe"],
                    "agents": agents,
                })
            }
            "ai" => json!({
                "ai": val["config"]["ai"],
                "aiStatus": val["aiStatus"],
            }),
            "magpie" => json!({
                "magpie": val["config"]["magpie"],
            }),
            "config" => val["config"].clone(),
            other => {
                return Err(RpcError::invalid_params(format!(
                    "unknown --section {other:?}; expected one of: roles, agents, ai, magpie, config"
                )));
            }
        }
    } else if compact {
        let mut roles_compact = serde_json::Map::new();
        if let Some(map) = val["config"]["roles"].as_object() {
            for (k, v) in map {
                roles_compact.insert(
                    k.clone(),
                    json!({
                        "access": v["access"],
                        "primary": v["primary"],
                        "fallbackCount": v["fallbacks"].as_array().map(|a| a.len()).unwrap_or(0),
                    }),
                );
            }
        }
        let installed_agents: Vec<Value> = val["probed"]["agents"]
            .as_array()
            .map(|arr| arr.iter().filter(|a| a["installed"] == true).map(summarize_agent).collect())
            .unwrap_or_default();
        json!({
            "machine": val["machine"],
            "webPort": val["webPort"],
            "maxConcurrencyDefault": val["config"]["maxConcurrencyDefault"],
            "note": val["config"]["note"],
            "roles": Value::Object(roles_compact),
            "installedAgents": installed_agents,
            "aiStatus": {
                "configured": val["aiStatus"]["configured"],
                "model": val["aiStatus"]["model"],
            },
            "magpieEnabled": val["config"]["magpie"]["enabled"],
        })
    } else {
        val
    };
    let human = serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string());
    Ok((out, human))
}

fn compact_show_snapshot(mut snap: Value) -> Value {
    let mut review_verdicts = Vec::new();
    let mut review_circuits = Vec::new();
    let mut any_failed = false;
    let mut any_circuit_attention = false;
    if let Some(tasks) = snap.get_mut("tasks").and_then(Value::as_array_mut) {
        for task in tasks.iter_mut() {
            if task["status"] == "failed" {
                any_failed = true;
            }
            if let Some(rc) = task.get("reviewCircuit").and_then(Value::as_object) {
                let stage = rc.get("stage").and_then(Value::as_str).unwrap_or("");
                if matches!(stage, "handoff_to_coordinator" | "post_handoff_review" | "awaiting_user_decision") {
                    if matches!(stage, "handoff_to_coordinator" | "awaiting_user_decision") {
                        any_circuit_attention = true;
                    }
                    review_circuits.push(json!({
                        "taskId": task["id"],
                        "taskKey": task["key"],
                        "stage": stage,
                        "reviewRounds": rc.get("reviewRounds").cloned().unwrap_or(json!(0)),
                        "maxNormalRounds": rc.get("maxNormalRounds").cloned().unwrap_or(json!(2)),
                        "handoffToCoordinator": rc.get("handoffToCoordinator").cloned().unwrap_or(json!(false)),
                        "postHandoffReviewUsed": rc.get("postHandoffReviewUsed").cloned().unwrap_or(json!(false)),
                        "awaitingUserDecision": rc.get("awaitingUserDecision").cloned().unwrap_or(json!(false)),
                    }));
                }
            }
            if let Some(data) = task.get("result").and_then(|r| r.get("data")).and_then(Value::as_object) {
                let raw_verdict = data
                    .get("review_status")
                    .or_else(|| data.get("reviewStatus"))
                    .or_else(|| data.get("verdict"))
                    .and_then(Value::as_str);
                if let Some(v) = raw_verdict {
                    let norm = v.trim().to_ascii_lowercase();
                    if matches!(norm.as_str(), "changes_requested" | "request_changes" | "rejected" | "needs_changes") {
                        review_verdicts.push(json!({
                            "taskId": task["id"],
                            "taskKey": task["key"],
                            "verdict": v,
                        }));
                    }
                }
            }
            if let Some(obj) = task.as_object_mut() {
                // Keep essential run/task status, assign, result, isolation, reviewCircuit fields while stripping bulky spec text.
                obj.remove("spec");
            }
        }
    }
    let pending_msgs = snap["messages"]
        .as_array()
        .is_some_and(|arr| arr.iter().any(|m| m["needsAction"] == true && m["handledAt"].is_null()));
    let needs_attention = pending_msgs || any_failed || !review_verdicts.is_empty() || any_circuit_attention;
    if let Some(obj) = snap.as_object_mut() {
        obj.insert("needsAttention".into(), Value::Bool(needs_attention));
        if !review_verdicts.is_empty() {
            obj.insert("reviewVerdicts".into(), Value::Array(review_verdicts));
        }
        if !review_circuits.is_empty() {
            obj.insert("reviewCircuits".into(), Value::Array(review_circuits));
        }
    }
    snap
}

fn format_wait_human(run_id: &str, res: &Value) -> String {
    let settled = res["settled"].as_bool().unwrap_or(false);
    let timed_out = res["timedOut"].as_bool().unwrap_or(false);
    let needs_attention = res["needsAttention"].as_bool().unwrap_or(false);
    let msgs = res["messages"].as_array().map(|a| a.len()).unwrap_or(0);
    let mut parts = vec![
        format!("settled={settled}"),
        format!("timedOut={timed_out}"),
        format!("needsAttention={needs_attention}"),
        format!("messages={msgs}"),
    ];
    if let Some(newly) = res["newlyCompleted"].as_array().filter(|a| !a.is_empty()) {
        let keys: Vec<&str> = newly.iter().filter_map(Value::as_str).collect();
        parts.push(format!("newlyCompleted=[{}]", keys.join(",")));
    }
    if let Some(verdicts) = res["reviewVerdicts"].as_array().filter(|a| !a.is_empty()) {
        let items: Vec<String> = verdicts
            .iter()
            .map(|v| format!("{}:{}", v["taskKey"].as_str().unwrap_or("?"), v["verdict"].as_str().unwrap_or("?")))
            .collect();
        parts.push(format!("reviewVerdicts=[{}]", items.join(",")));
    }
    if let Some(circuits) = res["reviewCircuits"].as_array().filter(|a| !a.is_empty()) {
        let items: Vec<String> = circuits
            .iter()
            .map(|c| {
                format!(
                    "{}:{}(r{}/{})",
                    c["taskKey"].as_str().unwrap_or("?"),
                    c["stage"].as_str().unwrap_or("?"),
                    c["reviewRounds"].as_u64().unwrap_or(0),
                    c["maxNormalRounds"].as_u64().unwrap_or(2),
                )
            })
            .collect();
        parts.push(format!("reviewCircuits=[{}]", items.join(",")));
    }
    format!("run {run_id}: {}", parts.join(" "))
}

fn wait_loop(
    paths: &Paths,
    on: Option<&str>,
    run_id: &str,
    timeout_sec: u64,
    since_seq: u64,
    compact: bool,
    on_task_done: bool,
) -> Result<Value, RpcError> {
    let known_done = if on_task_done {
        proxy_call(paths, on, "GET", &format!("/api/runs/{run_id}"), "", None)
            .ok()
            .and_then(|snap| {
                snap["tasks"].as_array().map(|arr| {
                    arr.iter()
                        .filter(|t| {
                            t["template"] != true
                                && matches!(
                                    t["status"].as_str(),
                                    Some("succeeded" | "failed" | "skipped" | "cancelled")
                                )
                        })
                        .filter_map(|t| t["id"].as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                })
            })
            .unwrap_or_default()
    } else {
        String::new()
    };

    let deadline = Instant::now() + Duration::from_secs(timeout_sec.max(1));
    let mut disconnected_since: Option<Instant> = None;
    let mut backoff_ms: u64 = 250;
    loop {
        let remain = deadline.saturating_duration_since(Instant::now()).as_secs().clamp(1, 20);
        let mut q = format!("timeoutSec={remain}&sinceSeq={since_seq}");
        if compact {
            q.push_str("&compact=1");
        }
        if on_task_done {
            q.push_str("&onTaskDone=1");
            if !known_done.is_empty() {
                q.push_str("&knownDone=");
                q.push_str(&known_done);
            }
        }
        match proxy_call(paths, on, "GET", &format!("/api/runs/{run_id}/wait"), &q, None) {
            Ok(res) => {
                disconnected_since = None;
                backoff_ms = 250;
                if !res["timedOut"].as_bool().unwrap_or(false) || Instant::now() >= deadline {
                    return Ok(res);
                }
            }
            Err(err) if err.kind == "unavailable" => {
                let now = Instant::now();
                let first_fail = *disconnected_since.get_or_insert(now);
                if now >= deadline || now.saturating_duration_since(first_fail) >= WAIT_RECONNECT_BUDGET {
                    return Err(err);
                }
                let sleep_dur = Duration::from_millis(backoff_ms)
                    .min(deadline.saturating_duration_since(now))
                    .min(WAIT_RECONNECT_BUDGET.saturating_sub(now.saturating_duration_since(first_fail)));
                if !sleep_dur.is_zero() {
                    std::thread::sleep(sleep_dur);
                }
                backoff_ms = (backoff_ms * 2).min(2000);
            }
            Err(err) => return Err(err),
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
    let env_att = std::env::var("H2_DROVE_ATTEMPT_ID").unwrap_or_default();

    // Returns `(attempt_id, is_explicit)`. When `--attempt` is omitted, dynamically resolve the
    // worker's active attempt via `/api/worker/context` (falling back to env attempt + workerId
    // which the daemon also resolves dynamically to the worker's active attempt).
    let resolve_attempt = |explicit: Option<String>| -> Result<(String, bool), RpcError> {
        if let Some(a) = explicit.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
            return Ok((a, true));
        }
        if !wid.is_empty()
            && let Ok(ctx) = worker_http_post(&url, &tok, "/api/worker/context", &json!({ "workerId": wid }))
            && let Some(a) = ctx["currentAttempt"].as_str()
            && !a.is_empty()
        {
            return Ok((a.to_string(), false));
        }
        Ok((env_att.clone(), false))
    };

    let attach_worker_meta = |body: &mut Value, att_id: String, explicit: bool| {
        if !wid.is_empty() {
            body["workerId"] = Value::String(wid.clone());
        }
        if !att_id.is_empty() {
            body["attemptId"] = Value::String(att_id);
        }
        if explicit {
            body["explicitAttempt"] = Value::Bool(true);
        }
    };

    match action {
        WorkerAction::Context => {
            let mut body = json!({});
            if !wid.is_empty() {
                body["workerId"] = Value::String(wid.clone());
            }
            if !env_att.is_empty() {
                body["attemptId"] = Value::String(env_att.clone());
            }
            let val = worker_http_post(&url, &tok, "/api/worker/context", &body)?;
            Ok((val.clone(), serde_json::to_string_pretty(&val).unwrap_or_default()))
        }
        WorkerAction::Inbox { attempt } => {
            let (att_id, explicit) = resolve_attempt(attempt)?;
            let mut body = json!({});
            attach_worker_meta(&mut body, att_id, explicit);
            let val = worker_http_post(&url, &tok, "/api/worker/inbox", &body)?;
            Ok((val.clone(), serde_json::to_string_pretty(&val).unwrap_or_default()))
        }
        WorkerAction::Ask { question, options, recommend, why, timeout, attempt } => {
            let (att_id, explicit) = resolve_attempt(attempt)?;
            let mut body = json!({
                "question": question,
                "timeoutSec": timeout,
            });
            attach_worker_meta(&mut body, att_id, explicit);
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
        WorkerAction::Note { text, subject, attempt } => {
            let (att_id, explicit) = resolve_attempt(attempt)?;
            let mut body = json!({ "body": text, "subject": subject });
            attach_worker_meta(&mut body, att_id, explicit);
            let val = worker_http_post(&url, &tok, "/api/worker/note", &body)?;
            Ok((val, "note recorded".into()))
        }
        WorkerAction::Read { target, offset, limit, grep, attempt } => {
            let (att_id, explicit) = resolve_attempt(attempt)?;
            let mut body = json!({
                "target": target,
                "offset": offset,
                "limit": limit,
                "grep": grep,
            });
            attach_worker_meta(&mut body, att_id, explicit);
            let val = worker_http_post(&url, &tok, "/api/worker/read", &body)?;
            Ok((val.clone(), serde_json::to_string_pretty(&val).unwrap_or_default()))
        }
        WorkerAction::Done { outcome, summary, reason, data, files, no_retry, attempt } => {
            let (att_id, explicit) = resolve_attempt(attempt)?;
            let parsed_data = match data {
                Some(raw) if !raw.trim().is_empty() => Some(
                    serde_json::from_str::<Value>(&raw)
                        .map_err(|e| RpcError::invalid_params(format!("--data is not valid JSON: {e}")))?,
                ),
                _ => None,
            };
            let mut body = json!({
                "outcome": outcome,
                "noRetry": no_retry,
            });
            attach_worker_meta(&mut body, att_id, explicit);
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
    for (name, content) in [("drove", SKILL_DROVE), ("drove-add", SKILL_DROVE_ADD), ("drove-setup", SKILL_DROVE_SETUP)]
    {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).map_err(|e| RpcError::internal(format!("mkdir {}: {e}", dir.display())))?;
        let path = dir.join("SKILL.md");
        std::fs::write(&path, content).map_err(|e| RpcError::internal(format!("write {}: {e}", path.display())))?;
        written.push(path.display().to_string());
    }
    let out = json!({ "ok": true, "target": base.display().to_string(), "files": written });
    Ok((out, format!("installed Drove skills into {}", base.display())))
}

pub(crate) fn _silence_role_target(_t: Option<RoleTarget>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_run_isolates_sessions_matches_cwd_and_refuses_ambiguous_writes() {
        let runs = vec![
            json!({
                "id": "run_a",
                "status": "active",
                "cwd": "/tmp/repo-a",
                "coordinator": { "hostSession": "t1" }
            }),
            json!({
                "id": "run_b",
                "status": "active",
                "cwd": "/tmp/repo-b",
                "coordinator": { "hostSession": "t2" }
            }),
        ];

        // 1. Session-scoped `drove use` takes precedence over global active list.
        assert_eq!(
            select_run_from_list(&runs, Some("t1"), Some("/tmp/repo-b"), Some("run_a"), None, ResolveMode::Write)
                .unwrap(),
            "run_a"
        );

        // 2. Host session ownership matches its own Run even when another terminal starts a newer Run.
        assert_eq!(
            select_run_from_list(&runs, Some("t2"), Some("/tmp/other"), None, None, ResolveMode::Write).unwrap(),
            "run_b"
        );

        // 3. CWD matching resolves unambiguously for an unbound terminal inside `/tmp/repo-b`.
        assert_eq!(
            select_run_from_list(&runs, Some("t3"), Some("/tmp/repo-b"), None, None, ResolveMode::Write).unwrap(),
            "run_b"
        );

        // 4. Unrelated session + unrelated cwd MUST refuse write operations when multiple runs are active.
        let err = select_run_from_list(&runs, Some("t9"), Some("/tmp/other"), None, None, ResolveMode::Write)
            .expect_err("ambiguous multi-run write must be refused");
        assert_eq!(err.kind, "ambiguous_run");
        assert!(err.message.contains("run_a") && err.message.contains("run_b"));

        // 5. Read mode falls back cleanly without failing.
        assert_eq!(
            select_run_from_list(&runs, Some("t9"), Some("/tmp/other"), None, Some("run_b"), ResolveMode::Read)
                .unwrap(),
            "run_b"
        );
    }

    #[test]
    fn cli_accepts_both_run_flag_and_positional_run_id_across_commands() {
        for args in [
            vec!["drove", "complete", "--run", "run_1", "--summary", "ok"],
            vec!["drove", "complete", "run_1", "--summary", "ok"],
            vec!["drove", "pause", "--run", "run_1"],
            vec!["drove", "pause", "run_1"],
            vec!["drove", "resume", "--run", "run_1"],
            vec!["drove", "resume", "run_1"],
            vec!["drove", "show", "--run", "run_1", "--full"],
            vec!["drove", "show", "run_1"],
            vec!["drove", "wait", "--run", "run_1", "--on-task-done", "--full"],
            vec!["drove", "wait", "run_1", "--on-change"],
            vec!["drove", "inbox", "--run", "run_1"],
            vec!["drove", "inbox", "run_1"],
            vec!["drove", "inspect", "task1", "--tail", "80"],
            vec!["drove", "retry", "final-review", "--user-override", "--override-reason", "user approved"],
            vec!["drove", "settings", "--section", "roles", "--compact"],
        ] {
            let parsed = StandaloneCli::try_parse_from(args.clone())
                .unwrap_or_else(|e| panic!("failed to parse {args:?}: {e}"));
            assert!(parsed.cmd.is_some());
        }
    }

    #[test]
    fn settings_section_and_compact_strip_large_model_lists() {
        let sample = json!({
            "machine": "local",
            "webPort": 7799,
            "config": {
                "maxConcurrencyDefault": 4,
                "note": "prefer codex",
                "roles": {
                    "coder": {
                        "access": "writable",
                        "primary": { "agent": "codex", "model": "gpt-5.4", "effort": "high" },
                        "fallbacks": [{ "agent": "agy" }]
                    }
                },
                "ai": { "enabled": true },
                "magpie": { "enabled": false }
            },
            "aiStatus": { "configured": true, "model": "gpt-4o-mini" },
            "probed": {
                "probedAt": "2026-11-24T00:00:00Z",
                "fullProbe": true,
                "agents": [
                    { "id": "codex", "installed": true, "defaultModel": "gpt-5.4", "models": ["a", "b", "c"], "efforts": ["high"] },
                    { "id": "gemini", "installed": false, "models": [], "efforts": [] }
                ]
            }
        });

        let (roles, _) = format_settings_output(sample.clone(), Some("roles"), false).unwrap();
        assert!(roles.get("roles").is_some());
        assert!(roles.get("probed").is_none());

        let (compact, _) = format_settings_output(sample, None, true).unwrap();
        assert_eq!(compact["installedAgents"].as_array().unwrap().len(), 1);
        assert_eq!(compact["installedAgents"][0]["modelCount"], 3);
        assert!(compact["installedAgents"][0].get("models").is_none());
    }

    #[test]
    fn compact_show_strips_spec_and_surfaces_review_verdicts() {
        let raw = json!({
            "run": { "id": "run_1", "status": "active" },
            "tasks": [
                {
                    "id": "tsk_rev",
                    "key": "review",
                    "status": "succeeded",
                    "spec": { "change": "very long spec text" },
                    "result": { "data": { "review_status": "changes_requested" } }
                }
            ],
            "messages": []
        });
        let compact = compact_show_snapshot(raw);
        assert!(compact["tasks"][0].get("spec").is_none());
        assert_eq!(compact["needsAttention"], true);
        assert_eq!(compact["reviewVerdicts"][0]["taskKey"], "review");
        assert_eq!(compact["reviewVerdicts"][0]["verdict"], "changes_requested");
    }

    #[test]
    fn compact_show_and_wait_human_surface_review_circuits() {
        let raw = json!({
            "run": { "id": "run_1", "status": "active" },
            "tasks": [
                {
                    "id": "tsk_rev",
                    "key": "final-review",
                    "status": "succeeded",
                    "spec": { "change": "long spec" },
                    "reviewCircuit": {
                        "stage": "awaiting_user_decision",
                        "reviewRounds": 3,
                        "maxNormalRounds": 2,
                        "handoffToCoordinator": true,
                        "postHandoffReviewUsed": true,
                        "awaitingUserDecision": true
                    }
                }
            ],
            "messages": []
        });
        let compact = compact_show_snapshot(raw);
        assert_eq!(compact["needsAttention"], true);
        assert_eq!(compact["reviewCircuits"][0]["taskKey"], "final-review");
        assert_eq!(compact["reviewCircuits"][0]["stage"], "awaiting_user_decision");
        assert_eq!(compact["reviewCircuits"][0]["reviewRounds"], 3);

        let wait_line = format_wait_human(
            "run_1",
            &json!({
                "settled": true,
                "timedOut": false,
                "needsAttention": true,
                "messages": [],
                "reviewCircuits": compact["reviewCircuits"]
            }),
        );
        assert!(
            wait_line.contains("reviewCircuits=[final-review:awaiting_user_decision(r3/2)]"),
            "unexpected wait line: {wait_line}"
        );
    }
}
