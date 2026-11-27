//! Built-in stdio MCP server (`h2 mcp`), auto-injected into ACP agents by default.

use crate::client;
use crate::paths::Paths;
use crate::rpc::RpcError;
use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Framing {
    ContentLength,
    Ndjson,
}

/// Serves MCP JSON-RPC 2.0 over stdin/stdout, auto-detecting Content-Length vs NDJSON framing.
pub fn serve_stdio() -> Result<(), RpcError> {
    serve_stdio_profile(None)
}

/// Serves MCP JSON-RPC 2.0 for the requested tool surface profile (`None`/`"default"` or `"drove-worker"`).
pub fn serve_stdio_profile(profile: Option<&str>) -> Result<(), RpcError> {
    let effective = profile
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("H2_MCP_PROFILE").ok().map(|s| s.trim().to_ascii_lowercase()));
    let stdin = io::stdin();
    let stdout = io::stdout();
    if effective.as_deref() == Some("drove-worker") {
        return serve_drove_worker_streams(BufReader::new(stdin.lock()), stdout.lock(), &drove_worker_http_post)
            .map_err(|e| RpcError::internal(format!("drove-worker mcp stdio failed: {e}")));
    }
    let paths = Paths::from_env()?;
    serve_streams(BufReader::new(stdin.lock()), stdout.lock(), &|method, params| {
        let stream = client::connect_or_start(&paths)?;
        client::call(&stream, method, params).map_err(|e| client::explain_closed(&paths, e))
    })
    .map_err(|e| RpcError::internal(format!("mcp stdio failed: {e}")))
}

fn serve_streams<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    call: &impl Fn(&str, Value) -> Result<Value, RpcError>,
) -> io::Result<()> {
    let mut framing: Option<Framing> = None;
    while let Some((msg, detected)) = read_message(&mut reader, framing)? {
        if framing.is_none() {
            framing = Some(detected);
        }
        let active_framing = framing.unwrap_or(detected);
        if let Some(reply) = handle_json_rpc(msg, call) {
            write_message(&mut writer, &reply, active_framing)?;
        }
    }
    Ok(())
}

fn read_message<R: BufRead>(reader: &mut R, hint: Option<Framing>) -> io::Result<Option<(Value, Framing)>> {
    let mut first = String::new();
    loop {
        first.clear();
        if reader.read_line(&mut first)? == 0 {
            return Ok(None);
        }
        let trimmed = first.trim();
        if trimmed.is_empty() {
            continue;
        }
        if hint == Some(Framing::ContentLength) || first.to_ascii_lowercase().starts_with("content-length:") {
            let mut content_length: Option<usize> = parse_content_length(&first);
            let mut header = String::new();
            loop {
                header.clear();
                if reader.read_line(&mut header)? == 0 {
                    return Ok(None);
                }
                if header.trim().is_empty() {
                    break;
                }
                if let Some(len) = parse_content_length(&header) {
                    content_length = Some(len);
                }
            }
            let len = content_length
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header"))?;
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf)?;
            let val: Value = serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            return Ok(Some((val, Framing::ContentLength)));
        }
        let val: Value = serde_json::from_str(trimmed).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        return Ok(Some((val, Framing::Ndjson)));
    }
}

fn parse_content_length(line: &str) -> Option<usize> {
    let (k, v) = line.split_once(':')?;
    k.trim().eq_ignore_ascii_case("content-length").then(|| v.trim().parse().ok()).flatten()
}

fn write_message<W: Write>(writer: &mut W, msg: &Value, framing: Framing) -> io::Result<()> {
    let json = serde_json::to_string(msg).map_err(io::Error::other)?;
    match framing {
        Framing::ContentLength => {
            write!(writer, "Content-Length: {}\r\n\r\n{}", json.len(), json)?;
        }
        Framing::Ndjson => {
            writer.write_all(json.as_bytes())?;
            writer.write_all(b"\n")?;
        }
    }
    writer.flush()
}

