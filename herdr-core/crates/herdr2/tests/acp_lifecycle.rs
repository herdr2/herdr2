mod common;

use common::*;
use serde_json::Value;

fn state_json(s: &Session, a: &str) -> Value {
    s.json(&["send", "--wait", a, "STATE"]);
    let last = s.json(&["read", "--last", a])["text"].as_str().unwrap().to_owned();
    let line = last.lines().find(|l| l.starts_with('{')).unwrap_or_else(|| panic!("no STATE json in {last:?}"));
    serde_json::from_str(line).unwrap()
}

#[test]
fn headless_capabilities_and_stripped_terminal_env_are_enforced() {
    let mut s = Session::new();
    s.env.extend([
        ("TERM".into(), "xterm-256color".into()),
        ("COLORTERM".into(), "truecolor".into()),
        ("TERM_PROGRAM".into(), "iTerm.app".into()),
        ("H2_EXTRA_KEEP".into(), "preserved".into()),
    ]);

    let spec = fake_acp_spec();
    let run = s.json(&[
        "run",
        "--agent",
        &spec,
        "--prompt",
        "ENV:TERM\nENV:COLORTERM\nENV:TERM_PROGRAM\nENV:H2_AGENT_ID\nENV:H2_EXTRA_KEEP",
    ]);
    let addr = run["address"].as_str().unwrap();
    let out = run["output"].as_str().unwrap();
    assert!(out.contains("ENV:TERM=<unset>"), "{out}");
    assert!(out.contains("ENV:COLORTERM=<unset>"), "{out}");
    assert!(out.contains("ENV:TERM_PROGRAM=<unset>"), "{out}");
    assert!(out.contains(&format!("ENV:H2_AGENT_ID={addr}")), "{out}");
    assert!(out.contains("ENV:H2_EXTRA_KEEP=preserved"), "{out}");

    let st = state_json(&s, addr);
    assert_eq!(st["client_caps"]["fs"]["readTextFile"], false, "{st}");
    assert_eq!(st["client_caps"]["fs"]["writeTextFile"], false, "{st}");
    assert_eq!(st["client_caps"]["terminal"], false, "{st}");
}

#[test]
fn presets_templates_model_mode_and_system_prompt_flow_to_acp() {
    let s = Session::new();
    let parts: Vec<String> = fake_acp_spec().split_whitespace().map(|p| format!("{p:?}")).collect();
    let conf_dir = s.root.join("config/herdr2");
    std::fs::create_dir_all(conf_dir.join("templates")).unwrap();
    std::fs::write(conf_dir.join("config.toml"), format!("[agents]\nfake = [{}]\n", parts.join(", "))).unwrap();
    std::fs::write(
        conf_dir.join("templates/review.md"),
        "---\nagent: fake\nmodel: m-tmpl\nmode: code\npermissions: approve-all\nsystem_prompt: System rule\n---\nReview carefully\n",
    )
    .unwrap();

    let tmpls = s.json(&["templates"]);
    assert_eq!(tmpls["templates"][0]["name"], "review", "{tmpls}");

    let a = s.json(&["run", "--bg", "review", "CHECK_TASK"])["address"].as_str().unwrap().to_owned();
    wait_until("template prompt finished", || {
        let g = get(&s, &a);
        g["turn"].as_u64().is_some_and(|t| t >= 1) && g["status"]["state"] == "idle"
    });
    let hist = s.json(&["read", "--lines", "50", &a])["text"].as_str().unwrap().to_owned();
    assert!(hist.contains("echo: Review carefully") && hist.contains("echo: CHECK_TASK"), "{hist}");

    let st = state_json(&s, &a);
    assert_eq!(st["model"], "m-tmpl", "{st}");
    assert_eq!(st["mode"], "code", "{st}");
    assert!(st["system_prompt"].as_str().unwrap_or("").contains("System rule"), "{st}");

    let (_, shim_err) = s.err(&["--json", "run", "--bg", "--agent", "/tmp/.windsurf-server/bin/claude"]);
    assert_eq!(shim_err["error"]["kind"], "config", "{shim_err}");

    let b =
        run_fake(&s, &["--model", "gpt-test", "--mode", "plan", "--system-prompt", "Be terse", "--prompt", "STATE"]);
    wait_until("b finished STATE", || get(&s, &b)["turn"] == 1 && get(&s, &b)["status"]["state"] == "idle");
    let st_b = serde_json::from_str::<Value>(
        s.json(&["read", "--last", &b])["text"].as_str().unwrap().lines().find(|l| l.starts_with('{')).unwrap(),
    )
    .unwrap();
    assert_eq!(st_b["model"], "gpt-test", "{st_b}");
    assert_eq!(st_b["mode"], "plan", "{st_b}");
    assert!(st_b["system_prompt"].as_str().unwrap().contains("Be terse"), "{st_b}");
}

