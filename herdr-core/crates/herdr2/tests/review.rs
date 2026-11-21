#![cfg(unix)]

mod common;

use common::*;
use serde_json::json;

#[test]
fn agent_review_delivers_inline_to_acp_and_stages_oversized_payload() {
    let s = Session::new();
    let spec = fake_acp_spec();
    let run = s.json(&["run", "--agent", &spec, "--prompt", "Initial setup"]);
    let a1 = run["address"].as_str().unwrap().to_string();

    let mut conn = s.connect();
    let resp = conn.call(
        1,
        "agent.review",
        json!({
            "target": a1,
            "source": "changes",
            "repo_root": "/tmp/demo",
            "note": "Please keep API backwards-compatible",
            "language": "en",
            "comments": [
                {
                    "path": "src/api.rs",
                    "side": "new",
                    "start_line": 42,
                    "end_line": 48,
                    "hunk_header": "@@ -40,5 +40,9 @@",
                    "snippet": "+let val = parse(input)?;",
                    "body": "Handle empty string cleanly before parsing."
                },
                {
                    "path": "src/lib.rs",
                    "side": "old",
                    "start_line": 12,
                    "body": "Keep this re-export for downstream callers."
                }
            ]
        }),
    );
    let result = &resp["result"];
    assert_eq!(result["target"], a1, "{resp}");
    assert_eq!(result["target_kind"], "agent", "{resp}");
    assert_eq!(result["comment_count"], 2, "{resp}");
    assert_eq!(result["file_count"], 2, "{resp}");
    assert!(result["staged_path"].is_null(), "small ACP payload should deliver inline: {resp}");

    // Wait for turn to finish and verify transcript contains structured Markdown.
    let _ = s.json(&["wait", &a1]);
    let read = s.json(&["read", &a1]);
    let text = read["text"].as_str().unwrap();
    assert!(text.contains("File Line Notes (2 notes across 2 files)"), "{text}");
    assert!(text.contains("src/api.rs:L42-L48"), "{text}");
    assert!(text.contains("Handle empty string cleanly before parsing."), "{text}");
    assert!(text.contains("src/lib.rs:L12 (old/deleted side)"), "{text}");

    // Oversized ACP payload (> 24 KiB) should auto-stage line-notes.md and send a compact pointer.
    let huge_comments: Vec<_> = (0..30)
        .map(|i| {
            json!({
                "path": format!("src/mod_{i}.rs"),
                "side": "new",
                "start_line": i + 1,
                "snippet": format!("+{}", "x".repeat(900)),
                "body": format!("Note item {i}: {}", "detail ".repeat(40)),
            })
        })
        .collect();
    let big = conn.call(
        2,
        "agent.review",
        json!({
            "target": a1,
            "source": "changes",
            "language": "zh-CN",
            "comments": huge_comments,
        }),
    );
    let big_res = &big["result"];
    assert_eq!(big_res["comment_count"], 30, "{big}");
    let staged = big_res["staged_path"].as_str().expect("oversized ACP payload must stage file");
    let staged_body = std::fs::read_to_string(staged).expect("staged notes file readable");
    assert!(staged_body.contains("文件行级消息（30 条"), "{staged_body}");
}

#[test]
fn agent_review_stages_file_and_injects_pointer_for_pty_terminal() {
    let s = Session::new();
    s.json(&["server", "start"]);
    let mut conn = s.connect();
    let started = conn.call(
        1,
        "terminal.run",
        json!({
            "argv": ["/bin/sh", "-c", "stty -echo; cat"],
        }),
    );
    let t1 = started["result"]["address"].as_str().expect("terminal address").to_string();

    let resp = conn.call(
        2,
        "agent.review",
        json!({
            "target": t1,
            "source": "checkpoints",
            "checkpoint_turn": 2,
            "language": "zh-CN",
            "comments": [
                {
                    "path": "src/main.rs",
                    "side": "new",
                    "start_line": 15,
                    "end_line": 19,
                    "snippet": "+println!(\"debug\");",
                    "body": "移除调试打印语句"
                }
            ]
        }),
    );
    let result = &resp["result"];
    assert_eq!(result["target"], t1, "{resp}");
    assert_eq!(result["target_kind"], "terminal", "{resp}");
    assert_eq!(result["comment_count"], 1, "{resp}");
    assert_eq!(result["file_count"], 1, "{resp}");
    let staged = result["staged_path"].as_str().expect("PTY review must stage diff-review.md");
    let content = std::fs::read_to_string(staged).expect("staged file exists");
    assert!(content.contains("来源：快照 T2"), "{content}");
    assert!(content.contains("src/main.rs:L15-L19"), "{content}");
    assert!(content.contains("移除调试打印语句"), "{content}");

    // Verify the terminal received the concise pointer containing the staged path.
    wait_until("pty receives staged review pointer", || {
        let out = conn.call(3, "terminal.read", json!({ "terminal": t1 }));
        out["result"]["text"].as_str().is_some_and(|txt| txt.contains(staged))
    });
}

#[test]
fn agent_review_rejects_invalid_and_finished_targets() {
    let s = Session::new();
    let spec = fake_acp_spec();
    let run = s.json(&["run", "--agent", &spec, "--prompt", "hi"]);
    let a1 = run["address"].as_str().unwrap().to_string();

    let mut conn = s.connect();

    // Empty comments rejected.
    let empty = conn.call(1, "agent.review", json!({ "target": a1, "comments": [] }));
    assert_eq!(empty["error"]["data"]["kind"], "invalid_params", "{empty}");

    // Empty body rejected.
    let blank = conn.call(
        2,
        "agent.review",
        json!({
            "target": a1,
            "comments": [{ "path": "a.rs", "start_line": 1, "body": "   " }]
        }),
    );
    assert_eq!(blank["error"]["data"]["kind"], "invalid_params", "{blank}");

    // Finished target rejected.
    let _ = s.json(&["kill", &a1]);
    let dead = conn.call(
        3,
        "agent.review",
        json!({
            "target": a1,
            "comments": [{ "path": "a.rs", "start_line": 1, "body": "Fix this" }]
        }),
    );
    assert_eq!(dead["error"]["data"]["kind"], "finished", "{dead}");
}