fn handle_json_rpc(msg: Value, call: &impl Fn(&str, Value) -> Result<Value, RpcError>) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str);
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));

    let Some(id) = id else {
        // Notification (e.g. notifications/initialized): no response.
        return None;
    };

    let result = match method {
        Some("initialize") => {
            let protocol = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2024-11-05");
            json!({
                "protocolVersion": protocol,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "herdr2", "version": env!("CARGO_PKG_VERSION") }
            })
        }
        Some("ping") => json!({}),
        Some("tools/list") => json!({ "tools": tool_definitions() }),
        Some("tools/call") => dispatch_tool(&params, call),
        Some(other) => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("unknown MCP method {other:?}") }
            }));
        }
        None => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32600, "message": "invalid JSON-RPC request" }
            }));
        }
    };

    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn dispatch_tool(params: &Value, call: &impl Fn(&str, Value) -> Result<Value, RpcError>) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let me = client::agent_id().or_else(client::terminal_id);

    let rpc = match name {
        "h2_run" => {
            let mut obj = args.as_object().cloned().unwrap_or_default();
            if let Some(me) = &me {
                if !obj.contains_key("parent") {
                    obj.insert("parent".into(), json!(me));
                }
                if !obj.contains_key("caller") {
                    obj.insert("caller".into(), json!(me));
                }
            }
            ("agent.run", Value::Object(obj))
        }
        "h2_send" => {
            let mut obj = args.as_object().cloned().unwrap_or_default();
            if let Some(me) = &me {
                if !obj.contains_key("from") {
                    obj.insert("from".into(), json!(me));
                }
                if !obj.contains_key("caller") {
                    obj.insert("caller".into(), json!(me));
                }
            }
            ("agent.send", Value::Object(obj))
        }
        "h2_wait" => {
            let mut obj = args.as_object().cloned().unwrap_or_default();
            if let Some(caller) = &me
                && !obj.contains_key("caller")
            {
                obj.insert("caller".into(), json!(caller));
            }
            ("agent.wait", Value::Object(obj))
        }
        "h2_read" => {
            let mut obj = args.as_object().cloned().unwrap_or_default();
            if let Some(caller) = &me
                && !obj.contains_key("caller")
            {
                obj.insert("caller".into(), json!(caller));
            }
            ("agent.read", Value::Object(obj))
        }
        "h2_handoff" => {
            let mut obj = args.as_object().cloned().unwrap_or_default();
            if let Some(caller) = &me
                && !obj.contains_key("caller")
            {
                obj.insert("caller".into(), json!(caller));
            }
            ("agent.handoff", Value::Object(obj))
        }
        "h2_ls" => {
            let mut obj = args.as_object().cloned().unwrap_or_default();
            if !obj.contains_key("labels")
                && let Some(raw) = obj.remove("label").and_then(|v| v.as_str().map(str::to_string))
                && let Some((k, v)) = raw.split_once('=')
                && !k.is_empty()
            {
                obj.insert("labels".into(), json!({ k: v }));
            }
            ("agent.list", Value::Object(obj))
        }
        "h2_approve" => {
            let mut obj = args.as_object().cloned().unwrap_or_default();
            if let Some(caller) = &me
                && !obj.contains_key("caller")
            {
                obj.insert("caller".into(), json!(caller));
            }
            ("agent.approve", Value::Object(obj))
        }
        "h2_deny" => {
            let mut obj = args.as_object().cloned().unwrap_or_default();
            if let Some(caller) = &me
                && !obj.contains_key("caller")
            {
                obj.insert("caller".into(), json!(caller));
            }
            ("agent.deny", Value::Object(obj))
        }
        other => {
            return tool_error(format!("unknown tool {other:?}"));
        }
    };

    match call(rpc.0, rpc.1) {
        Ok(res) => {
            let text = serde_json::to_string_pretty(&res).unwrap_or_else(|_| res.to_string());
            json!({ "content": [{ "type": "text", "text": text }], "isError": false })
        }
        Err(err) => {
            let msg = if err.hint.is_empty() {
                format!("{}: {}", err.kind, err.message)
            } else {
                format!("{}: {} ({})", err.kind, err.message, err.hint)
            };
            tool_error(msg)
        }
    }
}

