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
    assert!(a2_get["parent"].is_null(), "handoff should be a standalone top-level session, not a sub-agent: {a2_get}");

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

    // Prompt-less ACP side-chat / fork / handoff must immediately deliver the carried context on startup
    // (consistent with CLI mode) using a read-only bootstrap prompt, so the agent receives it right away.
    let sc = s.json(&["handoff", &a1, "--kind", "side-chat", "--wait"]);
    let a3 = sc["target"].as_str().unwrap().to_string();
    assert_eq!(sc["target_kind"], "agent", "{sc}");
    assert_eq!(sc["prompt_sent"], true, "{sc}");
    assert_eq!(sc["wait"]["turn"].as_u64(), Some(1), "{sc}");

    let a3_read = s.json(&["read", &a3]);
    let a3_entries = a3_read["entries"].as_array().unwrap();
    assert_eq!(a3_entries[0]["kind"], "handoff", "{a3_read}");
    assert_eq!(a3_entries[1]["kind"], "user", "{a3_read}");
    assert!(
        a3_entries[1]["text"].as_str().unwrap_or_default().contains("Do NOT make any modifications"),
        "expected read-only bootstrap prompt for prompt-less ACP side-chat: {a3_read}"
    );

    // Next user turn on a3 must NOT re-inject <herdr2-handoff> since turn 1 already delivered it.
    s.json(&["send", "--wait", &a3, "STATE"]);
    let a3_last: Value = {
        let last_txt = s.json(&["read", &a3, "--last"])["text"].as_str().unwrap().to_string();
        let line = last_txt.lines().find(|l| l.starts_with('{')).unwrap_or_else(|| panic!("bad STATE json: {last_txt:?}"));
        serde_json::from_str(line).unwrap()
    };
    assert!(
        !a3_last["raw"].as_str().unwrap_or_default().contains("<herdr2-handoff"),
        "Turn 2 on a3 should not re-inject handoff envelope: {a3_last}"
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
    assert!(t1_get["parent"].is_null(), "PTY handoff should be a standalone top-level session, not a sub-agent: {t1_get}");
}

#[test]
fn fork_worktree_creates_branch_notifies_parent_on_merge_and_cleans_up_on_rm() {
    let s = Session::new();
    let repo = s.root.join("repo-wt");
    init_git_repo(&repo);

    let spec = fake_acp_spec();
    let run1 = s.json(&[
        "run",
        "--agent",
        &spec,
        "--cwd",
        &repo.to_string_lossy(),
        "--prompt",
        "Work on calculator",
    ]);
    let a1 = run1["address"].as_str().unwrap().to_string();

    // 1. Fork with --worktree: creates a real local Git branch (not detached HEAD).
    let fk = s.json(&[
        "handoff",
        &a1,
        "--kind",
        "fork",
        "--worktree",
        "--prompt",
        "Try alternative implementation",
        "--wait",
    ]);
    let a2 = fk["target"].as_str().unwrap().to_string();
    let wt_path = fk["worktree_path"].as_str().expect("worktree_path must be set").to_string();
    let wt_branch = fk["worktree_branch"].as_str().expect("worktree_branch must be set").to_string();
    assert!(wt_branch.starts_with("herdr/fork-"), "unexpected branch: {wt_branch}");
    assert!(Path::new(&wt_path).is_dir(), "worktree dir must exist: {wt_path}");

    let a2_get = s.json(&["get", &a2]);
    assert_eq!(a2_get["lineage"]["worktree_branch"].as_str(), Some(wt_branch.as_str()), "{a2_get}");
    assert_ne!(a2_get["lineage"]["worktree_merged"].as_bool(), Some(true), "{a2_get}");

    // Verify fork worktree is on the real branch and supports commits.
    let wt_head = String::from_utf8_lossy(
        &Command::new("git")
            .args(["-C", &wt_path, "rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert_eq!(wt_head, wt_branch);
    std::fs::write(Path::new(&wt_path).join("feature.txt"), "from fork branch\n").unwrap();
    git(Path::new(&wt_path), &["add", "feature.txt"]);
    git(Path::new(&wt_path), &["commit", "-m", "add feature on fork"]);

    // 2. Merge back with --merge-worktree: notifies parent to merge WITHOUT deleting worktree or branch,
    // and marks `worktree_merged = true` on the fork record.
    let mb = s.json(&["handoff", &a2, "--into", &a1, "--merge-worktree", "--wait"]);
    assert_eq!(mb["target"], a1, "{mb}");
    assert_eq!(mb["worktree_merge_requested"], true, "{mb}");
    assert_eq!(mb["worktree_branch"].as_str(), Some(wt_branch.as_str()), "{mb}");
    assert!(Path::new(&wt_path).is_dir(), "merge back must NOT delete worktree dir");

    let a2_after_mb = s.json(&["get", &a2]);
    assert_eq!(a2_after_mb["lineage"]["worktree_merged"].as_bool(), Some(true), "{a2_after_mb}");
    assert_eq!(a2_after_mb["lineage"]["worktree_path"].as_str(), Some(wt_path.as_str()), "{a2_after_mb}");

    let a1_read = s.json(&["read", &a1]);
    let merge_user = a1_read["entries"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e["kind"] == "user")
        .expect("parent should receive merge instruction");
    let merge_text = merge_user["text"].as_str().unwrap_or_default();
    assert!(merge_text.contains(&wt_branch), "parent prompt must name fork branch: {merge_text}");
    assert!(merge_text.contains(&wt_path), "parent prompt must include worktree path: {merge_text}");
    assert!(merge_text.contains("Do NOT delete"), "parent prompt must forbid deleting worktree/branch: {merge_text}");

    // 3. Killing & removing the fork agent permanently deletes both the worktree directory and Git branch.
    s.json(&["kill", &a2]);
    s.json(&["rm", &a2]);
    assert!(!Path::new(&wt_path).exists(), "rm must delete worktree dir");
    let branch_exists = Command::new("git")
        .args(["-C", &repo.to_string_lossy(), "rev-parse", "--verify", &format!("refs/heads/{wt_branch}")])
        .output()
        .unwrap()
        .status
        .success();
    assert!(!branch_exists, "rm must delete fork git branch {wt_branch}");
}
