//! What Herdr2 needs to know about the ZCode CLI (`zcode`): where its hooks live
//! and what it is told after a rollback.
//!
//! ZCode reads hooks from one runtime config shared by every session
//! (`~/.zcode/cli/config.json`), so like `agy`'s plugin they are registered
//! there when Herdr2 starts a `zcode`. Outside a Herdr2 terminal (`H2_TERMINAL_ID`
//! unset) `h2 checkpoint hook` answers `{}` at once without connecting to
//! anything, so other ZCode sessions go on undisturbed.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// `(event, hook phase, timeout in ms)` for each hook Herdr2 listens to.
const HOOKS: [(&str, &str, u64); 3] =
    [("PreToolUse", "guard", 10_000), ("UserPromptSubmit", "pre", 10_000), ("Stop", "post", 15_000)];

/// `ZCODE_DATA_BASE_DIR` moves ZCode's data; unset or empty means the home folder.
fn base_dir(home: &Path, data_base_dir: Option<&str>) -> PathBuf {
    data_base_dir.map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.to_path_buf())
}

/// ZCode's runtime config: the one file its hooks are read from.
pub fn config_path(home: &Path) -> PathBuf {
    base_dir(home, std::env::var("ZCODE_DATA_BASE_DIR").ok().as_deref()).join(".zcode").join("cli").join("config.json")
}

/// Herdr2's hook for `phase`, run without a shell so a path with spaces is safe.
fn hook(h2_exe: &str, phase: &str, timeout_ms: u64) -> Value {
    json!({
        "type": "process",
        "command": h2_exe,
        "args": ["checkpoint", "hook", "--source", "zcode", "--phase", phase],
        "timeoutMs": timeout_ms,
    })
}

/// Whether `v` is Herdr2's own `--source zcode` hook, whatever its age: an older
/// copy (another `h2` path) is replaced on the next registration.
fn is_ours(v: &Value) -> bool {
    let by_args = v
        .get("args")
        .and_then(Value::as_array)
        .is_some_and(|a| a.windows(2).any(|w| w[0] == "--source" && w[1] == "zcode"));
    let by_command =
        v.get("command").and_then(Value::as_str).is_some_and(|c| c.contains("checkpoint hook --source zcode"));
    by_args || by_command
}

/// Adds Herdr2's hooks to ZCode's runtime config, leaving everything else in it as
/// it is. Returns whether the file on disk is behind `config`.
///
/// Hooks are one switch for all of ZCode's sessions: turning them on for Herdr2
/// also turns on any other hooks the config lists.
pub fn merge(config: &mut Value, h2_exe: &str) -> Result<bool, String> {
    if !config.is_object() {
        return Err("the ZCode runtime config is not a JSON object".into());
    }
    let hooks = config.as_object_mut().expect("checked").entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        return Err("`hooks` in the ZCode runtime config is not an object".into());
    }
    let mut changed = false;
    let hooks = hooks.as_object_mut().expect("checked");
    let enabled = hooks.entry("enabled").or_insert(Value::Bool(false));
    if enabled != &Value::Bool(true) {
        *enabled = Value::Bool(true);
        changed = true;
    }
    let events = hooks.entry("events").or_insert_with(|| json!({}));
    if !events.is_object() {
        return Err("`hooks.events` in the ZCode runtime config is not an object".into());
    }
    let events = events.as_object_mut().expect("checked");
    for (event, phase, timeout_ms) in HOOKS {
        let want = hook(h2_exe, phase, timeout_ms);
        let list = events.entry(event).or_insert_with(|| json!([]));
        if !list.is_array() {
            return Err(format!("`hooks.events.{event}` in the ZCode runtime config is not a list"));
        }
        let groups = list.as_array_mut().expect("checked");
        let mut present = false;
        for group in groups.iter_mut() {
            let Some(own) = group.as_object_mut().and_then(|g| g.get_mut("hooks")).and_then(Value::as_array_mut) else {
                continue;
            };
            let before = own.len();
            own.retain(|h| {
                if *h == want {
                    present = true;
                    true
                } else {
                    !is_ours(h)
                }
            });
            changed |= own.len() != before;
        }
        // A group is a matcher and its hooks; one left without hooks says nothing.
        let before = groups.len();
        groups.retain(|g| g.pointer("/hooks").and_then(Value::as_array).is_none_or(|a| !a.is_empty()));
        changed |= groups.len() != before;
        if !present {
            groups.push(json!({ "hooks": [want] }));
            changed = true;
        }
    }
    Ok(changed)
}

