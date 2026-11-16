//! One framed connection to herdr2 core (`h2`) through `h2 connect`: JSON-RPC on
//! channel 0 and streamed events / transcripts on subscribed channels.

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HEADER: usize = 9;
pub const DATA: u8 = 0;
pub const CREDIT: u8 = 1;
pub const CLOSE: u8 = 2;
pub const RESIZE: u8 = 3;
pub const FOCUS: u8 = 4;
pub const FACTS: u8 = 5;
pub const AUTOMATIC_INPUT: u8 = 6;

pub enum Stream {
    Data(Vec<u8>),
    /// The core ended the stream; the payload says why (e.g. the agent exited).
    Closed(Value),
    /// The connection to the core dropped.
    Lost,
}

/// Event streams are lines; DATA frame boundaries can fall anywhere in a line.
#[derive(Default)]
pub struct EventLines {
    pending: Vec<u8>,
    /// The last event applied: older ones were in the snapshot.
    seq: u64,
    /// Every agent the stream describes, in order.
    terminals: Vec<Value>,
    /// Each agent's highest `herdr.seen` / `herdr2.seen` among the label changes
    /// since the last `take_seen`.
    seen: HashMap<String, u64>,
    /// A fresh snapshot replaced the list since the last `take_seen`.
    resynced: bool,
}

/// What a push changed.
#[derive(Default)]
pub struct Pushed {
    /// A layout document changed, or events were missed: read them again.
    pub docs: bool,
    pub terminals: bool,
    /// Structured transcript entries arrived for an agent: `(address, entry_json)`.
    pub transcripts: Vec<(String, Value)>,
}

impl EventLines {
    /// From `events.subscribe`'s reply: its `seq` and `agents` (or `terminals`).
    pub fn new(snapshot: &Value) -> Self {
        let terminals = snapshot["agents"]
            .as_array()
            .or_else(|| snapshot["terminals"].as_array())
            .cloned()
            .unwrap_or_default();
        EventLines { seq: snapshot["seq"].as_u64().unwrap_or(0), terminals, resynced: true, ..Default::default() }
    }

    /// The `herdr.seen` values that went by, and whether a snapshot replaced everything.
    pub fn take_seen(&mut self) -> (HashMap<String, u64>, bool) {
        (std::mem::take(&mut self.seen), std::mem::take(&mut self.resynced))
    }

    fn note_seen(&mut self, address: &Value, labels: &Value) {
        let seen = labels["herdr.seen"]
            .as_str()
            .or_else(|| labels["herdr2.seen"].as_str())
            .and_then(|s| s.parse::<u64>().ok());
        let (Some(address), Some(seen)) = (address.as_str(), seen) else { return };
        let high = self.seen.entry(address.to_owned()).or_default();
        *high = (*high).max(seen);
    }

    /// The live agents, with both `terminals` (for mods) and `agents` keys.
    pub fn live(&self) -> Value {
        let live: Vec<&Value> = self.terminals.iter().filter(|t| t["state"] == "live").collect();
        json!({ "terminals": live, "agents": live })
    }

    /// Parent references from cores may still use host names. All app
    /// consumers need the same addresses as the agent records themselves.
    pub fn live_for(&self, hello: &Value, machines: &Value) -> Value {
        let mut live = self.live();
        for key in ["terminals", "agents"] {
            for terminal in live[key].as_array_mut().into_iter().flatten() {
                let Some(parent) = terminal["parent"].as_str() else { continue };
                let Some((prefix, id)) = parent.split_once('/') else { continue };
                let local = [hello["core"]["id"].as_str(), hello["core"]["name"].as_str()].contains(&Some(prefix));
                if local {
                    terminal["parent"] = id.into();
                    continue;
                }
                let known = machines["machines"].as_array().into_iter().flatten();
                let machine = known.clone().find(|m| m["name"] == prefix || m["id"] == prefix).or_else(|| {
                    let mut hosts = known.filter(|m| m["host"] == prefix);
                    match (hosts.next(), hosts.next()) {
                        (Some(machine), None) => Some(machine),
                        _ => None,
                    }
                });
                let Some(name) = machine.and_then(|m| m["name"].as_str()) else { continue };
                terminal["parent"] = format!("{name}/{id}").into();
            }
        }
        live
    }