fn tool_error(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

fn serve_drove_worker_streams<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    worker_http: &impl Fn(&str, Value) -> Result<Value, RpcError>,
) -> io::Result<()> {
    let mut framing: Option<Framing> = None;
    while let Some((msg, detected)) = read_message(&mut reader, framing)? {
        if framing.is_none() {
            framing = Some(detected);
        }
        let active_framing = framing.unwrap_or(detected);
        if let Some(reply) = handle_drove_worker_rpc(msg, worker_http) {
            write_message(&mut writer, &reply, active_framing)?;
        }
    }
    Ok(())
}

fn handle_drove_worker_rpc(
    msg: Value,
    worker_http: &impl Fn(&str, Value) -> Result<Value, RpcError>,
) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str);
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));

    let result = match method {
        Some("initialize") => {
            let protocol = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2024-11-05");
            json!({
                "protocolVersion": protocol,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "drove", "version": env!("CARGO_PKG_VERSION") }
            })
        }
        Some("ping") => json!({}),
        Some("tools/list") => {
            let active_schema = worker_http("/api/worker/context", json!({}))
                .ok()
                .and_then(|ctx| ctx.get("outputSchema").cloned())
                .filter(|s| !s.is_null());
            json!({ "tools": drove_worker_tool_definitions(active_schema.as_ref()) })
        }
        Some("tools/call") => dispatch_drove_worker_tool(&params, worker_http),
        Some(other) => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("unknown MCP method {other:?}") }
            }));
        }
        None => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32600, "message": "invalid JSON-RPC request" }
            }));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn dispatch_drove_worker_tool(
    params: &Value,
    worker_http: &impl Fn(&str, Value) -> Result<Value, RpcError>,
) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));

    let (endpoint, body) = match name {
        "drove_finish" => {
            let outcome = args.get("outcome").and_then(Value::as_str).unwrap_or("done");
            let mut req = json!({ "outcome": outcome });
            if let Some(s) = args.get("summary").and_then(Value::as_str) {
                req["summary"] = Value::String(s.to_string());
            }
            if let Some(r) = args.get("reason").and_then(Value::as_str) {
                req["reason"] = Value::String(r.to_string());
            }
            if let Some(d) = args.get("data").filter(|v| !v.is_null()) {
                req["data"] = d.clone();
            }
            if let Some(f) = args.get("files").and_then(Value::as_array) {
                req["filesModified"] = Value::Array(f.clone());
            }
            if let Some(nr) = args.get("no_retry").and_then(Value::as_bool) {
                req["noRetry"] = Value::Bool(nr);
            }
            ("/api/worker/done", req)
        }
        "drove_ask" => {
            let mut req = json!({
                "question": args.get("question").and_then(Value::as_str).unwrap_or_default(),
            });
            if let Some(opts) = args.get("options").and_then(Value::as_array) {
                req["options"] = Value::Array(opts.clone());
            }
            if let Some(rec) = args.get("recommend").and_then(Value::as_str) {
                req["recommend"] = Value::String(rec.to_string());
            }
            if let Some(why) = args.get("why").and_then(Value::as_str) {
                req["why"] = Value::String(why.to_string());
            }
            if let Some(t) = args.get("timeout").and_then(Value::as_u64) {
                req["timeoutSec"] = Value::Number(t.into());
            }
            ("/api/worker/ask", req)
        }
        "drove_context" => {
            let action = args.get("action").and_then(Value::as_str).unwrap_or("read");
            match action {
                "read" => {
                    let mut req = json!({
                        "target": args.get("target").and_then(Value::as_str).unwrap_or_default(),
                    });
                    if let Some(jp) = args.get("json_path").or_else(|| args.get("path")).and_then(Value::as_str) {
                        req["jsonPath"] = Value::String(jp.to_string());
                    }
                    if let Some(off) = args.get("offset").and_then(Value::as_u64) {
                        req["offset"] = Value::Number(off.into());
                    }
                    if let Some(lim) = args.get("limit").and_then(Value::as_u64) {
                        req["limit"] = Value::Number(lim.into());
                    }
                    if let Some(g) = args.get("grep").and_then(Value::as_str) {
                        req["grep"] = Value::String(g.to_string());
                    }
                    ("/api/worker/read", req)
                }
                "publish" => (
                    "/api/worker/contract",
                    json!({
                        "action": "publish",
                        "channel": args.get("channel").and_then(Value::as_str).unwrap_or("main"),
                        "key": args.get("key").and_then(Value::as_str).unwrap_or_default(),
                        "summary": args.get("summary").and_then(Value::as_str).unwrap_or_default(),
                        "payload": args.get("payload").cloned().unwrap_or(Value::Null),
                    }),
                ),
                "list" => (
                    "/api/worker/contract",
                    json!({
                        "action": "list",
                        "channel": args.get("channel").and_then(Value::as_str),
                    }),
                ),
                "note" => (
                    "/api/worker/note",
                    json!({
                        "text": args.get("summary").or_else(|| args.get("text")).and_then(Value::as_str).unwrap_or_default(),
                        "subject": args.get("key").and_then(Value::as_str),
                    }),
                ),
                "inbox" => ("/api/worker/inbox", json!({})),
                other => return tool_error(format!("unknown drove_context action {other:?}")),
            }
        }
        other => return tool_error(format!("unknown tool {other:?}")),
    };

    match worker_http(endpoint, body) {
        Ok(res) => {
            let text = serde_json::to_string_pretty(&res).unwrap_or_else(|_| res.to_string());
            json!({ "content": [{ "type": "text", "text": text }], "isError": false })
        }
        Err(err) => {
            let msg = if err.hint.is_empty() {
                format!("{}: {}", err.kind, err.message)
            } else {
                format!("{}: {} ({})", err.kind, err.message, err.hint)
            };
            tool_error(msg)
        }
    }
}

