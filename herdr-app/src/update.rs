//! The app's own updates, UI bundle hot-updates, and core updates.
//!
//! An installed app (Linux: `~/.local/share/herdr2`, macOS: `Herdr2.app`,
//! Windows: `%LOCALAPPDATA%\Programs\herdr2`) checks `<releases_url>/app/latest.json`,
//! `<releases_url>/ui/latest.json`, and `<releases_url>/core/latest.json` shortly
//! after startup and every hour:
//! - App binary updates download, verify SHA-256, stage to `<root>.new`, and
//!   swap atomically on Restart or quit (or on next startup after crash).
//! - UI bundle updates (`ui/latest.json`) download, verify SHA-256 & host API
//!   compatibility, and hot-apply live without restarting the window.
//! - Core updates (`core/latest.json`) upgrade `h2` in-place via `h2 update`
//!   while keeping all live terminals running.
//!
//! Releases URL priority: `HERDR2_RELEASES_URL` env -> `settings.json`
//! `releases_url` -> compile-time `HERDR2_DEFAULT_RELEASES_URL`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const EVERY: Duration = Duration::from_secs(60 * 60);
/// After a failed check or download (offline, a damaged file).
const RETRY: Duration = Duration::from_secs(30 * 60);
/// Wait briefly after startup so initial UI rendering stays snappy.
const INITIAL_DELAY: Duration = Duration::from_secs(12);

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

fn normalize_url(raw: Option<String>) -> Option<String> {
    raw.map(|u| u.trim().trim_end_matches('/').to_owned()).filter(|u| !u.is_empty())
}

fn resolve_releases_url(env_url: Option<String>, settings_url: Option<String>, default_url: Option<&str>) -> Option<String> {
    normalize_url(env_url)
        .or_else(|| normalize_url(settings_url))
        .or_else(|| normalize_url(default_url.map(str::to_owned)))
}

/// Configured releases base URL (`HERDR2_RELEASES_URL` > `settings.json` > compile-time default).
pub fn releases() -> Option<String> {
    let env_url = std::env::var("HERDR2_RELEASES_URL").ok();
    let settings_url = crate::settings::load_person().ok().and_then(|p| p.releases_url);
    resolve_releases_url(env_url, settings_url, option_env!("HERDR2_DEFAULT_RELEASES_URL"))
}

static READY: Mutex<Option<String>> = Mutex::new(None);

pub fn ready() -> Option<String> {
    READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
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

fn has_explicit_releases_url() -> bool {
    std::env::var("HERDR2_RELEASES_URL").ok().is_some_and(|u| !u.trim().is_empty())
        || crate::settings::load_person()
            .ok()
            .and_then(|p| p.releases_url)
            .is_some_and(|u| !u.trim().is_empty())
}

fn checks_app() -> Option<PathBuf> {
    releases()?;
    let release_build = option_env!("HERDR2_APP_RELEASE").is_some()
        || option_env!("HERDR_APP_RELEASE").is_some()
        || has_explicit_releases_url();
    if !release_build {
        return None;
    }
    root()
}

fn checks_core() -> bool {
    releases().is_some()
}

fn checks_ui() -> bool {
    releases().is_some()
        && crate::mods::dev_defaults_dir().is_none()
        && crate::ui_update::mode() != "off"
}

/// For the settings screen / mods: reports current versions, staged updates, and UI bundle state.
pub fn status() -> serde_json::Value {
    let app_checks = checks_app().is_some();
    let any_checks = app_checks || checks_core() || checks_ui();
    serde_json::json!({
        "app": {
            "version": env!("CARGO_PKG_VERSION"),
            "ready": ready(),
            "updates": app_checks,
        },
        "ui": crate::ui_update::info(),
        "core": core(),
        "checked": *CHECKED.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        "checks": any_checks,
        "checking": CHECKING.load(Ordering::Relaxed),
        "releases_url": releases(),
        "dev_defaults": crate::mods::dev_defaults_dir().map(|p| p.display().to_string()),
    })
}

pub fn check_now() {
    if !RUNNING.load(Ordering::Relaxed) {
        start();
    }
    let mut woken = WAKE.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    *woken = true;
    WAKE.1.notify_all();
}

fn sleep(d: Duration) {
    let woken = WAKE.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (mut woken, _) = WAKE.1.wait_timeout_while(woken, d, |w| !*w).unwrap();
    *woken = false;
}

/// Starts the background update checker thread (idempotent).
pub fn start() {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(r) = checks_app()
        && let Some(ver) = staged_version(&r)
        && newer(&ver, env!("CARGO_PKG_VERSION"))
    {
        *READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ver);
    }
    let _ = std::thread::Builder::new().name("herdr2-update".into()).spawn(|| {
        sleep(INITIAL_DELAY);
        loop {
            let ok = run_one_check();
            sleep(if ok { EVERY } else { RETRY });
        }
    });
}

