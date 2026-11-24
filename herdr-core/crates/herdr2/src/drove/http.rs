//! Local HTTP/1.1 + SSE server for Drove Web (`127.0.0.1:7799`) with SSH proxy federation.

use super::DroveRuntime;
use super::config::{read_ai_key, web_agents_payload};
use super::crypto;
use super::routes::*;
use super::store::StoreEvent;
use super::types::*;
use crate::remote::{self, LineState};
use crate::rpc::RpcError;
use crate::runtime::Core;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const MAX_BODY_BYTES: usize = 1024 * 1024;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const FEDERATION_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct HttpProxyRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct HttpProxyResponse {
    pub status: u16,
    pub body: Value,
}

#[derive(Debug)]
pub(super) struct ApiErr {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl ApiErr {
    pub fn new(status: u16, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { status, code: code.into(), message: message.into() }
    }
    pub fn into_json_value(self) -> (u16, Value) {
        (self.status, json!({ "error": self.code, "message": self.message }))
    }
}

/// Binds the localhost HTTP/SSE server (`127.0.0.1:<H2_DROVE_PORT or 7799>`) and serves until shutdown.
pub async fn serve_http(core: Arc<Core>) {
    let explicit_port = std::env::var("H2_DROVE_PORT").ok().and_then(|v| v.trim().parse::<u16>().ok());
    let desired = explicit_port.unwrap_or(7799);
    let listener = match TcpListener::bind(("127.0.0.1", desired)).await {
        Ok(l) => l,
        Err(_) if explicit_port.is_none() => match TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("drove-http: failed to bind localhost port: {e}");
                return;
            }
        },
        Err(e) => {
            eprintln!("drove-http: failed to bind 127.0.0.1:{desired}: {e}");
            return;
        }
    };

    let port = listener.local_addr().map(|a| a.port()).unwrap_or(desired);
    core.drove.web_port.store(port, Ordering::SeqCst);
    core.drove.write_web_info(port);

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { break };
                let c = core.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(c, stream).await;
                });
            }
            _ = core.drove.http_stop.notified() => break,
        }
    }
}

async fn handle_connection(core: Arc<Core>, stream: TcpStream) -> std::io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).await? == 0 {
        return Ok(());
    }
    let parts: Vec<&str> = request_line.trim_end_matches(['\r', '\n']).split_whitespace().collect();
    if parts.len() < 2 {
        return write_json(&mut write_half, 400, &json!({"error":"bad_request","message":"malformed request line"}))
            .await;
    }
    let method = parts[0].to_ascii_uppercase();
    let raw_target = parts[1];

    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let host = headers.get("host").map(String::as_str).unwrap_or("");
    if !is_valid_local_host(host) {
        return write_json(
            &mut write_half,
            403,
            &json!({"error":"invalid_host","message":"Host must be 127.0.0.1 or localhost"}),
        )
        .await;
    }

    // Read body if Content-Length or chunked.
    let body_bytes = match read_http_body(&mut reader, &headers).await {
        Ok(b) => b,
        Err(e) => {
            let (st, val) = e.into_json_value();
            return write_json(&mut write_half, st, &val).await;
        }
    };

    let (raw_path, raw_query) = raw_target.split_once('?').unwrap_or((raw_target, ""));
    let segments = match split_and_decode_segments(raw_path) {
        Ok(s) => s,
        Err(e) => {
            let (st, val) = e.into_json_value();
            return write_json(&mut write_half, st, &val).await;
        }
    };
    let query = parse_query(raw_query);

    // Handle SSE separately as a long-lived stream.
    if method == "GET" && segments.len() == 2 && segments[0] == "api" && segments[1] == "events" {
        return serve_sse(core, write_half, query.get("run").cloned(), query.get("local").is_some_and(|v| v == "1"))
            .await;
    }

    // Non-API paths serve Drove Web static assets (SPA fallback to index.html).
    if segments.first().map(String::as_str) != Some("api") {
        if method != "GET" && method != "HEAD" {
            return write_json(
                &mut write_half,
                405,
                &json!({"error":"method_not_allowed","message":"method not allowed"}),
            )
            .await;
        }
        return serve_static(&core.drove, &mut write_half, raw_path, method == "HEAD").await;
    }

    // Parse JSON body if present.
    let parsed_body = if body_bytes.is_empty() || method == "GET" || method == "HEAD" {
        None
    } else {
        match serde_json::from_slice::<Value>(&body_bytes) {
            Ok(v) => Some(v),
            Err(_) => {
                return write_json(
                    &mut write_half,
                    400,
                    &json!({"error":"invalid_json","message":"invalid JSON request body"}),
                )
                .await;
            }
        }
    };

    if let Err(e) = authorize_request(&core.drove, &method, &segments, &headers) {
        let (st, val) = e.into_json_value();
        return write_json(&mut write_half, st, &val).await;
    }

    let req = HttpProxyRequest {
        on: None,
        method,
        path: raw_path.to_string(),
        query: raw_query.to_string(),
        headers,
        body: parsed_body,
    };

    let resp = dispatch_http(&core, req, false).await;
    write_json(&mut write_half, resp.status, &resp.body).await
}

pub(crate) fn is_valid_local_host(host: &str) -> bool {
    let lower = host.trim().to_ascii_lowercase();
    let (h, port) = match lower.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (h, true),
        _ => (lower.as_str(), true),
    };
    let _ = port;
    matches!(h, "127.0.0.1" | "localhost")
}

async fn read_http_body<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    headers: &BTreeMap<String, String>,
) -> Result<Vec<u8>, ApiErr> {
    if let Some(te) = headers.get("transfer-encoding")
        && te.to_ascii_lowercase().contains("chunked")
    {
        let mut out = Vec::new();
        loop {
            let mut size_line = String::new();
            if reader.read_line(&mut size_line).await.map_err(|e| ApiErr::new(400, "bad_request", e.to_string()))? == 0
            {
                break;
            }
            let hex = size_line.split(';').next().unwrap_or("").trim();
            let sz =
                usize::from_str_radix(hex, 16).map_err(|_| ApiErr::new(400, "bad_request", "invalid chunk size"))?;
            if sz == 0 {
                let mut trailer = String::new();
                let _ = reader.read_line(&mut trailer).await;
                break;
            }
            if out.len() + sz > MAX_BODY_BYTES {
                return Err(ApiErr::new(413, "body_too_large", "JSON request body exceeds 1 MiB"));
            }
            let mut chunk = vec![0u8; sz];
            reader.read_exact(&mut chunk).await.map_err(|e| ApiErr::new(400, "bad_request", e.to_string()))?;
            out.extend_from_slice(&chunk);
            let mut crlf = [0u8; 2];
            let _ = reader.read_exact(&mut crlf).await;
        }
        return Ok(out);
    }

    if let Some(cl) = headers.get("content-length") {
        let len: usize = cl.trim().parse().map_err(|_| ApiErr::new(400, "bad_request", "invalid content-length"))?;
        if len > MAX_BODY_BYTES {
            return Err(ApiErr::new(413, "body_too_large", "JSON request body exceeds 1 MiB"));
        }
        if len > 0 {
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).await.map_err(|e| ApiErr::new(400, "bad_request", e.to_string()))?;
            return Ok(buf);
        }
    }
    Ok(Vec::new())
}

fn split_and_decode_segments(raw_path: &str) -> Result<Vec<String>, ApiErr> {
    let mut out = Vec::new();
    for seg in raw_path.split('/').filter(|s| !s.is_empty()) {
        let dec = percent_decode(seg, false).ok_or_else(|| ApiErr::new(400, "bad_request", "invalid URL encoding"))?;
        if dec.contains('\0') {
            return Err(ApiErr::new(400, "bad_request", "null byte in path"));
        }
        out.push(dec);
    }
    Ok(out)
}

fn parse_query(raw: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if raw.is_empty() {
        return map;
    }
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if let (Some(dk), Some(dv)) = (percent_decode(k, true), percent_decode(v, true)) {
            map.insert(dk, dv);
        }
    }
    map
}

