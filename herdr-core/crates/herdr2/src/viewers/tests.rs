use super::*;
use crate::wire;
use std::cell::Cell;

thread_local! {
    pub(super) static PAINTS: Cell<usize> = const { Cell::new(0) };
}

#[test]
fn resize_drags_serialize_nothing_without_observers_and_only_latest_when_stalled() {
    let root = crate::test_tmp().join(format!("hc-resize-{}", crate::platform::random_hex()));
    let paths = crate::paths::Paths {
        socket: root.join("core.sock"),
        holder_socket: root.join("holder.sock"),
        state: root.clone(),
        config: root.join("config.toml"),
    };
    paths.create_dirs().unwrap();
    let core = Core::start(paths, crate::api::CoreInfo { id: "test".into(), name: "test".into() }).unwrap();
    let id = {
        let mut st = core.lock();
        let id = st
            .pty_reg
            .create(
                crate::terminals::NewTerminal { argv: vec!["sh".into()], cols: 80, rows: 24, ..Default::default() },
                0,
            )
            .unwrap();
        st.screens.insert(id, ghostty_vt::Terminal::new(80, 24, 1 << 20));
        id
    };
    let leader = Arc::new(Conn::default());
    let request = |observe, resize_notifications| AttachRequest {
        observe,
        resize_notifications,
        history: 0,
        size: (80, 24),
        display: Display::default(),
        resumed: false,
    };
    let channel = core.attach(&leader, id, request(false, false)).unwrap();
    PAINTS.set(0);
    for cols in 81..100 {
        core.viewer_resize(&leader, channel, cols, 24);
    }
    assert_eq!(PAINTS.get(), 0, "desktop-only drags must not serialize paints");
    let observer = Arc::new(Conn::default());
    let observed = core.attach(&observer, id, request(true, true)).unwrap();
    observer.with(|out| {
        out.resume(observed);
        out.data(observed, &vec![b'x'; wire::WINDOW]);
        while out.next_frame().is_some() {}
    });
    PAINTS.set(0);
    for cols in 100..200 {
        core.viewer_resize(&leader, channel, cols, 24);
        core.prepare_repaints(&observer);
    }
    assert_eq!(PAINTS.get(), 0, "no serialization while credit stalled");
    observer.with(|out| {
        assert_eq!(out.queued(observed), 0, "no obsolete paints accumulated");
        assert!(out.next_frame().is_none());
        out.credit(observed, wire::WINDOW);
    });
    core.prepare_repaints(&observer);
    assert_eq!(PAINTS.get(), 1);
    observer.with(|out| {
        let frame = out.next_frame().unwrap();
        assert_eq!(frame[4], wire::RESIZE);
        assert_eq!(&frame[wire::HEADER..], &[199, 0, 24, 0]);
        let frame = out.next_frame().unwrap();
        assert_eq!(frame[4], wire::DATA);
        assert_eq!(frame[wire::HEADER], 0x18, "cancel a partial old-grid sequence");
    });
    core.prepare_repaints(&observer);
    assert_eq!(PAINTS.get(), 1, "unchanged grid is not repainted twice");
    // Exit before the writer next wakes must still deliver the final screen.
    core.viewer_resize(&leader, channel, 200, 24);
    core.pty_record_finish(&mut core.lock(), id, crate::runtime::Exit { code: Some(0), signal: None }, "exited");
    observer.with(|out| {
        let size = out.next_frame().expect("final resize");
        assert_eq!(size[4], wire::RESIZE);
        assert_eq!(&size[wire::HEADER..], &[200, 0, 24, 0]);
        assert_eq!(out.next_frame().unwrap()[4], wire::DATA);
        while let Some(frame) = out.next_frame() {
            if frame[4] == wire::CLOSE {
                break;
            }
        }
    });
    drop(core);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn viewer_input_preserves_manual_status_on_passive_or_draft_and_clears_on_concrete_submit() {
    assert!(!is_user_interaction_input(b""));
    assert!(!is_user_interaction_input(b"\x1b[I"));
    assert!(!is_user_interaction_input(b"\x1b[O"));
    assert!(!is_user_interaction_input(b"\x1b[<35;12;8M"));
    assert!(!is_user_interaction_input(b"\x1b[<64;12;8M"));
    assert!(!is_user_interaction_input(b"\x1b[<0;12;8m"));
    assert!(is_user_interaction_input(b"\x1b[<0;12;8M"));
    assert!(is_user_interaction_input(b"y"));
    assert!(is_user_interaction_input(b"\r"));

    let root = crate::test_tmp().join(format!("hc-manual-{}", crate::platform::random_hex()));
    let paths = crate::paths::Paths {
        socket: root.join("core.sock"),
        holder_socket: root.join("holder.sock"),
        state: root.clone(),
        config: root.join("config.toml"),
    };
    paths.create_dirs().unwrap();
    let core = Core::start(paths, crate::api::CoreInfo { id: "test".into(), name: "test".into() }).unwrap();
    let id = {
        let mut st = core.lock();
        let id = st
            .pty_reg
            .create(
                crate::terminals::NewTerminal { argv: vec!["sh".into()], cols: 80, rows: 24, ..Default::default() },
                0,
            )
            .unwrap();
        st.screens.insert(id, ghostty_vt::Terminal::new(80, 24, 1 << 20));
        st.pty_reg.terminals.get_mut(&id).unwrap().labels.insert("herdr.status".into(), "done".into());
        id
    };
    let conn = Arc::new(Conn::default());
    let channel = core
        .attach(
            &conn,
            id,
            AttachRequest {
                observe: false,
                resize_notifications: false,
                history: 0,
                size: (80, 24),
                display: Display::default(),
                resumed: false,
            },
        )
        .unwrap();

    // Passive focus & wheel events must not clear manual status.
    core.viewer_input(&conn, channel, b"\x1b[I\x1b[<64;10;10M", false);
    assert_eq!(core.lock().pty_reg.terminals[&id].labels.get("herdr.status").map(String::as_str), Some("done"));

    // Automatic query answers must not clear manual status.
    core.viewer_input(&conn, channel, b"y\r", true);
    assert_eq!(core.lock().pty_reg.terminals[&id].labels.get("herdr.status").map(String::as_str), Some("done"));

    // Bare Enter with an empty draft or typing unsent draft characters must not clear manual status.
    core.viewer_input(&conn, channel, b"\r", false);
    assert_eq!(core.lock().pty_reg.terminals[&id].labels.get("herdr.status").map(String::as_str), Some("done"));
    core.viewer_input(&conn, channel, b"fix", false);
    assert_eq!(core.lock().pty_reg.terminals[&id].labels.get("herdr.status").map(String::as_str), Some("done"));

    // Submitting that non-empty prompt is a concrete new operation and clears manual status back to auto.
    core.viewer_input(&conn, channel, b"\r", false);
    assert!(!core.lock().pty_reg.terminals[&id].labels.contains_key("herdr.status"));

    drop(core);
    std::fs::remove_dir_all(root).unwrap();
}
