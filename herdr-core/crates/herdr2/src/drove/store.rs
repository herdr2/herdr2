//! Thread-safe Drove store backed by atomic `state.json` and append-only `items/<worker>.jsonl`.

use super::config::DrovePaths;
use super::crypto;
use super::types::*;
use crate::paths::write_atomic;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, broadcast};

const MAX_DAEMON_EVENTS: usize = 5000;
const MAX_CACHED_WORKER_ITEMS: usize = 24;
const WORKER_HEARTBEAT_EMIT_INTERVAL: Duration = Duration::from_secs(10);
const DEBOUNCED_STATE_FLUSH_INTERVAL: Duration = Duration::from_millis(200);

/// Event broadcast on every state or item mutation for SSE and long-poll waiters.
#[derive(Clone, Debug)]
pub enum StoreEvent {
    Change(ChangeEvent),
    Item { run_id: Option<String>, item: Item },
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    next_msg_seq: u64,
    next_event_id: u64,
    next_item_id: u64,
    #[serde(default)]
    next_contract_seq: u64,
    #[serde(default)]
    runs: BTreeMap<String, Run>,
    #[serde(default)]
    tasks: BTreeMap<String, Task>,
    #[serde(default)]
    workers: BTreeMap<String, Worker>,
    #[serde(default)]
    attempts: BTreeMap<String, Attempt>,
    #[serde(default)]
    messages: Vec<Message>,
    #[serde(default)]
    contracts: Vec<ContractPost>,
    #[serde(default)]
    daemon_events: Vec<DaemonEvent>,
}

struct StoreInner {
    data: PersistedState,
    items: HashMap<String, Vec<Item>>,
    items_lru: VecDeque<String>,
    state_file: PathBuf,
    items_dir: PathBuf,
    artifacts_dir: PathBuf,
    dirty: bool,
    last_worker_touch_emit: HashMap<String, Instant>,
}

impl StoreInner {
    fn flush_state(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if let Some(parent) = self.state_file.parent() {
            crate::platform::private_dir(parent)?;
        }
        let bytes = serde_json::to_vec(&self.data).map_err(io::Error::other)?;
        write_atomic(&self.state_file, &bytes)?;
        self.dirty = false;
        Ok(())
    }

    fn append_item_disk(&self, worker_id: &str, item: &Item) -> io::Result<()> {
        crate::platform::private_dir(&self.items_dir)?;
        let path = self.items_dir.join(format!("{worker_id}.jsonl"));
        let mut f = OpenOptions::new().create(true).append(true).open(path)?;
        serde_json::to_writer(&mut f, item).map_err(io::Error::other)?;
        f.write_all(b"\n")?;
        Ok(())
    }

    fn touch_items_lru(&mut self, worker_id: &str) {
        self.items_lru.retain(|id| id != worker_id);
        self.items_lru.push_back(worker_id.to_string());
        while self.items.len() > MAX_CACHED_WORKER_ITEMS {
            let Some(pos) = self.items_lru.iter().position(|wid| {
                wid != worker_id
                    && !self
                        .data
                        .workers
                        .get(wid)
                        .is_some_and(|w| matches!(w.state, WorkerState::Busy | WorkerState::Starting))
            }) else {
                break;
            };
            if let Some(evict_id) = self.items_lru.remove(pos) {
                self.items.remove(&evict_id);
            }
        }
    }

    fn load_worker_items(&mut self, worker_id: &str) -> &[Item] {
        if !self.items.contains_key(worker_id) {
            let path = self.items_dir.join(format!("{worker_id}.jsonl"));
            let mut loaded = Vec::new();
            if let Ok(file) = File::open(path) {
                for line in BufReader::new(file).lines().map_while(Result::ok) {
                    if let Ok(item) = serde_json::from_str::<Item>(&line) {
                        if item.id > self.data.next_item_id {
                            self.data.next_item_id = item.id;
                        }
                        loaded.push(item);
                    }
                }
            }
            self.items.insert(worker_id.to_string(), loaded);
        }
        self.touch_items_lru(worker_id);
        self.items.get(worker_id).map(Vec::as_slice).unwrap_or(&[])
    }
}

pub struct CreateRunInput {
    pub objective: String,
    pub cwd: String,
    pub host: String,
    pub host_session: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub host_pid: Option<u32>,
    pub session_bound: Option<bool>,
    pub max_concurrency: u32,
    pub budget: Option<Budget>,
    pub verify: Option<VerifySpec>,
    pub goal: Option<GoalPolicy>,
}

pub struct CreateTaskInput {
    pub run_id: String,
    pub key: String,
    pub title: String,
    pub phase: Option<String>,
    pub spec: Spec,
    pub tags: Vec<String>,
    pub files: Vec<String>,
    pub deps: Vec<String>,
    pub output_schema: Option<Value>,
    pub isolation: Isolation,
    pub timeout_sec: Option<u64>,
    pub stall_warn_sec: Option<u64>,
    pub stall_kill_sec: Option<u64>,
    pub when: Option<TaskWhen>,
    pub assign: Option<Assign>,
    pub status: TaskStatus,
    pub result: Option<TaskResult>,
    pub kind: Option<String>,
    pub trigger_rule: Option<TriggerRule>,
    pub context: Option<Value>,
    pub exec: Option<ExecSpec>,
    pub foreach_from: Option<ForeachFrom>,
    pub template: Option<bool>,
    pub expanded_from: Option<String>,
    pub review: Option<ReviewSpec>,
    pub cache: Option<Value>,
    pub cached_from: Option<String>,
    pub retry_on_worker_reported: Option<bool>,
    pub fork_from: Option<String>,
    pub fork_mode: Option<String>,
    pub channels: Vec<String>,
}

