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
    assert!(
        r1["system_prompt"].as_str().is_some_and(|sp| sp.contains("STATE")),
        "fallback session/new should inject portable handoff context from prior transcript: {r1}"
    );
    assert_eq!(n1["loaded"], false, "no-load-cap must fall back to session/new: {n1}");
    assert_ne!(n1["session_id"], n0["session_id"]);
    assert!(
        n1["system_prompt"].as_str().is_some_and(|sp| sp.contains("STATE")),
        "no-load-cap fallback should inject portable handoff context from prior transcript: {n1}"
    );
}

#[test]
fn kept_acp_agent_preserves_paused_inbox_across_restart_and_resumes_on_command() {
    let s = Session::new();
    let a = run_fake(&s, &["--keep"]);
    let orig_pid = get(&s, &a)["pid"].as_i64().unwrap();

    // Start a long turn and enqueue a follow-up prompt into the serial inbox.
    s.json(&["send", a.as_str(), "SLEEP:5000"]);
    wait_until("agent working", || get(&s, &a)["status"]["state"] == "working");
    s.json(&["send", a.as_str(), "follow-up-after-reboot"]);
    wait_until("prompt queued in inbox", || get(&s, &a)["inbox_len"] == 1);

    // Restart the core (simulating reboot/restart): queued prompt must move into paused_inbox,
    // NOT be dropped and NOT auto-execute before user confirmation.
    s.json(&["server", "restart"]);
    wait_until("kept agent recovered with paused_inbox", || {
        let g = get(&s, &a);
        g["state"] == "live"
            && g["pid"].as_i64().is_some_and(|p| p != orig_pid)
            && g["paused_inbox"].as_array().is_some_and(|arr| arr.len() == 1)
    });
    let after_restart = get(&s, &a);
    assert_eq!(after_restart["inbox_len"].as_u64().unwrap_or(0), 0);
    assert_eq!(after_restart["paused_inbox"][0]["text"], "follow-up-after-reboot");

    // Explicitly resume paused_inbox via CLI -> prompt is delivered and answered.
    let resumed = s.json(&["inbox", "resume", &a]);
    assert_eq!(resumed["paused_inbox"].as_array().map_or(0, Vec::len), 0);
    wait_until("resumed prompt answered", || {
        let read = s.json(&["read", "--last", &a]);
        read["text"].as_str().is_some_and(|t| t.contains("follow-up-after-reboot"))
    });
}

