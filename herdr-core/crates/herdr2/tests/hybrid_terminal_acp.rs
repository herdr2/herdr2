mod common;

use common::*;
use serde_json::{Value, json};
use std::process::Stdio;

fn h2_from_terminal(s: &Session, term_id: &str, args: &[&str]) -> Value {
    let mut cmd = s.cmd();
    cmd.args(["--json"])
        .args(args)
        .env("H2_TERMINAL_ID", term_id)
        .env("H2_TERMINAL_SOCKET", s.socket())
        .stdin(Stdio::null());
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "h2 {args:?} from {term_id} failed: {}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("bad json ({e}): {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn workspace_terminal_dispatches_headless_acp_subagent_and_cleans_up_on_kill() {
    let s = Session::new();
    s.json(&["server", "start"]);
    let mut c = s.connect();

    // 1. Open a real interactive PTY workspace terminal (`t1`).
    let term = c.call(1, "terminal.run", json!({"argv": ["sh"], "name": "workspace-shell"}));
    assert!(term.get("error").is_none(), "terminal.run failed: {term}");
    let t_addr = term["result"]["address"].as_str().unwrap().to_owned();
    assert_eq!(t_addr, "t1", "{term}");
    assert_eq!(term["result"]["state"], "live", "{term}");

    let tlist = c.call(2, "terminal.list", json!({}))["result"]["terminals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["address"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(tlist, vec!["t1"]);

    // 2. Dispatch a subordinate ACP agent (`a1`) from inside `t1` (`H2_TERMINAL_ID=t1`).
    let spec = fake_acp_spec();
    let run = h2_from_terminal(
        &s,
        "t1",
        &[
            "run",
            "--bg",
            "--name",
            "sub-worker",
            "--agent",
            &spec,
            "--prompt",
            "ENV:H2_TERMINAL_ID\nENV:TERM\nENV:H2_AGENT_ID",
        ],
    );
    let a_addr = run["address"].as_str().unwrap().to_owned();
    assert_eq!(a_addr, "a1", "{run}");
    assert_eq!(run["parent"], "t1", "{run}");

    // `terminal.list` stays strictly PTY-only; `a1` opens NO terminal pane.
    let tlist_after = c.call(3, "terminal.list", json!({}))["result"]["terminals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["address"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(tlist_after, vec!["t1"], "ACP sub-agent must never appear in terminal.list");

    let alist = c.call(4, "agent.list", json!({}))["result"]["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["address"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(alist, vec!["a1"]);

    let ls = s.json(&["ls"]);
    assert_eq!(ls["agents"].as_array().unwrap().len(), 1, "{ls}");
    assert_eq!(ls["terminals"].as_array().unwrap().len(), 1, "{ls}");

    // 3. `h2 wait --children all` called from `t1` waits on child `a1`.
    let waited = h2_from_terminal(&s, "t1", &["wait", "--children", "all"]);
    assert_eq!(waited["ended"], true, "{waited}");
    assert_eq!(waited["reason"], "children", "{waited}");
    let read_a = s.json(&["read", "a1"])["text"].as_str().unwrap().to_owned();
    assert!(read_a.contains("ENV:H2_TERMINAL_ID=<unset>"), "{read_a}");
    assert!(read_a.contains("ENV:TERM=<unset>"), "{read_a}");
    assert!(read_a.contains("ENV:H2_AGENT_ID=a1"), "{read_a}");

    // ACP agents must never inject terminal input or automatic notes into PTY `t1`.
    let blocked_agent_send =
        c.call(5, "agent.send", json!({"agent": "t1", "caller": "a1", "text": "echo SHOULD_NOT_TYPE"}));
    assert_eq!(blocked_agent_send["error"]["data"]["kind"], "invalid_params", "{blocked_agent_send}");
    let blocked_term_send =
        c.call(6, "terminal.send", json!({"terminal": "t1", "caller": "a1", "text": "echo SHOULD_NOT_TYPE"}));
    assert_eq!(blocked_term_send["error"]["data"]["kind"], "invalid_params", "{blocked_term_send}");
    let screen_t1 = s.json(&["read", "t1"])["text"].as_str().unwrap().to_owned();
    assert!(!screen_t1.contains("<h2"), "terminal must not receive <h2> notes: {screen_t1}");
    assert!(!screen_t1.contains("SHOULD_NOT_TYPE"), "ACP input leaked to PTY: {screen_t1}");

    // 4. `h2 send t1` + `h2 read t1` types into and reads the live PTY workspace screen.
    s.json(&["send", "t1", "echo HYBRID_OK_77"]);
    wait_until("PTY shell echoes HYBRID_OK_77", || {
        s.json(&["read", "t1"])["text"].as_str().is_some_and(|t| t.contains("HYBRID_OK_77"))
    });

    // 5. Killing workspace `t1` automatically terminates child `a1` (`die_with_parent`).
    let killed = s.json(&["kill", "t1"]);
    assert_eq!(killed["state"], "finished", "{killed}");
    wait_until("child a1 stops when parent t1 is killed", || get(&s, "a1")["state"] == "finished");
}

#[test]
fn acp_subagent_stays_warm_for_reuse_and_closes_when_parent_is_trashed() {
    let s = Session::new();
    s.json(&["server", "start"]);
    let mut c = s.connect();

    // 1. Open workspace terminal `t1` and spawn child ACP agent `a1`.
    let term = c.call(1, "terminal.run", json!({"argv": ["sh"], "name": "parent-term"}));
    assert!(term.get("error").is_none(), "terminal.run failed: {term}");
    let spec = fake_acp_spec();
    let run = h2_from_terminal(
        &s,
        "t1",
        &["run", "--bg", "--name", "reusable-sub", "--agent", &spec, "--prompt", "first task"],
    );
    assert_eq!(run["address"], "a1", "{run}");
    assert_eq!(run["parent"], "t1", "{run}");

    // Wait for turn 1 to finish and verify `a1` stays live & warm for reuse.
    let w1 = h2_from_terminal(&s, "t1", &["wait", "a1"]);
    assert_eq!(w1["ended"], true, "{w1}");
    let after_t1 = get(&s, "a1");
    assert_eq!(after_t1["state"], "live", "child must remain live after first turn: {after_t1}");
    assert_eq!(after_t1["turn"], 1, "{after_t1}");

    // 2. Reuse the same warm child via `h2 send` for a follow-up task.
    h2_from_terminal(&s, "t1", &["send", "a1", "follow-up task"]);
    let w2 = h2_from_terminal(&s, "t1", &["wait", "a1"]);
    assert_eq!(w2["ended"], true, "{w2}");
    let after_t2 = get(&s, "a1");
    assert_eq!(after_t2["state"], "live", "child must remain live after follow-up turn: {after_t2}");
    assert_eq!(after_t2["turn"], 2, "{after_t2}");

    // 3. Moving parent terminal `t1` to recycle bin (`herdr.trashed`) automatically closes child `a1`.
    let trashed_t1 = c.call(2, "terminal.label", json!({"terminal": "t1", "labels": {"herdr.trashed": "123456"}}));
    assert!(trashed_t1.get("error").is_none(), "terminal.label failed: {trashed_t1}");
    wait_until("child a1 stops when parent terminal t1 is trashed", || get(&s, "a1")["state"] == "finished");

    // 4. Same lifecycle guarantee for ACP parent `a2` -> ACP child `a3`.
    let parent_acp = s.json(&["run", "--bg", "--name", "parent-acp", "--agent", &spec]);
    assert_eq!(parent_acp["address"], "a2", "{parent_acp}");
    let child_acp = s.json(&[
        "run",
        "--bg",
        "--name",
        "child-acp",
        "--parent",
        "a2",
        "--agent",
        &spec,
        "--prompt",
        "nested task",
    ]);
    assert_eq!(child_acp["address"], "a3", "{child_acp}");
    let _ = s.json(&["wait", "a3"]);
    assert_eq!(get(&s, "a3")["state"], "live");
    let trashed_a2 = c.call(3, "agent.label", json!({"agent": "a2", "labels": {"herdr.trashed": "123456"}}));
    assert!(trashed_a2.get("error").is_none(), "agent.label failed: {trashed_a2}");
    wait_until("child a3 stops when parent ACP a2 is trashed", || get(&s, "a3")["state"] == "finished");
}