pub struct PostMessageInput {
    pub run_id: String,
    pub kind: MessageType,
    pub from: String,
    pub to: String,
    pub thread_id: Option<String>,
    pub task_id: Option<String>,
    pub attempt_id: Option<String>,
    pub subject: String,
    pub body: Option<String>,
    pub options: Option<Vec<String>>,
    pub recommend: Option<String>,
    pub why: Option<String>,
    pub raised_by: Option<String>,
    pub raised_by_worker: Option<String>,
    pub tag: Option<String>,
    pub ai_draft: Option<MessageAiDraft>,
    pub needs_action: bool,
}

pub struct NewDaemonEvent {
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub attempt_id: Option<String>,
    pub worker_id: Option<String>,
    pub message_id: Option<String>,
    pub category: String,
    pub kind: String,
    pub status: String,
    pub title: String,
    pub detail: Option<String>,
    pub duration_ms: Option<u64>,
    pub tokens: Option<DaemonEventTokens>,
    pub cost: Option<f64>,
}

/// Persistent in-memory + disk state store for one `DroveRuntime`.
pub struct Store {
    inner: Mutex<StoreInner>,
    events: broadcast::Sender<StoreEvent>,
    flush_notify: Arc<Notify>,
    pub changed: Arc<Notify>,
}

impl Drop for Store {
    fn drop(&mut self) {
        if let Ok(mut g) = self.inner.lock() {
            let _ = g.flush_state();
        }
    }
}

