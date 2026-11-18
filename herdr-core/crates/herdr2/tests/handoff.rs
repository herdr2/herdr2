mod common;

use common::*;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
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
}

fn init_git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-b", "main"]);
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "init"]);
}

#[test]
fn acp_handoff_lazy_injection_turn_slice_and_merge_back() {
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
        "Design auth token validator",
    ]);
    let a1 = run1["address"].as_str().unwrap().to_string();
    assert_eq!(run1["wait"]["turn"].as_u64(), Some(1), "{run1}");

    let send2 = s.json(&["send", "--wait", &a1, "Add refresh rotation"]);
    assert_eq!(send2["wait"]["turn"].as_u64(), Some(2), "{send2}");

    // Verify self-serve --from-turn/--to-turn slicing excludes turn 1.
    let slice = s.json(&["read", &a1, "--from-turn", "2", "--to-turn", "2"]);
    let slice_entries = slice["entries"].as_array().unwrap();
    assert!(!slice_entries.is_empty(), "{slice}");
    assert!(slice_entries.iter().all(|e| e["turn"].as_u64() == Some(2)), "{slice}");
    let slice_text = slice["text"].as_str().unwrap();
    assert!(slice_text.contains("Add refresh rotation"), "{slice_text}");
    assert!(!slice_text.contains("Design auth token validator"), "{slice_text}");

    // Hand off from a1 to a2 (auto-reusing a1's raw fake_acp_spec binary when preset is omitted).
    let ho = s.json(&[
        "handoff",
        &a1,
        "--kind",
        "handoff",
        "--note",
        "Switch to verifier",
        "--prompt",
        "STATE",
        "--wait",
    ]);
    let a2 = ho["target"].as_str().unwrap().to_string();
    assert_eq!(ho["target_kind"], "agent", "{ho}");
    assert_eq!(ho["source"], a1, "{ho}");
    assert_eq!(ho["kind"], "handoff", "{ho}");
    assert_eq!(ho["prompt_sent"], true, "{ho}");
    assert_eq!(ho["wait"]["turn"].as_u64(), Some(1), "{ho}");

    let a2_get = s.json(&["get", &a2]);
    assert_eq!(a2_get["lineage"]["kind"], "handoff", "{a2_get}");
    assert_eq!(a2_get["lineage"]["parent"], a1, "{a2_get}");

    // Read a2: Entry::Handoff at turn 0, clean Entry::User ("STATE"), while raw wire prompt held <herdr2-handoff>.
    let a2_read = s.json(&["read", &a2]);
    let entries = a2_read["entries"].as_array().unwrap();
    assert_eq!(entries[0]["kind"], "handoff", "{a2_read}");
    assert_eq!(entries[0]["turn"], 0, "{a2_read}");
    assert_eq!(entries[0]["from_address"], a1, "{a2_read}");
    assert_eq!(entries[1]["kind"], "user", "{a2_read}");
    assert_eq!(entries[1]["text"], "STATE", "User transcript entry must stay clean: {a2_read}");

    let last1: Value = {
        let last_txt = s.json(&["read", &a2, "--last"])["text"].as_str().unwrap().to_string();
        let line = last_txt.lines().find(|l| l.starts_with('{')).unwrap_or_else(|| panic!("bad STATE json: {last_txt:?}"));
        serde_json::from_str(line).unwrap()
    };
    let wire1 = last1["raw"].as_str().unwrap();
    assert!(wire1.contains("<herdr2-handoff"), "{wire1}");
    assert!(wire1.contains("Design auth token validator"), "{wire1}");
    assert!(wire1.contains("Switch to verifier"), "{wire1}");

    // Turn 2 on a2 must NOT re-inject pending_handoff.
    s.json(&["send", "--wait", &a2, "STATE"]);
    let last2: Value = {
        let last_txt = s.json(&["read", &a2, "--last"])["text"].as_str().unwrap().to_string();
        let line = last_txt.lines().find(|l| l.starts_with('{')).unwrap_or_else(|| panic!("bad STATE json: {last_txt:?}"));
        serde_json::from_str(line).unwrap()
    };
    let wire2 = last2["raw"].as_str().unwrap();
    assert!(!wire2.contains("<herdr2-handoff"), "Turn 2 should not contain handoff envelope: {wire2}");

    // Merge back from a2 into a1 via --into.
    let mb = s.json(&["handoff", &a2, "--into", &a1, "--prompt", "STATE", "--wait"]);
    assert_eq!(mb["target"], a1, "{mb}");
    assert_eq!(mb["prompt_sent"], true, "{mb}");

    let a1_read = s.json(&["read", &a1]);
    let a1_entries = a1_read["entries"].as_array().unwrap();
    assert!(
        a1_entries.iter().any(|e| e["kind"] == "handoff" && e["from_address"] == a2),
        "a1 should record incoming merge-back Entry::Handoff: {a1_read}"
    );
    let a1_last: Value = {
        let last_txt = s.json(&["read", &a1, "--last"])["text"].as_str().unwrap().to_string();
        let line = last_txt.lines().find(|l| l.starts_with('{')).unwrap_or_else(|| panic!("bad STATE json: {last_txt:?}"));
        serde_json::from_str(line).unwrap()
    };
    assert!(a1_last["raw"].as_str().unwrap().contains("<herdr2-handoff"), "{a1_last}");

    // Prompt-less merge back into an ACP parent should still trigger a follow-up turn.
    let mb_default = s.json(&["handoff", &a2, "--into", &a1, "--wait"]);
    assert_eq!(mb_default["target"], a1, "{mb_default}");
    assert_eq!(mb_default["kind"], "handoff", "{mb_default}");
    assert_eq!(mb_default["prompt_sent"], true, "{mb_default}");
    assert_eq!(mb_default["wait"]["turn"].as_u64(), Some(4), "{mb_default}");

    let a1_after_default = s.json(&["read", &a1]);
    let turn4_user = a1_after_default["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "user" && e["turn"].as_u64() == Some(4))
        .expect("turn 4 user entry must exist after prompt-less merge back");
    assert!(
        turn4_user["text"].as_str().unwrap_or_default().contains("Handoff from"),
        "{turn4_user}"
    );
}

#[test]
fn pty_handoff_stages_markdown_and_records_lineage() {
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
        "Design auth token validator",
    ]);
    let a1 = run1["address"].as_str().unwrap().to_string();
    assert_eq!(run1["wait"]["turn"].as_u64(), Some(1), "{run1}");

    let ho = s.json(&["handoff", &a1, "--pty", "--", "sh", "-c", "printf 'ready\\n'; sleep 3"]);
    assert_eq!(ho["target_kind"], "terminal", "{ho}");
    assert_eq!(ho["source"], a1, "{ho}");
    assert_eq!(ho["native_fork"], false, "{ho}");

    let t1 = ho["target"].as_str().unwrap();
    let artifact = ho["artifact_path"].as_str().expect("artifact_path must be set");
    let contents = std::fs::read_to_string(artifact).expect("staged handoff.md must exist on disk");
    assert!(contents.contains("Original Objective (Turn 1)"), "{contents}");
    assert!(contents.contains("Design auth token validator"), "{contents}");

    let t1_get = s.json(&["get", t1]);
    assert_eq!(t1_get["lineage"]["parent"], a1, "{t1_get}");
    assert_eq!(t1_get["lineage"]["kind"], "handoff", "{t1_get}");
}
