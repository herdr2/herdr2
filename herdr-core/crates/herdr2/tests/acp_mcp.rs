mod common;

use common::*;
use serde_json::Value;

fn state_json(s: &Session, a: &str) -> Value {
    s.json(&["send", "--wait", a, "STATE"]);
    let last = s.json(&["read", "--last", a])["text"].as_str().unwrap().to_owned();
    let line = last.lines().find(|l| l.starts_with('{')).unwrap_or_else(|| panic!("no STATE json in {last:?}"));
    serde_json::from_str(line).unwrap()
}

fn mcp_call(s: &Session, a: &str, tool: &str, args: Value) -> String {
    let prompt = format!("MCP_CALL:{tool}:{}", serde_json::to_string(&args).unwrap());
    s.json(&["send", "--wait", a, &prompt]);
    let hist = s.json(&["read", "--lines", "200", a])["text"].as_str().unwrap().to_owned();
    let idx = hist.rfind("MCP_RESULT:").or_else(|| hist.rfind("MCP_ERROR:")).unwrap_or(0);
    hist[idx..].to_owned()
}

#[test]
fn mcp_server_is_injected_and_drives_child_lifecycle_and_permissions() {
    let s = Session::new();
    let parent = run_fake(&s, &[]);
    let st = state_json(&s, &parent);
    let servers = st["mcp_servers"].as_array().unwrap();
    assert_eq!(servers.len(), 1, "expected herdr2 MCP server injected: {st}");
    assert_eq!(servers[0]["name"], "herdr2", "{st}");

    // 1. h2_run spawns a child attributed automatically to `parent`.
    let spec = fake_acp_spec();
    let run_out = mcp_call(
        &s,
        &parent,
        "h2_run",
        serde_json::json!({
            "agent": spec,
            "name": "kid",
            "prompt": "WRITE_TOOL",
            "bg": true
        }),
    );
    assert!(run_out.contains("MCP_RESULT:") && run_out.contains("kid"), "{run_out}");
    assert_eq!(get(&s, "kid")["parent"], parent.as_str());

    // Wait for kid to block on WRITE_TOOL, then verify h2_ls, h2_approve, h2_send, h2_wait, h2_read!
    wait_until("kid blocked on WRITE_TOOL", || get(&s, "kid")["status"]["state"] == "blocked");

    let ls_out = mcp_call(&s, &parent, "h2_ls", serde_json::json!({}));
    assert!(ls_out.contains("MCP_RESULT:") && ls_out.contains("kid"), "{ls_out}");

    let app_out = mcp_call(&s, &parent, "h2_approve", serde_json::json!({"agent": "kid"}));
    assert!(app_out.contains("MCP_RESULT:"), "{app_out}");

    let wait1 = mcp_call(&s, &parent, "h2_wait", serde_json::json!({"agent": "kid", "timeout": 10}));
    assert!(wait1.contains("MCP_RESULT:"), "{wait1}");

    let send_out =
        mcp_call(&s, &parent, "h2_send", serde_json::json!({"agent": "kid", "text": "hello-via-mcp", "wait": true}));
    assert!(send_out.contains("MCP_RESULT:"), "{send_out}");

    let read_out = mcp_call(&s, &parent, "h2_read", serde_json::json!({"agent": "kid", "lines": 100}));
    assert!(
        read_out.contains("MCP_RESULT:")
            && read_out.contains("write_tool=selected:allow-once")
            && read_out.contains("echo: hello-via-mcp"),
        "{read_out}"
    );

    // Also test h2_deny on a blocked EXEC_TOOL child.
    let deny_child = run_fake(&s, &["--parent", &parent, "--name", "kid2", "--prompt", "EXEC_TOOL"]);
    wait_until("kid2 blocked", || get(&s, &deny_child)["status"]["state"] == "blocked");
    let deny_out = mcp_call(&s, &parent, "h2_deny", serde_json::json!({"agent": "kid2"}));
    assert!(deny_out.contains("MCP_RESULT:"), "{deny_out}");
    wait_until("kid2 finished exec denial", || {
        let g = get(&s, &deny_child);
        g["turn"].as_u64().is_some_and(|t| t >= 2) && g["status"]["state"] == "idle"
    });
    assert!(text(&s, &deny_child).contains("exec_tool=selected:reject-once"));
}

#[test]
fn no_mcp_flag_disables_mcp_server_injection() {
    let s = Session::new();
    let a = run_fake(&s, &["--no-mcp"]);
    let st = state_json(&s, &a);
    assert_eq!(st["mcp_servers"].as_array().unwrap().len(), 0, "--no-mcp must omit mcpServers: {st}");
}
