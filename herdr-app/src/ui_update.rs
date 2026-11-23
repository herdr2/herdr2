//! Production OTA UI-bundle hot-updates: downloads verified `mods/`, `frame/`,
//! `layouts/`, `themes/`, and `icons/` bundles from `<releases_url>/ui/latest.json`
//! and applies them live without restarting the Herdr2 window or interrupting
//! running terminals.
//!
//! Safety guarantees:
//! - Compatibility gated by `host_api_generation` and `[app_version_min, app_version_max]`.
//! - SHA-256 checksum verified before unpacking.
//! - Every unpacked archive path is checked against path traversal and symlinks.
//! - Shipped mods/layouts go through `mods::sync_defaults_owned` / `sync_layouts`,
//!   so any user-customized mod is preserved as `<name>-edited`.
//! - Previous files are backed up to `.ui-prev/`; if reloading mods reports any
//!   load error, the window automatically rolls back and records the failed
//!   version in `ui-failed.json` to prevent retry loops.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const RECEIPT_FILE: &str = "ui-bundle.json";
const FAILED_FILE: &str = "ui-failed.json";
const STAGE_DIR: &str = ".ui-stage";
const DOWNLOAD_DIR: &str = ".ui-download";
const PREV_DIR: &str = ".ui-prev";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiReceipt {
    pub version: String,
    pub app_version: String,
    pub sha256: String,
    pub applied_at: u64,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct UiBundleInfo {
    pub version: String,
    pub app_version: String,
    pub ready: Option<String>,
    pub ready_notes: Option<String>,
    pub applying: bool,
    pub mode: String,
    pub error: Option<String>,
    pub last_applied_at: Option<u64>,
    pub active_override: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UiManifest {
    pub version: String,
    #[serde(default)]
    pub app_version_min: Option<String>,
    #[serde(default)]
    pub app_version_max: Option<String>,
    pub host_api_generation: u32,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StagedReceipt {
    version: String,
    sha256: String,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AppliedUi {
    pub version: String,
    pub notes: Option<String>,
}

#[derive(Default)]
struct State {
    ready: Option<String>,
    ready_notes: Option<String>,
    applying: bool,
    error: Option<String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    ready: None,
    ready_notes: None,
    applying: false,
    error: None,
});

/// Configured UI hot-update mode: `"prompt"` (default), `"auto"`, or `"off"`.
pub fn mode() -> String {
    let raw = crate::settings::load_person()
        .ok()
        .and_then(|p| p.ui_hot_update)
        .unwrap_or_default();
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => "auto".into(),
        "off" | "disabled" => "off".into(),
        _ => "prompt".into(),
    }
}

pub fn active_receipt(app_dir: &Path) -> Option<UiReceipt> {
    let bytes = std::fs::read(app_dir.join(RECEIPT_FILE)).ok()?;
    let receipt: UiReceipt = serde_json::from_slice(&bytes).ok()?;
    if receipt.app_version != env!("CARGO_PKG_VERSION") {
        return None;
    }
    Some(receipt)
}

/// True when an OTA UI bundle has been applied for the currently running binary version.
pub fn has_active_override(app_dir: &Path) -> bool {
    active_receipt(app_dir).is_some()
}

pub fn current_version(app_dir: &Path) -> String {
    active_receipt(app_dir)
        .map(|r| r.version)
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
}

pub fn info() -> UiBundleInfo {
    let dir = crate::mods::app_dir();
    let receipt = active_receipt(&dir);
    let st = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    UiBundleInfo {
        version: receipt
            .as_ref()
            .map(|r| r.version.clone())
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned()),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        ready: st.ready.clone(),
        ready_notes: st.ready_notes.clone(),
        applying: st.applying,
        mode: mode(),
        error: st.error.clone(),
        last_applied_at: receipt.as_ref().map(|r| r.applied_at),
        active_override: receipt.is_some(),
    }
}

pub fn ready() -> Option<String> {
    STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).ready.clone()
}

pub fn set_applying(applying: bool) {
    STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).applying = applying;
}

pub fn set_error(err: Option<String>) {
    let mut st = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    st.error = err;
    st.applying = false;
}