    pub fn push(&mut self, bytes: &[u8]) -> Pushed {
        self.pending.extend_from_slice(bytes);
        let mut consumed = 0;
        let mut pushed = Pushed::default();
        let lines: Vec<Value> = self
            .pending
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .map(|(i, _)| {
                let line = serde_json::from_slice::<Value>(&self.pending[consumed..i]).unwrap_or_default();
                consumed = i + 1;
                line
            })
            .collect();
        self.pending.drain(..consumed);
        for line in lines {
            let p = &line["params"];
            match line["method"].as_str() {
                Some("missed") => {
                    self.terminals = p["agents"]
                        .as_array()
                        .or_else(|| p["terminals"].as_array())
                        .cloned()
                        .unwrap_or_default();
                    self.seq = p["seq"].as_u64().unwrap_or(self.seq);
                    self.resynced = true;
                    pushed.docs = true;
                    pushed.terminals = true;
                }
                Some("event") if p["seq"].as_u64().is_none_or(|s| s > self.seq) => {
                    self.seq = p["seq"].as_u64().unwrap_or(self.seq);
                    let kind = p["type"].as_str().unwrap_or_default();
                    if kind == "doc" {
                        pushed.docs = true;
                    } else if kind == "transcript" {
                        if let Some(addr) = p.get("agent").and_then(Value::as_str).or_else(|| p.get("terminal").and_then(Value::as_str)) {
                            pushed.transcripts.push((addr.to_owned(), p["data"].clone()));
                        }
                    } else {
                        pushed.terminals |= self.apply(p);
                    }
                }
                _ => {}
            }
        }
        pushed
    }

    /// One event's change to the agent it names.
    fn apply(&mut self, p: &Value) -> bool {
        let kind = p["type"].as_str().unwrap_or_default();
        let address = p.get("agent").filter(|v| !v.is_null()).unwrap_or(&p["terminal"]);
        let data = &p["data"];
        if matches!(kind, "created" | "restarted" | "labels") {
            self.note_seen(address, &data["labels"]);
        }
        let at = self.terminals.iter().position(|t| &t["address"] == address);
        let whole = match kind {
            "created" | "restarted" => Some(data),
            "labels" => data.get("record"),
            _ => None,
        };
        if let Some(record) = whole {
            match at {
                Some(i) => self.terminals[i] = record.clone(),
                None => self.terminals.push(record.clone()),
            }
            return true;
        }
        let Some(i) = at else { return false };
        if kind == "removed" {
            self.terminals.remove(i);
            return true;
        }
        let t = &mut self.terminals[i];
        let mut set = |keys: &[&str]| keys.iter().for_each(|k| t[*k] = data[*k].clone());
        match kind {
            "finished" | "lost" => {
                set(&["reason", "exit_code", "signal", "finished_ms"]);
                t["state"] = if kind == "lost" { "live" } else { "finished" }.into();
                if kind == "lost" {
                    t["pid"] = Value::Null;
                }
                t.as_object_mut().map(|r| r.remove("foreground"));
            }
            "status" => set(&["status", "records", "reported", "pending_permission", "inbox_len"]),
            "turn" => set(&["turn"]),
            "title" => set(&["title"]),
            "labels" => set(&["labels"]),
            "keep" => set(&["keep"]),
            "size" => set(&["cols", "rows"]),
            "foreground" => t["foreground"] = data.clone(),
            _ => return false,
        }
        true
    }
}

#[derive(Default)]
struct Routes {
    closed: bool,
    automatic_input: HashSet<u32>,
    replies: HashMap<u64, mpsc::Sender<Result<Value, Value>>>,
    streams: HashMap<u32, async_channel::Sender<Stream>>,
    /// Frames for a channel whose subscribe reply hasn't been handled yet.
    early: HashMap<u32, Vec<Stream>>,
}

/// The supported core API generation; a generation change requires a matching app update.
pub const GENERATION: u64 = 1;
/// What the app calls on herdr2 core (`h2`): a core without one of them can't run it.
pub const USES: &[&str] = &[
    "agent.run",
    "agent.list",
    "agent.get",
    "agent.kill",
    "agent.label",
    "agent.keep",
    "agent.read",
    "agent.send",
    "agent.interrupt",
    "agent.approve",
    "agent.deny",
    "agent.put",
    "doc.get",
    "doc.set",
    "events.subscribe",
];

