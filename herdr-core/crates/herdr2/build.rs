use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

const NPM_BIN: &str = if cfg!(windows) { "npm.cmd" } else { "npm" };
const VITE_BIN_REL: &str = if cfg!(windows) { "node_modules/.bin/vite.cmd" } else { "node_modules/.bin/vite" };

struct BuildLock(PathBuf);

impl Drop for BuildLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn acquire_build_lock(lock_path: &Path) -> Result<BuildLock, String> {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        match OpenOptions::new().write(true).create_new(true).open(lock_path) {
            Ok(_) => return Ok(BuildLock(lock_path.to_path_buf())),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if let Ok(meta) = std::fs::metadata(lock_path)
                    && let Ok(mtime) = meta.modified()
                    && SystemTime::now().duration_since(mtime).unwrap_or_default() > Duration::from_secs(600)
                {
                    let _ = std::fs::remove_file(lock_path);
                    continue;
                }
                if Instant::now() >= deadline {
                    return Err(format!("timed out waiting for {}", lock_path.display()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("cannot create {}: {e}", lock_path.display())),
        }
    }
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn max_mtime(acc: Option<SystemTime>, cand: Option<SystemTime>) -> Option<SystemTime> {
    match (acc, cand) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

fn track_tree(dir: &Path, web_root: &Path, latest: &mut Option<SystemTime>) {
    if !dir.is_dir() {
        return;
    }
    if let Ok(rel) = dir.strip_prefix(web_root) {
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if rel_str.is_empty() {
            println!("cargo:rerun-if-changed=../../../drove-web");
        } else {
            println!("cargo:rerun-if-changed=../../../drove-web/{rel_str}");
        }
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            track_tree(&p, web_root, latest);
        } else if p.is_file() {
            if let Ok(rel) = p.strip_prefix(web_root) {
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                println!("cargo:rerun-if-changed=../../../drove-web/{rel_str}");
            }
            *latest = max_mtime(*latest, file_mtime(&p));
        }
    }
}

fn track_web_inputs(web_dir: &Path) -> Option<SystemTime> {
    let mut latest = None;
    for name in ["index.html", "package.json", "package-lock.json", "tsconfig.json", "vite.config.ts"] {
        let p = web_dir.join(name);
        if p.is_file() {
            println!("cargo:rerun-if-changed=../../../drove-web/{name}");
            latest = max_mtime(latest, file_mtime(&p));
        }
    }
    track_tree(&web_dir.join("src"), web_dir, &mut latest);
    track_tree(&web_dir.join("public"), web_dir, &mut latest);
    latest
}

fn dist_is_fresh(dist_dir: &Path, latest_src: Option<SystemTime>) -> bool {
    let Some(index_mtime) = file_mtime(&dist_dir.join("index.html")) else {
        return false;
    };
    let assets_dir = dist_dir.join("assets");
    let has_assets = std::fs::read_dir(&assets_dir)
        .ok()
        .is_some_and(|mut rd| rd.any(|e| e.ok().is_some_and(|f| f.path().is_file())));
    if !has_assets {
        return false;
    }
    latest_src.is_none_or(|src| index_mtime >= src)
}

fn needs_npm_install(web_dir: &Path) -> bool {
    if !web_dir.join(VITE_BIN_REL).is_file() {
        return true;
    }
    let lock_src = web_dir.join("package-lock.json");
    let lock_installed = web_dir.join("node_modules/.package-lock.json");
    if let Some(src_t) = file_mtime(&lock_src) {
        return file_mtime(&lock_installed).is_none_or(|inst_t| inst_t < src_t);
    }
    if let Some(pkg_t) = file_mtime(&web_dir.join("package.json")) {
        return file_mtime(&web_dir.join("node_modules")).is_none_or(|nm_t| nm_t < pkg_t);
    }
    false
}

fn run_npm(web_dir: &Path, args: &[&str]) -> Result<(), String> {
    let out = Command::new(NPM_BIN)
        .args(args)
        .current_dir(web_dir)
        .output()
        .map_err(|e| format!("failed to run `{NPM_BIN} {}`: {e}", args.join(" ")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = if !stderr.trim().is_empty() { stderr.trim() } else { stdout.trim() };
        return Err(format!("`{NPM_BIN} {}` exited with {}: {detail}", args.join(" "), out.status));
    }
    Ok(())
}

fn ensure_web_bundle(web_dir: &Path, dist_dir: &Path, latest_src: Option<SystemTime>) {
    let skip = std::env::var("H2_SKIP_WEB_BUILD").as_deref() == Ok("1");
    let require_bundle = std::env::var("H2_REQUIRE_WEB_BUNDLE").as_deref() == Ok("1");
    let is_release = std::env::var("PROFILE").as_deref() == Ok("release");

    if !web_dir.join("package.json").is_file() {
        if require_bundle && !dist_dir.join("index.html").is_file() {
            panic!("H2_REQUIRE_WEB_BUNDLE=1 but {} does not exist", dist_dir.join("index.html").display());
        }
        return;
    }

    if skip || dist_is_fresh(dist_dir, latest_src) {
        if require_bundle && !dist_dir.join("index.html").is_file() {
            panic!("H2_REQUIRE_WEB_BUNDLE=1 but {} does not exist", dist_dir.join("index.html").display());
        }
        return;
    }

    let build_res = (|| -> Result<(), String> {
        let _lock = acquire_build_lock(&web_dir.join(".build.lock"))?;
        if dist_is_fresh(dist_dir, latest_src) {
            return Ok(());
        }
        if needs_npm_install(web_dir) {
            let install_cmd = if web_dir.join("package-lock.json").is_file() { "ci" } else { "install" };
            run_npm(web_dir, &[install_cmd])?;
        }
        run_npm(web_dir, &["run", "build"])?;
        if !dist_dir.join("index.html").is_file() {
            return Err(format!("build completed without producing {}", dist_dir.join("index.html").display()));
        }
        Ok(())
    })();

    if let Err(err) = build_res {
        if require_bundle || (is_release && !dist_dir.join("index.html").is_file()) {
            panic!(
                "failed to build drove-web bundle: {err}\nInstall Node.js/npm or set H2_SKIP_WEB_BUILD=1 to bypass."
            );
        }
        println!("cargo:warning=failed to auto-build drove-web ({err}); using existing dist or fallback");
    }
}

fn walk_dist(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            println!("cargo:rerun-if-changed=../../../drove-web/dist/{rel}");
            walk_dist(&p, root, out);
        } else if p.is_file() {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            println!("cargo:rerun-if-changed=../../../drove-web/dist/{rel}");
            out.push(format!(
                "    ({rel:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../../drove-web/dist/{rel}\"))),"
            ));
        }
    }
}

fn emit_drove_web_assets() {
    println!("cargo:rerun-if-env-changed=H2_SKIP_WEB_BUILD");
    println!("cargo:rerun-if-env-changed=H2_REQUIRE_WEB_BUNDLE");
    println!("cargo:rerun-if-changed=../../../drove-web/dist");
    println!("cargo:rerun-if-changed=../../../drove-web/dist/index.html");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let web_dir = manifest.join("../../../drove-web");
    let dist = web_dir.join("dist");
    let latest_src = track_web_inputs(&web_dir);
    ensure_web_bundle(&web_dir, &dist, latest_src);
    let mut lines = Vec::new();
    if dist.is_dir() {
        walk_dist(&dist, &dist, &mut lines);
    }
    let code = format!("pub const DROVE_WEB_ASSETS: &[(&str, &[u8])] = &[\n{}\n];\n", lines.join("\n"));
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out_dir.join("drove_web_assets.rs"), code).unwrap();
}

