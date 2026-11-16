#![cfg(target_os = "macos")]

mod common;

use common::*;
use serde_json::Value;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

fn coalition(pid: i64) -> u64 {
    let mut info = [0u64; 5];
    let size = std::mem::size_of_val(&info) as i32;
    let read = unsafe { libc::proc_pidinfo(pid as i32, 20, 0, info.as_mut_ptr().cast(), size) };
    assert_eq!(read, size, "coalition lookup for {pid}");
    info[0]
}

fn responsible(pid: i64) -> i64 {
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"responsibility_get_pid_responsible_for_pid".as_ptr()) };
    assert!(!symbol.is_null(), "responsibility lookup unavailable");
    let lookup: unsafe extern "C" fn(libc::pid_t) -> libc::pid_t = unsafe { std::mem::transmute(symbol) };
    unsafe { lookup(pid as libc::pid_t) as i64 }
}

fn gui() -> bool {
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let available = Command::new("/bin/launchctl").args(["print", &domain]).output().unwrap().status.success();
    if !available {
        eprintln!("skipping: no GUI launchd domain");
    }
    available
}

fn independent(s: &Session, pid: i64) -> bool {
    if coalition(pid) != coalition(std::process::id() as i64) {
        return true;
    }
    let log = std::fs::read_to_string(s.root.join("state/herdr2/test/core.log")).unwrap_or_default();
    if log.contains("launchctl bootstrap")
        && ["Operation not permitted", "Not privileged", "Permission denied"].iter().any(|error| log.contains(error))
    {
        eprintln!("skipping: bootstrap denied by this environment: {log}");
        return false;
    }
    panic!("core did not leave caller's coalition: {log}");
}

