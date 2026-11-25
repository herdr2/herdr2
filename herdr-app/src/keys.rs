//! ~/.config/herdr-app/keys.json: string or object actions, reloaded live.
//!
//! ```json
//! {
//!   "global":   { "ctrl-shift-t": "tab.new", "ctrl-1": "tab.select:1" },
//!   "terminal": { "ctrl-left": "send:\u001bb", "ctrl-shift-t": "terminal" },
//!   "layout:terminals": { "ctrl-]": "space.next" },
//!   "mod:editor": { "ctrl-s": "editor.save" }
//! }
//! ```
//!
//! An action is a command (`pane.split:right`: the value fills its first
//! argument), an object (`{"command":"layout.resize","args":{"id":"sidebar","px":240}}`),
//! `send:<text>` (types into the focused terminal),
//! `<mod>.<command>` (delivered to that mod's `nextCommand()`), or `terminal`
//! (the program gets the key: the binding is switched off where this section
//! applies).

use gpui::{App, KeyBinding, KeyBindingContextPredicate, NoAction};
use std::rc::Rc;
use gpui_shell::action::ShellAction;
use std::collections::BTreeMap;

/// The reserved shortcut that restores the default layout and keys.
pub const ESCAPE_HATCH: &str = "ctrl-alt-shift-escape";

/// Command name for AI prompt polish.
pub const POLISH_ACTION: &str = "prompt.polish";

/// Ordered fallback chords for `prompt.polish` when the configured shortcut is
/// occupied by another application as a system-wide global hotkey.
#[cfg(not(target_os = "macos"))]
pub const POLISH_FALLBACK_CHORDS: &[&str] = &[
    "ctrl-alt-r",
    "ctrl-alt-e",
    "ctrl-alt-l",
    "ctrl-alt-i",
    "alt-shift-p",
    "ctrl-shift-y",
    "ctrl-alt-k",
    "ctrl-alt-o",
];
#[cfg(target_os = "macos")]
pub const POLISH_FALLBACK_CHORDS: &[&str] = &[
    "cmd-alt-r",
    "cmd-alt-e",
    "cmd-alt-l",
    "cmd-alt-i",
    "alt-shift-p",
    "cmd-shift-y",
];

/// Outcome of checking whether the configured `prompt.polish` shortcut is
/// hijacked by another application's system-wide global hotkey.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolishShortcutResolution {
    /// The occupied shortcut was replaced in `keys.json` with a free fallback.
    Reassigned { from: String, to: String },
    /// The shortcut is occupied, but no free fallback chord was available.
    OccupiedNoFallback { from: String },
}

/// The shipped keys: prefix-free; plain ctrl+letter always stays with the
/// program. Herdr uses ctrl+shift (defaults/keys.json), or cmd on macOS
/// (defaults/keys.macos.json, with Mac terminal editing keys).
#[cfg(not(target_os = "macos"))]
pub const DEFAULTS: &str = include_str!("../defaults/keys.json");
#[cfg(target_os = "macos")]
pub const DEFAULTS: &str = include_str!("../defaults/keys.macos.json");

// Exact originals from release tags, deduplicated across v0.1.4–v0.1.15.
// Keep these when defaults change: only untouched shipped files may migrate.
#[cfg(any(not(target_os = "macos"), test))]
const SHIPPED_KEYS: &[&str] = &[
    include_str!("keys/shipped/v0.1.4-keys.json"),
    include_str!("keys/shipped/v0.1.5-keys.json"),
    include_str!("keys/shipped/v0.1.12-keys.json"),
    include_str!("keys/shipped/v0.2.0-keys.json"),
    include_str!("keys/shipped/v0.2.1-keys.json"),
];
#[cfg(any(target_os = "macos", test))]
const SHIPPED_MACOS_KEYS: &[&str] = &[
    include_str!("keys/shipped/v0.1.4-keys.macos.json"),
    include_str!("keys/shipped/v0.1.5-keys.macos.json"),
    include_str!("keys/shipped/v0.1.15-keys.macos.json"),
    include_str!("keys/shipped/v0.2.0-keys.macos.json"),
    include_str!("keys/shipped/v0.2.1-keys.macos.json"),
];

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Binding {
    String(String),
    Object(serde_json::Map<String, serde_json::Value>),
}

type Sections = BTreeMap<String, BTreeMap<String, Binding>>;

/// Kept separate from string actions so colon payloads always stay literal.
#[derive(Clone, PartialEq, gpui::Action, serde::Deserialize)]
#[action(no_json)]
pub struct CommandBinding {
    pub value: serde_json::Map<String, serde_json::Value>,
}

