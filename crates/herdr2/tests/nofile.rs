mod common;

use common::*;
use std::os::unix::process::CommandExt;

#[test]
fn core_raises_low_nofile_and_handles_many_acp_agents() {
    let s = Session::new();
    let mut cmd = s.cmd();
    cmd.args(["serve"]);
    unsafe {
        cmd.pre_exec(|| {
            let lim = libc::rlimit { rlim_cur: 256, rlim_max: 4096 };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &lim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let mut core = cmd.spawn().unwrap();

    wait_until("core socket ready", || s.connect_ok());

    let out = s.h2(&["exec", "sh", "-c", "ulimit -Sn"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let soft: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    assert!(soft >= 1024, "expected core to raise NOFILE from 256 to >= 1024, got {soft}");

    let addrs: Vec<String> = (0..32).map(|_| run_fake(&s, &[])).collect();
    assert_eq!(addrs.len(), 32);
    let ls = s.json(&["ls"]);
    assert_eq!(ls["agents"].as_array().unwrap().len(), 32);
    for a in addrs {
        s.json(&["kill", &a]);
    }
    s.json(&["server", "stop"]);
    let _ = core.wait();
}
