mod common;

use common::*;
use serde_json::Value;
use std::thread;

#[test]
fn serial_inbox_queues_prompts_in_fifo_order_and_send_wait_tracks_its_own_turn() {
    let s = Session::new();
    let a = run_fake(&s, &["--prompt", "SLEEP:2000"]);
    wait_until("first turn working", || get(&s, &a)["status"]["state"] == "working");

    // While turn 1 is sleeping, queue two more prompts: both stay in the inbox until turn 1 finishes.
    let s2 = s.json(&["send", &a, "second"]);
    assert_eq!(s2["started"], true, "{s2}");
    assert_eq!(s2["inbox_len"], 1, "{s2}");

    // `send --wait` for a third prompt must wait for ITS OWN turn ("third"), not the active turn.
    let waiter = {
        let mut cmd = s.cmd();
        cmd.args(["--json", "send", "--wait", &a, "third"]);
        thread::spawn(move || cmd.output().unwrap())
    };
    wait_until("inbox reached len 2", || get(&s, &a)["inbox_len"] == 2);
    assert!(!waiter.is_finished(), "send --wait returned before queued turn ran");

    let out = waiter.join().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let waited: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(waited["wait"]["turn"], 3, "{waited}");

    let transcript = s.json(&["read", "--lines", "100", &a])["text"].as_str().unwrap().to_owned();
    let pos1 = transcript.find("slept:2000").unwrap_or_else(|| panic!("{transcript}"));
    let pos2 = transcript.find("echo: second").unwrap_or_else(|| panic!("{transcript}"));
    let pos3 = transcript.find("echo: third").unwrap_or_else(|| panic!("{transcript}"));
    assert!(pos1 < pos2 && pos2 < pos3, "turns executed out of FIFO order:\n{transcript}");
}

#[test]
fn steer_cancels_active_turn_and_delivers_ahead_of_queued_prompts() {
    let s = Session::new();
    let a = run_fake(&s, &["--prompt", "HOLD"]);
    wait_until("HOLD working", || get(&s, &a)["status"]["state"] == "working");

    let q = s.json(&["send", &a, "queued-later"]);
    assert_eq!(q["inbox_len"], 1, "{q}");

    let steer = s.json(&["send", "--steer", "--wait", &a, "urgent-steer"]);
    assert!(steer["wait"]["turn"].as_u64().is_some_and(|t| t >= 2), "{steer}");

    wait_until("all queued turns idle", || {
        let g = get(&s, &a);
        g["status"]["state"] == "idle"
            && g["inbox_len"].as_u64().unwrap_or(0) == 0
            && g["turn"].as_u64().is_some_and(|t| t >= 3)
    });

    let transcript = s.json(&["read", "--lines", "100", &a])["text"].as_str().unwrap().to_owned();
    let p_cancel = transcript.find("cancelled").unwrap_or_else(|| panic!("{transcript}"));
    let p_steer = transcript.find("echo: urgent-steer").unwrap_or_else(|| panic!("{transcript}"));
    let p_queued = transcript.find("echo: queued-later").unwrap_or_else(|| panic!("{transcript}"));
    assert!(
        p_cancel < p_steer && p_steer < p_queued,
        "steer must run after cancel and ahead of queued:\n{transcript}"
    );
}

#[test]
fn wait_children_any_and_all_and_sender_attribution_tags() {
    let s = Session::new();
    let parent = run_fake(&s, &[]);
    let c1 = run_fake(&s, &["--parent", &parent, "--name", "fast", "--prompt", "SLEEP:200"]);
    let c2 = run_fake(&s, &["--parent", &parent, "--name", "slow", "--prompt", "SLEEP:1000"]);

    let any = s
        .cmd()
        .env("H2_AGENT_ID", &parent)
        .args(["--json", "wait", "--children", "any", "--timeout", "10"])
        .output()
        .unwrap();
    assert!(any.status.success(), "{}", String::from_utf8_lossy(&any.stderr));
    let any_v: Value = serde_json::from_slice(&any.stdout).unwrap();
    assert_eq!(any_v["ended"], true, "{any_v}");

    let all = s
        .cmd()
        .env("H2_AGENT_ID", &parent)
        .args(["--json", "wait", "--children", "all", "--timeout", "10"])
        .output()
        .unwrap();
    assert!(all.status.success(), "{}", String::from_utf8_lossy(&all.stderr));
    let all_v: Value = serde_json::from_slice(&all.stdout).unwrap();
    assert_eq!(all_v["ended"], true, "{all_v}");
    assert_eq!(all_v["children"].as_array().unwrap().len(), 2, "{all_v}");

    // Send from child c1 back to parent checks <h2 from="c1 fast">...</h2> wrapping in raw prompt & transcript entry.
    let send = s
        .cmd()
        .env("H2_AGENT_ID", &c1)
        .args(["--json", "send", "--wait", &parent, "report-ready"])
        .output()
        .unwrap();
    assert!(send.status.success(), "{}", String::from_utf8_lossy(&send.stderr));
    let p_hist = s.json(&["read", "--lines", "50", &parent])["text"].as_str().unwrap().to_owned();
    assert!(p_hist.contains(&format!("[user from {c1}] <h2 from=\"{c1} fast\">report-ready</h2>")), "{p_hist}");

    s.json(&["send", "--wait", &parent, "STATE"]);
    let last = s.json(&["read", "--last", &parent])["text"].as_str().unwrap().to_owned();
    let st: Value = serde_json::from_str(last.lines().find(|l| l.starts_with('{')).unwrap()).unwrap();
    assert_eq!(st["raw"], "STATE");
    let _ = c2;
}
