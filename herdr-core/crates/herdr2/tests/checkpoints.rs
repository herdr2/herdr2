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
    let sibling = s.json(&["run", "--agent", &spec, "--cwd", &repo.to_string_lossy()])["address"]
        .as_str()
        .unwrap()
        .to_string();
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