#[test]
fn waited_run_read_modes_interrupt_kill_and_rm_work_cleanly() {
    let s = Session::new();
    let spec = fake_acp_spec();
    let res = s.json(&["run", "--agent", &spec, "--prompt", "line-one\nline-two\nline-three"]);
    let a = res["address"].as_str().unwrap().to_owned();
    assert!(res["output"].as_str().unwrap().contains("echo: line-two"));

    let full = s.json(&["read", &a])["text"].as_str().unwrap().to_owned();
    assert!(full.contains("echo: line-one") && full.contains("echo: line-three"), "{full}");
    let last = s.json(&["read", "--last", &a])["text"].as_str().unwrap().to_owned();
    assert!(last.contains("echo: line-one") && !last.contains("[user"), "--last returns assistant text only: {last}");
    let tailed = s.json(&["read", "--lines", "1", &a])["text"].as_str().unwrap().to_owned();
    assert_eq!(tailed.lines().count(), 1, "{tailed}");

    s.json(&["send", &a, "SLEEP:10000"]);
    wait_until("working on sleep", || get(&s, &a)["status"]["state"] == "working");
    let intr = s.json(&["interrupt", &a]);
    assert_eq!(intr["outcome"], "stopped", "{intr}");
    wait_until("back to idle after interrupt", || get(&s, &a)["status"]["state"] == "idle");
    assert!(text(&s, &a).contains("cancelled"));

    // Still usable for further turns after interrupt.
    s.json(&["send", "--wait", &a, "after-cancel"]);
    assert!(s.json(&["read", "--last", &a])["text"].as_str().unwrap().contains("echo: after-cancel"));

    let killed = s.json(&["kill", &a]);
    assert_eq!(killed["state"], "finished", "{killed}");
    s.json(&["rm", &a]);
    assert_eq!(s.err(&["--json", "get", &a]).1["error"]["kind"], "not_found");
}

#[test]
fn async_start_returns_starting_immediately_and_records_spawn_failure() {
    let s = Session::new();
    // Ensure daemon is started, then connect over Unix socket.
    s.json(&["server", "status"]);
    let mut conn = s.connect();

    // Valid fake ACP agent with async_start: returns immediately with "starting", then transitions to "idle".
    let parts: Vec<String> = fake_acp_spec().split_whitespace().map(str::to_owned).collect();
    let res = conn.call(
        1,
        "agent.run",
        serde_json::json!({
            "argv": parts,
            "keep": true,
            "async_start": true,
        }),
    );
    let ok = &res["result"];
    let addr = ok["address"].as_str().unwrap().to_owned();
    assert_eq!(ok["status"]["state"], "starting", "{res}");
    wait_until("async_start becomes idle", || get(&s, &addr)["status"]["state"] == "idle");

    // Nonexistent binary with async_start + keep: returns immediately, then records Error + spawn_failed with keep=false.
    let bad = conn.call(
        2,
        "agent.run",
        serde_json::json!({
            "argv": ["herdr2-missing-binary-for-test-999"],
            "keep": true,
            "async_start": true,
        }),
    );
    let bad_addr = bad["result"]["address"].as_str().unwrap().to_owned();
    assert_eq!(bad["result"]["status"]["state"], "starting", "{bad}");
    wait_until("bad async_start transitions to finished spawn_failed", || {
        let g = get(&s, &bad_addr);
        g["state"] == "finished" && g["reason"] == "spawn_failed"
    });
    let g = get(&s, &bad_addr);
    assert_eq!(g["keep"], false, "{g}");
    assert!(text(&s, &bad_addr).contains("not found") || text(&s, &bad_addr).contains("No such file"), "{g}");
}

#[test]
fn session_config_options_and_h2_config_update_work_end_to_end() {
    let s = Session::new();
    let a = run_fake(&s, &[]);
    let initial = get(&s, &a);
    let opts = initial["config_options"].as_array().unwrap();
    assert!(opts.iter().any(|o| o["id"] == "model"), "{initial}");
    assert!(opts.iter().any(|o| o["id"] == "reasoning_effort"), "{initial}");
    assert!(opts.iter().any(|o| o["id"] == "fast-mode" && o["kind"] == "boolean"), "{initial}");

    let after_model = s.json(&["config", &a, "--id", "model", "--value", "gpt-test-2"]);
    assert_eq!(after_model["model"], "gpt-test-2", "{after_model}");

    let after_effort = s.json(&["config", &a, "--id", "reasoning_effort", "--value", "xhigh"]);
    let effort_opt =
        after_effort["config_options"].as_array().unwrap().iter().find(|o| o["id"] == "reasoning_effort").unwrap();
    assert_eq!(effort_opt["current_value"], "xhigh", "{after_effort}");

    let after_fast = s.json(&["config", &a, "--id", "fast-mode", "--bool", "true"]);
    let fast_opt = after_fast["config_options"].as_array().unwrap().iter().find(|o| o["id"] == "fast-mode").unwrap();
    assert_eq!(fast_opt["current_value"], true, "{after_fast}");

    let after_bypass = s.json(&["config", &a, "--id", "mode", "--value", "dangerously-skip-permissions"]);
    assert_eq!(after_bypass["mode"], "bypassPermissions", "{after_bypass}");
    assert_eq!(after_bypass["permission_mode"], "approve-all", "{after_bypass}");

    let st = state_json(&s, &a);
    assert_eq!(st["model"], "gpt-test-2", "{st}");
    assert_eq!(st["reasoning_effort"], "xhigh", "{st}");
    assert_eq!(st["fast_mode"], true, "{st}");
    assert_eq!(st["mode"], "bypassPermissions", "{st}");

    let skipped = run_fake(&s, &["--dangerously-skip-permissions"]);
    let skipped_rec = get(&s, &skipped);
    assert_eq!(skipped_rec["permission_mode"], "approve-all", "{skipped_rec}");
    assert_eq!(skipped_rec["mode"], "bypassPermissions", "{skipped_rec}");
}