/// Builds the ultra-compact 3-tool MCP definition for Drove workers, dynamically inlining
/// the active task's `outputSchema` into `drove_finish.inputSchema.properties.data`.
pub fn drove_worker_tool_definitions(output_schema: Option<&Value>) -> Value {
    let data_prop = match output_schema.filter(|s| s.is_object()) {
        Some(schema) => schema.clone(),
        None => json!({
            "type": "object",
            "description": "Optional structured JSON result payload"
        }),
    };
    let finish_desc = if output_schema.is_some() {
        "Report task completion (`done`) or explicit failure (`failed`) to Drove gates. `data` MUST satisfy the embedded schema when `outcome` is `done`."
    } else {
        "Report task completion (`done`) or explicit failure (`failed`) to Drove verification/review gates."
    };
    json!([
        {
            "name": "drove_finish",
            "description": finish_desc,
            "inputSchema": {
                "type": "object",
                "required": ["outcome"],
                "properties": {
                    "outcome": { "type": "string", "enum": ["done", "failed"] },
                    "summary": { "type": "string", "description": "Concise summary of changes and verification (required when outcome is done)" },
                    "data": data_prop,
                    "reason": { "type": "string", "description": "Failure reason when outcome is failed" },
                    "files": { "type": "array", "items": { "type": "string" } }
                }
            }
        },
        {
            "name": "drove_ask",
            "description": "Ask the Coordinator a blocking question when requirements are ambiguous and return the reply.",
            "inputSchema": {
                "type": "object",
                "required": ["question"],
                "properties": {
                    "question": { "type": "string" },
                    "options": { "type": "array", "items": { "type": "string" } },
                    "recommend": { "type": "string" },
                    "why": { "type": "string" }
                }
            }
        },
        {
            "name": "drove_context",
            "description": "Read upstream `@task:<key>#data` / `#contract:<seq>` / `verify` / `review` slices, publish/list shared channel contracts, or post progress notes.",
            "inputSchema": {
                "type": "object",
                "required": ["action"],
                "properties": {
                    "action": { "type": "string", "enum": ["read", "publish", "list", "note", "inbox"] },
                    "target": { "type": "string", "description": "Read target: task key, `#contract:<seq_or_key>`, `verify`, or `review`" },
                    "json_path": { "type": "string", "description": "Optional dot path (e.g. `items.0`) when reading task/contract JSON" },
                    "channel": { "type": "string", "description": "Contract channel name (default `main`)" },
                    "key": { "type": "string", "description": "Contract key (for `publish`) or note subject" },
                    "summary": { "type": "string", "description": "1-line contract summary or progress note text" },
                    "payload": { "type": "object", "description": "Structured contract JSON payload for `publish`" },
                    "grep": { "type": "string" }
                }
            }
        }
    ])
}

