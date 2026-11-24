mod common;

use common::*;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn init_git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-b", "main"]);
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "init"]);
}

#[test]
fn checkpoint_hook_without_terminal_env_exits_fast_without_starting_daemon() {
    let root = test_tmp().join(format!("h2-hook-noop-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let sock = root.join("never-created.sock");
    let out = Command::new(env!("CARGO_BIN_EXE_herdr2"))
        .args(["checkpoint", "hook", "--source", "agy", "--event", "SessionStart"])
        .env("H2_SOCKET", &sock)
        .env_remove("H2_TERMINAL_ID")
        .env_remove("H2_TERMINAL_SOCKET")
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "{}");
    assert!(!sock.exists(), "hook outside herdr2 must never start a daemon");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn acp_auto_checkpoints_safety_gate_and_dual_rollback_end_to_end() {
    let s = Session::new();
    let repo = s.root.join("repo");
    init_git_repo(&repo);

    let spec = fake_acp_spec();
    let run1 = s.json(&[
        "run",
        "--agent",
        &spec,
        "--cwd",
        &repo.to_string_lossy(),
        "--prompt",
        "SH:printf 'turn1\\n' > a.txt",
    ]);
    let addr = run1["address"].as_str().unwrap().to_string();

    let run2 = s.json(&["send", "--wait", &addr, "SH:printf 'turn2\\n' > b.txt && printf 'mutated\\n' > base.txt"]);
    assert_eq!(run2["wait"]["turn"].as_u64(), Some(2), "{run2}");

    // List checkpoints: turn 0, turn 1, turn 2 must exist and isolation must warn main checkout.
    let listed = s.json(&["checkpoint", "ls", &addr]);
    let ckpts = listed["checkpoints"].as_array().unwrap();
    let turns: Vec<u64> = ckpts.iter().filter_map(|c| c["turn"].as_u64()).collect();
    assert_eq!(turns, vec![0, 1, 2], "{listed}");
    assert_eq!(listed["isolation"]["level"], "exclusive_confirm", "{listed}");
    assert_eq!(listed["isolation"]["is_main_checkout"], true, "{listed}");

    // Diff between turn 1 and turn 2 shows only b.txt + base.txt (not a.txt from turn 1).
    let diff = s.json(&["checkpoint", "diff", &addr, "--from", "1", "--to", "2"]);
    let patch = diff["patch"].as_str().unwrap();
    assert!(patch.contains("b.txt"), "{diff}");
    assert!(patch.contains("base.txt"), "{diff}");
    assert!(!patch.contains("a.txt"), "{diff}");

    // Idle sibling ACP agent requires `--confirm` (`confirm_required`), whereas an actively working
    // sibling in the same workspace blocks file rollback (`shared_workspace`).
    let sibling =
        s.json(&["run", "--agent", &spec, "--cwd", &repo.to_string_lossy()])["address"].as_str().unwrap().to_string();
    let (_, sib_confirm_err) = s.err(&["--json", "checkpoint", "rollback", &addr, "--to", "1"]);
    assert_eq!(sib_confirm_err["error"]["kind"], "confirm_required", "{sib_confirm_err}");

    s.json(&["send", &sibling, "SH:sleep 30"]);
    for _ in 0..80 {
        if get(&s, &sibling)["status"]["state"] == "working" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    let (_, blocked_err) = s.err(&["--json", "checkpoint", "rollback", &addr, "--to", "1", "--confirm"]);
    assert_eq!(blocked_err["error"]["kind"], "shared_workspace", "{blocked_err}");

    s.json(&["kill", &sibling]);

    // Main checkout without --confirm fails with `confirm_required`.
    let (_, confirm_err) = s.err(&["--json", "checkpoint", "rollback", &addr, "--to", "1"]);
    assert_eq!(confirm_err["error"]["kind"], "confirm_required", "{confirm_err}");

    let pid_before = get(&s, &addr)["pid"].as_u64().unwrap();

    // Dual rollback with --confirm restores files, rebuilds clean session, rewinds transcript, and prepares handoff.
    let rb = s.json(&["checkpoint", "rollback", &addr, "--to", "1", "--confirm"]);
    assert_eq!(rb["turn"], 1);
    assert_eq!(rb["restored_files"], true);
    assert_eq!(rb["rewound_session"], true);
    assert_eq!(rb["restored_prompt"].as_str(), Some("SH:printf 'turn2\\n' > b.txt && printf 'mutated\\n' > base.txt"));

    assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "turn1\n");
    assert!(!repo.join("b.txt").exists(), "turn 2 untracked file must be cleaned");
    assert_eq!(std::fs::read_to_string(repo.join("base.txt")).unwrap(), "base\n");

    let pid_after = get(&s, &addr)["pid"].as_u64().unwrap();
    assert_ne!(pid_after, pid_before, "rollback must rebuild a clean ACP child session");

    let rewound_read = s.json(&["read", &addr]);
    let entries = rewound_read["entries"].as_array().unwrap();
    assert!(entries.iter().any(|e| e["kind"] == "rewind" && e["to_turn"] == 1), "{rewound_read}");
    assert!(!entries.iter().any(|e| e["turn"] == 2), "turn 2 entries must be trimmed: {rewound_read}");

    // Next turn receives injected portable handoff mentioning turn 1, while transcript keeps clean user entry.
    let next = s.json(&["send", "--wait", &addr, "follow-up check"]);
    assert_eq!(next["wait"]["turn"].as_u64(), Some(2), "{next}");
    let read_last = s.json(&["read", "--last", &addr]);
    let output = read_last["text"].as_str().unwrap();
    assert!(output.contains("<herdr2-context-handoff"), "{output}");
    assert!(output.contains("SH:printf 'turn1\\n' > a.txt"), "{output}");

    // Manual snapshot bookmark works via target or CLI flag.
    let snap = s.json(&["checkpoint", "snap", &addr, "--label", "manual bookmark"]);
    assert_eq!(snap["checkpoint"]["label"], "manual bookmark");

    // Rolling back to turn 0 keeps the initial T0 baseline and resets workspace to initial state,
    // and a subsequent new turn adds T1 alongside the preserved T0.
    let rb0 = s.json(&["checkpoint", "rollback", &addr, "--to", "0", "--confirm"]);
    assert_eq!(rb0["turn"], 0);
    assert_eq!(rb0["restored_files"], true);
    assert!(!repo.join("a.txt").exists(), "rolling back to turn 0 must remove turn 1 file");
    let after_zero = s.json(&["checkpoint", "ls", &addr]);
    let zero_turns: Vec<u64> =
        after_zero["checkpoints"].as_array().unwrap().iter().filter_map(|c| c["turn"].as_u64()).collect();
    assert_eq!(zero_turns, vec![0], "{after_zero}");

    let reborn = s.json(&["send", "--wait", &addr, "SH:printf 'reborn\\n' > c.txt"]);
    assert_eq!(reborn["wait"]["turn"].as_u64(), Some(1), "{reborn}");
    let after_reborn = s.json(&["checkpoint", "ls", &addr]);
    let reborn_turns: Vec<u64> =
        after_reborn["checkpoints"].as_array().unwrap().iter().filter_map(|c| c["turn"].as_u64()).collect();
    assert_eq!(reborn_turns, vec![0, 1], "{after_reborn}");
}

#[cfg(unix)]
#[test]
fn pty_claude_cli_rollback_reenters_claude_even_after_holder_drains() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let repo = s.root.join("repo-pty");
    init_git_repo(&repo);

    // Create a fake `claude` executable in a local bin dir that logs its argv and stays open.
    let bin_dir = s.root.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let log_file = s.root.join("claude-launches.log");
    let fake_claude = bin_dir.join("claude");
    std::fs::write(
        &fake_claude,
        format!(
            "#!/bin/sh\necho \"LAUNCH:$0 $*\" >> '{}'\ntrap 'exit 0' TERM INT\nwhile true; do sleep 1; done\n",
            log_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Launch PTY terminal running fake claude (with --keep, matching interactive workspace panes).
    let _ = s.json(&["server", "status"]);
    let mut c = s.connect();
    let created_resp = c.call(
        1,
        "terminal.run",
        serde_json::json!({
            "argv": [fake_claude.to_string_lossy(), "--dangerously-skip-permissions"],
            "cwd": repo.to_string_lossy(),
            "keep": true,
        }),
    );
    assert!(created_resp.get("error").is_none(), "terminal.run failed: {created_resp}");
    let created = &created_resp["result"];
    let addr = created["address"].as_str().unwrap().to_string();
    let pid_before = created["pid"].as_u64().unwrap();

    // Wait for fake claude to launch and foreground tracking to observe it before capturing checkpoints.
    wait_until("initial claude foreground observed", || {
        std::fs::read_to_string(&log_file).is_ok_and(|c| !c.is_empty()) && get(&s, &addr)["foreground"].is_object()
    });

    // Capture T0 baseline, mutate workspace, capture T1, then send a prompt (populating optimistic
    // `pty_prompts` just like `h2 send` or `agent.handoff --into`).
    s.json(&["checkpoint", "snap", &addr, "--label", "turn0"]);
    std::fs::write(repo.join("mutated.txt"), "after\n").unwrap();
    s.json(&["checkpoint", "snap", &addr, "--label", "turn1"]);
    let stale_handoff = "[Handoff from t30] Context handoff saved at: /tmp/handoff-from-t30.md";
    s.json(&["send", &addr, stale_handoff]);
    let before_rb_read = s.json(&["read", &addr]);
    assert!(
        before_rb_read["entries"]
            .as_array()
            .is_some_and(|arr| arr.iter().any(|e| e["text"].as_str().is_some_and(|t| t.contains("Handoff from t30")))),
        "sent prompt should appear in optimistic transcript before rollback: {before_rb_read}"
    );

    // Roll back to turn 0: must respawn fake claude cleanly in the same terminal pane (not drop to shell)
    // and clear `pty_prompts` so rolled-back prompts never resurface in the conversation view.
    let rb = s.json(&["checkpoint", "rollback", &addr, "--to", "0", "--confirm"]);
    assert_eq!(rb["turn"], 0, "{rb}");
    assert_eq!(rb["restored_files"], true, "{rb}");
    assert!(!repo.join("mutated.txt").exists(), "workspace file must be restored");

    let after_rb_read = s.json(&["read", &addr]);
    assert!(
        !after_rb_read["entries"]
            .as_array()
            .is_some_and(|arr| arr.iter().any(|e| e["text"].as_str().is_some_and(|t| t.contains("Handoff from t30")))),
        "rolled-back prompt must not linger in conversation view after rollback: {after_rb_read}"
    );

    wait_until("respawned claude launches", || {
        std::fs::read_to_string(&log_file).map(|c| c.lines().count() >= 2).unwrap_or(false)
    });
    let after = get(&s, &addr);
    assert_eq!(after["state"], "live", "terminal must remain live after rollback: {after}");
    assert_ne!(after["pid"].as_u64(), Some(pid_before), "rollback must spawn a fresh claude process: {after}");
    let argv = after["argv"].as_array().unwrap();
    assert!(
        argv.iter().any(|a| a.as_str().is_some_and(|v| v.contains("claude"))),
        "respawned argv must re-enter claude, not fall back to bare shell: {after}"
    );
    let launches = std::fs::read_to_string(&log_file).unwrap();
    assert!(
        launches.lines().last().is_some_and(|l| l.contains("--session-id")),
        "expected fresh --session-id on turn-0 rollback: {launches}"
    );
}

/// Runs one of ZCode's hooks the way the CLI does: stdin JSON in Claude Code's shape, with
/// `transcript_path` a one-message file in a folder removed once the hook has answered.
fn zcode_hook(s: &Session, addr: &str, phase: &str, sid: &str, prompt: Option<&str>) -> String {
    use std::io::Write as _;
    use std::process::Stdio;

    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let event = if phase == "pre" { "UserPromptSubmit" } else { "Stop" };
    let mut payload = serde_json::json!({
        "hookEventName": event,
        "hook_event_name": event,
        "sessionId": sid,
        "session_id": sid,
        "mode": "yolo",
        "permission_mode": "yolo",
    });
    let folder = s.root.join(format!(
        "zcode-hook-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&folder).unwrap();
    let transcript = folder.join("transcript.jsonl");
    let line = serde_json::json!({
        "message": { "content": [{ "text": prompt.unwrap_or("done"), "type": "text" }], "role": if prompt.is_some() { "user" } else { "assistant" } }
    });
    std::fs::write(&transcript, format!("{line}\n")).unwrap();
    if let Some(p) = prompt {
        payload["prompt"] = serde_json::json!(p);
    }
    payload["transcript_path"] = serde_json::json!(transcript.to_string_lossy());

    let mut cmd = s.program(&s.bin);
    cmd.args(["checkpoint", "hook", "--source", "zcode", "--phase", phase])
        .env("H2_TERMINAL_ID", addr)
        .env("H2_TERMINAL_SOCKET", s.socket())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let body = format!("{payload}\n").into_bytes();
    let feed = std::thread::spawn(move || drop(stdin.write_all(&body)));
    let out = child.wait_with_output().unwrap();
    feed.join().unwrap();
    std::fs::remove_dir_all(&folder).unwrap();
    assert!(out.status.success(), "zcode {phase} hook failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[cfg(unix)]
#[test]
fn pty_zcode_hooks_capture_turns_and_rollback_resumes_the_session_with_a_note() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let repo = s.root.join("repo-zcode");
    init_git_repo(&repo);

    // A fake `zcode`: logs how it was started and stays open, as the TUI does.
    let bin_dir = s.root.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let log_file = s.root.join("zcode-launches.log");
    let fake_zcode = bin_dir.join("zcode");
    std::fs::write(
        &fake_zcode,
        format!(
            "#!/bin/sh\necho \"LAUNCH:$*\" >> '{}'\ntrap 'exit 0' TERM INT\nwhile true; do sleep 1; done\n",
            log_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_zcode, std::fs::Permissions::from_mode(0o755)).unwrap();

    let _ = s.json(&["server", "status"]);
    let mut c = s.connect();
    let created_resp = c.call(
        1,
        "terminal.run",
        serde_json::json!({
            "argv": [fake_zcode.to_string_lossy(), "--mode", "yolo"],
            "cwd": repo.to_string_lossy(),
            "keep": true,
        }),
    );
    assert!(created_resp.get("error").is_none(), "terminal.run failed: {created_resp}");
    let addr = created_resp["result"]["address"].as_str().unwrap().to_string();
    wait_until("zcode foreground observed", || {
        std::fs::read_to_string(&log_file).is_ok_and(|c| !c.is_empty()) && get(&s, &addr)["foreground"].is_object()
    });

    // Starting `zcode` from Herdr2 registers the hooks in ZCode's runtime config
    // (here the session's own HOME).
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(s.root.join(".zcode/cli/config.json")).unwrap()).unwrap();
    assert_eq!(config["hooks"]["enabled"], true, "{config}");
    for (event, phase) in [("PreToolUse", "guard"), ("UserPromptSubmit", "pre"), ("Stop", "post")] {
        let ours = config["hooks"]["events"][event]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
            .any(|h| h["args"][3] == "zcode" && h["args"][5] == phase);
        assert!(ours, "no Herdr2 {phase} hook under {event}: {config}");
    }

    // Two turns, each as ZCode reports it.
    let sid = "sess_0f1e2d3c-aaaa-4bbb-8ccc-000000000001";
    assert_eq!(zcode_hook(&s, &addr, "pre", sid, Some("add a.txt")), "{}");
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    assert_eq!(zcode_hook(&s, &addr, "post", sid, None), "{}");
    zcode_hook(&s, &addr, "pre", sid, Some("add b.txt"));
    std::fs::write(repo.join("b.txt"), "b\n").unwrap();
    zcode_hook(&s, &addr, "post", sid, None);

    let listed = s.json(&["checkpoint", "ls", &addr]);
    let turns: Vec<u64> = listed["checkpoints"].as_array().unwrap().iter().filter_map(|c| c["turn"].as_u64()).collect();
    assert_eq!(turns, vec![0, 1, 2], "{listed}");
    assert_eq!(listed["cli_kind"], "zcode", "{listed}");
    assert_eq!(listed["checkpoints"][1]["prompt"], "add a.txt", "{listed}");
    assert_eq!(listed["checkpoints"][2]["prompt"], "add b.txt", "{listed}");

    // Back to turn 1: b.txt goes, and the session is resumed as it was.
    let rb = s.json(&["checkpoint", "rollback", &addr, "--to", "1", "--confirm"]);
    assert_eq!(rb["rewound_session"], true, "{rb}");
    assert_eq!(rb["restored_files"], true, "{rb}");
    assert!(!repo.join("b.txt").exists(), "turn 2's file must be gone");
    assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "a\n");
    wait_until("respawned zcode launches", || std::fs::read_to_string(&log_file).is_ok_and(|c| c.lines().count() >= 2));
    let launches = std::fs::read_to_string(&log_file).unwrap();
    assert_eq!(
        launches.lines().last(),
        Some(format!("LAUNCH:--mode yolo --resume {sid}").as_str()),
        "rollback must resume the same ZCode session: {launches}"
    );

    // ... and told what happened with its next prompt. Once.
    let next = zcode_hook(&s, &addr, "pre", sid, Some("continue"));
    let out: serde_json::Value = serde_json::from_str(&next).unwrap();
    assert_eq!(out["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit", "{next}");
    let context = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default();
    assert!(context.contains("rolled this workspace back to the end of turn 1"), "{next}");
    assert_eq!(zcode_hook(&s, &addr, "post", sid, None), "{}");
    assert_eq!(zcode_hook(&s, &addr, "pre", sid, Some("and again")), "{}");

    // A note left for one session is not another's to hear: when a different ZCode session
    // speaks first, it is dropped, not saved for later.
    zcode_hook(&s, &addr, "post", sid, None);
    s.json(&["checkpoint", "rollback", &addr, "--to", "1", "--confirm"]);
    let other = "sess_0f1e2d3c-aaaa-4bbb-8ccc-000000000002";
    assert_eq!(zcode_hook(&s, &addr, "pre", other, Some("another session")), "{}");
    assert_eq!(zcode_hook(&s, &addr, "pre", sid, Some("back in the first")), "{}");
}

#[test]
fn zcode_hook_outside_herdr2_answers_empty_and_starts_nothing() {
    // Herdr2's hooks are in ZCode's one runtime config, so they run for every ZCode session:
    // away from Herdr2 they must cost nothing.
    let root = test_tmp().join(format!("h2-hook-zcode-noop-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let sock = root.join("never-created.sock");
    let out = Command::new(env!("CARGO_BIN_EXE_herdr2"))
        .args(["checkpoint", "hook", "--source", "zcode", "--phase", "pre"])
        .env("H2_SOCKET", &sock)
        .env_remove("H2_TERMINAL_ID")
        .env_remove("H2_TERMINAL_SOCKET")
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "{}");
    assert!(!sock.exists(), "hook outside herdr2 must never start a daemon");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn acp_readonly_turn_ignores_concurrent_external_edits_and_omits_synthetic_ppid_from() {
    let s = Session::new();
    let repo = s.root.join("repo-readonly");
    init_git_repo(&repo);

    // Start an ACP agent with no initial prompt (triggers background turn 0 capture).
    let spec = fake_acp_spec();
    let run = s.json(&["run", "--agent", &spec, "--cwd", &repo.to_string_lossy()]);
    let addr = run["address"].as_str().unwrap().to_string();

    // Wait for background turn 0 baseline to exist.
    wait_until("turn 0 checkpoint created", || {
        let ls = s.json(&["checkpoint", "ls", &addr]);
        ls["checkpoints"].as_array().is_some_and(|a| !a.is_empty())
    });

    // Simulate another agent/editor mutating the shared repo BEFORE turn 1 starts.
    std::fs::write(repo.join("external_before.txt"), "edited by sibling before turn 1\n").unwrap();

    // Turn 1: pure Q&A turn (zero tool calls) — must NOT attribute external_before.txt,
    // and Entry::User must NOT record a synthetic `ppid:*` `from` field.
    let qa = s.json(&["send", "--wait", &addr, "当前是什么大模型？"]);
    assert_eq!(qa["wait"]["turn"].as_u64(), Some(1), "{qa}");

    let read_out = s.json(&["read", &addr]);
    let entries = read_out["entries"].as_array().unwrap();
    let user_entry = entries.iter().find(|e| e["kind"] == "user" && e["turn"] == 1).unwrap();
    assert!(user_entry.get("from").is_none() || user_entry["from"].is_null(), "synthetic ppid caller leaked into user entry: {user_entry}");
    let ckpt_entry = entries.iter().find(|e| e["kind"] == "checkpoint" && e["turn"] == 1).unwrap();
    assert!(
        ckpt_entry.get("files").and_then(|v| v.as_array()).is_none_or(Vec::is_empty),
        "pure Q&A turn must not claim changed files: {ckpt_entry}"
    );

    let diff1 = s.json(&["checkpoint", "diff", &addr, "--to", "1"]);
    assert!(diff1["files"].as_array().unwrap().is_empty(), "turn 1 diff must be empty: {diff1}");
    assert!(diff1["patch"].as_str().unwrap().is_empty(), "turn 1 patch must be empty: {diff1}");

    // Simulate another external edit during the idle gap between Turn 1 and Turn 2.
    std::fs::write(repo.join("external_idle.txt"), "edited by user between turns\n").unwrap();

    // Turn 2: real mutating tool call (`SH:...`) writes only `owned.txt`.
    // Turn 2's checkpoint & single-turn diff must ONLY include `owned.txt`, NOT `external_idle.txt`.
    let edit = s.json(&["send", "--wait", &addr, "SH:printf 'agent edit\\n' > owned.txt"]);
    assert_eq!(edit["wait"]["turn"].as_u64(), Some(2), "{edit}");

    let ls = s.json(&["checkpoint", "ls", &addr]);
    let t2 = ls["checkpoints"].as_array().unwrap().iter().find(|c| c["turn"] == 2).unwrap();
    let t2_paths: Vec<&str> =
        t2["files"].as_array().unwrap().iter().filter_map(|f| f["path"].as_str()).collect();
    assert_eq!(t2_paths, vec!["owned.txt"], "turn 2 must exclude idle-gap external edits: {t2}");

    let diff2 = s.json(&["checkpoint", "diff", &addr, "--to", "2"]);
    let diff2_paths: Vec<&str> =
        diff2["files"].as_array().unwrap().iter().filter_map(|f| f["path"].as_str()).collect();
    assert_eq!(diff2_paths, vec!["owned.txt"], "single-turn diff must exclude idle-gap external edits: {diff2}");
}