impl CommandBinding {
    pub fn command(&self) -> Result<(String, serde_json::Value), String> {
        use serde_json::{Value, json};
        if let Some(key) = self.value.keys().find(|k| !matches!(k.as_str(), "command" | "args")) {
            return Err(format!("unknown key binding field {key:?}; use command and args"));
        }
        let name = self.value.get("command").and_then(Value::as_str)
            .ok_or("key binding command must be a string")?;
        let name = current(name);
        let command = crate::commands::find(&name).ok_or_else(|| format!("unknown key binding command {name:?}"))?;
        let args = self.value.get("args").cloned().unwrap_or_else(|| json!({}));
        if !args.is_object() {
            return Err(format!("{name}: key binding args must be an object"));
        }
        crate::commands::validate_args(command, &args)?;
        Ok((name, args))
    }
}

fn defaults() -> Sections {
    serde_json::from_str(DEFAULTS).expect("defaults/keys.json is valid")
}

/// Every binding as `(section, keys, action)`, for the keybind help.
pub fn listing() -> Vec<(String, String, String)> {
    let text = std::fs::read_to_string(path()).unwrap_or_default();
    let sections: Sections = serde_json::from_str(&text).unwrap_or_else(|_| defaults());
    let mut out = Vec::new();
    for (s, keys) in sections {
        for (k, a) in keys {
            match a {
                Binding::String(a) => {
                    let a = current(&a);
                    if !gone_mod(&a) {
                        out.push((s.clone(), k, a));
                    }
                }
                Binding::Object(a) => out.push((s.clone(), k, serde_json::Value::Object(a).to_string())),
            }
        }
    }
    out.push(("global".into(), ESCAPE_HATCH.into(), "app.reset".into()));
    out
}

pub fn path() -> std::path::PathBuf {
    crate::mods::app_dir().join("keys.json")
}

/// The GPUI key tab a section applies in. `global` is the window itself,
/// not "no tab": GPUI ranks a binding without one above every other, so
/// it would beat `layout:` (same depth, added earlier) and only lose to
/// `terminal` and `mod:` by order.
fn tab(section: &str) -> Result<String, String> {
    match section {
        "global" => Ok("Herdr".into()),
        "terminal" => Ok("Terminal".into()),
        s => match (s.strip_prefix("mod:"), s.strip_prefix("layout:")) {
            (Some(p), _) if !p.is_empty() => Ok(format!("mod == {p}")),
            // While layouts/<name>.json is shown (the window's tab says which).
            (_, Some(l)) if !l.is_empty() => Ok(format!("layout == {l}")),
            _ => Err(format!("unknown section {s:?}: use \"global\", \"terminal\", \"layout:<name>\" or \"mod:<name>\"")),
        },
    }
}

/// 0.1.0's words in a keys.json from then (installed once, so still theirs):
/// projects became spaces, contexts tabs, the start popup the new tab.
pub(crate) fn current(action: &str) -> String {
    if action == "mod.open:start" {
        return "tab.new".into();
    }
    let command = |a: &str| crate::commands::find(a.split_once(':').map_or(a, |(n, _)| n)).is_some();
    for (old, new) in [("project.", "space."), ("context.", "tab.")] {
        if let Some(rest) = action.strip_prefix(old) {
            let renamed = format!("{new}{rest}");
            // Only a command that was renamed: a mod named `project` keeps its own.
            if command(&renamed) && !command(action) {
                return renamed;
            }
        }
    }
    action.to_owned()
}

/// A mod's command whose mod is gone (0.1.x's `ribbon.next`): bound to nothing.
fn gone_mod(action: &str) -> bool {
    let name = action.split_once(':').map_or(action, |(n, _)| n);
    crate::commands::find(name).is_none()
        && name.split_once('.').is_some_and(|(m, _)| !m.starts_with("frame-") && !crate::mods::mods_dir().join(m).is_dir())
}

fn valid_action(action: &str) -> bool {
    let name = action.split_once(':').map_or(action, |(n, _)| n);
    crate::commands::find(name).is_some()
        || action == "terminal"
        || action.starts_with("send:")
        || name.split_once('.').is_some_and(|(m, c)| !m.is_empty() && !c.is_empty())
}

/// Reads keys.json, installing or updating untouched defaults, or says what's wrong.
pub fn load() -> Result<Vec<KeyBinding>, String> {
    #[cfg(not(target_os = "macos"))]
    let originals = SHIPPED_KEYS;
    #[cfg(target_os = "macos")]
    let originals = SHIPPED_MACOS_KEYS;
    parse(&load_text(&path(), DEFAULTS, originals)?)
}