#[test]
fn executable_has_bound_core_identity_and_usage_descriptions() {
    let output = Command::new("/usr/bin/otool").args(["-P", BIN]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let plist = String::from_utf8(output.stdout).unwrap();
    for value in ["dev.herdr2.core", "Herdr2", "NSLocalNetworkUsageDescription", "NSAppleEventsUsageDescription"] {
        assert!(plist.contains(value), "{plist}");
    }
    assert!(!plist.contains("@VERSION@"), "{plist}");
}

#[test]
fn launchd_core_and_agents_survive_the_launcher_and_leave_no_job() {
    if !gui() {
        return;
    }
    let s = Session::new();
    let child = s.cmd().args(["server", "status"]).stdout(Stdio::piped()).spawn().unwrap();
    let launcher = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let first: Value = serde_json::from_slice(&output.stdout).unwrap();
    let core = first["pid"].as_i64().unwrap();
    if !independent(&s, core) {
        return;
    }
    let meta = std::fs::metadata(s.root.join("state/herdr2/test/core.log")).unwrap();
    assert_eq!(meta.uid(), unsafe { libc::getuid() }, "first log belongs to the caller");
    let jobs = Command::new("/bin/launchctl").arg("list").output().unwrap();
    assert!(
        !String::from_utf8_lossy(&jobs.stdout).contains(&format!("dev.herdr2.core.{launcher}.")),
        "one-shot job removed"
    );

    let a = run_fake(&s, &["--prompt", "ENV:H2_LAUNCHD_DETACH"]);
    wait_until("agent output", || text(&s, &a).contains("ENV:H2_LAUNCHD_DETACH=<unset>"));
    let agent_pid = get(&s, &a)["pid"].as_i64().unwrap();
    assert_eq!(coalition(agent_pid), coalition(core));
    assert_eq!(responsible(core), core, "detached core is live and responsible");
    s.json(&["server", "stop"]);
    wait_until("agent stopped", || !pid_alive(agent_pid));
    assert!(!s.socket().exists());
}

#[test]
fn launchd_core_registers_the_host_shutdown_watch() {
    if !gui() {
        return;
    }
    let s = Session::new();
    let core = s.json(&["server", "status"])["pid"].as_i64().unwrap();
    if !independent(&s, core) {
        return;
    }
    let log = std::fs::read_to_string(s.root.join("state/herdr2/test/core.log")).unwrap();
    assert!(!log.contains("cannot watch host shutdown"), "{log}");
}

#[test]
fn a_direct_serve_from_an_old_launcher_relaunches_before_taking_the_lock() {
    if !gui() {
        return;
    }
    let s = Session::new();
    let mut launcher = s.cmd().env("H2_LAUNCHD_DIRECT", "0").arg("serve").spawn().unwrap();
    wait_until("core listening", || s.connect_ok());
    let pid = s.json(&["server", "status"])["pid"].as_i64().unwrap();
    if !independent(&s, pid) {
        s.json(&["server", "stop"]);
        launcher.wait().unwrap();
        return;
    }
    assert!(launcher.wait().unwrap().success());
    assert_ne!(pid, launcher.id() as i64);
}

#[test]
fn launchd_preserves_environment_bytes_and_hard_file_limit_without_its_own_variables() {
    if !gui() {
        return;
    }
    let s = Session::new();
    let mut cmd = s.cmd();
    cmd.args(["server", "status"])
        .env("LESS_TERMCAP_md", "\x1b[1m\r")
        .env("H2_TEST_NONUTF8", OsString::from_vec(vec![0xff]));
    let limit = libc::rlimit { rlim_cur: 512, rlim_max: 1024 };
    unsafe {
        cmd.pre_exec(move || {
            if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    if !independent(&s, status["pid"].as_i64().unwrap()) {
        return;
    }
    let out = s.h2(&[
        "exec",
        "sh",
        "-c",
        "ulimit -Sn; ulimit -Hn; printf '%s' \"$LESS_TERMCAP_md\"; printf '\\n%s %s %s' \"${H2_LAUNCHD_DETACH-unset}\" \"${H2_LAUNCHD_DIRECT-unset}\" \"$XPC_SERVICE_NAME\"",
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, b"1024\n1024\n\x1b[1m\r\nunset unset 0");
}

#[test]
fn serve_help_does_not_start_a_core_and_leading_global_options_still_escape() {
    let s = Session::new();
    let out = s.h2(&["serve", "--help"]);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("Usage:"), "{out:?}");
    assert!(!s.socket().exists(), "help must not start a core");
    let out = s.h2(&["serve", "--not-a-real-flag"]);
    assert!(!out.status.success());
    assert!(!s.socket().exists(), "invalid arguments must not start a core");
    if !gui() {
        return;
    }
    let mut launcher = s.cmd().args(["--json", "serve"]).spawn().unwrap();
    wait_until("core listening", || s.connect_ok());
    let pid = s.json(&["server", "status"])["pid"].as_i64().unwrap();
    if !independent(&s, pid) {
        s.json(&["server", "stop"]);
        launcher.wait().unwrap();
        return;
    }
    assert!(launcher.wait().unwrap().success());
}

#[test]
fn ssh_starts_directly_without_an_app_lifetime_warning() {
    let mut s = Session::new();
    s.env.push(("SSH_CONNECTION".into(), "test".into()));
    let pid = s.json(&["server", "status"])["pid"].as_i64().unwrap();
    assert_eq!(coalition(pid), coalition(std::process::id() as i64));
    let log = std::fs::read_to_string(s.root.join("state/herdr2/test/core.log")).unwrap();
    assert!(!log.contains("launchd startup failed"), "{log}");
}

#[test]
fn launchd_failure_logs_once_and_preserves_direct_start() {
    let s = Session::new();
    let cwd = s.root.join("line\rbreak");
    std::fs::create_dir(&cwd).unwrap();
    let out = s.cmd().current_dir(cwd).args(["server", "status"]).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(coalition(status["pid"].as_i64().unwrap()), coalition(std::process::id() as i64));
    let log = std::fs::read_to_string(s.root.join("state/herdr2/test/core.log")).unwrap();
    assert_eq!(log.matches("launchd startup failed; starting directly").count(), 1, "{log}");
    assert!(log.contains("XML control character"), "{log}");
}