#[cfg(unix)]
#[test]
fn kept_pty_cli_agent_resumes_session_after_simulated_reboot_and_plain_terminal_does_not_restore_screen() {
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let work = s.root.join("proj");
    let fake_bin = s.root.join("fake-bin");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();
    let log_file = s.root.join("claude-invocations.log");
    let claude_projects = s.root.join(".claude").join("projects").join("test-proj");
    std::fs::create_dir_all(&claude_projects).unwrap();

    // Fake `claude` executable: records its argv, creates a non-empty Claude session JSONL when
    // launched with `--session-id <sid>`, and stays alive waiting for EOF/signals.
    let fake_claude = fake_bin.join("claude");
    let script = format!(
        "#!/bin/sh\n\
printf '%s\\n' \"$*\" >> '{}'\n\
prev=''\n\
for arg in \"$@\"; do\n\
  if [ \"$prev\" = '--session-id' ]; then\n\
    printf '{{\"type\":\"user\",\"message\":{{\"content\":\"hello\"}}}}\\n' > '{}/'$arg'.jsonl'\n\
  fi\n\
  prev=\"$arg\"\n\
done\n\
while :; do sleep 1; done\n",
        log_file.display(),
        claude_projects.display(),
    );
    std::fs::write(&fake_claude, script).unwrap();
    std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Start core with HOME pointing to s.root and fake_bin on PATH so both `claude` and
    // `find_claude_session_file` resolve cleanly inside this isolated test sandbox.
    let mut start_cmd = s.cmd();
    start_cmd
        .args(["--json", "server", "start"])
        .env("HOME", &s.root)
        .env("PATH", format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap_or_default()));
    let out = start_cmd.output().unwrap();
    assert!(out.status.success(), "server start failed: {}", String::from_utf8_lossy(&out.stderr));

    let mut c = s.connect();
    let claude_res = c.call(
        1,
        "terminal.run",
        json!({
            "argv": ["/bin/sh", "-lic", "\"$@\"; exec \"$0\" -li", "/bin/sh", fake_claude.to_string_lossy(), "--dangerously-skip-permissions"],
            "cwd": work.to_string_lossy(),
            "keep": true
        }),
    );
    assert!(claude_res.get("error").is_none(), "claude terminal.run failed: {claude_res}");
    let t_claude = claude_res["result"]["address"].as_str().unwrap().to_owned();
    let orig_claude_pid = claude_res["result"]["pid"].as_i64().unwrap();

    let plain_res = c.call(
        2,
        "terminal.run",
        json!({
            "argv": ["/bin/sh", "-c", "printf 'PLAIN_BEFORE_REBOOT\\n'; while :; do sleep 1; done"],
            "cwd": work.to_string_lossy(),
            "keep": true
        }),
    );
    assert!(plain_res.get("error").is_none(), "plain terminal.run failed: {plain_res}");
    let t_plain = plain_res["result"]["address"].as_str().unwrap().to_owned();
    let orig_plain_pid = plain_res["result"]["pid"].as_i64().unwrap();

    // Wait for initial fake `claude` launch to record its injected `--session-id` and create the JSONL file.
    wait_until("initial claude session recorded", || {
        std::fs::read_to_string(&log_file)
            .ok()
            .is_some_and(|txt| txt.contains("--session-id"))
    });
    let first_line = std::fs::read_to_string(&log_file).unwrap().lines().next().unwrap().to_owned();
    let tokens: Vec<&str> = first_line.split_whitespace().collect();
    let sid_pos = tokens.iter().position(|t| *t == "--session-id").expect("injected --session-id");
    let orig_sid = tokens[sid_pos + 1].to_owned();
    assert!(claude_projects.join(format!("{orig_sid}.jsonl")).is_file());

    // Wait for plain terminal output to reach the core screen.
    wait_until("plain terminal printed output", || {
        let r = c.call(3, "terminal.read", json!({"terminal": &t_plain, "lines": 20}));
        r["result"]["text"].as_str().is_some_and(|txt| txt.contains("PLAIN_BEFORE_REBOOT"))
    });
    drop(c);

    // Simulate a reboot: SIGKILL both the core and the holder (and children).
    let core_pid = s.json(&["server", "status"])["pid"].as_i64().unwrap();
    unsafe {
        libc::kill(core_pid as i32, libc::SIGKILL);
        libc::kill(orig_claude_pid as i32, libc::SIGKILL);
        libc::kill(orig_plain_pid as i32, libc::SIGKILL);
    }
    // Also SIGKILL any holder belonging to this isolated session so attach_all sees no live holder.
    for entry in std::fs::read_dir(&s.root).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.contains(".holder.") && name.ends_with(".sock") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    wait_until("old core gone", || !pid_alive(core_pid));

    // Start core again (as after reboot) with the same HOME and PATH.
    let mut restart_cmd = s.cmd();
    restart_cmd
        .args(["--json", "server", "start"])
        .env("HOME", &s.root)
        .env("PATH", format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap_or_default()));
    let out = restart_cmd.output().unwrap();
    assert!(out.status.success(), "post-reboot server start failed: {}", String::from_utf8_lossy(&out.stderr));

    // Wait until fake `claude` is automatically re-invoked with `--resume <orig_sid>`!
    wait_until("claude terminal resumed with --resume", || {
        std::fs::read_to_string(&log_file)
            .ok()
            .is_some_and(|txt| txt.lines().skip(1).any(|l| l.contains(&format!("--resume {orig_sid}"))))
    });
    let log_all = std::fs::read_to_string(&log_file).unwrap();
    let resumed_line = log_all.lines().skip(1).find(|l| l.contains("--resume")).unwrap();
    assert!(
        resumed_line.contains("--dangerously-skip-permissions"),
        "resumed claude must preserve launch flags: {resumed_line}"
    );

    // Verify plain terminal recovered as a clean shell WITHOUT restoring the old screen.
    let mut c2 = s.connect();
    wait_until("plain terminal restarted with new pid", || {
        let g = c2.call(10, "terminal.get", json!({"terminal": &t_plain}));
        g["result"]["state"] == "live" && g["result"]["pid"].as_i64().is_some_and(|p| p != orig_plain_pid)
    });
    let plain_after = c2.call(11, "terminal.read", json!({"terminal": &t_plain, "lines": 50}));
    let plain_text = plain_after["result"]["text"].as_str().unwrap_or_default();
    assert!(
        !plain_text.contains("PLAIN_BEFORE_REBOOT") && !plain_text.contains("the program above ended"),
        "plain terminal screen must NOT be restored: {plain_text:?}"
    );
    let claude_after = c2.call(12, "terminal.get", json!({"terminal": &t_claude}));
    assert_eq!(claude_after["result"]["state"], "live");
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
