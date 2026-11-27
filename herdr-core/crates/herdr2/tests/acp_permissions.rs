mod common;

use common::*;

#[test]
fn approve_reads_auto_approves_reads_and_blocks_mutations_with_parent_note() {
    let s = Session::new();
    let parent = run_fake(&s, &["--name", "lead"]);

    let r = run_fake(&s, &["--parent", &parent, "--prompt", "READ_TOOL"]);
    wait_until("READ_TOOL auto-approved", || {
        let g = get(&s, &r);
        g["turn"].as_u64().is_some_and(|t| t >= 1) && g["status"]["state"] == "idle"
    });
    let r_out = text(&s, &r);
    assert!(r_out.contains("read_tool=selected:allow-once"), "{r_out}");

    let w = run_fake(&s, &["--parent", &parent, "--name", "writer", "--prompt", "WRITE_TOOL"]);
    wait_until("WRITE_TOOL blocked", || get(&s, &w)["status"]["state"] == "blocked");
    let info = get(&s, &w);
    assert_eq!(info["pending_permission"]["title"], "Edit output.txt", "{info}");
    assert_eq!(info["pending_permission"]["kind"], "edit", "{info}");

    wait_until("parent notified about blocked child", || {
        let pt = s.json(&["read", "--lines", "50", &parent])["text"].as_str().unwrap().to_owned();
        pt.contains(&w) && pt.contains("blocked")
    });

    // Normal send is refused while waiting on a permission decision.
    let (_, blocked_send) = s.err(&["--json", "send", &w, "hello"]);
    assert_eq!(blocked_send["error"]["kind"], "blocked", "{blocked_send}");

    s.json(&["approve", &w]);
    wait_until("WRITE_TOOL resumed and completed", || {
        let g = get(&s, &w);
        g["turn"].as_u64().is_some_and(|t| t >= 2) && g["status"]["state"] == "idle"
    });
    let w_out = text(&s, &w);
    assert!(w_out.contains("write_tool=selected:allow-once"), "{w_out}");
    assert!(get(&s, &w)["pending_permission"].is_null());
}

#[test]
fn deny_rejects_pending_permission_and_permission_policies_behave_distinctly() {
    let s = Session::new();

    // `approve-reads` + `deny` on EXEC_TOOL.
    let a = run_fake(&s, &["--prompt", "EXEC_TOOL"]);
    wait_until("EXEC_TOOL blocked", || get(&s, &a)["status"]["state"] == "blocked");
    s.json(&["deny", &a]);
    wait_until("EXEC_TOOL denied and completed", || {
        let g = get(&s, &a);
        g["turn"].as_u64().is_some_and(|t| t >= 2) && g["status"]["state"] == "idle"
    });
    let a_out = text(&s, &a);
    assert!(a_out.contains("exec_tool=selected:reject-once"), "{a_out}");

    // `approve-all` auto-approves WRITE_TOOL and EXEC_TOOL without blocking.
    let all = run_fake(&s, &["--permissions", "approve-all", "--prompt", "WRITE_TOOL\nEXEC_TOOL"]);
    wait_until("approve-all completed", || {
        let g = get(&s, &all);
        g["turn"].as_u64().is_some_and(|t| t >= 1) && g["status"]["state"] == "idle"
    });
    let all_out = text(&s, &all);
    assert!(
        all_out.contains("write_tool=selected:allow-once") && all_out.contains("exec_tool=selected:allow-once"),
        "{all_out}"
    );

    // `deny-all` rejects even READ_TOOL automatically.
    let denied = run_fake(&s, &["--permissions", "deny-all", "--prompt", "READ_TOOL"]);
    wait_until("deny-all completed", || {
        let g = get(&s, &denied);
        g["turn"].as_u64().is_some_and(|t| t >= 1) && g["status"]["state"] == "idle"
    });
    let denied_out = text(&s, &denied);
    assert!(denied_out.contains("read_tool=selected:reject-once"), "{denied_out}");

    // `ask` blocks even on READ_TOOL until explicitly approved.
    let ask = run_fake(&s, &["--permissions", "ask", "--prompt", "READ_TOOL"]);
    wait_until("ask blocks READ_TOOL", || get(&s, &ask)["status"]["state"] == "blocked");
    s.json(&["approve", &ask, "--option", "allow-always"]);
    wait_until("ask resumed", || {
        let g = get(&s, &ask);
        g["turn"].as_u64().is_some_and(|t| t >= 2) && g["status"]["state"] == "idle"
    });
    assert!(text(&s, &ask).contains("read_tool=selected:allow-always"));
}

#[test]
fn question_blocked_acp_session_accepts_reply_and_transitions_to_working() {
    let s = Session::new();

    // Turn 1 ends by asking the user a question -> blocked (kind=question, no pending_permission).
    let a = run_fake(&s, &["--prompt", "请问应该修改全局配置还是仅修改当前项目？"]);
    wait_until("question turn marked blocked", || {
        let g = get(&s, &a);
        g["turn"].as_u64().is_some_and(|t| t >= 1) && g["status"]["state"] == "blocked"
    });
    let info = get(&s, &a);
    assert_eq!(info["status"]["kind"], "question", "{info}");
    assert!(info["pending_permission"].is_null(), "{info}");

    // Sending a normal follow-up prompt (without --steer) must succeed and immediately switch to working.
    let sent = s.json(&["send", &a, "SLEEP:600\n仅修改当前项目"]);
    assert_eq!(sent["started"], true, "{sent}");
    assert_eq!(get(&s, &a)["status"]["state"], "working");

    wait_until("follow-up turn completed", || {
        let g = get(&s, &a);
        g["turn"].as_u64().is_some_and(|t| t >= 2) && g["status"]["state"] == "idle"
    });
    let out = text(&s, &a);
    assert!(out.contains("slept:600") && out.contains("仅修改当前项目"), "{out}");
}

