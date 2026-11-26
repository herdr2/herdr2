//! Starting programs. The app has no console on Windows, so each console
//! program it starts (git, hc, curl) would open a window of its own.

use std::ffi::OsStr;
use std::process::Command;

pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Guard against introducing raw `Command::new(...)` calls in Windows-reachable
    /// app production code without going through `crate::spawn::command(...)`.
    #[test]
    fn windows_production_spawns_use_spawn_command() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let allowlisted = [
            "spawn.rs",
            // Unix / Linux / macOS-only command paths:
            "core_bin.rs",
            "files.rs",
            "links.rs",
            "macos_check.rs",
            "update.rs",
        ];
        let mut files = Vec::new();
        collect_rs_files(&src, &mut files);
        let mut offenders = Vec::new();
        for path in files {
            let rel = path.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
            if rel.ends_with("_tests.rs") || rel.ends_with("/tests.rs") || allowlisted.contains(&rel.as_str()) {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let prod = text
                .split("#[cfg(test)]")
                .next()
                .unwrap_or(&text)
                .split("#[cfg(all(test,")
                .next()
                .unwrap_or(&text);
            for (idx, line) in prod.lines().enumerate() {
                if line.contains("Command::new(") {
                    offenders.push(format!("{rel}:{}: {}", idx + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "Found raw Command::new(...) in app production code; use crate::spawn::command(...) so Windows does not flash console windows:\n{}",
            offenders.join("\n")
        );
    }
}