fn load_text(path: &std::path::Path, defaults: &str, originals: &[&str]) -> Result<String, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(path, defaults).map_err(|e| e.to_string())?;
            return Ok(defaults.to_owned());
        }
        Err(e) => return Err(e.to_string()),
    };
    if text != defaults && originals.contains(&text.as_str()) {
        std::fs::write(path, defaults).map_err(|e| e.to_string())?;
        return Ok(defaults.to_owned());
    }
    Ok(text)
}

pub fn parse(text: &str) -> Result<Vec<KeyBinding>, String> {
    let sections: Sections = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for (section, keys) in &sections {
        let ctx = match tab(section) {
            Ok(c) => c,
            Err(e) => {
                problems.push(e);
                continue;
            }
        };
        for (keys, action) in keys {
            let string = match action {
                Binding::String(action) => Some(current(action)),
                Binding::Object(_) => None,
            };
            if let Some(action) = &string {
                if gone_mod(action) {
                    continue;
                }
                if !valid_action(action) {
                    problems.push(format!("{section}.{keys}: unknown action {action:?}"));
                    continue;
                }
            }
            if keys == ESCAPE_HATCH {
                problems.push(format!("{ESCAPE_HATCH} is reserved for resetting Herdr"));
                continue;
            }
            let predicate = match KeyBindingContextPredicate::parse(&ctx) {
                Ok(p) => Some(Rc::new(p)),
                Err(e) => {
                    problems.push(format!("{section}: {e}"));
                    continue;
                }
            };
            let action: Box<dyn gpui::Action> = if let Binding::Object(value) = action {
                Box::new(CommandBinding { value: value.clone() })
            } else if string.as_deref() == Some("terminal") {
                // Off where this section applies; globally, simply not bound.
                if section == "global" {
                    continue;
                }
                Box::new(NoAction)
            } else {
                let action = string.as_ref().expect("string binding");
                #[cfg(target_os = "macos")]
                let native = crate::macos::action(action);
                #[cfg(not(target_os = "macos"))]
                let native: Option<Box<dyn gpui::Action>> = None;
                native.unwrap_or_else(|| Box::new(ShellAction::new(action.clone())))
            };
            let binding = KeyBinding::load(keys, action, predicate, false, None, &gpui::DummyKeyboardMapper);
            match binding {
                Ok(b) => out.push(b),
                Err(e) => problems.push(format!("{section}.{keys}: {e}")),
            }
        }
    }
    if problems.is_empty() { Ok(out) } else { Err(problems.join("; ")) }
}

/// The UI kit's own keys (text editing in inputs, lists): bound at its init,
/// kept across every reload of keys.json.
struct KitKeys(Vec<KeyBinding>);
impl gpui::Global for KitKeys {}

/// Restores kit bindings, installs user bindings, then adds reserved bindings.
pub fn install(bindings: Vec<KeyBinding>, cx: &mut App) {
    if !cx.has_global::<KitKeys>() {
        let kit = cx.key_bindings().borrow().bindings().cloned().collect();
        cx.set_global(KitKeys(kit));
    }
    let kit = cx.global::<KitKeys>().0.clone();
    cx.clear_key_bindings();
    cx.bind_keys(kit);
    cx.bind_keys(bindings);
    cx.bind_keys([
        KeyBinding::new(ESCAPE_HATCH, ShellAction::new("app.reset"), None),
        KeyBinding::new("escape", ShellAction::new("overlay.close"), Some("Overlay")),
    ]);
    // The menu bar shows the keys it finds when it's built.
    #[cfg(target_os = "macos")]
    crate::macos::refresh_menus(cx);
}

/// Parses a single-stroke GPUI chord (e.g. `"ctrl-alt-p"`) into Win32
/// `RegisterHotKey` `(fsModifiers, vk)` values. Returns `None` for bare keys
/// (no modifier), multi-stroke sequences, or unsupported keys.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn win32_hotkey_parts(chord: &str) -> Option<(u32, u32)> {
    const MOD_ALT: u32 = 0x0001;
    const MOD_CONTROL: u32 = 0x0002;
    const MOD_SHIFT: u32 = 0x0004;
    const MOD_WIN: u32 = 0x0008;

    let trimmed = chord.trim();
    if trimmed.is_empty() || trimmed.split_whitespace().count() != 1 {
        return None;
    }

    let mut parts: Vec<&str> = trimmed.split('-').collect();
    let mut key = parts.pop()?;
    if key.is_empty() {
        // Trailing `-`, e.g. `"ctrl--"`.
        key = "-";
        parts.pop()?;
    }

    let mut mods = 0u32;
    for part in parts {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" | "option" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "cmd" | "super" | "win" | "platform" => mods |= MOD_WIN,
            "secondary" => {
                if cfg!(target_os = "macos") {
                    mods |= MOD_WIN;
                } else {
                    mods |= MOD_CONTROL;
                }
            }
            _ => return None,
        }
    }
    if mods == 0 {
        return None;
    }

    let key_lower = key.to_ascii_lowercase();
    let vk: u32 = match key_lower.as_str() {
        s if s.len() == 1 => {
            let b = s.as_bytes()[0];
            match b {
                b'a'..=b'z' => (b - b'a' + 0x41) as u32,
                b'0'..=b'9' => b as u32,
                b';' => 0xBA,
                b'=' | b'+' => 0xBB,
                b',' => 0xBC,
                b'-' => 0xBD,
                b'.' => 0xBE,
                b'/' | b'?' => 0xBF,
                b'`' => 0xC0,
                b'[' => 0xDB,
                b'\\' => 0xDC,
                b']' => 0xDD,
                b'\'' => 0xDE,
                _ => return None,
            }
        }
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "escape" | "esc" => 0x1B,
        "tab" => 0x09,
        "backspace" => 0x08,
        "delete" => 0x2E,
        "insert" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        s if s.starts_with('f') => {
            let n: u32 = s[1..].parse().ok()?;
            if (1..=24).contains(&n) {
                0x6F + n
            } else {
                return None;
            }
        }
        _ => return None,
    };

    Some((mods, vk))
}