#[derive(Debug)]
pub struct RpcError(pub Value);

impl RpcError {
    pub fn kind(&self) -> Option<&str> {
        self.0.pointer("/data/kind").and_then(Value::as_str)
    }
}
impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.get("message").and_then(Value::as_str).unwrap_or("core call failed"))?;
        if let Some(hint) = self.0.pointer("/data/hint").and_then(Value::as_str) {
            write!(f, " ({hint})")?;
        }
        Ok(())
    }
}
impl std::error::Error for RpcError {}

pub fn error_kind(error: &anyhow::Error) -> Option<&str> {
    error.downcast_ref::<RpcError>().and_then(RpcError::kind)
}

/// Why a running core can't serve this app.
#[derive(Debug, Clone, PartialEq)]
pub enum Mismatch {
    /// Older than the oldest core this app works with: update the core.
    TooOld { have: String, need: String },
    /// A newer generation than this app speaks: update the app.
    Newer { generation: u64 },
    /// It lacks methods the app calls (an unusual build): update the core.
    Missing(Vec<String>),
}

/// The oldest core this app works with (distribution/core-version).
pub fn core_min() -> &'static str {
    include_str!("../distribution/core-version").trim()
}

fn semver(v: &str) -> (u64, u64, u64) {
    let trimmed = v.trim();
    let stripped = trimmed.strip_prefix("h2 ").or_else(|| trimmed.strip_prefix("hc ")).unwrap_or(trimmed);
    let mut p = stripped.split(['.', '-']).map(|n| n.parse::<u64>().unwrap_or(0));
    (p.next().unwrap_or(0), p.next().unwrap_or(0), p.next().unwrap_or(0))
}

/// Whether a core's hello says it can serve this app.
pub fn check(hello: &Value) -> Result<(), Mismatch> {
    let generation = hello.get("generation").and_then(Value::as_u64).unwrap_or(0);
    if generation > GENERATION {
        return Err(Mismatch::Newer { generation });
    }
    let have = hello.get("version").and_then(Value::as_str).unwrap_or("0.0.0").to_owned();
    if generation < GENERATION || semver(&have) < semver(core_min()) {
        return Err(Mismatch::TooOld { have, need: core_min().to_owned() });
    }
    let methods: Vec<&str> = hello.get("methods").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    let missing: Vec<String> = USES.iter().filter(|m| !methods.contains(m)).map(|m| m.to_string()).collect();
    if missing.is_empty() { Ok(()) } else { Err(Mismatch::Missing(missing)) }
}

pub struct Core {
    /// What the core said when we connected (generation, version, methods).
    pub hello: Value,
    /// Frames for the writer thread: a core that stops reading never blocks the caller (the UI).
    writer: mpsc::Sender<Vec<u8>>,
    routes: Arc<Mutex<Routes>>,
    next_id: AtomicU64,
    child: Mutex<Child>,
    /// Closes when the connection to the core drops.
    lost: async_channel::Receiver<()>,
}

fn encode(channel: u32, kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.extend_from_slice(&channel.to_le_bytes());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

fn read_frame(r: &mut impl Read) -> Option<(u32, u8, Vec<u8>)> {
    let mut h = [0u8; HEADER];
    r.read_exact(&mut h).ok()?;
    let channel = u32::from_le_bytes(h[0..4].try_into().unwrap());
    let len = u32::from_le_bytes(h[5..9].try_into().unwrap()) as usize;
    let mut payload = vec![0; len];
    r.read_exact(&mut payload).ok()?;
    Some((channel, h[4], payload))
}

#[derive(Debug)]
pub struct ConnectError(pub String);

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ConnectError {}

fn connect_error(stderr: &str) -> Option<ConnectError> {
    let reply: Value = serde_json::from_str(stderr.trim()).ok()?;
    let error = reply.get("error")?;
    let message = error.get("message")?.as_str()?;
    Some(ConnectError(match error.get("hint").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(hint) => format!("{message}\n\n{hint}"),
        None => message.to_owned(),
    }))
}

