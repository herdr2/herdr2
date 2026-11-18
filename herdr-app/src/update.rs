//! The app's own updates. An installed app (Linux: `~/.local/share/herdr-app`,
//! macOS: `Herdr.app`) checks `releases.herdr.dev/app/latest.json` a little
//! after it opens and every hour, downloads a newer build for this
//! machine, checks its sha256 and unpacks it next to itself (`<install>.new`).
//! The frame then shows "Restart"; the swap happens on restart or the next
//! quit, or before opening the next window after a crash. The first start
//! after a swap runs `hc update` for the core.
//!
//! A build from source never updates: only `scripts/package` (the release
//! builds) sets `HERDR_APP_RELEASE`. `scripts/install-macos` makes a
//! Herdr.app too, so the install's shape alone can't tell.
//!
//! The core is checked on the same clock (`core/latest.json`): a newer core
//! of the same generation is offered (also applied after an app update), unless
//! a package manager owns it. Within a generation a newer core only adds.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

const EVERY: Duration = Duration::from_secs(60 * 60);
/// After a failed check or download (offline, a damaged file).
const RETRY: Duration = Duration::from_secs(30 * 60);

/// Where this app is installed: the folder (Linux) or bundle (macOS) an
/// update replaces. Read once: after a swap this process's own file is gone.
fn root() -> Option<PathBuf> {
    static ROOT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    ROOT.get_or_init(find_root).clone()
}

fn find_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let dir = exe.parent()?;
    if cfg!(target_os = "macos") {
        // Herdr2.app/Contents/MacOS/Herdr2
        let bundle = dir.parent()?.parent()?;
        (bundle.extension()? == "app").then(|| bundle.to_path_buf())
    } else {
        // The release folder holds the binary and its icon; cargo's target/ doesn't.
        dir.join("herdr2.svg").is_file().then(|| dir.to_path_buf())
    }
}

/// The folder or bundle name inside a release tarball.
const PACKED: &str = if cfg!(target_os = "macos") { "Herdr2.app" } else { "herdr2" };

fn beside(root: &Path, suffix: &str) -> PathBuf {
    let mut name = root.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    // macOS: Spotlight skips a folder ending in `.noindex`, so a staged
    // `Herdr.app.new` never shows up in search under that name.
    if cfg!(target_os = "macos") {
        name.push(".noindex");
    }
    root.with_file_name(name)
}