/// Checks whether another application has registered `chord` as a system-wide
/// global hotkey (preventing key events from reaching Herdr2).
#[cfg(windows)]
pub fn is_system_hotkey_occupied(chord: &str) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, GetLastError};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};

    const PROBE_HOTKEY_ID: i32 = 0x4850; // Within 0x0000..=0xBFFF application range.

    let Some((mods, vk)) = win32_hotkey_parts(chord) else {
        return false;
    };
    unsafe {
        if RegisterHotKey(std::ptr::null_mut(), PROBE_HOTKEY_ID, mods, vk) != 0 {
            let _ = UnregisterHotKey(std::ptr::null_mut(), PROBE_HOTKEY_ID);
            false
        } else {
            GetLastError() == ERROR_HOTKEY_ALREADY_REGISTERED
        }
    }
}

#[cfg(not(windows))]
pub fn is_system_hotkey_occupied(_chord: &str) -> bool {
    false
}

/// Formats a GPUI chord string (e.g. `"ctrl-alt-p"`) for display in user notices.
pub fn format_chord_display(chord: &str, mac: bool) -> String {
    let trimmed = chord.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed
        .split_whitespace()
        .map(|stroke| {
            let mut parts: Vec<&str> = stroke.split('-').collect();
            let mut key = parts.pop().unwrap_or_default();
            if key.is_empty() {
                key = "-";
                let _ = parts.pop();
            }
            let (mut ctrl, mut alt, mut shift, mut cmd, mut fn_key) = (false, false, false, false, false);
            for p in parts {
                match p.to_ascii_lowercase().as_str() {
                    "ctrl" | "control" => ctrl = true,
                    "alt" | "option" => alt = true,
                    "shift" => shift = true,
                    "cmd" | "super" | "win" | "platform" => cmd = true,
                    "fn" => fn_key = true,
                    "secondary" => {
                        if mac {
                            cmd = true;
                        } else {
                            ctrl = true;
                        }
                    }
                    _ => {}
                }
            }
            if key.len() == 1 && key.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                shift = true;
            }
            let key_label = match key.to_ascii_lowercase().as_str() {
                "enter" | "return" => "Enter".to_owned(),
                "escape" | "esc" => "Esc".to_owned(),
                "backspace" => "Backspace".to_owned(),
                "tab" => "Tab".to_owned(),
                "space" => "Space".to_owned(),
                "pageup" => "PgUp".to_owned(),
                "pagedown" => "PgDn".to_owned(),
                "up" => "↑".to_owned(),
                "down" => "↓".to_owned(),
                "left" => "←".to_owned(),
                "right" => "→".to_owned(),
                "insert" => "Ins".to_owned(),
                "delete" => "Del".to_owned(),
                s if s.len() == 1 => s.to_ascii_uppercase(),
                s if s.starts_with('f') && s[1..].chars().all(|c| c.is_ascii_digit()) => s.to_ascii_uppercase(),
                s => {
                    let mut chars = s.chars();
                    match chars.next() {
                        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                        None => String::new(),
                    }
                }
            };
            let mut tokens: Vec<String> = Vec::new();
            if ctrl {
                tokens.push(if mac { "⌃" } else { "Ctrl" }.into());
            }
            if alt {
                tokens.push(if mac { "⌥" } else { "Alt" }.into());
            }
            if shift {
                tokens.push(if mac { "⇧" } else { "Shift" }.into());
            }
            if cmd {
                tokens.push(if mac { "⌘" } else { "Super" }.into());
            }
            if fn_key {
                tokens.push(if mac { "fn" } else { "Fn" }.into());
            }
            tokens.push(key_label);
            tokens.join(if mac { "" } else { "+" })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_polish_binding(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => current(s) == POLISH_ACTION,
        serde_json::Value::Object(map) => map
            .get("command")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| current(s) == POLISH_ACTION),
        _ => false,
    }
}

