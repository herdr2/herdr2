#!/usr/bin/env python3
"""Programmable ACP v1 stdio agent test double for herdr2 integration tests."""

import json
import os
import queue
import re
import subprocess
import sys
import threading
import time

stdout_lock = threading.Lock()
cancel_event = threading.Event()
work_queue: queue.Queue = queue.Queue()
pending_lock = threading.Lock()
pending_replies: dict[str, tuple[threading.Event, dict]] = {}
next_rpc_id = 1000

client_caps: dict = {}
session_id: str = ""
was_loaded: bool = False
mcp_servers: list = []
system_prompt: str | None = None
current_model: str | None = None
current_mode: str | None = None
reasoning_effort: str = "high"
fast_mode: bool = False
last_cwd: str = ""
reject_load = "--reject-load" in sys.argv
no_load_cap = "--no-load-cap" in sys.argv
hang_init = "--hang-init" in sys.argv


def send(obj: dict) -> None:
    line = json.dumps(obj, separators=(",", ":")) + "\n"
    with stdout_lock:
        sys.stdout.write(line)
        sys.stdout.flush()


def notify(method: str, params: dict) -> None:
    send({"jsonrpc": "2.0", "method": method, "params": params})


def reply_ok(rpc_id, result: dict) -> None:
    send({"jsonrpc": "2.0", "id": rpc_id, "result": result})


def reply_err(rpc_id, code: int, message: str) -> None:
    send({"jsonrpc": "2.0", "id": rpc_id, "error": {"code": code, "message": message}})


def call_client(method: str, params: dict, timeout: float = 30.0) -> dict:
    global next_rpc_id
    with pending_lock:
        next_rpc_id += 1
        cid = f"cli-{next_rpc_id}"
        ev = threading.Event()
        pending_replies[cid] = (ev, {})
    send({"jsonrpc": "2.0", "id": cid, "method": method, "params": params})
    if not ev.wait(timeout):
        raise TimeoutError(f"timed out waiting for client reply to {method}")
    with pending_lock:
        _, msg = pending_replies.pop(cid)
    if "error" in msg:
        raise RuntimeError(f"client error: {msg['error']}")
    return msg.get("result", {})


def emit_text(text: str) -> None:
    notify(
        "session/update",
        {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": text},
            },
        },
    )


def request_permission(tool_id: str, title: str, kind: str) -> dict:
    notify(
        "session/update",
        {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": tool_id,
                "title": title,
                "kind": kind,
                "status": "in_progress",
            },
        },
    )
    res = call_client(
        "session/request_permission",
        {
            "sessionId": session_id,
            "toolCall": {"toolCallId": tool_id, "title": title, "kind": kind},
            "options": [
                {"optionId": "allow-once", "name": "Allow once", "kind": "allow_once"},
                {"optionId": "allow-always", "name": "Allow always", "kind": "allow_always"},
                {"optionId": "reject-once", "name": "Reject once", "kind": "reject_once"},
            ],
        },
    )
    outcome = res.get("outcome", {})
    status = "completed" if outcome.get("outcome") == "selected" and str(outcome.get("optionId", "")).startswith("allow") else "failed"
    notify(
        "session/update",
        {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": tool_id,
                "status": status,
            },
        },
    )
    return outcome


def call_mcp(tool_name: str, args: dict) -> str:
    srv = next((s for s in mcp_servers if s.get("name") == "herdr2"), None)
    if not srv:
        raise RuntimeError("herdr2 MCP server not injected")
    env = os.environ.copy()
    for item in srv.get("env", []):
        env[item["name"]] = item["value"]
    proc = subprocess.Popen(
        [srv["command"], *srv.get("args", [])],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        text=True,
    )
    assert proc.stdin and proc.stdout
    try:
        proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}}) + "\n")
        proc.stdin.flush()
        proc.stdout.readline()
        proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": tool_name, "arguments": args}}) + "\n")
        proc.stdin.flush()
        line = proc.stdout.readline()
        proc.stdin.close()
        proc.wait(timeout=5)
        resp = json.loads(line)
        content = resp.get("result", {}).get("content", [])
        is_err = resp.get("result", {}).get("isError", False)
        text = content[0].get("text", "") if content else json.dumps(resp)
        if is_err:
            return f"MCP_ERROR:{text}"
        return f"MCP_RESULT:{text}"
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()