/// Coalesces bursty non-structural state updates (streaming token counters, heartbeats, `ok` events)
/// into bounded atomic `state.json` disk writes without serializing on every streaming delta.
pub async fn run_flush_loop(store: Arc<Store>, stop: Arc<Notify>) {
    loop {
        tokio::select! {
            _ = store.flush_notify.notified() => {}
            _ = stop.notified() => {
                let _ = store.flush_now();
                break;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(DEBOUNCED_STATE_FLUSH_INTERVAL) => {
                let _ = store.flush_now();
            }
            _ = stop.notified() => {
                let _ = store.flush_now();
                break;
            }
        }
    }
}

impl Store {
    pub fn open(paths: &DrovePaths) -> io::Result<Arc<Self>> {
        paths.ensure_dirs()?;
        let data: PersistedState = crate::paths::read_state(&paths.state_file)?;
        let (events, _) = broadcast::channel(2048);
        Ok(Arc::new(Self {
            inner: Mutex::new(StoreInner {
                data,
                items: HashMap::new(),
                items_lru: VecDeque::new(),
                state_file: paths.state_file.clone(),
                items_dir: paths.items_dir.clone(),
                artifacts_dir: paths.artifacts_dir.clone(),
                dirty: false,
                last_worker_touch_emit: HashMap::new(),
            }),
            events,
            flush_notify: Arc::new(Notify::new()),
            changed: Arc::new(Notify::new()),
        }))
    }

    /// Synchronously flushes any pending `dirty` state mutations to `state.json`.
    pub fn flush_now(&self) -> io::Result<()> {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.flush_state()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StoreEvent> {
        self.events.subscribe()
    }

    fn emit_change(&self, table: &str, id: Value, run_id: Option<String>) {
        let _ = self.events.send(StoreEvent::Change(ChangeEvent { table: table.into(), id, run_id }));
        self.changed.notify_waiters();
    }

    // --- Runs ---------------------------------------------------------------

    pub fn list_runs(&self) -> Vec<Run> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.data.runs.values().cloned().collect()
    }

    pub fn get_run(&self, id: &str) -> Option<Run> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.data.runs.get(id).cloned()
    }

    pub fn create_run(&self, input: CreateRunInput) -> Run {
        let now = now_iso();
        let run = Run {
            id: crypto::rand_id("run"),
            objective: input.objective,
            cwd: input.cwd,
            coordinator: CoordinatorInfo {
                host: input.host,
                host_session: input.host_session,
                generation: 1,
                agent: input.agent,
                model: input.model,
                host_pid: input.host_pid,
                session_bound: input.session_bound,
                notification: None,
            },
            max_concurrency: input.max_concurrency,
            budget: input.budget,
            status: RunStatus::Active,
            pause_reason: None,
            verify: input.verify,
            goal: input.goal,
            finalization: None,
            verified_at: None,
            name: None,
            order: None,
            trashed_at: None,
            status_before_trash: None,
            ai_usage: None,
            briefing: None,
            machine: None,
            created_at: now.clone(),
            updated_at: now,
        };
        {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.data.runs.insert(run.id.clone(), run.clone());
            g.dirty = true;
            let _ = g.flush_state();
        }
        self.emit_change("runs", Value::String(run.id.clone()), Some(run.id.clone()));
        run
    }

    pub fn update_run(&self, id: &str, f: impl FnOnce(&mut Run)) -> Option<Run> {
        let updated = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let run = g.data.runs.get_mut(id)?;
            f(run);
            run.updated_at = now_iso();
            let cloned = run.clone();
            g.dirty = true;
            let _ = g.flush_state();
            cloned
        };
        self.emit_change("runs", Value::String(updated.id.clone()), Some(updated.id.clone()));
        Some(updated)
    }

    pub fn reorder_runs(&self, ids: &[String]) {
        {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            for (idx, id) in ids.iter().enumerate() {
                if let Some(run) = g.data.runs.get_mut(id) {
                    run.order = Some(idx as f64);
                    run.updated_at = now_iso();
                }
            }
            g.dirty = true;
            let _ = g.flush_state();
        }
        for id in ids {
            self.emit_change("runs", Value::String(id.clone()), Some(id.clone()));
        }
    }

    pub fn delete_run(&self, id: &str) -> bool {
        let worker_ids: Vec<String>;
        let artifacts_path: PathBuf;
        {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if g.data.runs.remove(id).is_none() {
                return false;
            }
            g.data.tasks.retain(|_, t| t.run_id != id);
            g.data.attempts.retain(|_, a| a.run_id != id);
            worker_ids = g.data.workers.values().filter(|w| w.run_id == id).map(|w| w.id.clone()).collect();
            g.data.workers.retain(|_, w| w.run_id != id);
            g.data.messages.retain(|m| m.run_id != id);
            g.data.contracts.retain(|c| c.run_id != id);
            g.data.daemon_events.retain(|e| e.run_id.as_deref() != Some(id));
            for wid in &worker_ids {
                g.items.remove(wid);
                g.items_lru.retain(|id| id != wid);
                let _ = std::fs::remove_file(g.items_dir.join(format!("{wid}.jsonl")));
            }
            artifacts_path = g.artifacts_dir.join(id);
            g.dirty = true;
            let _ = g.flush_state();
        }
        let _ = std::fs::remove_dir_all(artifacts_path);
        self.emit_change("runs", Value::String(id.to_string()), Some(id.to_string()));
        true
    }

    pub fn count_tasks_by_run(&self) -> HashMap<String, BTreeMap<String, u32>> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: HashMap<String, BTreeMap<String, u32>> = HashMap::new();
        for task in g.data.tasks.values() {
            let entry = out.entry(task.run_id.clone()).or_insert_with(empty_task_counts);
            let k = task_status_str(task.status);
            *entry.entry(k.to_string()).or_insert(0) += 1;
        }
        out
    }

    // --- Tasks --------------------------------------------------------------

    pub fn list_tasks(&self, run_id: &str) -> Vec<Task> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut list: Vec<Task> = g.data.tasks.values().filter(|t| t.run_id == run_id).cloned().collect();
        list.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.key.cmp(&b.key)));
        list
    }

    pub fn get_task(&self, id: &str) -> Option<Task> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.data.tasks.get(id).cloned()
    }

    pub fn find_task_in_run(&self, run_id: &str, id_or_key: &str) -> Option<Task> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.data
            .tasks
            .get(id_or_key)
            .filter(|t| t.run_id == run_id)
            .cloned()
            .or_else(|| g.data.tasks.values().find(|t| t.run_id == run_id && t.key == id_or_key).cloned())
    }

    pub fn create_task(&self, input: CreateTaskInput) -> Task {
        let now = now_iso();
        let task = Task {
            id: crypto::rand_id("tsk"),
            run_id: input.run_id,
            key: input.key,
            title: input.title,
            phase: input.phase,
            spec: input.spec,
            tags: input.tags,
            files: input.files,
            deps: input.deps,
            output_schema: input.output_schema,
            isolation: input.isolation,
            effective_isolation: None,
            isolation_reason: None,
            timeout_sec: input.timeout_sec,
            stall_warn_sec: input.stall_warn_sec,
            stall_kill_sec: input.stall_kill_sec,
            when: input.when,
            assign: input.assign,
            status: input.status,
            result: input.result,
            failures: 0,
            created_at: now.clone(),
            updated_at: now,
            kind: input.kind,
            exec_phase: None,
            execution_version: Some(1),
            trigger_rule: input.trigger_rule,
            context: input.context,
            exec: input.exec,
            foreach_from: input.foreach_from,
            template: input.template,
            expansion_state: input.template.filter(|t| *t).map(|_| "pending".to_string()),
            expanded_from: input.expanded_from,
            review: input.review,
            review_state: None,
            review_circuit: None,
            candidate_result: None,
            cache: input.cache,
            cached_from: input.cached_from,
            retry_on_worker_reported: input.retry_on_worker_reported,
            fork_from: input.fork_from,
            fork_mode: input.fork_mode,
            channels: input.channels,
        };
        {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.data.tasks.insert(task.id.clone(), task.clone());
            g.dirty = true;
            let _ = g.flush_state();
        }
        self.emit_change("tasks", Value::String(task.id.clone()), Some(task.run_id.clone()));
        task
    }

    pub fn update_task(&self, id: &str, f: impl FnOnce(&mut Task)) -> Option<Task> {
        let updated = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let task = g.data.tasks.get_mut(id)?;
            f(task);
            task.updated_at = now_iso();
            let cloned = task.clone();
            g.dirty = true;
            let _ = g.flush_state();
            cloned
        };
        self.emit_change("tasks", Value::String(updated.id.clone()), Some(updated.run_id.clone()));
        Some(updated)
    }

    // --- Workers ------------------------------------------------------------

    pub fn list_workers(&self, run_id: Option<&str>) -> Vec<Worker> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: Vec<Worker> =
            g.data.workers.values().filter(|w| run_id.is_none_or(|r| w.run_id == r)).cloned().collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        out
    }

    pub fn get_worker(&self, id: &str) -> Option<Worker> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.data.workers.get(id).cloned()
    }

    pub fn find_worker_by_h2_agent(&self, h2_address: &str) -> Option<Worker> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.data
            .workers
            .values()
            .find(|w| w.h2_agent.as_deref() == Some(h2_address) || w.session_key == h2_address)
            .cloned()
    }

    pub fn insert_worker(&self, mut worker: Worker) -> Worker {
        sanitize_worker_forked_from(&mut worker, None);
        {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.data.workers.insert(worker.id.clone(), worker.clone());
            g.dirty = true;
            let _ = g.flush_state();
        }
        self.emit_change("workers", Value::String(worker.id.clone()), Some(worker.run_id.clone()));
        worker
    }

    pub fn update_worker(&self, id: &str, f: impl FnOnce(&mut Worker)) -> Option<Worker> {
        let (updated, structural) = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let worker = g.data.workers.get_mut(id)?;
            let prev_state = worker.state;
            let prev_att = worker.current_attempt_id.clone();
            let prev_wt = worker.worktree.clone();
            let prev_fork = worker.forked_from.clone();
            f(worker);
            sanitize_worker_forked_from(worker, prev_fork.as_deref());
            worker.last_active_at = now_iso();
            let structural =
                worker.state != prev_state || worker.current_attempt_id != prev_att || worker.worktree != prev_wt;
            let cloned = worker.clone();
            g.dirty = true;
            if structural {
                let _ = g.flush_state();
            }
            (cloned, structural)
        };
        if !structural {
            self.flush_notify.notify_one();
        }
        self.emit_change("workers", Value::String(updated.id.clone()), Some(updated.run_id.clone()));
        Some(updated)
    }

    /// Lightweight heartbeat update for high-frequency streaming chunks: keeps `last_active_at`
    /// accurate in memory for stall watchdogs while throttling `state.json` disk flushes and
    /// `table: "workers"` SSE broadcasts to avoid snapshot storms.
    pub fn touch_worker_active(&self, id: &str) {
        let emit_run_id = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let Some(worker) = g.data.workers.get_mut(id) else {
                return;
            };
            worker.last_active_at = now_iso();
            let run_id = worker.run_id.clone();
            g.dirty = true;
            let now = Instant::now();
            let should_emit = g
                .last_worker_touch_emit
                .get(id)
                .is_none_or(|prev| now.saturating_duration_since(*prev) >= WORKER_HEARTBEAT_EMIT_INTERVAL);
            if should_emit {
                g.last_worker_touch_emit.insert(id.to_string(), now);
                Some(run_id)
            } else {
                None
            }
        };
        if let Some(run_id) = emit_run_id {
            self.flush_notify.notify_one();
            self.emit_change("workers", Value::String(id.to_string()), Some(run_id));
        }
    }

    // --- Attempts -----------------------------------------------------------

    pub fn list_attempts(&self, run_id: Option<&str>, task_id: Option<&str>, worker_id: Option<&str>) -> Vec<Attempt> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut list: Vec<Attempt> = g
            .data
            .attempts
            .values()
            .filter(|a| {
                run_id.is_none_or(|r| a.run_id == r)
                    && task_id.is_none_or(|t| a.task_id == t)
                    && worker_id.is_none_or(|w| a.worker_id == w)
            })
            .cloned()
            .collect();
        list.sort_by(|a, b| a.started_at.cmp(&b.started_at).then_with(|| a.id.cmp(&b.id)));
        list
    }

    pub fn get_attempt(&self, id: &str) -> Option<Attempt> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.data.attempts.get(id).cloned()
    }

    pub fn insert_attempt(&self, attempt: Attempt) -> Attempt {
        {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.data.attempts.insert(attempt.id.clone(), attempt.clone());
            g.dirty = true;
            let _ = g.flush_state();
        }
        self.emit_change("attempts", Value::String(attempt.id.clone()), Some(attempt.run_id.clone()));
        attempt
    }

    pub fn update_attempt(&self, id: &str, f: impl FnOnce(&mut Attempt)) -> Option<Attempt> {
        let (updated, structural) = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let attempt = g.data.attempts.get_mut(id)?;
            let prev_outcome = attempt.outcome;
            let prev_ended = attempt.ended_at.clone();
            let prev_verify_rej = attempt.verify_rejections;
            let prev_review = attempt.review_round.as_ref().map(|r| r.round);
            f(attempt);
            let structural = attempt.outcome != prev_outcome
                || attempt.ended_at != prev_ended
                || attempt.verify_rejections != prev_verify_rej
                || attempt.review_round.as_ref().map(|r| r.round) != prev_review
                || attempt.ai_diagnosis.is_some();
            let cloned = attempt.clone();
            g.dirty = true;
            if structural {
                let _ = g.flush_state();
            }
            (cloned, structural)
        };
        if !structural {
            self.flush_notify.notify_one();
        }
        self.emit_change("attempts", Value::String(updated.id.clone()), Some(updated.run_id.clone()));
        Some(updated)
    }

    // --- Messages -----------------------------------------------------------

    pub fn post_message(&self, input: PostMessageInput) -> Message {
        let msg = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.data.next_msg_seq += 1;
            let seq = g.data.next_msg_seq;
            let msg = Message {
                id: crypto::rand_id("msg"),
                seq,
                run_id: input.run_id,
                kind: input.kind,
                from: input.from,
                to: input.to,
                thread_id: input.thread_id,
                task_id: input.task_id,
                attempt_id: input.attempt_id,
                subject: input.subject,
                body: input.body,
                options: input.options,
                recommend: input.recommend,
                why: input.why,
                raised_by: input.raised_by,
                raised_by_worker: input.raised_by_worker,
                tag: input.tag,
                ai_draft: input.ai_draft,
                needs_action: input.needs_action,
                handled_at: None,
                created_at: now_iso(),
            };
            g.data.messages.push(msg.clone());
            g.dirty = true;
            let _ = g.flush_state();
            msg
        };
        self.emit_change("messages", Value::String(msg.id.clone()), Some(msg.run_id.clone()));
        msg
    }

    pub fn get_message(&self, id: &str) -> Option<Message> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.data.messages.iter().find(|m| m.id == id).cloned()
    }

    pub fn list_messages(
        &self,
        run_id: &str,
        needs_action: Option<bool>,
        unhandled_only: bool,
        limit: Option<usize>,
    ) -> Vec<Message> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let iter = g.data.messages.iter().filter(|m| {
            m.run_id == run_id
                && needs_action.is_none_or(|na| m.needs_action == na)
                && (!unhandled_only || m.handled_at.is_none())
        });
        let mut out: Vec<Message> = iter.cloned().collect();
        if let Some(lim) = limit
            && out.len() > lim
        {
            out = out.split_off(out.len() - lim);
        }
        out
    }

    pub fn latest_reply_for(&self, run_id: &str, thread_id: &str) -> Option<Message> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.data
            .messages
            .iter()
            .rev()
            .find(|m| m.run_id == run_id && m.thread_id.as_deref() == Some(thread_id) && m.kind == MessageType::Reply)
            .cloned()
    }

    pub fn drain_worker_inbox(&self, worker_id: &str, attempt_id: &str) -> Vec<Message> {
        let to_addr = format!("worker:{worker_id}");
        let mut drained = Vec::new();
        {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let now = now_iso();
            for m in &mut g.data.messages {
                if m.to == to_addr
                    && m.kind != MessageType::Reply
                    && m.handled_at.is_none()
                    && m.attempt_id.as_deref().is_none_or(|aid| aid == attempt_id)
                {
                    m.handled_at = Some(now.clone());
                    drained.push(m.clone());
                }
            }
            if !drained.is_empty() {
                g.dirty = true;
                let _ = g.flush_state();
            }
        }
        for m in &drained {
            self.emit_change("messages", Value::String(m.id.clone()), Some(m.run_id.clone()));
        }
        drained
    }

    /// Atomically posts a reply message and marks the parent question/escalation handled in a single
    /// lock + state flush, avoiding double serialization/fsync on the coordinator reply path.
    pub fn post_reply_and_mark_handled(&self, parent_id: &str, input: PostMessageInput) -> Message {
        let (reply, parent_updated) = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.data.next_msg_seq += 1;
            let seq = g.data.next_msg_seq;
            let now = now_iso();
            let reply = Message {
                id: crypto::rand_id("msg"),
                seq,
                run_id: input.run_id,
                kind: input.kind,
                from: input.from,
                to: input.to,
                thread_id: input.thread_id,
                task_id: input.task_id,
                attempt_id: input.attempt_id,
                subject: input.subject,
                body: input.body,
                options: input.options,
                recommend: input.recommend,
                why: input.why,
                raised_by: input.raised_by,
                raised_by_worker: input.raised_by_worker,
                tag: input.tag,
                ai_draft: input.ai_draft,
                needs_action: input.needs_action,
                handled_at: None,
                created_at: now.clone(),
            };
            g.data.messages.push(reply.clone());
            let parent_updated = if let Some(m) = g.data.messages.iter_mut().find(|m| m.id == parent_id) {
                if m.handled_at.is_none() {
                    m.handled_at = Some(now);
                }
                Some((m.id.clone(), m.run_id.clone()))
            } else {
                None
            };
            g.dirty = true;
            let _ = g.flush_state();
            (reply, parent_updated)
        };
        self.emit_change("messages", Value::String(reply.id.clone()), Some(reply.run_id.clone()));
        if let Some((pid, rid)) = parent_updated {
            self.emit_change("messages", Value::String(pid), Some(rid));
        }
        reply
    }

    pub fn mark_handled(&self, id: &str) -> Option<Message> {
        let updated = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let m = g.data.messages.iter_mut().find(|m| m.id == id)?;
            if m.handled_at.is_none() {
                m.handled_at = Some(now_iso());
            }
            let cloned = m.clone();
            g.dirty = true;
            let _ = g.flush_state();
            cloned
        };
        self.emit_change("messages", Value::String(updated.id.clone()), Some(updated.run_id.clone()));
        Some(updated)
    }

    pub fn update_message(&self, id: &str, f: impl FnOnce(&mut Message)) -> Option<Message> {
        let updated = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let m = g.data.messages.iter_mut().find(|m| m.id == id)?;
            f(m);
            let cloned = m.clone();
            g.dirty = true;
            let _ = g.flush_state();
            cloned
        };
        self.emit_change("messages", Value::String(updated.id.clone()), Some(updated.run_id.clone()));
        Some(updated)
    }

    // --- Worker Transcript Items --------------------------------------------

    pub fn append_item(
        &self,
        worker_id: &str,
        attempt_id: Option<String>,
        kind: &str,
        text: Option<String>,
        tool: Option<ItemTool>,
        usage: Option<ItemUsage>,
    ) -> Item {
        let (item, run_id) = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.data.next_item_id += 1;
            let id = g.data.next_item_id;
            let run_id = g.data.workers.get(worker_id).map(|w| w.run_id.clone());
            let item = Item {
                id,
                worker_id: worker_id.into(),
                attempt_id,
                ts: now_iso(),
                kind: kind.into(),
                text,
                tool,
                usage,
            };
            let _ = g.load_worker_items(worker_id);
            g.items.entry(worker_id.to_string()).or_default().push(item.clone());
            g.touch_items_lru(worker_id);
            let _ = g.append_item_disk(worker_id, &item);
            g.dirty = true;
            (item, run_id)
        };
        self.flush_notify.notify_one();
        let _ = self.events.send(StoreEvent::Item { run_id, item: item.clone() });
        item
    }

    pub fn list_items(
        &self,
        worker_id: &str,
        attempt_id: Option<&str>,
        before: Option<u64>,
        limit: usize,
    ) -> Vec<Item> {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let all = g.load_worker_items(worker_id);
        let filtered: Vec<Item> = all
            .iter()
            .filter(|it| attempt_id.is_none_or(|aid| it.attempt_id.as_deref() == Some(aid)))
            .filter(|it| before.is_none_or(|b| it.id < b))
            .cloned()
            .collect();
        if filtered.len() > limit { filtered[filtered.len() - limit..].to_vec() } else { filtered }
    }

    // --- Daemon Events ------------------------------------------------------

    pub fn publish_contract(
        &self,
        run_id: &str,
        channel: &str,
        key: &str,
        author_task: &str,
        worker_id: Option<String>,
        summary: String,
        payload: Option<Value>,
    ) -> ContractPost {
        let norm_channel = {
            let trimmed = channel.trim();
            if trimmed.is_empty() { "main".to_string() } else { trimmed.trim_start_matches('#').to_string() }
        };
        let norm_key = key.trim().to_string();
        let post = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.data.next_contract_seq += 1;
            let seq = g.data.next_contract_seq;
            let post = ContractPost {
                seq,
                run_id: run_id.to_string(),
                channel: norm_channel,
                key: norm_key,
                author_task: author_task.to_string(),
                worker_id,
                summary,
                payload,
                created_at: now_iso(),
            };
            g.data.contracts.push(post.clone());
            g.dirty = true;
            let _ = g.flush_state();
            post
        };
        self.emit_change("contracts", Value::Number(post.seq.into()), Some(run_id.to_string()));
        post
    }

    pub fn list_contracts(&self, run_id: &str, channels: Option<&[String]>) -> Vec<ContractPost> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.data
            .contracts
            .iter()
            .filter(|c| c.run_id == run_id)
            .filter(|c| match channels {
                None => true,
                Some(allowed) => {
                    c.channel == "main"
                        || allowed.iter().any(|ch| ch == "*" || ch.trim_start_matches('#') == c.channel)
                }
            })
            .cloned()
            .collect()
    }

    pub fn get_contract(&self, run_id: &str, selector: &str) -> Option<ContractPost> {
        let clean = selector
            .trim()
            .trim_start_matches('#')
            .strip_prefix("contract:")
            .unwrap_or_else(|| selector.trim().trim_start_matches('#'));
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Ok(seq) = clean.parse::<u64>() {
            return g.data.contracts.iter().find(|c| c.run_id == run_id && c.seq == seq).cloned();
        }
        if let Some((ch, k)) = clean.split_once(':') {
            let ch_norm = ch.trim().trim_start_matches('#');
            let k_norm = k.trim();
            return g
                .data
                .contracts
                .iter()
                .rev()
                .find(|c| c.run_id == run_id && c.channel == ch_norm && c.key == k_norm)
                .cloned();
        }
        g.data.contracts.iter().rev().find(|c| c.run_id == run_id && c.key == clean).cloned()
    }

    pub fn append_daemon_event(&self, ev: NewDaemonEvent) -> DaemonEvent {
        let immediate = ev.status != "ok";
        let event = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.data.next_event_id += 1;
            let id = g.data.next_event_id;
            let event = DaemonEvent {
                id,
                ts: now_iso(),
                run_id: ev.run_id,
                task_id: ev.task_id,
                attempt_id: ev.attempt_id,
                worker_id: ev.worker_id,
                message_id: ev.message_id,
                category: ev.category,
                kind: ev.kind,
                status: ev.status,
                title: ev.title,
                detail: ev.detail,
                duration_ms: ev.duration_ms,
                tokens: ev.tokens,
                cost: ev.cost,
            };
            g.data.daemon_events.push(event.clone());
            if g.data.daemon_events.len() > MAX_DAEMON_EVENTS {
                let excess = g.data.daemon_events.len() - MAX_DAEMON_EVENTS;
                g.data.daemon_events.drain(..excess);
            }
            g.dirty = true;
            if immediate {
                let _ = g.flush_state();
            }
            event
        };
        if !immediate {
            self.flush_notify.notify_one();
        }
        let _ = self.events.send(StoreEvent::Change(ChangeEvent {
            table: "daemon_events".into(),
            id: Value::Number(event.id.into()),
            run_id: event.run_id.clone(),
        }));
        self.changed.notify_waiters();
        event
    }

    pub fn list_daemon_events(
        &self,
        run_id: Option<&str>,
        category: Option<&str>,
        status: Option<&str>,
        before: Option<u64>,
        limit: usize,
    ) -> (Vec<DaemonEvent>, bool) {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut matched: Vec<DaemonEvent> = g
            .data
            .daemon_events
            .iter()
            .rev()
            .filter(|e| {
                run_id.is_none_or(|r| e.run_id.as_deref() == Some(r))
                    && category.is_none_or(|c| e.category == c)
                    && status.is_none_or(|s| e.status == s)
                    && before.is_none_or(|b| e.id < b)
            })
            .take(limit + 1)
            .cloned()
            .collect();
        let has_more = matched.len() > limit;
        if has_more {
            matched.truncate(limit);
        }
        (matched, has_more)
    }

    // --- Artifacts ----------------------------------------------------------

    pub fn write_artifact(&self, run_id: &str, artifact_name: &str, content: &[u8]) -> io::Result<PathBuf> {
        validate_artifact_name(artifact_name)?;
        validate_artifact_name(run_id)?;
        let dir = {
            let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.artifacts_dir.join(run_id)
        };
        crate::platform::private_dir(&dir)?;
        let path = dir.join(artifact_name);
        write_atomic(&path, content)?;
        Ok(path)
    }

    pub fn read_artifact_slice(
        &self,
        run_id: &str,
        artifact_name: &str,
        offset: usize,
        limit: usize,
        grep: Option<&str>,
    ) -> io::Result<Value> {
        validate_artifact_name(artifact_name)?;
        validate_artifact_name(run_id)?;
        let path = {
            let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.artifacts_dir.join(run_id).join(artifact_name)
        };
        let bytes = std::fs::read(&path)?;
        if let Some(needle) = grep.filter(|s| !s.is_empty()) {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.lines().filter(|l| l.contains(needle)).collect();
            let joined = lines.join("\n");
            let total = joined.len();
            let slice_bytes = joined.as_bytes();
            let start = offset.min(total);
            let end = start.saturating_add(limit).min(total);
            let content = String::from_utf8_lossy(&slice_bytes[start..end]).into_owned();
            return Ok(json!({
                "content": content,
                "totalBytes": total,
                "truncated": end < total,
                "offset": start,
            }));
        }
        let total = bytes.len();
        let start = offset.min(total);
        let end = start.saturating_add(limit).min(total);
        let content = String::from_utf8_lossy(&bytes[start..end]).into_owned();
        Ok(json!({
            "content": content,
            "totalBytes": total,
            "truncated": end < total,
            "offset": start,
        }))
    }

    // --- Historical Stats ---------------------------------------------------

    pub fn compute_stats(&self) -> BTreeMap<String, StatsEntry> {
        #[derive(Default)]
        struct Acc {
            n: u32,
            success: u32,
            tokens: u64,
            cost: f64,
            duration_ms: u64,
            retried: u32,
            no_done: u32,
        }
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut by_key: BTreeMap<String, Acc> = BTreeMap::new();
        for a in g.data.attempts.values() {
            let Some(outcome) = a.outcome else { continue };
            let mut keys = vec![a.agent.clone()];
            if let Some(m) = &a.model
                && !m.trim().is_empty()
            {
                keys.push(format!("{}/{m}", a.agent));
            }
            let total_tokens = a.tokens.input + a.tokens.output + a.tokens.cache_read;
            let cost = a.cost.unwrap_or(0.0);
            let dur = a
                .ended_at
                .as_deref()
                .and_then(parse_iso_ms)
                .zip(parse_iso_ms(&a.started_at))
                .map(|(e, s)| e.saturating_sub(s))
                .unwrap_or(0);
            let is_no_done = a.failure.as_ref().is_some_and(|f| f.kind == FailureKind::NoDone);
            for k in keys {
                let acc = by_key.entry(k).or_default();
                acc.n += 1;
                if outcome == Outcome::Succeeded {
                    acc.success += 1;
                }
                acc.tokens += total_tokens;
                acc.cost += cost;
                acc.duration_ms += dur;
                if a.retry_of.is_some() {
                    acc.retried += 1;
                }
                if is_no_done {
                    acc.no_done += 1;
                }
            }
        }
        by_key
            .into_iter()
            .filter(|(_, v)| v.n > 0)
            .map(|(k, v)| {
                let n = v.n as f64;
                (
                    k,
                    StatsEntry {
                        n: v.n,
                        success: v.success,
                        avg_tokens: (v.tokens as f64) / n,
                        avg_cost: v.cost / n,
                        avg_duration_ms: (v.duration_ms as f64) / n,
                        retry_rate: (v.retried as f64) / n,
                        no_done_rate: (v.no_done as f64) / n,
                    },
                )
            })
            .collect()
    }
}

