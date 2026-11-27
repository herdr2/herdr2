#![cfg(unix)] // Windows: tests/windows.rs

mod common;

use common::*;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;

fn state_json(s: &Session, a: &str) -> Value {
    s.json(&["send", "--wait", a, "STATE"]);
    let last = s.json(&["read", "--last", a])["text"].as_str().unwrap().to_owned();
    let line = last.lines().find(|l| l.starts_with('{')).unwrap_or_else(|| panic!("no STATE json in {last:?}"));
    serde_json::from_str(line).unwrap()
}

#[test]
fn an_up_to_date_binary_restarts_only_an_older_running_core() {
    let mut s = Session::new();
    let version = env!("CARGO_PKG_VERSION");
    std::fs::create_dir_all(s.root.join("core")).unwrap();
    std::fs::write(
        s.root.join("core/latest.json"),
        json!({"version": version, "generation": herdr2::api::GENERATION, "targets": {}}).to_string(),
    )
    .unwrap();
    s.env.push(("HERDR2_RELEASES_URL".into(), format!("file://{}", s.root.display())));
    let args = ["update", "--version", version];
    assert_eq!(s.json(&args)["updated"], false);
    assert!(!s.connect_ok(), "checking an absent core must not start one");
    std::fs::create_dir_all(s.socket().parent().unwrap()).unwrap();
    for (running, latest) in
        [(version, false), ("999.0.0", false), ("unknown", false), ("0.0.0", false), ("0.0.0", true)]
    {
        let listener = UnixListener::bind(s.socket()).unwrap();
        let socket = s.socket();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(req["method"], "server.status");
            writeln!(stream, "{}", json!({"id": 1, "result": {"version": running}})).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            if running == "0.0.0" {
                let req: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(req["method"], "server.restart");
                writeln!(stream, "{}", json!({"id": 1, "result": {}})).unwrap();
            } else {
                assert!(line.is_empty(), "must leave {running} running");
            }
            drop(listener);
            std::fs::remove_file(socket).unwrap();
        });
        let out = s.json(if latest { &["update"] } else { &args });
        server.join().unwrap();
        assert_eq!(out["updated"], false);
        if running == "0.0.0" {
            assert_eq!(out["restarted"], true);
            assert_eq!(s.json(&["server", "status"])["version"], version);
            s.json(&["server", "stop"]);
        } else {
            assert_ne!(out["restarted"], true);
        }
    }
}

#[test]
fn an_install_in_a_read_only_folder_explains_how_to_update_it() {
    if unsafe { libc::geteuid() } == 0 {
        return; // Root can replace files in read-only directories.
    }
    let mut s = Session::new();
    let dir = s.root.join("bin");
    std::fs::create_dir(&dir).unwrap();
    s.bin = dir.join("herdr2");
    std::fs::copy(BIN, &s.bin).unwrap();
    s.json(&["server", "start"]);
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o555)).unwrap();
    let status = s.cmd().args(["server", "status"]).output().unwrap();
    let update = s.cmd().args(["update", "--from", BIN]).output().unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["installed_by"], "system package manager");
    let error: Value = serde_json::from_slice(&update.stderr).unwrap();
    assert!(!update.status.success());
    assert_eq!(error["error"]["kind"], "refused", "{error}");
    assert_eq!(error["error"]["message"], format!("{} isn't writable by you", dir.canonicalize().unwrap().display()));
    assert_eq!(error["error"]["hint"], "update herdr2 the way it was installed (package manager, or sudo)");
}