def strip_wrapper(text: str) -> str:
    lines = text.strip().splitlines()
    if lines and lines[0].startswith("<h2") and lines[-1].strip() == "</h2>":
        return "\n".join(lines[1:-1]).strip()
    m = re.match(r"^<h2\b[^>]*>(.*)</h2>$", text.strip(), re.S)
    if m:
        return m.group(1).strip()
    return text.strip()


def handle_prompt(rpc_id, params: dict) -> None:
    cancel_event.clear()
    blocks = params.get("prompt", [])
    raw = "\n".join(b.get("text", "") for b in blocks if isinstance(b, dict))
    inner = strip_wrapper(raw)

    outputs: list[str] = []
    for line in [l.strip() for l in inner.splitlines() if l.strip()]:
        if line.startswith("EXIT:"):
            code = int(line.split(":", 1)[1])
            os._exit(code)
        elif line == "HOLD":
            while not cancel_event.is_set():
                time.sleep(0.05)
            emit_text("cancelled\n")
            reply_ok(rpc_id, {"stopReason": "cancelled"})
            return
        elif line.startswith("SLEEP:"):
            ms = int(line.split(":", 1)[1])
            deadline = time.monotonic() + (ms / 1000.0)
            while time.monotonic() < deadline:
                if cancel_event.is_set():
                    emit_text("cancelled\n")
                    reply_ok(rpc_id, {"stopReason": "cancelled"})
                    return
                time.sleep(0.02)
            outputs.append(f"slept:{ms}")
        elif line == "READ_TOOL":
            out = request_permission("tc-read", "Read README.md", "read")
            outputs.append(f"read_tool={out.get('outcome')}:{out.get('optionId', '')}")
        elif line == "WRITE_TOOL":
            out = request_permission("tc-write", "Edit output.txt", "edit")
            outputs.append(f"write_tool={out.get('outcome')}:{out.get('optionId', '')}")
        elif line == "EXEC_TOOL":
            out = request_permission("tc-exec", "Execute command", "execute")
            outputs.append(f"exec_tool={out.get('outcome')}:{out.get('optionId', '')}")
        elif line.startswith("MCP_CALL:"):
            _, rest = line.split(":", 1)
            tool_name, args_json = rest.split(":", 1)
            outputs.append(call_mcp(tool_name, json.loads(args_json)))
        elif line.startswith("ENV:"):
            key = line.split(":", 1)[1]
            outputs.append(f"ENV:{key}={os.environ.get(key, '<unset>')}")
        elif line == "STATE":
            outputs.append(
                json.dumps(
                    {
                        "session_id": session_id,
                        "loaded": was_loaded,
                        "model": current_model,
                        "mode": current_mode,
                        "reasoning_effort": reasoning_effort,
                        "fast_mode": fast_mode,
                        "mcp_servers": mcp_servers,
                        "system_prompt": system_prompt,
                        "client_caps": client_caps,
                        "raw": raw,
                    }
                )
            )
        elif line.startswith("SH:"):
            cmd = line.split(":", 1)[1]
            res = subprocess.run(cmd, shell=True, cwd=last_cwd or None, text=True, capture_output=True)
            if res.stdout:
                emit_text(res.stdout)
            if res.stderr:
                emit_text(res.stderr)
            outputs.append(f"sh:{res.returncode}")
        else:
            outputs.append(f"echo: {line}")

    if cancel_event.is_set():
        emit_text("cancelled\n")
        reply_ok(rpc_id, {"stopReason": "cancelled"})
        return

    emit_text("\n".join(outputs) + "\n")
    reply_ok(rpc_id, {"stopReason": "end_turn"})


def current_config_options() -> list[dict]:
    return [
        {
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": current_model or "gpt-test-1",
            "options": [
                {"value": "gpt-test-1", "name": "GPT Test 1"},
                {"value": "gpt-test-2", "name": "GPT Test 2"},
            ],
        },
        {
            "id": "reasoning_effort",
            "name": "Reasoning effort",
            "category": "thought_level",
            "type": "select",
            "currentValue": reasoning_effort,
            "options": [
                {"value": "low", "name": "Low"},
                {"value": "medium", "name": "Medium"},
                {"value": "high", "name": "High"},
                {"value": "xhigh", "name": "Extra High"},
            ],
        },
        {
            "id": "fast-mode",
            "name": "Fast mode",
            "category": "model_config",
            "type": "boolean",
            "currentValue": fast_mode,
        },
        {
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "type": "select",
            "currentValue": current_mode or "agent",
            "options": [
                {"value": "agent", "name": "Auto review"},
                {"value": "plan", "name": "Plan"},
                {"value": "bypassPermissions", "name": "Bypass permissions"},
            ],
        },
    ]