fn target() -> String {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

fn releases() -> String {
    std::env::var("HERDR2_RELEASES_URL").unwrap_or_default()
}

/// Self-updates are disabled: always reports None.
static READY: Mutex<Option<String>> = Mutex::new(None);

pub fn ready() -> Option<String> {
    None
}

/// The core the app runs, from its `server.status`, and a newer release waiting.
#[derive(Clone, Default, serde::Serialize)]
pub struct CoreInfo {
    pub version: Option<String>,
    /// A package manager that owns it (homebrew, nix, mise): never updated by the app.
    pub managed: Option<String>,
    pub ready: Option<String>,
    pub updating: bool,
    pub error: Option<String>,
}

static CORE: Mutex<CoreInfo> = Mutex::new(CoreInfo { version: None, managed: None, ready: None, updating: false, error: None });
/// When the last check succeeded (unix seconds); a failed check clears it.
static CHECKED: Mutex<Option<u64>> = Mutex::new(None);
/// "Check now" wakes the checker early.
static WAKE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
/// The checker runs (a release, or pointed at a test server), and is looking right now.
static RUNNING: AtomicBool = AtomicBool::new(false);
static CHECKING: AtomicBool = AtomicBool::new(false);

pub fn core() -> CoreInfo {
    CORE.lock().unwrap().clone()
}

/// What the connected core says about itself (on connect and every reconnect).
pub fn set_core(version: Option<String>, managed: Option<String>) {
    let mut c = CORE.lock().unwrap();
    if let (Some(have), Some(ready)) = (&version, &c.ready)
        && !newer(ready, have)
    {
        c.ready = None;
    }
    c.version = version;
    c.managed = managed;
    if c.managed.is_some() {
        c.error = None;
    }
}

fn strip_core_prefix(v: &str) -> &str {
    let v = v.trim();
    v.strip_prefix("h2 ").or_else(|| v.strip_prefix("herdr2 ")).or_else(|| v.strip_prefix("hc ")).unwrap_or(v)
}

fn checks_app() -> Option<PathBuf> {
    None
}

/// Self-updates from upstream Herdr are disabled.
fn checks_core() -> bool {
    false
}

/// For the settings screen / mods: reports current versions without update prompts.
pub fn status() -> serde_json::Value {
    serde_json::json!({
        "app": { "version": env!("CARGO_PKG_VERSION"), "ready": serde_json::Value::Null, "updates": false },
        "notes": serde_json::Value::Array(Vec::new()),
        "core": core(),
        "checked": serde_json::Value::Null,
        "checks": false,
        "checking": false,
    })
}

pub fn check_now() {}

fn sleep(d: Duration) {
    let woken = WAKE.0.lock().unwrap();
    let (mut woken, _) = WAKE.1.wait_timeout_while(woken, d, |w| !*w).unwrap();
    *woken = false;
}

/// Self-updates from upstream Herdr are disabled.
pub fn start() {}

fn checked_at(succeeded: bool, now: u64) -> Option<u64> {
    succeeded.then_some(now)
}

/// Looks for a newer core of this app's generation.
fn check_core() -> Result<(), String> {
    let have = CORE.lock().unwrap().version.clone();
    let Some(have) = have else { return Ok(()) };
    let latest = latest_core()?;
    let same = latest.generation == Some(crate::hc::GENERATION);
    CORE.lock().unwrap().ready = (same && newer(&latest.version, &have)).then_some(latest.version);
    Ok(())
}

#[derive(serde::Deserialize)]
struct CoreRelease {
    version: String,
    generation: Option<u64>,
}

fn latest_core() -> Result<CoreRelease, String> {
    let text = curl_text(&format!("{}/core/latest.json", releases()))?;
    serde_json::from_str(&text).map_err(|e| format!("core/latest.json: {e}"))
}

fn after_update_target<'a>(have: &str, latest: Option<&'a CoreRelease>, generation: u64, core_min: &'a str) -> Option<&'a str> {
    if newer("0.1.5", have) {
        return None;
    }
    Some(match latest {
        Some(latest) if latest.generation == Some(generation) && newer(&latest.version, have) => &latest.version,
        _ => core_min,
    })
}

/// Core auto-updates are disabled.
pub fn update_core() {}

/// Downloads the latest release if it's newer. Ok(Some): a version is ready.
fn check(root: &Path) -> Result<Option<String>, String> {
    check_from(root, &releases())
}

fn check_from(root: &Path, releases: &str) -> Result<Option<String>, String> {
    let path = "app/latest.json";
    let text = curl_text(&format!("{releases}/{path}"))?;
    let manifest: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    let version = manifest["version"].as_str().ok_or(format!("{path}: no version"))?;
    if !newer(version, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    // Notes are optional: a missing feed must never hold up the update.
    if !crate::update_notes::has(version)
        && let Ok(text) = curl_text(&format!("{releases}/app/{version}/changelog.md"))
    {
        crate::update_notes::cache(&text);
    }
    let staged = beside(root, ".new");
    if staged_version(root).as_deref() == Some(version) {
        return Ok(Some(version.to_owned()));
    }
    let entry = &manifest["targets"][target()];
    let (Some(url), Some(sha)) = (entry["url"].as_str(), entry["sha256"].as_str()) else {
        return Ok(None);
    };
    tracing::info!("downloading Herdr {version}");
    let work = beside(root, ".download");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let result = unpack(url, sha, &work).and_then(|unpacked| {
        if version_of(&unpacked).as_deref() != Some(version) {
            return Err(format!("the download doesn't run as {version}"));
        }
        let _ = std::fs::remove_file(beside(root, ".new.json"));
        let _ = std::fs::remove_dir_all(&staged);
        std::fs::rename(&unpacked, &staged).map_err(|e| format!("{}: {e}", staged.display()))?;
        // Outside the bundle: writing a receipt must not change its signature.
        std::fs::write(beside(root, ".new.json"), serde_json::json!({ "version": version }).to_string()).map_err(|e| e.to_string())
    });
    let _ = std::fs::remove_dir_all(&work);
    result.map(|()| Some(version.to_owned()))
}

/// Downloads, checks and unpacks a release into `work`; returns the app inside.
fn unpack(url: &str, sha: &str, work: &Path) -> Result<PathBuf, String> {
    let tarball = work.join("app.tar.gz");
    run(crate::spawn::command(system("curl")).args(["-fsSL", "--max-time", "600", "-o"]).arg(&tarball).arg(url))?;
    let bytes = std::fs::read(&tarball).map_err(|e| e.to_string())?;
    if sha256(&bytes) != sha.to_ascii_lowercase() {
        return Err("the download is damaged (checksum mismatch)".into());
    }
    run(crate::spawn::command(system("tar")).arg("-xzf").arg(&tarball).arg("-C").arg(work))?;
    let app = work.join(PACKED);
    if cfg!(target_os = "macos") {
        // Authority is absent at -dv, even for a Developer ID release.
        let signature = Command::new("/usr/bin/codesign").arg("-dvv").arg(&app).output().map_err(|e| format!("codesign: {e}"))?;
        prepare_macos_bundle(&app, &signature)?;
    }
    Ok(app)
}

fn prepare_macos_bundle(app: &Path, signature: &std::process::Output) -> Result<(), String> {
    if release_signature(signature.status.success(), &String::from_utf8_lossy(&signature.stderr))? {
        run(Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(app))?;
    } else {
        // Only known dev builds may change the plist and lose their signature.
        let home = std::env::var("HOME").unwrap_or_default();
        let path = format!("{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin");
        let plist = app.join("Contents/Info.plist");
        run(Command::new("/usr/libexec/PlistBuddy").arg("-c").arg(format!("Add :LSEnvironment:PATH string {path}")).arg(&plist))?;
        run(Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(app))?;
    }
    Ok(())
}

fn release_signature(success: bool, signature: &str) -> Result<bool, String> {
    if success && signature.lines().any(|line| line.starts_with("Authority=Developer ID Application:")) {
        Ok(true)
    } else if (success && signature.lines().any(|line| line == "Signature=adhoc"))
        || (!success && signature.lines().any(|line| line.ends_with(": code object is not signed at all")))
    {
        Ok(false)
    } else {
        Err(format!("cannot determine app signature: {}", signature.trim()))
    }
}

/// curl and tar: on Windows its own (System32), since Git's tar, often first
/// on PATH, takes `C:\...` for a remote host.
pub fn system(name: &str) -> PathBuf {
    match std::env::var_os("SystemRoot").filter(|_| cfg!(windows)) {
        Some(root) => PathBuf::from(root).join("System32").join(format!("{name}.exe")),
        None => PathBuf::from(name),
    }
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn curl_text(url: &str) -> Result<String, String> {
    let out = crate::spawn::command(system("curl")).args(["-fsSL", "--max-time", "30", url]).stdin(Stdio::null()).output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("{url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.stdin(Stdio::null()).output().map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{:?}: {}", cmd.get_program(), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn exe_in(app: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        app.join("Contents/MacOS/Herdr2")
    } else {
        app.join(format!("herdr2{}", std::env::consts::EXE_SUFFIX))
    }
}

/// What `herdr2 --version` says in this folder or bundle (`0.2.0`).
fn version_of(app: &Path) -> Option<String> {
    let out = crate::spawn::command(exe_in(app)).arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return None;
    }
    text.trim().strip_prefix("herdr2 ").map(str::to_owned)
}

/// `a` is a later version than `b` (semver; `0.2.0-rc.1` comes before `0.2.0`).
pub(crate) fn newer(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> (Vec<u64>, Option<&str>) {
        let (core, pre) = v.trim_start_matches('v').split_once('-').map_or((v.trim_start_matches('v'), None), |(c, p)| (c, Some(p)));
        (core.split('.').map(|n| n.parse().unwrap_or(0)).collect(), pre)
    }
    let ((ca, pa), (cb, pb)) = (parts(a), parts(b));
    match ca.cmp(&cb) {
        std::cmp::Ordering::Equal => match (pa, pb) {
            (None, Some(_)) => true,
            (Some(x), Some(y)) => x > y,
            _ => false,
        },
        o => o == std::cmp::Ordering::Greater,
    }
}

/// A receipt is published only after the archive and its executable were verified.
/// Legacy stages without one are left for the normal checker to download again.
fn staged_version(root: &Path) -> Option<String> {
    let staged = beside(root, ".new");
    let receipt = beside(root, ".new.json");
    let version = std::fs::read(&receipt).ok().and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value["version"].as_str().map(str::to_owned));
    let version = version?;
    if version_of(&staged).as_deref() != Some(&version) {
        tracing::warn!("update: staged executable does not match its verified manifest; discarding it");
        let _ = std::fs::remove_file(receipt);
        let _ = std::fs::remove_dir_all(staged);
        return None;
    }
    Some(version)
}

fn startup_target<'a>(running: &str, staged: Option<&'a str>, relaunched: Option<&str>) -> Option<&'a str> {
    if relaunched.is_some() {
        return None;
    }
    staged.filter(|version| newer(version, running))
}

