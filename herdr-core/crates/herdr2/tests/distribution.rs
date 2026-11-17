//! Releases: the install script, `h2 update` from the release list, and h2
//! on a machine of another OS or CPU, all against a fake releases site
//! (python3's http.server) built from this very binary.

#![cfg(unix)] // Windows: tests/windows.rs

mod common;

use common::*;
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Child, Command};

const ROOT: &str = env!("CARGO_MANIFEST_DIR");
/// This build's version: what scripts/manifest writes, what h2 says it is.
const THIS: &str = env!("CARGO_PKG_VERSION");
/// A release newer than this build (it is this build inside, dressed up).
const NEWER: &str = "9.9.9";

#[cfg(target_os = "macos")]
fn live_executable(pid: i64) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let mut buf = [0u8; 4096];
    let len = unsafe { libc::proc_pidpath(pid as i32, buf.as_mut_ptr().cast(), buf.len() as u32) };
    assert!(len > 0);
    let len = buf.iter().position(|b| *b == 0).unwrap();
    PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len]))
}

fn state_json(s: &Session, a: &str) -> Value {
    s.json(&["send", "--wait", a, "STATE"]);
    let last = s.json(&["read", "--last", a])["text"].as_str().unwrap().to_owned();
    let line = last.lines().find(|l| l.starts_with('{')).unwrap_or_else(|| panic!("no STATE json in {last:?}"));
    serde_json::from_str(line).unwrap()
}

fn run_in_pty(s: &Session, args: &[&str], interactions: &[(&str, &str)]) -> (i32, String) {
    let py = r#"
import json, os, pty, select, subprocess, sys, time
steps = json.loads(os.environ["H2_PTY_STEPS"])
master, slave = pty.openpty()
proc = subprocess.Popen(sys.argv[1:], stdin=slave, stdout=slave, stderr=slave, close_fds=True)
os.close(slave)
buf = bytearray()
pos = 0
idx = 0
deadline = time.monotonic() + 20.0
while time.monotonic() < deadline:
    r, _, _ = select.select([master], [], [], 0.1)
    if r:
        try:
            chunk = os.read(master, 4096)
        except OSError:
            break
        if not chunk:
            break
        buf.extend(chunk)
        text = buf.decode("utf-8", "replace")
        while idx < len(steps):
            expect, reply = steps[idx]
            found = text.find(expect, pos)
            if found == -1:
                break
            pos = found + len(expect)
            os.write(master, (reply + "\n").encode())
            idx += 1
    elif proc.poll() is not None:
        break
os.close(master)
rc = proc.wait(timeout=5)
print(json.dumps({"code": rc, "output": buf.decode("utf-8", "replace")}))
"#;
    let out = s
        .program("python3")
        .env("H2_PTY_STEPS", serde_json::to_string(interactions).unwrap())
        .arg("-c")
        .arg(py)
        .arg(BIN)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "pty helper failed: {}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    (v["code"].as_i64().unwrap() as i32, v["output"].as_str().unwrap().to_string())
}

/// A releases site with this build as `version`, for `targets`.
struct Releases {
    root: PathBuf,
    url: String,
    server: Child,
}

