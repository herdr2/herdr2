//! Released builds from a configured release server (`HERDR2_RELEASES_URL` or
//! `releases_url` in config.toml): the list (`core/latest.json`, or
//! `core/<version>/manifest.json` for one version) and its checked
//! downloads. curl, sha256sum/shasum (Windows: its own crypto) and tar do
//! the work: no HTTP or crypto code in h2 for something run this rarely.

use crate::rpc::RpcError;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;

#[derive(Deserialize)]
pub struct Manifest {
    pub version: String,
    pub generation: u32,
    pub targets: BTreeMap<String, Target>,
}

#[derive(Deserialize)]
pub struct Target {
    pub url: String,
    pub sha256: String,
}

fn normalize_url(raw: Option<String>) -> Option<String> {
    raw.map(|u| u.trim().trim_end_matches('/').to_owned()).filter(|u| !u.is_empty())
}

fn resolve_base(
    env_url: Option<String>,
    cfg_url: Option<String>,
    default_url: Option<&str>,
) -> Result<String, RpcError> {
    normalize_url(env_url)
        .or_else(|| normalize_url(cfg_url))
        .or_else(|| normalize_url(default_url.map(str::to_owned)))
        .ok_or_else(|| {
            RpcError::new(
                "unavailable",
                "automatic core updates require HERDR2_RELEASES_URL or releases_url in config.toml",
                "set HERDR2_RELEASES_URL, add releases_url to ~/.config/herdr2/config.toml, or run h2 update --from <path>",
            )
        })
}

fn base() -> Result<String, RpcError> {
    let env_url = std::env::var("HERDR2_RELEASES_URL").ok();
    let cfg_url = crate::paths::Paths::from_env()
        .ok()
        .and_then(|paths| crate::config::load(&paths.config).ok())
        .and_then(|cfg| cfg.releases_url);
    resolve_base(env_url, cfg_url, option_env!("HERDR2_DEFAULT_RELEASES_URL"))
}

fn unavailable(why: String) -> RpcError {
    RpcError::new("unavailable", why, "check your connection or HERDR2_RELEASES_URL")
}

fn curl_program() -> PathBuf {
    match std::env::var_os("SystemRoot") {
        Some(root) if cfg!(windows) => Path::new(&root).join(r"System32\curl.exe"),
        _ => "curl".into(),
    }
}

fn curl(url: &str, to: &Path) -> Result<(), RpcError> {
    // Quiet: our error says what failed, and piped stderr is one JSON error.
    let mut cmd = crate::platform::bg_command(curl_program());
    // Windows System32 curl.exe uses Schannel, which fails with CRYPT_E_REVOCATION_OFFLINE
    // when firewalls/proxies block CRL/OCSP endpoints (still verifies TLS certs & hostnames).
    if cfg!(windows) {
        cmd.arg("--ssl-no-revoke");
    }
    let ok = cmd
        .args(["-fsL", "--retry", "2", "--connect-timeout", "10", "-o"])
        .arg(to)
        .arg(url)
        .stderr(std::process::Stdio::null())
        .status();
    match ok {
        Ok(s) if s.success() => Ok(()),
        _ => Err(unavailable(format!("cannot download {url}"))),
    }
}

fn is_github_release_base(base: &str) -> bool {
    base.contains("/releases/download/") || base.ends_with("/releases/latest/download")
}