/// Windows can't rename a folder that is a process's working directory, and a
/// launcher or shortcut may start the app in its own folder. Leave it before
/// the core and terminals inherit it, or no update could ever be put in place.
pub fn leave_install_folder() {
    if !cfg!(windows) {
        return;
    }
    let (Some(root), Ok(cwd)) = (root(), std::env::current_dir()) else { return };
    if cwd.canonicalize().is_ok_and(|cwd| cwd.starts_with(&root))
        && let Some(home) = std::env::home_dir()
        && let Err(e) = std::env::set_current_dir(&home)
    {
        tracing::warn!("update: cannot leave the install folder for {}: {e}", home.display());
    }
}

/// Self-updates are disabled: startup never swaps binaries.
pub fn apply_on_startup() -> bool {
    false
}

/// Self-updates are disabled: nothing is applied.
pub fn apply() -> std::io::Result<bool> {
    Ok(false)
}

fn swap(root: &Path) -> std::io::Result<()> {
    let staged = beside(root, ".new");
    let old = beside(root, ".old");
    let _ = std::fs::remove_dir_all(&old);
    if let Err(e) = std::fs::rename(root, &old) {
        tracing::warn!("update: cannot move {} aside: {e}", root.display());
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&staged, root) {
        tracing::warn!("update: cannot put the new version in place: {e}");
        if let Err(rollback) = std::fs::rename(&old, root) {
            tracing::error!("update: cannot restore the previous version: {rollback}");
        }
        return Err(e);
    }
    let _ = std::fs::remove_file(beside(root, ".new.json"));
    *READY.lock().unwrap() = None;
    let _ = std::fs::write(marker(), "");
    Ok(())
}

