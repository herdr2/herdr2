use super::*;
use crate::acp::proto::AgentCapabilities;
use crate::acp::transport::Transport;
use crate::agents::NewAgent;
use crate::permissions::PublicPermissionOption;

#[tokio::test]
async fn startup_finishes_unkept_agents_and_prepares_kept_ones_for_recovery() {
    let root = crate::test_tmp().join(format!("h2-rt-start-{}", platform::random_hex()));
    let paths = || Paths {
        socket: root.join("core.sock"),
        holder_socket: root.join("holder.sock"),
        state: root.join("state"),
        config: root.join("config.toml"),
    };
    paths().create_dirs().unwrap();
    let info = || CoreInfo { id: "c1".into(), name: "box".into() };
    let core = Core::start(paths(), info()).unwrap();
    {
        let mut st = core.lock();
        let unkept = st.reg.create(NewAgent { argv: vec!["cat".into()], ..Default::default() }, 1).unwrap();
        let kept = st
            .reg
            .create(NewAgent { argv: vec!["cat".into()], keep: true, ..Default::default() }, 2)
            .unwrap();
        st.reg.agents.get_mut(&kept).unwrap().acp_session_id = Some("sess-42".into());
        core.record_transcript(
            &mut st,
            kept,
            Entry::Assistant { turn: 1, text: "persisted text".into(), attachments: vec![], ts_ms: 10 },
        );
        core.save(&st);
        assert_eq!((unkept, kept), (1, 2));
    }
    core.flush_saved().await.unwrap();
    drop(core);

    let restarted = Core::start(paths(), info()).unwrap();
    {
        let st = restarted.lock();
        assert!(!st.reg.agents[&1].is_live());
        assert_eq!(st.reg.agents[&1].reason.as_deref(), Some("stopped_by_restart"));
        assert!(st.reg.agents[&2].is_live());
        assert_eq!(st.reg.agents[&2].acp_session_id.as_deref(), Some("sess-42"));
    }
    assert_eq!(restarted.read_transcript(2, None, true).unwrap().0, "persisted text");
    drop(restarted);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn status_changed_updates_turn_and_emits_news_to_parent() {
    let root = crate::test_tmp().join(format!("h2-rt-status-{}", platform::random_hex()));
    let paths = Paths { socket: root.join("core.sock"), holder_socket: root.join("holder.sock"), state: root.join("state"), config: root.join("config.toml") };
    paths.create_dirs().unwrap();
    let core = Core::start(paths, CoreInfo { id: "c1".into(), name: "box".into() }).unwrap();
    let mut st = core.lock();
    let parent = st.reg.create(NewAgent { argv: vec!["a".into()], ..Default::default() }, 1).unwrap();
    let child = st
        .reg
        .create(NewAgent { argv: vec!["b".into()], parent: Some(address(parent)), notify: true, ..Default::default() }, 2)
        .unwrap();
    core.apply_status(&mut st, child, &StatusReport::root("working"));
    assert_eq!(st.reg.agents[&child].turn, 0);
    core.apply_status(&mut st, child, &StatusReport::root("idle"));
    assert_eq!(st.reg.agents[&child].turn, 1);
    assert_eq!(st.reg.agents[&child].news, 1);
    drop(st);
    core.flush_saved().await.unwrap();
    drop(core);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn approve_and_deny_validate_options_and_remove_cleans_transcript() {
    let root = crate::test_tmp().join(format!("h2-rt-perm-{}", platform::random_hex()));
    let paths = Paths { socket: root.join("core.sock"), holder_socket: root.join("holder.sock"), state: root.join("state"), config: root.join("config.toml") };
    paths.create_dirs().unwrap();
    let core = Core::start(paths, CoreInfo { id: "c1".into(), name: "box".into() }).unwrap();
    let id = {
        let mut st = core.lock();
        let id = st.reg.create(NewAgent { argv: vec!["sh".into()], ..Default::default() }, 1).unwrap();
        core.record_transcript(&mut st, id, Entry::User { turn: 1, from: None, text: "hi".into(), attachments: vec![], ts_ms: 1 });
        id
    };
    assert!(transcript_path(&core.paths.state, &address(id)).is_file());

    // Spawn a tiny cat transport solely so PendingPermission has a valid Transport instance.
    let (transport, waiter, _rx) = Transport::spawn(&["cat".into()], &Default::default(), "/").unwrap();
    {
        let mut st = core.lock();
        let _ = AgentCapabilities::default();
        st.pending_perms.insert(
            id,
            PendingPermission {
                request_id: "perm-1".into(),
                rpc_id: serde_json::json!(7),
                title: "Write file".into(),
                options: vec![
                    PublicPermissionOption { option_id: "allow".into(), name: "Allow".into(), kind: "allow_once".into() },
                    PublicPermissionOption { option_id: "deny".into(), name: "Deny".into(), kind: "reject_once".into() },
                ],
                transport: transport.clone(),
                turn: 1,
            },
        );
        core.apply_status(&mut st, id, &StatusReport::root("blocked"));
    }
    assert_eq!(core.resolve_permission(id, true, Some("bad")).unwrap_err().kind, "invalid_params");
    let (req_id, opt) = core.resolve_permission(id, true, None).unwrap();
    assert_eq!(req_id, "perm-1");
    assert_eq!(opt.as_deref(), Some("allow"));
    assert_eq!(core.lock().reg.agents[&id].status.as_ref().map(|s| s.state.as_str()), Some("working"));

    transport.kill();
    let _ = waiter.wait().await;
    {
        let mut st = core.lock();
        core.remove(&mut st, id);
    }
    assert!(!transcript_path(&core.paths.state, &address(id)).exists());
    core.flush_saved().await.unwrap();
    drop(core);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn kept_agent_restart_preserves_queued_inbox_as_paused_and_supports_resume_or_clear() {
    let root = crate::test_tmp().join(format!("h2-rt-paused-inbox-{}", platform::random_hex()));
    let paths = || Paths {
        socket: root.join("core.sock"),
        holder_socket: root.join("holder.sock"),
        state: root.join("state"),
        config: root.join("config.toml"),
    };
    paths().create_dirs().unwrap();
    let info = || CoreInfo { id: "c1".into(), name: "box".into() };
    let core = Core::start(paths(), info()).unwrap();
    {
        let mut st = core.lock();
        let id = st
            .reg
            .create(NewAgent { argv: vec!["cat".into()], keep: true, ..Default::default() }, 1)
            .unwrap();
        let p1 = crate::acp::QueuedPrompt::new("first queued follow-up".into(), None);
        let p2 = crate::acp::QueuedPrompt::new("second queued follow-up".into(), None);
        st.inboxes.entry(id).or_default().push_back(p1);
        st.inboxes.entry(id).or_default().push_back(p2);
        core.sync_inbox(&mut st, id, false);
        assert_eq!(st.reg.agents[&id].inbox.len(), 2);
        core.save(&st);
    }
    core.flush_saved().await.unwrap();
    drop(core);

    // Simulate reboot / core restart: reconcile_startup moves `inbox` -> `paused_inbox`.
    let restarted = Core::start(paths(), info()).unwrap();
    {
        let st = restarted.lock();
        let a = &st.reg.agents[&1];
        assert!(a.is_live());
        assert_eq!(a.inbox_len, 0);
        assert!(a.inbox.is_empty());
        assert_eq!(a.paused_inbox.len(), 2);
        assert_eq!(a.paused_inbox[0].text, "first queued follow-up");
        assert_eq!(a.paused_inbox[1].text, "second queued follow-up");
        // Public JSON exposes paused_inbox for desktop UI.
        let pub_json = serde_json::to_value(crate::agents::Public(a.clone())).unwrap();
        assert_eq!(pub_json["paused_inbox"].as_array().unwrap().len(), 2);
    }

    // Mark agent as having an active pid (simulating recovered session) and test resume_paused_inbox.
    {
        let mut st = restarted.lock();
        let a = st.reg.agents.get_mut(&1).unwrap();
        a.pid = Some(99999);
        a.reason = None;
    }
    let resumed = restarted.resume_paused_inbox(1).unwrap();
    assert!(resumed.paused_inbox.is_empty());
    assert_eq!(resumed.inbox_len, 2);
    assert_eq!(resumed.inbox[0].text, "first queued follow-up");

    // Simulate host_shutdown finish -> moves inbox back into paused_inbox, then clear_paused_inbox discards it.
    {
        let mut st = restarted.lock();
        restarted.record_finish(&mut st, 1, Exit::default(), "host_shutdown");
        assert!(st.reg.agents[&1].is_live());
        assert_eq!(st.reg.agents[&1].paused_inbox.len(), 2);
    }
    let cleared = restarted.clear_paused_inbox(1).unwrap();
    assert!(cleared.paused_inbox.is_empty());

    restarted.flush_saved().await.unwrap();
    drop(restarted);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn foreground_tracks_saved_cli_and_clears_it_on_exit_to_shell() {
    use crate::foreground::Foreground;
    use crate::terminals::{NewTerminal, Public as TerminalPublic};

    let root = crate::test_tmp().join(format!("h2-rt-saved-cli-{}", platform::random_hex()));
    let paths = Paths {
        socket: root.join("core.sock"),
        holder_socket: root.join("holder.sock"),
        state: root.join("state"),
        config: root.join("config.toml"),
    };
    paths.create_dirs().unwrap();
    let core = Core::start(paths, CoreInfo { id: "c1".into(), name: "box".into() }).unwrap();
    let cwd = root.to_string_lossy().to_string();
    let id = {
        let mut st = core.lock();
        let id = st
            .pty_reg
            .create(
                NewTerminal {
                    argv: vec!["/bin/zsh".into(), "-li".into()],
                    cwd: cwd.clone(),
                    keep: true,
                    cols: 80,
                    rows: 24,
                    ..Default::default()
                },
                1,
            )
            .unwrap();
        st.pty_reg.terminals.get_mut(&id).unwrap().pid = Some(100);
        id
    };

    // User types `claude --dangerously-skip-permissions` inside plain shell.
    {
        let mut st = core.lock();
        core.apply_foreground_update(
            &mut st,
            id,
            Some(Foreground {
                pid: 200,
                name: "claude".into(),
                argv: vec!["claude".into(), "--dangerously-skip-permissions".into(), "--session-id".into(), "sid-live".into()],
                cwd: cwd.clone(),
            }),
        );
        let t = &st.pty_reg.terminals[&id];
        let saved = t.saved_cli.as_ref().expect("saved_cli should be tracked");
        assert_eq!(saved.kind, "claude");
        assert_eq!(saved.session_id.as_deref(), Some("sid-live"));
        assert_eq!(saved.wrapper_shell.as_deref(), Some("/bin/zsh"));
        // Public JSON must keep saved_cli private.
        let pub_json = serde_json::to_value(TerminalPublic(t.clone())).unwrap();
        assert!(pub_json.get("saved_cli").is_none());
    }

    // Host shutdown preserves saved_cli on kept terminal.
    {
        let st = core.lock();
        let mut cloned = st.pty_reg.terminals[&id].clone();
        let mut reg = crate::terminals::Registry::default();
        reg.terminals.insert(id, cloned.clone());
        reg.finish(id, None, None, "host_shutdown", 50);
        assert!(reg.terminals[&id].is_live());
        assert!(reg.terminals[&id].saved_cli.is_some());
        cloned.state = "live".into();
        reg.terminals.insert(id, cloned);
        reg.finish(id, Some(0), None, "exited", 60);
        assert!(!reg.terminals[&id].is_live());
        assert!(reg.terminals[&id].saved_cli.is_none());
    }

    // User exits claude back to interactive shell (pid 100 = zsh) -> saved_cli must be cleared!
    {
        let mut st = core.lock();
        core.apply_foreground_update(
            &mut st,
            id,
            Some(Foreground { pid: 100, name: "zsh".into(), argv: vec!["/bin/zsh".into(), "-li".into()], cwd }),
        );
        assert!(st.pty_reg.terminals[&id].saved_cli.is_none(), "exiting to shell must clear saved_cli");
    }

    core.flush_saved().await.unwrap();
    drop(core);
    std::fs::remove_dir_all(root).unwrap();
}
