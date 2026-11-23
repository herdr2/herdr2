#![cfg(unix)] // Windows: tests/windows.rs

mod common;

use common::*;
use serde_json::{Value, json};
use std::time::Duration;

fn with_rohan() -> (Session, Session) {
    with_rohan_env(&[])
}

fn with_rohan_env(env: &[(&str, &str)]) -> (Session, Session) {
    let rohan = Session::new();
    let mut s = Session::new();
    s.env.push(fake_ssh(&s, &[("rohan", &rohan)]));
    s.env.push(("H2_RECONNECT_MS".into(), "200".into()));
    s.env.extend(env.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    s.json(&["machine", "add", "rohan"]);
    (rohan, s)
}

fn addresses(list: &Value) -> Vec<String> {
    list["agents"].as_array().unwrap().iter().map(|a| a["address"].as_str().unwrap().to_string()).collect()
}

fn history(s: &Session, a: &str) -> String {
    s.json(&["read", "--lines", "200", a])["text"].as_str().unwrap().to_string()
}

fn recording_ssh(local: &Session, remote: &Session) -> (String, std::path::PathBuf) {
    let ssh = local.root.join("record-ssh");
    let line_pid = local.root.join("line-pid");
    let r = remote.root.display();
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\necho $$ > {}\nexec env -i HOME={r} PATH={}:/usr/bin:/bin H2_SESSION=test \\
         XDG_RUNTIME_DIR={r}/run XDG_STATE_HOME={r}/state XDG_CONFIG_HOME={r}/config {BIN} connect\n",
            line_pid.display(),
            remote.bin.parent().unwrap().display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    (ssh.display().to_string(), line_pid)
}

fn next_params(conn: &mut Conn, pick: impl Fn(&Value) -> bool) -> Value {
    next_event(conn, |m| m["method"] == "event" && pick(&m["params"]))["params"].clone()
}

fn spawn_child_on_rohan(s: &Session, parent: &str, args: &[&str]) -> String {
    let spec = fake_acp_spec();
    let out = s
        .cmd()
        .env("H2_AGENT_ID", parent)
        .args(["--json", "run", "--bg", "--on", "rohan", "--agent", &spec])
        .args(args)
        .output()
        .unwrap();
    let started: Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)));
    assert_eq!(started["parent"], parent, "{started}");
    started["address"].as_str().unwrap().into()
}

