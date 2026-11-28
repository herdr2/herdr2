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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const EVERY: Duration = Duration::from_secs(60 * 60);
/// After a failed check or download (offline, a damaged file).
const RETRY: Duration = Duration::from_secs(10 * 60);
/// Wait briefly after startup so initial UI rendering stays snappy.
const INITIAL_DELAY: Duration = Duration::from_secs(12);

pub(crate) fn is_github_release_base(base: &str) -> bool {
    let trimmed = base.trim_end_matches('/');
    trimmed.contains("/releases/download/") || trimmed.ends_with("/releases/latest/download")
}

/// Resolves a channel manifest URL for either a standard hierarchical release server
/// (`<base>/<channel>/latest.json`) or a flat GitHub Releases tag (`<base>/<channel>-latest.json`).
pub(crate) fn feed_url(base: &str, channel: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if is_github_release_base(trimmed) {
        format!("{trimmed}/{channel}-latest.json")
    } else {
        format!("{trimmed}/{channel}/latest.json")
    }
}

/// Where this app is installed: the folder (Linux) or bundle (macOS) an
/// update replaces. Read once: after a swap this process's own file is gone.
fn root() -> Option<PathBuf> {
    static ROOT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    ROOT.get_or_init(find_root).clone()
}

/// Strips Win32 verbatim prefixes (`\\?\` and `\\?\UNC\`) returned by `Path::canonicalize()`
/// on Windows. Windows Shell APIs (Taskbar, `Start-Process`, `SHChangeNotify`, shortcut
/// matching) do not normalize `\\?\` paths and fall back to blank/generic icons when given one.
fn strip_windows_verbatim_prefix(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

fn find_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let exe = if cfg!(windows) { strip_windows_verbatim_prefix(&exe) } else { exe };
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

#[derive(Clone, Debug)]
struct AppAvailable {
    version: String,
    url: String,
    sha256: String,
    notes: Option<String>,
}

static READY: Mutex<Option<String>> = Mutex::new(None);
static APP_AVAILABLE: Mutex<Option<AppAvailable>> = Mutex::new(None);
static UPDATING: AtomicBool = AtomicBool::new(false);
static APP_UPDATING: AtomicBool = AtomicBool::new(false);
static ELEVATED_APPLIED: AtomicBool = AtomicBool::new(false);
static APP_ERROR: Mutex<Option<String>> = Mutex::new(None);
static CHECK_SEQ: AtomicU64 = AtomicU64::new(0);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

struct UpdatingGuard;

impl Drop for UpdatingGuard {
    fn drop(&mut self) {
        UPDATING.store(false, Ordering::SeqCst);
        crate::mods::request(crate::mods::Request::Refresh);
    }
}

fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(
        ".herdr2-write-probe-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// True on Linux when the install folder or its parent isn't writable by the current
/// user (for example a `.deb` package installed into root-owned `/opt/herdr2`).
fn needs_elevation_for(root: &Path) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let parent_writable = root.parent().is_some_and(dir_writable);
    let root_writable = dir_writable(root);
    !(parent_writable && root_writable)
}

fn needs_elevation() -> bool {
    static NEEDS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *NEEDS.get_or_init(|| root().is_some_and(|r| needs_elevation_for(&r)))
}

fn has_bundled_core(root: &Path) -> bool {
    root.join("herdr2-core").is_file() || root.join("Contents/Helpers/h2").is_file()
}

pub fn ready() -> Option<String> {
    READY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .or_else(|| {
            APP_AVAILABLE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .map(|a| a.version.clone())
        })
}

pub fn is_updating() -> bool {
    UPDATING.load(Ordering::Relaxed)
        || APP_UPDATING.load(Ordering::Relaxed)
        || crate::ui_update::is_applying()
        || CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).updating
        || MACHINES_UPDATING.load(Ordering::Relaxed)
}

/// The core the app runs, from its `server.status`, and a newer release waiting.
#[derive(Clone, Default, serde::Serialize)]
pub struct CoreInfo {
    pub version: Option<String>,
    /// A package manager that owns it (homebrew, nix, mise): never updated by the app.
    pub managed: Option<String>,
    pub ready: Option<String>,
    pub notes: Option<String>,
    pub updating: bool,
    pub error: Option<String>,
}

/// Status of one configured SSH remote machine's `herdr2-core` for auto-sync updates.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct RemoteMachineUpdateInfo {
    pub name: String,
    pub target: String,
    pub version: Option<String>,
    /// Normalized display status (`needs_update` when unreachable or outdated, else `connected`/`connecting`).
    pub state: String,
    /// Raw connection state reported by `h2 machine ls`.
    pub raw_state: String,
    pub why: Option<String>,
    pub ready: Option<String>,
    pub needs_update: bool,
    pub unreachable: bool,
    pub updating: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct RemoteSyncSummary {
    updated: Vec<(String, String)>,
    failed: Vec<(String, String)>,
}

static CORE: Mutex<CoreInfo> = Mutex::new(CoreInfo { version: None, managed: None, ready: None, notes: None, updating: false, error: None });
/// Cached `--version` of the active `core_bin::path()` binary on disk (refreshed off the UI thread).
static CORE_DISK_VERSION: Mutex<Option<String>> = Mutex::new(None);
static MACHINES: Mutex<Vec<RemoteMachineUpdateInfo>> = Mutex::new(Vec::new());
static MACHINES_UPDATING: AtomicBool = AtomicBool::new(false);
#[cfg(not(test))]
static MACHINES_INIT_REFRESH: AtomicBool = AtomicBool::new(false);
/// When the last check succeeded (unix seconds); a failed check clears it.
static CHECKED: Mutex<Option<u64>> = Mutex::new(None);
/// "Check now" wakes the checker early.
static WAKE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
/// The checker runs (a release, or pointed at a test server), and is looking right now.
static RUNNING: AtomicBool = AtomicBool::new(false);
static CHECKING: AtomicBool = AtomicBool::new(false);

