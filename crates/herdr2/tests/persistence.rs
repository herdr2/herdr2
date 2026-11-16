mod common;

use common::*;
use std::time::{Duration, Instant};

#[test]
fn failed_registry_and_stderr_writes_do_not_stop_persistence() {
    let s = Session::new();
    let state = s.root.join("state/herdr2/test");
    std::fs::create_dir_all(&state).unwrap();

    let a1 = run_fake(&s, &["--keep"]);
    // Simulate a broken/ENOSPC target at agents.tmp and core.log while core saves registry state.
    let tmp = state.join("agents.tmp");
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink("/dev/full", &tmp).unwrap();
    std::fs::remove_file(state.join("core.log")).ok();
    std::os::unix::fs::symlink("/dev/full", state.join("core.log")).unwrap();

    let a2 = run_fake(&s, &["--keep"]);
    // Removing the poisoned temp file lets atomic replacement write agents.json cleanly again.
    std::fs::remove_file(&tmp).unwrap();
    s.json(&["label", &a2, "saved=yes"]);

    let pid1 = get(&s, &a1)["pid"].as_i64().unwrap();
    let pid2 = get(&s, &a2)["pid"].as_i64().unwrap();
    s.json(&["server", "restart"]);
    wait_until("kept agents persisted after transient ENOSPC", || {
        let g1 = get(&s, &a1);
        let g2 = get(&s, &a2);
        g1["state"] == "live"
            && g1["pid"].as_i64().is_some_and(|p| p != pid1)
            && g2["state"] == "live"
            && g2["pid"].as_i64().is_some_and(|p| p != pid2)
    });
    assert_eq!(get(&s, &a2)["labels"]["saved"], "yes");
}

#[test]
fn server_stop_does_not_hang_if_a_write_is_blocked() {
    let s = Session::new();
    let state = s.root.join("state/herdr2/test");
    std::fs::create_dir_all(&state).unwrap();
    let _ = run_fake(&s, &[]);
    // Even if core.log points to /dev/full, server stop completes promptly.
    std::fs::remove_file(state.join("core.log")).ok();
    std::os::unix::fs::symlink("/dev/full", state.join("core.log")).unwrap();

    let start = Instant::now();
    let out = s.h2(&["server", "stop"]);
    assert!(out.status.success(), "server stop failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(start.elapsed() < Duration::from_secs(5), "server stop hung for {:?}", start.elapsed());
}