impl Core {
    /// Connects to the user's herdr2 core (`h2`) and switches to framed mode.
    pub fn connect() -> Result<Arc<Core>> {
        let mut child = crate::spawn::command(crate::core_bin::path())
            .arg("connect")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot run `h2 connect`: is herdr2 core (h2) installed?")?;
        let mut stderr = child.stderr.take().unwrap();
        let (error_tx, errors) = mpsc::channel();
        std::thread::Builder::new().name("h2-stderr".into()).spawn(move || {
            let mut captured = Vec::new();
            let mut buffer = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                let _ = std::io::stderr().write_all(&buffer[..n]);
                captured.extend_from_slice(&buffer[..n.min(65536 - captured.len())]);
            }
            let _ = error_tx.send(String::from_utf8_lossy(&captured).into_owned());
        })?;
        let mut stdin = child.stdin.take().unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let hello = (|| -> Result<Value> {
            stdin.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"hello\",\"params\":{\"framed\":true}}\n")?;
            let mut line = String::new();
            reader.read_line(&mut line)?;
            serde_json::from_str(&line).context("bad hello reply from core")
        })();
        let hello = match hello {
            Ok(hello) => hello,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                if let Some(error) = connect_error(&errors.recv_timeout(Duration::from_secs(1)).unwrap_or_default()) {
                    return Err(error.into());
                }
                return Err(error);
            }
        };
        if let Some(err) = hello.get("error") {
            bail!("core refused hello: {err}");
        }

        let (writer, frames) = mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new().name("h2-writer".into()).spawn(move || {
            while let Ok(frame) = frames.recv() {
                if stdin.write_all(&frame).is_err() {
                    break;
                }
            }
        })?;

        let routes = Arc::new(Mutex::new(Routes::default()));
        let (lost_tx, lost) = async_channel::bounded::<()>(1);
        let core = Arc::new(Core {
            hello: hello.get("result").cloned().unwrap_or(Value::Null),
            writer,
            routes: routes.clone(),
            next_id: AtomicU64::new(1),
            child: Mutex::new(child),
            lost,
        });
        std::thread::Builder::new().name("h2-reader".into()).spawn(move || {
            let _lost_tx = lost_tx;
            while let Some((channel, kind, payload)) = read_frame(&mut reader) {
                let mut r = routes.lock().unwrap();
                if channel == 0 {
                    let Ok(msg) = serde_json::from_slice::<Value>(&payload) else { continue };
                    let Some(id) = msg.get("id").and_then(Value::as_u64) else { continue };
                    if let Some(tx) = r.replies.remove(&id) {
                        let _ = tx.send(match msg.get("error") {
                            Some(e) => Err(e.clone()),
                            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                        });
                    }
                    continue;
                }
                let item = match kind {
                    DATA => Stream::Data(payload),
                    CLOSE => Stream::Closed(serde_json::from_slice(&payload).unwrap_or(Value::Null)),
                    _ => continue,
                };
                let closing = matches!(item, Stream::Closed(_));
                match r.streams.get(&channel) {
                    Some(tx) => {
                        let _ = tx.try_send(item);
                    }
                    None => r.early.entry(channel).or_default().push(item),
                }
                if closing {
                    r.automatic_input.remove(&channel);
                    r.streams.remove(&channel);
                }
            }
            let mut r = routes.lock().unwrap();
            r.closed = true;
            r.automatic_input.clear();
            for (_, tx) in r.streams.drain() {
                let _ = tx.try_send(Stream::Lost);
            }
            for (_, tx) in r.replies.drain() {
                let _ = tx.send(Err(json!({"message": "lost the core"})));
            }
        })?;
        Ok(core)
    }

    /// Resolves once the connection to the core is gone.
    pub async fn closed(&self) {
        let _ = self.lost.recv().await;
    }

    pub fn send(&self, channel: u32, kind: u8, payload: &[u8]) {
        let _ = self.writer.send(encode(channel, kind, payload));
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_timeout(method, params, Duration::from_secs(10))
    }

    pub fn call_timeout(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        {
            let mut routes = self.routes.lock().unwrap();
            if routes.closed {
                bail!("{method}: lost the core");
            }
            routes.replies.insert(id, tx);
        }
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.send(0, DATA, req.to_string().as_bytes());
        match rx.recv_timeout(timeout) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(anyhow::Error::new(RpcError(e)).context(method.to_owned())),
            Err(_) => {
                self.routes.lock().unwrap().replies.remove(&id);
                Err(anyhow!("{method}: no reply from core"))
            }
        }
    }

    /// Starts a headless ACP agent and returns its address (`aN` or `machine/aN`).
    pub fn run(&self, argv: Vec<String>, prompt: Option<String>, cwd: Option<String>, labels: Value, on: Option<&str>) -> Result<String> {
        let mut params = json!({
            "argv": argv,
            "cwd": cwd,
            "labels": labels,
            "on": on,
            "keep": true,
        });
        if let Some(prompt) = prompt.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) {
            params["prompt"] = json!(prompt);
        }
        let r = self.call("agent.run", params)?;
        r.get("address")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("agent.run returned no address: {r}"))
    }

    pub fn get_agent(&self, agent: &str) -> Result<Value> {
        self.call("agent.get", json!({ "agent": agent }))
    }

    pub fn read_agent(&self, agent: &str) -> Result<Value> {
        self.call("agent.read", json!({ "agent": agent }))
    }

    pub fn send_prompt(&self, agent: &str, text: &str, steer: bool) -> Result<Value> {
        self.call("agent.send", json!({ "agent": agent, "text": text, "steer": steer }))
    }

    pub fn interrupt_agent(&self, agent: &str) -> Result<Value> {
        self.call("agent.interrupt", json!({ "agent": agent }))
    }

    pub fn approve_agent(&self, agent: &str, option: Option<&str>) -> Result<Value> {
        let mut params = json!({ "agent": agent });
        if let Some(opt) = option {
            params["option"] = json!(opt);
        }
        self.call("agent.approve", params)
    }

    pub fn deny_agent(&self, agent: &str) -> Result<Value> {
        self.call("agent.deny", json!({ "agent": agent }))
    }

    pub fn put_file(&self, agent: &str, name: &str, data_base64: &str) -> Result<String> {
        let r = self.call("agent.put", json!({ "agent": agent, "name": name, "data": data_base64 }))?;
        r.get("path")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("agent.put returned no path: {r}"))
    }

    /// One subscription includes local and forwarded remote events.
    pub fn subscribe(&self) -> Result<(u32, async_channel::Receiver<Stream>, Value)> {
        let result = self.call("events.subscribe", json!({}))?;
        let channel = result["channel"].as_u64().ok_or_else(|| anyhow!("subscription returned no channel"))? as u32;
        let (tx, rx) = async_channel::unbounded();
        let mut r = self.routes.lock().unwrap();
        for item in r.early.remove(&channel).unwrap_or_default() {
            let _ = tx.try_send(item);
        }
        r.streams.insert(channel, tx);
        drop(r);
        self.credit(channel, 1024 * 1024);
        Ok((channel, rx, result))
    }

    pub fn credit(&self, channel: u32, n: usize) {
        self.send(channel, CREDIT, &(n as u32).to_le_bytes());
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A core that is never there, for tests that open the window: every call
/// fails at once, and it never reports the loss (so nothing reconnects).
#[cfg(test)]
pub fn offline() -> Arc<Core> {
    let (writer, frames) = mpsc::channel();
    std::mem::forget(frames);
    let (lost_tx, lost) = async_channel::bounded(1);
    std::mem::forget(lost_tx);
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Arc::new(Core {
        hello: Value::Null,
        writer,
        routes: Arc::new(Mutex::new(Routes { closed: true, ..Default::default() })),
        next_id: AtomicU64::new(1),
        child: Mutex::new(child),
        lost,
    })
}

/// A core that answers every call at once: `agent.run` with a new address,
/// whose argv it passes on to `runs`; anything else with `{}`.
#[cfg(test)]
pub fn answering() -> (Arc<Core>, mpsc::Receiver<Value>) {
    let (writer, frames) = mpsc::channel::<Vec<u8>>();
    let (runs_tx, runs) = mpsc::channel();
    let (lost_tx, lost) = async_channel::bounded(1);
    std::mem::forget(lost_tx);
    let routes = Arc::new(Mutex::new(Routes::default()));
    let replies = routes.clone();
    std::thread::spawn(move || {
        for (n, frame) in frames.into_iter().enumerate() {
            let Some(Ok(req)) = frame.get(HEADER..).map(serde_json::from_slice::<Value>) else { continue };
            let result = if req["method"] == "agent.run" {
                let _ = runs_tx.send(req["params"]["argv"].clone());
                json!({ "address": format!("a{}", 100 + n) })
            } else {
                json!({})
            };
            let id = req["id"].as_u64().unwrap_or_default();
            if let Some(tx) = replies.lock().unwrap().replies.remove(&id) {
                let _ = tx.send(Ok(result));
            }
        }
    });
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let core = Arc::new(Core { hello: Value::Null, writer, routes, next_id: AtomicU64::new(1), child: Mutex::new(child), lost });
    (core, runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_errors_keep_the_cores_message_and_recovery_hint() {
        let stderr = json!({"error": {
            "kind": "unavailable", "message": "the core pipe cannot be accessed",
            "hint": "stop it with h2 server stop in an elevated terminal"
        }})
        .to_string();
        assert_eq!(connect_error(&stderr).unwrap().to_string(), "the core pipe cannot be accessed\n\nstop it with h2 server stop in an elevated terminal");
        assert!(connect_error("not JSON").is_none());
    }

    #[test]
    fn the_event_stream_keeps_the_agents_and_transcripts_as_the_core_has_them() {
        let event = |seq: u64, kind: &str, a: &str, data: Value| {
            format!("{}\n", json!({"method": "event", "params": {"seq": seq, "type": kind, "agent": a, "data": data}}))
        };
        let rec = |a: &str| json!({"address": a, "state": "live", "title": "", "labels": {}, "status": null});
        let mut lines = EventLines::new(&json!({"seq": 5, "agents": [rec("a1")]}));
        let addresses = |l: &EventLines| l.live()["terminals"].as_array().unwrap().iter().map(|t| t["address"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        assert_eq!(addresses(&lines), ["a1"]);
        assert!(!lines.push(event(5, "title", "a1", json!({"title": "old"})).as_bytes()).terminals, "not newer than the snapshot");
        let got = lines.push(format!("{}{}", event(6, "created", "a2", rec("a2")), event(7, "title", "a1", json!({"title": "hi"}))).as_bytes());
        assert!(got.terminals && !got.docs);
        assert_eq!(addresses(&lines), ["a1", "a2"]);
        assert_eq!(lines.live()["terminals"][0]["title"], "hi");
        lines.push(event(8, "status", "a2", json!({"status": {"state": "blocked"}, "records": {}, "reported": true, "inbox_len": 2, "pending_permission": {"request_id": "r1", "session_id": "s1", "title": "Run shell", "options": []}})).as_bytes());
        assert_eq!(lines.live()["agents"][1]["status"]["state"], "blocked");
        assert_eq!(lines.live()["agents"][1]["inbox_len"], 2);
        assert_eq!(lines.live()["agents"][1]["pending_permission"]["title"], "Run shell");
        let got_tx = lines.push(event(9, "transcript", "a2", json!({"kind": "assistant", "turn": 1, "text": "hello", "ts_ms": 1})).as_bytes());
        assert_eq!(got_tx.transcripts, vec![("a2".to_owned(), json!({"kind": "assistant", "turn": 1, "text": "hello", "ts_ms": 1}))]);
        lines.push(event(10, "finished", "a1", json!({"reason": "exited", "exit_code": 0, "signal": null, "finished_ms": 1})).as_bytes());
        assert_eq!(addresses(&lines), ["a2"], "finished: no longer live");
        lines.push(event(11, "removed", "a2", json!({})).as_bytes());
        assert!(addresses(&lines).is_empty());
        let missed = format!("{}\n", json!({"method": "missed", "params": {"seq": 20, "agents": [rec("a9")]}}));
        let got = lines.push(missed.as_bytes());
        assert!(got.terminals && got.docs, "missed: everything again");
        assert_eq!(addresses(&lines), ["a9"]);
        assert!(lines.push(format!("{}\n", json!({"method": "event", "params": {"seq": 21, "type": "doc"}})).as_bytes()).docs);
    }

    #[test]
    fn a_seen_write_stays_known_under_a_later_lower_one() {
        let labels = |seq: u64, seen: &str| {
            format!("{}\n", json!({"method": "event", "params": {"seq": seq, "type": "labels", "agent": "a1", "data": {"labels": {"herdr.seen": seen}}}}))
        };
        let mut lines = EventLines::new(&json!({"seq": 1, "agents": [{"address": "a1", "state": "live", "labels": {}}]}));
        assert_eq!(lines.take_seen(), (HashMap::new(), true), "a snapshot: can't tell");
        lines.push(format!("{}{}", labels(2, "5"), labels(3, "4")).as_bytes());
        assert_eq!(lines.live()["terminals"][0]["labels"]["herdr.seen"], "4");
        assert_eq!(lines.take_seen(), (HashMap::from([("a1".to_owned(), 5)]), false));
        assert_eq!(lines.take_seen(), (HashMap::new(), false));
        lines.push(format!("{}\n", json!({"method": "missed", "params": {"seq": 9, "agents": []}})).as_bytes());
        assert!(lines.take_seen().1, "missed: can't tell");
    }

    #[test]
    fn parent_addresses_match_the_viewing_cores_agent_addresses() {
        let lines = EventLines::new(&json!({"agents": [
            {"address": "beta/a2", "state": "live", "parent": "host-local/a1"},
            {"address": "beta/a3", "state": "live", "parent": "host-alpha/a4"},
            {"address": "beta/a6", "state": "live", "parent": "a7"},
            {"address": "a10", "state": "live", "parent": "alpha-id/a4"},
            {"address": "a11", "state": "live", "parent": "local-id/a1"},
            {"address": "a12", "state": "live", "parent": "alpha/a4"},
            {"address": "a13", "state": "live", "parent": "unknown/a1"},
            {"address": "a14", "state": "live", "parent": null},
            {"address": "a15", "state": "live", "parent": "shared-host/a4"},
            {"address": "a16", "state": "live", "parent": "beta/a4"}
        ]}));
        let hello = json!({"core": {"id": "local-id", "name": "host-local"}});
        let machines = json!({"machines": [
            {"name": "alpha", "id": "alpha-id", "host": "host-alpha"},
            {"name": "beta", "id": "beta-id", "host": "host-beta"},
            {"name": "gamma", "id": "gamma-id", "host": "shared-host"},
            {"name": "delta", "id": "delta-id", "host": "shared-host"},
            {"name": "epsilon", "id": "epsilon-id", "host": "beta"}
        ]});
        let parents = |machines: &Value| {
            lines.live_for(&hello, machines)["terminals"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["parent"].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            parents(&machines),
            vec![
                json!("a1"),
                json!("alpha/a4"),
                json!("a7"),
                json!("alpha/a4"),
                json!("a1"),
                json!("alpha/a4"),
                json!("unknown/a1"),
                Value::Null,
                json!("shared-host/a4"),
                json!("beta/a4"),
            ]
        );
    }

    #[test]
    fn document_and_missed_events_survive_arbitrary_frame_boundaries() {
        for line in [b"{\"method\":\"event\",\"params\":{\"type\":\"doc\"}}\n".as_slice(), b"{\"method\":\"missed\",\"params\":{\"agents\":[]}}\n".as_slice()] {
            for at in 1..line.len() {
                let mut lines = EventLines::default();
                assert!(!lines.push(&line[..at]).docs);
                assert!(lines.push(&line[at..]).docs);
                assert!(lines.pending.is_empty());
            }
            let mut lines = EventLines::default();
            let mut refresh = false;
            for chunk in line.chunks(3) {
                refresh |= lines.push(chunk).docs;
            }
            assert!(refresh);
        }
        let mut old = EventLines::default();
        old.push(b"{\"method\":\"mis");
        let mut replacement = EventLines::default();
        assert!(replacement.push(b"{\"method\":\"missed\"}\n").docs);
    }

    #[test]
    fn calls_after_disconnect_fail_without_waiting_for_a_reply() {
        let (writer, _frames) = mpsc::channel();
        let (_lost_tx, lost) = async_channel::bounded(1);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let core = Core {
            hello: Value::Null,
            writer,
            routes: Arc::new(Mutex::new(Routes { closed: true, ..Default::default() })),
            next_id: AtomicU64::new(1),
            child: Mutex::new(child),
            lost,
        };
        let start = std::time::Instant::now();
        assert!(core.call("doc.get", json!({"name":"herdr2-app"})).unwrap_err().to_string().contains("lost the core"));
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(core.routes.lock().unwrap().replies.is_empty());
    }

    #[test]
    fn run_sends_agent_run_without_pty_fields() {
        let (writer, frames) = mpsc::channel();
        let (_lost_tx, lost) = async_channel::bounded(1);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let core = Core {
            hello: Value::Null,
            writer,
            routes: Arc::new(Mutex::new(Routes::default())),
            next_id: AtomicU64::new(1),
            child: Mutex::new(child),
            lost,
        };
        let routes = core.routes.clone();
        let peer = std::thread::spawn(move || {
            let frame = frames.recv().unwrap();
            let request: Value = serde_json::from_slice(&frame[HEADER..]).unwrap();
            assert_eq!(request["method"], "agent.run");
            assert_eq!(request["params"]["argv"], json!(["claude"]));
            assert_eq!(request["params"]["prompt"], json!("fix tests"));
            assert_eq!(request["params"]["keep"], json!(true));
            assert!(request["params"].get("size").is_none());
            assert!(request["params"].get("display").is_none());
            assert!(request["params"].get("env").is_none());
            routes.lock().unwrap().replies.remove(&request["id"].as_u64().unwrap()).unwrap().send(Ok(json!({"address": "a1"}))).unwrap();
        });
        assert_eq!(core.run(vec!["claude".into()], Some("fix tests".into()), None, json!({}), None).unwrap(), "a1");
        peer.join().unwrap();
    }

    #[test]
    fn agent_rpcs_send_exact_field_names() {
        let (writer, frames) = mpsc::channel();
        let (_lost_tx, lost) = async_channel::bounded(1);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let core = Core {
            hello: Value::Null,
            writer,
            routes: Arc::new(Mutex::new(Routes::default())),
            next_id: AtomicU64::new(1),
            child: Mutex::new(child),
            lost,
        };
        let routes = core.routes.clone();
        let peer = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..4 {
                let frame = frames.recv().unwrap();
                let req: Value = serde_json::from_slice(&frame[HEADER..]).unwrap();
                seen.push((req["method"].clone(), req["params"].clone()));
                let reply = if req["method"] == "agent.put" { json!({"path": "/tmp/paste.png", "bytes": 4}) } else { json!({"ok": true}) };
                routes.lock().unwrap().replies.remove(&req["id"].as_u64().unwrap()).unwrap().send(Ok(reply)).unwrap();
            }
            seen
        });
        core.send_prompt("a1", "hello", true).unwrap();
        core.approve_agent("a1", Some("allow_once")).unwrap();
        core.deny_agent("a1").unwrap();
        let path = core.put_file("a1", "paste.png", "AAAA").unwrap();
        assert_eq!(path, "/tmp/paste.png");
        let sent = peer.join().unwrap();
        assert_eq!(sent[0], (json!("agent.send"), json!({"agent": "a1", "text": "hello", "steer": true})));
        assert_eq!(sent[1], (json!("agent.approve"), json!({"agent": "a1", "option": "allow_once"})));
        assert_eq!(sent[2], (json!("agent.deny"), json!({"agent": "a1"})));
        assert_eq!(sent[3], (json!("agent.put"), json!({"agent": "a1", "name": "paste.png", "data": "AAAA"})));
    }

    fn hello(generation: u64, version: &str, methods: &[&str]) -> Value {
        json!({ "generation": generation, "version": version, "methods": methods })
    }

    #[test]
    fn a_core_serves_the_app_by_generation_version_and_methods() {
        assert_eq!(check(&hello(1, "9.9.9", USES)), Ok(()));
        assert_eq!(check(&hello(1, "0.0.1", USES)), Err(Mismatch::TooOld { have: "0.0.1".into(), need: core_min().into() }));
        assert_eq!(check(&hello(2, "9.9.9", USES)), Err(Mismatch::Newer { generation: 2 }));
        assert_eq!(check(&hello(1, "9.9.9", &["agent.run"])), Err(Mismatch::Missing(USES[1..].iter().map(|m| m.to_string()).collect())));
        assert!(semver("h2 0.10.0") > semver("h2 0.9.3"));
    }
}