fn percent_decode(s: &str, plus_as_space: bool) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' if plus_as_space => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                let val = u8::from_str_radix(hex, 16).ok()?;
                out.push(val);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn authorize_request(
    drove: &DroveRuntime,
    method: &str,
    segments: &[String],
    headers: &BTreeMap<String, String>,
) -> Result<(), ApiErr> {
    let segs: Vec<&str> = segments.iter().map(String::as_str).collect();
    let is_worker_post = method == "POST"
        && matches!(segs.as_slice(), ["api", "worker", "context" | "ask" | "note" | "done" | "inbox" | "read"]);

    if header(headers, "x-drove-worker-token", "x-co-worker-token").is_some() {
        if !is_worker_post {
            return Err(ApiErr::new(403, "forbidden", "worker token cannot call management endpoints"));
        }
        return Ok(());
    }
    if matches!(method, "GET" | "HEAD" | "OPTIONS")
        || is_worker_post
        || matches!((method, segs.as_slice()), ("PUT", ["api", "agents"]) | ("PUT", ["api", "ai", "config"]))
    {
        return Ok(());
    }

    let is_web_action = match (method, segs.as_slice()) {
        ("PUT", ["api", "tasks", _, "assign"]) => true,
        ("PUT", ["api", "ai", "language"]) => true,
        ("POST", ["api", "ai", "test"]) => true,
        ("POST", ["api", "workers", _, "stop"]) => true,
        ("PATCH", ["api", "runs", _]) => true,
        ("POST", ["api", "runs", "reorder"]) => true,
        ("POST", ["api", "runs", _, "trash" | "restore" | "pause" | "resume"]) => true,
        ("DELETE", ["api", "runs", _]) => true,
        _ => false,
    };
    if is_web_action
        && let Some(tok) = header(headers, "x-drove-token", "x-co-token")
        && crypto::secure_eq(tok, &drove.web_token)
    {
        return Ok(());
    }
    if let Some(sec) = header(headers, "x-drove-coordinator-secret", "x-co-coordinator-secret")
        && crypto::secure_eq(sec, &drove.coordinator_secret)
    {
        return Ok(());
    }
    Err(ApiErr::new(401, "unauthorized", "missing or invalid coordinator/web token"))
}

/// Dispatches an API HTTP request locally or federates to connected SSH machines when addressed as `<machine>/<id>` or `?on=<machine>`.
pub async fn dispatch_http(core: &Arc<Core>, req: HttpProxyRequest, internal_admin: bool) -> HttpProxyResponse {
    match dispatch_http_inner(core, req, internal_admin).await {
        Ok(val) => HttpProxyResponse { status: 200, body: val },
        Err(err) => {
            let (status, body) = err.into_json_value();
            HttpProxyResponse { status, body }
        }
    }
}