fn drove_worker_http_post(endpoint: &str, mut body: Value) -> Result<Value, RpcError> {
    use std::io::Read;
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;

    let raw_url = std::env::var("H2_DROVE_URL").unwrap_or_else(|_| "http://127.0.0.1:7799".into());
    let token = std::env::var("H2_DROVE_WORKER_TOKEN")
        .or_else(|_| std::env::var("H2_DROVE_WORKER_TICKET"))
        .map_err(|_| RpcError::new("unauthorized", "H2_DROVE_WORKER_TOKEN is not set", ""))?;
    let worker_id = std::env::var("H2_DROVE_WORKER_ID").unwrap_or_default();
    if let Some(obj) = body.as_object_mut()
        && !worker_id.trim().is_empty()
        && !obj.contains_key("workerId")
    {
        obj.insert("workerId".into(), Value::String(worker_id.clone()));
    }

    let stripped = raw_url.trim().strip_prefix("http://").unwrap_or(raw_url.trim());
    let host_port = stripped.split('/').next().unwrap_or("127.0.0.1:7799");
    let addr: SocketAddr = host_port
        .parse()
        .or_else(|_| {
            let port = host_port.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok()).unwrap_or(7799);
            Ok::<SocketAddr, std::net::AddrParseError>(SocketAddr::from(([127, 0, 0, 1], port)))
        })
        .map_err(|e| RpcError::internal(format!("invalid H2_DROVE_URL {raw_url:?}: {e}")))?;

    let timeout = if endpoint == "/api/worker/context" { Duration::from_secs(3) } else { Duration::from_secs(600) };
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))
        .map_err(|e| RpcError::new("unavailable", format!("failed connecting to Drove {addr}: {e}"), ""))?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));

    let payload = serde_json::to_vec(&body).map_err(|e| RpcError::internal(e.to_string()))?;
    let req = format!(
        "POST {endpoint} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\nContent-Type: application/json\r\nx-drove-worker-token: {token}\r\nx-drove-worker-id: {worker_id}\r\nContent-Length: {}\r\n\r\n",
        payload.len()
    );
    stream.write_all(req.as_bytes()).and_then(|_| stream.write_all(&payload)).and_then(|_| stream.flush()).map_err(
        |e| RpcError::new("unavailable", format!("failed sending worker HTTP request: {e}"), ""),
    )?;

    let mut raw_resp = Vec::new();
    stream
        .read_to_end(&mut raw_resp)
        .map_err(|e| RpcError::new("unavailable", format!("failed reading worker HTTP response: {e}"), ""))?;
    let sep = raw_resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| RpcError::internal("malformed HTTP response from Drove"))?;
    let head = String::from_utf8_lossy(&raw_resp[..sep]);
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(500);
    let resp_val: Value = serde_json::from_slice(&raw_resp[sep + 4..]).unwrap_or_else(|_| json!({}));
    if status >= 400 {
        let code = resp_val["error"].as_str().unwrap_or("drove_error");
        let msg = resp_val["message"].as_str().unwrap_or("worker request rejected");
        return Err(RpcError::new(code, msg, ""));
    }
    Ok(resp_val)
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "h2_run",
            "description": "Start a subordinate ACP agent session (preset or custom command). Child sessions stay warm after a turn and auto-exit when their parent closes; check h2_ls and prefer reusing an existing idle child via h2_send for related follow-up tasks instead of spawning a new session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "description": "Preset name (claude, codex, gemini, opencode, etc.) or command string" },
                    "argv": { "type": "array", "items": { "type": "string" }, "description": "Explicit command argv if agent is omitted" },
                    "prompt": { "type": "string", "description": "Initial prompt queued immediately" },
                    "name": { "type": "string", "description": "Unique name for addressing this agent" },
                    "cwd": { "type": "string", "description": "Working directory" },
                    "model": { "type": "string" },
                    "mode": { "type": "string" },
                    "permissions": {
                        "type": "string",
                        "enum": ["approve-all", "approve-reads", "deny-all", "ask", "dangerously-skip-permissions", "bypassPermissions"]
                    },
                    "dangerously_skip_permissions": {
                        "type": "boolean",
                        "description": "Skip all permission checks (sets approve-all policy and bypassPermissions session mode when supported)"
                    },
                    "system_prompt": { "type": "string" },
                    "no_mcp": { "type": "boolean" },
                    "on": { "type": "string" },
                    "wait": { "type": "boolean" },
                    "timeout": { "type": "number" },
                    "keep": { "type": "boolean" },
                    "rm": { "type": "boolean" },
                    "no_notify": { "type": "boolean" }
                }
            }
        },
        {
            "name": "h2_send",
            "description": "Queue a text or multimodal prompt (images, resources, resource links) for a live ACP agent, reusing its warm session context and prompt cache for follow-up tasks (cannot target PTY terminals; terminal agents receive ACP results via h2 wait).",
            "inputSchema": {
                "type": "object",
                "required": ["agent"],
                "properties": {
                    "agent": { "type": "string" },
                    "text": { "type": "string", "description": "Prompt text (may be empty if content blocks are provided)" },
                    "content": {
                        "type": "array",
                        "description": "Optional ACP multimodal content blocks (image, audio, resource, resource_link, text)",
                        "items": { "type": "object" }
                    },
                    "steer": { "type": "boolean" },
                    "wait": { "type": "boolean" },
                    "timeout": { "type": "number" }
                }
            }
        },
        {
            "name": "h2_wait",
            "description": "Wait for an agent or children to finish their turn, block on permission, or exit.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent": { "type": "string" },
                    "children": { "type": "string", "enum": ["any", "all"] },
                    "output": { "type": "string" },
                    "timeout": { "type": "number" }
                }
            }
        },
        {
            "name": "h2_read",
            "description": "Read an agent's transcript (latest turn, --last assistant text, trailing history lines, or --from-turn/--to-turn slice).",
            "inputSchema": {
                "type": "object",
                "required": ["agent"],
                "properties": {
                    "agent": { "type": "string" },
                    "last": { "type": "boolean" },
                    "lines": { "type": "integer", "minimum": 1 },
                    "from_turn": { "type": "integer", "minimum": 1 },
                    "to_turn": { "type": "integer", "minimum": 1 },
                    "wait": { "type": "boolean" },
                    "timeout": { "type": "number" }
                }
            }
        },
        {
            "name": "h2_handoff",
            "description": "Hand off, fork, open a side chat, or merge back session context across ACP and PTY agents.",
            "inputSchema": {
                "type": "object",
                "required": ["source"],
                "properties": {
                    "source": { "type": "string" },
                    "kind": { "type": "string", "enum": ["handoff", "fork", "side-chat"] },
                    "preset": { "type": "string" },
                    "pty": { "type": "boolean" },
                    "into": { "type": "string" },
                    "from_turn": { "type": "integer", "minimum": 1 },
                    "to_turn": { "type": "integer", "minimum": 1 },
                    "prompt": { "type": "string" },
                    "note": { "type": "string" },
                    "cwd": { "type": "string" },
                    "name": { "type": "string" },
                    "model": { "type": "string" },
                    "keep": { "type": "boolean" },
                    "wait": { "type": "boolean" },
                    "timeout": { "type": "number" }
                }
            }
        },
        {
            "name": "h2_ls",
            "description": "List active (and optionally finished) agents.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "all": { "type": "boolean" },
                    "labels": { "type": "object", "additionalProperties": { "type": "string" } }
                }
            }
        },
        {
            "name": "h2_approve",
            "description": "Approve a blocked agent's pending ACP permission request.",
            "inputSchema": {
                "type": "object",
                "required": ["agent"],
                "properties": {
                    "agent": { "type": "string" },
                    "option": { "type": "string" }
                }
            }
        },
        {
            "name": "h2_deny",
            "description": "Deny a blocked agent's pending ACP permission request.",
            "inputSchema": {
                "type": "object",
                "required": ["agent"],
                "properties": {
                    "agent": { "type": "string" },
                    "option": { "type": "string" }
                }
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_ndjson_and_content_length_framing() {
        let reqs = format!(
            "{}\n{}\n",
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        );
        let mut out = Vec::new();
        serve_streams(BufReader::new(reqs.as_bytes()), &mut out, &|_, _| Ok(json!({}))).unwrap();
        let reply: Value = serde_json::from_str(std::str::from_utf8(&out).unwrap().trim()).unwrap();
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["serverInfo"]["name"], "herdr2");

        let body = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}).to_string();
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut framed_out = Vec::new();
        serve_streams(BufReader::new(framed.as_bytes()), &mut framed_out, &|_, _| Ok(json!({}))).unwrap();
        let text = std::str::from_utf8(&framed_out).unwrap();
        assert!(text.starts_with("Content-Length: "));
        let (_, json_part) = text.split_once("\r\n\r\n").unwrap();
        let list: Value = serde_json::from_str(json_part).unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 8);
        assert!(tools.iter().any(|t| t["name"] == "h2_handoff"));
    }

    #[test]
    fn routes_tool_calls_to_agent_rpc_methods() {
        let seen = std::cell::RefCell::new(Vec::new());
        let call = |method: &str, params: Value| {
            seen.borrow_mut().push((method.to_string(), params));
            Ok(json!({ "ok": true }))
        };
        let req = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": { "name": "h2_send", "arguments": { "agent": "a1", "text": "go", "steer": true } }
        });
        let resp = handle_json_rpc(req, &call).unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let seen = seen.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "agent.send");
        assert_eq!(seen[0].1["agent"], "a1");
        assert_eq!(seen[0].1["steer"], true);
    }

    #[test]
    fn drove_worker_profile_exposes_three_tools_with_dynamic_output_schema() {
        let calls = std::cell::RefCell::new(Vec::new());
        let mock_http = |endpoint: &str, body: Value| -> Result<Value, RpcError> {
            calls.borrow_mut().push((endpoint.to_string(), body.clone()));
            match endpoint {
                "/api/worker/context" => Ok(json!({
                    "workerId": "wrk_1",
                    "outputSchema": {
                        "type": "object",
                        "required": ["endpoints"],
                        "properties": { "endpoints": { "type": "array", "items": { "type": "string" } } }
                    }
                })),
                "/api/worker/done" => Ok(json!({ "ok": true })),
                "/api/worker/contract" => Ok(json!({ "seq": 1, "key": "api-v1" })),
                _ => Ok(json!({ "ok": true })),
            }
        };

        let list_resp = handle_drove_worker_rpc(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
            &mock_http,
        )
        .unwrap();
        let tools = list_resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, vec!["drove_finish", "drove_ask", "drove_context"]);
        assert_eq!(tools[0]["inputSchema"]["properties"]["data"]["required"][0], "endpoints");

        let finish_resp = handle_drove_worker_rpc(
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "drove_finish",
                    "arguments": { "outcome": "done", "summary": "Implemented API", "data": { "endpoints": ["/v1/ping"] } }
                }
            }),
            &mock_http,
        )
        .unwrap();
        assert_eq!(finish_resp["result"]["isError"], false);

        let pub_resp = handle_drove_worker_rpc(
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "drove_context",
                    "arguments": { "action": "publish", "channel": "api", "key": "api-v1", "summary": "Ping route", "payload": { "method": "GET" } }
                }
            }),
            &mock_http,
        )
        .unwrap();
        assert_eq!(pub_resp["result"]["isError"], false);
        let recorded = calls.borrow();
        assert_eq!(recorded[1].0, "/api/worker/done");
        assert_eq!(recorded[1].1["data"]["endpoints"][0], "/v1/ping");
        assert_eq!(recorded[2].0, "/api/worker/contract");
        assert_eq!(recorded[2].1["channel"], "api");
    }
}
