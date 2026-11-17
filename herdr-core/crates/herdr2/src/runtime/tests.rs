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
            Entry::Assistant { turn: 1, text: "persisted text".into(), ts_ms: 10 },
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
        core.record_transcript(&mut st, id, Entry::User { turn: 1, from: None, text: "hi".into(), ts_ms: 1 });
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