impl Releases {
    fn new(s: &Session, version: &str, targets: &[&str]) -> Releases {
        let root = s.root.join("releases");
        std::fs::create_dir_all(&root).unwrap();
        let ready = root.join("server-port");
        let server = Command::new("python3")
            .arg(format!("{ROOT}/tests/releases_server.py"))
            .arg(&ready)
            .current_dir(&root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap();
        let mut r = Releases { root, url: String::new(), server };
        let mut port = None;
        wait_until("the releases site", || {
            assert!(r.server.try_wait().unwrap().is_none(), "the releases site exited before becoming ready");
            port = std::fs::read_to_string(&ready).ok().and_then(|text| text.parse::<u16>().ok());
            port.is_some()
        });
        r.url = format!("http://127.0.0.1:{}", port.unwrap());
        let dir = r.root.join("core").join(version);
        std::fs::create_dir_all(&dir).unwrap();
        let bin_dir = std::path::Path::new(BIN).parent().unwrap();
        for t in targets {
            let tar = dir.join(format!("herdr2-{t}.tar.gz"));
            if cfg!(target_os = "macos") && t.starts_with("macos-") {
                let app = s.root.join("Herdr2.app");
                let standalone = s.root.join("herdr2");
                std::fs::copy(BIN, &standalone).unwrap();
                for (script, args) in [
                    ("mac-bundle", vec![PathBuf::from(BIN), app.clone()]),
                    ("mac-sign", vec![app.clone()]),
                    ("mac-sign", vec![standalone]),
                    ("package", vec![app, tar]),
                ] {
                    let out = Command::new("bash")
                        .arg(format!("{ROOT}/../../scripts/{script}"))
                        .args(args)
                        .env_remove("APPLE_CERTIFICATE_P12")
                        .env_remove("APPLE_CERTIFICATE_PASSWORD")
                        .output()
                        .unwrap();
                    assert!(out.status.success(), "{script}: {out:?}");
                }
                continue;
            }
            assert!(
                Command::new("tar")
                    .arg("-czf")
                    .arg(&tar)
                    .arg("-C")
                    .arg(bin_dir)
                    .arg("herdr2")
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let out = Command::new(format!("{ROOT}/../../scripts/manifest"))
            .arg(&dir)
            .env("RELEASES_URL", &r.url)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        // Dressed up as `version`: its field and its links.
        let manifest = String::from_utf8(out.stdout)
            .unwrap()
            .replace(&format!("\"version\": \"{THIS}\""), &format!("\"version\": \"{version}\""))
            .replace(&format!("/core/{THIS}/"), &format!("/core/{version}/"));
        std::fs::write(dir.join("manifest.json"), &manifest).unwrap();
        std::fs::write(r.root.join("core/latest.json"), &manifest).unwrap();
        r
    }
}

impl Drop for Releases {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
    }
}

fn here() -> String {
    let arch = if std::env::consts::ARCH == "aarch64" { "aarch64" } else { "x86_64" };
    format!("{}-{arch}", std::env::consts::OS)
}

#[test]
fn the_install_script_installs_and_then_updates_without_losing_kept_agents() {
    let mut s = Session::new();
    let r = Releases::new(&s, THIS, &[&here()]);
    let bin = s.root.join("bin");
    let install = |s: &Session| {
        // A PATH without the h2 this machine already has: else no `h2` link is added.
        let mut cmd = s.program("sh");
        cmd.arg(format!("{ROOT}/../../distribution/install.sh")).env("PATH", "/usr/bin:/bin");
        let out = cmd.env("HERDR2_RELEASES_URL", &r.url).env("H2_INSTALL_DIR", &bin).output().unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    assert!(install(&s).contains("installed"));
    assert!(bin.join("herdr2").is_file() && bin.join("h2").is_symlink());
    if cfg!(target_os = "macos") {
        let bundle = s.root.join(".local/share/herdr2/active/Herdr2.app");
        assert!(bundle.join("Contents/Info.plist").is_file());
        assert_eq!(
            bin.join("herdr2").canonicalize().unwrap(),
            bundle.join("Contents/MacOS/herdr2").canonicalize().unwrap()
        );
    }

    s.bin = bin.join("h2");
    if cfg!(target_os = "macos") {
        let (_, err) = s.err(&["update", "--from", BIN]);
        assert_eq!(err["error"]["kind"], "refused");
        assert!(err["error"]["hint"].as_str().unwrap().contains("Herdr2.app"));
        let source = s.root.join("Herdr2.app");
        assert_eq!(s.json(&["update", "--from", source.to_str().unwrap()])["now"], format!("h2 {THIS}"));
    }
    let a = run_fake(&s, &["--keep"]);
    let old_pid = get(&s, &a)["pid"].as_i64().unwrap();
    let old_sid = state_json(&s, &a)["session_id"].as_str().unwrap().to_owned();
    let status = s.json(&["server", "status"]);
    let pid = status["pid"].clone();
    assert!(install(&s).contains("updated"), "again: an update");
    assert_ne!(s.json(&["server", "status"])["pid"], pid, "the planned restart");
    wait_until("kept agent resumes via session/load", || {
        let g = get(&s, &a);
        g["state"] == "live" && g["pid"].as_i64().is_some_and(|p| p != old_pid)
    });
    let st = state_json(&s, &a);
    assert_eq!(st["session_id"], old_sid);
    assert_eq!(st["loaded"], true);
    drop(r);
}

#[test]
fn a_macos_system_install_uses_the_standalone_release() {
    use std::os::unix::fs::PermissionsExt;
    let s = Session::new();
    let tools = s.root.join("sudo-tools");
    std::fs::create_dir(&tools).unwrap();
    let r = Releases::new(&s, THIS, &["macos-x86_64"]);
    for (name, script) in [("uname", "if [ \"$1\" = -s ]; then echo Darwin; else echo x86_64; fi"), ("id", "echo 0")] {
        let path = tools.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let out = s
        .program("sh")
        .arg(format!("{ROOT}/../../distribution/install.sh"))
        .env("PATH", format!("{}:/usr/bin:/bin", tools.display()))
        .env("HERDR2_RELEASES_URL", &r.url)
        .env("H2_INSTALL_DIR", s.root.join("system-bin"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(s.root.join("system-bin/herdr2").is_file());
    assert!(!s.root.join("system-bin/herdr2").is_symlink());
    assert!(!s.root.join(".local/share/herdr2/current").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn bundle_completion_skips_releases_without_a_bundle() {
    let s = Session::new();
    let r = Releases::new(&s, THIS, &[&here()]);
    let bin = s.root.join("not-installed");
    let out = s
        .program("sh")
        .arg(format!("{ROOT}/../../distribution/install.sh"))
        .env("HERDR2_RELEASES_URL", &r.url)
        .env("H2_INSTALL_DIR", &bin)
        .env("H2_BUNDLE_ONLY", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(!bin.exists(), "no binary swap or restart for a pre-bundle release");
}

#[cfg(target_os = "linux")]
#[test]
fn the_install_script_installs_when_the_temp_dir_cannot_execute() {
    let s = Session::new();
    let r = Releases::new(&s, THIS, &[&here()]);
    let bin = s.root.join("bin");
    let tmp = s.root.join("noexec-tmp");
    std::fs::create_dir(&tmp).unwrap();
    let probe = Command::new("unshare").args(["-rm", "--map-root-user", "true"]).output().unwrap();
    if !probe.status.success() {
        return;
    }
    let script = format!(
        "set -e; mount --make-rprivate /; mount -t tmpfs -o size=256M,noexec tmpfs {}; \
         export TAR_OPTIONS=--no-same-owner; \
         export H2_INSTALL_DIR={} TMPDIR={} HERDR2_RELEASES_URL={}; \
         /usr/bin/sh {}/../../distribution/install.sh; \
         /usr/bin/sh {}/../../distribution/install.sh",
        tmp.display(),
        bin.display(),
        tmp.display(),
        r.url,
        ROOT,
        ROOT
    );
    let out = s.program("unshare").args(["-rm", "--map-root-user", "sh", "-c", &script]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("does not run"), "the noexec mount was read as a wrong machine: {stdout}{stderr}");
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("installed"));
    assert!(stdout.contains("updated"));
    assert!(bin.join("herdr2").is_file());
    assert!(!bin.join("herdr2.staged").exists(), "the stage is cleaned up");
}

#[cfg(target_os = "macos")]
#[test]
fn machine_installs_keep_the_complete_bundle_and_both_aliases() {
    for cross_arch in [false, true] {
        use std::os::unix::fs::PermissionsExt;
        let mut remote = Session::new();
        remote.bin = remote.root.join(".local/bin/herdr2");
        let mut local = Session::new();
        let target = if cross_arch { "macos-x86_64" } else { &here() };
        let r = Releases::new(&local, THIS, &[target]);
        let source = local.root.join("Herdr2.app/Contents/MacOS/herdr2");
        let installed = local.root.join(".local/bin/herdr2");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        let out = local
            .program(&source)
            .arg("--h2-install-bundle")
            .arg(&source)
            .arg(&installed)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(out.status.success(), "local bundle install: {out:?}");
        local.bin = installed.with_file_name("h2");
        assert!(local.bin.is_symlink());
        local.env.push(("HERDR2_RELEASES_URL".into(), r.url.clone()));
        local.env.push(fake_ssh(&local, &[("bare", &remote)]));
        if cross_arch {
            let fake = remote.root.join("fakebin");
            std::fs::create_dir(&fake).unwrap();
            std::fs::write(fake.join("uname"), "#!/bin/sh\necho Darwin x86_64\n").unwrap();
            std::fs::set_permissions(fake.join("uname"), std::fs::Permissions::from_mode(0o755)).unwrap();
            if std::env::consts::ARCH == "x86_64" {
                continue;
            }
        }
        let added = local.json(&["machine", "add", "bare", "--install"]);
        assert_eq!(added["name"], "bare");
        let current = remote.root.join(".local/share/herdr2/current");
        assert!(current.join("Contents/Info.plist").is_file());
        let h2 = remote.bin.with_file_name("h2");
        assert_eq!(h2.canonicalize().unwrap(), remote.bin.canonicalize().unwrap());
        local.json(&["machine", "update", "bare"]);
        assert_eq!(h2.canonicalize().unwrap(), remote.bin.canonicalize().unwrap());
        if !cross_arch {
            let bundled = remote.bin.canonicalize().unwrap();
            let before = std::fs::read(&bundled).unwrap();
            local.bin = local.root.join("herdr2");
            let (_, error) = local.err(&["--json", "machine", "update", "bare"]);
            assert_eq!(error["error"]["kind"], "install_failed");
            assert!(error["error"]["message"].as_str().unwrap().contains("complete bundle"), "{error}");
            assert_eq!(remote.bin.canonicalize().unwrap(), bundled);
            assert_eq!(h2.canonicalize().unwrap(), bundled);
            assert_eq!(std::fs::read(&bundled).unwrap(), before);
        }
        local.json(&["machine", "rm", "bare"]);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_root_remote_gets_the_independently_signed_standalone_release() {
    use std::os::unix::fs::PermissionsExt;
    let mut remote = Session::new();
    remote.bin = remote.root.join(".local/bin/herdr2");
    let tools = remote.root.join("fakebin");
    std::fs::create_dir(&tools).unwrap();
    std::fs::write(tools.join("id"), "#!/bin/sh\necho 0\n").unwrap();
    std::fs::set_permissions(tools.join("id"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut local = Session::new();
    let r = Releases::new(&local, THIS, &[&here()]);
    let source = local.root.join("Herdr2.app/Contents/MacOS/herdr2");
    local.bin = local.root.join(".local/bin/h2");
    std::fs::create_dir_all(local.bin.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&source, &local.bin).unwrap();
    local.env.push(("HERDR2_RELEASES_URL".into(), r.url.clone()));
    local.env.push(fake_ssh(&local, &[("root", &remote)]));
    local.json(&["machine", "add", "root", "--install"]);
    for update in [false, true] {
        if update {
            local.json(&["machine", "update", "root"]);
        }
        assert!(!remote.bin.is_symlink());
        assert!(!remote.root.join(".local/share/herdr2/current").exists());
        assert_eq!(remote.bin.with_file_name("h2").canonicalize().unwrap(), remote.bin.canonicalize().unwrap());
        let out = Command::new("/usr/bin/codesign").args(["--verify", "--strict"]).arg(&remote.bin).output().unwrap();
        assert!(out.status.success(), "standalone signature: {out:?}");
    }
    local.json(&["machine", "rm", "root"]);
}

/// The URL the install script downloads h2 from, for a list giving `url`,
/// with curl stubbed to record it (the download itself fails).
fn install_script_downloads(s: &Session, url: &str, releases: &str, installed: bool) -> String {
    let dir = s.root.join(format!(
        "marker-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let (stub, bin) = (dir.join("stub"), dir.join("bin"));
    std::fs::create_dir_all(&stub).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    if installed {
        std::fs::write(bin.join("herdr2"), "#!/bin/sh\n").unwrap();
        Command::new("chmod").arg("755").arg(bin.join("herdr2")).status().unwrap();
    }
    let list = dir.join("latest.json");
    std::fs::write(
        &list,
        format!(r#"{{"version":"1.0.0","generation":1,"targets":{{"{}":{{"url":"{url}","sha256":"x"}}}}}}"#, here()),
    )
    .unwrap();
    let log = dir.join("urls");
    std::fs::write(
        stub.join("curl"),
        format!(
            "#!/bin/sh\nout=; url=; prev=\nfor a; do\n  if [ \"$prev\" = -o ]; then out=$a; else case $a in -*) ;; *) url=$a ;; esac; fi\n  prev=$a\ndone\necho \"$url\" >> {log}\ncase $url in */latest.json) cp {list} \"$out\"; exit 0 ;; esac\nexit 22\n",
            log = log.display(),
            list = list.display()
        ),
    )
    .unwrap();
    Command::new("chmod").arg("755").arg(stub.join("curl")).status().unwrap();
    let out = Command::new("sh")
        .arg(format!("{ROOT}/../../distribution/install.sh"))
        .env("PATH", format!("{}:/usr/bin:/bin", stub.display()))
        .env("HERDR2_RELEASES_URL", releases)
        .env_remove("H2_VERSION")
        .env("H2_INSTALL_DIR", &bin)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("download failed"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read_to_string(log).unwrap().lines().last().unwrap().to_owned()
}

#[test]
fn the_install_script_marks_only_plain_release_downloads() {
    let s = Session::new();
    let r = "https://releases.example";
    let tarball = format!("{r}/core/1.0.0/herdr2-{}.tar.gz", here());
    assert_eq!(install_script_downloads(&s, &tarball, r, false), format!("{tarball}?src=install"));
    assert_eq!(install_script_downloads(&s, &tarball, r, true), format!("{tarball}?src=update"));
    for url in [
        format!("{tarball}?sig=a%2Fb&exp=1"),
        format!("{tarball}?src=install"),
        format!("{tarball}#x"),
        "https://mirror.example/core/1.0.0/h2.tar.gz".to_owned(),
        "https://releases.example.evil/core/1.0.0/h2.tar.gz".to_owned(),
    ] {
        assert_eq!(install_script_downloads(&s, &url, r, false), url);
    }
}

#[test]
fn h2_update_downloads_checks_and_swaps() {
    let mut s = Session::new();
    let r = Releases::new(&s, NEWER, &[&here()]);
    let dir = s.root.join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(BIN, dir.join("herdr2")).unwrap();
    s.bin = dir.join("herdr2");
    s.env.push(("HERDR2_RELEASES_URL".into(), r.url.clone()));
    let pid = s.json(&["server", "status"])["pid"].clone();

    let up = s.json(&["update"]);
    assert_eq!((up["restarted"].as_bool(), up["release"].as_str()), (Some(true), Some(NEWER)), "{up}");
    assert_ne!(s.json(&["server", "status"])["pid"], pid);

    let tar = r.root.join(format!("core/{NEWER}/herdr2-{}.tar.gz", here()));
    std::fs::write(&tar, b"not what the list says").unwrap();
    let (_, e) = s.err(&["--json", "update"]);
    assert_eq!(e["error"]["kind"], "refused", "a damaged download changes nothing: {e}");
    drop(r);
}

#[test]
fn h2_update_to_a_version_takes_at_least_that_one() {
    let mut s = Session::new();
    let r = Releases::new(&s, NEWER, &[&here()]);
    let dir = s.root.join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(BIN, dir.join("herdr2")).unwrap();
    s.bin = dir.join("herdr2");
    s.env.push(("HERDR2_RELEASES_URL".into(), r.url.clone()));
    s.json(&["server", "status"]);

    let up = s.json(&["update", "--version", NEWER]);
    assert_eq!((up["restarted"].as_bool(), up["release"].as_str()), (Some(true), Some(NEWER)), "{up}");
    let same = s.json(&["update", "--version", "0.0.1"]);
    assert_eq!((same["updated"].as_bool(), same["release"].as_str()), (Some(false), Some("0.0.1")), "{same}");
    let (_, e) = s.err(&["--json", "update", "--version", "9.9.10"]);
    assert_eq!(e["error"]["kind"], "unavailable", "no such release: {e}");
    let (_, e) = s.err(&["--json", "update", "--version", "../x"]);
    assert_eq!(e["error"]["kind"], "usage", "{e}");
    drop(r);
}

#[test]
fn h2_update_never_takes_a_release_of_another_generation() {
    let mut s = Session::new();
    let r = Releases::new(&s, NEWER, &[&here()]);
    let latest = r.root.join("core/latest.json");
    let breaking = std::fs::read_to_string(&latest).unwrap().replace("\"generation\": 1", "\"generation\": 2");
    std::fs::write(&latest, breaking).unwrap();
    let dir = s.root.join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(BIN, dir.join("herdr2")).unwrap();
    s.bin = dir.join("herdr2");
    s.env.push(("HERDR2_RELEASES_URL".into(), r.url.clone()));
    let (_, e) = s.err(&["--json", "update"]);
    assert_eq!(e["error"]["kind"], "refused", "apps built for this generation must keep working: {e}");
    assert!(e["error"]["message"].as_str().unwrap().contains("generation"), "{e}");
    drop(r);
}

#[test]
fn a_machine_of_another_cpu_gets_the_released_build_for_it() {
    let mut odd = Session::new();
    odd.bin = odd.root.join("bin/herdr2"); // no h2 there yet
    std::fs::create_dir_all(odd.root.join("fakebin")).unwrap();
    let other = if std::env::consts::ARCH == "aarch64" { "x86_64" } else { "aarch64" };
    let os = if std::env::consts::OS == "macos" { "Darwin" } else { "Linux" };
    let uname = odd.root.join("fakebin/uname");
    std::fs::write(&uname, format!("#!/bin/sh\necho '{os} {other}'\n")).unwrap();
    std::fs::set_permissions(&uname, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let mut s = Session::new();
    let target = format!("{}-{other}", std::env::consts::OS);
    let r = Releases::new(&s, THIS, &[&target]);
    s.env.push(fake_ssh(&s, &[("odd", &odd)]));
    s.env.push(("HERDR2_RELEASES_URL".into(), r.url.clone()));
    let (code, screen) = run_in_pty(&s, &["machine", "add", "odd"], &[("[Y/n]", "y")]);
    assert_eq!(code, 0, "{screen}");
    assert!(odd.root.join(".local/bin/herdr2").is_file());
    odd.bin = odd.root.join(".local/bin/herdr2");
    drop(r);
}