/// Opens the app again once this process has exited (after `apply`).
pub fn relaunch() {
    relaunch_expected(None);
}

#[cfg(windows)]
fn relaunch_expected(expected: Option<&str>) -> bool {
    let Some(root) = root() else { return false };
    let exe = exe_in(&root).to_string_lossy().replace('\'', "''");
    let script = format!("Wait-Process -Id {} -ErrorAction SilentlyContinue; Start-Process '{exe}'", std::process::id());
    let mut command = crate::spawn::command("powershell");
    if let Some(expected) = expected {
        command.env("HERDR2_APP_UPDATE_EXPECTED", expected);
    }
    let spawned = command
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("update: cannot reopen the app: {e}");
            false
        }
    }
}

/// Opens the app again once this process has exited (after `apply`).
#[cfg(unix)]
fn relaunch_expected(expected: Option<&str>) -> bool {
    let Some(root) = root() else { return false };
    // LaunchServices need not preserve the startup guard or an isolated home.
    let open = if cfg!(target_os = "macos") {
        if expected.is_some() { "exec \"$1/Contents/MacOS/Herdr2\"" } else { "open \"$1\"" }
    } else {
        "exec \"$1/herdr2\""
    };
    let script = format!("while kill -0 \"$0\" 2>/dev/null; do sleep 0.1; done; {open}");
    let spawned = {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("sh");
        if let Some(expected) = expected {
            command.env("HERDR2_APP_UPDATE_EXPECTED", expected);
        }
        command
            .arg("-c")
            .arg(script)
            .arg(std::process::id().to_string())
            .arg(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
    };
    match spawned {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("update: cannot reopen the app: {e}");
            false
        }
    }
}

/// Left by `apply`: the next start brings the core up to date too.
fn marker() -> PathBuf {
    crate::mods::app_dir().join(".updated")
}