#[test]
fn a_frozen_machine_never_freezes_h2() {
    let mut rohan = Session::new();
    rohan.env.push(("H2_HEARTBEAT_MS".into(), "200".into()));
    let pid = rohan.json(&["server", "status"])["pid"].as_i64().unwrap() as i32;
    let mut s = Session::new();
    s.env.push(fake_ssh(&s, &[("rohan", &rohan)]));
    s.env.extend([("H2_RECONNECT_MS", "200"), ("H2_HEARTBEAT_MS", "200")].map(|(k, v)| (k.into(), v.into())));
    let config = s.root.join("config/herdr2/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(config, "machine_timeout = 2\n").unwrap();
    s.json(&["machine", "add", "rohan"]);
    let state = |s: &Session| s.json(&["machine", "ls"])["machines"][0]["state"].clone();
    wait_until("connected", || state(&s) == "connected");

    unsafe { libc::kill(pid, libc::SIGSTOP) };
    let started = std::time::Instant::now();
    let spec = fake_acp_spec();
    let (_, err) = s.err(&["run", "--bg", "--on", "rohan", "--agent", &spec]);
    let took = started.elapsed();
    let thawed = || unsafe { libc::kill(pid, libc::SIGCONT) };
    if took > Duration::from_secs(6) {
        thawed();
        panic!("a call to a frozen machine took {took:?}: {err}");
    }
    assert_eq!(err["error"]["kind"], "unavailable", "{err}");
    let quiet = std::time::Instant::now();
    while state(&s) == "connected" && quiet.elapsed() < Duration::from_secs(8) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let after = state(&s);
    let started = std::time::Instant::now();
    let (_, err) = s.err(&["run", "--bg", "--on", "rohan", "--agent", &spec]);
    let took = started.elapsed();
    thawed();
    assert_ne!(after, "connected", "a silent line is given up");
    assert!(took < Duration::from_secs(6), "a call while it redials took {took:?}: {err}");
    wait_until("back once it answers again", || state(&s) == "connected");
}

#[test]
fn a_big_answer_on_a_slow_link_is_not_mistaken_for_a_frozen_machine() {
    let mut rohan = Session::new();
    rohan.env.push(("H2_HEARTBEAT_MS".into(), "200".into()));
    let mut s = Session::new();
    let (var, fake) = fake_ssh(&s, &[("rohan", &rohan)]);
    let slow = s.root.join("slow-ssh");
    let throttle = "import sys, time\nwhile b := sys.stdin.buffer.read1(32768):\n    sys.stdout.buffer.write(b); sys.stdout.buffer.flush(); time.sleep(0.1)";
    std::fs::write(&slow, format!("#!/bin/sh\n{fake} \"$@\" | python3 -c '{throttle}'\n")).unwrap();
    std::fs::set_permissions(&slow, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    s.env.push((var, slow.display().to_string()));
    let config = s.root.join("config/herdr2/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(config, "machine_timeout = 2\n").unwrap();
    s.env.push(("H2_HEARTBEAT_MS".into(), "200".into()));
    rohan.json(&["server", "start"]);
    s.json(&["machine", "add", "rohan"]);

    std::thread::sleep(Duration::from_secs(1));
    let r = s.json(&["exec", "--on", "rohan", "head", "-c", "750000", "/dev/zero"]);
    assert_eq!(r["stdout"].as_str().unwrap().len(), 1_000_000, "took ~3 s to arrive, still flowing");
}

#[test]
fn two_machines_that_added_each_other_list_at_once() {
    let (mut rohan, mut s) = (Session::new(), Session::new());
    s.env.push(fake_ssh(&s, &[("rohan", &rohan)]));
    rohan.env.push(fake_ssh(&rohan, &[("desk", &s)]));
    let (here, there) = (run_fake(&s, &[]), run_fake(&rohan, &[]));
    s.json(&["machine", "add", "rohan"]);
    rohan.json(&["machine", "add", "desk"]);
    let wants = [[format!("rohan/{there}"), here.clone()], [format!("desk/{here}"), there]];
    for (s, mut want) in [&s, &rohan].into_iter().zip(wants) {
        let started = std::time::Instant::now();
        let list = s.json(&["ls"]);
        assert!(started.elapsed() < Duration::from_secs(2), "took {:?}", started.elapsed());
        let mut got = addresses(&list);
        got.sort();
        want.sort();
        assert_eq!(got, want, "each lists its own and the other's, nothing relayed back");
    }
    rohan.json(&["machine", "rm", "desk"]);
}

#[test]
fn two_machines_that_added_each_other_never_echo_events_back() {
    let (mut rohan, mut s) = (Session::new(), Session::new());
    s.env.push(fake_ssh(&s, &[("rohan", &rohan)]));
    rohan.env.push(fake_ssh(&rohan, &[("desk", &s)]));
    run_fake(&s, &[]);
    run_fake(&rohan, &[]);
    s.json(&["machine", "add", "rohan"]);
    rohan.json(&["machine", "add", "desk"]);
    wait_until("both connected", || {
        [&s, &rohan].iter().all(|s| s.json(&["machine", "ls"])["machines"][0]["state"] == "connected")
    });
    let mut c = s.connect();
    c.call(1, "events.subscribe", json!({}));
    s.connect().call(2, "doc.set", json!({"name": "app", "value": 1, "version": 0}));
    c.reader.get_ref().set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    let (started, mut events) = (std::time::Instant::now(), 0);
    while started.elapsed() < Duration::from_secs(2) {
        let mut line = String::new();
        events += usize::from(std::io::BufRead::read_line(&mut c.reader, &mut line).is_ok_and(|n| n > 0));
    }
    assert!(events < 20, "{events} events in 2 s");
    rohan.json(&["machine", "rm", "desk"]);
}

#[test]
fn agents_on_another_machine_work_like_local_ones() {
    let (rohan, s) = with_rohan_env(&[("H2_TEST_SSH_DELAY", "0.4")]);
    let local = run_fake(&s, &[]);
    let a = run_fake(&s, &["--on", "rohan", "--name", "echo", "--prompt", "ENV:HOME"]);
    assert_eq!(a, "rohan/a1");
    assert_eq!(addresses(&rohan.json(&["ls"])), ["a1"]);
    assert_eq!(addresses(&s.json(&["ls"])), [local.as_str(), "rohan/a1"]);

    wait_until("its output", || text(&s, &a).contains(&rohan.root.display().to_string()));
    s.json(&["send", "--wait", "rohan/echo", "hello"]);
    assert!(text(&s, "rohan/echo").contains("echo: hello"));
    assert_eq!(get(&s, &a)["name"], "echo");

    let done = run_fake(&s, &["--on", "rohan", "--prompt", "SLEEP:250\nEXIT:3"]);
    let w = s.json(&["wait", &done]);
    assert_eq!((w["agent"].as_str(), w["exit_code"].as_i64()), (Some(done.as_str()), Some(3)), "{w}");

    rohan.h2(&["server", "restart"]);
    assert!(text(&s, &a).contains("echo: hello"), "no 'unreachable' in between");
    assert_eq!(s.json(&["kill", &a])["state"], "finished");
    s.json(&["rm", &a]);
    assert_eq!(s.err(&["--json", "read", "nope/a1"]).1["error"]["kind"], "machine_unknown");
}

#[test]
fn a_first_foreground_remote_child_can_reach_only_its_parent() {
    let (rohan, s) = with_rohan();
    let parent = run_fake(&s, &[]);
    let unrelated = run_fake(&s, &[]);
    let core_id = s.json(&["server", "status"])["core"]["id"].as_str().unwrap().to_string();
    let spec = fake_acp_spec();
    let prompt = format!("MCP_CALL:h2_read:{{\"agent\":\"{core_id}/{parent}\"}}");
    let out = s
        .cmd()
        .env("H2_AGENT_ID", &parent)
        .args(["--json", "run", "--on", "rohan", "--no-notify", "--agent", &spec, "--prompt", &prompt])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    let text_out = result["output"].as_str().unwrap();
    assert!(text_out.contains("MCP_RESULT:") && !text_out.contains("MCP_ERROR:"), "{result}");

    let (_, denied) = rohan.err(&["--json", "get", &format!("{core_id}/{unrelated}")]);
    assert_eq!(denied["error"]["kind"], "denied", "{denied}");
}

#[test]
fn failed_or_detached_remote_runs_do_not_grant_access_to_the_caller() {
    let (rohan, s) = with_rohan();
    let remote_parent = run_fake(&rohan, &[]);
    let core_id = s.json(&["server", "status"])["core"]["id"].as_str().unwrap().to_string();
    let spec = fake_acp_spec();
    for kind in ["no-parent", "remote-parent", "missing"] {
        let parent = run_fake(&s, &[]);
        let mut cmd = s.cmd();
        cmd.env("H2_AGENT_ID", &parent).args(["--json", "run", "--on", "rohan", "--no-notify"]);
        match kind {
            "no-parent" => {
                cmd.args(["--no-parent", "--agent", &spec]);
            }
            "remote-parent" => {
                cmd.args(["--parent", &remote_parent, "--agent", &spec]);
            }
            _ => {
                cmd.args(["--agent", "missing-acp-program-xyz"]);
            }
        }
        let out = cmd.output().unwrap();
        if kind == "missing" {
            assert!(!out.status.success());
            let error: Value = serde_json::from_slice(&out.stderr).unwrap();
            assert_eq!(error["error"]["kind"], "spawn_failed", "{error}");
        } else {
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let result: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(
                result["parent"],
                if kind == "remote-parent" { json!(format!("rohan/{remote_parent}")) } else { Value::Null },
                "{result}"
            );
        }
        let (_, denied) = rohan.err(&["--json", "get", &format!("{core_id}/{parent}")]);
        assert_eq!(denied["error"]["kind"], "denied", "{denied}");
    }
}

#[test]
fn a_failed_remote_launch_preserves_a_concurrent_and_then_established_grant() {
    let (rohan, s) = with_rohan();
    let parent = run_fake(&s, &[]);
    let core_id = s.json(&["server", "status"])["core"]["id"].as_str().unwrap().to_string();
    let address = format!("{core_id}/{parent}");
    let spec = fake_acp_spec();
    let mut command = s.cmd();
    command.env("H2_AGENT_ID", &parent).args([
        "--json",
        "run",
        "--on",
        "rohan",
        "--no-notify",
        "--agent",
        &spec,
        "--prompt",
        "HOLD",
    ]);
    let foreground = std::thread::spawn(move || command.output().unwrap());
    wait_until("remote child started", || !addresses(&rohan.json(&["ls"])).is_empty());

    for pending in [true, false] {
        let failed = s
            .cmd()
            .env("H2_AGENT_ID", &parent)
            .args(["--json", "run", "--on", "rohan", "--agent", "missing-acp-program-xyz"])
            .output()
            .unwrap();
        assert!(!failed.status.success());
        assert_eq!(rohan.json(&["get", &address])["state"], "live");
        if pending {
            rohan.json(&["interrupt", "a1"]);
            wait_until("foreground run completes", || foreground.is_finished());
        }
    }
    let out = foreground.join().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_child_on_another_machine_reports_back_to_its_parent() {
    let (rohan, s) = with_rohan();
    let parent = run_fake(&s, &[]);
    let child = spawn_child_on_rohan(&s, &parent, &["--prompt", "HOLD"]);
    let parent_there = get(&rohan, "a1")["parent"].as_str().unwrap().to_string();
    assert!(parent_there.ends_with(&format!("/{parent}")), "{parent_there}");

    let said = rohan.cmd().env("H2_AGENT_ID", "a1").args(["send", &parent_there, "hi-parent"]).output().unwrap();
    assert!(said.status.success(), "{}", String::from_utf8_lossy(&said.stderr));
    wait_until("the child's message", || {
        let h = history(&s, &parent);
        h.contains(&format!("<h2 from=\"{child}\">hi-parent</h2>")) && h.contains("echo: hi-parent")
    });

    let other = run_fake(&s, &[]);
    let prefix = parent_there.split('/').next().unwrap();
    let (_, refused) = rohan.err(&["--json", "send", &format!("{prefix}/{other}"), "x"]);
    assert_eq!(refused["error"]["kind"], "denied", "only its parents: {refused}");

    rohan.json(&["send", "--steer", "a1", "EXIT:3"]);
    wait_until("the exit note", || {
        let h = history(&s, &parent);
        h.contains(&format!("{child} exited (exit 3)")) && h.contains(&format!("h2 read {child}"))
    });
}

#[test]
fn a_send_from_outside_an_agent_on_another_machine_arrives_without_h2_tag() {
    let (_rohan, s) = with_rohan();
    let agent = run_fake(&s, &["--on", "rohan"]);
    s.json(&["send", "--wait", &agent, "hello-plain"]);
    let out = text(&s, &agent);
    assert!(out.contains("echo: hello-plain"), "{out}");
    assert!(!out.contains("<h2"), "plain caller is not wrapped in <h2>: {out}");
}

#[test]
fn remote_templates_and_permissions_work_across_machines() {
    let (rohan, s) = with_rohan();
    let templates = rohan.root.join("config/herdr2/templates");
    std::fs::create_dir_all(&templates).unwrap();
    let spec = fake_acp_spec();
    std::fs::write(templates.join("worker.md"), format!("---\nagent: {spec}\n---\nRemote system note\n")).unwrap();

    let parent = run_fake(&s, &[]);
    let out = s
        .cmd()
        .env("H2_AGENT_ID", &parent)
        .args(["--json", "run", "--bg", "--on", "rohan", "worker", "WRITE_TOOL"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let child = serde_json::from_slice::<Value>(&out.stdout).unwrap()["address"].as_str().unwrap().to_owned();

    wait_until("remote child blocked on WRITE_TOOL", || get(&s, &child)["status"]["state"] == "blocked");
    wait_until("parent notified of remote block", || {
        let h = history(&s, &parent);
        h.contains("blocked") && h.contains(&child)
    });

    s.json(&["approve", &child]);
    wait_until("remote child finished write", || {
        let g = get(&s, &child);
        g["status"]["state"] == "idle" && g["turn"].as_u64().is_some_and(|t| t >= 2)
    });
    assert!(history(&s, &child).contains("write_tool=selected:allow-once"));
}

#[test]
fn waiting_on_children_counts_the_ones_on_other_machines() {
    let (_rohan, s) = with_rohan();
    let parent = run_fake(&s, &[]);
    let child = spawn_child_on_rohan(&s, &parent, &["--prompt", "HOLD"]);
    let wait = |mode: &str, timeout: &str| {
        let out = s
            .cmd()
            .env("H2_AGENT_ID", &parent)
            .args(["--json", "wait", "--children", mode, "--timeout", timeout])
            .output()
            .unwrap();
        serde_json::from_slice::<Value>(&out.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)))
    };
    let still = wait("all", "0.5");
    assert_eq!((&still["ended"], &still["children"][0]["agent"]), (&json!(false), &json!(child)), "{still}");

    s.json(&["interrupt", &child]);
    let any = wait("any", "10");
    assert_eq!(
        (&any["ended"], &any["agent"], &any["children"][0]["agent"]),
        (&json!(true), &json!(parent), &json!(child)),
        "{any}"
    );
    let all = wait("all", "10");
    assert_eq!((&all["ended"], &all["children"][0]["status"]), (&json!(true), &json!("idle")), "{all}");
}

#[test]
fn a_child_ends_with_its_parent_on_another_machine_too() {
    let (rohan, s) = with_rohan();
    let parent = run_fake(&s, &["--rm", "--prompt", "HOLD"]);
    let _child = spawn_child_on_rohan(&s, &parent, &["--prompt", "HOLD"]);
    s.json(&["send", "--steer", &parent, "EXIT:0"]);
    wait_until("the child there ended", || get(&rohan, "a1")["state"] == "finished");
}

#[test]
fn a_note_made_while_the_line_is_down_arrives_when_it_is_back() {
    let (rohan, s) = with_rohan();
    let parent = run_fake(&s, &[]);
    let child = spawn_child_on_rohan(&s, &parent, &["--prompt", "HOLD"]);
    std::fs::write(s.root.join("down"), "").unwrap();
    rohan.h2(&["server", "restart"]);
    wait_until("rohan unreachable", || s.json(&["machine", "ls"])["machines"][0]["state"] == "unreachable");
    std::fs::remove_file(s.root.join("down")).unwrap();
    wait_until("the exit note", || {
        let txt = history(&s, &parent);
        txt.contains(&child) && txt.contains("exited")
    });
}

#[test]
fn events_cover_agents_on_other_machines() {
    let (_rohan, s) = with_rohan();
    let mut all = s.connect();
    all.call(1, "events.subscribe", json!({}));
    let a = run_fake(&s, &["--on", "rohan", "--prompt", "HOLD"]);
    let created = next_params(&mut all, |e| e["type"] == "created" && e["agent"] == a.as_str());
    assert_eq!(created["data"]["address"], a.as_str(), "{created}");

    s.json(&["interrupt", &a]);
    let idle = next_params(&mut all, |e| {
        e["type"] == "status" && e["agent"] == a.as_str() && e["data"]["status"]["state"] == "idle"
    });
    assert!(idle["seq"].as_u64().unwrap() > created["seq"].as_u64().unwrap());

    let mut one = s.connect();
    let snap = one.call(1, "events.subscribe", json!({"agent": a}));
    let agents = snap["result"]["agents"].as_array().unwrap().clone();
    assert_eq!((agents.len(), &agents[0]["status"]["state"]), (1, &json!("idle")), "{snap}");
    s.json(&["kill", &a]);
    next_params(&mut one, |e| e["type"] == "finished" && e["agent"] == a.as_str());
}

#[test]
fn a_run_or_wait_there_rides_out_a_dropped_line() {
    let rohan = Session::new();
    rohan.json(&["server", "start"]);
    let mut s = Session::new();
    let (ssh, line_pid) = recording_ssh(&s, &rohan);
    s.env.push(("H2_SSH".into(), ssh));
    s.env.push(("H2_RECONNECT_MS".into(), "150".into()));
    s.json(&["machine", "add", "rohan"]);
    let drop_line = || {
        let pid: i32 = std::fs::read_to_string(&line_pid).unwrap().trim().parse().unwrap();
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    };

    let spec = fake_acp_spec();
    let waited = {
        let mut cmd = s.cmd();
        cmd.args([
            "--json",
            "run",
            "--on",
            "rohan",
            "--timeout",
            "30",
            "--agent",
            &spec,
            "--prompt",
            "SLEEP:1200\nfinished-fine",
        ]);
        std::thread::spawn(move || cmd.output().unwrap())
    };
    wait_until("started there", || !addresses(&rohan.json(&["ls"])).is_empty());
    drop_line();
    let out = waited.join().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let res: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(res["output"].as_str().unwrap().contains("echo: finished-fine"), "{res}");

    let a = run_fake(&s, &["--on", "rohan", "--prompt", "SLEEP:1000\nEXIT:0"]);
    let waiting = {
        let mut cmd = s.cmd();
        cmd.args(["--json", "wait", &a, "--timeout", "30"]);
        std::thread::spawn(move || cmd.output().unwrap())
    };
    std::thread::sleep(Duration::from_millis(200));
    drop_line();
    let out = waiting.join().unwrap();
    let r: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    assert_eq!(
        (&r["ended"], &r["exit_code"]),
        (&json!(true), &json!(0)),
        "{r} / {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_machine_that_drops_off_is_shown_not_waited_on_and_comes_back() {
    let (rohan, s) = with_rohan();
    run_fake(&s, &["--on", "rohan", "--keep"]);
    assert_eq!(s.json(&["machine", "ls"])["machines"][0]["state"], "connected");

    std::fs::write(s.root.join("down"), "").unwrap();
    rohan.h2(&["server", "restart"]);
    wait_until("rohan unreachable", || s.json(&["machine", "ls"])["machines"][0]["state"] == "unreachable");
    let list = s.json(&["ls"]);
    assert_eq!(list["machines"][0]["state"], "unreachable", "{list}");
    assert!(addresses(&list).is_empty());
    assert_eq!(s.err(&["--json", "read", "rohan/a1"]).1["error"]["kind"], "unavailable");

    std::fs::remove_file(s.root.join("down")).unwrap();
    wait_until("rohan back", || s.json(&["machine", "ls"])["machines"][0]["state"] == "connected");
    wait_until("kept agent back on rohan", || addresses(&s.json(&["ls"])) == ["rohan/a1"]);
}
