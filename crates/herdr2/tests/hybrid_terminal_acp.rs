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
