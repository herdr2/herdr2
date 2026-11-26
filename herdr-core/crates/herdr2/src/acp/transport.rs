//! Asynchronous stdio JSON-RPC 2.0 transport for one ACP child process.

use crate::platform::{self, AgentGuard};
use crate::rpc::RpcError;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};

const MAX_STDERR_LINES: usize = 64;
const MAX_STDERR_LINE_BYTES: usize = 512;

#[derive(Debug, Clone)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

#[derive(Debug)]
pub enum CallError {
    Rpc(JsonRpcError),
    Io(String),
    Timeout,
    Closed,
}

impl CallError {
    pub fn into_rpc_error(self, context: &str, stderr: &str) -> RpcError {
        let base = match self {
            Self::Rpc(e) => {
                let detail = e
                    .data
                    .as_ref()
                    .and_then(|d| d.as_str().or_else(|| d.get("message").and_then(Value::as_str)))
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case(&e.message));
                match detail {
                    Some(d) => format!("{context}: {}: {d} (code {})", e.message, e.code),
                    None => format!("{context}: {} (code {})", e.message, e.code),
                }
            }
            Self::Io(e) => format!("{context}: {e}"),
            Self::Timeout => format!("{context}: timed out waiting for ACP response"),
            Self::Closed => format!("{context}: agent closed stdio before replying"),
        };
        let tail = stderr.trim();
        let message = if tail.is_empty() { base } else { format!("{base}\nstderr:\n{tail}") };
        RpcError::new("spawn_failed", message, "")
    }

    pub fn is_invalid_params(&self) -> bool {
        matches!(self, Self::Rpc(e) if e.code == -32602)
    }

    pub fn is_method_not_found(&self) -> bool {
        matches!(self, Self::Rpc(e) if e.code == -32601)
    }
}

#[derive(Debug)]
pub enum Inbound {
    Update(Value),
    Permission { rpc_id: Value, params: Value },
    Stderr(String),
    Closed,
}

struct Inner {
    pid: Option<u32>,
    guard: AgentGuard,
    write_tx: mpsc::UnboundedSender<String>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, JsonRpcError>>>>,
    stderr_tail: Mutex<VecDeque<String>>,
}

#[derive(Clone)]
pub struct Transport {
    inner: Arc<Inner>,
}

pub struct ChildWaiter {
    child: Child,
    inner: Arc<Inner>,
    reaped: bool,
    stderr_task: tokio::task::JoinHandle<()>,
    writer_task: tokio::task::JoinHandle<()>,
    stdout_task: tokio::task::JoinHandle<()>,
}

impl ChildWaiter {
    pub async fn wait(mut self) -> std::io::Result<ExitStatus> {
        let status = self.child.wait().await;
        self.reaped = true;
        let _ = tokio::time::timeout(Duration::from_millis(250), &mut self.stderr_task).await;
        let _ = tokio::time::timeout(Duration::from_millis(250), &mut self.stdout_task).await;
        self.writer_task.abort();
        status
    }
}

impl Drop for ChildWaiter {
    fn drop(&mut self) {
        if !self.reaped {
            if let Some(pid) = self.inner.pid {
                platform::signal_agent(pid, platform::signal::KILL);
            }
            self.inner.guard.terminate();
        }
        self.stderr_task.abort();
        self.stdout_task.abort();
        self.writer_task.abort();
    }
}

