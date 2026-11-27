//! The core's live side: active ACP agent sessions, transcripts, persistence,
//! permission approvals, and lifecycle transitions.

use crate::acp::proto::{permission_cancelled, permission_selected};
use crate::acp::{PendingPermission, QueuedPrompt, SessionHandle};
use crate::agents::{Agent, Registry, address};
use crate::api::CoreInfo;
use crate::paths::{Paths, read_state};
use crate::permissions;
use crate::platform::{self, signal};
use crate::rpc::RpcError;
use crate::status::StatusReport;
use crate::transcript::{Entry, Transcript, append_disk, load as load_transcript, path as transcript_path};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{Notify, oneshot};

mod persistence;
mod pty;
use persistence::RegistryWriter;
pub use pty::Link;

/// Identifies either an ACP agent (`a*`) or a PTY terminal (`t*`) in shared
/// maps (waits, notes, messages, last sends, remote outboxes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TargetKey {
    Agent(u32),
    Terminal(u32),
}

impl TargetKey {
    pub fn parse(address: &str) -> Option<Self> {
        if let Some(id) = crate::agents::parse_id(address) {
            Some(Self::Agent(id))
        } else if let Some(id) = crate::terminals::parse_id(address) {
            Some(Self::Terminal(id))
        } else {
            None
        }
    }

