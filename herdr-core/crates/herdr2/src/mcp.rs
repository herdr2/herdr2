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
    let paths = Paths::from_env()?;
    let stdin = io::stdin();
    let stdout = io::stdout();
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
            let val: Value =
                serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
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
            let protocol =
                params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2024-11-05");
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

fn tool_definitions() -> Value {
    json!([
        {
            "name": "h2_run",
            "description": "Start a subordinate ACP agent session (preset or custom command).",
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
            "description": "Queue a prompt for an ACP agent (cannot target PTY terminals; terminal agents receive ACP results via h2 wait).",
            "inputSchema": {
                "type": "object",
                "required": ["agent", "text"],
                "properties": {
                    "agent": { "type": "string" },
                    "text": { "type": "string" },
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
}