#[test]
fn claude_preset_defaults_to_dangerously_skip_permissions() {
    let s = Session::new();
    let parts: Vec<String> = fake_acp_spec().split_whitespace().map(|p| format!("{p:?}")).collect();
    let conf_dir = s.root.join("config/herdr2");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(conf_dir.join("config.toml"), format!("[agents]\nclaude = [{}]\n", parts.join(", "))).unwrap();

    let a = s.json(&["run", "--bg", "--agent", "claude"])["address"].as_str().unwrap().to_owned();
    let rec = get(&s, &a);
    assert_eq!(rec["mode"], "bypassPermissions", "{rec}");
    assert_eq!(rec["permission_mode"], "approve-all", "{rec}");
    let st = state_json(&s, &a);
    assert_eq!(st["mode"], "bypassPermissions", "{st}");

    // Explicit non-bypass mode is still respected.
    let manual =
        s.json(&["run", "--bg", "--agent", "claude", "--mode", "default"])["address"].as_str().unwrap().to_owned();
    let manual_rec = get(&s, &manual);
    assert_eq!(manual_rec["mode"], "default", "{manual_rec}");
    assert_eq!(manual_rec["permission_mode"], "approve-reads", "{manual_rec}");
}

#[test]
fn agent_send_preserves_manual_status_until_explicitly_cleared() {
    let s = Session::new();
    let a1 = run_fake(&s, &[]);
    let a2 = run_fake(&s, &[]);

    s.json(&["label", &a1, "herdr.status=done"]);
    s.json(&["label", &a2, "herdr.status=blocked"]);
    assert_eq!(get(&s, &a1)["labels"]["herdr.status"], "done");
    assert_eq!(get(&s, &a2)["labels"]["herdr.status"], "blocked");

    let send_out = s.cmd().env("H2_AGENT_ID", &a1).args(["send", &a2, "--wait", "PING"]).output().unwrap();
    assert!(send_out.status.success(), "{}", String::from_utf8_lossy(&send_out.stderr));

    let after_a1 = get(&s, &a1);
    let after_a2 = get(&s, &a2);
    assert_eq!(after_a1["labels"]["herdr.status"], "done", "sender manual status must remain pinned: {after_a1}");
    assert_eq!(after_a2["labels"]["herdr.status"], "blocked", "receiver manual status must remain pinned: {after_a2}");

    // Explicit clearing restores auto mode.
    s.json(&["label", &a1, "herdr.status="]);
    assert!(get(&s, &a1)["labels"].get("herdr.status").is_none());
}

#[test]
fn acp_available_commands_update_populates_get_read_and_omits_from_list() {
    let s = Session::new();
    let spec = format!("{} --emit-commands", fake_acp_spec());
    let agent = s.json(&["run", "--bg", "--agent", &spec])["address"].as_str().unwrap().to_owned();

    wait_until("available_commands populated on agent.get", || {
        get(&s, &agent)["available_commands"].as_array().is_some_and(|a| a.len() == 3)
    });

    let rec = get(&s, &agent);
    let cmds = rec["available_commands"].as_array().unwrap();
    assert_eq!(cmds[0]["name"], "plan");
    assert_eq!(cmds[0]["source"], "command");
    assert_eq!(cmds[1]["name"], "compact");
    assert_eq!(cmds[1]["input_hint"], "[focus]");
    assert_eq!(cmds[2]["name"], "$drove");
    assert_eq!(cmds[2]["source"], "skill");

    // `agent.read` returns normalized available_commands too.
    let read = s.json(&["read", "--json", &agent]);
    let read_cmds = read["available_commands"].as_array().expect("read available_commands");
    assert_eq!(read_cmds.len(), 3);
    assert_eq!(read_cmds[2]["name"], "$drove");

    // `agent.list` stays compact without available_commands.
    let list = s.json(&["ls"]);
    let entry = list["agents"].as_array().unwrap().iter().find(|a| a["address"] == agent).unwrap();
    assert!(entry.get("available_commands").is_none(), "agent.list should omit available_commands: {entry}");
}