/// Pure resolver: checks whether all `global` bindings for `prompt.polish` in `text`
/// are occupied by external system global hotkeys; if so, rewrites `text` to the
/// first unused & unoccupied fallback chord.
pub fn resolve_polish_conflict_in_text(
    text: &str,
    fallbacks: &[&str],
    is_occupied: impl Fn(&str) -> bool,
) -> Result<Option<(String, PolishShortcutResolution)>, String> {
    let mut doc: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let Some(root) = doc.as_object_mut() else {
        return Ok(None);
    };
    let Some(global) = root.get("global").and_then(serde_json::Value::as_object) else {
        return Ok(None);
    };

    let bound: Vec<(String, serde_json::Value)> = global
        .iter()
        .filter(|(_, val)| is_polish_binding(val))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if bound.is_empty() {
        return Ok(None);
    }

    // If at least one configured chord for `prompt.polish` is free, leave keys.json untouched.
    if bound.iter().any(|(chord, _)| !is_occupied(chord)) {
        return Ok(None);
    }

    let from = bound[0].0.clone();
    let original_value = bound[0].1.clone();

    // Collect every chord already used in `global` or `terminal` so we never steal another shortcut.
    let mut used = std::collections::BTreeSet::new();
    for section in ["global", "terminal"] {
        if let Some(map) = root.get(section).and_then(serde_json::Value::as_object) {
            for k in map.keys() {
                used.insert(k.to_ascii_lowercase());
            }
        }
    }

    let chosen = fallbacks.iter().copied().find(|cand| {
        let lower = cand.to_ascii_lowercase();
        lower != ESCAPE_HATCH
            && !used.contains(&lower)
            && gpui::Keystroke::parse(cand).is_ok()
            && !is_occupied(cand)
    });

    let Some(to) = chosen else {
        return Ok(Some((
            text.to_owned(),
            PolishShortcutResolution::OccupiedNoFallback { from },
        )));
    };

    if let Some(global_mut) = root.get_mut("global").and_then(serde_json::Value::as_object_mut) {
        for (k, _) in &bound {
            global_mut.remove(k);
        }
        global_mut.insert(to.to_owned(), original_value);
    }

    let mut serialized = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    serialized.push('\n');
    Ok(Some((
        serialized,
        PolishShortcutResolution::Reassigned {
            from,
            to: to.to_owned(),
        },
    )))
}

/// Checks `path` (`keys.json`) for an occupied `prompt.polish` global hotkey and,
/// if a free fallback is available, atomically updates `path` on disk.
pub fn resolve_polish_conflict(path: &std::path::Path) -> Result<Option<PolishShortcutResolution>, String> {
    resolve_polish_conflict_with(path, POLISH_FALLBACK_CHORDS, is_system_hotkey_occupied)
}

