use super::*;
use crate::events::{Filter, Sink};
use crate::paths::Paths;
use std::sync::atomic::AtomicBool;

#[tokio::test]
async fn removing_or_replacing_a_machine_drops_its_status_poll() {
    for replacement in [false, true] {
        let root = std::path::PathBuf::from(format!("/var/tmp/h2-poll-{}", crate::platform::random_hex()));
        let paths = Paths { socket: root.join("core.sock"), holder_socket: root.join("holder.sock"), state: root.clone(), config: root.join("config.toml") };
        paths.create_dirs().unwrap();
        let core = Core::start(paths, crate::api::CoreInfo { id: "here".into(), name: "here".into() }).unwrap();
        let machine: Machine = serde_json::from_value(json!({
            "name": "away", "target": "away", "id": "away", "host": "away",
            "version": "test", "session": null
        }))
        .unwrap();
        core.lock().machines.push(machine.clone());
        let mut child = tokio::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let dialed = crate::machines::Dialed {
            stdin: child.stdin.take().unwrap(),
            stdout: tokio::io::BufReader::new(child.stdout.take().unwrap()),
            stderr: child.stderr.take().unwrap(),
            child,
            hello: json!({}),
        };
        let owner = core.clone();
        let task = tokio::spawn(async move { hold(&owner, &machine, dialed).await });
        core.lock().line_tasks.insert("away".into(), task.abort_handle());
        let line = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(LineState::Connected(line)) = core.lock().lines.get("away")
                    && !lock(&line.pending).is_empty()
                {
                    break line.clone();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let weak = Arc::downgrade(&line);
        drop(line);
        {
            let mut st = core.lock();
            forget(&core, &mut st, "away");
            if replacement {
                st.lines.insert("away".into(), LineState::Connecting);
            } else {
                st.machines.clear();
            }
        }
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(1), async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("removed line is still retained by a detached poll");
        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }
}

#[tokio::test]
async fn a_send_on_an_already_closed_line_fails_without_waiting() {
    for writer_gone in [false, true] {
        let (frames, mut rx) = mpsc::unbounded_channel();
        let line = Line::new(frames, &json!({}));
        if writer_gone {
            rx.close();
        } else {
            line.close(); // The writer may not have finished shutting down yet.
        }
        let sent = tokio::time::timeout(
            Duration::from_millis(200),
            line.call("away", "agent.send", json!({"agent": "a1", "text": "hi"}), Duration::from_secs(30)),
        )
        .await
        .expect("a closed transport must not hold a send");
        assert!(sent.unwrap_err().message.contains("lost the line"));
        assert!(lock(&line.pending).is_empty());
    }
}

#[test]
fn remote_misses_and_renames_keep_subscribers_on_the_current_machine() {
    let root = crate::test_tmp().join(format!("h2-doc-missed-{}-{}", std::process::id(), crate::platform::random_hex()));
    let paths = Paths { socket: root.join("core.sock"), holder_socket: root.join("holder.sock"), state: root.clone(), config: root.join("config.toml") };
    paths.create_dirs().unwrap();
    let core = Core::start(paths, crate::api::CoreInfo { id: "here".into(), name: "here".into() }).unwrap();
    let machine = Machine {
        name: "rohan".into(),
        target: "rohan".into(),
        via: false,
        temporary: false,
        session: None,
        id: "remote".into(),
        host: "rohan".into(),
        version: "test".into(),
        keeps_running_after_logout: None,
        logout_linger_user: None,
    };
    let (tx, mut rx) = mpsc::channel(4);
    let (filtered_tx, mut filtered_rx) = mpsc::channel(4);
    {
        let mut st = core.lock();
        st.events.seq = 7;
        st.machines.push(machine.clone());
        core.subscribe(&mut st, Filter::default(), Sink::Plain { tx, behind: Arc::new(AtomicBool::new(false)) });
        core.subscribe(
            &mut st,
            Filter { agent: Some("a1".into()), ..Default::default() },
            Sink::Plain { tx: filtered_tx, behind: Arc::new(AtomicBool::new(false)) },
        );
    }
    let (frames, _rx) = mpsc::unbounded_channel();
    let line = Line::new(frames, &json!({}));
    mirror_events(&core, &machine, &line, b"{\"method\":\"missed\",\"params\":{\"seq\":9999,\"agents\":[]}}\n");
    let msg: Value = serde_json::from_str(&rx.try_recv().expect("local missed notification")).unwrap();
    assert_eq!(msg["method"], "missed");
    assert_eq!(msg["params"]["seq"], 7);
    assert!(filtered_rx.try_recv().is_err());
    mirror_replace(&core, &machine, vec![json!({"address":"a1", "state":"live", "status":{"state":"working"}})]);
    rx.try_recv().unwrap();
    let conn = Arc::new(Conn::default());
    conn.with(|out| {
        out.open(1);
        assert!(out.data_lines(1, &vec![b'\n'; wire::MAX_PENDING]));
    });
    {
        let mut st = core.lock();
        core.subscribe(
            &mut st,
            Filter { agent: Some("renamed/a1".into()), ..Default::default() },
            Sink::Framed { conn: conn.clone(), channel: 1 },
        );
        st.machines[0].name = "renamed".into();
        rename_mirror(&core, &mut st, &machine.id, "rohan", "renamed");
    }
    let mut bytes = Vec::new();
    conn.with(|out| {
        while let Some(frame) = out.next_frame() {
            bytes.extend_from_slice(&frame[wire::HEADER..]);
        }
    });
    let missed = bytes
        .split(|b| *b == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .find(|line| line["method"] == "missed")
        .expect("backpressure snapshot");
    assert_eq!(missed["params"]["agents"][0]["address"], "renamed/a1");
    let removed: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
    let created: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
    assert_eq!(removed["params"]["agent"], "rohan/a1");
    assert_eq!(removed["params"]["type"], "removed");
    assert_eq!(created["params"]["agent"], "renamed/a1");
    assert_eq!(created["params"]["data"]["status"]["state"], "working");
    mirror_events(
        &core,
        &machine,
        &line,
        b"{\"method\":\"event\",\"params\":{\"type\":\"status\",\"agent\":\"a1\",\"data\":{\"status\":{\"state\":\"blocked\"}}}}\n",
    );
    let status: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
    assert_eq!(status["params"]["agent"], "renamed/a1");
    assert_eq!(core.lock().mirrors[&machine.id]["renamed/a1"]["status"]["state"], "blocked");
    core.lock().machines.clear();
    mirror_event(&core, &machine, json!({"type": "doc", "data": {"name": "app", "version": 1}}));
    assert!(rx.try_recv().is_err(), "removed machines must not emit doc events");
    drop(core);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn remote_handoff_localizes_from_address_and_routes_into() {
    let root = crate::test_tmp().join(format!("h2-handoff-remote-{}-{}", std::process::id(), crate::platform::random_hex()));
    let paths = Paths { socket: root.join("core.sock"), holder_socket: root.join("holder.sock"), state: root.clone(), config: root.join("config.toml") };
    paths.create_dirs().unwrap();
    let core = Core::start(paths, crate::api::CoreInfo { id: "here".into(), name: "here".into() }).unwrap();
    let machine = Machine {
        name: "wsl".into(),
        target: "wsl".into(),
        via: false,
        temporary: false,
        session: None,
        id: "wsl-id".into(),
        host: "wsl-host".into(),
        version: "test".into(),
        keeps_running_after_logout: None,
        logout_linger_user: None,
    };
    core.lock().machines.push(machine);

    // 1. Transcript `Entry::Handoff` must localize `from_address` while leaving non-address `source` fields intact.
    let read_reply = localize(
        &core,
        "wsl",
        &[],
        json!({
            "entries": [{
                "kind": "handoff",
                "turn": 0,
                "handoff_kind": "side-chat",
                "from_address": "t22",
                "from_provider": "claude",
                "from_turns": "1-3"
            }],
            "config_options": [{
                "id": "model",
                "name": "Model",
                "kind": "select",
                "current_value": "opus",
                "source": "legacy_model"
            }]
        }),
    );
    assert_eq!(read_reply["entries"][0]["from_address"], "wsl/t22");
    assert_eq!(read_reply["config_options"][0]["source"], "legacy_model");

    // 2. `HandoffResult` localizes `target`, `source`, and `lineage.parent`.
    let ho_reply = localize(
        &core,
        "wsl",
        &[],
        json!({
            "target": "a4",
            "target_kind": "agent",
            "kind": "side-chat",
            "source": "t22",
            "lineage": { "kind": "side-chat", "parent": "t22" }
        }),
    );
    assert_eq!(ho_reply["target"], "wsl/a4");
    assert_eq!(ho_reply["source"], "wsl/t22");
    assert_eq!(ho_reply["lineage"]["parent"], "wsl/t22");

    // 3. `route` for remote `agent.handoff` strips matching machine prefix from both `source` and `into`.
    let routed = route(
        &core,
        "agent.handoff",
        &json!({ "source": "wsl/a4", "into": "wsl/t22", "kind": "handoff" }),
    )
    .unwrap()
    .expect("routed to wsl");
    assert_eq!(routed.1["source"], "a4");
    assert_eq!(routed.1["into"], "t22");

    // 4. `route` for remote `ai.handoff_recommend` and `checkpoint.bisect` strips matching machine prefix.
    let rec_routed = route(
        &core,
        "ai.handoff_recommend",
        &json!({ "source": "wsl/a4", "installedPresets": ["claude", "codex"] }),
    )
    .unwrap()
    .expect("ai.handoff_recommend routed to wsl");
    assert_eq!(rec_routed.1["source"], "a4");

    let bisect_routed = route(
        &core,
        "checkpoint.bisect",
        &json!({ "target": "wsl/t22", "query": "which turn broke auth?" }),
    )
    .unwrap()
    .expect("checkpoint.bisect routed to wsl");
    assert_eq!(bisect_routed.1["target"], "t22");

    drop(core);
    std::fs::remove_dir_all(root).unwrap();
}
