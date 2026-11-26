//! Embeds the default mods (defaults/mods/**), the program icons
//! (defaults/icons/*) and the frame's mods (defaults/frame/**) so Herdr can
//! install them.
//!
//! Worktrees may share one target dir, and with it this script and its output.
//! So nothing here names this checkout: paths are relative to the package and
//! the generated code includes files through the compiling crate's
//! `CARGO_MANIFEST_DIR`.

use std::path::Path;

fn walk(dir: &Path, base: &Path, root: &Path, out: &mut Vec<String>) {
    if let Ok(rel_dir) = dir.strip_prefix(base) {
        let rel_dir = rel_dir.to_string_lossy().replace('\\', "/");
        if !rel_dir.is_empty() {
            println!("cargo:rerun-if-changed={rel_dir}");
        }
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, base, root, out);
        } else {
            let path = p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
            println!("cargo:rerun-if-changed={path}");
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            out.push(format!("    ({rel:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/{path}\"))),"));
        }
    }
}

fn embed(base: &Path, folder: &str, name: &str, file: &str) {
    println!("cargo:rerun-if-changed={folder}");
    let mut lines = Vec::new();
    let root = base.join(folder);
    walk(&root, base, &root, &mut lines);
    let code = format!("pub const {name}: &[(&str, &[u8])] = &[\n{}\n];\n", lines.join("\n"));
    std::fs::write(Path::new(&std::env::var("OUT_DIR").unwrap()).join(file), code).unwrap();
}

fn main() {
    let base = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    embed(&base, "defaults/mods", "DEFAULT_MODS", "default_mods.rs");
    embed(&base, "defaults/icons", "DEFAULT_ICONS", "default_icons.rs");
    embed(&base, "defaults/frame", "FRAME_MODS", "frame_mods.rs");

    // Windows takes the window, taskbar and Start menu icon from the program file.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/icons/herdr-app.ico");
        #[cfg(windows)]
        {
            winresource::WindowsResource::new()
                .set_icon("assets/icons/herdr-app.ico")
                .compile()
                .expect("cannot embed the icon");
        }
        #[cfg(not(windows))]
        {
            embed_windows_icon_cross(&base);
        }
    }
}

#[cfg(not(windows))]
fn embed_windows_icon_cross(base: &Path) {
    use std::process::Command;

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = base.join("assets/icons/herdr-app.ico");
    let rc_path = out_dir.join("herdr-app-icon.rc");
    let res_path = out_dir.join("herdr-app-icon.res");

    let ico_escaped = ico.to_string_lossy().replace('\\', "/");
    std::fs::write(&rc_path, format!("1 ICON \"{ico_escaped}\"\n"))
        .expect("cannot write Windows icon resource script");

    // Emit a raw Win32 `.res` file so lld-link / link.exe merges our icon cleanly with
    // any manifest `.res` contributed by dependencies (passing a pre-converted `.obj`
    // conflicts with lld-link's internal `.res` merge).
    let built_with_llvm = Command::new("llvm-rc")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .status()
        .is_ok_and(|s| s.success());

    let built = built_with_llvm || {
        let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "x86_64".into());
        let windres_candidates = if arch == "x86" {
            ["i686-w64-mingw32-windres", "windres"]
        } else {
            ["x86_64-w64-mingw32-windres", "windres"]
        };
        windres_candidates.into_iter().any(|tool| {
            Command::new(tool)
                .args(["-O", "res", "-i"])
                .arg(&rc_path)
                .arg("-o")
                .arg(&res_path)
                .status()
                .is_ok_and(|s| s.success())
        })
    };

    if built && res_path.is_file() {
        println!("cargo:rustc-link-arg-bin=herdr2={}", res_path.display());
    } else {
        println!(
            "cargo:warning=could not compile Windows icon resource on cross-build (install llvm-rc or mingw-w64 windres)"
        );
    }
}