#[test]
fn update_from_a_file_swaps_h2_and_keeps_kept_agents_running() {
    let mut s = Session::new();
    s.env.push(("H2_SESSION_VALUE".into(), "original".into()));
    let dir = s.root.join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(BIN, dir.join("herdr2")).unwrap();
    std::os::unix::fs::symlink("herdr2", dir.join("h2")).unwrap();
    s.bin = dir.join("h2"); // run as `h2`, the way people do
    let a = run_fake(&s, &["--keep"]);
    let old_pid = get(&s, &a)["pid"].as_i64().unwrap();
    let old_sid = state_json(&s, &a)["session_id"].as_str().unwrap().to_owned();
    let pid = s.json(&["server", "status"])["pid"].clone();

    let out = s
        .cmd()
        .env("NO_COLOR", "1")
        .env("H2_SESSION_VALUE", "updater")
        .env("H2_UPDATER_ONLY", "leaked")
        .args(["update", "--from", BIN])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let r: Value = serde_json::from_slice(&out.stdout).unwrap();
    let real = std::fs::canonicalize(dir.join("herdr2")).unwrap(); // macOS: /var is /private/var
    assert_eq!(r["path"], real.display().to_string(), "the file h2 runs from, not the link: {r}");
    assert!(r["was"].as_str().unwrap().starts_with("h2 ") && r["now"].as_str().unwrap().starts_with("h2 "), "{r}");
    assert_eq!(r["restarted"], true);
    assert_ne!(s.json(&["server", "status"])["pid"], pid, "a new core runs");
    assert!(dir.join("h2").is_symlink(), "the link stays a link");

    wait_until("kept agent resumes via session/load", || {
        let g = get(&s, &a);
        g["state"] == "live" && g["pid"].as_i64().is_some_and(|p| p != old_pid)
    });
    let st = state_json(&s, &a);
    assert_eq!(st["session_id"], old_sid);
    assert_eq!(st["loaded"], true);

    let probe = "printf 'ENV:%s|%s|%s\\n' \"${NO_COLOR-unset}\" \"$H2_SESSION_VALUE\" \"${H2_UPDATER_ONLY-unset}\"";
    let out = s.h2(&["exec", "sh", "-c", probe]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("ENV:unset|original|unset"), "{out:?}");

    let stream = herdr2::platform::connect(&s.socket()).unwrap();
    let agent = herdr2::client::call(
        &stream,
        "agent.run",
        json!({
            "argv": ["python3", fake_acp_path()],
            "prompt": "ENV:NO_COLOR\nENV:H2_SESSION_VALUE\nENV:H2_UPDATER_ONLY",
            "wait": true,
            "env": {"NO_COLOR": "1", "H2_SESSION_VALUE": "override"},
        }),
    )
    .unwrap();
    let output = agent["output"].as_str().unwrap();
    assert!(
        output.contains("ENV:NO_COLOR=1")
            && output.contains("ENV:H2_SESSION_VALUE=override")
            && output.contains("ENV:H2_UPDATER_ONLY=<unset>"),
        "{agent}"
    );

    let older = s.root.join("older");
    std::fs::write(&older, "#!/bin/sh\necho 'h2 0.0.1'\n").unwrap();
    std::fs::set_permissions(&older, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let (_, e) = s.err(&["--json", "update", "--from", older.to_str().unwrap()]);
    assert_eq!(e["error"]["kind"], "refused", "never older by accident: {e}");
    let junk = s.root.join("junk");
    std::fs::write(&junk, "not a program").unwrap();
    assert_eq!(s.err(&["--json", "update", "--from", junk.to_str().unwrap()]).1["error"]["kind"], "refused");
    assert!(
        std::fs::read_dir(&dir).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains(".new")),
        "no leftovers"
    );
}