/// Makes sure ZCode's runtime config has Herdr2's hooks, and returns its path.
/// A config that isn't JSON, or whose hooks aren't shaped as ZCode expects, is
/// left alone and said so: never rewritten into something its owner didn't write.
pub fn ensure_hooks_registered(home: &Path, h2_exe: &str) -> Result<PathBuf, String> {
    let path = config_path(home);
    let mut config: Value = match std::fs::read(&path) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => json!({}),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("{} is not valid JSON ({e}); Herdr2 left it alone", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    if !merge(&mut config, h2_exe)? {
        return Ok(path);
    }
    let bytes = serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?;
    let dir = path.parent().ok_or_else(|| format!("{} has no folder", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    // ZCode rewrites this file itself; keep what it last wrote beside it.
    let backup = path.with_file_name("config.json.bak-herdr2");
    if path.is_file() {
        std::fs::copy(&path, &backup).map_err(|e| format!("cannot back up {}: {e}", path.display()))?;
    }
    // The config can hold keys for MCP servers: written for its owner only.
    let tmp = path.with_file_name(format!(".herdr2-{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut file = crate::platform::private_file(&tmp).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    drop(file);
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", path.display())
    })?;
    Ok(path)
}

/// What a ZCode session is told with its next prompt after Herdr2 rolled the
/// workspace back. ZCode keeps the conversation in its own database, where
/// Herdr2 cannot cut it at a turn, so the session is resumed as it was and told
/// which later turns no longer exist on disk.
pub fn rollback_note(to_turn: u64) -> String {
    format!(
        "Herdr2 rolled this workspace back to the end of turn {to_turn}: files are as they were then, and \
         anything turns after {to_turn} wrote is gone. Those turns are still in this conversation only; \
         don't assume their results exist, and read a file before relying on what you remember of it."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phases(config: &Value, event: &str) -> Vec<String> {
        config["hooks"]["events"][event]
            .as_array()
            .expect("a list of groups")
            .iter()
            .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
            .filter(|h| h["type"] == "process" && h["command"] == "/h2")
            .map(|h| h["args"].as_array().expect("args")[5].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn merge_adds_herdr2_hooks_once_and_leaves_the_rest_alone() {
        let mut config = json!({ "hooks": { "enabled": true, "events": { "Stop": [
            { "matcher": "", "hooks": [{ "type": "command", "command": "mine" }] }
        ] } }, "ui": { "locale": "zh-CN" } });
        assert!(merge(&mut config, "/h2").unwrap());
        assert_eq!(config["ui"]["locale"], "zh-CN");
        for (event, phase, _) in HOOKS {
            assert_eq!(phases(&config, event), [phase.to_string()], "{event}");
        }
        let mine = config["hooks"]["events"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(mine, "mine");

        // Again: nothing to write.
        assert!(!merge(&mut config, "/h2").unwrap());
    }

    #[test]
    fn merge_enables_hooks_and_replaces_an_older_copy_of_ours() {
        let mut config = json!({ "hooks": { "enabled": false, "events": { "UserPromptSubmit": [
            { "hooks": [
                { "type": "process", "command": "/old/h2", "args": ["checkpoint", "hook", "--source", "zcode", "--phase", "pre"] },
                { "type": "command", "command": "/old/h2 checkpoint hook --source zcode --phase pre", "timeout": 10 },
                { "type": "command", "command": "keep me", "timeout": 5 }
            ] }
        ] } } });
        assert!(merge(&mut config, "/h2").unwrap());
        assert_eq!(config["hooks"]["enabled"], true);
        // The person's group is theirs alone; Herdr2's hook is a group of its own beside it.
        let groups = config["hooks"]["events"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "{groups:?}");
        assert_eq!(groups[0]["hooks"].as_array().unwrap().len(), 1, "the old copies go, the person's own hook stays");
        assert_eq!(groups[0]["hooks"][0]["command"], "keep me");
        let ours = &groups[1]["hooks"][0];
        assert_eq!(ours["command"], "/h2");
        assert_eq!(ours["timeoutMs"], 10_000);
        assert_eq!(ours["args"][3], "zcode");
    }

    #[test]
    fn merge_refuses_a_config_it_does_not_understand() {
        let mut shaped_wrong = json!({ "hooks": [] });
        assert!(merge(&mut shaped_wrong, "/h2").is_err());
        let mut events_wrong = json!({ "hooks": { "events": { "Stop": {} } } });
        assert!(merge(&mut events_wrong, "/h2").is_err());
        let mut not_object = json!([]);
        assert!(merge(&mut not_object, "/h2").is_err());
    }

    #[test]
    fn base_dir_follows_zcode_data_base_dir() {
        let home = Path::new("/home/someone");
        assert_eq!(base_dir(home, None), home);
        assert_eq!(base_dir(home, Some("  ")), home);
        assert_eq!(base_dir(home, Some(" /data/z ")), Path::new("/data/z"));
    }

    #[test]
    fn ensure_registered_writes_once_and_never_touches_a_broken_config() {
        let home = crate::test_tmp().join(format!("h2-zcode-hooks-{}", crate::platform::random_hex()));
        let path = config_path(&home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // A missing config: written with the hooks, private.
        assert_eq!(ensure_hooks_registered(&home, "/h2").unwrap(), path);
        let written = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(phases(&parsed, "PreToolUse"), ["guard"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // Already current: the file is not rewritten (its time would move).
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(ensure_hooks_registered(&home, "/h2").unwrap(), path);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before);
        assert!(!path.with_file_name("config.json.bak-herdr2").exists());

        // Another `h2` path: replaced, and what was there is kept beside it.
        ensure_hooks_registered(&home, "/new/h2").unwrap();
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["hooks"]["events"]["Stop"][0]["hooks"][0]["command"], "/new/h2");
        let backup: Value =
            serde_json::from_str(&std::fs::read_to_string(path.with_file_name("config.json.bak-herdr2")).unwrap())
                .unwrap();
        assert_eq!(backup["hooks"]["events"]["Stop"][0]["hooks"][0]["command"], "/h2");

        // Not JSON: refused, and the file is as it was.
        std::fs::write(&path, "{ not json").unwrap();
        let err = ensure_hooks_registered(&home, "/h2").unwrap_err();
        assert!(err.contains("left it alone"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");

        // Empty: a fresh config.
        std::fs::write(&path, "").unwrap();
        assert!(ensure_hooks_registered(&home, "/h2").is_ok());
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["hooks"]["enabled"], true);

        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn rollback_note_names_the_turn() {
        let note = rollback_note(3);
        assert!(note.contains("turn 3") && note.contains("still in this conversation"));
    }
}