fn is_missing_feed_err(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("404")
        || lower.contains("not found")
        || lower.contains("couldn't read a file://")
        || lower.contains("no such file")
}

fn check_cycle_succeeded(app_err: Option<&str>, ui_ok: bool, core_err: Option<&str>) -> bool {
    let optional_ok = |err: Option<&str>| match err {
        None => true,
        Some(e) => ui_ok && is_missing_feed_err(e),
    };
    optional_ok(app_err) && optional_ok(core_err)
}

fn run_one_check() -> bool {
    let Some(base_url) = releases() else {
        return true;
    };
    CHECKING.store(true, Ordering::Relaxed);
    let mut any_attempted = false;
    let mut app_err: Option<String> = None;
    let mut ui_ok = false;
    let mut core_err: Option<String> = None;

    // 1. App binary update (installed bundles / release installs)
    let mut app_update_ready = false;
    if let Some(install_root) = checks_app() {
        any_attempted = true;
        match check_from(&install_root, &base_url) {
            Ok(ver) => {
                app_update_ready = ver.is_some();
                *READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = ver;
            }
            Err(e) => {
                if is_missing_feed_err(&e) {
                    tracing::debug!("app update feed absent: {e}");
                } else {
                    tracing::warn!("app update check failed: {e}");
                }
                app_err = Some(e);
            }
        }
    }

    // 2. UI bundle hot-update (skip if a full app binary update is already ready)
    if !app_update_ready && checks_ui() {
        any_attempted = true;
        let app_dir = crate::mods::app_dir();
        match crate::ui_update::check_from(&app_dir, &base_url) {
            Ok(Some(_ver)) => {
                ui_ok = true;
                if crate::ui_update::mode() == "auto" {
                    crate::mods::request(crate::mods::Request::ApplyUiBundle);
                }
            }
            Ok(None) => {
                ui_ok = true;
            }
            Err(e) => {
                // UI feed is optional on servers that only host app/core manifests.
                tracing::debug!("ui bundle check skipped/failed: {e}");
            }
        }
    }

    // 3. Core update check
    if checks_core() {
        any_attempted = true;
        if let Err(e) = check_core_from(&base_url) {
            if is_missing_feed_err(&e) {
                tracing::debug!("core update feed absent: {e}");
            } else {
                tracing::warn!("core update check failed: {e}");
            }
            core_err = Some(e);
        }
    }

    let all_ok = check_cycle_succeeded(app_err.as_deref(), ui_ok, core_err.as_deref());
    CHECKING.store(false, Ordering::Relaxed);
    if any_attempted {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        *CHECKED.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = checked_at(all_ok, now);
    }
    all_ok
}

fn checked_at(succeeded: bool, now: u64) -> Option<u64> {
    succeeded.then_some(now)
}

/// Looks for a newer core of this app's generation.
#[cfg(test)]
fn check_core() -> Result<(), String> {
    let base_url = releases().ok_or_else(|| "HERDR2_RELEASES_URL not configured".to_owned())?;
    check_core_from(&base_url)
}

fn check_core_from(base_url: &str) -> Result<(), String> {
    let have = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).version.clone();
    let Some(have) = have else { return Ok(()) };
    let latest = latest_core_from(base_url)?;
    let same = latest.generation == Some(crate::hc::GENERATION);
    CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).ready =
        (same && newer(&latest.version, &have)).then_some(latest.version);
    Ok(())
}

#[derive(serde::Deserialize)]
struct CoreRelease {
    version: String,
    generation: Option<u64>,
}