pub fn core() -> CoreInfo {
    CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

pub fn machines() -> Vec<RemoteMachineUpdateInfo> {
    MACHINES.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

fn target_core_version() -> Option<String> {
    let c = core();
    c.ready.or(c.version)
}

fn clean_core_version(v: &str) -> &str {
    strip_core_prefix(v).split_whitespace().next().unwrap_or("")
}

pub(crate) fn newer_core_version(a: &str, b: &str) -> bool {
    let ca = clean_core_version(a);
    let cb = clean_core_version(b);
    !ca.is_empty() && !cb.is_empty() && newer(ca, cb)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CoreUpdateStrategy {
    /// Standard user-writable core: run `h2 update --json`.
    Direct,
    /// Root-owned bundled core on Linux (e.g. `.deb` `/opt/herdr2/herdr2-core`):
    /// download verified core binary, atomically swap via `pkexec`, and hot-restart the daemon.
    ElevatedLinuxBundled,
    /// macOS helper core bundled inside `Herdr2.app/Contents/Helpers/h2`.
    MacosBundled,
    /// Externally owned package manager (homebrew, nix, mise, or non-bundled unwritable system path).
    ExternalManaged,
}

fn core_update_strategy_for(
    core_info: &CoreInfo,
    install_root: Option<&Path>,
    active_core: &Path,
    is_linux: bool,
    is_macos: bool,
) -> CoreUpdateStrategy {
    let resolved = active_core.canonicalize().ok();
    if is_macos
        && let Some(p) = resolved.as_deref()
        && install_root.is_some_and(|r| p.starts_with(r))
        && p.ends_with("Contents/Helpers/h2")
    {
        return CoreUpdateStrategy::MacosBundled;
    }
    if core_info.managed.is_none() {
        return CoreUpdateStrategy::Direct;
    }
    if is_linux
        && core_info.managed.as_deref() == Some("system package manager")
        && let Some(p) = resolved.as_deref()
        && p.is_file()
        && (install_root.is_some_and(|r| p.starts_with(r)) || p.starts_with("/opt/herdr2"))
    {
        return CoreUpdateStrategy::ElevatedLinuxBundled;
    }
    CoreUpdateStrategy::ExternalManaged
}

fn disk_core_newer_than_running(core_info: &CoreInfo, disk_ver: Option<&str>) -> Option<String> {
    let disk = disk_ver.map(clean_core_version).filter(|s| !s.is_empty())?;
    let running = core_info.version.as_deref().map(clean_core_version).unwrap_or("");
    (!running.is_empty() && newer(disk, running)).then(|| disk.to_owned())
}

fn disk_core_covers_ready(core_info: &CoreInfo, disk_ver: Option<&str>) -> Option<String> {
    disk_core_newer_than_running(core_info, disk_ver).filter(|disk| {
        core_info
            .ready
            .as_deref()
            .map(clean_core_version)
            .filter(|r| !r.is_empty())
            .is_none_or(|r| !newer(r, disk))
    })
}

fn core_can_self_update_for(
    core_info: &CoreInfo,
    disk_ver: Option<&str>,
    install_root: Option<&Path>,
    active_core: &Path,
    is_linux: bool,
    is_macos: bool,
) -> (bool, bool) {
    if disk_core_covers_ready(core_info, disk_ver).is_some() {
        return (true, false);
    }
    match core_update_strategy_for(core_info, install_root, active_core, is_linux, is_macos) {
        CoreUpdateStrategy::Direct => (true, false),
        CoreUpdateStrategy::ElevatedLinuxBundled => (true, true),
        CoreUpdateStrategy::MacosBundled => (core_info.managed.is_none(), false),
        CoreUpdateStrategy::ExternalManaged => (false, false),
    }
}

fn cached_disk_core_version() -> Option<String> {
    CORE_DISK_VERSION.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

fn refresh_disk_core_version() -> Option<String> {
    let ver = crate::core_bin::version(&crate::core_bin::path())
        .map(|v| clean_core_version(&v).to_owned())
        .filter(|v| !v.is_empty());
    *CORE_DISK_VERSION.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = ver.clone();
    ver
}

fn core_self_update_capabilities(core_info: &CoreInfo) -> (bool, bool) {
    let install_root = root();
    let active_core = crate::core_bin::path();
    let disk_ver = cached_disk_core_version();
    core_can_self_update_for(
        core_info,
        disk_ver.as_deref(),
        install_root.as_deref(),
        &active_core,
        cfg!(target_os = "linux"),
        cfg!(target_os = "macos"),
    )
}

pub fn core_can_update() -> bool {
    core_self_update_capabilities(&core()).0
}

fn classify_remote_machine(
    m: &crate::core_bin::RemoteMachine,
    target_core: Option<&str>,
    prev: Option<&RemoteMachineUpdateInfo>,
) -> RemoteMachineUpdateInfo {
    let raw_state = m.state.clone();
    let unreachable = matches!(raw_state.as_str(), "unreachable" | "needs_login");
    let version = m
        .version
        .as_deref()
        .map(clean_core_version)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let target_clean = target_core.map(clean_core_version).filter(|s| !s.is_empty());
    let outdated = raw_state == "needs_update"
        || match (version.as_deref(), target_clean) {
            (Some(cur), Some(tgt)) => newer(tgt, cur),
            (None, Some(_)) => true,
            _ => false,
        };
    let needs_update = outdated || unreachable;
    let state = if needs_update {
        "needs_update".to_owned()
    } else if raw_state == "connected" {
        "connected".to_owned()
    } else {
        raw_state.clone()
    };
    let ready = if outdated { target_clean.map(str::to_owned) } else { None };
    let updating = prev.is_some_and(|p| p.updating);
    let error = if needs_update { prev.and_then(|p| p.error.clone()) } else { None };
    RemoteMachineUpdateInfo {
        name: m.name.clone(),
        target: m.target.clone(),
        version,
        state,
        raw_state,
        why: m.why.clone(),
        ready,
        needs_update,
        unreachable,
        updating,
        error,
    }
}

fn remote_ready_from(machines: &[RemoteMachineUpdateInfo], target_core: Option<&str>) -> Option<String> {
    machines
        .iter()
        .find(|m| !m.updating && (m.ready.is_some() || m.raw_state == "needs_update"))
        .and_then(|m| m.ready.clone().or_else(|| target_core.map(str::to_owned)))
}

pub fn remote_ready() -> Option<String> {
    let list = machines();
    let target = target_core_version();
    remote_ready_from(&list, target.as_deref())
}

fn has_pending_remote_updates() -> bool {
    machines().iter().any(|m| !m.updating && (m.ready.is_some() || m.raw_state == "needs_update"))
}

fn reclassify_machines() {
    let target = target_core_version();
    let mut guard = MACHINES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for m in guard.iter_mut() {
        let raw = crate::core_bin::RemoteMachine {
            name: m.name.clone(),
            target: m.target.clone(),
            version: m.version.clone(),
            state: m.raw_state.clone(),
            why: m.why.clone(),
            temporary: false,
        };
        *m = classify_remote_machine(&raw, target.as_deref(), Some(m));
    }
}

pub fn refresh_machines() {
    let Ok(raw_list) = crate::core_bin::list_machines() else {
        return;
    };
    let target = target_core_version();
    let mut guard = MACHINES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let prev = std::mem::take(&mut *guard);
    *guard = raw_list
        .iter()
        .map(|m| {
            let old = prev.iter().find(|p| p.name == m.name);
            classify_remote_machine(m, target.as_deref(), old)
        })
        .collect();
}

/// What the connected core says about itself (on connect and every reconnect).
pub fn set_core(version: Option<String>, managed: Option<String>) {
    let cleaned = version
        .as_deref()
        .map(clean_core_version)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let (Some(have), Some(ready)) = (&cleaned, &c.ready)
        && !newer(ready, have)
    {
        c.ready = None;
    }
    c.version = cleaned;
    c.managed = managed;
    if !core_self_update_capabilities(&c).0 {
        c.error = None;
    }
    drop(c);
    reclassify_machines();
    #[cfg(not(test))]
    if !MACHINES_INIT_REFRESH.swap(true, Ordering::SeqCst) {
        let _ = std::thread::Builder::new().name("herdr2-machines-init".into()).spawn(|| {
            refresh_disk_core_version();
            refresh_machines();
            crate::mods::request(crate::mods::Request::Refresh);
        });
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

/// For the settings screen / mods: reports current versions, available updates, and progress.
pub fn status() -> serde_json::Value {
    let install_root = checks_app();
    let app_checks = install_root.is_some();
    let app_needs_elevation = app_checks && needs_elevation();
    let any_checks = app_checks || checks_core() || checks_ui();
    let app_staged = READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let app_avail = APP_AVAILABLE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let app_ready = app_staged.clone().or_else(|| app_avail.as_ref().map(|a| a.version.clone()));
    let app_notes = app_avail.as_ref().and_then(|a| a.notes.clone());
    let app_updating = APP_UPDATING.load(Ordering::Relaxed);
    let app_error = APP_ERROR.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let ui = crate::ui_update::info();
    let core_info = core();
    let (core_can_update, core_needs_elev) = core_self_update_capabilities(&core_info);
    let machines_snapshot = machines();
    let machines_updating = MACHINES_UPDATING.load(Ordering::Relaxed) || machines_snapshot.iter().any(|m| m.updating);
    let updating = UPDATING.load(Ordering::Relaxed) || app_updating || ui.applying || core_info.updating || machines_updating;
    let core_updates_with_app = app_ready.is_some()
        && install_root.as_deref().is_some_and(has_bundled_core)
        && matches!(core_info.managed.as_deref(), None | Some("system package manager"));
    let target_core = core_info.ready.clone().or_else(|| core_info.version.clone());
    let remote_ready_ver = remote_ready_from(&machines_snapshot, target_core.as_deref());

    let mut items: Vec<serde_json::Value> = Vec::new();
    if let Some(ver) = &app_ready {
        items.push(serde_json::json!({
            "kind": "app",
            "current": env!("CARGO_PKG_VERSION"),
            "next": ver,
            "notes": app_notes,
        }));
    }
    if app_ready.is_none() && let Some(ver) = &ui.ready {
        items.push(serde_json::json!({
            "kind": "ui",
            "current": ui.version,
            "next": ver,
            "notes": ui.ready_notes,
        }));
    }
    if (core_can_update || core_updates_with_app) && let Some(ver) = &core_info.ready {
        items.push(serde_json::json!({
            "kind": "core",
            "current": core_info.version,
            "next": ver,
            "notes": core_info.notes,
        }));
    }
    for m in &machines_snapshot {
        if m.ready.is_some() || m.raw_state == "needs_update" {
            items.push(serde_json::json!({
                "kind": "remote",
                "machine": m.name,
                "target": m.target,
                "current": m.version,
                "next": m.ready.clone().or_else(|| target_core.clone()),
                "state": m.state,
                "raw_state": m.raw_state,
                "unreachable": m.unreachable,
            }));
        }
    }
    let available = if let Some(ver) = &app_ready {
        Some(serde_json::json!({
            "kind": "app",
            "version": ver,
            "notes": app_notes,
            "restart": true,
            "needs_elevation": app_needs_elevation,
            "items": items,
        }))
    } else if let Some(ver) = &ui.ready {
        Some(serde_json::json!({
            "kind": "ui",
            "version": ver,
            "notes": ui.ready_notes,
            "restart": false,
            "needs_elevation": core_can_update && core_info.ready.is_some() && core_needs_elev,
            "items": items,
        }))
    } else if core_can_update && let Some(ver) = &core_info.ready {
        Some(serde_json::json!({
            "kind": "core",
            "version": ver,
            "notes": core_info.notes,
            "restart": false,
            "needs_elevation": core_needs_elev,
            "items": items,
        }))
    } else if let Some(ver) = &remote_ready_ver {
        Some(serde_json::json!({
            "kind": "remote",
            "version": ver,
            "notes": serde_json::Value::Null,
            "restart": false,
            "items": items,
        }))
    } else {
        None
    };

    let mut core_json = serde_json::to_value(&core_info).unwrap_or_default();
    if let Some(obj) = core_json.as_object_mut() {
        obj.insert("can_update".into(), serde_json::Value::Bool(core_can_update));
        obj.insert("needs_elevation".into(), serde_json::Value::Bool(core_needs_elev));
    }

    serde_json::json!({
        "app": {
            "version": env!("CARGO_PKG_VERSION"),
            "ready": app_ready,
            "staged": app_staged.is_some(),
            "notes": app_notes,
            "updating": app_updating,
            "error": app_error,
            "updates": app_checks,
            "needs_elevation": app_needs_elevation,
        },
        "ui": ui,
        "core": core_json,
        "machines": machines_snapshot,
        "updating": updating,
        "available": available,
        "check_seq": CHECK_SEQ.load(Ordering::Relaxed),
        "last_error": LAST_ERROR.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
        "checked": *CHECKED.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        "checks": any_checks,
        "checking": CHECKING.load(Ordering::Relaxed),
        "releases_url": releases(),
        "dev_defaults": crate::mods::dev_defaults_dir().map(|p| p.display().to_string()),
    })
}

pub fn check_now() {
    CHECKING.store(true, Ordering::SeqCst);
    *LAST_ERROR.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    {
        let mut woken = WAKE.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *woken = true;
        WAKE.1.notify_all();
    }
    if !RUNNING.load(Ordering::Relaxed) {
        start();
    }
    crate::mods::request(crate::mods::Request::Refresh);
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
        *LAST_ERROR.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some("No update URL configured".into());
        CHECKING.store(false, Ordering::SeqCst);
        CHECK_SEQ.fetch_add(1, Ordering::SeqCst);
        crate::mods::request(crate::mods::Request::Refresh);
        return true;
    };
    CHECKING.store(true, Ordering::SeqCst);
    *LAST_ERROR.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    crate::mods::request(crate::mods::Request::Refresh);

    let mut any_attempted = false;
    let mut app_err: Option<String> = None;
    let mut ui_ok = false;
    let mut ui_err: Option<String> = None;
    let mut core_err: Option<String> = None;

    // 1. App binary update metadata check (fast; download starts only when user clicks Update)
    let mut app_has_update = false;
    if let Some(install_root) = checks_app() {
        any_attempted = true;
        match check_app_manifest_from(&install_root, &base_url) {
            Ok(avail) => {
                app_has_update = avail.is_some();
                if let Some(ref a) = avail
                    && staged_version(&install_root).as_deref() == Some(a.version.as_str())
                {
                    *READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(a.version.clone());
                } else if avail.is_none() {
                    *READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                }
                *APP_AVAILABLE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = avail;
                *APP_ERROR.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
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

    // 2. UI bundle hot-update metadata check (skip if a full app binary update is available)
    if app_has_update {
        crate::ui_update::clear_available();
    } else if checks_ui() {
        any_attempted = true;
        let app_dir = crate::mods::app_dir();
        match crate::ui_update::check_manifest_from(&app_dir, &base_url) {
            Ok(_) => {
                ui_ok = true;
            }
            Err(e) => {
                tracing::debug!("ui bundle check skipped/failed: {e}");
                ui_err = Some(e);
            }
        }
    }

    // 3. Core update metadata check
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

    // 4. Refresh SSH remote machines' versions & update status
    refresh_machines();

    let all_ok = check_cycle_succeeded(app_err.as_deref(), ui_ok, core_err.as_deref());
    let first_err = if all_ok {
        None
    } else {
        app_err.or(core_err).or(ui_err)
    };
    *LAST_ERROR.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = first_err;
    if any_attempted {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        *CHECKED.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = checked_at(all_ok, now);
    }
    CHECKING.store(false, Ordering::SeqCst);
    CHECK_SEQ.fetch_add(1, Ordering::SeqCst);
    crate::mods::request(crate::mods::Request::Refresh);
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

fn refresh_core_state_from_live() {
    if let Some(live) = crate::mods::live_core()
        && let Ok(s) = live.call_timeout("server.status", serde_json::json!({}), Duration::from_secs(3))
    {
        let text = |k: &str| s.get(k).and_then(serde_json::Value::as_str).map(str::to_owned);
        set_core(text("version"), text("installed_by"));
    }
    let disk_ver = refresh_disk_core_version();
    if let Some(disk) = disk_ver {
        let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if c.version.is_none() {
            c.version = Some(disk);
        }
    }
}

fn check_core_from(base_url: &str) -> Result<(), String> {
    refresh_core_state_from_live();
    let have = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).version.clone();
    let Some(have) = have else { return Ok(()) };
    let have_clean = clean_core_version(&have);
    let disk_ver = cached_disk_core_version();
    let latest = match latest_core_from(base_url) {
        Ok(latest) => latest,
        Err(e) => {
            if let Some(disk) = disk_ver.as_deref().filter(|d| !have_clean.is_empty() && newer(d, have_clean)) {
                let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                c.ready = Some(disk.to_owned());
                c.notes = None;
                drop(c);
                reclassify_machines();
                return Ok(());
            }
            return Err(e);
        }
    };
    let same = latest.generation == Some(crate::hc::GENERATION);
    let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if same && newer(&latest.version, have_clean) {
        c.ready = Some(latest.version);
        c.notes = latest.notes;
    } else if let Some(disk) = disk_ver.as_deref().filter(|d| !have_clean.is_empty() && newer(d, have_clean)) {
        c.ready = Some(disk.to_owned());
        c.notes = None;
    } else {
        c.ready = None;
        c.notes = None;
    }
    drop(c);
    reclassify_machines();
    Ok(())
}

#[derive(serde::Deserialize)]
struct CoreRelease {
    version: String,
    generation: Option<u64>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    targets: serde_json::Value,
}

fn latest_core_from(base_url: &str) -> Result<CoreRelease, String> {
    let text = curl_text(&feed_url(base_url, "core"))?;
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

fn sync_remote_machines_blocking(force: bool) -> RemoteSyncSummary {
    refresh_machines();
    let targets: Vec<String> = {
        let guard = MACHINES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .iter()
            .filter(|m| force || m.needs_update)
            .map(|m| m.name.clone())
            .collect()
    };
    if targets.is_empty() {
        return RemoteSyncSummary::default();
    }

    MACHINES_UPDATING.store(true, Ordering::SeqCst);
    {
        let mut guard = MACHINES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for m in guard.iter_mut() {
            if targets.iter().any(|t| t == &m.name) {
                m.updating = true;
                m.error = None;
            }
        }
    }
    crate::mods::request(crate::mods::Request::Refresh);

    let results: Vec<(String, Result<String, String>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = targets
            .iter()
            .map(|name| {
                let name = name.clone();
                scope.spawn(move || {
                    let res = crate::core_bin::update_machine(&name);
                    {
                        let mut guard = MACHINES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        if let Some(m) = guard.iter_mut().find(|m| m.name == name) {
                            m.updating = false;
                            match &res {
                                Ok(now) => {
                                    let ver = clean_core_version(now).to_owned();
                                    if !ver.is_empty() {
                                        m.version = Some(ver);
                                    }
                                    m.state = "connected".into();
                                    m.raw_state = "connected".into();
                                    m.why = None;
                                    m.ready = None;
                                    m.needs_update = false;
                                    m.unreachable = false;
                                    m.error = None;
                                }
                                Err(err) => {
                                    m.state = "needs_update".into();
                                    m.needs_update = true;
                                    m.error = Some(err.clone());
                                }
                            }
                        }
                    }
                    crate::mods::request(crate::mods::Request::Refresh);
                    (name, res)
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });

    let mut summary = RemoteSyncSummary::default();
    for (name, res) in results {
        match res {
            Ok(now) => {
                let ver = clean_core_version(&now).to_owned();
                summary.updated.push((name, ver));
            }
            Err(err) => {
                tracing::warn!("update: remote machine {name} core sync failed: {err}");
                summary.failed.push((name, err));
            }
        }
    }

    MACHINES_UPDATING.store(false, Ordering::SeqCst);
    refresh_machines();
    // Preserve fresh sync errors on failed machines so Settings -> Updates can surface them.
    if !summary.failed.is_empty() {
        let mut guard = MACHINES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for (name, err) in &summary.failed {
            if let Some(m) = guard.iter_mut().find(|m| &m.name == name) {
                m.state = "needs_update".into();
                m.needs_update = true;
                m.error = Some(err.clone());
            }
        }
    }
    crate::mods::request(crate::mods::Request::Refresh);
    summary
}

fn format_remote_sync_toast(summary: &RemoteSyncSummary, local: Option<(&str, &str)>) -> Option<(String, String, String)> {
    let ok_list = summary
        .updated
        .iter()
        .map(|(n, v)| if v.is_empty() { n.clone() } else { format!("{n} (v{v})") })
        .collect::<Vec<_>>()
        .join(", ");
    let fail_list = summary
        .failed
        .iter()
        .map(|(n, e)| format!("{n}: {e}"))
        .collect::<Vec<_>>()
        .join("; ");

    if let Some((title, base_body)) = local {
        let mut parts = vec![base_body.to_owned()];
        if !ok_list.is_empty() {
            parts.push(format!("Synced remote: {ok_list}."));
        }
        if !fail_list.is_empty() {
            parts.push(format!("Remote needs_update: {fail_list}."));
        }
        let level = if summary.failed.is_empty() { "info" } else { "error" };
        return Some((title.to_owned(), parts.join(" "), level.to_owned()));
    }

    if summary.updated.is_empty() && summary.failed.is_empty() {
        return None;
    }
    if summary.failed.is_empty() {
        Some((
            "Remote herdr2-core updated".into(),
            format!("Synced {ok_list}. Running terminals stayed alive."),
            "info".into(),
        ))
    } else if summary.updated.is_empty() {
        Some((
            "Remote core sync failed".into(),
            format!("needs_update: {fail_list}"),
            "error".into(),
        ))
    } else {
        Some((
            "Remote core sync partially completed".into(),
            format!("Synced: {ok_list}. Failed (needs_update): {fail_list}"),
            "error".into(),
        ))
    }
}

fn emit_remote_sync_toast(summary: &RemoteSyncSummary, local: Option<(&str, &str)>) {
    if let Some((title, body, level)) = format_remote_sync_toast(summary, local) {
        crate::mods::request(crate::mods::Request::UpdateToast { title, body, level });
    }
}

/// Syncs outdated / needs_update SSH remote machines in the background.
#[allow(dead_code)]
pub fn update_remotes() {
    if MACHINES_UPDATING.load(Ordering::Relaxed) {
        return;
    }
    let _ = std::thread::Builder::new().name("herdr2-remote-update".into()).spawn(|| {
        let summary = sync_remote_machines_blocking(false);
        emit_remote_sync_toast(&summary, None);
    });
}

fn sync_installed_skills_best_effort() -> Option<crate::core_bin::SkillSyncSummary> {
    match crate::core_bin::sync_skills() {
        Ok(summary) => {
            if summary.updated_count > 0 {
                tracing::info!(
                    "update: synced {} installed skill file(s) ({})",
                    summary.updated_count,
                    summary.targets.join(", ")
                );
            }
            Some(summary)
        }
        Err(e) => {
            tracing::debug!("update: installed skill sync skipped/failed: {e}");
            None
        }
    }
}

#[cfg(unix)]
fn update_elevated_linux_core_blocking() -> Result<String, String> {
    let base_url = releases().ok_or_else(|| "automatic core updates require releases_url".to_owned())?;
    let active = crate::core_bin::path()
        .canonicalize()
        .map_err(|e| format!("cannot locate active herdr2-core binary: {e}"))?;
    let current_ver = crate::core_bin::version(&active)
        .map(|v| clean_core_version(&v).to_owned())
        .filter(|v| !v.is_empty())
        .or_else(|| core().version);
    let work = TempWorkDir::new("herdr2-core-update")?;
    let Some((new_bin, new_ver)) = download_verified_core(&base_url, work.path(), current_ver.as_deref())? else {
        // Binary on disk already matches or exceeds latest release; restart the running daemon.
        crate::core_bin::restart()?;
        return Ok(current_ver.unwrap_or_else(|| "the latest".into()));
    };
    let mut targets = vec![active.clone()];
    if let Some(dir) = active.parent() {
        for sibling_name in ["herdr2-core", "h2"] {
            let sibling = dir.join(sibling_name);
            if sibling != active
                && sibling.symlink_metadata().is_ok_and(|m| m.file_type().is_file())
            {
                targets.push(sibling);
            }
        }
    }
    run_elevated_core_swap_with(
        Path::new("pkexec"),
        &["--disable-internal-agent"],
        work.path(),
        &new_bin,
        &targets,
        &new_ver,
    )?;
    crate::core_bin::restart()?;
    Ok(new_ver)
}

#[cfg(not(unix))]
fn update_elevated_linux_core_blocking() -> Result<String, String> {
    Err("elevated core update is only supported on Linux".into())
}

#[cfg(target_os = "macos")]
fn update_macos_bundled_core_blocking() -> Result<String, String> {
    let base_url = releases().ok_or_else(|| "automatic core updates require releases_url".to_owned())?;
    let install_root = root().ok_or_else(|| "cannot locate Herdr2.app bundle".to_owned())?;
    let helper = install_root.join("Contents/Helpers/h2");
    if !helper.is_file() {
        return crate::core_bin::update();
    }
    let signature = Command::new("/usr/bin/codesign")
        .arg("-dvv")
        .arg(&install_root)
        .output()
        .map_err(|e| format!("codesign: {e}"))?;
    if release_signature(signature.status.success(), &String::from_utf8_lossy(&signature.stderr))? {
        return Err(
            "Herdr2.app is Developer ID signed; reinstall the latest macOS DMG or use AI Update to refresh its bundled h2 helper."
                .into(),
        );
    }
    let current_ver = crate::core_bin::version(&helper)
        .map(|v| clean_core_version(&v).to_owned())
        .filter(|v| !v.is_empty())
        .or_else(|| core().version);
    let work = TempWorkDir::new("herdr2-macos-core-update")?;
    let Some((new_bin, new_ver)) = download_verified_core(&base_url, work.path(), current_ver.as_deref())? else {
        crate::core_bin::restart()?;
        return Ok(current_ver.unwrap_or_else(|| "the latest".into()));
    };
    let tmp = helper.with_file_name(format!(".h2.new.{}", std::process::id()));
    copy_executable(&new_bin, &tmp)?;
    if let Err(e) = std::fs::rename(&tmp, &helper) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot replace {}: {e}", helper.display()));
    }
    run(Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(&helper))?;
    run(Command::new("/usr/bin/codesign").args(["--force", "--deep", "--sign", "-"]).arg(&install_root))?;
    crate::core_bin::restart()?;
    Ok(new_ver)
}

#[cfg(not(target_os = "macos"))]
fn update_macos_bundled_core_blocking() -> Result<String, String> {
    crate::core_bin::update()
}

fn run_core_update_blocking() {
    let (snapshot, strategy) = {
        let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !core_self_update_capabilities(&c).0 || c.updating {
            return;
        }
        let install_root = root();
        let active_core = crate::core_bin::path();
        let strategy = core_update_strategy_for(
            &c,
            install_root.as_deref(),
            &active_core,
            cfg!(target_os = "linux"),
            cfg!(target_os = "macos"),
        );
        c.updating = true;
        c.error = None;
        (c.clone(), strategy)
    };
    crate::mods::request(crate::mods::Request::Refresh);

    let disk_ver = refresh_disk_core_version();
    let res = if let Some(disk_ready) = disk_core_covers_ready(&snapshot, disk_ver.as_deref()) {
        crate::core_bin::restart().map(|()| disk_ready)
    } else {
        match strategy {
            CoreUpdateStrategy::Direct => crate::core_bin::update(),
            CoreUpdateStrategy::ElevatedLinuxBundled => update_elevated_linux_core_blocking(),
            CoreUpdateStrategy::MacosBundled => update_macos_bundled_core_blocking(),
            CoreUpdateStrategy::ExternalManaged => Err("herdr2-core is managed by an external package manager".into()),
        }
    };
    let _ = refresh_disk_core_version();
    let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    c.updating = false;
    match res {
        Ok(now) => {
            let ver = clean_core_version(&now).to_owned();
            c.version = Some(ver.clone());
            c.ready = None;
            c.notes = None;
            c.error = None;
            drop(c);
            let skill_summary = sync_installed_skills_best_effort();
            reclassify_machines();
            let remote_summary = sync_remote_machines_blocking(false);
            let base_body = match skill_summary.as_ref().filter(|s| s.updated_count > 0) {
                Some(s) => format!("Running terminals stayed alive. Synced {} installed skill(s).", s.updated_count),
                None => "Running terminals stayed alive.".to_owned(),
            };
            emit_remote_sync_toast(
                &remote_summary,
                Some((&format!("herdr2-core updated to v{ver}"), &base_body)),
            );
        }
        Err(e) => {
            c.error = Some(e.clone());
            drop(c);
            crate::mods::request(crate::mods::Request::UpdateToast {
                title: "Core update failed".into(),
                body: e,
                level: "error".into(),
            });
        }
    }
    crate::mods::request(crate::mods::Request::Refresh);
}

/// Upgrades local `h2` and all SSH remote machines' `herdr2-core` in the background (keeping running terminals alive).
pub fn update_core() {
    {
        let c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !core_self_update_capabilities(&c).0 || c.updating {
            return;
        }
    }
    let _ = std::thread::Builder::new().name("herdr2-core-update".into()).spawn(run_core_update_blocking);
}

fn run_ui_update_blocking(releases_base: Option<&str>, refresh_first: bool) -> bool {
    let app_dir = crate::mods::app_dir();
    match crate::ui_update::download_and_stage_from(&app_dir, releases_base, refresh_first) {
        Ok(_) => {
            if !core().updating {
                let _ = sync_installed_skills_best_effort();
            }
            crate::mods::request(crate::mods::Request::ApplyUiBundle);
            true
        }
        Err(e) => {
            crate::ui_update::set_error(Some(e.clone()));
            crate::mods::request(crate::mods::Request::UpdateToast {
                title: "UI update failed".into(),
                body: e,
                level: "error".into(),
            });
            crate::mods::request(crate::mods::Request::Refresh);
            false
        }
    }
}

/// Downloads and applies the available UI bundle in the background without restarting.
pub fn start_ui_update() {
    if crate::ui_update::is_applying() {
        return;
    }
    crate::ui_update::set_applying(true);
    crate::mods::request(crate::mods::Request::Refresh);
    let _ = std::thread::Builder::new().name("herdr2-ui-update".into()).spawn(|| {
        let base = releases();
        run_ui_update_blocking(base.as_deref(), base.is_some());
    });
}

fn run_app_update_blocking(install_root: PathBuf, already_staged: Option<String>, avail: Option<AppAvailable>) {
    APP_UPDATING.store(true, Ordering::SeqCst);
    *APP_ERROR.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    crate::mods::request(crate::mods::Request::Refresh);

    let elevated = needs_elevation_for(&install_root);
    let stage_res: Result<(String, bool, Option<String>), String> = if elevated {
        match avail {
            Some(a) => download_and_apply_elevated_linux(&install_root, &a)
                .map(|(ver, core_ver)| (ver, true, core_ver)),
            None => Err("no app update available".into()),
        }
    } else if let Some(ver) = already_staged {
        *READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ver.clone());
        Ok((ver, false, None))
    } else if let Some(a) = avail {
        download_and_stage_app(&install_root, &a).map(|v| (v, false, None))
    } else {
        Err("no app update available".into())
    };
    match stage_res {
        Ok((_, already_swapped, bundled_core_ver)) => {
            let active_in_root = crate::core_bin::path()
                .canonicalize()
                .ok()
                .is_some_and(|p| p.starts_with(&install_root));
            if let Some(new_core_ver) = bundled_core_ver {
                if active_in_root {
                    match crate::core_bin::restart() {
                        Ok(()) => {
                            let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            c.version = Some(new_core_ver);
                            c.ready = None;
                            c.notes = None;
                            c.error = None;
                        }
                        Err(e) => {
                            tracing::warn!("update: core restart after elevated swap failed: {e}");
                        }
                    }
                }
            }
            // Also update external h2 core in lockstep if a non-managed core update is waiting.
            // Skip when the active core is bundled inside `install_root` and waiting for
            // post-restart swap (`after_update()` restarts the swapped bundled core on startup).
            let should_update_core = if active_in_root && !already_swapped {
                false
            } else {
                let c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                c.managed.is_none() && c.ready.is_some() && !c.updating
            };
            if should_update_core {
                if let Ok(now) = crate::core_bin::update() {
                    let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    c.version = Some(clean_core_version(&now).to_owned());
                    c.ready = None;
                    c.notes = None;
                    c.error = None;
                }
            }
            reclassify_machines();
            // Sync already-installed local skills and all SSH remote machines now when
            // local core is already swapped on disk. (If local core is bundled inside
            // `install_root` and waiting for post-restart swap, `after_update()` will
            // restart the new bundled core, sync installed skills, and sync all remotes.)
            if already_swapped || !active_in_root {
                let _ = sync_installed_skills_best_effort();
                let remote_summary = sync_remote_machines_blocking(false);
                if !remote_summary.failed.is_empty() {
                    emit_remote_sync_toast(&remote_summary, None);
                }
            }
            if already_swapped {
                ELEVATED_APPLIED.store(true, Ordering::SeqCst);
            }
            crate::mods::request(crate::mods::Request::ApplyAppUpdate);
        }
        Err(e) => {
            APP_UPDATING.store(false, Ordering::SeqCst);
            *APP_ERROR.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e.clone());
            crate::mods::request(crate::mods::Request::UpdateToast {
                title: "Update failed".into(),
                body: e,
                level: "error".into(),
            });
            crate::mods::request(crate::mods::Request::Refresh);
        }
    }
}

/// Refreshes App manifest right before applying an update so clicking the top-right
/// Update button never uses a stale hourly SHA-256 against rolling `ota-latest` assets.
fn refresh_app_manifest_for_root(install_root: &Path, base_url: &str) -> Result<Option<AppAvailable>, String> {
    let avail = check_app_manifest_from(install_root, base_url)?;
    if let Some(ref a) = avail
        && staged_version(install_root).as_deref() == Some(a.version.as_str())
    {
        *READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(a.version.clone());
    } else if avail.is_none() {
        *READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
    *APP_AVAILABLE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = avail.clone();
    *APP_ERROR.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    Ok(avail)
}

fn run_start_update_blocking() {
    let base_url = releases();
    let install_root_opt = checks_app();
    let had_app_ready = ready().is_some();
    let had_ui_ready = crate::ui_update::ready().is_some();
    let had_core_ready = {
        let c = core();
        core_self_update_capabilities(&c).0 && c.ready.is_some()
    };

    let mut app_refresh_err: Option<String> = None;
    let mut ui_refresh_err: Option<String> = None;
    let mut core_refresh_err: Option<String> = None;

    // Always re-fetch live manifests before downloading so rolling `ota-latest` tarballs
    // (or newly published releases superseding an older UI/Core update) match the latest SHA-256.
    if let (Some(install_root), Some(base)) = (install_root_opt.as_deref(), base_url.as_deref()) {
        if let Err(e) = refresh_app_manifest_for_root(install_root, base)
            && !is_missing_feed_err(&e)
        {
            tracing::warn!("update: pre-update app manifest refresh failed: {e}");
            app_refresh_err = Some(e);
        }
    }

    // 1. App update (staged or available)
    if let Some(install_root) = install_root_opt {
        let avail = if app_refresh_err.is_some() {
            // Avoid downloading against a potentially stale cached SHA-256 when manifest refresh failed.
            None
        } else {
            APP_AVAILABLE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
        };
        let already_staged = staged_version(&install_root)
            .filter(|v| newer(v, env!("CARGO_PKG_VERSION")))
            .filter(|v| avail.as_ref().is_none_or(|a| !newer(&a.version, v)));

        if already_staged.is_some() || avail.is_some() {
            crate::ui_update::clear_available();
            if checks_core()
                && let Some(base) = base_url.as_deref()
            {
                let _ = check_core_from(base);
            }
            run_app_update_blocking(install_root, already_staged, avail);
            return;
        }
        if let Some(err) = app_refresh_err.take()
            && had_app_ready
        {
            *APP_ERROR.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(err.clone());
            crate::mods::request(crate::mods::Request::UpdateToast {
                title: "Update failed".into(),
                body: err,
                level: "error".into(),
            });
            return;
        }
    }

    if checks_ui()
        && let Some(base) = base_url.as_deref()
    {
        let app_dir = crate::mods::app_dir();
        if let Err(e) = crate::ui_update::check_manifest_from(&app_dir, base)
            && !is_missing_feed_err(&e)
        {
            tracing::warn!("update: pre-update ui manifest refresh failed: {e}");
            ui_refresh_err = Some(e);
        }
    }

    if checks_core()
        && let Some(base) = base_url.as_deref()
        && let Err(e) = check_core_from(base)
        && !is_missing_feed_err(&e)
    {
        tracing::warn!("update: pre-update core manifest refresh failed: {e}");
        core_refresh_err = Some(e);
    }

    // 2. UI bundle hot-update (also sync local core / SSH remotes if waiting)
    if crate::ui_update::ready().is_some() && ui_refresh_err.is_none() {
        crate::ui_update::set_applying(true);
        crate::mods::request(crate::mods::Request::Refresh);
        let _ = run_ui_update_blocking(base_url.as_deref(), false);
        let c = core();
        if core_self_update_capabilities(&c).0 && c.ready.is_some() {
            run_core_update_blocking();
        } else if has_pending_remote_updates() {
            let summary = sync_remote_machines_blocking(false);
            emit_remote_sync_toast(&summary, None);
        }
        return;
    }

    // 3. Local Core update (automatically syncs all SSH remotes afterward)
    let c = core();
    if core_self_update_capabilities(&c).0 && c.ready.is_some() {
        run_core_update_blocking();
        return;
    }

    // 4. Remote-only SSH core sync
    if has_pending_remote_updates() {
        let summary = sync_remote_machines_blocking(false);
        emit_remote_sync_toast(&summary, None);
        return;
    }

    // Surface refresh failure if the user clicked Update on a previously advertised UI/Core update while offline.
    if let Some(err) = ui_refresh_err.filter(|_| had_ui_ready) {
        crate::ui_update::set_error(Some(err.clone()));
        crate::mods::request(crate::mods::Request::UpdateToast {
            title: "UI update failed".into(),
            body: err,
            level: "error".into(),
        });
    } else if let Some(err) = core_refresh_err.filter(|_| had_core_ready) {
        CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).error = Some(err.clone());
        crate::mods::request(crate::mods::Request::UpdateToast {
            title: "Core update failed".into(),
            body: err,
            level: "error".into(),
        });
    }
}

/// Unified one-click background updater: refreshes live manifests, downloads, verifies,
/// and applies whichever update channel (App, UI bundle, local Core, and all SSH remote Cores)
/// is available, keeping UI responsive with a spinner.
pub fn start_update() {
    if is_updating() {
        return;
    }
    if UPDATING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    crate::mods::request(crate::mods::Request::Refresh);
    let _ = std::thread::Builder::new().name("herdr2-start-update".into()).spawn(|| {
        let _guard = UpdatingGuard;
        run_start_update_blocking();
    });
}

pub fn clear_app_updating() {
    APP_UPDATING.store(false, Ordering::SeqCst);
}

/// Checks `app/latest.json` metadata only (no tarball download yet).
fn check_app_manifest_from(root: &Path, releases: &str) -> Result<Option<AppAvailable>, String> {
    let url = feed_url(releases, "app");
    let text = curl_text(&url)?;
    let manifest: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("app/latest.json: {e}"))?;
    let version = manifest["version"].as_str().ok_or("app/latest.json: no version".to_owned())?;
    if !newer(version, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let notes = manifest["notes"].as_str().map(str::to_owned);
    if staged_version(root).as_deref() == Some(version) {
        return Ok(Some(AppAvailable {
            version: version.to_owned(),
            url: String::new(),
            sha256: String::new(),
            notes,
        }));
    }
    let entry = &manifest["targets"][target()];
    let (Some(dl_url), Some(sha)) = (entry["url"].as_str(), entry["sha256"].as_str()) else {
        return Ok(None);
    };
    Ok(Some(AppAvailable {
        version: version.to_owned(),
        url: dl_url.to_owned(),
        sha256: sha.to_owned(),
        notes,
    }))
}

fn copy_executable(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::copy(src, dst).map_err(|e| format!("copy {} -> {}: {e}", src.display(), dst.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

fn download_verified_core_once(
    releases_base: &str,
    work: &Path,
    current_ver: Option<&str>,
) -> Result<Option<(PathBuf, String)>, String> {
    let latest = latest_core_from(releases_base)?;
    if latest.generation != Some(crate::hc::GENERATION) {
        return Ok(None);
    }
    if let Some(have) = current_ver
        && !newer(&latest.version, have)
    {
        return Ok(None);
    }
    let entry = &latest.targets[target()];
    let (Some(dl_url), Some(sha)) = (entry["url"].as_str(), entry["sha256"].as_str()) else {
        return Ok(None);
    };
    let core_work = work.join("core-dl");
    let _ = std::fs::remove_dir_all(&core_work);
    std::fs::create_dir_all(&core_work).map_err(|e| format!("{}: {e}", core_work.display()))?;
    let ext = if dl_url.ends_with(".zip") { "zip" } else { "tar.gz" };
    let archive = core_work.join(format!("core.{ext}"));
    download_file(dl_url, &archive)?;
    let bytes = std::fs::read(&archive).map_err(|e| e.to_string())?;
    if sha256(&bytes) != sha.to_ascii_lowercase() {
        return Err("core download checksum mismatch".into());
    }
    let unpacked = core_work.join("unpacked");
    std::fs::create_dir_all(&unpacked).map_err(|e| e.to_string())?;
    unpack_archive(&archive, &unpacked)?;
    let bin = unpacked.join(format!("herdr2{}", std::env::consts::EXE_SUFFIX));
    if !bin.is_file() {
        return Err("core download archive holds no herdr2 binary".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755));
    }
    let reported = crate::core_bin::version(&bin)
        .ok_or_else(|| format!("downloaded core binary does not run as h2 {}", latest.version))?;
    let reported_ver = strip_core_prefix(&reported).split_whitespace().next().unwrap_or("");
    if reported_ver != latest.version {
        return Err(format!("downloaded core reported {reported}, expected {}", latest.version));
    }
    Ok(Some((bin, latest.version)))
}

/// Downloads and verifies the latest same-generation `h2` core binary into `work/core-dl`
/// when it is newer than `current_ver`. Retries once if a rolling `ota-latest` upload causes
/// a transient checksum mismatch.
fn download_verified_core(
    releases_base: &str,
    work: &Path,
    current_ver: Option<&str>,
) -> Result<Option<(PathBuf, String)>, String> {
    match download_verified_core_once(releases_base, work, current_ver) {
        Err(e) if e.contains("checksum mismatch") => {
            tracing::warn!("update: bundled core checksum mismatch; re-fetching core manifest and retrying once");
            std::thread::sleep(Duration::from_secs(1));
            download_verified_core_once(releases_base, work, current_ver)
        }
        other => other,
    }
}

/// Preserves bundled files (`Contents/Helpers/h2` on macOS, `herdr2-core`/`h2` and extra PNG icons
/// on Linux) when replacing an install folder or `.app` bundle. If a newer same-generation core
/// release is available from `releases_base`, upgrades the bundled core binary in lockstep;
/// otherwise copies the existing one so symlinks (such as `~/.local/bin/h2` or `/usr/bin/h2`) and
/// the desktop app never lose their embedded `h2`.
fn preserve_or_update_bundled_core(
    root: &Path,
    unpacked: &Path,
    work: &Path,
    releases_base: Option<&str>,
) -> Result<Option<String>, String> {
    preserve_or_update_bundled_core_inner(root, unpacked, work, releases_base, cfg!(target_os = "macos"))
}

fn preserve_or_update_bundled_core_inner(
    root: &Path,
    unpacked: &Path,
    work: &Path,
    releases_base: Option<&str>,
    resign_macos: bool,
) -> Result<Option<String>, String> {
    for asset in ["herdr2.png", "herdr2-512.png", "herdr2.ico"] {
        let src = root.join(asset);
        let dst = unpacked.join(asset);
        if src.is_file() && !dst.exists() {
            let _ = std::fs::copy(&src, &dst);
        }
    }

    // macOS `.app` bundle layout: `Herdr2.app/Contents/Helpers/h2`.
    let existing_helper = root.join("Contents/Helpers/h2");
    let unpacked_helper = unpacked.join("Contents/Helpers/h2");
    if unpacked_helper.is_file() {
        // The downloaded OTA bundle already ships a signed `Contents/Helpers/h2`: never mutate it
        // (which would invalidate a Developer ID signature).
        let old_ver = existing_helper
            .is_file()
            .then(|| crate::core_bin::version(&existing_helper))
            .flatten()
            .map(|v| strip_core_prefix(&v).split_whitespace().next().unwrap_or("").to_owned())
            .filter(|v| !v.is_empty());
        let new_ver = crate::core_bin::version(&unpacked_helper)
            .map(|v| strip_core_prefix(&v).split_whitespace().next().unwrap_or("").to_owned())
            .filter(|v| !v.is_empty());
        return Ok(match (new_ver, old_ver) {
            (Some(new_v), Some(old_v)) if newer(&new_v, &old_v) => Some(new_v),
            (Some(new_v), None) => Some(new_v),
            _ => None,
        });
    }
    if existing_helper.is_file() {
        let current_ver = crate::core_bin::version(&existing_helper)
            .map(|v| strip_core_prefix(&v).split_whitespace().next().unwrap_or("").to_owned())
            .filter(|v| !v.is_empty())
            .or_else(|| core().version);
        if let Some(parent) = unpacked_helper.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut upgraded = None;
        if let Some(base) = releases_base {
            match download_verified_core(base, work, current_ver.as_deref()) {
                Ok(Some((new_bin, new_ver))) => {
                    copy_executable(&new_bin, &unpacked_helper)?;
                    upgraded = Some(new_ver);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!("update: could not download bundled macOS helper core ({e}); preserving existing h2");
                }
            }
        }
        if !unpacked_helper.is_file() {
            copy_executable(&existing_helper, &unpacked_helper)?;
        }
        if resign_macos {
            run(Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(&unpacked_helper))?;
            run(Command::new("/usr/bin/codesign").args(["--force", "--deep", "--sign", "-"]).arg(unpacked))?;
        }
        return Ok(upgraded);
    }

    let existing_core = root.join("herdr2-core");
    let existing_h2 = root.join("h2");
    let has_core = existing_core.is_file();
    let has_h2 = existing_h2.is_file();
    if !has_core && !has_h2 {
        return Ok(None);
    }

    let current_ver = (if has_core {
        crate::core_bin::version(&existing_core)
    } else {
        crate::core_bin::version(&existing_h2)
    })
    .map(|v| strip_core_prefix(&v).split_whitespace().next().unwrap_or("").to_owned())
    .filter(|v| !v.is_empty())
    .or_else(|| core().version);

    if let Some(base) = releases_base {
        match download_verified_core(base, work, current_ver.as_deref()) {
            Ok(Some((new_bin, new_ver))) => {
                if has_core {
                    copy_executable(&new_bin, &unpacked.join("herdr2-core"))?;
                }
                if has_h2 && !unpacked.join("h2").exists() {
                    copy_executable(&new_bin, &unpacked.join("h2"))?;
                }
                return Ok(Some(new_ver));
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("update: could not download bundled core update ({e}); preserving existing core");
            }
        }
    }

    if has_core && !unpacked.join("herdr2-core").exists() {
        copy_executable(&existing_core, &unpacked.join("herdr2-core"))?;
    }
    if has_h2 && !unpacked.join("h2").exists() {
        copy_executable(&existing_h2, &unpacked.join("h2"))?;
    }
    Ok(None)
}

#[cfg(unix)]
struct TempWorkDir(PathBuf);

#[cfg(unix)]
impl TempWorkDir {
    fn new(prefix: &str) -> Result<Self, String> {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(unix)]
impl Drop for TempWorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
const ELEVATED_CORE_SWAP_SCRIPT: &str = r#"#!/bin/sh
set -eu
src="$1"
shift
if [ "$#" -lt 1 ]; then
  echo "missing target core path" >&2
  exit 1
fi
for dst in "$@"; do
  if [ -z "$dst" ] || [ "$dst" = "/" ]; then
    echo "refusing unsafe core path: $dst" >&2
    exit 1
  fi
  tmp="${dst}.new.$$"
  rm -f "$tmp"
  cp "$src" "$tmp"
  chown --reference="$dst" "$tmp" 2>/dev/null || chown 0:0 "$tmp"
  chmod 755 "$tmp"
  if ! "$tmp" --version >/dev/null 2>&1; then
    rm -f "$tmp"
    echo "staged core binary failed verification: $dst" >&2
    exit 1
  fi
  mv -f "$tmp" "$dst"
done
"#;

#[cfg(unix)]
fn run_elevated_core_swap_with(
    elevator: &Path,
    extra_args: &[&str],
    work: &Path,
    new_bin: &Path,
    targets: &[PathBuf],
    version: &str,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let first_target = targets.first().ok_or_else(|| "no core binary target to update".to_owned())?;
    let script_path = work.join("apply-core-update.sh");
    std::fs::write(&script_path, ELEVATED_CORE_SWAP_SCRIPT)
        .map_err(|e| format!("{}: {e}", script_path.display()))?;
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {e}", script_path.display()))?;

    let mut cmd = Command::new(elevator);
    cmd.args(extra_args)
        .arg("/bin/sh")
        .arg(&script_path)
        .arg(new_bin);
    for dst in targets {
        cmd.arg(dst);
    }
    let out = cmd.stdin(Stdio::null()).output();

    match out {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let code = output.status.code();
            let stderr = decode_process_output(&output.stderr).trim().to_owned();
            let detail = if stderr.is_empty() {
                decode_process_output(&output.stdout).trim().to_owned()
            } else {
                stderr
            };
            match code {
                Some(126) => Err("Administrator authentication was cancelled; core update aborted.".into()),
                Some(127) => {
                    let suffix = if detail.is_empty() { String::new() } else { format!(" ({detail})") };
                    Err(format!(
                        "Administrator privileges are required to update {}{suffix}. Install the new .deb with `sudo apt install ./herdr2_{version}_amd64.deb` or run `sudo h2 update`.",
                        first_target.display()
                    ))
                }
                _ => Err(if detail.is_empty() {
                    format!("Elevated core update failed for {} (exit code {:?})", first_target.display(), code)
                } else {
                    format!("Elevated core update failed for {}: {detail}", first_target.display())
                }),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "Administrator privileges are required to update {}, but pkexec is not installed. Install the new .deb with `sudo apt install ./herdr2_{version}_amd64.deb` or run `sudo h2 update`.",
            first_target.display()
        )),
        Err(e) => Err(format!("Cannot launch {}: {e}", elevator.display())),
    }
}

#[cfg(unix)]
const ELEVATED_SWAP_SCRIPT: &str = r#"#!/bin/sh
set -eu
src="$1"
dst="$2"
if [ -z "$dst" ] || [ "$dst" = "/" ]; then
  echo "refusing unsafe install path: $dst" >&2
  exit 1
fi
new="${dst}.new"
old="${dst}.old"
rm -rf "$new" "$old"
mkdir -p "$new"
cp -a "$src/." "$new/"
chown -R --reference="$dst" "$new" 2>/dev/null || chown -R 0:0 "$new"
chmod 755 "$new"
if [ -f "$new/herdr2" ]; then chmod 755 "$new/herdr2"; fi
if [ -f "$new/herdr2-core" ]; then chmod 755 "$new/herdr2-core"; fi
if [ -f "$new/h2" ]; then chmod 755 "$new/h2"; fi
mv "$dst" "$old"
if ! mv "$new" "$dst"; then
  mv "$old" "$dst" || true
  rm -rf "$new"
  exit 1
fi
rm -rf "$old"
"#;

#[cfg(unix)]
fn run_elevated_swap_with(
    elevator: &Path,
    extra_args: &[&str],
    work: &Path,
    unpacked: &Path,
    install_root: &Path,
    version: &str,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let script_path = work.join("apply-update.sh");
    std::fs::write(&script_path, ELEVATED_SWAP_SCRIPT)
        .map_err(|e| format!("{}: {e}", script_path.display()))?;
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {e}", script_path.display()))?;

    let out = Command::new(elevator)
        .args(extra_args)
        .arg("/bin/sh")
        .arg(&script_path)
        .arg(unpacked)
        .arg(install_root)
        .stdin(Stdio::null())
        .output();

    match out {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let code = output.status.code();
            let stderr = decode_process_output(&output.stderr).trim().to_owned();
            let detail = if stderr.is_empty() {
                decode_process_output(&output.stdout).trim().to_owned()
            } else {
                stderr
            };
            match code {
                Some(126) => Err("Administrator authentication was cancelled; update aborted.".into()),
                Some(127) => {
                    let suffix = if detail.is_empty() { String::new() } else { format!(" ({detail})") };
                    Err(format!(
                        "Administrator privileges are required to update {}{suffix}. Install the new .deb with `sudo apt install ./herdr2_{version}_amd64.deb` or switch to ~/.local via app-install.sh.",
                        install_root.display()
                    ))
                }
                _ => Err(if detail.is_empty() {
                    format!("Elevated update failed for {} (exit code {:?})", install_root.display(), code)
                } else {
                    format!("Elevated update failed for {}: {detail}", install_root.display())
                }),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "Administrator privileges are required to update {}, but pkexec is not installed. Install the new .deb with `sudo apt install ./herdr2_{version}_amd64.deb` or switch to ~/.local via app-install.sh.",
            install_root.display()
        )),
        Err(e) => Err(format!("Cannot launch {}: {e}", elevator.display())),
    }
}

/// Unpacks and verifies an app archive, retrying once with a freshly fetched `app-latest.json`
/// if a rolling `ota-latest` release clobbered the archive mid-download (`checksum mismatch`).
fn unpack_app_with_retry(
    root: &Path,
    avail: &AppAvailable,
    work: &Path,
    releases_base: Option<&str>,
) -> Result<(PathBuf, String), String> {
    let try_once = |cand: &AppAvailable, dir: &Path| -> Result<(PathBuf, String), String> {
        let unpacked = unpack(&cand.url, &cand.sha256, dir)?;
        if version_of(&unpacked).as_deref() != Some(cand.version.as_str()) {
            return Err(format!("the download doesn't run as {}", cand.version));
        }
        Ok((unpacked, cand.version.clone()))
    };

    match try_once(avail, work) {
        Ok(ok) => Ok(ok),
        Err(err) if err.contains("checksum mismatch") => {
            let Some(base) = releases_base else {
                return Err(err);
            };
            tracing::warn!(
                "update: checksum mismatch downloading Herdr {}; refreshing manifest from {} and retrying once",
                avail.version,
                base
            );
            std::thread::sleep(Duration::from_secs(1));
            let Ok(Some(fresh)) = refresh_app_manifest_for_root(root, base) else {
                return Err(err);
            };
            if fresh.url.is_empty() {
                return Err(err);
            }
            let _ = std::fs::remove_dir_all(work);
            std::fs::create_dir_all(work).map_err(|e| format!("{}: {e}", work.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(work, std::fs::Permissions::from_mode(0o700));
            }
            try_once(&fresh, work)
        }
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
fn download_and_apply_elevated_linux(root: &Path, avail: &AppAvailable) -> Result<(String, Option<String>), String> {
    tracing::info!("downloading Herdr {} for elevated swap into {}", avail.version, root.display());
    let work = TempWorkDir::new("herdr2-app-update")?;
    let releases_base = releases();
    let (unpacked, version) = unpack_app_with_retry(root, avail, work.path(), releases_base.as_deref())?;
    let updated_core_ver = preserve_or_update_bundled_core(root, &unpacked, work.path(), releases_base.as_deref())?;
    run_elevated_swap_with(
        Path::new("pkexec"),
        &["--disable-internal-agent"],
        work.path(),
        &unpacked,
        root,
        &version,
    )?;
    *READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    *APP_AVAILABLE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    let _ = std::fs::write(marker(), "");
    Ok((version, updated_core_ver))
}

#[cfg(not(unix))]
fn download_and_apply_elevated_linux(_root: &Path, _avail: &AppAvailable) -> Result<(String, Option<String>), String> {
    Err("elevated update is only supported on Linux".into())
}

fn download_and_stage_app(root: &Path, avail: &AppAvailable) -> Result<String, String> {
    download_and_stage_app_from(root, avail, releases().as_deref())
}

fn download_and_stage_app_from(
    root: &Path,
    avail: &AppAvailable,
    releases_base: Option<&str>,
) -> Result<String, String> {
    let staged = beside(root, ".new");
    if staged_version(root).as_deref() == Some(avail.version.as_str()) {
        *READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(avail.version.clone());
        return Ok(avail.version.clone());
    }
    tracing::info!("downloading Herdr {}", avail.version);
    let work = beside(root, ".download");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let result = unpack_app_with_retry(root, avail, &work, releases_base).and_then(|(unpacked, final_version)| {
        let _ = preserve_or_update_bundled_core(root, &unpacked, &work, releases_base)?;
        let _ = std::fs::remove_file(beside(root, ".new.json"));
        let _ = std::fs::remove_dir_all(&staged);
        std::fs::rename(&unpacked, &staged).map_err(|e| format!("{}: {e}", staged.display()))?;
        // Outside the bundle: writing a receipt must not change its signature.
        std::fs::write(
            beside(root, ".new.json"),
            serde_json::json!({ "version": final_version }).to_string(),
        )
        .map_err(|e| e.to_string())?;
        Ok(final_version)
    });
    let _ = std::fs::remove_dir_all(&work);
    result.map(|final_version| {
        *READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(final_version.clone());
        final_version
    })
}

/// Checks & downloads the latest release if it's newer. Ok(Some): a version is ready.
#[allow(dead_code)]
fn check(root: &Path) -> Result<Option<String>, String> {
    let base_url = releases().ok_or_else(|| "HERDR2_RELEASES_URL not configured".to_owned())?;
    check_from(root, &base_url)
}

#[allow(dead_code)]
fn check_from(root: &Path, releases: &str) -> Result<Option<String>, String> {
    let Some(avail) = check_app_manifest_from(root, releases)? else {
        return Ok(None);
    };
    download_and_stage_app(root, &avail).map(Some)
}

/// Downloads, checks and unpacks a release into `work`; returns the app inside.
fn unpack(url: &str, sha: &str, work: &Path) -> Result<PathBuf, String> {
    let ext = if url.ends_with(".zip") { "zip" } else { "tar.gz" };
    let tarball = work.join(format!("app.{ext}"));
    download_file(url, &tarball)?;
    let bytes = std::fs::read(&tarball).map_err(|e| e.to_string())?;
    let got_sha = sha256(&bytes);
    let want_sha = sha.to_ascii_lowercase();
    if got_sha != want_sha {
        tracing::warn!("update: checksum mismatch for {url}: expected {want_sha}, got {got_sha}");
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
    run(curl_command()
        .args(["-fsSL", "-H", "Cache-Control: no-cache", "--max-time", "600", "-o"])
        .arg(dest)
        .arg(url))
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
        let helper = app.join("Contents/Helpers/h2");
        if helper.is_file() {
            run(Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(&helper))?;
        }
        run(Command::new("/usr/bin/codesign").args(["--force", "--deep", "--sign", "-"]).arg(app))?;
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

fn curl_command() -> Command {
    let mut cmd = crate::spawn::command(system("curl"));
    // Windows System32 curl.exe uses Schannel, which fails with CRYPT_E_REVOCATION_OFFLINE
    // when firewalls/proxies block CRL/OCSP endpoints (still verifies TLS certs & hostnames).
    if cfg!(windows) {
        cmd.arg("--ssl-no-revoke");
    }
    cmd
}

#[cfg(windows)]
fn decode_process_output(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    const CP_ACP: u32 = 0;
    unsafe extern "system" {
        fn MultiByteToWideChar(
            code_page: u32,
            flags: u32,
            multi_byte_str: *const u8,
            cb_multi_byte: i32,
            wide_char_str: *mut u16,
            cch_wide_char: i32,
        ) -> i32;
    }
    let Ok(len) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    if len == 0 {
        return String::new();
    }
    // SAFETY: valid slice pointer/len passed to query required UTF-16 buffer length.
    let needed = unsafe { MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0u16; needed as usize];
    // SAFETY: `wide` has length `needed` matching the preceding query.
    let written = unsafe { MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), needed) };
    if written <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    String::from_utf16_lossy(&wide[..written as usize])
}

#[cfg(not(windows))]
fn decode_process_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub(crate) fn curl_text(url: &str) -> Result<String, String> {
    let out = curl_command()
        .args(["-fsSL", "-H", "Cache-Control: no-cache", "--max-time", "30", url])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("{url}: {}", decode_process_output(&out.stderr).trim()));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.stdin(Stdio::null()).output().map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{:?}: {}", cmd.get_program(), decode_process_output(&out.stderr).trim()))
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
    if cwd
        .canonicalize()
        .map(|p| strip_windows_verbatim_prefix(&p))
        .is_ok_and(|cwd| cwd.starts_with(&root))
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
    let mut cleaned = false;
    let old = beside(&root, ".old");
    if old.exists() && std::fs::remove_dir_all(&old).is_ok() {
        cleaned = true;
    }
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.contains(".old-") && entry.file_type().is_ok_and(|ft| ft.is_file()) && std::fs::remove_file(entry.path()).is_ok() {
                cleaned = true;
            }
        }
    }
    if cleaned {
        notify_shell_icon_refresh(&root);
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
    if ELEVATED_APPLIED.swap(false, Ordering::SeqCst) {
        return Ok(true);
    }
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
fn windows_relaunch_script(pid: u32, exe: &Path) -> String {
    let exe_str = strip_windows_verbatim_prefix(exe).to_string_lossy().replace('\'', "''");
    let work_dir = std::env::home_dir()
        .map(|h| strip_windows_verbatim_prefix(&h))
        .map(|h| format!(" -WorkingDirectory '{}'", h.to_string_lossy().replace('\'', "''")))
        .unwrap_or_default();
    format!("Wait-Process -Id {pid} -ErrorAction SilentlyContinue; Start-Process -FilePath '{exe_str}'{work_dir}")
}

#[cfg(windows)]
pub fn relaunch_current() -> bool {
    let exe = root()
        .map(|r| exe_in(&r))
        .filter(|p| p.is_file())
        .or_else(|| std::env::current_exe().ok());
    let Some(exe) = exe else { return false };
    let script = windows_relaunch_script(std::process::id(), &exe);
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

/// Replaces files inside `root` in-place from `staged`, renaming the active executable
/// (`herdr2.exe` -> `herdr2.exe.old-<pid>-<ts>`) so the install directory itself stays intact.
/// Preserves installer-owned files (such as Inno Setup's `unins000.exe`/`unins000.dat`) and
/// avoids momentarily removing `%LOCALAPPDATA%\Programs\herdr2` while Windows Explorer resolves icons.
fn swap_files_in_place(root: &Path, staged: &Path, exe_name: &str) -> std::io::Result<()> {
    let staged_exe = staged.join(exe_name);
    if !staged_exe.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("staged executable missing: {}", staged_exe.display()),
        ));
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let suffix = format!(".old-{}-{stamp}", std::process::id());
    let cur_exe = root.join(exe_name);
    let backup_exe = root.join(format!("{exe_name}{suffix}"));

    let moved_cur = if cur_exe.exists() {
        std::fs::rename(&cur_exe, &backup_exe)?;
        true
    } else {
        false
    };
    if let Err(e) = std::fs::rename(&staged_exe, &cur_exe) {
        if moved_cur {
            let _ = std::fs::rename(&backup_exe, &cur_exe);
        }
        return Err(e);
    }

    for entry in std::fs::read_dir(staged)? {
        let entry = entry?;
        let fname = entry.file_name();
        if fname.to_string_lossy().eq_ignore_ascii_case(exe_name) {
            continue;
        }
        let src = entry.path();
        let dst = root.join(&fname);
        if dst.exists() {
            let removed = if dst.is_dir() {
                std::fs::remove_dir_all(&dst).is_ok()
            } else {
                std::fs::remove_file(&dst).is_ok()
            };
            if !removed {
                let fallback_old = root.join(format!("{}{suffix}", fname.to_string_lossy()));
                let _ = std::fs::rename(&dst, &fallback_old);
            }
        }
        if std::fs::rename(&src, &dst).is_err() && src.is_file() {
            std::fs::copy(&src, &dst)?;
        }
    }

    // Bump executable mtime so Explorer's icon cache does not reuse a stale entry for the path.
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&cur_exe) {
        let _ = file.set_modified(SystemTime::now());
    }
    let _ = std::fs::remove_dir_all(staged);
    Ok(())
}

#[cfg(windows)]
fn notify_shell_icon_refresh(root: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        SHCNE_ASSOCCHANGED, SHCNE_UPDATEDIR, SHCNE_UPDATEITEM, SHCNF_FLUSHNOWAIT, SHCNF_IDLIST, SHCNF_PATHW,
        SHChangeNotify,
    };

    let to_wide = |p: &Path| -> Vec<u16> {
        strip_windows_verbatim_prefix(p).as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    };
    let notify_item = |p: &Path| {
        let w = to_wide(p);
        unsafe {
            SHChangeNotify(SHCNE_UPDATEITEM as i32, SHCNF_PATHW | SHCNF_FLUSHNOWAIT, w.as_ptr().cast(), std::ptr::null());
        }
    };

    notify_item(&exe_in(root));
    let dir_w = to_wide(root);
    unsafe {
        SHChangeNotify(SHCNE_UPDATEDIR as i32, SHCNF_PATHW | SHCNF_FLUSHNOWAIT, dir_w.as_ptr().cast(), std::ptr::null());
    }

    let mut shortcuts = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
        shortcuts.push(appdata.join(r"Microsoft\Windows\Start Menu\Programs\Herdr2.lnk"));
        shortcuts.push(appdata.join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar\Herdr2.lnk"));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        shortcuts.push(profile.join(r"Desktop\Herdr2.lnk"));
    }
    if let Some(public) = std::env::var_os("PUBLIC").map(PathBuf::from) {
        shortcuts.push(public.join(r"Desktop\Herdr2.lnk"));
    }
    for lnk in shortcuts {
        if lnk.is_file() {
            notify_item(&lnk);
        }
    }

    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST | SHCNF_FLUSHNOWAIT, std::ptr::null(), std::ptr::null());
    }
}

#[cfg(target_os = "macos")]
fn notify_shell_icon_refresh(root: &Path) {
    let _ = Command::new("/usr/bin/touch").arg(root).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    let _ = Command::new("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister")
        .args(["-f"])
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(any(windows, target_os = "macos")))]
fn notify_shell_icon_refresh(_root: &Path) {}

fn swap(root: &Path) -> std::io::Result<()> {
    let staged = beside(root, ".new");
    if cfg!(windows) {
        let exe_name = format!("herdr2{}", std::env::consts::EXE_SUFFIX);
        swap_files_in_place(root, &staged, &exe_name)?;
        let _ = std::fs::remove_file(beside(root, ".new.json"));
        *READY.lock().unwrap() = None;
        let _ = std::fs::write(marker(), "");
        notify_shell_icon_refresh(root);
        return Ok(());
    }
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
    notify_shell_icon_refresh(root);
    Ok(())
}

/// Opens the app again once this process has exited (after `apply`).
pub fn relaunch() {
    relaunch_expected(None);
}

#[cfg(windows)]
fn relaunch_expected(expected: Option<&str>) -> bool {
    let Some(root) = root() else { return false };
    let script = windows_relaunch_script(std::process::id(), &exe_in(&root));
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

/// Runs once after an app binary swap: brings local `h2` core, already-installed skills,
/// and all SSH remote machines up to date.
pub fn after_update(connected: std::sync::Arc<crate::hc::Core>) {
    let mark = marker();
    if !mark.is_file() {
        return;
    }
    let _ = std::fs::remove_file(&mark);
    let _ = std::thread::Builder::new().name("herdr2-after-update".into()).spawn(move || {
        let status = connected.call("server.status", serde_json::json!({})).map_err(|e| format!("{e:#}"));
        let managed = after_update_managed(status);

        // If a bundled `herdr2-core` inside the install folder was just swapped on disk,
        // restart the local daemon so it runs the new version before syncing skills and remotes.
        let active_in_root = root().is_some_and(|r| {
            crate::core_bin::path()
                .canonicalize()
                .ok()
                .is_some_and(|p| p.starts_with(&r))
        });
        if active_in_root
            && let Some(disk_ver) = crate::core_bin::version(&crate::core_bin::path())
        {
            let disk_clean = clean_core_version(&disk_ver).to_owned();
            let running = core().version.unwrap_or_default();
            if !disk_clean.is_empty() && newer(&disk_clean, &running) {
                match crate::core_bin::restart() {
                    Ok(()) => {
                        let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        c.version = Some(disk_clean);
                        c.ready = None;
                        c.error = None;
                    }
                    Err(e) => {
                        tracing::warn!("update: post-swap bundled core restart failed: {e}");
                    }
                }
            }
        }

        if !managed && checks_core() {
            let have = core().version.unwrap_or_default();
            let latest = releases().and_then(|base| latest_core_from(&base).ok());
            if let Some(target) = after_update_target(&have, latest.as_ref(), crate::hc::GENERATION, crate::hc::core_min()) {
                let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if !c.updating {
                    c.updating = true;
                    c.error = None;
                    drop(c);
                    let res = crate::core_bin::update_to(Some(target));
                    let mut c = CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    c.updating = false;
                    match res {
                        Ok(now) => {
                            c.version = Some(clean_core_version(&now).to_owned());
                            c.ready = None;
                            c.error = None;
                        }
                        Err(e) => {
                            c.error = Some(e);
                        }
                    }
                }
            }
        }

        let skill_summary = sync_installed_skills_best_effort();
        reclassify_machines();
        let remote_summary = sync_remote_machines_blocking(false);
        if remote_summary.updated.is_empty() && remote_summary.failed.is_empty() {
            if let Some(s) = skill_summary.filter(|s| s.updated_count > 0) {
                let suffix = if s.targets.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", s.targets.join(", "))
                };
                crate::mods::request(crate::mods::Request::UpdateToast {
                    title: "Installed skills synced".into(),
                    body: format!("Updated {} installed skill file(s){suffix}.", s.updated_count),
                    level: "info".into(),
                });
            }
        } else {
            emit_remote_sync_toast(&remote_summary, None);
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
        let release = |version: &str, generation| CoreRelease {
            version: version.into(),
            generation,
            notes: None,
            targets: serde_json::Value::Null,
        };
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
    fn feed_url_maps_github_releases_flat_and_standard_servers_hierarchical() {
        assert_eq!(
            feed_url("https://github.com/herdr2/herdr2/releases/download/ota-latest", "ui"),
            "https://github.com/herdr2/herdr2/releases/download/ota-latest/ui-latest.json"
        );
        assert_eq!(
            feed_url("https://github.com/herdr2/herdr2/releases/latest/download/", "app"),
            "https://github.com/herdr2/herdr2/releases/latest/download/app-latest.json"
        );
        assert_eq!(
            feed_url("https://releases.example.com/", "core"),
            "https://releases.example.com/core/latest.json"
        );
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

    #[test]
    fn auto_update_checks_every_hour_and_status_exposes_check_seq_and_updating() {
        assert_eq!(EVERY, Duration::from_secs(60 * 60));
        let st = status();
        assert!(st.get("check_seq").and_then(|v| v.as_u64()).is_some());
        assert!(st.get("updating").and_then(|v| v.as_bool()).is_some());
        assert!(st.get("available").is_some());
    }

    #[test]
    fn curl_command_and_output_decoding_handle_platform_behavior() {
        let cmd = curl_command();
        let args: Vec<_> = cmd.get_args().collect();
        if cfg!(windows) {
            assert!(args.iter().any(|a| *a == "--ssl-no-revoke"));
        } else {
            assert!(!args.iter().any(|a| *a == "--ssl-no-revoke"));
        }
        assert_eq!(decode_process_output("检查更新失败".as_bytes()), "检查更新失败");
        assert_eq!(decode_process_output(b""), "");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn needs_elevation_detects_read_only_install_root_on_linux() {
        use std::os::unix::fs::PermissionsExt;
        // Skip permission-denial checks if tests run as root (root bypasses DAC write checks).
        if Command::new("id").arg("-u").output().ok().is_some_and(|o| o.stdout == b"0\n") {
            return;
        }
        let work = TempWorkDir::new("herdr2-test-elev-perm").unwrap();
        let parent = work.path().join("opt");
        let root = parent.join("herdr2");
        std::fs::create_dir_all(&root).unwrap();
        assert!(!needs_elevation_for(&root));

        // Read-only install root (e.g. root-owned ~/.local/share/herdr2) requires elevation.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(needs_elevation_for(&root));
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Read-only parent (e.g. /opt owned by root) requires elevation.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(needs_elevation_for(&root));
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn elevated_swap_replaces_root_and_preserves_or_upgrades_bundled_core() {
        use std::os::unix::fs::PermissionsExt;
        let work = TempWorkDir::new("herdr2-test-elev-swap").unwrap();
        let root = work.path().join("opt/herdr2");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("herdr2"), "#!/bin/sh\necho herdr2 2.0.8\n").unwrap();
        std::fs::write(root.join("herdr2-core"), "#!/bin/sh\necho h2 2.0.8\n").unwrap();
        std::fs::write(root.join("herdr2.svg"), "<svg/>").unwrap();
        std::fs::write(root.join("herdr2.png"), "png-old").unwrap();
        std::fs::set_permissions(root.join("herdr2"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(root.join("herdr2-core"), std::fs::Permissions::from_mode(0o755)).unwrap();

        // Case 1: no core feed -> preserves existing herdr2-core & png during elevated swap.
        let unpacked1 = work.path().join("unpacked1/herdr2");
        std::fs::create_dir_all(&unpacked1).unwrap();
        std::fs::write(unpacked1.join("herdr2"), "#!/bin/sh\necho herdr2 2.0.9\n").unwrap();
        std::fs::write(unpacked1.join("herdr2.svg"), "<svg>new</svg>").unwrap();
        std::fs::set_permissions(unpacked1.join("herdr2"), std::fs::Permissions::from_mode(0o755)).unwrap();

        let upgraded1 = preserve_or_update_bundled_core(&root, &unpacked1, work.path(), None).unwrap();
        assert_eq!(upgraded1, None);
        assert!(unpacked1.join("herdr2-core").is_file());
        assert_eq!(std::fs::read_to_string(unpacked1.join("herdr2.png")).unwrap(), "png-old");

        let swap_work1 = TempWorkDir::new("herdr2-test-swap1").unwrap();
        run_elevated_swap_with(
            Path::new("/usr/bin/env"),
            &[],
            swap_work1.path(),
            &unpacked1,
            &root,
            "2.0.9",
        )
        .unwrap();
        assert_eq!(version_of(&root).as_deref(), Some("2.0.9"));
        assert_eq!(crate::core_bin::version(&root.join("herdr2-core")).as_deref(), Some("h2 2.0.8"));
        assert!(!beside(&root, ".old").exists());
        assert!(!beside(&root, ".new").exists());

        // Case 2: newer core release available -> upgrades bundled herdr2-core in lockstep.
        let server = work.path().join("server");
        let core_pkg_src = work.path().join("core-pkg-src");
        std::fs::create_dir_all(server.join("core")).unwrap();
        std::fs::create_dir_all(&core_pkg_src).unwrap();
        std::fs::write(core_pkg_src.join("herdr2"), "#!/bin/sh\necho h2 2.0.10\n").unwrap();
        std::fs::set_permissions(core_pkg_src.join("herdr2"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let core_tar = server.join("core/herdr2-core.tar.gz");
        assert!(Command::new("tar").args(["-czf"]).arg(&core_tar).arg("-C").arg(&core_pkg_src).arg("herdr2").status().unwrap().success());
        let core_sha = sha256(&std::fs::read(&core_tar).unwrap());
        let core_manifest = serde_json::json!({
            "version": "2.0.10",
            "generation": crate::hc::GENERATION,
            "targets": {
                target(): {
                    "url": format!("file://{}", core_tar.display()),
                    "sha256": core_sha
                }
            }
        });
        std::fs::write(server.join("core/latest.json"), core_manifest.to_string()).unwrap();

        let unpacked2 = work.path().join("unpacked2/herdr2");
        std::fs::create_dir_all(&unpacked2).unwrap();
        std::fs::write(unpacked2.join("herdr2"), "#!/bin/sh\necho herdr2 2.0.10\n").unwrap();
        std::fs::write(unpacked2.join("herdr2.svg"), "<svg>new2</svg>").unwrap();
        std::fs::set_permissions(unpacked2.join("herdr2"), std::fs::Permissions::from_mode(0o755)).unwrap();

        let base_url = format!("file://{}", server.display());
        let upgraded2 = preserve_or_update_bundled_core(&root, &unpacked2, work.path(), Some(&base_url)).unwrap();
        assert_eq!(upgraded2.as_deref(), Some("2.0.10"));

        let swap_work2 = TempWorkDir::new("herdr2-test-swap2").unwrap();
        run_elevated_swap_with(
            Path::new("/usr/bin/env"),
            &[],
            swap_work2.path(),
            &unpacked2,
            &root,
            "2.0.10",
        )
        .unwrap();
        assert_eq!(version_of(&root).as_deref(), Some("2.0.10"));
        assert_eq!(crate::core_bin::version(&root.join("herdr2-core")).as_deref(), Some("h2 2.0.10"));
    }

    #[cfg(unix)]
    #[test]
    fn elevated_swap_maps_pkexec_exit_codes_and_missing_binary() {
        use std::os::unix::fs::PermissionsExt;
        let work = TempWorkDir::new("herdr2-test-pkexec-err").unwrap();
        let unpacked = work.path().join("herdr2");
        let root = work.path().join("dst/herdr2");
        std::fs::create_dir_all(&unpacked).unwrap();
        std::fs::create_dir_all(&root).unwrap();

        let mk_stub = |name: &str, body: &str| {
            let p = work.path().join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };

        let cancelled = mk_stub("stub-126", "exit 126");
        let err = run_elevated_swap_with(&cancelled, &[], work.path(), &unpacked, &root, "2.0.10").unwrap_err();
        assert!(err.contains("cancelled"), "{err}");

        let unauth = mk_stub("stub-127", "echo 'No authentication agent found' >&2; exit 127");
        let err = run_elevated_swap_with(&unauth, &[], work.path(), &unpacked, &root, "2.0.10").unwrap_err();
        assert!(err.contains("No authentication agent found") && err.contains("herdr2_2.0.10_amd64.deb"), "{err}");

        let failed = mk_stub("stub-1", "echo 'disk full' >&2; exit 1");
        let err = run_elevated_swap_with(&failed, &[], work.path(), &unpacked, &root, "2.0.10").unwrap_err();
        assert!(err.contains("disk full"), "{err}");

        let missing = work.path().join("does-not-exist-pkexec");
        let err = run_elevated_swap_with(&missing, &[], work.path(), &unpacked, &root, "2.0.10").unwrap_err();
        assert!(err.contains("pkexec is not installed"), "{err}");
    }

    #[test]
    fn strip_windows_verbatim_prefix_handles_drive_and_unc_paths() {
        assert_eq!(
            strip_windows_verbatim_prefix(Path::new(r"\\?\C:\Users\alice\AppData\Local\Programs\herdr2")),
            PathBuf::from(r"C:\Users\alice\AppData\Local\Programs\herdr2")
        );
        assert_eq!(
            strip_windows_verbatim_prefix(Path::new(r"\\?\UNC\server\share\herdr2")),
            PathBuf::from(r"\\server\share\herdr2")
        );
        assert_eq!(
            strip_windows_verbatim_prefix(Path::new(r"C:\Users\alice\herdr2")),
            PathBuf::from(r"C:\Users\alice\herdr2")
        );
        assert_eq!(
            strip_windows_verbatim_prefix(Path::new("/opt/herdr2")),
            PathBuf::from("/opt/herdr2")
        );
    }

    #[test]
    fn windows_in_place_swap_replaces_files_and_preserves_uninstaller() {
        let work = std::env::temp_dir().join(format!(
            "herdr2-win-inplace-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        let root = work.join("herdr2");
        let staged = work.join("herdr2.new");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&staged).unwrap();

        std::fs::write(root.join("herdr2.exe"), "old-exe").unwrap();
        std::fs::write(root.join("herdr2.svg"), "old-svg").unwrap();
        std::fs::write(root.join("unins000.exe"), "inno-uninstaller").unwrap();
        std::fs::write(root.join("unins000.dat"), "inno-data").unwrap();

        std::fs::write(staged.join("herdr2.exe"), "new-exe").unwrap();
        std::fs::write(staged.join("herdr2.svg"), "new-svg").unwrap();
        std::fs::write(staged.join("LICENSE"), "apache-2.0").unwrap();

        swap_files_in_place(&root, &staged, "herdr2.exe").unwrap();

        assert_eq!(std::fs::read_to_string(root.join("herdr2.exe")).unwrap(), "new-exe");
        assert_eq!(std::fs::read_to_string(root.join("herdr2.svg")).unwrap(), "new-svg");
        assert_eq!(std::fs::read_to_string(root.join("LICENSE")).unwrap(), "apache-2.0");
        assert_eq!(std::fs::read_to_string(root.join("unins000.exe")).unwrap(), "inno-uninstaller");
        assert_eq!(std::fs::read_to_string(root.join("unins000.dat")).unwrap(), "inno-data");
        assert!(!staged.exists());

        let old_backups: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("herdr2.exe.old-"))
            .collect();
        assert_eq!(old_backups.len(), 1);
        assert_eq!(std::fs::read_to_string(old_backups[0].path()).unwrap(), "old-exe");

        // Missing staged executable leaves the existing installation untouched.
        let bad_stage = work.join("herdr2.bad");
        std::fs::create_dir_all(&bad_stage).unwrap();
        std::fs::write(bad_stage.join("herdr2.svg"), "corrupt").unwrap();
        let err = swap_files_in_place(&root, &bad_stage, "herdr2.exe").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(std::fs::read_to_string(root.join("herdr2.exe")).unwrap(), "new-exe");
        assert_eq!(std::fs::read_to_string(root.join("herdr2.svg")).unwrap(), "new-svg");

        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn classify_remote_machine_marks_outdated_and_unreachable_as_needs_update() {
        let mk = |name: &str, version: Option<&str>, state: &str, why: Option<&str>| crate::core_bin::RemoteMachine {
            name: name.into(),
            target: format!("dev@{name}"),
            version: version.map(str::to_owned),
            state: state.into(),
            why: why.map(str::to_owned),
            temporary: false,
        };

        // 1. Connected & current -> connected, not needs_update
        let current = classify_remote_machine(&mk("box-ok", Some("2.0.15"), "connected", None), Some("2.0.15"), None);
        assert_eq!(current.state, "connected");
        assert!(!current.needs_update);
        assert!(!current.unreachable);
        assert_eq!(current.ready, None);

        // 2. Connected & older -> needs_update with ready target
        let outdated = classify_remote_machine(&mk("box-old", Some("2.0.12"), "connected", None), Some("2.0.15"), None);
        assert_eq!(outdated.state, "needs_update");
        assert!(outdated.needs_update);
        assert!(!outdated.unreachable);
        assert_eq!(outdated.ready.as_deref(), Some("2.0.15"));

        // 3. Unreachable -> annotated as needs_update even if last seen version matches
        let offline = classify_remote_machine(
            &mk("box-down", Some("2.0.15"), "unreachable", Some("ssh timed out")),
            Some("2.0.15"),
            None,
        );
        assert_eq!(offline.state, "needs_update");
        assert_eq!(offline.raw_state, "unreachable");
        assert!(offline.needs_update);
        assert!(offline.unreachable);
        assert_eq!(offline.ready, None);
        assert_eq!(offline.why.as_deref(), Some("ssh timed out"));

        // 4. Unreachable & older -> needs_update + ready target
        let offline_old = classify_remote_machine(&mk("box-down-old", Some("2.0.10"), "needs_login", None), Some("2.0.15"), None);
        assert_eq!(offline_old.state, "needs_update");
        assert!(offline_old.needs_update);
        assert!(offline_old.unreachable);
        assert_eq!(offline_old.ready.as_deref(), Some("2.0.15"));

        // 5. Protocol mismatch (`needs_update`) -> needs_update
        let mismatch = classify_remote_machine(&mk("box-proto", Some("2.0.15"), "needs_update", None), Some("2.0.15"), None);
        assert_eq!(mismatch.state, "needs_update");
        assert!(mismatch.needs_update);
        assert_eq!(mismatch.ready.as_deref(), Some("2.0.15"));

        // remote_ready_from triggers when an outdated or protocol-mismatched remote exists, not on current unreachable-only
        assert_eq!(remote_ready_from(&[current.clone(), offline.clone()], Some("2.0.15")), None);
        assert_eq!(remote_ready_from(&[current, outdated.clone()], Some("2.0.15")).as_deref(), Some("2.0.15"));

        // Toast formatting summarizes both local + remote sync results cleanly.
        let summary = RemoteSyncSummary {
            updated: vec![("box-old".into(), "2.0.15".into())],
            failed: vec![("box-down".into(), "ssh timed out".into())],
        };
        let (title, body, level) = format_remote_sync_toast(
            &summary,
            Some(("herdr2-core updated to v2.0.15", "Running terminals stayed alive.")),
        )
        .unwrap();
        assert_eq!(title, "herdr2-core updated to v2.0.15");
        assert!(body.contains("box-old (v2.0.15)") && body.contains("box-down: ssh timed out"), "{body}");
        assert_eq!(level, "error");
    }

    #[cfg(unix)]
    #[test]
    fn macos_bundle_preserves_or_upgrades_helpers_h2_and_keeps_packaged_helper() {
        use std::os::unix::fs::PermissionsExt;
        let work = TempWorkDir::new("herdr2-test-macos-helper").unwrap();
        let root = work.path().join("Applications/Herdr2.app");
        std::fs::create_dir_all(root.join("Contents/Helpers")).unwrap();
        std::fs::write(root.join("Contents/Helpers/h2"), "#!/bin/sh\necho h2 2.0.15\n").unwrap();
        std::fs::set_permissions(root.join("Contents/Helpers/h2"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(has_bundled_core(&root));

        // Case 1: OTA bundle omits Contents/Helpers/h2 and no core feed -> preserves existing helper.
        let unpacked1 = work.path().join("unpacked1/Herdr2.app");
        std::fs::create_dir_all(unpacked1.join("Contents/MacOS")).unwrap();
        let upgraded1 = preserve_or_update_bundled_core_inner(&root, &unpacked1, work.path(), None, false).unwrap();
        assert_eq!(upgraded1, None);
        assert_eq!(
            crate::core_bin::version(&unpacked1.join("Contents/Helpers/h2")).as_deref(),
            Some("h2 2.0.15")
        );

        // Case 2: OTA bundle omits Contents/Helpers/h2 and newer core feed exists -> upgrades helper.
        let server = work.path().join("server");
        let core_pkg_src = work.path().join("core-pkg-src");
        std::fs::create_dir_all(server.join("core")).unwrap();
        std::fs::create_dir_all(&core_pkg_src).unwrap();
        std::fs::write(core_pkg_src.join("herdr2"), "#!/bin/sh\necho h2 2.0.16\n").unwrap();
        std::fs::set_permissions(core_pkg_src.join("herdr2"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let core_tar = server.join("core/herdr2-core.tar.gz");
        assert!(Command::new("tar").args(["-czf"]).arg(&core_tar).arg("-C").arg(&core_pkg_src).arg("herdr2").status().unwrap().success());
        let core_sha = sha256(&std::fs::read(&core_tar).unwrap());
        let core_manifest = serde_json::json!({
            "version": "2.0.16",
            "generation": crate::hc::GENERATION,
            "targets": {
                target(): {
                    "url": format!("file://{}", core_tar.display()),
                    "sha256": core_sha
                }
            }
        });
        std::fs::write(server.join("core/latest.json"), core_manifest.to_string()).unwrap();

        let unpacked2 = work.path().join("unpacked2/Herdr2.app");
        std::fs::create_dir_all(unpacked2.join("Contents/MacOS")).unwrap();
        let base_url = format!("file://{}", server.display());
        let upgraded2 = preserve_or_update_bundled_core_inner(&root, &unpacked2, work.path(), Some(&base_url), false).unwrap();
        assert_eq!(upgraded2.as_deref(), Some("2.0.16"));
        assert_eq!(
            crate::core_bin::version(&unpacked2.join("Contents/Helpers/h2")).as_deref(),
            Some("h2 2.0.16")
        );

        // Case 3: OTA bundle already ships Contents/Helpers/h2 -> leave it untouched (signature-safe).
        let unpacked3 = work.path().join("unpacked3/Herdr2.app");
        std::fs::create_dir_all(unpacked3.join("Contents/Helpers")).unwrap();
        let packaged_body = "#!/bin/sh\n# packaged-marker\necho h2 2.0.17\n";
        std::fs::write(unpacked3.join("Contents/Helpers/h2"), packaged_body).unwrap();
        std::fs::set_permissions(unpacked3.join("Contents/Helpers/h2"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let upgraded3 = preserve_or_update_bundled_core_inner(&root, &unpacked3, work.path(), Some(&base_url), false).unwrap();
        assert_eq!(upgraded3.as_deref(), Some("2.0.17"));
        assert_eq!(
            std::fs::read_to_string(unpacked3.join("Contents/Helpers/h2")).unwrap(),
            packaged_body
        );
    }

    #[cfg(unix)]
    #[test]
    fn stale_app_manifest_sha256_refreshes_and_retries_on_rolling_ota_channel() {
        use std::os::unix::fs::PermissionsExt;
        let work = TempWorkDir::new("herdr2-test-stale-app-sha").unwrap();
        let root = work.path().join(PACKED);
        std::fs::create_dir_all(exe_in(&root).parent().unwrap()).unwrap();
        std::fs::write(exe_in(&root), format!("#!/bin/sh\necho herdr2 {}\n", env!("CARGO_PKG_VERSION"))).unwrap();
        std::fs::set_permissions(exe_in(&root), std::fs::Permissions::from_mode(0o755)).unwrap();

        let ota_dir = work.path().join("releases/download/ota-latest");
        std::fs::create_dir_all(&ota_dir).unwrap();
        let archive = ota_dir.join(format!("herdr2-app-{}.tar.gz", target()));

        let build_pkg = |ver: &str, marker_comment: &str| -> String {
            let pkg_root = work.path().join("pkg-src").join(PACKED);
            let _ = std::fs::remove_dir_all(work.path().join("pkg-src"));
            let pkg_exe = exe_in(&pkg_root);
            std::fs::create_dir_all(pkg_exe.parent().unwrap()).unwrap();
            std::fs::write(&pkg_exe, format!("#!/bin/sh\n# {marker_comment}\necho herdr2 {ver}\n")).unwrap();
            std::fs::set_permissions(&pkg_exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            if cfg!(target_os = "macos") {
                std::fs::create_dir_all(pkg_root.join("Contents")).unwrap();
                std::fs::write(
                    pkg_root.join("Contents/Info.plist"),
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict/></plist>",
                )
                .unwrap();
            } else {
                std::fs::write(pkg_root.join("herdr2.svg"), "<svg/>").unwrap();
            }
            assert!(
                Command::new("tar")
                    .args(["-czf"])
                    .arg(&archive)
                    .arg("-C")
                    .arg(work.path().join("pkg-src"))
                    .arg(PACKED)
                    .status()
                    .unwrap()
                    .success()
            );
            sha256(&std::fs::read(&archive).unwrap())
        };

        let base_url = format!("file://{}", ota_dir.display());
        let write_app_manifest = |ver: &str, sha: &str| {
            let doc = serde_json::json!({
                "version": ver,
                "notes": "Rolling OTA",
                "targets": {
                    target(): {
                        "url": format!("file://{}", archive.display()),
                        "sha256": sha,
                    }
                }
            });
            std::fs::write(ota_dir.join("app-latest.json"), doc.to_string()).unwrap();
        };

        // 1) Background hourly check sees Build A of 99.0.1 and caches sha_a.
        let sha_a = build_pkg("99.0.1", "build-from-ota-update-yml");
        write_app_manifest("99.0.1", &sha_a);
        let stale_avail = refresh_app_manifest_for_root(&root, &base_url).unwrap().unwrap();
        assert_eq!(stale_avail.sha256, sha_a);

        // 2) `release.yml` finishes later and clobbers `ota-latest` with Build B (different SHA-256!).
        let sha_b = build_pkg("99.0.1", "build-from-release-yml");
        assert_ne!(sha_a, sha_b);
        write_app_manifest("99.0.1", &sha_b);

        // Even if download started with `stale_avail`, `unpack_app_with_retry` refreshes `app-latest.json`
        // and stages Build B cleanly instead of failing with `checksum mismatch`.
        let staged = download_and_stage_app_from(&root, &stale_avail, Some(&base_url)).unwrap();
        assert_eq!(staged, "99.0.1");
        assert_eq!(staged_version(&root).as_deref(), Some("99.0.1"));

        *READY.lock().unwrap() = None;
        *APP_AVAILABLE.lock().unwrap() = None;
    }

    #[cfg(unix)]
    #[test]
    fn core_update_strategy_and_capabilities_allow_standalone_bundled_and_stale_daemon_updates() {
        use std::os::unix::fs::PermissionsExt;
        let work = TempWorkDir::new("herdr2-test-core-strategy").unwrap();
        let root = work.path().join("opt/herdr2");
        std::fs::create_dir_all(&root).unwrap();
        let bundled = root.join("herdr2-core");
        std::fs::write(&bundled, "#!/bin/sh\necho h2 2.0.21\n").unwrap();
        std::fs::set_permissions(&bundled, std::fs::Permissions::from_mode(0o755)).unwrap();

        let sys_managed = CoreInfo {
            version: Some("2.0.21".into()),
            managed: Some("system package manager".into()),
            ready: Some("2.0.22".into()),
            ..Default::default()
        };
        assert_eq!(
            core_update_strategy_for(&sys_managed, Some(&root), &bundled, true, false),
            CoreUpdateStrategy::ElevatedLinuxBundled
        );
        assert_eq!(
            core_can_self_update_for(&sys_managed, Some("2.0.21"), Some(&root), &bundled, true, false),
            (true, true)
        );

        // If the binary on disk is already 2.0.22 (only the running daemon is stale at 2.0.21),
        // updating only needs `h2 server restart` — no elevation needed, even for package-managed cores.
        assert_eq!(
            core_can_self_update_for(&sys_managed, Some("2.0.22"), Some(&root), &bundled, true, false),
            (true, false)
        );
        let brew_managed = CoreInfo {
            version: Some("2.0.21".into()),
            managed: Some("homebrew".into()),
            ready: Some("2.0.22".into()),
            ..Default::default()
        };
        assert_eq!(
            core_update_strategy_for(&brew_managed, Some(&root), Path::new("/nonexistent/h2"), true, false),
            CoreUpdateStrategy::ExternalManaged
        );
        assert_eq!(
            core_can_self_update_for(&brew_managed, Some("2.0.21"), Some(&root), Path::new("/nonexistent/h2"), true, false),
            (false, false)
        );
        assert_eq!(
            core_can_self_update_for(&brew_managed, Some("2.0.22"), Some(&root), Path::new("/nonexistent/h2"), true, false),
            (true, false)
        );

        // Non-bundled /nonexistent system-managed core remains ExternalManaged.
        assert_eq!(
            core_can_self_update_for(&sys_managed, None, Some(&root), Path::new("/nonexistent/herdr-core"), true, false),
            (false, false)
        );

        // Unmanaged user-writable core uses Direct strategy.
        let unmanaged = CoreInfo {
            version: Some("2.0.21".into()),
            managed: None,
            ready: Some("2.0.22".into()),
            ..Default::default()
        };
        assert_eq!(
            core_update_strategy_for(&unmanaged, Some(&root), &bundled, true, false),
            CoreUpdateStrategy::Direct
        );
        assert_eq!(
            core_can_self_update_for(&unmanaged, Some("2.0.21"), Some(&root), &bundled, true, false),
            (true, false)
        );

        // newer_core_version strips `h2 ` prefixes and build metadata suffixes safely.
        assert!(newer_core_version("h2 2.0.22", "2.0.21 (a1b2c3d 2026-11-27T08:00:00Z)"));
        assert!(!newer_core_version("h2 2.0.22", "2.0.22 (a1b2c3d 2026-11-27T08:00:00Z)"));
        assert!(!newer_core_version("h2 2.0.21", "2.0.22"));
    }

    #[cfg(unix)]
    #[test]
    fn elevated_core_swap_replaces_bundled_core_atomically_and_maps_errors() {
        use std::os::unix::fs::PermissionsExt;
        let work = TempWorkDir::new("herdr2-test-elev-core-swap").unwrap();
        let opt_dir = work.path().join("opt/herdr2");
        std::fs::create_dir_all(&opt_dir).unwrap();
        let target_core = opt_dir.join("herdr2-core");
        let target_h2 = opt_dir.join("h2");
        std::fs::write(&target_core, "#!/bin/sh\necho h2 2.0.21\n").unwrap();
        std::fs::write(&target_h2, "#!/bin/sh\necho h2 2.0.21\n").unwrap();
        std::fs::set_permissions(&target_core, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&target_h2, std::fs::Permissions::from_mode(0o755)).unwrap();

        let new_bin = work.path().join("new-core");
        std::fs::write(&new_bin, "#!/bin/sh\necho h2 2.0.22\n").unwrap();
        std::fs::set_permissions(&new_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        run_elevated_core_swap_with(
            Path::new("/usr/bin/env"),
            &[],
            work.path(),
            &new_bin,
            &[target_core.clone(), target_h2.clone()],
            "2.0.22",
        )
        .unwrap();
        assert_eq!(crate::core_bin::version(&target_core).as_deref(), Some("h2 2.0.22"));
        assert_eq!(crate::core_bin::version(&target_h2).as_deref(), Some("h2 2.0.22"));

        let mk_stub = |name: &str, body: &str| {
            let p = work.path().join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let cancelled = mk_stub("core-stub-126", "exit 126");
        let err = run_elevated_core_swap_with(&cancelled, &[], work.path(), &new_bin, std::slice::from_ref(&target_core), "2.0.22").unwrap_err();
        assert!(err.contains("cancelled"), "{err}");

        let unauth = mk_stub("core-stub-127", "echo 'No auth agent' >&2; exit 127");
        let err = run_elevated_core_swap_with(&unauth, &[], work.path(), &new_bin, std::slice::from_ref(&target_core), "2.0.22").unwrap_err();
        assert!(err.contains("No auth agent") && err.contains("herdr2_2.0.22_amd64.deb"), "{err}");

        let missing = work.path().join("no-such-pkexec");
        let err = run_elevated_core_swap_with(&missing, &[], work.path(), &new_bin, std::slice::from_ref(&target_core), "2.0.22").unwrap_err();
        assert!(err.contains("pkexec is not installed"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn status_advertises_core_update_when_app_is_latest_and_disk_binary_is_newer_than_running() {
        const CHILD: &str = "HERDR_TEST_STALE_RUNNING_CORE_STATUS";
        if std::env::var_os(CHILD).is_none() {
            let root = std::env::temp_dir().join(format!("herdr-stale-core-status-{}", std::process::id()));
            std::fs::create_dir_all(root.join("core")).unwrap();
            std::fs::write(root.join("core/latest.json"), r#"{"version":"2.0.22","generation":1}"#).unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "update::tests::status_advertises_core_update_when_app_is_latest_and_disk_binary_is_newer_than_running", "--nocapture"])
                .env(CHILD, "1")
                .env("HERDR2_RELEASES_URL", format!("file://{}", root.display()))
                .output()
                .unwrap();
            let _ = std::fs::remove_dir_all(root);
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let work = TempWorkDir::new("herdr2-test-stale-core").unwrap();
        let disk_core = work.path().join("herdr2-core");
        std::fs::write(&disk_core, "#!/bin/sh\necho h2 2.0.22\n").unwrap();
        std::fs::set_permissions(&disk_core, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::core_bin::use_core(disk_core);

        set_core(Some("2.0.21".into()), Some("system package manager".into()));
        check_core().unwrap();

        let st = status();
        assert_eq!(st["app"]["ready"], serde_json::Value::Null);
        assert_eq!(st["core"]["ready"], "2.0.22");
        assert_eq!(st["core"]["can_update"], true);
        assert_eq!(st["core"]["needs_elevation"], false);
        assert_eq!(st["available"]["kind"], "core");
        assert_eq!(st["available"]["version"], "2.0.22");
        assert_eq!(st["available"]["needs_elevation"], false);
        assert_eq!(st["available"]["items"][0]["kind"], "core");
        assert_eq!(st["available"]["items"][0]["current"], "2.0.21");
        assert_eq!(st["available"]["items"][0]["next"], "2.0.22");
    }
}