def worker() -> None:
    global client_caps, session_id, was_loaded, mcp_servers, system_prompt, current_model, current_mode, reasoning_effort, fast_mode, last_cwd
    while True:
        msg = work_queue.get()
        if msg is None:
            return
        rpc_id = msg.get("id")
        method = msg.get("method")
        params = msg.get("params") or {}
        try:
            if method == "initialize":
                if hang_init:
                    time.sleep(60)
                client_caps = params.get("clientCapabilities", {})
                fs = client_caps.get("fs", {})
                if fs.get("readTextFile") is not False or fs.get("writeTextFile") is not False or client_caps.get("terminal") is not False:
                    reply_err(rpc_id, -32602, f"expected fs=false, terminal=false; got {client_caps}")
                    continue
                reply_ok(
                    rpc_id,
                    {
                        "protocolVersion": 1,
                        "agentInfo": {"name": "fake-acp", "version": "1.0"},
                        "agentCapabilities": {"loadSession": not no_load_cap},
                    },
                )
            elif method == "session/new":
                last_cwd = params.get("cwd", "")
                mcp_servers = params.get("mcpServers", [])
                meta = params.get("_meta") or {}
                sp = meta.get("systemPrompt")
                if isinstance(sp, dict):
                    system_prompt = sp.get("append")
                session_id = f"sess-{os.getpid()}-{int(time.time() * 1000)}"
                was_loaded = False
                reply_ok(rpc_id, {"sessionId": session_id, "configOptions": current_config_options()})
            elif method == "session/load":
                if reject_load:
                    reply_err(rpc_id, -32000, "load rejected")
                    continue
                last_cwd = params.get("cwd", "")
                mcp_servers = params.get("mcpServers", [])
                session_id = params.get("sessionId", session_id)
                was_loaded = True
                reply_ok(rpc_id, {"sessionId": session_id, "configOptions": current_config_options()})
            elif method == "session/set_mode":
                mode_id = params.get("modeId")
                if mode_id == "REJECT":
                    reply_err(rpc_id, -32602, "invalid mode")
                else:
                    current_mode = mode_id
                    reply_ok(rpc_id, {})
            elif method == "session/set_config_option":
                cfg_id = params.get("configId")
                val = params.get("value")
                if cfg_id == "model":
                    if val == "REJECT":
                        reply_err(rpc_id, -32602, "invalid model")
                        continue
                    current_model = val
                elif cfg_id == "reasoning_effort":
                    reasoning_effort = str(val)
                elif cfg_id == "fast-mode":
                    fast_mode = bool(val)
                elif cfg_id == "mode":
                    current_mode = str(val)
                reply_ok(rpc_id, {"configOptions": current_config_options()})
            elif method == "session/set_model":
                current_model = params.get("modelId")
                reply_ok(rpc_id, {})
            elif method == "session/prompt":
                handle_prompt(rpc_id, params)
            else:
                if rpc_id is not None:
                    reply_err(rpc_id, -32601, f"method not found: {method}")
        except Exception as exc:  # noqa: BLE001
            if rpc_id is not None:
                reply_err(rpc_id, -32000, f"agent exception: {exc}")


t = threading.Thread(target=worker, daemon=True)
t.start()

for raw_line in sys.stdin:
    raw_line = raw_line.strip()
    if not raw_line:
        continue
    try:
        msg = json.loads(raw_line)
    except json.JSONDecodeError:
        continue
    if "method" in msg:
        if msg["method"] == "session/cancel":
            cancel_event.set()
        else:
            work_queue.put(msg)
    elif "id" in msg:
        key = str(msg["id"])
        with pending_lock:
            entry = pending_replies.get(key)
        if entry is not None:
            ev, box = entry
            box.update(msg)
            ev.set()

work_queue.put(None)
t.join(timeout=2)