impl Transport {
    pub fn spawn(
        argv: &[String],
        env: &BTreeMap<String, String>,
        cwd: &str,
    ) -> Result<(Self, ChildWaiter, mpsc::UnboundedReceiver<Inbound>), RpcError> {
        let (mut child, guard) = platform::spawn_agent(argv, env, cwd).map_err(|e| {
            let program = argv.first().map_or("", String::as_str);
            let msg = if !std::path::Path::new(cwd).is_dir() {
                format!("cannot start {program}: missing working directory {cwd}: {e}")
            } else {
                format!("cannot start {program}: {e}")
            };
            RpcError::new("spawn_failed", msg, "")
        })?;
        let pid = child.id();
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<String>();
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<Inbound>();

        let inner = Arc::new(Inner {
            pid,
            guard,
            write_tx: write_tx.clone(),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            stderr_tail: Mutex::new(VecDeque::with_capacity(MAX_STDERR_LINES)),
        });

        let writer_task = tokio::spawn(async move {
            while let Some(line) = write_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        let inner_out = inner.clone();
        let tx_out = inbound_tx.clone();
        let stdout_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = platform::decode_process_output(&buf);
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Ok(msg) = serde_json::from_str::<Value>(trimmed) {
                            handle_message(&inner_out, &tx_out, msg);
                        }
                    }
                }
            }
            fail_all_pending(&inner_out);
            let _ = tx_out.send(Inbound::Closed);
        });

        let inner_err = inner.clone();
        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr.take(4 << 20));
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = platform::decode_process_output(&buf);
                        let mut clean = line.trim_end_matches(['\r', '\n']).to_string();
                        if clean.len() > MAX_STDERR_LINE_BYTES {
                            let mut end = MAX_STDERR_LINE_BYTES;
                            while !clean.is_char_boundary(end) {
                                end -= 1;
                            }
                            clean.truncate(end);
                            clean.push_str("...");
                        }
                        if clean.is_empty() {
                            continue;
                        }
                        {
                            let mut tail = inner_err.stderr_tail.lock().unwrap_or_else(|p| p.into_inner());
                            if tail.len() >= MAX_STDERR_LINES {
                                tail.pop_front();
                            }
                            tail.push_back(clean.clone());
                        }
                        let _ = inbound_tx.send(Inbound::Stderr(clean));
                    }
                }
            }
        });

        Ok((
            Transport { inner: inner.clone() },
            ChildWaiter { child, inner, reaped: false, stderr_task, writer_task, stdout_task },
            inbound_rx,
        ))
    }

    pub fn pid(&self) -> Option<u32> {
        self.inner.pid
    }

    pub fn recent_stderr(&self) -> String {
        self.inner.stderr_tail.lock().unwrap_or_else(|p| p.into_inner()).iter().cloned().collect::<Vec<_>>().join("\n")
    }

    pub async fn request<P: Serialize>(&self, method: &str, params: &P, timeout: Duration) -> Result<Value, CallError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let payload = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .map_err(|e| CallError::Io(e.to_string()))?;

        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap_or_else(|p| p.into_inner()).insert(id, tx);

        if self.inner.write_tx.send(payload).is_err() {
            self.inner.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
            return Err(CallError::Closed);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(val))) => Ok(val),
            Ok(Ok(Err(err))) => Err(CallError::Rpc(err)),
            Ok(Err(_)) => Err(CallError::Closed),
            Err(_) => {
                self.inner.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
                Err(CallError::Timeout)
            }
        }
    }

    pub fn notify<P: Serialize>(&self, method: &str, params: &P) -> Result<(), CallError> {
        let payload = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .map_err(|e| CallError::Io(e.to_string()))?;
        self.inner.write_tx.send(payload).map_err(|_| CallError::Closed)
    }

    pub fn reply_result(&self, id: Value, result: Value) {
        if let Ok(line) = serde_json::to_string(&json!({ "jsonrpc": "2.0", "id": id, "result": result })) {
            let _ = self.inner.write_tx.send(line);
        }
    }

    pub fn reply_error(&self, id: Value, code: i64, message: &str) {
        if let Ok(line) = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        })) {
            let _ = self.inner.write_tx.send(line);
        }
    }

    pub fn signal(&self, signal: i32) {
        if let Some(pid) = self.inner.pid {
            platform::signal_agent(pid, signal);
        }
        if signal == platform::signal::KILL {
            self.inner.guard.terminate();
        }
    }

    pub fn kill(&self) {
        self.signal(platform::signal::KILL);
    }
}

fn handle_message(inner: &Inner, inbound_tx: &mpsc::UnboundedSender<Inbound>, msg: Value) {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").cloned();

    match (method, id) {
        // Response to an outbound request
        (None, Some(ref id_val)) => {
            if let Some(num) = id_val.as_u64()
                && let Some(tx) = inner.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&num)
            {
                if let Some(err) = msg.get("error") {
                    let rpc_err = JsonRpcError {
                        code: err.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                        message: err.get("message").and_then(Value::as_str).unwrap_or("ACP error").into(),
                        data: err.get("data").cloned(),
                    };
                    let _ = tx.send(Err(rpc_err));
                } else {
                    let _ = tx.send(Ok(msg.get("result").cloned().unwrap_or(Value::Null)));
                }
            }
        }
        // Request from agent
        (Some(m), Some(rpc_id)) => {
            if m == "session/request_permission" {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                let _ = inbound_tx.send(Inbound::Permission { rpc_id, params });
            } else {
                if let Ok(line) = serde_json::to_string(&json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id,
                    "error": {
                        "code": -32601,
                        "message": format!("client method {m:?} is disabled (headless orchestration)")
                    }
                })) {
                    let _ = inner.write_tx.send(line);
                }
            }
        }
        // Notification from agent
        (Some(m), None) => {
            if m == "session/update" {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                let _ = inbound_tx.send(Inbound::Update(params));
            }
        }
        (None, None) => {}
    }
}

fn fail_all_pending(inner: &Inner) {
    let mut guard = inner.pending.lock().unwrap_or_else(|p| p.into_inner());
    for (_, tx) in guard.drain() {
        let _ = tx.send(Err(JsonRpcError { code: -32000, message: "agent stdio closed".into(), data: None }));
    }
}