#[test]
fn update_automatically_syncs_ssh_remotes_and_marks_unreachable_needs_update() {
    let remote = Session::new();
    let mut local = Session::new();
    let dir = local.root.join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(BIN, dir.join("herdr2")).unwrap();
    std::os::unix::fs::symlink("herdr2", dir.join("h2")).unwrap();
    local.bin = dir.join("h2");

    local.env.push(fake_ssh(&local, &[("linuxbox", &remote)]));
    let added = local.json(&["machine", "add", "dev@linuxbox"]);
    assert_eq!(added["name"], "linuxbox");

    // Add an older unreachable remote in machines.json alongside the connected linuxbox.
    let machines_path = local.root.join("state/herdr2/test/machines.json");
    let mut saved: Vec<Value> = serde_json::from_slice(&std::fs::read(&machines_path).unwrap()).unwrap();
    saved.push(json!({
        "name": "offlinebox",
        "target": "dev@offlinebox",
        "via": false,
        "temporary": false,
        "session": null,
        "id": "offline-core-id",
        "host": "offlinebox",
        "version": "0.0.1"
    }));
    std::fs::write(&machines_path, serde_json::to_vec_pretty(&saved).unwrap()).unwrap();
    local.h2(&["server", "restart"]);
    wait_until("linuxbox reconnects", || {
        local.json(&["machine", "ls"])["machines"]
            .as_array()
            .is_some_and(|arr| arr.iter().any(|m| m["name"] == "linuxbox" && m["state"] == "connected"))
    });

    // When H2_UPDATE_NO_MACHINES=1 is set, h2 update skips remote sync cleanly.
    let skipped_out = local
        .cmd()
        .env("H2_UPDATE_NO_MACHINES", "1")
        .args(["--json", "update", "--from", BIN])
        .output()
        .unwrap();
    assert!(skipped_out.status.success(), "{}", String::from_utf8_lossy(&skipped_out.stderr));
    let skipped_json: Value = serde_json::from_slice(&skipped_out.stdout).unwrap();
    assert!(skipped_json.get("machines").is_none(), "expected no remote sync when skipped: {skipped_json}");

    // Without --force: already-current connected linuxbox is kept as-is while outdated unreachable offlinebox is marked needs_update.
    let out = local.cmd().args(["--json", "update", "--from", BIN]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let synced: Value = serde_json::from_slice(&out.stdout).unwrap();
    let machines = synced["machines"].as_array().expect("machines sync report");
    let ok_box = machines.iter().find(|m| m["name"] == "linuxbox").expect("linuxbox entry");
    assert_eq!(ok_box["ok"], true, "{synced}");
    assert_eq!(ok_box["updated"], false, "{synced}");
    assert!(matches!(ok_box["state"].as_str(), Some("connected" | "connecting")), "{synced}");
    let bad_box = machines.iter().find(|m| m["name"] == "offlinebox").expect("offlinebox entry");
    assert_eq!(bad_box["ok"], false, "{synced}");
    assert_eq!(bad_box["state"], "needs_update", "{synced}");

    // With --force: reachable linuxbox is pushed the new binary over SSH while offlinebox still reports needs_update without aborting.
    let forced_out = local.cmd().args(["--json", "update", "--from", BIN, "--force"]).output().unwrap();
    assert!(forced_out.status.success(), "{}", String::from_utf8_lossy(&forced_out.stderr));
    let forced: Value = serde_json::from_slice(&forced_out.stdout).unwrap();
    let f_machines = forced["machines"].as_array().expect("forced machines sync report");
    let f_ok = f_machines.iter().find(|m| m["name"] == "linuxbox").expect("linuxbox entry");
    assert_eq!(f_ok["ok"], true, "{forced}");
    assert_eq!(f_ok["updated"], true, "{forced}");
    assert_eq!(f_ok["state"], "connected", "{forced}");
    let f_bad = f_machines.iter().find(|m| m["name"] == "offlinebox").expect("offlinebox entry");
    assert_eq!(f_bad["ok"], false, "{forced}");
    assert_eq!(f_bad["state"], "needs_update", "{forced}");
    assert!(remote.root.join(".local/bin/herdr2").is_file(), "remote binary was synced");
}

#[test]
fn update_automatically_syncs_already_installed_local_and_remote_skills() {
    let remote = Session::new();
    let mut local = Session::new();
    let dir = local.root.join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(BIN, dir.join("herdr2")).unwrap();
    std::os::unix::fs::symlink("herdr2", dir.join("h2")).unwrap();
    local.bin = dir.join("h2");

    // Local: install subset into .claude and custom target, leave .cursor detected-but-uninstalled.
    std::fs::create_dir_all(local.root.join(".claude")).unwrap();
    std::fs::create_dir_all(local.root.join(".cursor")).unwrap();
    local.json(&["skill", "install", "--agent", "claude", "--skills", "herdr2,drove"]);
    let custom_dir = local.root.join("custom-skills");
    local.json(&["skill", "install", "--target", custom_dir.to_str().unwrap(), "--skills", "drove-add"]);
    std::fs::write(local.root.join(".claude/skills/herdr2/SKILL.md"), "old local herdr2").unwrap();
    std::fs::write(custom_dir.join("drove-add/SKILL.md"), "old custom drove-add").unwrap();

    // Remote: already has stale .codex skill and detected-but-uninstalled .cursor dir.
    std::fs::create_dir_all(remote.root.join(".codex/skills/herdr2")).unwrap();
    std::fs::write(remote.root.join(".codex/skills/herdr2/SKILL.md"), "old remote herdr2").unwrap();
    std::fs::create_dir_all(remote.root.join(".cursor")).unwrap();

    local.env.push(fake_ssh(&local, &[("linuxbox", &remote)]));
    local.json(&["machine", "add", "dev@linuxbox"]);
    wait_until("linuxbox connected", || {
        local.json(&["machine", "ls"])["machines"]
            .as_array()
            .is_some_and(|arr| arr.iter().any(|m| m["name"] == "linuxbox" && m["state"] == "connected"))
    });

    // Running `h2 update --from ... --force` updates local binary, syncs already-installed local skills,
    // and syncs remote binary + already-installed remote skills.
    let out = local.cmd().args(["--json", "update", "--from", BIN, "--force"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["skills"]["ok"], true, "{reply}");
    assert_eq!(reply["skills"]["updatedCount"], 2, "{reply}");

    // Local installed skills were refreshed; uninstalled skills/agents stayed untouched.
    assert!(std::fs::read_to_string(local.root.join(".claude/skills/herdr2/SKILL.md")).unwrap().contains("name: herdr2"));
    assert!(std::fs::read_to_string(custom_dir.join("drove-add/SKILL.md")).unwrap().contains("name: drove-add"));
    assert!(!local.root.join(".claude/skills/drove-setup/SKILL.md").exists());
    assert!(!local.root.join(".cursor/skills").exists());

    // Remote installed skill was refreshed; uninstalled remote skills/agents stayed untouched.
    assert!(std::fs::read_to_string(remote.root.join(".codex/skills/herdr2/SKILL.md")).unwrap().contains("name: herdr2"));
    assert!(!remote.root.join(".codex/skills/drove/SKILL.md").exists());
    assert!(!remote.root.join(".cursor/skills").exists());
}
