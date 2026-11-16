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
