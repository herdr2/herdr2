#![cfg(windows)]
//! Windows end to end: named pipes, job objects, ACLs, SSH transport,
//! environment refresh, and headless ACP execution through the CLI.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_herdr2");

fn fake_acp_spec() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_acp_agent.py");
    format!("python3 {}", p.display())
}

#[test]
fn via_runs_without_a_console_and_passes_the_remote_line_whole() {
    let s = Session::new();
    std::fs::create_dir_all(s.root.join("via dir")).unwrap();
    let probe = s.file(
        r"via dir\via.ps1",
        r#"Add-Type 'using System; using System.Runtime.InteropServices; public static class ViaProbe {
            [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }'
[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'console.txt'), [ViaProbe]::GetConsoleWindow().ToInt64().ToString())
[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'line.txt'), [Environment]::GetCommandLineArgs()[-1])
$request = [Console]::ReadLine() | ConvertFrom-Json
if ($request.method -ne 'hello') { exit 2 }
[Console]::WriteLine('{"id":1,"error":{"code":-32000,"message":"via-probe","data":{"kind":"probe","hint":"done"}}}')
"#,
    );
    let via = format!("  powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{probe}\"");
    let output = cmd(&s.name, &s.root).args(["machine", "add", "--via", &via, "--name", "box"]).output().unwrap();
    let error: Value = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(error["error"]["kind"], "probe", "{error}");
    assert_eq!(std::fs::read_to_string(s.root.join("via dir/console.txt")).unwrap(), "0");
    let line = std::fs::read_to_string(s.root.join("via dir/line.txt")).unwrap();
    assert!(line.starts_with("sh -c 'p=$(command -v herdr2)") && line.ends_with("exec \"$p\" connect'"), "{line}");
}

static LIVE: Mutex<Vec<(String, PathBuf)>> = Mutex::new(Vec::new());

struct Session {
    name: String,
    root: PathBuf,
}