#[test]
fn completed_turn_without_question_is_done_not_blocked_and_ai_arbitrates_questions() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let s = Session::new();

    // 1. Completed task with numbered summary + "确认" + optional courtesy offer -> NOT blocked;
    //    when unread (`herdr.seen=0`), `ai.briefing` surfaces it as `done` (`readyToReview`).
    let done_agent = run_fake(
        &s,
        &[
            "--prompt",
            "按照上述方案已完成重构：\n1. 优化了状态判断逻辑\n2. 补充单测并确认全部通过\n需要我顺便提交一下 git commit 吗？",
        ],
    );
    wait_until("completed turn finishes in idle without false blocked", || {
        let g = get(&s, &done_agent);
        g["turn"].as_u64().is_some_and(|t| t >= 1) && g["status"]["state"] == "idle"
    });
    s.json(&["label", &done_agent, "herdr.seen=0"]);
    let mut conn = s.connect();
    let briefing = conn.call(1, "ai.briefing", serde_json::json!({ "language": "zh-CN" }));
    assert!(
        briefing["result"]["readyToReview"]
            .as_array()
            .is_some_and(|arr| arr.iter().any(|it| it["address"] == done_agent && it["status"] == "done")),
        "expected unread completed agent in readyToReview as done: {briefing}"
    );

    // 2. Configure local mock AI server:
    //    - Answers `GET /v1/models` during `ai.settings.set`
    //    - Chat call 1 returns `done` (prevents false `blocked` on ambiguous "?" text)
    //    - Chat call 2 returns `blocked` (`kind = question`) for a subtle non-"?" question
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let mut chat_replies = std::collections::VecDeque::from([
            r#"{"choices":[{"message":{"content":"{\"state\":\"done\",\"kind\":null,\"confidence\":0.96,\"summary\":null}"}}]}"#,
            r#"{"choices":[{"message":{"content":"{\"state\":\"blocked\",\"kind\":\"question\",\"confidence\":0.94,\"summary\":\"询问部署目标环境\"}"}}]}"#,
        ]);
        while !chat_replies.is_empty() {
            let Ok((mut stream, _)) = listener.accept() else { break };
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let mut header_end = None;
            let mut content_len = 0usize;
            while let Ok(n) = stream.read(&mut tmp) {
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if header_end.is_none()
                    && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                {
                    header_end = Some(pos + 4);
                    let head = String::from_utf8_lossy(&buf[..pos]);
                    for line in head.lines() {
                        if let Some((k, v)) = line.split_once(':')
                            && k.trim().eq_ignore_ascii_case("content-length")
                        {
                            content_len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                if let Some(he) = header_end
                    && buf.len() >= he + content_len
                {
                    break;
                }
            }
            let req_line = String::from_utf8_lossy(&buf[..buf.iter().position(|b| *b == b'\r').unwrap_or(buf.len())]);
            let body = if req_line.starts_with("GET ") && req_line.contains("/models") {
                r#"{"data":[{"id":"mock-nano"}]}"#
            } else {
                chat_replies.pop_front().unwrap_or("{}")
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });

    let set_ai = conn.call(
        2,
        "ai.settings.set",
        serde_json::json!({
            "enabled": true,
            "baseUrl": format!("http://{addr}/v1"),
            "model": "mock-nano",
            "ptyStatusArbitration": true,
            "semanticSummary": false,
            "checkpointLabeling": false,
            "notesDigest": false,
        }),
    );
    assert!(set_ai.get("error").is_none(), "{set_ai}");

    // Ambiguous trailing "?" that AI judges as completed (`done`) never lands in `blocked`.
    let a_done = run_fake(&s, &["--prompt", "为什么之前报错？因为空指针，现已彻底修复！还需要看下别的吗？"]);
    wait_until("AI-arbitrated completed turn settles in idle", || {
        let g = get(&s, &a_done);
        g["turn"].as_u64().is_some_and(|t| t >= 1) && g["status"]["state"] == "idle"
    });

    // Subtle non-"?" blocking question -> AI promotes turn to `blocked` (`kind = question`).
    let a_ask = run_fake(&s, &["--prompt", "生产环境与预发环境配置不同，先等你拍板部署哪个环境我再继续执行。"]);
    wait_until("AI-arbitrated question turn becomes blocked", || {
        let g = get(&s, &a_ask);
        g["turn"].as_u64().is_some_and(|t| t >= 1)
            && g["status"]["state"] == "blocked"
            && g["status"]["kind"] == "question"
            && g["status"]["msg"] == "询问部署目标环境"
    });

    let _ = handle.join();
}