fn failed_versions(app_dir: &Path) -> BTreeSet<String> {
    std::fs::read(app_dir.join(FAILED_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn record_failed_version(app_dir: &Path, version: &str) {
    let mut failed = failed_versions(app_dir);
    failed.insert(version.to_owned());
    if let Ok(text) = serde_json::to_string_pretty(&failed) {
        let _ = crate::persist::write_atomic(&app_dir.join(FAILED_FILE), text.as_bytes());
    }
}

/// Checks whether a manifest is compatible with the running host binary.
pub fn is_compatible(manifest: &UiManifest, running_app: &str, host_generation: u32) -> bool {
    if manifest.host_api_generation != host_generation {
        return false;
    }
    if let Some(min) = manifest.app_version_min.as_deref()
        && crate::update::newer(min, running_app)
    {
        return false;
    }
    if let Some(max) = manifest.app_version_max.as_deref()
        && crate::update::newer(running_app, max)
    {
        return false;
    }
    true
}

fn staged_receipt(app_dir: &Path) -> Option<StagedReceipt> {
    let stage = app_dir.join(STAGE_DIR);
    let bytes = std::fs::read(stage.join("ready.json")).ok()?;
    let receipt: StagedReceipt = serde_json::from_slice(&bytes).ok()?;
    if !stage.join("content").is_dir() {
        return None;
    }
    Some(receipt)
}

/// Compares UI bundle versions where `-ui.<n>` (or `+ui.<n>`) is a post-release
/// UI revision on top of the base app version (so `0.2.0-ui.1 > 0.2.0` and
/// `0.2.0-ui.10 > 0.2.0-ui.2`), while other versions follow standard SemVer.
pub(crate) fn is_newer_ui_version(candidate: &str, current: &str) -> bool {
    fn split_ui_rev(v: &str) -> (&str, Option<u64>) {
        let trimmed = v.trim().trim_start_matches('v');
        for sep in ["-ui.", "+ui."] {
            if let Some((base, rev)) = trimmed.rsplit_once(sep)
                && !base.is_empty()
                && let Ok(n) = rev.parse::<u64>()
            {
                return (base, Some(n));
            }
        }
        (trimmed, None)
    }

    let (ca, ra) = split_ui_rev(candidate);
    let (cb, rb) = split_ui_rev(current);
    if ca != cb {
        return crate::update::newer(ca, cb);
    }
    match (ra, rb) {
        (Some(a), Some(b)) => a > b,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => crate::update::newer(candidate, current),
    }
}

/// Checks `<releases>/ui/latest.json`, verifies compatibility & sha256, and stages the bundle.
/// Returns `Ok(Some(version))` when a newer compatible UI bundle is staged and ready to apply.
pub fn check_from(app_dir: &Path, releases: &str) -> Result<Option<String>, String> {
    let url = format!("{}/ui/latest.json", releases.trim_end_matches('/'));
    let text = crate::update::curl_text(&url)?;
    let manifest: UiManifest = serde_json::from_str(&text).map_err(|e| format!("ui/latest.json: {e}"))?;
    let running_app = env!("CARGO_PKG_VERSION");
    if !is_compatible(&manifest, running_app, crate::mods::UI_HOST_API_GENERATION) {
        tracing::info!(
            "ui_update: skipping {} (requires host_api_generation={}, app {:?}..={:?}; running {} gen {})",
            manifest.version,
            manifest.host_api_generation,
            manifest.app_version_min,
            manifest.app_version_max,
            running_app,
            crate::mods::UI_HOST_API_GENERATION
        );
        return Ok(None);
    }
    let current = current_version(app_dir);
    if !is_newer_ui_version(&manifest.version, &current) {
        let mut st = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        st.ready = None;
        st.ready_notes = None;
        return Ok(None);
    }
    if failed_versions(app_dir).contains(&manifest.version) {
        tracing::info!("ui_update: skipping previously failed UI bundle {}", manifest.version);
        return Ok(None);
    }
    if let Some(staged) = staged_receipt(app_dir)
        && staged.version == manifest.version
        && staged.sha256.eq_ignore_ascii_case(&manifest.sha256)
    {
        let mut st = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        st.ready = Some(manifest.version.clone());
        st.ready_notes = manifest.notes.clone();
        st.error = None;
        return Ok(Some(manifest.version));
    }

    tracing::info!("ui_update: downloading UI bundle {}", manifest.version);
    let work = app_dir.join(DOWNLOAD_DIR);
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;

    let stage_result = (|| -> Result<(), String> {
        let archive = work.join("ui-bundle.tar.gz");
        crate::update::download_file(&manifest.url, &archive)?;
        let bytes = std::fs::read(&archive).map_err(|e| e.to_string())?;
        if crate::update::sha256(&bytes) != manifest.sha256.to_ascii_lowercase() {
            return Err("UI bundle checksum mismatch".into());
        }
        let unpacked = work.join("content");
        std::fs::create_dir_all(&unpacked).map_err(|e| e.to_string())?;
        crate::update::unpack_archive(&archive, &unpacked)?;
        validate_unpacked_tree(&unpacked)?;

        let stage = app_dir.join(STAGE_DIR);
        let _ = std::fs::remove_dir_all(&stage);
        std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
        std::fs::rename(&unpacked, stage.join("content")).map_err(|e| e.to_string())?;
        let receipt = StagedReceipt {
            version: manifest.version.clone(),
            sha256: manifest.sha256.to_ascii_lowercase(),
            notes: manifest.notes.clone(),
        };
        let receipt_json = serde_json::to_string_pretty(&receipt).map_err(|e| e.to_string())?;
        std::fs::write(stage.join("ready.json"), receipt_json).map_err(|e| e.to_string())?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    stage_result?;

    let mut st = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    st.ready = Some(manifest.version.clone());
    st.ready_notes = manifest.notes.clone();
    st.error = None;
    Ok(Some(manifest.version))
}

fn validate_unpacked_tree(root: &Path) -> Result<(), String> {
    let mut has_content = false;
    fn walk(root: &Path, dir: &Path, has_content: &mut bool) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if meta.file_type().is_symlink() {
                return Err(format!("UI bundle must not contain symlinks: {}", path.display()));
            }
            let rel = path.strip_prefix(root).map_err(|e| e.to_string())?;
            let mut comps = rel.components();
            let Some(Component::Normal(first)) = comps.next() else {
                return Err(format!("invalid path in UI bundle: {}", rel.display()));
            };
            let first = first.to_string_lossy();
            if !matches!(first.as_ref(), "bundle.json" | "mods" | "frame" | "layouts" | "themes" | "icons") {
                return Err(format!("unexpected entry in UI bundle: {first}"));
            }
            if comps.any(|c| !matches!(c, Component::Normal(_))) {
                return Err(format!("unsafe path in UI bundle: {}", rel.display()));
            }
            if meta.is_dir() {
                walk(root, &path, has_content)?;
            } else if meta.is_file() {
                if matches!(first.as_ref(), "mods" | "frame" | "layouts" | "themes" | "icons") {
                    *has_content = true;
                }
            }
        }
        Ok(())
    }
    walk(root, root, &mut has_content)?;
    if !has_content {
        return Err("UI bundle contains no UI assets".into());
    }
    Ok(())
}

/// Applies the staged UI bundle into `app_dir`, backing up current files to `.ui-prev/`
/// for automatic rollback if reloading mods fails.
pub fn apply_staged(app_dir: &Path) -> Result<AppliedUi, String> {
    let staged = staged_receipt(app_dir).ok_or_else(|| "no staged UI update ready".to_owned())?;
    let content = app_dir.join(STAGE_DIR).join("content");
    validate_unpacked_tree(&content)?;

    // Backup current state for rollback.
    let prev = app_dir.join(PREV_DIR);
    let _ = std::fs::remove_dir_all(&prev);
    std::fs::create_dir_all(&prev).map_err(|e| e.to_string())?;
    for sub in ["mods", "frame", "layouts", "themes", "icons"] {
        let src = app_dir.join(sub);
        if src.is_dir() {
            copy_dir_all(&src, &prev.join(sub)).map_err(|e| format!("backup {sub}: {e}"))?;
        }
    }
    for f in ["installed.json", RECEIPT_FILE] {
        let src = app_dir.join(f);
        if src.is_file() {
            std::fs::copy(&src, prev.join(f)).map_err(|e| format!("backup {f}: {e}"))?;
        }
    }

    let apply_res = (|| -> Result<(), String> {
        // 1. Mods: use sync_defaults_owned so user-edited mods are safely preserved as *-edited.
        let mods_src = content.join("mods");
        if mods_src.is_dir() {
            let mut entries = Vec::new();
            collect_files_bytes(&mods_src, &mods_src, &mut entries).map_err(|e| e.to_string())?;
            crate::mods::sync_defaults_owned(&app_dir.join("mods"), &app_dir.join("installed.json"), &entries)
                .map_err(|e| format!("sync mods: {e}"))?;
        }

        // 2. Frame mods: app-owned, replaced safely (handles open directory watchers on Windows).
        let frame_src = content.join("frame");
        if frame_src.is_dir() {
            replace_dir_safe(&frame_src, &app_dir.join("frame"))
                .map_err(|e| format!("replace frame: {e}"))?;
        }

        // 3. Layouts: preserve user edits via sync_layouts.
        let layouts_src = content.join("layouts");
        if layouts_src.is_dir() {
            let mut json_layouts: Vec<(String, String)> = Vec::new();
            let mut previews: Vec<(String, Vec<u8>)> = Vec::new();
            for entry in std::fs::read_dir(&layouts_src).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let fname = entry.file_name().to_string_lossy().into_owned();
                if let Some(stem) = fname.strip_suffix(".json") {
                    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                    json_layouts.push((stem.to_owned(), text));
                } else {
                    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                    previews.push((fname, bytes));
                }
            }
            if !json_layouts.is_empty() {
                let borrowed: Vec<(&str, &str)> = json_layouts.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
                crate::mods::sync_layouts(app_dir, &borrowed).map_err(|e| format!("sync layouts: {e}"))?;
            }
            for (fname, bytes) in previews {
                let _ = crate::persist::write_atomic(&app_dir.join("layouts").join(fname), &bytes);
            }
        }

        // 4. Themes & icons: overwrite shipped files atomically.
        for sub in ["themes", "icons"] {
            let src = content.join(sub);
            if src.is_dir() {
                let dst = app_dir.join(sub);
                std::fs::create_dir_all(&dst).map_err(|e| e.to_string())?;
                copy_dir_all(&src, &dst).map_err(|e| format!("sync {sub}: {e}"))?;
            }
        }

        // 5. Write active receipt.
        let applied_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let receipt = UiReceipt {
            version: staged.version.clone(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            sha256: staged.sha256.clone(),
            applied_at,
            notes: staged.notes.clone(),
        };
        let text = serde_json::to_string_pretty(&receipt).map_err(|e| e.to_string())?;
        crate::persist::write_atomic(&app_dir.join(RECEIPT_FILE), text.as_bytes()).map_err(|e| e.to_string())?;
        Ok(())
    })();

    if let Err(err) = apply_res {
        let _ = rollback(app_dir, &staged.version, &err);
        return Err(err);
    }

    let _ = std::fs::remove_dir_all(app_dir.join(STAGE_DIR));
    let mut st = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    st.ready = None;
    st.ready_notes = None;
    st.applying = false;
    st.error = None;
    Ok(AppliedUi {
        version: staged.version,
        notes: staged.notes,
    })
}

/// Restores `.ui-prev/` back into `app_dir` and blacklists `failed_version`.
pub fn rollback(app_dir: &Path, failed_version: &str, reason: &str) -> Result<(), String> {
    let prev = app_dir.join(PREV_DIR);
    if !prev.is_dir() {
        record_failed_version(app_dir, failed_version);
        set_error(Some(format!("UI update {failed_version} failed ({reason}); no backup available")));
        return Err("no previous UI backup found".into());
    }
    for sub in ["mods", "frame", "layouts", "themes", "icons"] {
        let backup = prev.join(sub);
        let dst = app_dir.join(sub);
        if backup.is_dir() {
            replace_dir_safe(&backup, &dst).map_err(|e| format!("restore {sub}: {e}"))?;
        }
    }
    for f in ["installed.json", RECEIPT_FILE] {
        let backup = prev.join(f);
        let dst = app_dir.join(f);
        if backup.is_file() {
            std::fs::copy(&backup, &dst).map_err(|e| format!("restore {f}: {e}"))?;
        } else {
            let _ = std::fs::remove_file(&dst);
        }
    }
    let _ = std::fs::remove_dir_all(&prev);
    let _ = std::fs::remove_dir_all(app_dir.join(STAGE_DIR));
    record_failed_version(app_dir, failed_version);
    let mut st = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    st.ready = None;
    st.ready_notes = None;
    st.applying = false;
    st.error = Some(format!("UI update {failed_version} rolled back: {reason}"));
    Ok(())
}

/// Cleans up `.ui-prev/` once the window verifies the newly loaded mods are healthy.
pub fn commit_applied(app_dir: &Path) {
    let _ = std::fs::remove_dir_all(app_dir.join(PREV_DIR));
}

fn collect_files_bytes(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_files_bytes(root, &path, out)?;
        } else if path.is_file() {
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            out.push((rel, std::fs::read(&path)?));
        }
    }
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Replaces `dst` with the exact contents of `src`.
/// Tries an atomic sibling swap first; if `dst` cannot be renamed/removed (for example on Windows
/// when a directory watcher holds a handle on `dst`), falls back to in-place file sync + stale cleanup.
fn replace_dir_safe(src: &Path, dst: &Path) -> std::io::Result<()> {
    let parent = dst.parent().ok_or_else(|| std::io::Error::other("target dir has no parent"))?;
    let name = dst.file_name().and_then(|s| s.to_str()).unwrap_or("dir");
    let staged = parent.join(format!(".{name}-new"));
    let old = parent.join(format!(".{name}-old"));
    let _ = std::fs::remove_dir_all(&staged);
    let _ = std::fs::remove_dir_all(&old);
    copy_dir_all(src, &staged)?;

    let moved_old = if dst.exists() {
        std::fs::rename(dst, &old).is_ok()
    } else {
        true
    };
    if moved_old {
        match std::fs::rename(&staged, dst) {
            Ok(()) => {
                let _ = std::fs::remove_dir_all(&old);
                return Ok(());
            }
            Err(_) if old.exists() => {
                let _ = std::fs::rename(&old, dst);
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staged);
                return Err(e);
            }
        }
    }

    // Fallback: sync in-place when `dst` is pinned by an open directory handle (e.g. Windows watcher).
    let _ = std::fs::remove_dir_all(&staged);
    sync_dir_in_place(src, dst)
}

fn sync_dir_in_place(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    let mut keep = BTreeSet::new();
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let fname = entry.file_name();
        keep.insert(fname.clone());
        let from = entry.path();
        let to = dst.join(&fname);
        if entry.file_type()?.is_dir() {
            sync_dir_in_place(&from, &to)?;
        } else {
            let bytes = std::fs::read(&from)?;
            if std::fs::read(&to).ok().as_deref() != Some(bytes.as_slice()) {
                std::fs::write(&to, &bytes)?;
            }
        }
    }
    for entry in std::fs::read_dir(dst)? {
        let entry = entry?;
        if !keep.contains(&entry.file_name()) {
            let p = entry.path();
            if entry.file_type()?.is_dir() {
                let _ = std::fs::remove_dir_all(&p);
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = crate::test_tmp().join(format!(
            "herdr2-ui-update-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn ui_version_comparison_treats_ui_suffix_as_newer_than_base() {
        assert!(is_newer_ui_version("0.2.0-ui.1", "0.2.0"));
        assert!(is_newer_ui_version("0.2.0-ui.2", "0.2.0-ui.1"));
        assert!(is_newer_ui_version("0.2.0-ui.10", "0.2.0-ui.2"));
        assert!(is_newer_ui_version("0.2.0+ui.1", "0.2.0"));
        assert!(is_newer_ui_version("0.2.1", "0.2.0-ui.9"));
        assert!(is_newer_ui_version("0.2.1", "0.2.0"));
        assert!(!is_newer_ui_version("0.2.0", "0.2.0-ui.1"));
        assert!(!is_newer_ui_version("0.2.0-ui.1", "0.2.0-ui.1"));
        assert!(!is_newer_ui_version("0.2.0-rc.1", "0.2.0"));
    }

    #[test]
    fn replace_dir_safe_replaces_contents_and_prunes_stale_files() {
        let work = temp_dir("replace-dir");
        let src = work.join("src");
        let dst = work.join("dst");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("nested/a.js"), "new-a").unwrap();
        std::fs::create_dir_all(dst.join("nested")).unwrap();
        std::fs::write(dst.join("nested/a.js"), "old-a").unwrap();
        std::fs::write(dst.join("stale.js"), "gone").unwrap();

        replace_dir_safe(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("nested/a.js")).unwrap(), "new-a");
        assert!(!dst.join("stale.js").exists());

        // Also test the in-place sync fallback directly.
        std::fs::write(dst.join("extra.js"), "stale-2").unwrap();
        sync_dir_in_place(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("nested/a.js")).unwrap(), "new-a");
        assert!(!dst.join("extra.js").exists());

        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn compatibility_checks_generation_and_app_version_bounds() {
        let base = UiManifest {
            version: "0.2.0-ui.1".into(),
            app_version_min: Some("0.2.0".into()),
            app_version_max: Some("0.2.9".into()),
            host_api_generation: 1,
            url: "http://example/ui.tar.gz".into(),
            sha256: "00".into(),
            notes: None,
        };
        assert!(is_compatible(&base, "0.2.0", 1));
        assert!(is_compatible(&base, "0.2.5", 1));
        assert!(!is_compatible(&base, "0.1.9", 1));
        assert!(!is_compatible(&base, "0.3.0", 1));
        assert!(!is_compatible(&base, "0.2.0", 2));
    }

    #[test]
    fn validator_rejects_unexpected_top_level_and_symlinks() {
        let dir = temp_dir("validate");
        std::fs::create_dir_all(dir.join("mods/demo")).unwrap();
        std::fs::write(dir.join("mods/demo/main.js"), "export default {}").unwrap();
        assert!(validate_unpacked_tree(&dir).is_ok());

        std::fs::write(dir.join("evil.sh"), "rm -rf /").unwrap();
        assert!(validate_unpacked_tree(&dir).is_err());
        std::fs::remove_file(dir.join("evil.sh")).unwrap();

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", dir.join("mods/demo/link")).unwrap();
            assert!(validate_unpacked_tree(&dir).is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn end_to_end_download_apply_preserves_user_edits_and_rolls_back() {
        let work = temp_dir("e2e");
        let app_dir = work.join("app");
        let server_root = work.join("server");
        std::fs::create_dir_all(app_dir.join("mods/demo")).unwrap();
        std::fs::create_dir_all(app_dir.join("mods/custom")).unwrap();
        std::fs::create_dir_all(app_dir.join("frame/settings")).unwrap();
        std::fs::write(app_dir.join("mods/demo/main.js"), "// user edited\n").unwrap();
        std::fs::write(app_dir.join("mods/custom/main.js"), "// third party\n").unwrap();
        std::fs::write(app_dir.join("frame/settings/main.js"), "// old settings\n").unwrap();

        // Build a valid ui-bundle.tar.gz on disk.
        let bundle_src = work.join("bundle-src");
        std::fs::create_dir_all(bundle_src.join("mods/demo")).unwrap();
        std::fs::create_dir_all(bundle_src.join("frame/settings")).unwrap();
        std::fs::write(bundle_src.join("mods/demo/main.js"), "// v9.9.9 demo\n").unwrap();
        std::fs::write(bundle_src.join("frame/settings/main.js"), "// v9.9.9 settings\n").unwrap();

        std::fs::create_dir_all(server_root.join("ui/9.9.9")).unwrap();
        let archive = server_root.join("ui/9.9.9/herdr2-ui-bundle.tar.gz");
        let status = std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .arg("-C")
            .arg(&bundle_src)
            .args(["mods", "frame"])
            .status()
            .unwrap();
        assert!(status.success());
        let sha = crate::update::sha256(&std::fs::read(&archive).unwrap());
        let manifest = serde_json::json!({
            "version": "9.9.9",
            "app_version_min": env!("CARGO_PKG_VERSION"),
            "host_api_generation": crate::mods::UI_HOST_API_GENERATION,
            "url": format!("file://{}", archive.display()),
            "sha256": sha,
            "notes": "Hotfix UI"
        });
        std::fs::write(server_root.join("ui/latest.json"), manifest.to_string()).unwrap();

        let releases_url = format!("file://{}", server_root.display());
        let ready = check_from(&app_dir, &releases_url).unwrap();
        assert_eq!(ready.as_deref(), Some("9.9.9"));

        let applied = apply_staged(&app_dir).unwrap();
        assert_eq!(applied.version, "9.9.9");
        assert_eq!(std::fs::read_to_string(app_dir.join("mods/demo/main.js")).unwrap(), "// v9.9.9 demo\n");
        // User-edited mod was preserved as demo-edited!
        assert_eq!(std::fs::read_to_string(app_dir.join("mods/demo-edited/main.js")).unwrap(), "// user edited\n");
        // Third-party mod untouched!
        assert_eq!(std::fs::read_to_string(app_dir.join("mods/custom/main.js")).unwrap(), "// third party\n");
        assert_eq!(std::fs::read_to_string(app_dir.join("frame/settings/main.js")).unwrap(), "// v9.9.9 settings\n");
        assert!(has_active_override(&app_dir));

        // Now simulate a failed reload -> automatic rollback.
        rollback(&app_dir, "9.9.9", "syntax error").unwrap();
        assert_eq!(std::fs::read_to_string(app_dir.join("mods/demo/main.js")).unwrap(), "// user edited\n");
        assert_eq!(std::fs::read_to_string(app_dir.join("frame/settings/main.js")).unwrap(), "// old settings\n");
        assert!(!has_active_override(&app_dir));
        // Failed version is blacklisted so next check skips it.
        assert_eq!(check_from(&app_dir, &releases_url).unwrap(), None);

        let _ = std::fs::remove_dir_all(&work);
    }
}