impl Session {
    fn new() -> Session {
        static N: AtomicU32 = AtomicU32::new(0);
        static WATCHDOG: std::sync::Once = std::sync::Once::new();
        WATCHDOG.call_once(|| {
            std::thread::spawn(|| {
                let limit = std::env::var("H2_TEST_WATCHDOG_S").ok().and_then(|s| s.parse().ok()).unwrap_or(90);
                std::thread::sleep(Duration::from_secs(limit));
                eprintln!("test binary still running after {limit} s: stopping its cores and aborting");
                let live = LIVE.lock().unwrap_or_else(|p| p.into_inner()).clone();
                for (name, root) in live {
                    let Ok(mut stop) = cmd(&name, &root).args(["server", "stop"]).spawn() else { continue };
                    let start = std::time::Instant::now();
                    while stop.try_wait().ok().flatten().is_none() && start.elapsed() < Duration::from_secs(10) {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    let _ = stop.kill();
                }
                std::process::abort();
            });
        });
        let name = format!("test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
        let root = std::env::temp_dir().join(format!("h2-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        LIVE.lock().unwrap().push((name.clone(), root.clone()));
        Session { name, root }
    }

    fn out(&self, args: &[&str]) -> (i32, String) {
        let out = cmd(&self.name, &self.root).args(args).output().unwrap();
        if !out.stderr.is_empty() {
            eprintln!("{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn json(&self, args: &[&str]) -> Value {
        let (_, text) = self.out(args);
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{args:?}: {e}: {text}"))
    }

    fn bg_fake(&self, extra: &[&str]) -> String {
        let spec = fake_acp_spec();
        let mut args = vec!["run", "--bg", "--agent", &spec];
        args.extend_from_slice(extra);
        self.json(&args)["address"].as_str().unwrap().to_string()
    }

    fn file(&self, name: &str, body: &str) -> String {
        let path = self.root.join(name);
        std::fs::write(&path, body).unwrap();
        path.display().to_string()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = cmd(&self.name, &self.root).args(["server", "stop"]).output();
        LIVE.lock().unwrap().retain(|(n, _)| *n != self.name);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn cmd(session: &str, root: &Path) -> Command {
    cmd_at(Path::new(BIN), session, root)
}

fn cmd_at(exe: &Path, session: &str, root: &Path) -> Command {
    let mut cmd = Command::new(exe);
    cmd.current_dir(root)
        .env("H2_SESSION", session)
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env_remove("H2_AGENT_ID")
        .env_remove("H2_SOCKET")
        .env_remove("SSH_CONNECTION")
        .env_remove("SSH_CLIENT")
        .env_remove("SSH_TTY");
    cmd
}

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(start.elapsed() < Duration::from_secs(20), "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn alive(pid: u64) -> bool {
    let out = Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}

#[test]
fn sessions_select_distinct_pipes_even_with_the_same_state_home() {
    let a = Session::new();
    let b = Session::new();
    let status = |name: &str| {
        let out = cmd(name, &a.root).args(["server", "start"]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    let first = status(&a.name);
    let second = status(&b.name);
    assert_ne!(first["pid"], second["pid"], "H2_SESSION must select the pipe, not just state");
    assert_eq!(status(&a.name)["pid"], first["pid"]);
}

#[test]
fn ssh_reaches_the_host_without_a_control_socket() {
    use std::net::TcpListener;
    use std::sync::{Arc, atomic::AtomicBool};

    let s = Session::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = format!("ssh://127.0.0.1:{}", listener.local_addr().unwrap().port());
    listener.set_nonblocking(true).unwrap();
    let finished = Arc::new(AtomicBool::new(false));
    let stop = finished.clone();
    let server = std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok(_) => return true,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("accept: {e}"),
            }
        }
        false
    });
    let output = cmd(&s.name, &s.root).env_remove("H2_SSH").args(["machine", "add", &target]).output().unwrap();
    finished.store(true, Ordering::Relaxed);
    assert!(server.join().unwrap(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn ssh_auth_failure_points_to_keys_instead_of_repeating_login() {
    let s = Session::new();
    let ssh =
        s.file("ssh-denied.cmd", "@echo off\r\necho Permission denied (publickey,password). 1>&2\r\nexit /b 255\r\n");
    let output = cmd(&s.name, &s.root).env("H2_SSH", &ssh).args(["machine", "add", "example"]).output().unwrap();
    let error: Value = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(error["error"]["kind"], "needs_login", "{error}");
    let hint = error["error"]["hint"].as_str().unwrap();
    assert!(hint.contains("keys or an agent") && !hint.contains("h2 machine add"), "{hint}");
}

#[test]
fn ssh_transport_has_no_console_and_keeps_its_pipes() {
    let s = Session::new();
    let probe = s.file(
        "ssh-console.ps1",
        r#"Add-Type 'using System; using System.Runtime.InteropServices; public static class ConsoleProbe {
            [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }'
[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'console-window.txt'), [ConsoleProbe]::GetConsoleWindow().ToInt64().ToString())
$request = [Console]::ReadLine() | ConvertFrom-Json
if ($request.method -ne 'hello') { exit 2 }
[Console]::WriteLine('{"id":1,"error":{"code":-32000,"message":"ssh-probe","data":{"kind":"probe","hint":"done"}}}')
"#,
    );
    let ssh = s.file(
        "ssh-console.cmd",
        &format!("@echo off\r\npowershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{probe}\"\r\n"),
    );
    let output = cmd(&s.name, &s.root).env("H2_SSH", &ssh).args(["machine", "add", "example"]).output().unwrap();
    let error: Value = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(error["error"]["kind"], "probe", "{error}");
    assert_eq!(std::fs::read_to_string(s.root.join("console-window.txt")).unwrap(), "0");
}

fn release(dir: &Path, mark: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let mut exe = std::fs::read(BIN).unwrap();
    exe.extend_from_slice(mark.as_bytes());
    std::fs::write(dir.join("herdr2.exe"), exe).unwrap();
    dir.join("herdr2.exe")
}

fn marked(file: &Path, mark: &str) -> bool {
    std::fs::read(file).unwrap().ends_with(mark.as_bytes())
}

fn installed(s: &Session) -> PathBuf {
    let exe = release(&s.root.join("inst"), "old-build");
    std::fs::hard_link(&exe, s.root.join(r"inst\h2.exe")).unwrap();
    exe
}

fn h2_at(s: &Session, exe: &Path, args: &[&str]) -> std::process::Output {
    cmd_at(exe, &s.name, &s.root).args(args).output().unwrap()
}

#[test]
fn update_swaps_the_release_and_recovers_kept_agents() {
    let s = Session::new();
    let exe = installed(&s);
    let spec = fake_acp_spec();
    let run = h2_at(&s, &exe, &["run", "--bg", "--keep", "--agent", &spec]);
    let a = serde_json::from_slice::<Value>(&run.stdout).unwrap()["address"].as_str().unwrap().to_string();
    let theirs = [s.root.join(r"inst\settings.old-backup"), s.root.join(r"inst\h2.exe.old-backup")];
    for f in &theirs {
        std::fs::write(f, "keep").unwrap();
    }
    let leftover = s.root.join(r"inst\herdr2.exe.old-123-0123456789abcdef0123456789abcdef");
    std::fs::write(&leftover, "old").unwrap();
    let new = release(&s.root.join("new"), "new-build");
    let out = h2_at(&s, &exe, &["update", "--from", new.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(theirs.iter().all(|f| f.is_file()), "update deleted a file that isn't h2's");
    assert!(!leftover.exists(), "an old update's leftover stays");
    assert!(marked(&exe, "new-build"), "herdr2.exe swapped");
    assert!(marked(&s.root.join(r"inst\h2.exe"), "new-build"), "h2.exe follows it");
    wait_until("kept agent recovered", || s.json(&["get", &a])["state"] == "live");
}

#[test]
fn the_exe_needs_no_visual_cpp_runtime_installed() {
    let exe = String::from_utf8_lossy(&std::fs::read(BIN).unwrap()).to_ascii_lowercase();
    for dll in ["vcruntime140", "msvcp140", "api-ms-win-crt-"] {
        assert!(!exe.contains(dll), "herdr2.exe imports {dll}: a clean Windows can't run it");
    }
}

#[test]
fn only_this_account_may_open_the_cores_pipe() {
    let s = Session::new();
    s.out(&["server", "start"]);
    let user: String = std::env::var("USERNAME").unwrap().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let pipe = format!(r"\\.\pipe\herdr2-{user}-{}.sock", s.name);
    let sddl = unsafe {
        use windows_sys::Win32::Security::Authorization::*;
        use windows_sys::Win32::Security::*;
        let name: Vec<u16> = pipe.encode_utf16().chain(Some(0)).collect();
        use std::ptr::null_mut;
        let mut sd = null_mut();
        let err = GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        );
        assert_eq!(err, 0, "reading {pipe}'s permissions");
        let mut text = null_mut();
        assert_ne!(
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                null_mut()
            ),
            0
        );
        String::from_utf16_lossy(std::slice::from_raw_parts(text, (0..).take_while(|&i| *text.add(i) != 0).count()))
    };
    let account = sddl
        .strip_prefix("D:P(A;;FA;;;SY)(A;;FA;;;")
        .and_then(|r| r.strip_suffix(')'))
        .unwrap_or_else(|| panic!("{sddl}"));
    assert!(!account.contains('('), "more than one account: {sddl}");
    for group in ["WD", "AN", "BA", "BU", "AU", "IU"] {
        assert_ne!(account, group, "{group} has access: {sddl}");
    }
}

fn connect_once(s: &Session, stdin: std::process::Stdio, input: Option<&str>) -> String {
    let mut child =
        cmd(&s.name, &s.root).arg("connect").stdin(stdin).stdout(std::process::Stdio::piped()).spawn().unwrap();
    if let Some(input) = input {
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), input.as_bytes()).unwrap();
    }
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(child.wait_with_output()));
    let Ok(out) = rx.recv_timeout(Duration::from_secs(20)) else {
        let _ = Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).output();
        panic!("h2 connect did not exit after its input ended");
    };
    let out = out.unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn connect_answers_a_caller_that_is_done_sending() {
    let s = Session::new();
    s.json(&["server", "start"]);
    let get = |id: u32| json!({"jsonrpc": "2.0", "id": id, "method": "doc.get", "params": {"name": "probe"}});
    let file = s.root.join("request.json");
    std::fs::write(&file, format!("{}\n", get(1))).unwrap();
    for _ in 0..5 {
        for out in [
            connect_once(&s, std::process::Stdio::piped(), Some(&format!("{}\n", get(1)))),
            connect_once(&s, std::fs::File::open(&file).unwrap().into(), None),
        ] {
            let reply: Value = serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out:?}"));
            assert_eq!(reply["id"], 1, "{out}");
            assert_eq!(reply["result"]["version"], 0, "{out}");
        }
        assert_eq!(connect_once(&s, std::process::Stdio::null(), None), "");
    }
    let notification = json!({"jsonrpc": "2.0", "method": "doc.get", "params": {"name": "probe"}});
    let input = format!("{}\n{notification}\n\nnot json\n{}", get(1), get(2));
    let out = connect_once(&s, std::process::Stdio::piped(), Some(&input));
    let ids: Vec<Value> = out.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()["id"].clone()).collect();
    assert_eq!(ids, [json!(1), Value::Null, json!(2)], "{out}");
    for last in [
        json!({"jsonrpc": "2.0", "id": 3, "method": "events.subscribe", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "hello", "params": {}}),
    ] {
        let out = connect_once(&s, std::process::Stdio::piped(), Some(&format!("{last}\n")));
        let first = out.lines().next().unwrap_or_default();
        let reply: Value = serde_json::from_str(first).unwrap_or_else(|e| panic!("{e}: {out:?}"));
        assert_eq!(reply["id"], 3, "{out}");
    }
}

#[test]
fn status_does_not_start_a_core() {
    let s = Session::new();
    let out = cmd(&s.name, &s.root).args(["server", "status"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("h2 server start"));
    assert!(!s.root.join("state").exists(), "status created runtime state");
}

#[test]
fn exec_returns_exact_bytes_and_ends_what_it_started_at_its_timeout() {
    let s = Session::new();
    let out = cmd(&s.name, &s.root).args(["exec", "cmd", "/c", "echo a\tb& exit 3"]).output().unwrap();
    assert_eq!((out.stdout.as_slice(), out.status.code()), (&b"a\tb\r\n"[..], Some(3)));

    let pid_file = s.root.join("exec-pid.txt");
    let child =
        s.file("exec.ps1", &format!("$PID | Out-File -Encoding ascii '{}'; Start-Sleep 60", pid_file.display()));
    let (code, _) = s.out(&["exec", "--timeout", "5", "cmd", "/c", &format!("powershell -NoProfile -File {child}")]);
    assert_eq!(code, 124);
    let pid: u64 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    wait_until("grandchild gone", || !alive(pid));
}

#[test]
fn kill_ends_the_agent_and_acp_prompts_work() {
    let s = Session::new();
    let a = s.bg_fake(&[]);
    let pid = s.json(&["get", &a])["pid"].as_u64().unwrap();
    assert!(alive(pid));
    let sent = s.json(&["send", &a, "hi", "--wait", "--timeout", "15"]);
    assert_eq!(sent["turn"], 1, "{sent}");
    assert!(s.json(&["read", &a])["text"].as_str().unwrap().contains("echo: hi"));
    s.json(&["kill", &a]);
    wait_until("agent gone", || !alive(pid));
}

#[test]
fn a_copied_disks_restart_environment_does_not_replace_the_new_machines() {
    let s = Session::new();
    let machine = herdr2::platform::machine_id().expect("Windows machine ID");
    let state = s.root.join("state/herdr2").join(&s.name);
    std::fs::create_dir_all(&state).unwrap();
    let snapshot = json!({
        "machine_id": format!("other-{machine}"),
        "boot_id": null,
        "written_ms": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64,
        "vars": [(std::ffi::OsString::from("H2_SESSION_VALUE"), std::ffi::OsString::from("copied-machine"))],
    });
    std::fs::write(state.join("restart-env.json"), serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let out = cmd(&s.name, &s.root).env("H2_SESSION_VALUE", "new-machine").args(["server", "start"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let (code, out) = s.out(&["exec", "cmd", "/d", "/c", "echo %H2_SESSION_VALUE%"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("new-machine"), "{out}");
}

#[test]
fn a_failed_environment_handoff_still_restarts_the_core() {
    let s = Session::new();
    let before = s.json(&["server", "start"])["pid"].clone();
    let blocked = s.root.join("state/herdr2").join(&s.name).join("restart-env.json");
    std::fs::create_dir(&blocked).unwrap();
    let (code, _) = s.out(&["server", "restart"]);
    assert_eq!(code, 0);
    assert_ne!(s.json(&["server", "status"])["pid"], before);
    std::fs::remove_dir(blocked).unwrap();
}
