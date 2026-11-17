mod common;

use common::*;
use serde_json::Value;
use std::time::Duration;

fn state_json(s: &Session, a: &str) -> Value {
    s.json(&["send", "--wait", a, "STATE"]);
    let last = s.json(&["read", "--last", a])["text"].as_str().unwrap().to_owned();
    let line = last.lines().find(|l| l.starts_with('{')).unwrap_or_else(|| panic!("no STATE json in {last:?}"));
    serde_json::from_str(line).unwrap()
}

#[test]
fn kill_and_clean_exit_stop_a_kept_agent_without_restarting() {
    let s = Session::new();
    let a = run_fake(&s, &["--keep"]);
    assert_eq!(get(&s, &a)["keep"], true);
    let a_pid = get(&s, &a)["pid"].as_i64().unwrap();

    s.json(&["send", a.as_str(), "EXIT:0"]);
    wait_until("clean exit finished", || {
        let g = get(&s, &a);
        g["state"] == "finished" && g["exit_code"] == 0 && g["reason"] == "exited"
    });
    assert!(!pid_alive(a_pid));

    let b = run_fake(&s, &["--keep"]);
    let b_pid = get(&s, &b)["pid"].as_i64().unwrap();
    s.json(&["kill", &b]);
    wait_until("explicit kill finished", || get(&s, &b)["state"] == "finished");
    assert!(!pid_alive(b_pid));
}

#[test]
fn sigkill_of_kept_agent_restarts_and_resumes_via_session_load() {
    let s = Session::new();
    let a = run_fake(&s, &["--keep"]);
    let before = state_json(&s, &a);
    let orig_sid = before["session_id"].as_str().unwrap().to_owned();
    assert_eq!(before["loaded"], false);

    let old_pid = get(&s, &a)["pid"].as_i64().unwrap();
    unsafe { libc::kill(old_pid as i32, libc::SIGKILL) };

    wait_until("kept agent restarted", || {
        let g = get(&s, &a);
        g["state"] == "live" && g["pid"].as_i64().is_some_and(|p| p != old_pid)
    });

    let after = state_json(&s, &a);
    assert_eq!(after["session_id"], orig_sid, "acp session id must persist across session/load: {after}");
    assert_eq!(after["loaded"], true, "expected session/load after crash: {after}");
}

#[test]
fn core_restart_recovers_kept_agents_via_session_load_and_stops_unkept_ones() {
    let s = Session::new();
    let kept = run_fake(&s, &["--keep", "--name", "kept"]);
    let plain = run_fake(&s, &["--name", "plain"]);
    let before = state_json(&s, &kept);
    let sid = before["session_id"].as_str().unwrap().to_owned();
    let old_kept_pid = get(&s, &kept)["pid"].as_i64().unwrap();

    s.json(&["server", "restart"]);

    wait_until("kept agent recovered after server restart", || {
        let g = get(&s, &kept);
        g["state"] == "live" && g["pid"].as_i64().is_some_and(|p| p != old_kept_pid)
    });
    wait_until("unkept agent finished on restart", || get(&s, &plain)["state"] == "finished");

    let after = state_json(&s, &kept);
    assert_eq!(after["session_id"], sid);
    assert_eq!(after["loaded"], true);
}

#[test]
fn crashed_core_recovers_kept_agents_on_next_start() {
    let s = Session::new();
    let kept = run_fake(&s, &["--keep"]);
    let before = state_json(&s, &kept);
    let sid = before["session_id"].as_str().unwrap().to_owned();
    let old_pid = get(&s, &kept)["pid"].as_i64().unwrap();

    let status = s.json(&["server", "status"]);
    let core_pid = status["pid"].as_i64().unwrap();
    unsafe { libc::kill(core_pid as i32, libc::SIGKILL) };
    wait_until("old core gone", || !pid_alive(core_pid));

    // Next CLI command autostart recovers the kept agent from agents.json.
    wait_until("kept agent recovered after core SIGKILL", || {
        let g = get(&s, &kept);
        g["state"] == "live" && g["pid"].as_i64().is_some_and(|p| p != old_pid)
    });
    let after = state_json(&s, &kept);
    assert_eq!(after["session_id"], sid);
    assert_eq!(after["loaded"], true);
}

#[test]
fn load_failure_or_missing_load_cap_falls_back_to_fresh_session() {
    let s = Session::new();
    let reject = run(&s, &["--keep", "--agent", &format!("{} --reject-load", fake_acp_spec())]);
    let nocap = run(&s, &["--keep", "--agent", &format!("{} --no-load-cap", fake_acp_spec())]);

    let r0 = state_json(&s, &reject);
    let n0 = state_json(&s, &nocap);
    let r_pid = get(&s, &reject)["pid"].as_i64().unwrap();
    let n_pid = get(&s, &nocap)["pid"].as_i64().unwrap();

    s.json(&["server", "restart"]);

    wait_until("both fallback agents restarted", || {
        let gr = get(&s, &reject);
        let gn = get(&s, &nocap);
        gr["state"] == "live"
            && gr["pid"].as_i64().is_some_and(|p| p != r_pid)
            && gn["state"] == "live"
            && gn["pid"].as_i64().is_some_and(|p| p != n_pid)
    });

    let r1 = state_json(&s, &reject);
    let n1 = state_json(&s, &nocap);
    assert_eq!(r1["loaded"], false, "rejected load must fall back to session/new: {r1}");
    assert_ne!(r1["session_id"], r0["session_id"]);
    assert_eq!(n1["loaded"], false, "no-load-cap must fall back to session/new: {n1}");
    assert_ne!(n1["session_id"], n0["session_id"]);
}

#[test]
fn keep_toggle_and_missing_agent_command_work_cleanly() {
    let s = Session::new();
    let a = run_fake(&s, &[]);
    assert_eq!(get(&s, &a)["keep"], false);
    s.json(&["keep", &a]);
    assert_eq!(get(&s, &a)["keep"], true);

    s.json(&["keep", "--off", &a]);
    assert_eq!(get(&s, &a)["keep"], false);

    let pid = get(&s, &a)["pid"].as_i64().unwrap();
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    std::thread::sleep(Duration::from_millis(200));
    wait_until("agent with keep=off does not restart", || get(&s, &a)["state"] == "finished");

    let (_, err) = s.err(&["--json", "run", "--bg", "--agent", "definitely-missing-acp-binary-xyz"]);
    assert_eq!(err["error"]["kind"], "spawn_failed", "{err}");
}