pub fn empty_task_counts() -> BTreeMap<String, u32> {
    ["pending", "ready", "running", "succeeded", "failed", "blocked", "cancelled", "skipped"]
        .into_iter()
        .map(|k| (k.to_string(), 0))
        .collect()
}

pub fn task_status_str(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::Pending => "pending",
        TaskStatus::Ready => "ready",
        TaskStatus::Running => "running",
        TaskStatus::Succeeded => "succeeded",
        TaskStatus::Failed => "failed",
        TaskStatus::Blocked => "blocked",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Skipped => "skipped",
    }
}

fn validate_artifact_name(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > 200
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid artifact identifier"));
    }
    Ok(())
}

fn sanitize_worker_forked_from(worker: &mut Worker, prev_fork: Option<&str>) {
    let is_self = worker.forked_from.as_deref().is_some_and(|src| {
        let trimmed = src.trim();
        trimmed.is_empty() || trimmed == worker.id || worker.h2_agent.as_deref() == Some(trimmed)
    });
    if is_self {
        worker.forked_from = prev_fork
            .map(str::trim)
            .filter(|p| !p.is_empty() && *p != worker.id && worker.h2_agent.as_deref() != Some(*p))
            .map(String::from);
    }
}

/// Ensures path `p` stays inside directory `root`.
pub fn path_is_inside(root: &Path, candidate: &Path) -> bool {
    let Ok(r) = root.canonicalize() else { return false };
    let Ok(c) = candidate.canonicalize() else { return false };
    c.starts_with(&r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Arc<Store> {
        let dir = std::env::temp_dir().join(format!("herdr2-store-test-{}", crypto::rand_id("t")));
        let paths = DrovePaths::from_paths(&crate::paths::Paths {
            state: dir.clone(),
            config: dir.join("config.toml"),
            socket: dir.join("core.sock"),
            holder_socket: dir.join("core.holder.sock"),
        });
        Store::open(&paths).expect("open temp store")
    }

    #[test]
    fn touch_worker_active_throttles_workers_change_events() {
        let store = temp_store();
        let mut rx = store.subscribe();
        let w = Worker {
            id: "wrk_1".into(),
            run_id: "run_1".into(),
            agent: "codex".into(),
            model: None,
            context_window: None,
            effort: None,
            fast: None,
            session_key: "run_1:wrk_1".into(),
            h2_agent: Some("a5q".into()),
            cwd: "/tmp".into(),
            worktree: None,
            state: WorkerState::Busy,
            current_attempt_id: Some("att_1".into()),
            pid: None,
            fingerprint: Fingerprint::default(),
            context: WorkerContext::default(),
            totals: WorkerTotals::default(),
            created_at: now_iso(),
            last_active_at: now_iso(),
            role: Some("coder".into()),
            forked_from: Some("a5p".into()),
            protocol_gen: Some(1),
            read_only_dir: None,
        };
        store.insert_worker(w);
        let _ = rx.try_recv(); // drain insert event

        // Calling touch_worker_active 20 times in a burst must emit at most 1 workers ChangeEvent.
        for _ in 0..20 {
            store.touch_worker_active("wrk_1");
        }
        let mut emitted = 0usize;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, StoreEvent::Change(c) if c.table == "workers") {
                emitted += 1;
            }
        }
        assert_eq!(emitted, 1, "heartbeat touches must be throttled to prevent snapshot storms");
    }

    #[test]
    fn worker_forked_from_rejects_self_reference() {
        let store = temp_store();
        let w = Worker {
            id: "wrk_self".into(),
            run_id: "run_1".into(),
            agent: "codex".into(),
            model: None,
            context_window: None,
            effort: None,
            fast: None,
            session_key: "run_1:wrk_self".into(),
            h2_agent: Some("a5q".into()),
            cwd: "/tmp".into(),
            worktree: None,
            state: WorkerState::Busy,
            current_attempt_id: Some("att_1".into()),
            pid: None,
            fingerprint: Fingerprint::default(),
            context: WorkerContext::default(),
            totals: WorkerTotals::default(),
            created_at: now_iso(),
            last_active_at: now_iso(),
            role: Some("coder".into()),
            forked_from: Some("a5p".into()),
            protocol_gen: Some(1),
            read_only_dir: None,
        };
        store.insert_worker(w);
        let updated = store.update_worker("wrk_self", |upd| {
            upd.forked_from = Some("a5q".into());
        });
        assert_eq!(updated.unwrap().forked_from.as_deref(), Some("a5p"));
    }

    #[test]
    fn items_lru_evicts_cold_idle_workers_and_reloads_transparently() {
        let store = temp_store();
        store.insert_worker(Worker {
            id: "wrk_busy".into(),
            run_id: "run_1".into(),
            agent: "codex".into(),
            model: None,
            context_window: None,
            effort: None,
            fast: None,
            session_key: "run_1:wrk_busy".into(),
            h2_agent: Some("a1".into()),
            cwd: "/tmp".into(),
            worktree: None,
            state: WorkerState::Busy,
            current_attempt_id: Some("att_b".into()),
            pid: None,
            fingerprint: Fingerprint::default(),
            context: WorkerContext::default(),
            totals: WorkerTotals::default(),
            created_at: now_iso(),
            last_active_at: now_iso(),
            role: Some("coder".into()),
            forked_from: None,
            protocol_gen: Some(1),
            read_only_dir: None,
        });
        store.append_item("wrk_busy", Some("att_b".into()), "assistant", Some("busy-live".into()), None, None);

        for i in 0..30 {
            let wid = format!("wrk_idle_{i}");
            store.insert_worker(Worker {
                id: wid.clone(),
                run_id: "run_1".into(),
                agent: "codex".into(),
                model: None,
                context_window: None,
                effort: None,
                fast: None,
                session_key: format!("run_1:{wid}"),
                h2_agent: None,
                cwd: "/tmp".into(),
                worktree: None,
                state: WorkerState::Idle,
                current_attempt_id: None,
                pid: None,
                fingerprint: Fingerprint::default(),
                context: WorkerContext::default(),
                totals: WorkerTotals::default(),
                created_at: now_iso(),
                last_active_at: now_iso(),
                role: Some("coder".into()),
                forked_from: None,
                protocol_gen: Some(1),
                read_only_dir: None,
            });
            store.append_item(&wid, None, "assistant", Some(format!("msg-{i}")), None, None);
        }

        {
            let g = store.inner.lock().unwrap();
            assert!(
                g.items.len() <= MAX_CACHED_WORKER_ITEMS,
                "in-memory items cache exceeded cap: {}",
                g.items.len()
            );
            assert!(g.items.contains_key("wrk_busy"), "busy worker must remain pinned in memory cache");
            assert!(!g.items.contains_key("wrk_idle_0"), "oldest idle worker should have been evicted from RAM");
        }

        // Reading evicted worker transparently reloads from JSONL on disk.
        let reloaded = store.list_items("wrk_idle_0", None, None, 10);
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].text.as_deref(), Some("msg-0"));
    }
}
