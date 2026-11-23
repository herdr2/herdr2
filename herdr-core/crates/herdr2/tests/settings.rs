mod common;

use common::*;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;

#[test]
fn long_session_names_fail_with_a_clear_message_not_truncation() {
    let mut s = Session::new();
    let long = "x".repeat(200);
    s.env.push(("H2_SESSION".into(), long));
    let out = s.h2(&["server", "start"]);
    assert!(!out.status.success());
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    let msg = err["error"]["message"].as_str().unwrap();
    assert!(msg.contains("too long") || msg.contains("socket"), "{err}");
}

#[test]
fn the_socket_can_live_elsewhere_while_state_stays_under_xdg_state() {
    let s = Session::new();
    std::fs::create_dir_all(s.root.join("config/herdr2")).unwrap();
    let custom = s.root.join("custom.sock");
    std::fs::write(s.root.join("config/herdr2/config.toml"), format!("socket = {:?}\n", custom)).unwrap();

    let status = s.json(&["server", "status"]);
    assert!(status["pid"].is_i64(), "{status}");
    assert!(custom.exists(), "custom socket should exist at {}", custom.display());
    let _ = run_fake(&s, &[]);
    wait_until("agents.json under XDG_STATE_HOME", || s.root.join("state/herdr2/test/agents.json").exists());

    let mut s2 = Session::new();
    let custom2 = s2.root.join("env.sock");
    s2.env.push(("H2_SOCKET".into(), custom2.display().to_string()));
    let status2 = s2.json(&["server", "status"]);
    assert!(status2["pid"].is_i64(), "{status2}");
    assert!(custom2.exists(), "H2_SOCKET should exist at {}", custom2.display());
}

#[test]
fn a_client_looking_at_the_wrong_socket_sees_the_session_name_and_hint() {
    let s = Session::new();
    s.json(&["server", "status"]);
    let dir = s.root.join("wrong-run");
    std::fs::create_dir_all(&dir).unwrap();
    // Non-autostarting commands against wrong XDG_RUNTIME_DIR still point to the running core's socket.
    let out = s.cmd().env("XDG_RUNTIME_DIR", &dir).args(["--json", "kill", "a1"]).output().unwrap();
    assert!(!out.status.success());
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["error"]["kind"], "unavailable", "{err}");
    let hint = err["error"]["hint"].as_str().unwrap();
    assert!(hint.contains("H2_SOCKET"), "{err}");
    assert!(err["error"]["message"].as_str().unwrap().contains(&s.socket().display().to_string()), "{err}");
}

#[test]
fn a_core_with_other_state_leaves_a_running_core_alone() {
    let mut s1 = Session::new();
    s1.env.push(("H2_LAUNCHD_DIRECT".into(), "1".into()));
    let status1 = s1.json(&["server", "status"]);
    let pid1 = status1["pid"].as_i64().unwrap();
    let ino1 = std::fs::metadata(s1.socket()).unwrap().ino();

    // A second `herdr2 serve` with other state pointed at s1's live socket must leave s1 alone.
    let other_state = s1.root.join("other-state");
    std::fs::create_dir_all(&other_state).unwrap();
    let mut cmd = s1.cmd();
    cmd.env("XDG_STATE_HOME", &other_state)
        .env("H2_SOCKET", s1.socket())
        .args(["serve"])
        .stdin(std::process::Stdio::null());
    let out = cmd.output().unwrap();
    assert!(!out.status.success() || s1.json(&["server", "status"])["pid"] == pid1);
    assert_eq!(s1.json(&["server", "status"])["pid"], pid1);
    assert_eq!(std::fs::metadata(s1.socket()).unwrap().ino(), ino1);
}

#[test]
fn a_label_filtered_subscriber_hears_only_matching_agents() {
    let s = Session::new();
    s.json(&["server", "status"]);
    let mut conn = s.connect();
    let sub = conn.call(1, "events.subscribe", json!({"labels": {"team": "alpha"}}));
    assert!(sub.get("result").is_some(), "{sub}");

    let a_beta = run_fake(&s, &["--label", "team=beta"]);
    let a_alpha = run_fake(&s, &["--label", "team=alpha"]);

    let ev = next_event(&mut conn, |m| m["method"] == "event" && m["params"]["type"] == "created");
    assert_eq!(ev["params"]["agent"], a_alpha, "filtered stream should skip {a_beta}: {ev}");
    assert_eq!(ev["params"]["data"]["labels"]["team"], "alpha");
}

#[test]
fn events_carry_every_changing_fact_and_filter_parent_children() {
    let s = Session::new();
    let parent = run_fake(&s, &[]);
    let mut conn = s.connect();
    let sub = conn.call(1, "events.subscribe", json!({"children_of": parent}));
    assert!(sub.get("result").is_some(), "{sub}");

    let other = run_fake(&s, &[]);
    let child = run_fake(&s, &["--parent", &parent, "--prompt", "SLEEP:1500"]);

    let created = next_event(&mut conn, |m| m["method"] == "event" && m["params"]["type"] == "created");
    assert_eq!(created["params"]["agent"], child, "children_of filter must exclude {other}: {created}");
    assert_eq!(created["params"]["data"]["parent"], parent);

    s.json(&["kill", &child]);
    let exited = next_event(&mut conn, |m| {
        m["method"] == "event" && m["params"]["agent"] == child && m["params"]["type"] == "finished"
    });
    assert!(exited["params"]["data"]["exit_code"].is_null() || exited["params"]["data"]["signal"].is_number());

    // Check raw framing too: a disconnected client doesn't break the server.
    drop(conn);
    let _ = UnixStream::connect(s.socket()).and_then(|mut st| {
        st.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"hello\",\"params\":{}}\n")?;
        let mut buf = [0u8; 64];
        let _ = st.read(&mut buf);
        Ok(())
    });
    assert!(s.json(&["server", "status"])["pid"].is_i64());
}

#[test]
fn config_sets_default_permissions_inject_mcp_and_agent_presets() {
    let s = Session::new();
    std::fs::create_dir_all(s.root.join("config/herdr2")).unwrap();
    let parts: Vec<String> = fake_acp_spec().split_whitespace().map(|p| format!("{p:?}")).collect();
    std::fs::write(
        s.root.join("config/herdr2/config.toml"),
        format!(
            "default_permissions = \"approve-all\"\ninject_mcp = false\n[agents]\ncustom = [{}]\n",
            parts.join(", ")
        ),
    )
    .unwrap();

    let a =
        s.json(&["run", "--bg", "--agent", "custom", "--prompt", "WRITE_TOOL"])["address"].as_str().unwrap().to_owned();
    wait_until("approve-all completed WRITE_TOOL", || {
        let g = get(&s, &a);
        g["turn"] == 1 && g["status"]["state"] == "idle"
    });
    let out = text(&s, &a);
    assert!(out.contains("write_tool=selected:"), "{out}");

    s.json(&["send", "--wait", &a, "STATE"]);
    let last = s.json(&["read", "--last", &a])["text"].as_str().unwrap().to_owned();
    let state: Value = serde_json::from_str(last.lines().find(|l| l.starts_with('{')).unwrap()).unwrap();
    assert_eq!(state["mcp_servers"].as_array().unwrap().len(), 0, "inject_mcp=false should omit mcpServers: {state}");
}