    pub fn address(self) -> String {
        match self {
            Self::Agent(id) => crate::agents::address(id),
            Self::Terminal(id) => crate::terminals::address(id),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Exit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

pub struct Core {
    pub info: CoreInfo,
    pub login_session: Option<platform::LoginSession>,
    pub started: Instant,
    pub stop: Notify,
    /// Set by `server.restart`: on stop, cleanly end live ACP adapters and hand over PTY holders so kept entries recover on restart.
    pub restart: std::sync::atomic::AtomicBool,
    pub paths: Paths,
    pub state: Mutex<State>,
    /// Woken on every lifecycle change; waiters re-check what they need.
    pub changed: Notify,
    pub jobs: tokio::sync::mpsc::UnboundedSender<crate::notes::Job>,
    pub jobs_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<crate::notes::Job>>>,
    pub(crate) sweeps: Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) holder_start: Mutex<()>,
    dirty: std::sync::atomic::AtomicBool,
    pty_dirty: std::sync::atomic::AtomicBool,
    save_due: Notify,
    pub(crate) image_due: Notify,
    pub ai: Arc<crate::ai::AiRuntime>,
    pub drove: Arc<crate::drove::DroveRuntime>,
    pub adapters: Arc<crate::acp::adapters::AdapterManager>,
    registry_writer: RegistryWriter,
    pty_registry_writer: RegistryWriter,
    self_ref: std::sync::OnceLock<std::sync::Weak<Core>>,
}

#[derive(Default)]
pub struct State {
    pub reg: Registry,
    pub pty_reg: crate::terminals::Registry,
    pub docs: std::collections::BTreeMap<String, crate::api::DocGetResult>,
    pub sessions: HashMap<u32, SessionHandle>,
    pub inboxes: HashMap<u32, VecDeque<QueuedPrompt>>,
    pub pending_perms: HashMap<u32, PendingPermission>,
    pub transcripts: HashMap<u32, Transcript>,
    pub recovering: HashSet<u32>,
    pub events: crate::events::Events,
    /// Waits matching streaming output (`agent.wait --output`).
    pub output_watch: HashMap<u32, Vec<tokio::sync::mpsc::UnboundedSender<String>>>,
    pub screens: HashMap<u32, ghostty_vt::Terminal>,
    pub relays: HashMap<u32, crate::relay::Relay>,
    pub image_deadlines: HashMap<u32, (u64, tokio::time::Instant)>,
    pub viewers: HashMap<u32, Vec<crate::viewers::Viewer>>,
    pub gates: HashMap<u32, crate::pty_input::Gate>,
    pub queues: HashMap<u32, Arc<tokio::sync::Mutex<()>>>,
    pub sends: HashMap<u32, Arc<tokio::sync::Mutex<()>>>,
    pub foreground: HashMap<u32, Vec<oneshot::Sender<i32>>>,
    pub pty_output_watch: HashMap<u32, Vec<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>,
    pub last_output: HashMap<u32, Instant>,
    pub output_seq: HashMap<u32, u64>,
    pub stitching: HashSet<u32>,
    pub stitched: HashMap<u32, (u64, crate::stitch::Capture)>,
    pub holders: Vec<Link>,
    pub pending: HashMap<u32, oneshot::Sender<Result<crate::terminals::Terminal, String>>>,
    pub pty_killing: HashSet<u32>,
    pub relinked: HashMap<String, Instant>,
    pub disconnected: HashMap<String, PathBuf>,
    pub relayed: HashMap<(usize, u32), crate::remote::Relayed>,
    pub terminal_mirrors: HashMap<String, std::collections::BTreeMap<String, serde_json::Value>>,
    /// The turn of each caller's last send to an agent or terminal, until a bare wait reports it.
    pub last_send: HashMap<(String, TargetKey), u64>,
    /// Waits in progress by (caller, target): a parent waiting needs no note.
    pub waiting: HashMap<(String, TargetKey), usize>,
    /// A message arrived while this target was working and no wait of its returned since.
    pub messages: HashMap<TargetKey, Option<String>>,
    /// Parents with a note delivery running.
    pub note_tasks: HashSet<TargetKey>,
    pub killing: HashSet<u32>,
    pub stopping: bool,
    pub machines: Vec<crate::machines::Machine>,
    pub lines: HashMap<String, crate::remote::LineState>,
    pub line_tasks: HashMap<String, tokio::task::AbortHandle>,
    pub peers: std::collections::BTreeMap<String, crate::remote::Peer>,
    pub peer_lines: HashMap<String, (usize, Arc<crate::remote::Line>)>,
    pub afar_notes: HashMap<TargetKey, Vec<String>>,
    pub afar_runs: HashMap<(TargetKey, String), (usize, bool)>,
    pub machine_outbox: HashMap<String, Vec<serde_json::Value>>,
    pub mirrors: HashMap<String, std::collections::BTreeMap<String, serde_json::Value>>,
    pub pty_prompts: HashMap<u32, Vec<(u64, u64, String)>>,
    pub pty_respawning: std::collections::HashSet<u32>,
    pub ckpt_locks: HashMap<std::path::PathBuf, Arc<tokio::sync::Mutex<()>>>,
    pub checkpoints: HashMap<String, crate::checkpoint::TargetCheckpoints>,
    /// Per-agent throttle timestamp for background AI semantic digest refreshes.
    pub digest_last_ai: HashMap<u32, Instant>,
    /// Per-terminal throttle timestamp for background AI semantic digest refreshes.
    pub pty_digest_last_ai: HashMap<u32, Instant>,
    /// Last OSC title observed while a CLI agent was running in the terminal,
    /// used to clear stale CLI titles when exiting back to a shell without title hooks.
    pub pty_cli_titles: HashMap<u32, String>,
}

const SAVE_SOON: Duration = Duration::from_secs(2);

fn sanitize_ckpt_key(raw: &str) -> String {
    let s: String = raw
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(128)
        .collect();
    if s.is_empty() { "unknown".into() } else { s }
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

pub(crate) fn finish_reason(killed: bool, stopping: bool, restart: bool, host_shutdown: bool) -> &'static str {
    if killed {
        "killed"
    } else if restart {
        "stopped_by_restart"
    } else if stopping {
        "server_stop"
    } else if host_shutdown {
        "host_shutdown"
    } else {
        "exited"
    }
}

impl Core {
    pub fn start(paths: Paths, info: CoreInfo) -> io::Result<Arc<Core>> {
        let reg: Registry = read_state(&paths.state.join("agents.json"))?;
        let pty_reg: crate::terminals::Registry = read_state(&paths.state.join("terminals.json"))?;
        let docs_path = paths.state.join("docs.json");
        let docs = read_state(&docs_path).unwrap_or_else(|e| {
            eprintln!("core: cannot load documents, starting empty: {e}");
            if let Err(e) = std::fs::rename(&docs_path, paths.state.join("docs.json.bad")) {
                eprintln!("core: cannot quarantine documents: {e}");
            }
            Default::default()
        });
        crate::platform::private_dir(&paths.state.join("transcripts"))?;
        crate::platform::private_dir(&paths.state.join("checkpoints"))?;
        crate::platform::private_dir(&paths.state.join("hooks"))?;
        std::fs::create_dir_all(paths.state.join("screens"))?;
        let machines = read_state(&paths.state.join("machines.json"))?;
        let peers = read_state(&paths.state.join("peers.json"))?;
        let ai = crate::ai::AiRuntime::open(&paths)?;
        let drove = crate::drove::DroveRuntime::open(&paths, ai.clone())?;
        let (jobs, jobs_rx) = tokio::sync::mpsc::unbounded_channel();
        let core = Arc::new(Core {
            info,
            login_session: platform::login_session(),
            started: Instant::now(),
            stop: Notify::new(),
            restart: Default::default(),
            registry_writer: RegistryWriter::new(paths.state.join("agents.json"))?,
            pty_registry_writer: RegistryWriter::new(paths.state.join("terminals.json"))?,
            paths,
            state: Mutex::new(State { reg, pty_reg, docs, machines, peers, ..Default::default() }),
            changed: Notify::new(),
            jobs,
            jobs_rx: Mutex::new(Some(jobs_rx)),
            sweeps: Default::default(),
            holder_start: Default::default(),
            dirty: Default::default(),
            pty_dirty: Default::default(),
            save_due: Notify::new(),
            image_due: Notify::new(),
            ai,
            drove,
            adapters: Arc::new(crate::acp::adapters::AdapterManager::new()),
            self_ref: std::sync::OnceLock::new(),
        });
        let _ = core.self_ref.set(Arc::downgrade(&core));
        core.drove.attach_core(&core);
        crate::exec::sweep_leftovers(&core.paths.state);
        core.reconcile_startup();
        core.attach_all()?;
        Ok(core)
    }

    /// Reconciles agents left `"live"` by an earlier core process: ACP stdio pipes never
    /// cross process boundaries, so unkept ones finish and kept ones prepare for `recover_kept`.
    fn reconcile_startup(&self) {
        let mut st = self.lock();
        let stale_ids: Vec<u32> = st.reg.agents.values().filter(|a| a.is_live()).map(Agent::id).collect();
        let mut scopes = Vec::new();
        for id in stale_ids {
            if let Some(a) = st.reg.agents.get(&id)
                && let Some(scope) = &a.scope
            {
                scopes.push(scope.clone());
            }
            let keep = st.reg.agents.get(&id).is_some_and(|a| a.keep);
            if keep {
                if let Some(a) = st.reg.agents.get_mut(&id) {
                    a.pid = None;
                    a.pending_permission = None;
                    for item in a.inbox.drain(..) {
                        if !a.paused_inbox.iter().any(|existing| existing.id == item.id) {
                            a.paused_inbox.push(item);
                        }
                    }
                    a.inbox_len = 0;
                    if a.reason.is_none() {
                        a.reason = Some("stopped_by_restart".into());
                    }
                    let _ = crate::status::reset(a);
                }
                if let Ok(t) = load_transcript(&self.paths.state, &address(id)) {
                    st.transcripts.insert(id, t);
                }
            } else {
                self.record_finish(&mut st, id, Exit::default(), "stopped_by_restart");
            }
        }
        self.sweep_in_background(scopes);
        self.prune(&mut st);
        self.save(&st);
    }

    pub fn settings(&self) -> crate::config::Config {
        crate::config::load(&self.paths.config).unwrap_or_default()
    }

    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn save(&self, st: &State) {
        self.dirty.store(false, std::sync::atomic::Ordering::SeqCst);
        let bytes = serde_json::to_vec(&st.reg).expect("registry serializes");
        self.registry_writer.save(bytes);
    }

    pub async fn flush_saved(&self) -> Result<(), RpcError> {
        let timeout_err = |_| {
            RpcError::new(
                "unavailable",
                "registry persistence timed out; the operation may already have completed",
                "check the agent and disk before retrying",
            )
        };
        tokio::time::timeout(Duration::from_secs(10), self.registry_writer.flush())
            .await
            .map_err(timeout_err)?
            .map_err(|e| RpcError::internal(format!("cannot save agents: {e}")))?;
        tokio::time::timeout(Duration::from_secs(10), self.pty_registry_writer.flush())
            .await
            .map_err(timeout_err)?
            .map_err(|e| RpcError::internal(format!("cannot save terminals: {e}")))
    }

    pub fn save_soon(&self) {
        if !self.dirty.swap(true, std::sync::atomic::Ordering::SeqCst) {
            self.save_due.notify_one();
        }
    }

    pub async fn save_when_due(self: Arc<Self>) {
        loop {
            self.save_due.notified().await;
            tokio::time::sleep(SAVE_SOON).await;
            let st = self.lock();
            if self.dirty.load(std::sync::atomic::Ordering::SeqCst) {
                self.save(&st);
            }
            if self.pty_dirty.load(std::sync::atomic::Ordering::SeqCst) {
                self.save_pty(&st);
            }
        }
    }

    /// Appends one structured entry to the agent's transcript and notifies output watchers.
    pub fn record_transcript(&self, st: &mut State, id: u32, entry: Entry) {
        let addr = address(id);
        let _ = append_disk(&self.paths.state, &addr, &entry);
        let payload = serde_json::to_value(&entry).unwrap_or_default();
        let snippet = match &entry {
            Entry::Assistant { text, .. } => (!text.is_empty()).then(|| text.clone()),
            Entry::Error { message, .. } => Some(format!("[error] {message}\n")),
            Entry::ToolCall { title, status, .. } if !title.is_empty() => Some(format!("[tool: {status}] {title}\n")),
            Entry::Permission { title, outcome, .. } => Some(format!("[permission: {outcome}] {title}\n")),
            _ => None,
        };
        st.transcripts.entry(id).or_default().push(entry.clone());
        if let Some(wid) = st.reg.agents.get(&id).and_then(|a| a.labels.get("drove.worker")) {
            let _ =
                self.drove.pool_tx.send(crate::drove::pool::PoolSignal::Transcript { worker_id: wid.clone(), entry });
        }
        self.emit(st, "transcript", id, payload);
        if let Some(chunk) = snippet
            && let Some(watchers) = st.output_watch.get_mut(&id)
        {
            watchers.retain(|tx| tx.send(chunk.clone()).is_ok());
            if watchers.is_empty() {
                st.output_watch.remove(&id);
            }
        }
    }

    /// Synchronizes `Agent::{inbox_len, inbox}` with `st.inboxes[&id]` and emits a live `"status"` event if changed.
    pub fn sync_inbox(&self, st: &mut State, id: u32, emit: bool) {
        let items: Vec<crate::agents::QueuedInput> = st
            .inboxes
            .get(&id)
            .into_iter()
            .flatten()
            .map(|p| crate::agents::QueuedInput {
                id: p.id,
                text: p.text.clone(),
                from: p.from.clone(),
                attachments: p.attachments.clone(),
                queued_ms: p.queued_ms,
            })
            .collect();
        let payload = {
            let Some(a) = st.reg.agents.get_mut(&id) else {
                return;
            };
            let changed = a.inbox != items || a.inbox_len != items.len();
            a.inbox_len = items.len();
            a.inbox = items;
            (emit && changed && a.is_live()).then(|| {
                serde_json::json!({
                    "status": a.status,
                    "records": a.records,
                    "reported": a.reported,
                    "pending_permission": a.pending_permission,
                    "inbox_len": a.inbox_len,
                    "inbox": a.inbox,
                    "paused_inbox": a.paused_inbox,
                })
            })
        };
        if let Some(payload) = payload {
            self.emit(st, "status", id, payload);
        }
    }

    pub fn apply_status(&self, st: &mut State, id: u32, report: &StatusReport) {
        let word = |st: &State| st.reg.agents.get(&id).and_then(|a| a.status.as_ref().map(|s| s.state.clone()));
        let was = word(st);
        let Some(a) = st.reg.agents.get_mut(&id).filter(|a| a.is_live()) else {
            return;
        };
        let change = crate::status::apply(a, report);
        self.status_changed(st, id, change);
        if word(st) != was {
            self.save(st);
        } else {
            self.save_soon();
        }
        self.changed.notify_waiters();
    }

    pub fn status_changed(&self, st: &mut State, id: u32, change: crate::status::Change) {
        let a = st.reg.agents.get_mut(&id).expect("reported agent");
        let first_task_finished = a.first_task_turn.is_some_and(|turn| {
            a.turn > turn
                || (change.root_report
                    && a.status.as_ref().is_some_and(|s| matches!(s.state.as_str(), "done" | "blocked" | "error")))
        });
        if first_task_finished {
            a.first_task_turn = None;
        }
        let (
            status,
            records,
            turn,
            reported,
            pending_permission,
            inbox_len,
            inbox,
            working_since,
            work_ms,
            total_work_ms,
            status_ms,
        ) = (
            a.status.clone(),
            a.records.clone(),
            a.turn,
            a.reported,
            a.pending_permission.clone(),
            a.inbox_len,
            a.inbox.clone(),
            a.working_since,
            a.work_ms,
            a.total_work_ms,
            a.status_ms,
        );
        if change.status {
            self.emit(
                st,
                "status",
                id,
                serde_json::json!({
                    "status": status,
                    "records": records,
                    "reported": reported,
                    "pending_permission": pending_permission,
                    "inbox_len": inbox_len,
                    "inbox": inbox,
                    "working_since": working_since,
                    "work_ms": work_ms,
                    "total_work_ms": total_work_ms,
                    "status_ms": status_ms,
                }),
            );
        }
        if change.turn_ended {
            let state = status.map(|s| s.state);
            self.emit(st, "turn", id, serde_json::json!({"turn": turn, "state": state}));
        }
        if (change.turn_ended || change.stuck || first_task_finished) && st.reg.agents[&id].is_live() {
            self.news(st, id);
        }
    }

    /// Updates an ACP agent's adaptive `semantic_title` / `micro_summary` and emits a `"digest"` event if changed.
    pub fn apply_digest(
        &self,
        st: &mut State,
        id: u32,
        semantic_title: Option<String>,
        micro_summary: Option<String>,
        preserve_title: bool,
    ) {
        let payload = {
            let Some(a) = st.reg.agents.get_mut(&id).filter(|a| a.is_live()) else {
                return;
            };
            let next_title = if preserve_title && a.semantic_title.is_some() {
                a.semantic_title.clone()
            } else {
                semantic_title.or_else(|| a.semantic_title.clone())
            };
            let next_summary = micro_summary.or_else(|| a.micro_summary.clone());
            if a.semantic_title == next_title && a.micro_summary == next_summary {
                return;
            }
            a.semantic_title = next_title.clone();
            a.micro_summary = next_summary.clone();
            serde_json::json!({
                "semantic_title": next_title,
                "micro_summary": next_summary,
            })
        };
        self.emit(st, "digest", id, payload);
        self.save_soon();
    }

    /// Synchronously computes and applies a 0ms deterministic digest for an ACP agent.
    pub fn refresh_agent_digest_deterministic(&self, st: &mut State, id: u32, preserve_title: bool) {
        let Some(a) = st.reg.agents.get(&id).filter(|a| a.is_live()) else {
            return;
        };
        let status = a.status.as_ref().map(|s| s.state.as_str()).unwrap_or("idle");
        let turn = a.turn;
        let pending = a.pending_permission.clone();
        let existing_title = a.semantic_title.clone();
        let entries = st.transcripts.get(&id).map(|t| t.entries()).unwrap_or(&[]);
        let is_zh = crate::ai::digest::entries_prefer_zh(entries);
        let det = crate::ai::build_deterministic_acp_digest(
            status,
            turn,
            pending.as_ref(),
            entries,
            existing_title.as_deref(),
            is_zh,
        );
        self.apply_digest(st, id, det.semantic_title, det.micro_summary, preserve_title);
    }

    /// Spawns a throttled, lock-free background Core AI digest refinement for an ACP agent.
    pub fn spawn_agent_digest_ai(&self, id: u32, min_interval: Duration) {
        let Some(core) = self.self_ref.get().and_then(std::sync::Weak::upgrade) else {
            return;
        };
        let cfg = self.ai.config_snapshot();
        if !cfg.enabled || !cfg.semantic_summary || cfg.base_url.trim().is_empty() || self.ai.is_circuit_open() {
            return;
        }
        let snapshot = {
            let mut st = self.lock();
            let now = Instant::now();
            if st.digest_last_ai.get(&id).is_some_and(|last| now.saturating_duration_since(*last) < min_interval) {
                return;
            }
            let Some(a) = st.reg.agents.get(&id).filter(|a| a.is_live()).cloned() else {
                return;
            };
            st.digest_last_ai.insert(id, now);
            let status = a.status.as_ref().map(|s| s.state.clone()).unwrap_or_else(|| "idle".into());
            let program = a.agent.clone().unwrap_or_else(|| "acp".into());
            let existing = a.semantic_title.clone();
            let pending = a.pending_permission.clone();
            let turn = a.turn;
            let entries = st.transcripts.get(&id).map(|t| t.entries().to_vec()).unwrap_or_default();
            (a.address, program, status, turn, pending, existing, entries)
        };
        tokio::spawn(async move {
            let (addr, program, status, turn, pending, existing, entries) = snapshot;
            let is_zh = crate::ai::digest::entries_prefer_zh(&entries);
            let det = crate::ai::build_deterministic_acp_digest(
                &status,
                turn,
                pending.as_ref(),
                &entries,
                existing.as_deref(),
                is_zh,
            );
            let refined = crate::ai::refine_agent_digest(
                &core.ai,
                &addr,
                &program,
                &status,
                &det,
                existing.as_deref(),
                if is_zh { Some("zh") } else { None },
            )
            .await;
            if refined.used_ai {
                let mut st = core.lock();
                core.apply_digest(&mut st, id, refined.semantic_title, refined.micro_summary, false);
            }
        });
    }

    /// Spawns a background Core AI arbitration pass after an ACP turn ends to verify or refine
    /// whether the assistant's final response is `done`/`idle` vs `blocked` (question/choice).
    pub fn spawn_acp_status_ai(&self, id: u32, turn_num: u64, fallback_question: Option<String>) {
        let Some(core) = self.self_ref.get().and_then(std::sync::Weak::upgrade) else {
            return;
        };
        if !self.ai.status_arbitration_enabled() {
            return;
        }
        let snapshot = {
            let st = self.lock();
            let Some(a) = st.reg.agents.get(&id).filter(|a| a.is_live()) else {
                return;
            };
            if a.labels.contains_key("drove.worker")
                || a.pending_permission.is_some()
                || a.turn != turn_num
                || a.inbox_len > 0
                || st.inboxes.get(&id).is_some_and(|q| !q.is_empty())
                || a.status.as_ref().is_some_and(|s| matches!(s.state.as_str(), "working" | "error"))
            {
                return;
            }
            let entries = st.transcripts.get(&id).map(|t| t.entries()).unwrap_or(&[]);
            if crate::status::acp_turn_has_pending_question_tool(entries, turn_num) {
                return;
            }
            let assistant_text = crate::status::acp_turn_assistant_text(entries, turn_num);
            if assistant_text.trim().is_empty() {
                return;
            }
            let is_zh = crate::ai::digest::entries_prefer_zh(entries);
            let program = a.agent.clone().unwrap_or_else(|| "acp".into());
            (program, assistant_text, is_zh)
        };
        tokio::spawn(async move {
            let (program, assistant_text, is_zh) = snapshot;
            let verdict = crate::ai::classify_acp_turn_state(
                &core.ai,
                &program,
                &assistant_text,
                Some(if is_zh { "zh" } else { "en" }),
            )
            .await;
            let mut st = core.lock();
            let Some(a) = st.reg.agents.get(&id).filter(|a| a.is_live()) else {
                return;
            };
            if a.turn != turn_num
                || a.pending_permission.is_some()
                || a.inbox_len > 0
                || st.inboxes.get(&id).is_some_and(|q| !q.is_empty())
                || a.status.as_ref().is_some_and(|s| matches!(s.state.as_str(), "working" | "error"))
            {
                return;
            }
            let cur_state = a.status.as_ref().map(|s| s.state.as_str()).unwrap_or("idle");
            let cur_msg = a.status.as_ref().map(|s| s.msg.as_str()).unwrap_or("");
            if !verdict.used_ai {
                if let Some(msg) = fallback_question
                    && cur_state != "blocked"
                {
                    core.apply_status(
                        &mut st,
                        id,
                        &crate::status::StatusReport {
                            state: "blocked",
                            kind: Some("question"),
                            message: msg,
                            ..crate::status::StatusReport::root("blocked")
                        },
                    );
                }
                return;
            }
            if !verdict.accepted {
                if cur_state == "blocked" {
                    core.apply_status(&mut st, id, &crate::status::StatusReport::root("idle"));
                }
                return;
            }
            let Some(ai_state) = verdict.state.as_deref() else {
                return;
            };
            match ai_state {
                "blocked" => {
                    let kind = match verdict.kind.as_deref() {
                        Some("permission") => Some("permission"),
                        Some("auth") => Some("auth"),
                        _ => Some("question"),
                    };
                    let msg = verdict.summary.unwrap_or_default();
                    if cur_state != "blocked" || (!msg.is_empty() && cur_msg != msg) {
                        core.apply_status(
                            &mut st,
                            id,
                            &crate::status::StatusReport {
                                state: "blocked",
                                kind,
                                message: msg,
                                ..crate::status::StatusReport::root("blocked")
                            },
                        );
                    }
                }
                "done" | "idle" => {
                    if cur_state == "blocked" {
                        core.apply_status(&mut st, id, &crate::status::StatusReport::root("idle"));
                    }
                }
                "error" => {
                    let msg = verdict.summary.unwrap_or_default();
                    core.apply_status(
                        &mut st,
                        id,
                        &crate::status::StatusReport {
                            state: "error",
                            message: msg,
                            ..crate::status::StatusReport::root("error")
                        },
                    );
                }
                _ => {}
            }
        });
    }

    /// Updates a live PTY terminal's `semantic_title` / `micro_summary` and emits a `"digest"` event.
    pub fn apply_pty_digest(
        &self,
        st: &mut State,
        id: u32,
        semantic_title: Option<String>,
        micro_summary: Option<String>,
        preserve_title: bool,
    ) {
        let payload = {
            let Some(t) = st.pty_reg.terminals.get_mut(&id).filter(|t| t.is_live()) else {
                return;
            };
            let next_title = if preserve_title && t.semantic_title.is_some() {
                t.semantic_title.clone()
            } else {
                semantic_title.or_else(|| t.semantic_title.clone())
            };
            let next_summary = micro_summary.or_else(|| t.micro_summary.clone());
            if t.semantic_title == next_title && t.micro_summary == next_summary {
                return;
            }
            t.semantic_title = next_title.clone();
            t.micro_summary = next_summary.clone();
            serde_json::json!({
                "semantic_title": next_title,
                "micro_summary": next_summary,
            })
        };
        self.emit_pty(st, "digest", id, payload);
        self.save_pty_soon();
    }

    /// Spawns a throttled, lock-free background Core AI digest refinement for a live PTY terminal.
    /// Safe to invoke while holding `&mut State` (does not re-acquire `self.lock()` synchronously).
    pub fn spawn_pty_digest_ai_locked(
        &self,
        st: &mut State,
        id: u32,
        min_interval: Duration,
        language: Option<String>,
    ) {
        let Some(core) = self.self_ref.get().and_then(std::sync::Weak::upgrade) else {
            return;
        };
        let Ok(rt_handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let cfg = self.ai.config_snapshot();
        let ai_ready =
            cfg.enabled && cfg.semantic_summary && !cfg.base_url.trim().is_empty() && !self.ai.is_circuit_open();
        let now = Instant::now();
        if st.pty_digest_last_ai.get(&id).is_some_and(|last| now.saturating_duration_since(*last) < min_interval) {
            return;
        }
        let Some(t) = st.pty_reg.terminals.get(&id).filter(|t| t.is_live()).cloned() else {
            return;
        };
        if crate::foreground::is_at_shell_root(&t) {
            return;
        }
        // Skip bare shells that have never reported status, completed a turn, or received prompts.
        let has_prompts = st.pty_prompts.get(&id).is_some_and(|p| !p.is_empty());
        let has_cli_fg = t.foreground.as_ref().is_some_and(crate::foreground::is_cli_foreground);
        if t.turn == 0 && !t.reported && !has_prompts && !has_cli_fg {
            return;
        }
        st.pty_digest_last_ai.insert(id, now);
        let orig_started_ms = t.program_started_ms;
        let live_title = t.title.clone();
        let screen_fallback = self.screen_text(st, id);
        let mut snap = crate::pty_transcript::snapshot_from_terminal(&t, st.pty_prompts.get(&id));
        let addr = t.address.clone();
        crate::pty_transcript::populate_session_affinity(&mut snap, &addr, &core, st);
        let program = t
            .foreground
            .as_ref()
            .map(|f| f.name.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| t.argv.first().cloned())
            .unwrap_or_else(|| "terminal".into());
        let status = t.status.as_ref().map(|s| s.state.clone()).unwrap_or_else(|| "idle".into());
        let raw_turn = t.turn;
        let turn = raw_turn.max(1);
        let existing = t.semantic_title.clone();
        let has_sent_prompts = !snap.sent_prompts.is_empty();

        rt_handle.spawn(async move {
            let Ok((entries, _)) =
                tokio::task::spawn_blocking(move || crate::pty_transcript::extract_transcript(&snap)).await
            else {
                return;
            };
            let det = if !entries.is_empty() {
                let is_zh = language
                    .as_deref()
                    .map(|l| l.to_ascii_lowercase().starts_with("zh"))
                    .unwrap_or_else(|| crate::ai::digest::entries_prefer_zh(&entries));
                crate::ai::build_deterministic_acp_digest(&status, turn, None, &entries, existing.as_deref(), is_zh)
            } else if let Some(ref screen) = screen_fallback {
                crate::ai::digest::build_deterministic_screen_digest_with_title(
                    screen,
                    existing.as_deref(),
                    Some(&live_title),
                    Some(&program),
                )
            } else {
                crate::ai::digest::build_deterministic_screen_digest_with_title(
                    "",
                    existing.as_deref(),
                    Some(&live_title),
                    Some(&program),
                )
            };
            if entries.is_empty()
                && raw_turn == 0
                && !has_sent_prompts
                && status == "idle"
                && det.semantic_title.is_none()
            {
                return;
            }
            if det.task_hint.is_empty() && det.recent_steps.is_empty() && det.semantic_title.is_none() {
                return;
            }
            let refined = if ai_ready {
                crate::ai::refine_agent_digest(
                    &core.ai,
                    &addr,
                    &program,
                    &status,
                    &det,
                    existing.as_deref(),
                    language.as_deref(),
                )
                .await
            } else {
                crate::ai::AgentDigestResult {
                    address: addr,
                    semantic_title: det.semantic_title,
                    micro_summary: det.micro_summary,
                    used_ai: false,
                    from_cache: false,
                }
            };
            if refined.semantic_title.is_some() || refined.micro_summary.is_some() {
                let mut st = core.lock();
                let still_same_cli = st.pty_reg.terminals.get(&id).is_some_and(|cur| {
                    cur.is_live()
                        && !crate::foreground::is_at_shell_root(cur)
                        && cur.program_started_ms == orig_started_ms
                });
                if !still_same_cli {
                    return;
                }
                core.apply_pty_digest(&mut st, id, refined.semantic_title, refined.micro_summary, false);
            }
        });
    }

    /// Resolves a blocked permission prompt for agent `id`.
    pub fn resolve_permission(
        &self,
        id: u32,
        approve: bool,
        explicit_option: Option<&str>,
    ) -> Result<(String, Option<String>), RpcError> {
        let mut st = self.lock();
        let a = st
            .reg
            .agents
            .get(&id)
            .ok_or_else(|| RpcError::new("not_found", format!("no agent {}", address(id)), ""))?;
        if !a.is_live() {
            return Err(RpcError::new("finished", format!("{} already exited", a.address), ""));
        }
        let Some(pending) = st.pending_perms.remove(&id) else {
            return Err(RpcError::new(
                "invalid_state",
                format!("{} has no pending permission request", address(id)),
                "wait until status is blocked before approving or denying",
            ));
        };

        let chosen = if approve {
            permissions::pick_approve(&pending.options, explicit_option)
        } else {
            permissions::pick_deny(&pending.options, explicit_option)
        };

        if explicit_option.is_some() && chosen.is_none() {
            st.pending_perms.insert(id, pending);
            return Err(RpcError::invalid_params("unknown permission option id"));
        }
        if approve && chosen.is_none() {
            st.pending_perms.insert(id, pending);
            return Err(RpcError::invalid_params("permission request offers no approval option"));
        }

        let payload = match &chosen {
            Some(opt_id) => permission_selected(opt_id),
            None => permission_cancelled(),
        };
        pending.transport.reply_result(pending.rpc_id, payload);

        let outcome = if approve {
            format!("approved ({})", chosen.as_deref().unwrap_or("ok"))
        } else if let Some(opt_id) = &chosen {
            format!("denied ({opt_id})")
        } else {
            "denied (cancelled)".into()
        };
        if let Some(a) = st.reg.agents.get_mut(&id) {
            a.pending_permission = None;
        }
        self.record_transcript(
            &mut st,
            id,
            Entry::Permission { turn: pending.turn, title: pending.title, outcome, ts_ms: now_ms() },
        );
        self.apply_status(&mut st, id, &StatusReport::root("working"));
        self.save(&st);
        Ok((pending.request_id, chosen))
    }

    pub fn read_transcript(
        &self,
        id: u32,
        lines: Option<usize>,
        last: bool,
    ) -> Result<(String, bool, &'static str), RpcError> {
        let st = self.lock();
        if let Some(t) = st.transcripts.get(&id) {
            return Ok(t.render(lines, last));
        }
        drop(st);
        let t = load_transcript(&self.paths.state, &address(id))
            .map_err(|e| RpcError::internal(format!("cannot read transcript of {}: {e}", address(id))))?;
        let rendered = t.render(lines, last);
        let mut st = self.lock();
        if st.reg.agents.contains_key(&id) {
            st.transcripts.entry(id).or_insert(t);
        }
        Ok(rendered)
    }

    pub fn read_transcript_entries(&self, id: u32) -> Result<Vec<Entry>, RpcError> {
        let st = self.lock();
        if let Some(t) = st.transcripts.get(&id) {
            return Ok(t.entries().to_vec());
        }
        drop(st);
        let t = load_transcript(&self.paths.state, &address(id))
            .map_err(|e| RpcError::internal(format!("cannot read transcript of {}: {e}", address(id))))?;
        let entries = t.entries().to_vec();
        let mut st = self.lock();
        if st.reg.agents.contains_key(&id) {
            st.transcripts.entry(id).or_insert(t);
        }
        Ok(entries)
    }

    pub fn finish(&self, st: &mut State, id: u32, exit: Exit) {
        let reason = finish_reason(
            st.killing.contains(&id),
            st.stopping,
            self.restart.load(std::sync::atomic::Ordering::SeqCst),
            platform::host_shutdown_in_progress(),
        );
        self.record_finish(st, id, exit, reason);
        self.save(st);
        self.changed.notify_waiters();
    }

    pub(crate) fn record_finish(&self, st: &mut State, id: u32, exit: Exit, reason: &str) {
        st.sessions.remove(&id);
        st.inboxes.remove(&id);
        st.pending_perms.remove(&id);
        if !st.reg.agents.get(&id).is_some_and(|a| a.is_live()) {
            return;
        }
        let reason = if st.killing.remove(&id) {
            "killed"
        } else if reason == "exited" && st.reg.agents[&id].keep && exit.code.is_none() && exit.signal.is_some() {
            "signaled"
        } else {
            reason
        };
        if let Some(a) = st.reg.agents.get_mut(&id) {
            a.first_task_turn = None;
            a.pending_permission = None;
            let change = crate::status::reset(a);
            self.status_changed(st, id, change);
        }
        st.reg.finish(id, exit.code, exit.signal, reason, now_ms());
        let finished_ms = st.reg.agents.get(&id).and_then(|a| a.finished_ms);
        let end = serde_json::json!({"reason": reason, "exit_code": exit.code, "signal": exit.signal, "finished_ms": finished_ms});
        let pending = st.reg.agents[&id].is_live();
        self.emit(st, if pending { "lost" } else { "finished" }, id, end);
        if pending {
            let a = st.reg.agents.get_mut(&id).expect("lost");
            a.news += 1;
            a.told_parent = false;
        } else {
            self.news(st, id);
            let target = TargetKey::Agent(id);
            let _ = self.jobs.send(crate::notes::Job::Orphaned(target));
            crate::remote::orphaned_afar(self, st, target);
            let _ = self
                .drove
                .pool_tx
                .send(crate::drove::pool::PoolSignal::SessionClosed { address: crate::agents::address(id) });
        }
        if let Some(wid) = st.reg.agents.get(&id).and_then(|a| a.labels.get("drove.worker")) {
            let _ = self.drove.pool_tx.send(crate::drove::pool::PoolSignal::AgentExited {
                worker_id: wid.clone(),
                agent_id: id,
                reason: reason.to_string(),
            });
        }
        let rm = st.reg.agents.get(&id).is_some_and(|a| {
            !a.is_live()
                && a.rm
                && !a.parent.as_ref().is_some_and(|p| st.waiting.contains_key(&(p.clone(), TargetKey::Agent(id))))
        });
        if rm {
            self.remove(st, id);
        }
        self.prune(st);
    }

    pub fn prune(&self, st: &mut State) {
        for gone in st.reg.expired(now_ms()) {
            self.remove(st, gone);
        }
        self.prune_pty(st);
    }

    pub fn remove(&self, st: &mut State, id: u32) {
        let Some(was) = st.reg.agents.remove(&id) else { return };
        st.transcripts.remove(&id);
        st.sessions.remove(&id);
        st.inboxes.remove(&id);
        st.pending_perms.remove(&id);
        self.emit_from(st, "removed", id, serde_json::json!({}), Some(&was));
        let _ = std::fs::remove_file(transcript_path(&self.paths.state, &was.address));
        let _ = std::fs::remove_dir_all(crate::put::dir(&self.paths.state, id));
        self.cleanup_target_checkpoints(st, &was.address, &was.cwd);
        self.save(st);
    }

    pub fn arc(&self) -> Option<Arc<Self>> {
        self.self_ref.get().and_then(std::sync::Weak::upgrade)
    }

    pub fn session_slug(&self) -> String {
        let raw = self.paths.state.file_name().and_then(|s| s.to_str()).unwrap_or("default");
        crate::checkpoint::slug_segment(raw)
    }

    pub fn checkpoints_dir(&self) -> std::path::PathBuf {
        self.paths.state.join("checkpoints")
    }

    pub fn checkpoint_path(&self, addr: &str) -> std::path::PathBuf {
        self.checkpoints_dir().join(format!("{addr}.json"))
    }

    pub fn hooks_dir(&self) -> std::path::PathBuf {
        self.paths.state.join("hooks")
    }

    pub fn repo_ckpt_lock(&self, repo_root: &std::path::Path) -> Arc<tokio::sync::Mutex<()>> {
        let key = crate::checkpoint::canonicalize_path(repo_root);
        let mut st = self.lock();
        Arc::clone(st.ckpt_locks.entry(key).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))))
    }