/// The latest release's list, or one version's.
/// Supports both hierarchical static servers (`core/latest.json`) and flat GitHub Releases
/// asset URLs (`core-latest.json`).
pub fn manifest(version: Option<&str>, scratch: &Path) -> Result<Manifest, RpcError> {
    let base = base()?;
    let url = if is_github_release_base(&base) {
        match version {
            Some(v) => format!("{base}/core-{v}-manifest.json"),
            None => format!("{base}/core-latest.json"),
        }
    } else {
        match version {
            Some(v) => format!("{base}/core/{v}/manifest.json"),
            None => format!("{base}/core/latest.json"),
        }
    };
    let file = scratch.join("manifest.json");
    curl(&url, &file)?;
    let bytes = std::fs::read(&file).map_err(|e| unavailable(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| unavailable(format!("{url} is not a release list: {e}")))
}

/// Windows' own tar opens zips; Git Bash's GNU tar, often first on PATH there, can't.
fn tar_program() -> PathBuf {
    match std::env::var_os("SystemRoot") {
        Some(root) if cfg!(windows) => Path::new(&root).join(r"System32\tar.exe"),
        _ => "tar".into(),
    }
}

#[cfg(windows)]
fn sha256(file: &Path) -> Option<String> {
    crate::platform::sha256(&std::fs::read(file).ok()?)
}

#[cfg(unix)]
fn sha256(file: &Path) -> Option<String> {
    let run =
        |cmd: &str, args: &[&str]| Command::new(cmd).args(args).arg(file).output().ok().filter(|o| o.status.success());
    let out = run("sha256sum", &[]).or_else(|| run("shasum", &["-a", "256"]))?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().next().map(String::from)
}

/// Downloads `m`'s build for `target` (`linux-x86_64`) into `scratch`,
/// checks it against the list, unpacks it: the path of its herdr2 binary.
pub fn download(m: &Manifest, target: &str, scratch: &Path, bundle: bool) -> Result<PathBuf, RpcError> {
    let tar = fetch(m, target, scratch)?;
    let exe = if target.starts_with("windows") { "herdr2.exe" } else { "herdr2" };
    let ok = crate::platform::bg_command(tar_program())
        .arg("-xf")
        .arg(&tar)
        .arg("-C")
        .arg(scratch)
        .status()
        .is_ok_and(|s| s.success());
    // Root executable remains in macOS archives for old updaters. New ones
    // carry the entire signed bundle through installation instead.
    let bundled = scratch.join("Herdr2.app/Contents/MacOS/herdr2");
    let bin = if bundle && target.starts_with("macos-") && bundled.is_file() { bundled } else { scratch.join(exe) };
    if !ok || !bin.is_file() {
        return Err(RpcError::new("refused", format!("h2 {}'s download for {target} holds no herdr2", m.version), ""));
    }
    crate::platform::verify_release_bundle(&bin)
        .map_err(|e| RpcError::new("refused", format!("invalid release signature: {e}"), ""))?;
    Ok(bin)
}

/// Downloads `m`'s archive for `target` into `scratch` and checks it against
/// the list: its path, still packed.
pub fn fetch(m: &Manifest, target: &str, scratch: &Path) -> Result<PathBuf, RpcError> {
    let t = m.targets.get(target).ok_or_else(|| {
        RpcError::new("refused", format!("h2 {} has no build for {target}", m.version), "install h2 there yourself")
    })?;
    let archive = if target.starts_with("windows") { "zip" } else { "tar.gz" };
    let file = scratch.join(format!("herdr2-{target}.{archive}"));
    curl(&t.url, &file)?;
    if sha256(&file).as_deref() != Some(t.sha256.as_str()) {
        return Err(RpcError::new(
            "refused",
            format!("the download of h2 {} for {target} is damaged (checksum mismatch)", m.version),
            "try again",
        ));
    }
    Ok(file)
}

/// A private folder for one download, removed when dropped.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new() -> Result<Scratch, RpcError> {
        let dir =
            std::env::temp_dir().join(format!("h2-download-{}-{}", std::process::id(), crate::platform::random_hex()));
        std::fs::create_dir_all(&dir).map_err(|e| RpcError::internal(e.to_string()))?;
        Ok(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// This machine as release targets name it: `linux-x86_64`, `macos-aarch64`.
pub fn this_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_url_prefers_env_then_config_then_default() {
        assert_eq!(
            resolve_base(
                Some(" https://env.example/ ".into()),
                Some("https://cfg.example".into()),
                Some("https://def.example")
            )
            .unwrap(),
            "https://env.example"
        );
        assert_eq!(
            resolve_base(Some("  ".into()), Some("https://cfg.example/".into()), Some("https://def.example")).unwrap(),
            "https://cfg.example"
        );
        assert_eq!(resolve_base(None, None, Some("https://def.example/")).unwrap(), "https://def.example");
        assert!(resolve_base(None, None, None).is_err());
    }
}