fn walk_rerun_dir(dir: &Path, base: &Path) {
    if let Ok(rel) = dir.strip_prefix(base) {
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !rel.is_empty() {
            println!("cargo:rerun-if-changed={rel}");
        }
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk_rerun_dir(&p, base);
        } else if p.is_file()
            && let Ok(rel) = p.strip_prefix(base)
        {
            println!("cargo:rerun-if-changed={}", rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn emit_build_identity() {
    let pkg_ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    // Watch crate sources & manifest so incremental dirty builds refresh H2_BUILD_TIME and H2_BUILD_GIT_SHA.
    println!("cargo:rerun-if-changed=Cargo.toml");
    walk_rerun_dir(&manifest.join("src"), &manifest);
    // Locate nearest .git directory up to workspace parent so build sha refreshes on HEAD switch or staging.
    for ancestor in [manifest.join("../.."), manifest.join("../../..")] {
        let git_head = ancestor.join(".git/HEAD");
        if git_head.is_file() {
            println!("cargo:rerun-if-changed={}", git_head.display());
            let git_index = ancestor.join(".git/index");
            if git_index.is_file() {
                println!("cargo:rerun-if-changed={}", git_index.display());
            }
            break;
        }
    }
    let git_sha = Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .current_dir(&manifest)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(&manifest)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty());
    let sha_tag = if dirty && git_sha != "unknown" { format!("{git_sha}+dirty") } else { git_sha };
    let build_time = format_utc_now();
    let long_ver = format!("{pkg_ver} ({sha_tag} {build_time})");
    println!("cargo:rustc-env=H2_BUILD_GIT_SHA={sha_tag}");
    println!("cargo:rustc-env=H2_BUILD_TIME={build_time}");
    println!("cargo:rustc-env=H2_LONG_VERSION={long_ver}");
}

fn format_utc_now() -> String {
    let secs = SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let hour = sod / 3600;
    let min = (sod % 3600) / 60;
    let sec = sod % 60;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}Z")
}

fn main() {
    emit_build_identity();
    emit_drove_web_assets();
    println!("cargo:rerun-if-changed=../../distribution/macos-Info.plist");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let plist = include_str!("../../distribution/macos-Info.plist")
        .replace("@VERSION@", &std::env::var("CARGO_PKG_VERSION").unwrap());
    let path = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("Info.plist");
    std::fs::write(&path, plist).unwrap();
    // Separate arguments preserve paths containing spaces.
    for arg in ["-sectcreate", "__TEXT", "__info_plist"] {
        println!("cargo:rustc-link-arg-bin=herdr2=-Wl,{arg}");
    }
    println!("cargo:rustc-link-arg-bin=herdr2={}", path.display());
}
