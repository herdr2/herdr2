//! Thread-safe Drove store backed by atomic `state.json` and append-only `items/<worker>.jsonl`.

use super::config::DrovePaths;
use super::crypto;
use super::types::*;
use crate::paths::write_atomic;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, broadcast};

const MAX_DAEMON_EVENTS: usize = 5000;

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
    daemon_events: Vec<DaemonEvent>,
}

struct StoreInner {
    data: PersistedState,
    items: HashMap<String, Vec<Item>>,
    state_file: PathBuf,
    items_dir: PathBuf,
    artifacts_dir: PathBuf,
    dirty: bool,
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

    fn load_worker_items(&mut self, worker_id: &str) -> &[Item] {
        if !self.items.contains_key(worker_id) {
            let path = self.items_dir.join(format!("{worker_id}.jsonl"));
            let mut loaded = Vec::new();
            if let Ok(file) = File::open(path) {
                for line in BufReader::new(file).lines().map_while(Result::ok) {
                    if let Ok(item) = serde_json::from_str::<Item>(&line) {
                        loaded.push(item);
                    }
                }
            }
            self.items.insert(worker_id.to_string(), loaded);
        }
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
    pub changed: Arc<Notify>,
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
                state_file: paths.state_file.clone(),
                items_dir: paths.items_dir.clone(),
                artifacts_dir: paths.artifacts_dir.clone(),
                dirty: false,
            }),
            events,
            changed: Arc::new(Notify::new()),
        }))
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
            g.data.daemon_events.retain(|e| e.run_id.as_deref() != Some(id));
            for wid in &worker_ids {
                g.items.remove(wid);
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
            candidate_result: None,
            cache: input.cache,
            cached_from: input.cached_from,
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
        let mut out: Vec<Worker> = g
            .data
            .workers
            .values()
            .filter(|w| run_id.is_none_or(|r| w.run_id == r))
            .cloned()
            .collect();
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

    pub fn insert_worker(&self, worker: Worker) -> Worker {
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
        let updated = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let worker = g.data.workers.get_mut(id)?;
            f(worker);
            worker.last_active_at = now_iso();
            let cloned = worker.clone();
            g.dirty = true;
            let _ = g.flush_state();
            cloned
        };
        self.emit_change("workers", Value::String(updated.id.clone()), Some(updated.run_id.clone()));
        Some(updated)
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
        let updated = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let attempt = g.data.attempts.get_mut(id)?;
            f(attempt);
            let cloned = attempt.clone();
            g.dirty = true;
            let _ = g.flush_state();
            cloned
        };
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

    pub fn append_item(&self, worker_id: &str, attempt_id: Option<String>, kind: &str, text: Option<String>, tool: Option<ItemTool>, usage: Option<ItemUsage>) -> Item {
        let (item, run_id) = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.data.next_item_id += 1;
            let id = g.data.next_item_id;
            let run_id = g.data.workers.get(worker_id).map(|w| w.run_id.clone());
            let item = Item { id, worker_id: worker_id.into(), attempt_id, ts: now_iso(), kind: kind.into(), text, tool, usage };
            let _ = g.load_worker_items(worker_id);
            g.items.entry(worker_id.to_string()).or_default().push(item.clone());
            let _ = g.append_item_disk(worker_id, &item);
            g.dirty = true;
            let _ = g.flush_state();
            (item, run_id)
        };
        let _ = self.events.send(StoreEvent::Item { run_id, item: item.clone() });
        item
    }

    pub fn list_items(&self, worker_id: &str, attempt_id: Option<&str>, before: Option<u64>, limit: usize) -> Vec<Item> {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let all = g.load_worker_items(worker_id);
        let filtered: Vec<Item> = all
            .iter()
            .filter(|it| attempt_id.is_none_or(|aid| it.attempt_id.as_deref() == Some(aid)))
            .filter(|it| before.is_none_or(|b| it.id < b))
            .cloned()
            .collect();
        if filtered.len() > limit {
            filtered[filtered.len() - limit..].to_vec()
        } else {
            filtered
        }
    }

    // --- Daemon Events ------------------------------------------------------

    pub fn append_daemon_event(&self, ev: NewDaemonEvent) -> DaemonEvent {
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
            let _ = g.flush_state();
            event
        };
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

/// Ensures path `p` stays inside directory `root`.
pub fn path_is_inside(root: &Path, candidate: &Path) -> bool {
    let Ok(r) = root.canonicalize() else { return false };
    let Ok(c) = candidate.canonicalize() else { return false };
    c.starts_with(&r)
}