    pub fn target_checkpoints(&self, st: &mut State, addr: &str) -> crate::checkpoint::TargetCheckpoints {
        if let Some(tc) = st.checkpoints.get(addr) {
            return tc.clone();
        }
        let path = self.checkpoint_path(addr);
        let mut tc: crate::checkpoint::TargetCheckpoints = crate::paths::read_state(&path).unwrap_or_default();
        if tc.addr.is_empty() {
            tc.addr = addr.to_string();
        }
        st.checkpoints.insert(addr.to_string(), tc.clone());
        tc
    }

    fn session_checkpoints_dir(&self, cli_kind: &str) -> std::path::PathBuf {
        self.checkpoints_dir().join("sessions").join(sanitize_ckpt_key(cli_kind))
    }

    fn session_checkpoint_path(&self, cli_kind: &str, session_id: &str) -> std::path::PathBuf {
        self.session_checkpoints_dir(cli_kind).join(format!("{}.json", sanitize_ckpt_key(session_id)))
    }

    /// Mirrors a CLI session's checkpoints into `<state>/checkpoints/sessions/<cli_kind>/<sid>.json`
    /// so later `claude -c` / `claude --resume <id>` / `codex resume` can automatically re-attach them.
    pub fn archive_cli_session_checkpoints(&self, tc: &crate::checkpoint::TargetCheckpoints) -> bool {
        if tc.checkpoints.is_empty() {
            return false;
        }
        let Some(kind) = tc.cli_kind.as_deref().map(str::trim).filter(|k| !k.is_empty() && *k != "generic") else {
            return false;
        };
        let sid =
            tc.cli_session_id.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from).or_else(|| {
                tc.cli_transcript_path
                    .as_deref()
                    .and_then(|p| crate::pty_adapters::session_id_from_transcript(kind, std::path::Path::new(p)))
            });
        let Some(sid) = sid else {
            return false;
        };
        let mut archived = tc.clone();
        archived.cli_session_id = Some(sid.clone());
        archived.cli_fg_pid = None;
        archived.pending_handoff = None;
        archived.pending_rollback_turn = None;
        archived.active_turn_num = None;
        archived.active_turn_prompt = None;
        let dir = self.session_checkpoints_dir(kind);
        let _ = crate::platform::private_dir(&dir);
        let path = self.session_checkpoint_path(kind, &sid);
        if let Ok(bytes) = serde_json::to_vec_pretty(&archived) {
            let _ = crate::paths::write_atomic(&path, &bytes);
            return true;
        }
        false
    }

    /// Loads archived checkpoints for `(cli_kind, session_id)` (or most recent archive for `cwd_hint`),
    /// also scanning legacy per-terminal files for sessions created prior to session archiving.
    pub fn load_archived_cli_session(
        &self,
        cli_kind: &str,
        session_id: Option<&str>,
        cwd_hint: Option<&str>,
    ) -> Option<crate::checkpoint::TargetCheckpoints> {
        let kind = cli_kind.trim();
        if kind.is_empty() || kind == "generic" {
            return None;
        }
        if let Some(sid) = session_id.map(str::trim).filter(|s| !s.is_empty()) {
            let direct = self.session_checkpoint_path(kind, sid);
            if let Ok(tc) = crate::paths::read_state::<crate::checkpoint::TargetCheckpoints>(&direct)
                && !tc.checkpoints.is_empty()
            {
                return Some(tc);
            }
            // Legacy fallback: scan `<state>/checkpoints/*.json` for matching session_id or transcript stem.
            if let Ok(entries) = std::fs::read_dir(self.checkpoints_dir()) {
                let mut best: Option<(u64, crate::checkpoint::TargetCheckpoints)> = None;
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    let Ok(tc) = crate::paths::read_state::<crate::checkpoint::TargetCheckpoints>(&path) else {
                        continue;
                    };
                    if tc.checkpoints.is_empty() {
                        continue;
                    }
                    let matches_sid = tc.cli_session_id.as_deref() == Some(sid)
                        || tc.cli_transcript_path.as_deref().is_some_and(|p| {
                            crate::pty_adapters::session_id_from_transcript(kind, std::path::Path::new(p)).as_deref()
                                == Some(sid)
                        });
                    if matches_sid {
                        let ts = tc.checkpoints.iter().map(|c| c.ts_ms).max().unwrap_or(0);
                        if best.as_ref().is_none_or(|(b_ts, _)| ts > *b_ts) {
                            best = Some((ts, tc));
                        }
                    }
                }
                if let Some((_, tc)) = best {
                    return Some(tc);
                }
            }
            // A concrete session_id was resolved for the resumed CLI, and no archive exists for it.
            // Do not silently attach a different session's checkpoints from the same repository.
            return None;
        }

        // Fallback when only `-c` / `--continue` is known and transcript path hasn't been resolved yet:
        // match the most recent archived session for `(cli_kind, repo_root/cwd)`.
        let norm_hint = cwd_hint
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|p| crate::checkpoint::canonicalize_path(std::path::Path::new(p)));
        let repo_hint = cwd_hint
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|p| crate::checkpoint::probe_repo(std::path::Path::new(p)))
            .map(|r| crate::checkpoint::canonicalize_path(&r.toplevel));
        let mut best: Option<(u64, crate::checkpoint::TargetCheckpoints)> = None;
        if let Ok(entries) = std::fs::read_dir(self.session_checkpoints_dir(kind)) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Ok(tc) = crate::paths::read_state::<crate::checkpoint::TargetCheckpoints>(&path) else {
                    continue;
                };
                if tc.checkpoints.is_empty() {
                    continue;
                }
                let matches_repo = match (&repo_hint, &norm_hint, tc.repo_root.as_deref()) {
                    (Some(rh), _, Some(tr)) => crate::checkpoint::canonicalize_path(std::path::Path::new(tr)) == *rh,
                    (_, Some(nh), Some(tr)) => {
                        let ctr = crate::checkpoint::canonicalize_path(std::path::Path::new(tr));
                        nh.starts_with(&ctr) || ctr.starts_with(nh)
                    }
                    _ => false,
                };
                if matches_repo {
                    let ts = tc.checkpoints.iter().map(|c| c.ts_ms).max().unwrap_or(0);
                    if best.as_ref().is_none_or(|(b_ts, _)| ts > *b_ts) {
                        best = Some((ts, tc));
                    }
                }
            }
        }
        best.map(|(_, tc)| tc)
    }

    pub fn save_target_checkpoints(&self, st: &mut State, mut tc: crate::checkpoint::TargetCheckpoints) {
        if tc.addr.is_empty() {
            return;
        }
        let addr = tc.addr.clone();
        let _ = crate::platform::private_dir(&self.checkpoints_dir());
        let path = self.checkpoint_path(&addr);
        if let Ok(bytes) = serde_json::to_vec_pretty(&tc) {
            let _ = crate::paths::write_atomic(&path, &bytes);
        }
        let _ = self.archive_cli_session_checkpoints(&tc);
        if let Some(TargetKey::Terminal(id)) = TargetKey::parse(&addr)
            && let Some(kind_str) = tc.cli_kind.as_deref().filter(|k| !k.is_empty() && *k != "generic")
            && let Some(t) = st.pty_reg.terminals.get_mut(&id)
            && t.is_live()
            && !crate::foreground::is_at_shell_root(t)
        {
            let mut changed = false;
            if let Some(ref mut saved) = t.saved_cli {
                if saved.kind == kind_str {
                    if let Some(ref sid) = tc.cli_session_id
                        && saved.session_id.as_deref() != Some(sid.as_str())
                    {
                        saved.session_id = Some(sid.clone());
                        changed = true;
                    }
                    if let Some(ref tp) = tc.cli_transcript_path
                        && saved.transcript_path.as_deref() != Some(tp.as_str())
                    {
                        saved.transcript_path = Some(tp.clone());
                        changed = true;
                    }
                }
            } else if let Some(mut extracted) = crate::pty_adapters::extract_saved_cli_from_argv(
                t.foreground.as_ref().filter(|f| !f.argv.is_empty()).map_or(&t.argv, |f| &f.argv),
                crate::pty_adapters::terminal_shell_executable(&t.argv).as_deref(),
                tc.cli_session_id.clone(),
                tc.cli_transcript_path.clone(),
            ) {
                extracted.kind = kind_str.to_string();
                t.saved_cli = Some(extracted);
                changed = true;
            }
            if changed {
                self.save_pty_soon();
            }
        }
        st.checkpoints.insert(addr, std::mem::take(&mut tc));
    }

    /// Detaches checkpoints from `addr` (e.g. when exiting a CLI back to shell or starting a fresh CLI).
    /// Archives identifiable CLI sessions (preserving their Git refs for later `-c`/`--resume`),
    /// while pruning Git refs for anonymous/non-CLI checkpoints.
    pub fn detach_target_checkpoints(&self, st: &mut State, addr: &str, fallback_cwd: &str) {
        let loaded = st.checkpoints.remove(addr).or_else(|| crate::paths::read_state(&self.checkpoint_path(addr)).ok());
        let _ = std::fs::remove_file(self.checkpoint_path(addr));
        if let Some(ref tc) = loaded
            && self.archive_cli_session_checkpoints(tc)
        {
            return;
        }
        let ref_owner = loaded.as_ref().and_then(|tc| tc.ref_namespace.clone()).filter(|s| !s.is_empty());
        let explicit_refs: Vec<String> =
            loaded.as_ref().map(|tc| tc.checkpoints.iter().map(|c| c.ref_name.clone()).collect()).unwrap_or_default();
        let repo_dir =
            loaded.and_then(|tc| tc.repo_root).filter(|s| !s.is_empty()).unwrap_or_else(|| fallback_cwd.to_string());
        if repo_dir.is_empty() {
            return;
        }
        let slug = self.session_slug();
        let addr_owned = addr.to_string();
        let run = move || {
            if let Some(repo) = crate::checkpoint::probe_repo(std::path::Path::new(&repo_dir)) {
                let _ = crate::checkpoint::delete_all_checkpoint_refs(&repo, &slug, &addr_owned);
                if let Some(owner) = ref_owner.as_deref()
                    && owner != addr_owned
                {
                    let _ = crate::checkpoint::delete_all_checkpoint_refs(&repo, &slug, owner);
                }
                let _ = crate::checkpoint::delete_checkpoint_ref_names(&repo, &explicit_refs);
            }
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn_blocking(run);
        } else {
            run();
        }
    }

    pub fn cleanup_target_checkpoints(&self, st: &mut State, addr: &str, fallback_cwd: &str) {
        self.detach_target_checkpoints(st, addr, fallback_cwd);
    }

    pub fn signal(&self, st: &State, id: u32, sig: i32) {
        if let Some(session) = st.sessions.get(&id) {
            session.transport.signal(sig);
        }
    }

    /// Waits until `done` holds or the timeout passes; returns whether it held.
    pub async fn wait_for(&self, timeout: Duration, done: impl Fn(&State) -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if done(&self.lock()) {
                return true;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return done(&self.lock());
            }
        }
    }

    /// Terminates live agents cleanly (`HUP` -> `TERM` -> `KILL`) and cleans up records/scopes.
    pub async fn kill(&self, ids: &[u32]) {
        let all_done = |st: &State| ids.iter().all(|id| !st.sessions.contains_key(id));
        {
            let mut st = self.lock();
            for &id in ids {
                if !st.sessions.contains_key(&id) && st.reg.agents.get(&id).is_some_and(|a| a.is_live()) {
                    let reason = if st.killing.contains(&id) {
                        "killed"
                    } else if self.restart.load(std::sync::atomic::Ordering::SeqCst) {
                        "stopped_by_restart"
                    } else {
                        "server_stop"
                    };
                    self.record_finish(&mut st, id, Exit::default(), reason);
                }
            }
            self.save(&st);
        }
        for (sig, wait) in [(signal::HUP, 500), (signal::TERM, 500), (signal::KILL, 2000)] {
            {
                let st = self.lock();
                for &id in ids {
                    self.signal(&st, id, sig);
                }
            }
            if self.wait_for(Duration::from_millis(wait), all_done).await {
                return;
            }
        }
        let scopes: Vec<String> = {
            let mut st = self.lock();
            let scopes: Vec<String> =
                ids.iter().filter_map(|id| st.reg.agents.get(id).and_then(|a| a.scope.clone())).collect();
            for &id in ids {
                let reason = if st.killing.contains(&id) { "killed" } else { "server_stop" };
                self.record_finish(&mut st, id, Exit::default(), reason);
            }
            self.save(&st);
            scopes
        };
        self.sweep_in_background(scopes);
    }

    fn sweep_in_background(&self, scopes: Vec<String>) {
        if scopes.is_empty() {
            return;
        }
        self.sweeps.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let sweeps = self.sweeps.clone();
        std::thread::spawn(move || {
            crate::sweep::sweep(&scopes);
            sweeps.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
    }

    fn wait_for_sweeps(&self) {
        let deadline = Instant::now() + Duration::from_secs(6);
        while self.sweeps.load(std::sync::atomic::Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Planned restart: stop active ACP adapters cleanly and hand over PTY holders so kept entries recover.
    pub async fn handover(&self) -> io::Result<()> {
        let live: Vec<u32> = {
            let mut st = self.lock();
            st.stopping = true;
            st.reg.agents.values().filter(|a| a.is_live()).map(Agent::id).collect()
        };
        self.changed.notify_waiters();
        self.kill(&live).await;
        crate::exec::wait_all().await;
        self.wait_for_sweeps();
        self.goodbye(crate::holder_proto::Goodbye::Handover);
        self.wait_for_sweeps();
        {
            let mut st = self.lock();
            for id in live {
                self.record_finish(&mut st, id, Exit::default(), "stopped_by_restart");
            }
            self.save(&st);
            self.save_pty_handover_state(&mut st);
        }
        self.registry_writer.flush().await?;
        self.pty_registry_writer.flush().await
    }

    /// Server stop: terminate all live agents and PTY terminals, then flush registry state.
    pub async fn shutdown(&self) -> Result<(), RpcError> {
        let (live_agents, live_terminals): (Vec<u32>, Vec<u32>) = {
            let mut st = self.lock();
            st.stopping = true;
            (
                st.reg.agents.values().filter(|a| a.is_live()).map(Agent::id).collect(),
                st.pty_reg.terminals.values().filter(|t| t.is_live()).map(|t| t.id()).collect(),
            )
        };
        self.changed.notify_waiters();
        self.kill(&live_agents).await;
        self.pty_kill(&live_terminals).await;
        crate::exec::wait_all().await;
        self.wait_for_sweeps();
        self.goodbye(crate::holder_proto::Goodbye::Shutdown);
        self.wait_for_sweeps();
        {
            let mut st = self.lock();
            for id in live_agents {
                self.record_finish(&mut st, id, Exit::default(), "server_stop");
            }
            for id in live_terminals {
                self.pty_record_finish(&mut st, id, Exit::default(), "server_stop");
            }
            self.save(&st);
            self.save_pty(&st);
        }
        self.flush_saved().await
    }
}

#[cfg(test)]
mod tests;