async fn dispatch_http_inner(
    core: &Arc<Core>,
    mut req: HttpProxyRequest,
    internal_admin: bool,
) -> Result<Value, ApiErr> {
    let segments = split_and_decode_segments(&req.path)?;
    let query = parse_query(&req.query);
    let method = req.method.to_ascii_uppercase();
    let local_only = internal_admin || query.get("local").is_some_and(|v| v == "1");
    let segs: Vec<&str> = segments.iter().map(String::as_str).collect();

    // 1. Check for SSH federation routing before local handling.
    if !local_only {
        if method == "GET" && segs.as_slice() == ["api", "runs"] {
            return federated_list_runs(core, &query).await;
        }
        if let Some(on_machine) = query.get("on").filter(|s| !s.is_empty()) {
            return proxy_to_machine(core, on_machine, &req).await;
        }
        // When the local machine has no Drove config yet (`drove.json` missing) and a connected remote
        // machine exists (e.g. Windows desktop app connected to WSL), proxy global Drove status/settings
        // queries when `?on=` was not explicitly supplied.
        let global_drove_endpoint = (method == "GET"
            && matches!(segs.as_slice(), ["api", "ai", "status"] | ["api", "agents"]))
            || (method == "POST" && segs.as_slice() == ["api", "ai", "test"])
            || (method == "PUT" && segs.as_slice() == ["api", "ai", "language"])
            || (method == "GET"
                && segs.as_slice() == ["api", "daemon", "events"]
                && query.get("runId").is_none()
                && query.get("run").is_none());
        if global_drove_endpoint
            && !core.drove.paths.config_file.exists()
            && let Some(m) = connected_machines(core).into_iter().next()
        {
            return proxy_to_machine(core, &m.name, &req).await;
        }
        // Check resource id containing `<machine>/<id>` or `runId=<machine>/<id>`.
        if let Some((machine, rewritten_path)) = extract_machine_target(&segs) {
            req.path = rewritten_path;
            // Rewrite confirmation header for DELETE /api/runs/<machine>/<id>.
            if let Some(c) = header(&req.headers, "x-drove-confirm", "x-co-confirm").map(String::from)
                && let Some((_, rest)) = c.split_once('/')
            {
                req.headers.insert("x-drove-confirm".into(), rest.to_string());
                req.headers.insert("x-co-confirm".into(), rest.to_string());
            }
            return proxy_to_machine(core, &machine, &req).await;
        }
        if segs.as_slice() == ["api", "daemon", "events"]
            && let Some(raw_run) = query.get("runId").or_else(|| query.get("run"))
            && let Some((machine, remote_run)) = raw_run.split_once('/')
        {
            let mut next_q = query.clone();
            next_q.insert("runId".into(), remote_run.to_string());
            req.query = encode_query(&next_q);
            return proxy_to_machine(core, machine, &req).await;
        }
    }

    let body = req.body.unwrap_or(Value::Null);
    match (method.as_str(), segs.as_slice()) {
        ("GET", ["api", "health"]) => Ok(json!({
            "ok": true,
            "pid": std::process::id(),
            "version": env!("CARGO_PKG_VERSION"),
            "startedAt": core.drove.started_at,
        })),
        ("POST", ["api", "preflight", "worktree"]) => preflight_worktree_http(core, &body).await,
        ("GET", ["api", "ai", "status"]) => Ok(json!(core.drove.ai_status())),
        ("PUT", ["api", "ai", "config"]) => {
            Err(ApiErr::new(405, "configured_in_app", "Please configure AI Assistant in Herdr2 App -> Settings -> AI"))
        }
        ("PUT", ["api", "ai", "language"]) => {
            let lang = body["language"]
                .as_str()
                .or_else(|| body["lang"].as_str())
                .ok_or_else(|| ApiErr::new(400, "bad_request", "language must be a string"))?
                .to_string();
            core.ai
                .update_settings(crate::ai::AiSettingsSetParams { language: Some(lang), ..Default::default() })
                .await
                .map_err(rpc_to_api)?;
            core.drove.sync_ai_from_core();
            Ok(json!(core.drove.ai_status()))
        }
        ("POST", ["api", "ai", "test"]) => {
            let res = core.drove.rpc_ai_test(None, None, None).await.map_err(rpc_to_api)?;
            Ok(json!(res))
        }
        ("GET", ["api", "agents"]) => {
            let force = query.get("refresh").is_some_and(|v| v == "1");
            let cfg = core.drove.config_snapshot();
            let stats = core.drove.store.compute_stats();
            let (quotas, quota_err) = core.drove.magpie.fetch_if_enabled(&cfg.magpie, force).await;
            Ok(web_agents_payload(&cfg, stats, quotas, quota_err))
        }
        ("PUT", ["api", "agents"]) => {
            Err(ApiErr::new(405, "configured_in_app", "Please configure Roles in Herdr2 App -> Settings -> Drove"))
        }
        ("GET", ["api", "daemon", "events"]) => {
            let run_id = query.get("runId").or_else(|| query.get("run")).map(String::as_str);
            let category = query.get("category").map(String::as_str);
            let status = query.get("status").map(String::as_str);
            let before = query.get("before").and_then(|s| s.parse::<u64>().ok());
            let limit = query.get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(100).clamp(1, 200);
            let (events, has_more) = core.drove.store.list_daemon_events(run_id, category, status, before, limit);
            Ok(json!({ "events": events, "hasMore": has_more }))
        }
        ("GET", ["api", "runs"]) => {
            let trash = query.get("trash").is_some_and(|v| v == "1");
            let counts = core.drove.store.count_tasks_by_run();
            let mut runs: Vec<Run> = core
                .drove
                .store
                .list_runs()
                .into_iter()
                .filter(|r| r.trashed_at.as_ref().is_some_and(|s| !s.is_empty()) == trash)
                .collect();
            sort_runs(&mut runs);
            let summaries: Vec<RunSummary> = runs
                .into_iter()
                .map(|run| {
                    let task_counts = counts.get(&run.id).cloned().unwrap_or_else(super::store::empty_task_counts);
                    RunSummary { run, task_counts }
                })
                .collect();
            Ok(json!({ "runs": summaries }))
        }
        ("POST", ["api", "runs"]) => {
            let run = create_run_http(&core, &req.headers, &body, internal_admin).await?;
            if run["name"].is_null()
                && let Some(id) = run["id"].as_str()
                && let Some(obj) = run["objective"].as_str()
            {
                super::ai::spawn_auto_title(core.clone(), id.to_string(), obj.to_string());
            }
            Ok(run)
        }
        ("GET", ["api", "runs", id]) => build_snapshot(&core, id).await,
        ("PATCH", ["api", "runs", id]) => patch_run_http(&core.drove, &req.headers, id, &body, internal_admin),
        ("POST", ["api", "runs", "reorder"]) => {
            let ids = body["ids"].as_array().ok_or_else(|| ApiErr::new(400, "bad_request", "ids must be an array"))?;
            let mut parsed = Vec::with_capacity(ids.len());
            for v in ids {
                let s = v
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| ApiErr::new(400, "bad_request", "invalid run id"))?;
                if core.drove.store.get_run(s).is_none() {
                    return Err(ApiErr::new(404, "not_found", "Run not found"));
                }
                parsed.push(s.to_string());
            }
            core.drove.store.reorder_runs(&parsed);
            Ok(json!({ "ok": true }))
        }
        ("POST", ["api", "runs", id, "trash"]) => trash_run_http(&core, id).await,
        ("POST", ["api", "runs", id, "restore"]) => restore_run_http(&core.drove, id),
        ("POST", ["api", "runs", id, "pause"]) => pause_run_http(&core.drove, id),
        ("POST", ["api", "runs", id, "resume"]) => resume_run_http(&core.drove, id),
        ("DELETE", ["api", "runs", id]) => {
            let run = core.drove.store.get_run(id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
            if run.trashed_at.as_ref().is_none_or(String::is_empty) {
                return Err(ApiErr::new(409, "invalid_state", "only trashed runs can be permanently deleted"));
            }
            let confirm = header(&req.headers, "x-drove-confirm", "x-co-confirm").unwrap_or("");
            if confirm != *id {
                return Err(ApiErr::new(400, "confirmation_required", "missing confirmation header"));
            }
            let wids: Vec<String> = core.drove.store.list_workers(Some(id)).into_iter().map(|w| w.id).collect();
            super::pool::stop_run_workers(&core, id).await;
            for wid in wids {
                core.drove.remove_worker_token(&wid);
            }
            let _ = std::fs::remove_dir_all(core.drove.paths.state_dir.join("worktrees").join(id));
            core.drove.store.delete_run(id);
            Ok(json!({ "ok": true }))
        }
        ("GET", ["api", "runs", id, "inbox"]) => {
            let _ = core.drove.store.get_run(id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
            let messages = core.drove.store.list_messages(id, Some(true), true, None);
            Ok(json!({ "messages": messages }))
        }
        ("GET", ["api", "runs", id, "coordinator-transcript"]) => {
            let run = core.drove.store.get_run(id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
            let (agent, model, pid, entries, available) = resolve_coordinator_meta_and_entries(&core, &run, true).await;
            let run = sync_coordinator_meta(&core.drove, run, agent, model, pid);
            let effective_agent = run.coordinator.agent.clone();
            let models: Vec<String> = run.coordinator.model.clone().into_iter().collect();

            if available {
                let total = entries.len();
                let limit = query.get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(200).clamp(1, 500);
                let before = query.get("before").and_then(|s| s.parse::<usize>().ok()).unwrap_or(total).min(total);
                let start = before.saturating_sub(limit);
                let items: Vec<Value> =
                    entries[start..before].iter().enumerate().map(|(i, e)| entry_to_host_item(start + i, e)).collect();
                let first_ts = entries.iter().map(entry_ts_ms).find(|ms| *ms > 0).map(super::types::format_ms_iso);
                let last_ts = entries.iter().rev().map(entry_ts_ms).find(|ms| *ms > 0).map(super::types::format_ms_iso);
                return Ok(json!({
                    "available": true,
                    "host": run.coordinator.host,
                    "agent": effective_agent,
                    "hostSession": run.coordinator.host_session,
                    "models": models,
                    "firstTs": first_ts,
                    "lastTs": last_ts,
                    "total": total,
                    "start": start,
                    "hasMore": start > 0,
                    "items": items,
                }));
            }
            Ok(json!({
                "available": false,
                "host": run.coordinator.host,
                "agent": effective_agent,
                "hostSession": run.coordinator.host_session,
                "models": models,
                "total": 0,
                "start": 0,
                "hasMore": false,
                "items": [],
            }))
        }
        ("POST", ["api", "runs", id, "tasks"]) => {
            add_tasks_http(&core, id, header(&req.headers, "x-drove-coordinator", "x-co-coordinator"), &body).await
        }
        ("GET", ["api", "runs", id, "wait"]) => wait_run_http(&core, id, &query).await,
        ("POST", ["api", "runs", id, "complete"]) => complete_run_http(&core, id, &body).await,
        ("POST", ["api", "runs", id, "cancel"]) => cancel_run_http(&core, id).await,
        ("GET", ["api", "runs", id, "verify-log"]) => {
            let run = core.drove.store.get_run(id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
            let artifact = query.get("artifact").map(String::as_str).unwrap_or("");
            if artifact.is_empty() {
                return Err(ApiErr::new(400, "bad_request", "missing artifact parameter"));
            }
            let mut allowed = Vec::new();
            if let Some(a) = run.finalization.as_ref().and_then(|f| f.log_artifact_id.as_ref()) {
                allowed.push(a.clone());
            }
            for att in core.drove.store.list_attempts(Some(&run.id), None, None) {
                if let Some(a) = att.verify_log_artifact_id {
                    allowed.push(a);
                }
            }
            if !allowed.iter().any(|a| a == artifact) {
                return Err(ApiErr::new(403, "forbidden", "artifact does not belong to this run"));
            }
            let offset = query.get("offset").and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
            let limit = query.get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(65536).clamp(1, 262144);
            let mut slice = core
                .drove
                .store
                .read_artifact_slice(&run.id, artifact, offset, limit, None)
                .map_err(|e| ApiErr::new(404, "not_found", e.to_string()))?;
            slice["artifact"] = Value::String(artifact.to_string());
            Ok(slice)
        }
        ("GET", ["api", "workers", id, "items"]) => {
            if core.drove.store.get_worker(id).is_none() {
                return Err(ApiErr::new(404, "not_found", "Worker not found"));
            }
            let limit = query.get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(200).clamp(1, 1000);
            let before = query.get("before").and_then(|s| s.parse::<u64>().ok());
            let attempt = query.get("attempt").map(String::as_str).filter(|s| !s.is_empty());
            let mut items = core.drove.store.list_items(id, attempt, before, limit + 1);
            let has_more = items.len() > limit;
            if has_more {
                items.drain(..items.len() - limit);
            }
            Ok(json!({ "items": items, "hasMore": has_more }))
        }
        ("POST", ["api", "workers", id, "stop"]) => {
            if core.drove.store.get_worker(id).is_none() {
                return Err(ApiErr::new(404, "not_found", "Worker not found"));
            }
            let updated = super::pool::stop_worker(core, id).await;
            core.drove.wake_scheduler();
            Ok(json!(updated))
        }
        ("POST", ["api", "messages"]) => post_message_http(&core.drove, &body),
        ("GET", ["api", "messages", id]) => {
            let msg =
                core.drove.store.get_message(id).ok_or_else(|| ApiErr::new(404, "not_found", "Message not found"))?;
            Ok(json!(msg))
        }
        ("POST", ["api", "messages", id, "ack"]) => {
            let _ =
                core.drove.store.get_message(id).ok_or_else(|| ApiErr::new(404, "not_found", "Message not found"))?;
            core.drove.store.mark_handled(id);
            Ok(json!({ "ok": true }))
        }
        ("POST", ["api", "messages", id, "reply"]) => {
            let msg =
                core.drove.store.get_message(id).ok_or_else(|| ApiErr::new(404, "not_found", "Message not found"))?;
            if !matches!(msg.kind, MessageType::Question | MessageType::Escalation) {
                return Err(ApiErr::new(409, "invalid_state", "this message type cannot be replied to"));
            }
            let reply_body = body["body"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| ApiErr::new(400, "bad_request", "body must be non-empty"))?;
            let reply = core.drove.store.post_reply_and_mark_handled(
                &msg.id,
                super::store::PostMessageInput {
                    run_id: msg.run_id.clone(),
                    kind: MessageType::Reply,
                    from: "coordinator".into(),
                    to: msg.from.clone(),
                    thread_id: Some(msg.id.clone()),
                    task_id: msg.task_id.clone(),
                    attempt_id: msg.attempt_id.clone(),
                    subject: format!("Re: {}", msg.subject),
                    body: Some(reply_body.to_string()),
                    options: None,
                    recommend: None,
                    why: None,
                    raised_by: Some("coordinator".into()),
                    raised_by_worker: None,
                    tag: None,
                    ai_draft: None,
                    needs_action: false,
                },
            );
            core.drove.message_reply_wake.notify_waiters();
            Ok(json!(reply))
        }
        ("POST", ["api", "tasks", id, "retry"]) => retry_task_http(&core, id, &body),
        ("POST", ["api", "tasks", id, "cancel"]) => cancel_task_http(&core, id).await,
        ("POST", ["api", "tasks", id, "send"]) => send_task_http(&core, id, &body).await,
        ("GET", ["api", "tasks", id, "inspect"]) => {
            let tail = query.get("tail").and_then(|s| s.parse::<usize>().ok()).unwrap_or(30).clamp(1, 200);
            let mut diag = super::diagnose::diagnose_task_with_tail(&core.drove.store, id, tail)
                .ok_or_else(|| ApiErr::new(404, "not_found", "Task or attempt not found"))?;
            let want_ai = query.get("ai").is_some_and(|v| v == "1" || v == "true");
            let refresh = query.get("refresh").is_some_and(|v| v == "1" || v == "true");
            let lang = query.get("lang").or_else(|| query.get("language")).map(String::as_str);
            if want_ai {
                super::ai::generate_task_diagnosis(&core, &mut diag, refresh, lang).await;
            }
            Ok(json!(diag))
        }
        ("POST", ["api", "worker", "context"]) => worker_context_http(&core.drove, &req.headers, &body),
        ("POST", ["api", "worker", "inbox"]) => {
            let att = authenticate_worker(&core.drove, &req.headers, &body)?;
            let messages = core.drove.store.drain_worker_inbox(&att.worker_id, &att.id);
            Ok(json!({ "messages": messages }))
        }
        ("POST", ["api", "worker", "ask"]) => worker_ask_http(&core, &req.headers, &body).await,
        ("POST", ["api", "worker", "note"]) => worker_note_http(&core.drove, &req.headers, &body),
        ("POST", ["api", "worker", "read"]) => worker_read_http(&core.drove, &req.headers, &body),
        ("POST", ["api", "worker", "done"]) => worker_done_http(&core, &req.headers, &body).await,
        ("PUT", ["api", "tasks", id, "assign"]) => {
            let task = core.drove.store.get_task(id).ok_or_else(|| ApiErr::new(404, "not_found", "Task not found"))?;
            if !matches!(task.status, TaskStatus::Pending | TaskStatus::Ready)
                || !core.drove.store.list_attempts(None, Some(&task.id), None).is_empty()
            {
                return Err(ApiErr::new(409, "invalid_state", "task has already started"));
            }
            let mut assign: Assign =
                serde_json::from_value(body).map_err(|e| ApiErr::new(400, "invalid_field", e.to_string()))?;
            assign.source = Some("user".into());
            let updated = core.drove.store.update_task(id, |t| t.assign = Some(assign));
            Ok(json!(updated))
        }
        ("GET", ["api", "tasks", id, "review-diff"]) => {
            let task = core.drove.store.get_task(id).ok_or_else(|| ApiErr::new(404, "not_found", "Task not found"))?;
            let artifact = query.get("artifact").map(String::as_str).unwrap_or("");
            if artifact.is_empty() {
                return Err(ApiErr::new(400, "bad_request", "missing artifact parameter"));
            }
            let offset = query.get("offset").and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
            let limit = query.get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(65536).clamp(1, 262144);
            let mut slice = core
                .drove
                .store
                .read_artifact_slice(&task.run_id, &format!("{artifact}.patch"), offset, limit, None)
                .map_err(|e| ApiErr::new(404, "not_found", e.to_string()))?;
            slice["artifactId"] = Value::String(artifact.to_string());
            Ok(slice)
        }
        _ => Err(ApiErr::new(404, "not_found", "endpoint not implemented yet or not found")),
    }
}

async fn create_run_http(
    core: &Arc<Core>,
    headers: &BTreeMap<String, String>,
    body: &Value,
    internal_admin: bool,
) -> Result<Value, ApiErr> {
    let host_session = body["hostSession"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::new(400, "bad_request", "hostSession must be a non-empty string"))?;
    if !internal_admin && header(headers, "x-drove-coordinator", "x-co-coordinator") != Some(host_session) {
        return Err(ApiErr::new(409, "fenced", "coordinator session mismatch"));
    }
    let objective = body["objective"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::new(400, "bad_request", "objective must be non-empty"))?;
    let cwd = body["cwd"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::new(400, "bad_request", "cwd must be non-empty"))?;
    let host = body["host"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("herdr2");
    let max_conc = body["maxConcurrency"].as_u64().unwrap_or(4);
    if !(1..=16).contains(&max_conc) {
        return Err(ApiErr::new(400, "bad_request", "maxConcurrency must be 1..16"));
    }
    let (detected_agent, detected_model, detected_pid, _, _) =
        resolve_session_meta_and_entries(core, host_session, true, false).await;
    let session_bound = is_local_session_bound(core, host_session);
    let agent = body["agent"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from).or(detected_agent);
    let model = body["model"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from).or(detected_model);
    let host_pid = body["hostPid"].as_u64().and_then(|v| u32::try_from(v).ok()).or(detected_pid);
    let run = core.drove.store.create_run(super::store::CreateRunInput {
        objective: objective.into(),
        cwd: cwd.into(),
        host: host.into(),
        host_session: host_session.into(),
        agent,
        model,
        host_pid,
        session_bound,
        max_concurrency: max_conc as u32,
        budget: serde_json::from_value(body["budget"].clone()).ok().flatten(),
        verify: serde_json::from_value(body["verify"].clone()).ok().flatten(),
        goal: serde_json::from_value(body["goal"].clone()).ok().flatten(),
    });
    Ok(json!(run))
}

fn patch_run_http(
    drove: &DroveRuntime,
    headers: &BTreeMap<String, String>,
    id: &str,
    body: &Value,
    internal_admin: bool,
) -> Result<Value, ApiErr> {
    let run = drove.store.get_run(id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    let obj = body.as_object().ok_or_else(|| ApiErr::new(400, "bad_request", "body must be an object"))?;
    if !internal_admin && (obj.contains_key("verify") || obj.contains_key("goal")) {
        let sec = header(headers, "x-drove-coordinator-secret", "x-co-coordinator-secret").unwrap_or("");
        if !crypto::secure_eq(sec, &drove.coordinator_secret) {
            return Err(ApiErr::new(401, "unauthorized", "missing coordinator secret"));
        }
    }
    let updated = drove.store.update_run(&run.id, |r| {
        if let Some(name_val) = obj.get("name") {
            if let Some(s) = name_val.as_str() {
                let t = s.trim();
                r.name = (!t.is_empty()).then(|| t.to_string());
            } else if name_val.is_null() {
                r.name = None;
            }
        }
        if let Some(v) = obj.get("verify") {
            if v.is_null() {
                r.verify = None;
            } else if let Ok(parsed) = serde_json::from_value(v.clone()) {
                r.verify = Some(parsed);
            }
        }
        if let Some(g) = obj.get("goal") {
            if g.is_null() {
                r.goal = None;
            } else if let Ok(parsed) = serde_json::from_value(g.clone()) {
                r.goal = Some(parsed);
            }
        }
    });
    Ok(json!(updated))
}

async fn preflight_worktree_http(core: &Arc<Core>, body: &Value) -> Result<Value, ApiErr> {
    let cwd = body["cwd"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::new(400, "bad_request", "cwd is required"))?
        .to_string();
    let raw_tasks: Vec<Value> = body["tasks"].as_array().cloned().unwrap_or_default();
    let force = body["force"].as_bool() == Some(true);
    let cfg = core.drove.config_snapshot();
    let needed = force || super::routes::tasks_need_worktree_preflight(&cfg, &[], &raw_tasks);
    if !needed {
        return Ok(json!({ "ok": true, "checked": false }));
    }
    let cwd_clone = cwd.clone();
    let check = tokio::task::spawn_blocking(move || super::integrate::check_drove_worktree_ready(&cwd_clone))
        .await
        .map_err(|e| ApiErr::new(500, "internal", format!("preflight join error: {e}")))?;
    match check {
        Ok(ok) => Ok(json!({
            "ok": true,
            "checked": true,
            "isAosp": ok.is_aosp,
            "dirtyFiles": ok.dirty_files,
            "totalDirty": ok.total_dirty,
            "dirtyWarning": ok.dirty_warning,
        })),
        Err(blocker) => {
            Err(ApiErr::new(422, "worktree_not_ready", format!("{} | 建议处理：{}", blocker.message, blocker.fix_hint)))
        }
    }
}

async fn trash_run_http(core: &Arc<Core>, id: &str) -> Result<Value, ApiErr> {
    let drove = &core.drove;
    let run = drove.store.get_run(id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    if run.trashed_at.as_ref().is_some_and(|s| !s.is_empty()) {
        return Err(ApiErr::new(409, "invalid_state", "Run is already in trash"));
    }
    if drove.store.list_tasks(id).iter().any(|t| t.status == TaskStatus::Running)
        || drove.store.list_workers(Some(id)).iter().any(|w| w.state == WorkerState::Busy)
    {
        return Err(ApiErr::new(409, "invalid_state", "stop active tasks before trashing"));
    }
    let wids: Vec<String> = drove.store.list_workers(Some(id)).into_iter().map(|w| w.id).collect();
    super::pool::stop_run_workers(core, id).await;
    core.ai.evict_session_prefix(&format!("drove:run:{id}"));
    for wid in wids {
        drove.remove_worker_token(&wid);
    }
    let _ = std::fs::remove_dir_all(drove.paths.state_dir.join("worktrees").join(id));
    let prev = run.status;
    let updated = drove.store.update_run(id, |r| {
        r.status_before_trash = Some(prev);
        r.status = RunStatus::Paused;
        r.trashed_at = Some(now_iso());
    });
    Ok(json!(updated))
}

fn restore_run_http(drove: &DroveRuntime, id: &str) -> Result<Value, ApiErr> {
    let run = drove.store.get_run(id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    if run.trashed_at.as_ref().is_none_or(String::is_empty) {
        return Err(ApiErr::new(409, "invalid_state", "Run is not in trash"));
    }
    let restore = run.status_before_trash.unwrap_or(RunStatus::Active);
    let updated = drove.store.update_run(id, |r| {
        r.status = restore;
        r.trashed_at = None;
        r.status_before_trash = None;
    });
    Ok(json!(updated))
}

fn pause_run_http(drove: &DroveRuntime, id: &str) -> Result<Value, ApiErr> {
    let run = drove.store.get_run(id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    if run.trashed_at.is_some() || matches!(run.status, RunStatus::Done | RunStatus::Archived | RunStatus::Paused) {
        return Err(ApiErr::new(409, "invalid_state", "Run cannot be paused in its current state"));
    }
    let updated = drove.store.update_run(id, |r| {
        r.status = RunStatus::Paused;
        r.pause_reason = Some("user".into());
    });
    Ok(json!(updated))
}

fn resume_run_http(drove: &DroveRuntime, id: &str) -> Result<Value, ApiErr> {
    let run = drove.store.get_run(id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    if run.trashed_at.is_some() || run.status != RunStatus::Paused {
        return Err(ApiErr::new(409, "invalid_state", "Run is not paused"));
    }
    let updated = drove.store.update_run(id, |r| {
        r.status = RunStatus::Active;
        r.pause_reason = None;
    });
    Ok(json!(updated))
}

fn normalize_snapshot_deps(tasks: &mut [Task]) {
    let snapshot: Vec<Task> = tasks.to_vec();
    for task in tasks.iter_mut() {
        let mut resolved = Vec::new();
        let mut push_unique = |id: String| {
            if !resolved.contains(&id) {
                resolved.push(id);
            }
        };
        let parent_tpl = task.expanded_from.as_deref().and_then(|pid| snapshot.iter().find(|c| c.id == pid));
        let parent_dep_ids: std::collections::HashSet<String> = parent_tpl
            .map(|tpl| {
                tpl.deps
                    .iter()
                    .filter_map(|pd| snapshot.iter().find(|c| &c.id == pd || &c.key == pd))
                    .map(|c| c.id.clone())
                    .collect()
            })
            .unwrap_or_default();

        for dep in &task.deps {
            if let Some(target) = snapshot.iter().find(|c| &c.id == dep || &c.key == dep) {
                if parent_dep_ids.contains(&target.id) {
                    continue;
                }
                if target.template == Some(true)
                    && task.expanded_from.as_deref() != Some(target.id.as_str())
                    && target.expansion_state.as_deref() == Some("done")
                {
                    let children: Vec<String> = snapshot
                        .iter()
                        .filter(|c| c.expanded_from.as_deref() == Some(target.id.as_str()))
                        .map(|c| c.id.clone())
                        .collect();
                    if !children.is_empty() {
                        for cid in children {
                            push_unique(cid);
                        }
                    } else {
                        push_unique(target.id.clone());
                    }
                } else {
                    push_unique(target.id.clone());
                }
            } else {
                let mut matched_foreach = false;
                for child in snapshot
                    .iter()
                    .filter(|c| c.key.strip_prefix(dep.as_str()).is_some_and(|rest| rest.starts_with('#')))
                {
                    matched_foreach = true;
                    push_unique(child.id.clone());
                }
                if !matched_foreach {
                    push_unique(dep.clone());
                }
            }
        }
        if let Some(tpl) = parent_tpl {
            push_unique(tpl.id.clone());
        }
        task.deps = resolved;
    }
}

pub(super) fn is_local_session_bound(core: &Core, host_session: &str) -> Option<bool> {
    (super::pool::local_session_part(core, host_session).is_some()
        && super::pool::resolve_local_parent(core, host_session).is_some())
    .then_some(true)
}

pub(super) async fn resolve_session_meta_and_entries(
    core: &Arc<Core>,
    host_session: &str,
    probe_model: bool,
    include_entries: bool,
) -> (Option<String>, Option<String>, Option<u32>, Vec<crate::transcript::Entry>, bool) {
    let local_sess = host_session.rsplit_once('/').map(|(_, r)| r).unwrap_or(host_session);

    if let Some(aid) = crate::agents::parse_id(local_sess) {
        let (agent, model, pid, live) = {
            let st = core.lock();
            if let Some(a) = st.reg.agents.get(&aid) {
                let model = a.model.clone().or_else(|| {
                    a.config_options
                        .iter()
                        .find(|o| o.id == "model" || o.category.as_deref() == Some("model"))
                        .and_then(|o| o.current_value.as_str().map(String::from))
                });
                (a.agent.clone().or_else(|| a.agent_info.clone()), model, a.pid, a.is_live())
            } else {
                (None, None, None, false)
            }
        };
        let entries = if include_entries { core.read_transcript_entries(aid).unwrap_or_default() } else { Vec::new() };
        let available = live || !entries.is_empty();
        return (agent, model, pid, entries, available);
    }

    if let Some(tid) = crate::terminals::parse_id(local_sess) {
        let found = {
            let mut st = core.lock();
            if let Some(t) = st.pty_reg.terminals.get(&tid).cloned() {
                let mut snap = crate::pty_transcript::snapshot_from_terminal(&t, st.pty_prompts.get(&tid));
                crate::pty_transcript::populate_session_affinity(&mut snap, &t.address, core, &mut st);
                Some((snap, t.is_live()))
            } else {
                None
            }
        };
        if let Some((snap, _live)) = found {
            let agent = snap.agent_hint.clone();
            let pid = snap.fg_pid;
            if include_entries || probe_model {
                let extracted = tokio::task::spawn_blocking(move || crate::pty_transcript::extract_transcript(&snap))
                    .await
                    .unwrap_or_else(|_| (Vec::new(), None));
                let model = extracted.1.and_then(|m| m.model);
                let entries = if include_entries { extracted.0 } else { Vec::new() };
                let available = !entries.is_empty() || model.is_some();
                return (agent, model, pid, entries, available);
            }
            return (agent, None, pid, Vec::new(), false);
        }
    }

    (None, None, None, Vec::new(), false)
}

async fn resolve_coordinator_meta_and_entries(
    core: &Arc<Core>,
    run: &Run,
    include_entries: bool,
) -> (Option<String>, Option<String>, Option<u32>, Vec<crate::transcript::Entry>, bool) {
    let probe_model = run.coordinator.model.is_none() || run.status == RunStatus::Active;
    resolve_session_meta_and_entries(core, &run.coordinator.host_session, probe_model, include_entries).await
}

fn sync_coordinator_meta(
    drove: &DroveRuntime,
    run: Run,
    agent: Option<String>,
    model: Option<String>,
    pid: Option<u32>,
) -> Run {
    let next_agent = agent.filter(|s| !s.trim().is_empty()).or_else(|| run.coordinator.agent.clone());
    let next_model = model.filter(|s| !s.trim().is_empty()).or_else(|| run.coordinator.model.clone());
    let next_pid = pid.or(run.coordinator.host_pid);
    if next_agent != run.coordinator.agent
        || next_model != run.coordinator.model
        || next_pid != run.coordinator.host_pid
    {
        return drove
            .store
            .update_run(&run.id, |r| {
                r.coordinator.agent = next_agent.clone();
                r.coordinator.model = next_model.clone();
                r.coordinator.host_pid = next_pid;
            })
            .unwrap_or(run);
    }
    run
}

fn entry_ts_ms(entry: &crate::transcript::Entry) -> u64 {
    match entry {
        crate::transcript::Entry::User { ts_ms, .. }
        | crate::transcript::Entry::Thought { ts_ms, .. }
        | crate::transcript::Entry::Assistant { ts_ms, .. }
        | crate::transcript::Entry::ToolCall { ts_ms, .. }
        | crate::transcript::Entry::Plan { ts_ms, .. }
        | crate::transcript::Entry::Permission { ts_ms, .. }
        | crate::transcript::Entry::Error { ts_ms, .. }
        | crate::transcript::Entry::Stderr { ts_ms, .. }
        | crate::transcript::Entry::Checkpoint { ts_ms, .. }
        | crate::transcript::Entry::Rewind { ts_ms, .. }
        | crate::transcript::Entry::Handoff { ts_ms, .. } => *ts_ms,
    }
}

fn entry_to_host_item(index: usize, entry: &crate::transcript::Entry) -> Value {
    let opt_iso = |ms: u64| (ms > 0).then(|| super::types::format_ms_iso(ms));
    match entry {
        crate::transcript::Entry::User { text, ts_ms, .. } => json!({
            "index": index,
            "kind": "user",
            "text": text,
            "ts": opt_iso(*ts_ms),
        }),
        crate::transcript::Entry::Assistant { text, ts_ms, .. } => json!({
            "index": index,
            "kind": "assistant",
            "text": text,
            "ts": opt_iso(*ts_ms),
        }),
        crate::transcript::Entry::Thought { text, ts_ms, .. } => json!({
            "index": index,
            "kind": "thought",
            "text": text,
            "ts": opt_iso(*ts_ms),
        }),
        crate::transcript::Entry::ToolCall { title, status, detail, ts_ms, .. } => {
            let text = match detail.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(d) => format!("{title}: {d}"),
                None => title.clone(),
            };
            json!({
                "index": index,
                "kind": "tool",
                "title": title,
                "detail": detail,
                "status": status,
                "text": text,
                "ts": opt_iso(*ts_ms),
            })
        }
        crate::transcript::Entry::Plan { items, ts_ms, .. } => json!({
            "index": index,
            "kind": "thought",
            "text": items.join("\n"),
            "ts": opt_iso(*ts_ms),
        }),
        crate::transcript::Entry::Permission { title, outcome, ts_ms, .. } => json!({
            "index": index,
            "kind": "tool",
            "title": title,
            "status": outcome,
            "text": title,
            "ts": opt_iso(*ts_ms),
        }),
        crate::transcript::Entry::Error { message, ts_ms, .. } => json!({
            "index": index,
            "kind": "tool",
            "title": "error",
            "detail": message,
            "status": "failed",
            "text": format!("error: {message}"),
            "ts": opt_iso(*ts_ms),
        }),
        crate::transcript::Entry::Stderr { text, ts_ms } => json!({
            "index": index,
            "kind": "tool",
            "title": "stderr",
            "detail": text,
            "text": format!("stderr: {text}"),
            "ts": opt_iso(*ts_ms),
        }),
        crate::transcript::Entry::Checkpoint { turn, files, ts_ms, .. } => json!({
            "index": index,
            "kind": "thought",
            "text": format!("[checkpoint turn {turn}: {} file(s)]", files.len()),
            "files": files,
            "turn": turn,
            "ts": opt_iso(*ts_ms),
        }),
        crate::transcript::Entry::Rewind { to_turn, restore_files, ts_ms } => json!({
            "index": index,
            "kind": "thought",
            "text": format!("[rewind to turn {to_turn}: files={restore_files}]"),
            "to_turn": to_turn,
            "restore_files": restore_files,
            "ts": opt_iso(*ts_ms),
        }),
        crate::transcript::Entry::Handoff {
            handoff_kind,
            from_address,
            from_provider,
            from_turns,
            artifact_path,
            ts_ms,
            ..
        } => json!({
            "index": index,
            "kind": "thought",
            "text": format!("[{handoff_kind} from {from_address}]"),
            "handoff_kind": handoff_kind,
            "from_address": from_address,
            "from_provider": from_provider,
            "from_turns": from_turns,
            "artifact_path": artifact_path,
            "ts": opt_iso(*ts_ms),
        }),
    }
}

async fn build_snapshot(core: &Arc<Core>, id: &str) -> Result<Value, ApiErr> {
    let run = core.drove.store.get_run(id).ok_or_else(|| ApiErr::new(404, "not_found", "Run not found"))?;
    let (agent, model, pid, _, _) = resolve_coordinator_meta_and_entries(core, &run, false).await;
    let run = sync_coordinator_meta(&core.drove, run, agent, model, pid);
    let mut tasks = core.drove.store.list_tasks(id);
    normalize_snapshot_deps(&mut tasks);
    let workers = core.drove.store.list_workers(Some(id));
    let attempts = core.drove.store.list_attempts(Some(id), None, None);
    let messages = core.drove.store.list_messages(id, None, false, None);
    let pending_messages = core.drove.store.list_messages(id, Some(true), true, None);
    let attempt_tokens: u64 = attempts.iter().map(|a| a.tokens.input + a.tokens.output + a.tokens.cache_read).sum();
    let attempt_cost: f64 = attempts.iter().map(|a| a.cost.unwrap_or(0.0)).sum();
    let ai_tokens = run.ai_usage.as_ref().map(|u| u.input_tokens + u.output_tokens).unwrap_or(0);
    let ai_cost = run.ai_usage.as_ref().map(|u| u.cost).unwrap_or(0.0);
    let snap = RunSnapshot {
        run,
        tasks,
        workers,
        attempts,
        pending_messages,
        messages,
        stats: RunStats {
            tokens: attempt_tokens + ai_tokens,
            cost: attempt_cost + ai_cost,
            worker_tokens: (attempt_tokens > 0).then_some(attempt_tokens),
            worker_cost: (attempt_cost > 0.0).then_some(attempt_cost),
            ai_tokens: (ai_tokens > 0).then_some(ai_tokens),
            ai_cost: (ai_cost > 0.0).then_some(ai_cost),
        },
        stalls: Some(Vec::new()),
    };
    Ok(json!(snap))
}

fn sort_runs(runs: &mut [Run]) {
    runs.sort_by(|a, b| match (a.order, b.order) {
        (Some(ao), Some(bo)) if (ao - bo).abs() > f64::EPSILON => {
            ao.partial_cmp(&bo).unwrap_or(std::cmp::Ordering::Equal)
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => b.created_at.cmp(&a.created_at),
    });
}

// --- SSH Federation Helpers -------------------------------------------------

fn connected_machines(core: &Core) -> Vec<crate::machines::Machine> {
    let st = core.lock();
    st.machines.iter().filter(|m| matches!(st.lines.get(&m.id), Some(LineState::Connected(_)))).cloned().collect()
}

fn find_machine(core: &Core, name: &str) -> Result<crate::machines::Machine, ApiErr> {
    let st = core.lock();
    st.machines
        .iter()
        .find(|m| m.name == name || m.id == name)
        .cloned()
        .ok_or_else(|| ApiErr::new(404, "machine_unknown", format!("unknown machine {name:?}")))
}

async fn federated_list_runs(core: &Arc<Core>, query: &BTreeMap<String, String>) -> Result<Value, ApiErr> {
    let trash = query.get("trash").is_some_and(|v| v == "1");
    let counts = core.drove.store.count_tasks_by_run();
    let mut all_runs: Vec<Value> = core
        .drove
        .store
        .list_runs()
        .into_iter()
        .filter(|r| r.trashed_at.as_ref().is_some_and(|s| !s.is_empty()) == trash)
        .map(|run| {
            let task_counts = counts.get(&run.id).cloned().unwrap_or_else(super::store::empty_task_counts);
            let mut v = json!(run);
            v["taskCounts"] = json!(task_counts);
            v
        })
        .collect();

    let machines = connected_machines(core);
    let q_str = encode_query(&{
        let mut m = query.clone();
        m.insert("local".into(), "1".into());
        m
    });
    let mut handles = Vec::new();
    for m in machines {
        let c = core.clone();
        let qs = q_str.clone();
        handles.push(tokio::spawn(async move {
            let req = HttpProxyRequest {
                on: None,
                method: "GET".into(),
                path: "/api/runs".into(),
                query: qs,
                headers: BTreeMap::new(),
                body: None,
            };
            let fut = remote::forward(&c, &m, "drove.http_proxy", json!(req));
            let res = tokio::time::timeout(FEDERATION_TIMEOUT, fut).await;
            (m.name, res)
        }));
    }

    for h in handles {
        if let Ok((mname, Ok(Ok(val)))) = h.await
            && let Some(runs_arr) = val["body"]["runs"].as_array()
        {
            for r in runs_arr {
                let mut cloned = r.clone();
                prefix_remote_json(&mut cloned, &mname);
                all_runs.push(cloned);
            }
        }
    }

    all_runs.sort_by(|a, b| {
        let ao = a["order"].as_f64();
        let bo = b["order"].as_f64();
        match (ao, bo) {
            (Some(x), Some(y)) if (x - y).abs() > f64::EPSILON => {
                x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            _ => b["createdAt"].as_str().unwrap_or("").cmp(a["createdAt"].as_str().unwrap_or("")),
        }
    });

    Ok(json!({ "runs": all_runs }))
}

async fn proxy_to_machine(core: &Arc<Core>, machine: &str, req: &HttpProxyRequest) -> Result<Value, ApiErr> {
    let m = find_machine(core, machine)?;
    let resp_val = remote::forward(core, &m, "drove.http_proxy", json!(req)).await.map_err(rpc_to_api)?;
    let parsed: HttpProxyResponse = serde_json::from_value(resp_val)
        .map_err(|e| ApiErr::new(502, "bad_gateway", format!("invalid proxy reply: {e}")))?;
    let mut body = parsed.body;
    prefix_remote_json(&mut body, &m.name);
    if parsed.status >= 400 {
        let code = body["error"].as_str().unwrap_or("remote_error").to_string();
        let msg = body["message"].as_str().unwrap_or("remote error").to_string();
        return Err(ApiErr::new(parsed.status, code, msg));
    }
    Ok(body)
}

fn extract_machine_target(segs: &[&str]) -> Option<(String, String)> {
    let rest = segs.strip_prefix(&["api"])?;
    match rest {
        [res @ ("runs" | "tasks" | "workers" | "messages"), id, tail @ ..] if id.contains('/') => {
            let (machine, remote_id) = id.split_once('/')?;
            if machine.is_empty() || remote_id.is_empty() {
                return None;
            }
            let suffix = if tail.is_empty() { String::new() } else { format!("/{}", tail.join("/")) };
            Some((machine.to_string(), format!("/api/{res}/{remote_id}{suffix}")))
        }
        _ => None,
    }
}

/// Prefixes Drove entity IDs in remote JSON objects with `<machine>/` recursively.
pub(crate) fn prefix_remote_json(v: &mut Value, machine: &str) {
    const ID_KEYS: &[&str] = &[
        "id",
        "runId",
        "taskId",
        "attemptId",
        "workerId",
        "messageId",
        "currentAttemptId",
        "retryOf",
        "expandedFrom",
        "cachedFrom",
        "reviewerWorkerId",
        "threadId",
        "task",
    ];
    let looks_like_entity_id = |s: &str| {
        let local = s.rsplit_once('/').map(|(_, r)| r).unwrap_or(s);
        ["run_", "tsk_", "wrk_", "att_", "msg_", "r_", "t_", "w_", "a_", "m_"].iter().any(|p| local.starts_with(p))
    };
    let prefix = |s: &mut String| {
        if !s.is_empty()
            && !s.contains('/')
            && !matches!(s.as_str(), "exec" | "coordinator" | "board" | "daemon-ai" | "user")
        {
            *s = format!("{machine}/{s}");
        }
    };
    match v {
        Value::Object(map) => {
            let is_run = map.contains_key("objective") && map.contains_key("coordinator");
            let is_msg = map.contains_key("seq") && map.contains_key("needsAction");
            for (k, val) in map.iter_mut() {
                if let Value::String(s) = val
                    && (ID_KEYS.contains(&k.as_str()) || (k == "from" && is_msg && s.starts_with("worker:")))
                {
                    if let Some(wid) = s.strip_prefix("worker:").map(String::from) {
                        *s =
                            if wid.contains('/') { format!("worker:{wid}") } else { format!("worker:{machine}/{wid}") };
                    } else if k == "task" && !looks_like_entity_id(s) {
                        // Keep task key references unprefixed so frontend matches `task.key`.
                    } else {
                        prefix(s);
                    }
                } else if (k == "deps" || k == "taskIds" || k == "messageIds")
                    && let Value::Array(arr) = val
                {
                    for item in arr.iter_mut() {
                        if let Value::String(s) = item
                            && (k != "deps" || looks_like_entity_id(s))
                        {
                            prefix(s);
                        }
                    }
                } else {
                    prefix_remote_json(val, machine);
                }
            }
            if is_run {
                map.insert("machine".into(), Value::String(machine.to_string()));
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                prefix_remote_json(item, machine);
            }
        }
        _ => {}
    }
}

fn encode_query(map: &BTreeMap<String, String>) -> String {
    map.iter().map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v))).collect::<Vec<_>>().join("&")
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn rpc_to_api(err: RpcError) -> ApiErr {
    let status = match err.kind.as_str() {
        "not_found" | "machine_unknown" => 404,
        "invalid_params" => 400,
        "unavailable" => 503,
        _ => 500,
    };
    ApiErr::new(status, err.kind, err.message)
}

// --- SSE Stream (Local + Framed Remote Relay) -------------------------------

async fn serve_sse(
    core: Arc<Core>,
    mut write_half: tokio::net::tcp::OwnedWriteHalf,
    run_filter: Option<String>,
    local_only: bool,
) -> std::io::Result<()> {
    let hdr = concat!(
        "HTTP/1.1 200 OK\r\n",
        "content-type: text/event-stream; charset=utf-8\r\n",
        "cache-control: no-cache, no-transform\r\n",
        "connection: keep-alive\r\n",
        "x-accel-buffering: no\r\n\r\n",
        ": connected\n\n"
    );
    write_half.write_all(hdr.as_bytes()).await?;
    write_half.flush().await?;

    let mut local_rx = core.drove.store.subscribe();
    let (remote_tx, mut remote_rx) = tokio::sync::mpsc::unbounded_channel::<(String, Value)>();

    let remote_tasks =
        if local_only { Vec::new() } else { spawn_remote_sse_relays(&core, run_filter.as_deref(), remote_tx).await };

    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await;

    let res = loop {
        tokio::select! {
            ev = local_rx.recv() => match ev {
                Ok(StoreEvent::Change(c)) => {
                    if c.table == "items" {
                        continue;
                    }
                    if matches_run(run_filter.as_deref(), c.run_id.as_deref()) {
                        let frame = format!("event: change\ndata: {}\n\n", json!(c));
                        if let Err(e) = write_sse_frame(&mut write_half, &frame).await {
                            break Err(e);
                        }
                    }
                }
                Ok(StoreEvent::Item { run_id, item }) => {
                    if matches_run(run_filter.as_deref(), run_id.as_deref()) {
                        let frame = format!("event: item\ndata: {}\n\n", json!(item));
                        if let Err(e) = write_sse_frame(&mut write_half, &frame).await {
                            break Err(e);
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break Ok(()),
            },
            Some((mname, mut msg)) = remote_rx.recv() => {
                let kind = msg["event"].as_str().unwrap_or("change").to_string();
                let mut data = msg["data"].take();
                prefix_remote_json(&mut data, &mname);
                let ev_run = data["runId"].as_str().map(String::from);
                if matches_run(run_filter.as_deref(), ev_run.as_deref()) {
                    let frame = format!("event: {kind}\ndata: {data}\n\n");
                    if let Err(e) = write_sse_frame(&mut write_half, &frame).await {
                        break Err(e);
                    }
                }
            },
            _ = heartbeat.tick() => {
                if let Err(e) = write_sse_frame(&mut write_half, ": heartbeat\n\n").await {
                    break Err(e);
                }
            }
        }
    };

    for handle in remote_tasks {
        handle.abort();
    }
    res
}

async fn write_sse_frame(w: &mut tokio::net::tcp::OwnedWriteHalf, frame: &str) -> std::io::Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        w.write_all(frame.as_bytes()).await?;
        w.flush().await
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "SSE client stalled"))?
}

fn matches_run(filter: Option<&str>, event_run: Option<&str>) -> bool {
    match (filter, event_run) {
        (Some(f), Some(e)) => f == e,
        (Some(_), None) => true,
        (None, _) => true,
    }
}

async fn spawn_remote_sse_relays(
    core: &Arc<Core>,
    run_filter: Option<&str>,
    out: tokio::sync::mpsc::UnboundedSender<(String, Value)>,
) -> Vec<tokio::task::AbortHandle> {
    let target_machine = run_filter.and_then(|r| r.split_once('/')).map(|(m, _)| m);
    let machines: Vec<(String, Arc<remote::Line>)> = {
        let st = core.lock();
        st.machines
            .iter()
            .filter(|m| target_machine.is_none_or(|tm| tm == m.name || tm == m.id))
            .filter_map(|m| match st.lines.get(&m.id) {
                Some(LineState::Connected(l)) => Some((m.name.clone(), l.clone())),
                _ => None,
            })
            .collect()
    };

    let mut handles = Vec::new();
    for (mname, line) in machines {
        let tx = out.clone();
        let task = tokio::spawn(async move {
            let Ok((ch, mut rx)) = line.subscribe_drove(&mname).await else { return };
            let mut buf = Vec::new();
            while let Some(chunk) = rx.recv().await {
                buf.extend_from_slice(&chunk);
                while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                    let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
                    if let Ok(val) = serde_json::from_slice::<Value>(&line_bytes)
                        && val.get("event").is_some()
                        && tx.send((mname.clone(), val)).is_err()
                    {
                        line.close_drove_stream(ch);
                        return;
                    }
                }
            }
            line.close_drove_stream(ch);
        });
        handles.push(task.abort_handle());
    }
    handles
}

// --- Static File Serving & Token Injection ----------------------------------

include!(concat!(env!("OUT_DIR"), "/drove_web_assets.rs"));

fn resolve_web_dist() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("H2_DROVE_WEB_DIR")
        && !dir.trim().is_empty()
    {
        let p = PathBuf::from(dir.trim());
        if p.join("index.html").is_file() {
            return Some(p);
        }
    }
    if let Some(exe) = crate::client::exe_path()
        && let Some(dir) = exe.parent()
    {
        for cand in [
            dir.join("drove-web/dist"),
            dir.join("../drove-web/dist"),
            dir.join("../../drove-web/dist"),
            dir.join("../../../drove-web/dist"),
        ] {
            if cand.join("index.html").is_file() {
                return Some(cand);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let manifest_cand = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../drove-web/dist");
        if manifest_cand.join("index.html").is_file() {
            return Some(manifest_cand);
        }
    }
    None
}

fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn inject_web_token(bytes: &[u8], web_token: &str) -> Vec<u8> {
    let token = serde_json::to_string(web_token).unwrap_or_default().replace('<', "\\u003c");
    let script = format!("<script>window.__DROVE_TOKEN__={token};window.__CO_TOKEN__={token};</script>");
    let html = String::from_utf8_lossy(bytes);
    let injected = if html.contains("</head>") {
        html.replacen("</head>", &format!("{script}</head>"), 1)
    } else {
        format!("{script}{html}")
    };
    injected.into_bytes()
}

fn embedded_web_asset(rel: &str, safe_rel: bool) -> Option<(&'static str, &'static [u8])> {
    if safe_rel
        && !rel.is_empty()
        && let Some(found) = DROVE_WEB_ASSETS.iter().find(|(k, _)| *k == rel)
    {
        return Some(*found);
    }
    DROVE_WEB_ASSETS.iter().find(|(k, _)| *k == "index.html").copied()
}

async fn write_static_response(
    drove: &DroveRuntime,
    w: &mut tokio::net::tcp::OwnedWriteHalf,
    ext: &str,
    bytes: &[u8],
    head_only: bool,
) -> std::io::Result<()> {
    let mime = mime_for_ext(ext);
    if ext == "html" {
        let body = if head_only { Vec::new() } else { inject_web_token(bytes, &drove.web_token) };
        return write_http_bytes(w, 200, mime, "no-store", &body).await;
    }
    write_http_bytes(w, 200, mime, "no-cache", if head_only { &[] } else { bytes }).await
}

async fn serve_static(
    drove: &DroveRuntime,
    w: &mut tokio::net::tcp::OwnedWriteHalf,
    raw_path: &str,
    head_only: bool,
) -> std::io::Result<()> {
    let rel = raw_path.trim_start_matches('/');
    let rel_path = Path::new(rel);
    let safe_rel = rel_path.components().all(|c| matches!(c, Component::Normal(_)));

    let Some(root) = resolve_web_dist() else {
        if let Some((asset_rel, bytes)) = embedded_web_asset(rel, safe_rel) {
            let ext = Path::new(asset_rel).extension().and_then(|s| s.to_str()).unwrap_or("");
            return write_static_response(drove, w, ext, bytes, head_only).await;
        }
        let token = serde_json::to_string(&drove.web_token).unwrap_or_default().replace('<', "\\u003c");
        let html = format!(
            "<!doctype html><html><head><meta charset='utf-8'><title>Drove</title><script>window.__DROVE_TOKEN__={token};window.__CO_TOKEN__={token};</script></head>\
             <body style='font-family:system-ui;padding:24px'><h1>Drove</h1><p>Drove API is running. Web bundle not found; run <code>npm run build</code> in <code>drove-web/</code>.</p></body></html>"
        );
        return write_http_bytes(
            w,
            200,
            "text/html; charset=utf-8",
            "no-store",
            if head_only { &[] } else { html.as_bytes() },
        )
        .await;
    };

    let mut target = if safe_rel && !rel.is_empty() { root.join(rel_path) } else { root.join("index.html") };
    if !target.is_file() {
        target = root.join("index.html");
    }
    let Ok(canon_root) = root.canonicalize() else {
        return write_json(w, 404, &json!({"error":"not_found"})).await;
    };
    let Ok(canon_target) = target.canonicalize() else {
        return write_json(w, 404, &json!({"error":"not_found"})).await;
    };
    if !canon_target.starts_with(&canon_root) {
        return write_json(w, 403, &json!({"error":"forbidden"})).await;
    }

    let ext = canon_target.extension().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let bytes =
        tokio::task::spawn_blocking(move || std::fs::read(&canon_target)).await.map_err(std::io::Error::other)??;
    write_static_response(drove, w, &ext, &bytes, head_only).await
}

async fn write_json(w: &mut tokio::net::tcp::OwnedWriteHalf, status: u16, val: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(val).unwrap_or_else(|_| b"{}".to_vec());
    write_http_bytes(w, status, "application/json; charset=utf-8", "no-store", &body).await
}

async fn write_http_bytes(
    w: &mut tokio::net::tcp::OwnedWriteHalf,
    status: u16,
    content_type: &str,
    cache_control: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncache-control: {cache_control}\r\nx-content-type-options: nosniff\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    w.write_all(head.as_bytes()).await?;
    if !body.is_empty() {
        w.write_all(body).await?;
    }
    w.flush().await
}

pub(crate) fn _keep_read_ai_key_unused_silence() -> usize {
    let _ = read_ai_key;
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_web_bundle_contains_index_and_assets() {
        let web_pkg = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../drove-web/package.json");
        if !web_pkg.is_file() || std::env::var("H2_SKIP_WEB_BUILD").as_deref() == Ok("1") {
            return;
        }
        let (index_rel, index_bytes) = embedded_web_asset("", true).expect("DROVE_WEB_ASSETS should embed index.html");
        assert_eq!(index_rel, "index.html");
        let html = String::from_utf8_lossy(index_bytes);
        assert!(html.contains("<div id=\"root\"></div>"), "unexpected index.html: {html}");

        let injected = String::from_utf8(inject_web_token(index_bytes, "tok-123")).unwrap();
        assert!(injected.contains("window.__DROVE_TOKEN__=\"tok-123\""));
        assert!(injected.contains("</head>"));

        let has_js =
            DROVE_WEB_ASSETS.iter().any(|(k, b)| k.starts_with("assets/") && k.ends_with(".js") && !b.is_empty());
        let has_css =
            DROVE_WEB_ASSETS.iter().any(|(k, b)| k.starts_with("assets/") && k.ends_with(".css") && !b.is_empty());
        assert!(has_js, "embedded bundle should include JS asset");
        assert!(has_css, "embedded bundle should include CSS asset");

        // Unknown SPA routes fall back to index.html; traversal attempts also safely fall back.
        assert_eq!(embedded_web_asset("runs/r1", true).map(|(k, _)| k), Some("index.html"));
        assert_eq!(embedded_web_asset("../secret", false).map(|(k, _)| k), Some("index.html"));
    }
}
