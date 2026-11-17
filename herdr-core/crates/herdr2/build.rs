use std::path::{Path, PathBuf};

fn walk_dist(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
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
    println!("cargo:rerun-if-changed=../../../drove-web/dist");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dist = manifest.join("../../../drove-web/dist");
    let mut lines = Vec::new();
    if dist.is_dir() {
        walk_dist(&dist, &dist, &mut lines);
    }
    let code = format!("pub const DROVE_WEB_ASSETS: &[(&str, &[u8])] = &[\n{}\n];\n", lines.join("\n"));
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out_dir.join("drove_web_assets.rs"), code).unwrap();
}

fn main() {
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