pub fn resolve_polish_conflict_with(
    path: &std::path::Path,
    fallbacks: &[&str],
    is_occupied: impl Fn(&str) -> bool,
) -> Result<Option<PolishShortcutResolution>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let Some((updated, resolution)) = resolve_polish_conflict_in_text(&text, fallbacks, is_occupied)? else {
        return Ok(None);
    };
    if matches!(resolution, PolishShortcutResolution::Reassigned { .. }) {
        crate::persist::write_atomic(path, &updated).map_err(|e| e.to_string())?;
    }
    Ok(Some(resolution))
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_byte_identical_shipped_keys_are_upgraded() {
        let dir = std::env::temp_dir().join(format!("herdr-keys-migration-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keys.json");
        for (defaults, originals) in [
            (include_str!("../defaults/keys.json"), super::SHIPPED_KEYS),
            (include_str!("../defaults/keys.macos.json"), super::SHIPPED_MACOS_KEYS),
        ] {
            for original in originals {
                std::fs::write(&path, original).unwrap();
                assert_eq!(super::load_text(&path, defaults, originals).unwrap(), defaults);
                assert_eq!(std::fs::read_to_string(&path).unwrap(), defaults);

                // Even a whitespace-only edit is personal, not an untouched original.
                for edited in [format!("{original}\n"), original.replace("tab.new", "pane.zoom")] {
                    std::fs::write(&path, &edited).unwrap();
                    assert_eq!(super::load_text(&path, defaults, originals).unwrap(), edited);
                    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
                }
            }
            std::fs::write(&path, defaults).unwrap();
            assert_eq!(super::load_text(&path, defaults, originals).unwrap(), defaults);
            std::fs::remove_file(&path).unwrap();
            assert_eq!(super::load_text(&path, defaults, originals).unwrap(), defaults);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), defaults);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keys_from_0_1_0_mean_the_same_now() {
        assert_eq!(super::current("project.select:2"), "space.select:2");
        assert_eq!(super::current("context.new"), "tab.new");
        assert_eq!(super::current("mod.open:start"), "tab.new");
        assert_eq!(super::current("pane.zoom"), "pane.zoom");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_shifted_punctuation_matches_existing_keys_files() {
        use gpui::{KeybindingKeystroke, Keystroke};
        let bindings = super::parse(include_str!("../defaults/keys.macos.json")).unwrap();
        for (base, shifted) in [("]", "}"), ("[", "{")] {
            let event = Keystroke {
                unshifted_key: Some(base.into()),
                ..Keystroke::parse(&format!("cmd-{shifted}")).unwrap()
            };
            assert!(bindings.iter().any(|binding| binding.match_keystrokes(&[event.clone()]) == Some(false)));
        }
        // Layout-derived aliases, not a US punctuation lookup table.
        for (base, shifted) in [("=", "+"), ("-", "_"), ("ö", "Ö"), ("č", "Č"), ("7", "/")] {
            let event = Keystroke {
                unshifted_key: Some(base.into()),
                ..Keystroke::parse(&format!("cmd-{shifted}")).unwrap()
            };
            for keys in [format!("cmd-shift-{base}"), format!("cmd-{shifted}")] {
                let target = KeybindingKeystroke::from_keystroke(Keystroke::parse(&keys).unwrap());
                assert!(event.should_match(&target), "{keys}");
            }
        }
        assert!(Keystroke::parse("cmd-shift-]").unwrap().unshifted_key.is_none());
        assert!(Keystroke::default().unshifted_key.is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_help_binding_matches_shifted_slash() {
        // Linux reports Ctrl+Shift+/ as Ctrl+? without the Shift modifier.
        let event = gpui::Keystroke::parse("ctrl-?").unwrap();
        let defaults: serde_json::Value = serde_json::from_str(super::DEFAULTS).unwrap();
        let help = defaults["global"].as_object().unwrap().iter()
            .filter(|(_, action)| action.as_str() == Some("app.help"))
            .map(|(keys, action)| (keys.clone(), action.clone()))
            .collect::<serde_json::Map<_, _>>();
        let bindings = super::parse(&serde_json::json!({ "global": help }).to_string()).unwrap();
        assert!(bindings.iter().any(|binding| binding.match_keystrokes(&[event.clone()]) == Some(false)));
    }

    #[test]
    fn object_bindings_pass_named_typed_arguments() {
        use serde_json::json;
        for (name, args) in [
            ("layout.resize", json!({"id":"sidebar", "px":240})),
            ("tab.new", json!({"mod":"editor", "props":{"enabled":true, "items":[1,null,"a:b"]}})),
            ("app.help", json!({})),
        ] {
            let bindings = super::parse(&json!({"global":{"f6":{"command":name,"args":args}}}).to_string()).unwrap();
            assert_eq!(bindings.len(), 1);
            let action = bindings[0].action().as_any().downcast_ref::<super::CommandBinding>().unwrap();
            assert_eq!(action.command().unwrap(), (name.into(), args));
        }
        let bindings = super::parse(r#"{"global":{"f6":{"command":"app.help"}}}"#).unwrap();
        let action = bindings[0].action().as_any().downcast_ref::<super::CommandBinding>().unwrap();
        assert_eq!(action.command().unwrap(), ("app.help".into(), json!({})));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn object_bindings_resolve_native_app_actions_after_validation() {
        use serde_json::json;
        for name in ["app.quit", "app.hide", "app.hide_others", "app.show_all", "app.settings"] {
            let bindings = super::parse(&json!({"global":{"f6":{"command":name}}}).to_string()).unwrap();
            let action = bindings[0].action().as_any().downcast_ref::<super::CommandBinding>().unwrap();
            let (command, args) = action.command().unwrap();
            assert_eq!(args, json!({}));
            let native = crate::macos::action(&command).unwrap();
            let expected = crate::macos::action(name).unwrap();
            assert!(native.partial_eq(expected.as_ref()));
            let bad = super::CommandBinding { value: json!({"command":name,"args":{"typo":true}}).as_object().unwrap().clone() };
            assert!(bad.command().is_err());
        }
    }

    #[test]
    fn bad_object_bindings_stay_bound_for_error_toasts() {
        use serde_json::json;
        for value in [
            json!({}), json!({"command":3}), json!({"command":"unknown"}),
            json!({"command":"space.new:{work}"}),
            json!({"command":"app.help","extra":true}),
            json!({"command":"app.help","args":null}),
            json!({"command":"app.help","args":[]}),
            json!({"command":"app.help","args":"{}"}),
            json!({"command":"layout.resize","args":{"typo":240}}),
            json!({"command":"layout.resize","args":{"id":123,"px":240}}),
        ] {
            let bindings = super::parse(&json!({"terminal":{"f6":value}}).to_string()).unwrap();
            assert_eq!(bindings.len(), 1, "{value}");
            assert_eq!(bindings[0].match_keystrokes(&[gpui::Keystroke::parse("f6").unwrap()]), Some(false));
            let action = bindings[0].action().as_any().downcast_ref::<super::CommandBinding>().unwrap();
            assert!(action.command().is_err(), "{value}");
        }
    }

    #[test]
    fn colon_payloads_stay_literal_including_valid_and_invalid_json() {
        use serde_json::json;
        for value in ["{work}", "{}", "{", "  {bad}", r#"{"name":"work"}"#,
            r#"{"name":"work",}"#, r#"{"name":"work"} trailing"#,
            "hello:world", "", "3", "-1", "true", "null", "[]", "[broken", r#""name""#] {
            let action = format!("space.new:{value}");
            let bindings = super::parse(&json!({"global":{"f6":action}}).to_string()).unwrap();
            let bound = bindings[0].action().as_any().downcast_ref::<gpui_shell::action::ShellAction>().unwrap();
            assert_eq!(bound.id(), action);
            let (name, payload) = bound.id().split_once(':').unwrap();
            assert_eq!(crate::commands::shorthand(crate::commands::find(name).unwrap(), payload), json!({"name":value}));
        }
        assert_eq!(crate::commands::shorthand(crate::commands::find("tab.select").unwrap(), "3"), json!({"index":3}));
    }

    #[test]
    fn shipped_keys_load() {
        for keys in [include_str!("../defaults/keys.json"), include_str!("../defaults/keys.macos.json")] {
            assert_eq!(super::parse(keys).err(), None);
        }
    }

    #[test]
    fn win32_hotkey_parts_parses_modified_chords_and_rejects_bare_keys() {
        assert_eq!(super::win32_hotkey_parts("ctrl-alt-p"), Some((0x0002 | 0x0001, 0x50)));
        assert_eq!(super::win32_hotkey_parts("ctrl-alt-r"), Some((0x0002 | 0x0001, 0x52)));
        assert_eq!(super::win32_hotkey_parts("alt-shift-p"), Some((0x0001 | 0x0004, 0x50)));
        assert_eq!(super::win32_hotkey_parts("ctrl-shift-y"), Some((0x0002 | 0x0004, 0x59)));
        assert_eq!(super::win32_hotkey_parts("ctrl--"), Some((0x0002, 0xBD)));
        assert_eq!(super::win32_hotkey_parts("shift-f2"), Some((0x0004, 0x71)));
        assert_eq!(super::win32_hotkey_parts("p"), None);
        assert_eq!(super::win32_hotkey_parts("f2"), None);
        assert_eq!(super::win32_hotkey_parts("ctrl-k ctrl-p"), None);
    }

    #[test]
    fn format_chord_display_renders_readable_labels() {
        assert_eq!(super::format_chord_display("ctrl-alt-p", false), "Ctrl+Alt+P");
        assert_eq!(super::format_chord_display("ctrl-alt-r", false), "Ctrl+Alt+R");
        assert_eq!(super::format_chord_display("alt-shift-p", false), "Alt+Shift+P");
        assert_eq!(super::format_chord_display("cmd-alt-p", true), "⌥⌘P");
        assert_eq!(super::format_chord_display("ctrl-shift-enter", false), "Ctrl+Shift+Enter");
    }

    #[test]
    fn polish_conflict_reassigns_to_first_free_and_unbound_fallback() {
        let defaults = include_str!("../defaults/keys.json");
        let fallbacks = ["ctrl-alt-r", "ctrl-alt-e", "ctrl-alt-l"];

        // 1) Free chord -> no change.
        assert_eq!(
            super::resolve_polish_conflict_in_text(defaults, &fallbacks, |_| false).unwrap(),
            None
        );

        // 2) ctrl-alt-p occupied -> switches to ctrl-alt-r and keeps keys.json parseable.
        let (updated, res) = super::resolve_polish_conflict_in_text(defaults, &fallbacks, |k| k == "ctrl-alt-p")
            .unwrap()
            .expect("must reassign");
        assert_eq!(
            res,
            super::PolishShortcutResolution::Reassigned {
                from: "ctrl-alt-p".into(),
                to: "ctrl-alt-r".into(),
            }
        );
        assert_eq!(super::parse(&updated).err(), None);
        let parsed: serde_json::Value = serde_json::from_str(&updated).unwrap();
        assert!(parsed["global"].get("ctrl-alt-p").is_none());
        assert_eq!(parsed["global"]["ctrl-alt-r"], serde_json::json!("prompt.polish"));
        assert_eq!(parsed["global"]["ctrl-alt-u"], serde_json::json!("prompt.polish_undo"));

        // 3) ctrl-alt-p occupied AND ctrl-alt-r also occupied -> skips to ctrl-alt-e.
        let (updated2, res2) = super::resolve_polish_conflict_in_text(
            defaults,
            &fallbacks,
            |k| matches!(k, "ctrl-alt-p" | "ctrl-alt-r"),
        )
        .unwrap()
        .expect("must skip occupied fallback");
        assert_eq!(
            res2,
            super::PolishShortcutResolution::Reassigned {
                from: "ctrl-alt-p".into(),
                to: "ctrl-alt-e".into(),
            }
        );
        let parsed2: serde_json::Value = serde_json::from_str(&updated2).unwrap();
        assert_eq!(parsed2["global"]["ctrl-alt-e"], serde_json::json!("prompt.polish"));

        // 4) Fallback already bound in keys.json -> skipped without stealing existing binding.
        let mut with_r = serde_json::from_str::<serde_json::Value>(defaults).unwrap();
        with_r["global"]["ctrl-alt-r"] = serde_json::json!("app.help");
        let with_r_text = serde_json::to_string_pretty(&with_r).unwrap();
        let (updated3, res3) = super::resolve_polish_conflict_in_text(&with_r_text, &fallbacks, |k| k == "ctrl-alt-p")
            .unwrap()
            .expect("must avoid already-bound chord");
        assert_eq!(
            res3,
            super::PolishShortcutResolution::Reassigned {
                from: "ctrl-alt-p".into(),
                to: "ctrl-alt-e".into(),
            }
        );
        let parsed3: serde_json::Value = serde_json::from_str(&updated3).unwrap();
        assert_eq!(parsed3["global"]["ctrl-alt-r"], serde_json::json!("app.help"));
        assert_eq!(parsed3["global"]["ctrl-alt-e"], serde_json::json!("prompt.polish"));

        // 5) Every fallback occupied -> reports OccupiedNoFallback and leaves text intact.
        let (unchanged, res4) = super::resolve_polish_conflict_in_text(defaults, &fallbacks, |_| true)
            .unwrap()
            .expect("must report occupied without fallback");
        assert_eq!(
            res4,
            super::PolishShortcutResolution::OccupiedNoFallback {
                from: "ctrl-alt-p".into(),
            }
        );
        assert_eq!(unchanged, defaults);

        // 6) Multiple bindings where one is already free -> leaves keys.json untouched.
        let mut multi = serde_json::from_str::<serde_json::Value>(defaults).unwrap();
        multi["global"]["alt-shift-p"] = serde_json::json!("prompt.polish");
        let multi_text = serde_json::to_string_pretty(&multi).unwrap();
        assert_eq!(
            super::resolve_polish_conflict_in_text(&multi_text, &fallbacks, |k| k == "ctrl-alt-p").unwrap(),
            None
        );
    }

    #[test]
    fn polish_conflict_disk_round_trip_writes_valid_keys_json() {
        let dir = std::env::temp_dir().join(format!("herdr-polish-conflict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keys.json");
        std::fs::write(&path, include_str!("../defaults/keys.json")).unwrap();

        let res = super::resolve_polish_conflict_with(
            &path,
            &["ctrl-alt-r", "ctrl-alt-e"],
            |k| k == "ctrl-alt-p",
        )
        .unwrap();
        assert_eq!(
            res,
            Some(super::PolishShortcutResolution::Reassigned {
                from: "ctrl-alt-p".into(),
                to: "ctrl-alt-r".into(),
            })
        );
        let saved = std::fs::read_to_string(&path).unwrap();
        assert_eq!(super::parse(&saved).err(), None);
        let parsed: serde_json::Value = serde_json::from_str(&saved).unwrap();
        assert!(parsed["global"].get("ctrl-alt-p").is_none());
        assert_eq!(parsed["global"]["ctrl-alt-r"], serde_json::json!("prompt.polish"));

        // Second check sees ctrl-alt-r is free -> no-op.
        let second = super::resolve_polish_conflict_with(
            &path,
            &["ctrl-alt-r", "ctrl-alt-e"],
            |k| k == "ctrl-alt-p",
        )
        .unwrap();
        assert_eq!(second, None);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