/// Post-update core auto-upgrade is disabled.
pub fn after_update(_connected: std::sync::Arc<crate::hc::Core>) {}

fn after_update_managed(status: Result<serde_json::Value, String>) -> bool {
    match status {
        Ok(status) => {
            let text = |key: &str| status[key].as_str().map(str::to_owned);
            set_core(text("version").map(|v| strip_core_prefix(&v).to_owned()), text("installed_by"));
            core().managed.is_some()
        }
        Err(e) => {
            tracing::warn!("cannot check core ownership after app update: {e}; attempting the core update");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn managed_cores_report_new_releases_without_running_updates() {
        const CHILD: &str = "HERDR_TEST_MANAGED_CORE_UPDATE";
        if std::env::var_os(CHILD).is_none() {
            let root = std::env::temp_dir().join(format!("herdr-managed-release-{}", std::process::id()));
            std::fs::create_dir_all(root.join("core")).unwrap();
            std::fs::write(root.join("core/latest.json"), r#"{"version":"0.1.20","generation":1}"#).unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "update::tests::managed_cores_report_new_releases_without_running_updates", "--nocapture"])
                .env(CHILD, "1")
                .env("HERDR2_RELEASES_URL", format!("file://{}", root.display()))
                .output().unwrap();
            std::fs::remove_dir_all(root).unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }
        crate::core_bin::use_core(PathBuf::from("/nonexistent/herdr-core"));
        for manager in ["nix", "homebrew", "mise", "system package manager"] {
            CORE.lock().unwrap().error = Some("an earlier update failed".into());
            set_core(Some("0.1.18".into()), Some(manager.into()));
            check_core().unwrap();
            assert_eq!(core().ready.as_deref(), Some("0.1.20"));
            update_core();
            let state = core();
            assert_eq!(state.managed.as_deref(), Some(manager));
            assert!(!state.updating);
            assert!(state.error.is_none());
            assert_eq!(state.ready.as_deref(), Some("0.1.20"));
            assert!(after_update_managed(Ok(serde_json::json!({"version":"0.1.18", "installed_by":manager}))));
            assert!(!after_update_managed(Err("connection closed".into())), "a failed ownership lookup must still attempt the update");
        }
        assert!(!after_update_managed(Ok(serde_json::json!({"version":"0.1.18", "installed_by":null}))));
    }

    #[test]
    fn failed_check_clears_success_and_recovery_records_a_new_time() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        // An isolated release server drops the first connection, then recovers.
        // No environment changes, app window, core, or installed files are involved.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for recover in [false, true] {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                assert!(request.starts_with(b"GET /app/latest.json HTTP/1.1\r\n"));
                if recover {
                    let body = format!(r#"{{"version":"{}"}}"#, env!("CARGO_PKG_VERSION"));
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
            }
        });
        let home = std::env::temp_dir().join(format!("herdr-offline-check-{}", std::process::id()));
        let mut checked = checked_at(true, 100);
        assert_eq!(checked, Some(100));
        let failed = check_from(&home, &url);
        assert!(failed.is_err(), "a dropped connection must fail: {failed:?}");
        checked = checked_at(failed.is_ok(), 200);
        assert_eq!(checked, None);
        let recovered = check_from(&home, &url);
        assert_eq!(recovered, Ok(None));
        checked = checked_at(recovered.is_ok(), 300);
        assert_eq!(checked, Some(300));
        server.join().unwrap();
    }

    #[test]
    fn startup_applies_only_a_newer_version_and_never_relaunches_twice() {
        assert_eq!(startup_target("0.1.7", Some("0.1.8"), None), Some("0.1.8"));
        for staged in [None, Some("0.1.6"), Some("0.1.7")] {
            assert_eq!(startup_target("0.1.7", staged, None), None);
        }
        for expected in ["0.1.7", "0.1.8", "0.1.9"] {
            assert_eq!(startup_target("0.1.7", Some("0.1.8"), Some(expected)), None);
        }
    }

    #[cfg(unix)]
    #[test]
    fn stages_need_a_receipt_and_a_matching_executable() {
        use std::os::unix::fs::PermissionsExt;
        let work = std::env::temp_dir().join(format!("herdr-update-{}", std::process::id()));
        let root = work.join(PACKED);
        let staged = beside(&root, ".new");
        let receipt = beside(&root, ".new.json");
        let exe = exe_in(&staged);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "#!/bin/sh\necho herdr2 0.2.1\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(staged_version(&root), None);
        assert!(staged.is_dir()); // Legacy stages are left for the checker.
        std::fs::write(&receipt, r#"{"version":"0.2.1"}"#).unwrap();
        assert_eq!(staged_version(&root).as_deref(), Some("0.2.1"));
        std::fs::write(&receipt, r#"{"version":"0.2.2"}"#).unwrap();
        assert_eq!(staged_version(&root), None);
        assert!(!staged.exists());
        assert!(!receipt.exists());
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "corrupt").unwrap();
        std::fs::write(&receipt, r#"{"version":"0.2.1"}"#).unwrap();
        assert_eq!(staged_version(&root), None);
        assert!(!staged.exists());
        std::fs::remove_dir_all(work).unwrap();
    }

    // Windows' remove_dir_all deletes the file blocker, so the swap goes through there.
    #[cfg(unix)]
    #[test]
    fn failed_swap_keeps_the_download_and_reports_the_os_error() {
        let work = std::env::temp_dir().join(format!("herdr-update-blocked-{}", std::process::id()));
        let root = work.join(PACKED);
        let staged = beside(&root, ".new");
        let receipt = beside(&root, ".new.json");
        let old = beside(&root, ".old");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(&receipt, r#"{"version":"9.9.9"}"#).unwrap();
        // A file at the backup path blocks renaming the installed folder.
        std::fs::write(&old, "blocker").unwrap();
        let error = swap(&root).unwrap_err();
        assert!(error.raw_os_error().is_some(), "{error}");
        assert!(root.is_dir());
        assert!(staged.is_dir());
        assert!(receipt.is_file());
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "blocker");
        std::fs::remove_dir_all(work).unwrap();
    }

    #[test]
    fn failed_install_restores_the_previous_version() {
        let work = std::env::temp_dir().join(format!("herdr-update-rollback-{}", std::process::id()));
        let root = work.join(PACKED);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("previous"), "keep me").unwrap();
        // The missing stage fails only after the installed folder was moved.
        let error = swap(&root).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(std::fs::read_to_string(root.join("previous")).unwrap(), "keep me");
        assert!(!beside(&root, ".old").exists());
        std::fs::remove_dir_all(work).unwrap();
    }

    #[test]
    fn signature_inspection_fails_closed() {
        let release = include_str!("../scripts/fixtures/codesign/developer-id.txt");
        let adhoc = include_str!("../scripts/fixtures/codesign/adhoc.txt");
        let unsigned = include_str!("../scripts/fixtures/codesign/unsigned.txt");
        assert_eq!(release_signature(true, release), Ok(true));
        assert_eq!(release_signature(true, adhoc), Ok(false));
        assert_eq!(release_signature(false, unsigned), Ok(false));
        for (success, output) in [(false, release), (false, adhoc), (true, unsigned), (true, ""), (false, "codesign: internal error"), (true, "Authority=Apple Development: Someone")] {
            assert!(release_signature(success, output).unwrap_err().starts_with("cannot determine app signature:"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn unknown_signature_does_not_modify_the_bundle() {
        use std::os::unix::process::ExitStatusExt;
        let work = std::env::temp_dir().join(format!("herdr-unknown-signature-{}", std::process::id()));
        let plist = work.join("Contents/Info.plist");
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, "unchanged plist").unwrap();
        let signature = std::process::Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: b"codesign: internal error".to_vec(),
        };
        assert_eq!(prepare_macos_bundle(&work, &signature), Err("cannot determine app signature: codesign: internal error".into()));
        assert_eq!(std::fs::read_to_string(plist).unwrap(), "unchanged plist");
        assert_eq!(std::fs::read_dir(work.join("Contents")).unwrap().count(), 1);
        std::fs::remove_dir_all(work).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "downloads the current release; runs real codesign on isolated bundles"]
    fn macos_unpack_preserves_release_signature_and_prepares_dev_builds() {
        let version = env!("CARGO_PKG_VERSION");
        let manifest: serde_json::Value = serde_json::from_str(&curl_text(&format!("{}/app/{version}/manifest.json", releases())).unwrap()).unwrap();
        let work = std::env::temp_dir().join(format!("herdr-signature-test-{}", std::process::id()));
        let signed = work.join("signed");
        let reference = work.join("reference");
        std::fs::create_dir_all(&signed).unwrap();
        std::fs::create_dir_all(&reference).unwrap();
        let entry = &manifest["targets"][target()];
        let app = unpack(entry["url"].as_str().unwrap(), entry["sha256"].as_str().unwrap(), &signed).unwrap();
        let signature = Command::new("/usr/bin/codesign").arg("-dvv").arg(&app).output().unwrap();
        assert_eq!(release_signature(signature.status.success(), &String::from_utf8_lossy(&signature.stderr)), Ok(true));
        run(Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(&app)).unwrap();
        run(Command::new("tar").arg("-xzf").arg(signed.join("app.tar.gz")).arg("-C").arg(&reference)).unwrap();
        for file in ["Contents/Info.plist", "Contents/MacOS/Herdr", "Contents/_CodeSignature/CodeResources"] {
            assert_eq!(std::fs::read(app.join(file)).unwrap(), std::fs::read(reference.join(PACKED).join(file)).unwrap(), "{file} was modified");
        }
        for kind in ["adhoc", "unsigned"] {
            let source = reference.join(PACKED);
            let mut codesign = Command::new("/usr/bin/codesign");
            if kind == "adhoc" {
                codesign.args(["--force", "--sign", "-"]);
            } else {
                codesign.arg("--remove-signature");
            }
            run(codesign.arg(&source)).unwrap();
            let archive = work.join(format!("{kind}.tar.gz"));
            run(Command::new("tar").arg("-czf").arg(&archive).arg("-C").arg(&reference).arg(PACKED)).unwrap();
            let dev = work.join(kind);
            std::fs::create_dir_all(&dev).unwrap();
            let sha = sha256(&std::fs::read(&archive).unwrap());
            let app = unpack(&format!("file://{}", archive.display()), &sha, &dev).unwrap();
            let signature = Command::new("/usr/bin/codesign").arg("-dvv").arg(&app).output().unwrap();
            assert!(signature.status.success());
            let signature = String::from_utf8_lossy(&signature.stderr);
            assert_eq!(release_signature(true, &signature), Ok(false));
            assert!(signature.lines().any(|line| line == "Signature=adhoc"));
            run(Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(&app)).unwrap();
            let path = Command::new("/usr/libexec/PlistBuddy").args(["-c", "Print :LSEnvironment:PATH"]).arg(app.join("Contents/Info.plist")).output().unwrap();
            assert!(path.status.success());
            assert!(String::from_utf8_lossy(&path.stdout).contains("/opt/homebrew/bin"));
        }
        std::fs::remove_dir_all(work).unwrap();
    }

    #[test]
    fn versions_compare_like_semver() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("0.10.0", "0.9.0"));
        assert!(newer("1.0.0", "1.0.0-rc.1"));
        assert!(!newer("1.0.0-rc.1", "1.0.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn after_app_update_targets_latest_only_in_our_generation() {
        let release = |version: &str, generation| CoreRelease { version: version.into(), generation };
        let latest = release("0.1.13", Some(1));
        assert_eq!(after_update_target("0.1.12", Some(&latest), 1, "0.1.11"), Some("0.1.13"));
        for have in ["0.1.13", "0.1.14"] {
            assert_eq!(after_update_target(have, Some(&latest), 1, "0.1.11"), Some("0.1.11"));
        }
        for latest in [None, Some(release("0.2.0", Some(2))), Some(release("0.1.13", None))] {
            assert_eq!(after_update_target("0.1.12", latest.as_ref(), 1, "0.1.11"), Some("0.1.11"));
            assert_eq!(after_update_target("0.1.4", latest.as_ref(), 1, "0.1.11"), None);
        }
        assert_eq!(after_update_target("0.1.4", Some(&latest), 1, "0.1.11"), None);
        assert_eq!(after_update_target("0.1.5", Some(&latest), 1, "0.1.11"), Some("0.1.13"));
        assert_eq!(after_update_target("0.1.5", None, 1, "0.1.11"), Some("0.1.11"));
    }

    #[test]
    fn sha256_is_hex() {
        assert_eq!(sha256(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