fn latest_core_from(base_url: &str) -> Result<CoreRelease, String> {
    let text = curl_text(&format!("{base_url}/core/latest.json"))?;
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

/// Upgrades `h2` in the background (keeping running terminals alive).
pub fn update_core() {
    let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if c.managed.is_some() || c.updating {
        return;
    }
    c.updating = true;
    c.error = None;
    drop(c);
    let _ = std::thread::Builder::new().name("herdr2-core-update".into()).spawn(|| {
        let res = crate::core_bin::update();
        let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        c.updating = false;
        match res {
            Ok(now) => {
                c.version = Some(strip_core_prefix(&now).to_owned());
                c.ready = None;
                c.error = None;
            }
            Err(e) => {
                c.error = Some(e);
            }
        }
    });
}

/// Downloads the latest release if it's newer. Ok(Some): a version is ready.
#[allow(dead_code)]
fn check(root: &Path) -> Result<Option<String>, String> {
    let base_url = releases().ok_or_else(|| "HERDR2_RELEASES_URL not configured".to_owned())?;
    check_from(root, &base_url)
}

fn check_from(root: &Path, releases: &str) -> Result<Option<String>, String> {
    let path = "app/latest.json";
    let text = curl_text(&format!("{releases}/{path}"))?;
    let manifest: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    let version = manifest["version"].as_str().ok_or(format!("{path}: no version"))?;
    if !newer(version, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
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
    let ext = if url.ends_with(".zip") { "zip" } else { "tar.gz" };
    let tarball = work.join(format!("app.{ext}"));
    download_file(url, &tarball)?;
    let bytes = std::fs::read(&tarball).map_err(|e| e.to_string())?;
    if sha256(&bytes) != sha.to_ascii_lowercase() {
        return Err("the download is damaged (checksum mismatch)".into());
    }
    unpack_archive(&tarball, work)?;
    let app = work.join(PACKED);
    if cfg!(target_os = "macos") {
        // Authority is absent at -dv, even for a Developer ID release.
        let signature = Command::new("/usr/bin/codesign").arg("-dvv").arg(&app).output().map_err(|e| format!("codesign: {e}"))?;
        prepare_macos_bundle(&app, &signature)?;
    }
    Ok(app)
}

pub(crate) fn download_file(url: &str, dest: &Path) -> Result<(), String> {
    run(crate::spawn::command(system("curl")).args(["-fsSL", "--max-time", "600", "-o"]).arg(dest).arg(url))
}

pub(crate) fn unpack_archive(archive: &Path, dest: &Path) -> Result<(), String> {
    let flag = if archive.extension().is_some_and(|e| e == "zip") { "-xf" } else { "-xzf" };
    run(crate::spawn::command(system("tar")).arg(flag).arg(archive).arg("-C").arg(dest))
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

pub(crate) fn curl_text(url: &str) -> Result<String, String> {
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

/// Cleans up leftover `<root>.old` folders and in-place `*.old-*` swap files
/// from previous updates (Windows cannot delete the previous executable until it exits).
pub fn cleanup_old() {
    let Some(root) = root() else { return };
    let old = beside(&root, ".old");
    if old.exists() {
        let _ = std::fs::remove_dir_all(&old);
    }
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.contains(".old-") && entry.file_type().is_ok_and(|ft| ft.is_file()) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Applies a previously staged newer update before opening the window.
/// Returns `true` when swapped & relaunched (caller should exit immediately).
pub fn apply_on_startup() -> bool {
    let Some(root) = checks_app() else { return false };
    let staged = staged_version(&root);
    let relaunched = std::env::var("HERDR2_APP_UPDATE_EXPECTED").ok();
    let Some(target) = startup_target(env!("CARGO_PKG_VERSION"), staged.as_deref(), relaunched.as_deref()) else {
        return false;
    };
    if let Err(e) = swap(&root) {
        tracing::warn!("update: startup swap failed: {e}");
        return false;
    }
    relaunch_expected(Some(target))
}

/// Swaps a staged update into place; returns `Ok(true)` if swapped.
pub fn apply() -> std::io::Result<bool> {
    let Some(root) = checks_app() else { return Ok(false) };
    let Some(version) = staged_version(&root) else { return Ok(false) };
    if !newer(&version, env!("CARGO_PKG_VERSION")) {
        return Ok(false);
    }
    swap(&root)?;
    Ok(true)
}

/// Relaunches the currently running executable once this process exits.
/// Used by `app.restart` for fast dev-mode restarts (preserving live terminals in `h2`).
/// Prefers the canonical install executable (`<root>/herdr2.exe`) when installed so an
/// in-place rename swap (`herdr2.exe` -> `herdr2.exe.old-*`) always relaunches the new binary.
#[cfg(windows)]
pub fn relaunch_current() -> bool {
    let exe = root()
        .map(|r| exe_in(&r))
        .filter(|p| p.is_file())
        .or_else(|| std::env::current_exe().ok());
    let Some(exe) = exe else { return false };
    let exe = exe.to_string_lossy().replace('\'', "''");
    let script = format!("Wait-Process -Id {} -ErrorAction SilentlyContinue; Start-Process '{exe}'", std::process::id());
    crate::spawn::command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(unix)]
pub fn relaunch_current() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    use std::os::unix::process::CommandExt;
    let script = "while kill -0 \"$0\" 2>/dev/null; do sleep 0.1; done; exec \"$1\"";
    Command::new("sh")
        .arg("-c")
        .arg(script)
        .arg(std::process::id().to_string())
        .arg(&exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .is_ok()
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

/// Runs once after an app binary swap: brings `h2` core up to date in the same generation.
pub fn after_update(connected: std::sync::Arc<crate::hc::Core>) {
    let mark = marker();
    if !mark.is_file() {
        return;
    }
    let _ = std::fs::remove_file(&mark);
    if !checks_core() {
        return;
    }
    let _ = std::thread::Builder::new().name("herdr2-after-update".into()).spawn(move || {
        let status = connected.call("server.status", serde_json::json!({})).map_err(|e| format!("{e:#}"));
        if after_update_managed(status) {
            return;
        }
        let have = core().version.unwrap_or_default();
        let latest = releases().and_then(|base| latest_core_from(&base).ok());
        let Some(target) = after_update_target(&have, latest.as_ref(), crate::hc::GENERATION, crate::hc::core_min()) else {
            return;
        };
        let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if c.updating {
            return;
        }
        c.updating = true;
        c.error = None;
        drop(c);
        let res = crate::core_bin::update_to(Some(target));
        let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        c.updating = false;
        match res {
            Ok(now) => {
                c.version = Some(strip_core_prefix(&now).to_owned());
                c.ready = None;
                c.error = None;
            }
            Err(e) => {
                c.error = Some(e);
            }
        }
    });
}

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
        let manifest: serde_json::Value = serde_json::from_str(&curl_text(&format!("{}/app/{version}/manifest.json", releases().unwrap_or_default())).unwrap()).unwrap();
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

    #[test]
    fn releases_url_resolves_env_then_settings_then_default() {
        assert_eq!(
            resolve_releases_url(Some("https://env.test/ ".into()), Some("https://set.test".into()), Some("https://def.test")).as_deref(),
            Some("https://env.test")
        );
        assert_eq!(
            resolve_releases_url(Some("  ".into()), Some("https://set.test/".into()), Some("https://def.test")).as_deref(),
            Some("https://set.test")
        );
        assert_eq!(
            resolve_releases_url(None, None, Some("https://def.test/")).as_deref(),
            Some("https://def.test")
        );
        assert_eq!(resolve_releases_url(None, None, None), None);
    }

    #[test]
    fn ui_only_feed_404_on_app_and_core_still_counts_as_successful_check() {
        let missing_http = "http://127.0.0.1:8787/app/latest.json: curl: (22) The requested URL returned error: 404";
        let missing_file = "file:///tmp/core/latest.json: curl: (37) Couldn't read a file:// file";
        let net_err = "http://127.0.0.1:8787/app/latest.json: curl: (7) Failed to connect";

        assert!(check_cycle_succeeded(Some(missing_http), true, Some(missing_file)));
        assert!(!check_cycle_succeeded(Some(missing_http), false, Some(missing_file)));
        assert!(!check_cycle_succeeded(Some(net_err), true, None));
        assert!(check_cycle_succeeded(None, false, None));
    }
}
