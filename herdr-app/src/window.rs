//! The window. The chrome (`layouts/<name>.json`) is a tree of the locked
//! primitives; a `{ "tab": "active" }` block shows the selected
//! tab's terminals as it arranges them. Every visible part that isn't a
//! terminal is a mod. Commands (commands.rs) are the one way to change
//! anything: keys, mods, menus and agents all run them.

use crate::hc::Core;
mod floats;
mod movable;
mod pane_drag;
mod pane_size;
mod titlebar;

use crate::layout::{Axis, Background as Paint, Content, Fit, HeaderContent, Kind, Line, Node, PaneFrame, Size as Span};
use crate::model::{self, Tab, Model};
use crate::mods::{self, Found, Instance, Request};
use crate::settings;
use crate::layout_sync;
use crate::terminal::{TerminalEvent, TerminalView, scroll_target};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_shell::action::ShellAction;
use gpui_shell::{ShellRoot, ShellRuntime, Watcher};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

const HEADER_HEIGHT: f32 = 30.0;
const HANDLE: f32 = 6.0;

// The three native buttons plus their trailing breathing room (also used by the tabs mod).
const TRAFFIC_LIGHT_WIDTH: f32 = 64.0;

fn traffic_light_divider_inset(n: &Node, axis: Axis, index: usize, macos: bool, zoom: f32) -> f32 {
    if !macos || axis != Axis::Row || n.titlebar != Some(crate::layout::Titlebar::Layout) {
        return 0.0;
    }
    let Some([x, _]) = n.traffic_lights else { return 0.0 };
    let shown: Vec<_> = n.children().iter().filter(|child| !child.hidden).collect();
    let mut edge = n.padding.map_or(0.0, |padding| padding.axes().1)
        + index as f32 * (n.gap.unwrap_or(0.0) + n.divider.as_ref().map_or(0.0, |line| line.width));
    for child in shown.iter().take(index + 1) {
        let Some(Span::Px { px }) = child.size else { return 0.0 };
        edge += px;
    }
    if edge * zoom < x || edge * zoom > x + TRAFFIC_LIGHT_WIDTH {
        return 0.0;
    }
    // Keep the toolbar continuous above the rail; the divider starts with the content.
    shown.iter().skip(index).take(2).filter_map(|column| {
        let header = column.children().iter().find(|child| !child.hidden)?;
        match header.size {
            Some(Span::Px { px }) if header.window_drag => Some(px),
            _ => None,
        }
    }).reduce(f32::min).unwrap_or(0.0)
}

fn separator_line(row: bool, width: f32, color: Hsla, top: f32) -> Div {
    div().bg(color)
        .when(row, |d| d.w(px(width)))
        .when(row && top == 0.0, |d| d.h_full())
        .when(row && top > 0.0, |d| d.absolute().top(px(top)).bottom_0())
        .when(!row, |d| d.h(px(width)).w_full())
}

fn c(name: &str) -> Hsla {
    settings::color_or_literal(name)
}

fn file(name: &str) -> PathBuf {
    mods::app_dir().join(name)
}

fn is_dark(a: gpui::WindowAppearance) -> bool {
    matches!(a, gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark)
}

/// A layout's theme after picking `name`: it takes the half of its own
/// lightness; the other half stays (a single theme of the other lightness
/// becomes that half).
fn with_theme(was: Option<crate::layout::ThemeChoice>, name: String) -> crate::layout::ThemeChoice {
    use crate::layout::ThemeChoice::{One, Pair};
    let light = settings::theme_is_light(&name);
    let (l, d) = match was {
        Some(Pair { light, dark }) => (light, dark),
        Some(One(x)) if settings::theme_is_light(&x) == light => return One(name),
        Some(One(x)) => (x.clone(), x),
        None => (settings::DEFAULT_LIGHT.into(), settings::DEFAULT_DARK.into()),
    };
    if light { Pair { light: name, dark: d } } else { Pair { light: l, dark: name } }
}

fn mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

const WIN_PS_HOOK: &str = "$global:__h2_prompt = $function:prompt; function global:prompt { $p = & $global:__h2_prompt; $l = $ExecutionContext.SessionState.Path.CurrentLocation; if ($l.Provider.Name -eq 'FileSystem') { try { [Environment]::CurrentDirectory = $l.ProviderPath } catch {} }; $p }";

fn find_exe_in_path(path_os: Option<&std::ffi::OsStr>, exe: &str) -> Option<String> {
    let path = path_os?;
    std::env::split_paths(path)
        .map(|dir| dir.join(exe))
        .find(|cand| cand.is_file())
        .map(|cand| cand.to_string_lossy().into_owned())
}

fn find_git_bash_with(
    path_os: Option<&std::ffi::OsStr>,
    program_files: Option<&str>,
    program_files_x86: Option<&str>,
    local_app_data: Option<&str>,
) -> Option<String> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    for base in [program_files, program_files_x86].into_iter().flatten() {
        candidates.push(std::path::Path::new(base).join("Git").join("bin").join("bash.exe"));
        candidates.push(std::path::Path::new(base).join("Git").join("usr").join("bin").join("bash.exe"));
    }
    if let Some(local) = local_app_data {
        candidates.push(std::path::Path::new(local).join("Programs").join("Git").join("bin").join("bash.exe"));
    }
    if let Some(cand) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(cand.to_string_lossy().into_owned());
    }
    if let Some(path) = path_os {
        for dir in std::env::split_paths(path) {
            let cand = dir.join("bash.exe");
            let lower = dir.to_string_lossy().to_ascii_lowercase();
            if cand.is_file() && !lower.ends_with("\\system32") && !lower.ends_with("/system32") {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }
    None
}

pub(crate) fn resolve_windows_shell_argv_with(
    choice: &str,
    path_os: Option<&std::ffi::OsStr>,
    program_files: Option<&str>,
    program_files_x86: Option<&str>,
    local_app_data: Option<&str>,
    comspec: Option<&str>,
) -> Option<Vec<String>> {
    let ps_argv = |exe: String| vec![exe, "-NoExit".into(), "-Command".into(), WIN_PS_HOOK.into()];
    let c = choice.trim();
    match c.to_ascii_lowercase().as_str() {
        "" | "auto" | "wsl" => None,
        "powershell" | "windows-powershell" => Some(ps_argv("powershell.exe".into())),
        "pwsh" | "powershell7" => Some(ps_argv(
            find_exe_in_path(path_os, "pwsh.exe").unwrap_or_else(|| "pwsh.exe".into()),
        )),
        "git-bash" | "gitbash" | "bash" => {
            let exe = find_git_bash_with(path_os, program_files, program_files_x86, local_app_data)
                .unwrap_or_else(|| "bash.exe".into());
            Some(vec![exe, "--login".into(), "-i".into()])
        }
        "cmd" => Some(vec![comspec.filter(|s| !s.is_empty()).unwrap_or("cmd.exe").into()]),
        _ => Some(vec![c.to_owned()]),
    }
}

fn resolve_windows_shell_argv(choice: &str) -> Option<Vec<String>> {
    let path_os = std::env::var_os("PATH");
    let pf = std::env::var("ProgramFiles").ok();
    let pfx86 = std::env::var("ProgramFiles(x86)").ok();
    let local = std::env::var("LOCALAPPDATA").ok();
    let comspec = std::env::var("COMSPEC").ok();
    resolve_windows_shell_argv_with(
        choice,
        path_os.as_deref(),
        pf.as_deref(),
        pfx86.as_deref(),
        local.as_deref(),
        comspec.as_deref(),
    )
}

pub fn shell() -> String {
    if cfg!(windows) {
        let path = std::env::var_os("PATH").unwrap_or_default();
        return std::env::split_paths(&path)
            .map(|dir| dir.join("pwsh.exe"))
            .find(|exe| exe.is_file())
            .map_or_else(|| "powershell.exe".into(), |exe| exe.to_string_lossy().into_owned());
    }
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())
}

/// `cmd` in the person's shell, after their rc files (and so their PATH). `stay`: the
/// shell stays after it (a tab); else the shell becomes it, so its pane ends with it.
fn unix_agent_argv(sh: String, cmd: Vec<String>, login: bool, stay: bool) -> Vec<String> {
    let name = std::path::Path::new(&sh).file_name().and_then(|s| s.to_str());
    if !matches!(name, Some("sh" | "bash" | "zsh" | "dash" | "ksh" | "fish")) {
        if !stay {
            return cmd;
        }
        // Other shells cannot parse our wrapper: sh runs the command on the app's PATH, which the
        // installed check searches when the shell can't run it (`programs::installed_here`),
        // then the person's shell takes over.
        let path = std::env::var("PATH").map(|p| format!("PATH='{}' ", p.replace('\'', "'\\''"))).unwrap_or_default();
        let mut run = vec!["/bin/sh".into(), "-c".into(), format!("{path}\"$@\"; exec \"$0\" -i"), sh];
        run.extend(cmd);
        return run;
    }
    let fish = name == Some("fish");
    let script = match (fish, login, stay) {
        (true, true, true) => "command $argv[2..]; exec $argv[1] -li",
        (true, false, true) => "command $argv[2..]; exec $argv[1] -i",
        (false, true, true) => "\"$@\"; exec \"$0\" -li",
        (false, false, true) => "\"$@\"; exec \"$0\" -i",
        (true, _, false) => crate::remote_shell::FISH_EXEC,
        (false, _, false) => crate::remote_shell::POSIX_EXEC,
    };
    let mut run = vec![sh.clone(), if login { "-lic" } else { "-ic" }.into(), script.into(), sh];
    run.extend(cmd);
    run
}

/// A terminal that ended within 2 s, with an error. A split's wrapper that can't find its
/// program says so only after the person's shell config, however long that took.
fn launch_failure(record: &Value, output: impl FnOnce() -> String) -> Option<String> {
    let code = record["exit_code"].as_i64().filter(|c| *c != 0)?;
    let elapsed = record["finished_ms"].as_u64()?.checked_sub(record["created_ms"].as_u64()?)?;
    if record["reason"].as_str() != Some("exited") || (elapsed > 2000 && code != 127) {
        return None;
    }
    if elapsed > 2000 && !crate::remote_shell::cannot_start(record) {
        return None;
    }
    let output = output();
    let lines = output.lines().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>();
    let tail = lines[lines.len().saturating_sub(5)..].join("\n");
    Some(if tail.is_empty() { format!("Exit code {code}") } else { format!("Exit code {code}\n{tail}") })
}

/// The shipped layouts (defaults/layouts): `main` is Herdr's window (spaces
/// sidebar, tab bar).
const LAYOUTS: &[(&str, &str)] = &[
    ("main", include_str!("../defaults/layouts/main.json")),
    ("terminals", include_str!("../defaults/layouts/terminals.json")),
    ("win98", include_str!("../defaults/layouts/win98.json")),
];

/// Pictures shipped layouts name as their `preview`, written beside them.
const LAYOUT_PREVIEWS: &[(&str, &str)] = &[("win98.svg", include_str!("../defaults/layouts/win98.svg"))];

pub fn shipped_layouts() -> Vec<&'static str> {
    LAYOUTS.iter().map(|(n, _)| *n).collect()
}

fn shipped_layout(name: &str) -> &'static str {
    LAYOUTS.iter().find(|(n, _)| *n == name).unwrap_or(&LAYOUTS[0]).1
}

/// A layout as shipped (main for names Herdr doesn't ship).
fn default_layout(name: &str) -> Node {
    serde_json::from_str(shipped_layout(name)).expect("defaults/layouts are valid")
}

fn layout_path(name: &str) -> PathBuf {
    file(&format!("layouts/{name}.json"))
}

/// Layout names on disk, in order: main first, then the rest alphabetically.
/// A layout whose mods are gone (Zen after the update) isn't offered;
/// `layout.switch {name}` still reaches it.
pub(crate) fn layout_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(file("layouts"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_owned))
        .filter(|n| missing_mods(n).is_empty())
        .collect();
    names.sort_by_key(|n| (n != "main", n.clone()));
    names
}

fn state_path(name: &str) -> PathBuf {
    file(&format!("layouts/.state/{name}.json"))
}

/// The clicks on a layout (layout_sync.rs), none if unreadable.
fn read_state(name: &str) -> layout_sync::State {
    std::fs::read_to_string(state_path(name)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn state_text(s: &layout_sync::State) -> String {
    serde_json::to_string_pretty(s).unwrap_or_default() + "\n"
}

/// Where to set `path` aside: `<path>.<tag>-<time>`, with `-2`, `-3` ... when
/// that's taken, so an older one is never replaced.
pub(crate) fn aside(path: &std::path::Path, tag: &str) -> PathBuf {
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    (1..).map(|i| path.with_file_name(if i == 1 { format!("{name}.{tag}-{stamp}") } else { format!("{name}.{tag}-{stamp}-{i}") })).find(|p| !p.exists()).unwrap_or_else(|| path.to_path_buf())
}

/// Writes through a temporary file and a rename, so a reader never sees half
/// a file. Says whether it worked.
fn write_atomic(path: &std::path::Path, text: &str) -> bool {
    match crate::persist::write_atomic(path, text) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!("cannot write {}: {error}", path.display());
            false
        }
    }
}

/// The mods a layout file names that aren't in mods/ (frame mods are always there).
fn missing_mods(name: &str) -> Vec<String> {
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(o) => o.iter().for_each(|(k, v)| match v {
                Value::String(s) if k == "mod" => out.push(s.clone()),
                _ => walk(v, out),
            }),
            Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let Some(layout) = std::fs::read_to_string(layout_path(name)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return vec![] };
    let mut used = Vec::new();
    walk(&layout, &mut used);
    used.sort();
    used.dedup();
    used.retain(|m| !m.starts_with("frame-") && !mods::mods_dir().join(m).is_dir());
    used
}

fn install_layouts() {
    if let Err(e) = mods::sync_layouts(&mods::app_dir(), LAYOUTS) {
        tracing::warn!("cannot install layouts: {e}");
    }
    for (name, text) in LAYOUT_PREVIEWS {
        let _ = std::fs::write(file(&format!("layouts/{name}")), text);
    }
}

fn clone_layout(name: &str, to: &str) -> std::io::Result<()> {
    if !mods::valid_name(name) || !mods::valid_name(to) {
        return Err(std::io::Error::other("names use letters, digits, - or _"));
    }
    let mut node: Node = serde_json::from_str(&std::fs::read_to_string(layout_path(name))?)?;
    node.cloned_from = Some(crate::layout::ClonedFrom { name: name.into(), version: env!("CARGO_PKG_VERSION").into() });
    let text = layout_sync::text_of(&node);
    use std::io::Write;
    let mut dest = std::fs::OpenOptions::new().write(true).create_new(true).open(layout_path(to))?;
    let result = (|| {
        dest.write_all(text.as_bytes())?;
        mods::copy_layout_state(&mods::app_dir(), name, to)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(layout_path(to));
        let _ = std::fs::remove_file(state_path(to));
    }
    result
}

struct Term {
    view: Entity<TerminalView>,
    address: String,
    focus: FocusHandle,
    _sub: Subscription,
}

enum TerminalChange {
    Keep { space: String, tab: String, pane: String, name: String, target: Option<String>, axis: Axis, create_space: Option<(String, Option<String>)>, focus: bool },
    Park,
}

struct PendingTerminalChange {
    change: TerminalChange,
    reply: std::sync::mpsc::Receiver<Result<(), String>>,
}

/// The slot of a terminal shown in a pane in place of its own (`<pane>#swap`).
const SWAP: &str = "#swap";

/// The share of the script VM's heap ceiling past which a mod edit starts a
/// new VM instead of reloading into the old one (`files_changed`).
const RESTART_MODS_AT: f64 = 0.75;

/// QuickJS reports allocation failures both as exceptions and as host errors.
fn out_of_memory(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let message = cause.to_string();
        message.contains("out of memory") || message.contains("Allocation failed while creating object")
    })
}

struct Copy {
    inst: Instance,
    /// Its number for commands (`mods::prepare_load`); 0 if it never loaded.
    id: u64,
    root: Option<Entity<ShellRoot>>,
    _watch: Option<Watcher>,
    error: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum OverlayKind {
    Dialog,
    Popup,
    Toast,
    /// Stays until closed, moves where it's dropped, comes to the front when
    /// pressed (`movable.rs`). Never holds the keyboard on its own.
    Window,
}

impl OverlayKind {
    /// Dialogs and popups hold the keyboard while open (Esc closes them).
    fn modal_keys(self) -> bool {
        matches!(self, OverlayKind::Dialog | OverlayKind::Popup)
    }
}

struct Overlay {
    id: String,
    kind: OverlayKind,
    slot: String,
    at: Point<Pixels>,
    /// As designed; drawn at the current zoom. Up to 1: a share of the window.
    size: Size<f32>,
    expires: Option<Instant>,
    focus: FocusHandle,
    /// Blocks instead of one mod (terminals too), ids prefixed with the overlay's.
    layout: Option<Node>,
    /// `at: "top"`: centered, this far below the window's top.
    top: Option<f32>,
    /// As tall as its content (size.height is the most).
    fit: bool,
    /// A click beside it doesn't close it (the first-run welcome).
    modal: bool,
    /// `frame: "none"`: no border, corners, shadow or background; the mod draws its own.
    bare: bool,
    /// A window: where its top left is (None: centered) and its name (`key`).
    place: Option<Point<Pixels>>,
    key: Option<String>,
    /// Resizable floating window (`kind: "window"`, `resizable: true`).
    resizable: bool,
    min_size: Size<f32>,
    /// The app's own screen (a `frame-` mod: What's new, the welcome): only
    /// its own close ends it, never a layout switch.
    frame: bool,
}

struct Drag {
    container: String,
    index: usize,
    axis: Axis,
}

#[derive(Default)]
struct Watched {
    settings: Option<SystemTime>,
    keys: Option<SystemTime>,
    programs: Option<SystemTime>,
}

#[derive(Clone, Debug)]
struct ManualStatus {
    /// `Some("idle" | "working" | "blocked" | "done")`, or `None` while clearing (`"auto"`).
    status: Option<String>,
    turn: u64,
    reported_state: Option<String>,
    set_at: Instant,
    /// True once `herdr.status` has been observed back from core (or originally hydrated from core).
    label_synced: bool,
}

fn parse_manual_status_label(raw: &str) -> Option<(String, Option<u64>)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (word, turn) = match trimmed.split_once('@') {
        Some((w, t)) => (w.trim(), t.trim().parse::<u64>().ok()),
        None => (trimmed, None),
    };
    let canonical = match word {
        "idle" | "working" | "blocked" | "done" => word,
        _ => return None,
    };
    Some((canonical.to_owned(), turn))
}

fn normalize_manual_status(raw: &str) -> Option<Option<&'static str>> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "idle" => Some(Some("idle")),
        "working" => Some(Some("working")),
        "blocked" | "needs_you" | "needs-you" => Some(Some("blocked")),
        "done" => Some(Some("done")),
        "auto" | "clear" | "reset" => Some(None),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum RenameFocus {
    #[default]
    Tab,
    Space,
    Agent(Option<String>),
}

#[derive(Clone, Debug)]
struct AiTerminalVerdict {
    screen_hash: u64,
    output_at: Option<Instant>,
    typed_at: Option<Instant>,
    state: &'static str,
    kind: String,
    summary: String,
    semantic_title: String,
    micro_summary: String,
    decided_at: Instant,
    working_rechecks: u8,
}

#[derive(Clone, Debug)]
struct DigestWatermark {
    turn: u64,
    transcript_seq: u64,
    status: Option<String>,
    output_at: Option<Instant>,
    typed_at: Option<Instant>,
    attempted_at: Instant,
}

pub struct AppWindow {
    core: Arc<Core>,
    runtime: Option<Rc<ShellRuntime>>,
    restarting_mods: bool,
    load_recovery: Option<Instant>,
    failed_loads: HashSet<String>,
    mod_stamps: RefCell<HashMap<String, crate::files::ModStamp>>,
    chrome: Node,
    /// Which layout `chrome` is (`layouts/<name>.json`).
    layout_name: String,
    /// The agent an `{ "agent": "focused" }` block shows (by address); any core
    /// terminal, so sub-agents outside workspaces work too.
    agent: Option<String>,
    /// The agent view that has the keyboard, so it's only taken on change.
    agent_focused: Option<String>,
    /// Panes showing another terminal for now (pane → address), e.g. a sub-agent
    /// looked at in place of its parent. Not saved; the pane keeps its own.
    swapped: HashMap<String, String>,
    float_drag: Option<floats::FloatDrag>,
    pane_drag: Option<pane_drag::PaneDrag>,
    pane_grip_hover: Option<String>,
    /// Each pane's top row the grip sits centered in: its top header, else a strip.
    pane_grip_rows: RefCell<HashMap<String, f32>>,
    /// A window (`kind: "window"` overlay) being pressed or dragged.
    win_drag: Option<movable::WinDrag>,
    float_anims: HashMap<String, floats::FloatAnim>,
    float_reveal: HashMap<String, floats::Reveal>,
    float_hover: HashSet<String>,
    /// Bumped on every hover change, so a delayed leave knows it's still current.
    float_leave: HashMap<String, u64>,
    model: Model,
    /// Terminal views by block id (panes and chrome terminal blocks).
    terms: HashMap<String, Term>,
    /// Mod copies by slot (block id, `<stack>#header`, or `overlay:<id>`).
    copies: HashMap<String, Copy>,
    overlays: Vec<Overlay>,
    folders: BTreeMap<String, Found>,
    /// Mods placed once; later the layout decides where (or whether) they show.
    seen: BTreeSet<String>,
    bounds: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    hidden_fit: pane_size::HiddenFit,
    drag: Option<Drag>,
    written_layout: String,
    /// The shown layout as its file has it; `chrome` is this plus the clicks
    /// (layout_sync.rs).
    file_chrome: Node,
    written_state: String,
    picked_theme: Option<crate::layout::ThemeChoice>,
    /// Whether the shown layout's file read and parsed (or is simply
    /// missing): a broken one is never written over.
    layout_file_ok: bool,
    written_view: String,
    /// settings.json: zoom and sounds.
    person: settings::Person,
    /// What the last command made or why it didn't (for callers that wait for it).
    reply: Option<Value>,
    /// The `where` label last written per terminal address.
    written_where: HashMap<String, String>,
    /// `herdr.seen` turns written but not yet back from the core.
    written_seen: HashMap<String, u64>,
    /// Agents the person is looking at, and the turn they were shared as seen at.
    looked: HashMap<String, u64>,
    watched: Watched,
    /// Core's terminal and headless ACP agent records by address.
    live: HashMap<String, Value>,
    /// Mounted ACP sessions that finished externally and stay open read-only until closed.
    ended_acp: HashMap<String, Value>,
    /// Deterministic order matching the last `terminals_listed` batch.
    live_order: Vec<String>,
    /// PTY addresses that have hosted a child PID different from their root shell PID.
    shell_child_seen: HashSet<String>,
    /// Headless ACP agent currently shown inside the `terminal-peek` dialog overlay.
    peeked_agent: Option<String>,
    /// True when the right side-chat panel was auto-retracted because the focused agent has no side-chats.
    side_panel_auto_hidden: bool,
    /// Which sidebar/work target F2 rename acts on (`Tab`, `Space`, or `Agent`).
    rename_focus: RenameFocus,
    /// Manual status overrides (`herdr.status`), keyed by terminal/agent address.
    manual_statuses: HashMap<String, ManualStatus>,
    /// Cached async semantic arbitration verdicts for non-OSC-7501 PTY CLI agents.
    ai_terminal_states: HashMap<String, AiTerminalVerdict>,
    /// PTY addresses currently awaiting an in-flight `ai.classify_terminal_state` RPC.
    ai_inflight: HashSet<String>,
    /// Per-terminal throttle timestamp for semantic status arbitration.
    ai_last_attempt: HashMap<String, Instant>,
    /// Agent/terminal addresses currently awaiting an in-flight `ai.summarize_agent` RPC.
    ai_digest_inflight: HashSet<String>,
    /// Last conversation activity watermark attempted for automatic `ai.summarize_agent` refresh.
    ai_digest_attempted: HashMap<String, DigestWatermark>,
    /// Last terminal title observed while a PTY was running a CLI agent,
    /// used to clear stale CLI titles when returning to a shell without title hooks.
    last_agent_titles: HashMap<String, String>,
    /// Backoff deadline when local Core AI is unconfigured or disabled.
    ai_unavailable_until: Option<Instant>,
    /// Monotonic counter for `files.open` requests routed to the Files panel.
    file_open_seq: u64,
    listed_once: bool,
    /// Live folders by address (from the shell process).
    cwds: HashMap<String, String>,
    /// When each terminal's repo and branch were last read, and what it was doing.
    folders_checked: HashMap<String, FolderCheck>,
    /// When Herdr started each terminal: a listing already in flight can't know them yet.
    born: RefCell<HashMap<String, Instant>>,
    /// Connection epoch: ignore replies from an old connection.
    core_generation: u64,
    checking_closed: bool,
    documents_watching: bool,
    draining: bool,
    changing_terminals: HashSet<String>,
    /// Closed terminals stay excluded until kill completes and a listing drops them.
    closing_terminals: HashMap<String, bool>,
    pending_terminals: std::collections::BTreeMap<String, PendingTerminalChange>,
    /// Each machine owns a document; the empty key is this core.
    documents: std::collections::BTreeMap<String, crate::mirror_sync::Slot>,
    initial_view: Option<crate::mirror::View>,
    pointer: Point<Pixels>,
    /// Where a press on a `window_drag` node started, until it moves the window.
    drag_from: Option<Point<Pixels>>,
    titlebar_applied: Option<(crate::layout::Titlebar, Option<[f32; 2]>)>,
    blur_applied: bool,
    focused_pane: Option<String>,
    /// The mod block holding the keyboard for directional navigation.
    focused_block: Option<String>,
    synced_model_pane: Option<String>,
    /// A terminal block in a window (an overlay's layout) holding the keyboard.
    /// `focused()` reports it like a layout terminal block; the focused pane stays.
    window_terminal: Option<String>,
    /// Whether the last sync showed the tab, to hand it the keyboard back when a stack shows it again.
    tab_shown: bool,
    pane_history: Vec<String>,
    window_active: bool,
    /// When the window last lost OS focus, for away-return Catch-up Briefing detection.
    blurred_at: Option<Instant>,
    /// Agent/terminal statuses snapshotted at blur time to count new changes while away.
    away_statuses: HashMap<String, String>,
    /// The system's appearance; a layout with a light and a dark theme follows it.
    system_dark: bool,
    /// The OS tells us when files change (`files.rs`); else the disk is polled.
    file_events: Option<crate::files::FileEvents>,
    polls: u64,
    /// The downloaded update the frame's pill offers, and the one closed for now.
    /// What's waiting, in words (`update_label`).
    update_ready: Option<String>,
    #[allow(dead_code)]
    update_dismissed: Option<String>,
    core_update_error_dismissed: Option<String>,
    #[allow(dead_code)]
    update_success: bool,
    update_status: String,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
    /// Gives the keyboard back when what had it leaves the screen (`restore_focus`).
    _focus_lost: Option<Subscription>,
    /// Remembers where the window is (`place.rs`); the write waiting to settle.
    _place: Option<Subscription>,
    place_write: Option<Task<()>>,
}

impl AppWindow {
    pub fn new(core: Arc<Core>, requests: async_channel::Receiver<Request>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let file_events = crate::files::watch(&mods::app_dir(), #[cfg(test)] cx.background_executor().clone());
        Self::with_file_events(core, requests, file_events, window, cx)
    }

    fn with_file_events(core: Arc<Core>, requests: async_channel::Receiver<Request>, file_events: Option<crate::files::FileEvents>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let runtime = ShellRuntime::new(cx).map_err(|e| tracing::error!("script runtime failed, mods are off: {e:#}")).ok();
        if let (Some(runtime), Some(events)) = (&runtime, &file_events) {
            runtime.set_source_revision_provider(events.revision_provider());
        }
        mods::note_fonts(cx);
        let seen = std::fs::read_to_string(file("seen.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let mut w = AppWindow {
            core,
            runtime,
            restarting_mods: false,
            load_recovery: None,
            failed_loads: HashSet::new(),
            mod_stamps: RefCell::new(HashMap::new()),
            chrome: default_layout("main"),
            layout_name: "main".into(),
            agent: None,
            agent_focused: None,
            swapped: HashMap::new(),
            float_drag: None,
            pane_drag: None,
            pane_grip_hover: None,
            pane_grip_rows: RefCell::default(),
            win_drag: None,
            float_anims: HashMap::new(),
            float_reveal: HashMap::new(),
            float_hover: HashSet::new(),
            float_leave: HashMap::new(),
            model: Model::default(),
            terms: HashMap::new(),
            copies: HashMap::new(),
            overlays: Vec::new(),
            folders: mods::scan().into_iter().map(|f| (f.id.clone(), f)).collect(),
            seen,
            bounds: Default::default(),
            hidden_fit: Default::default(),
            drag: None,
            written_layout: String::new(),
            file_chrome: default_layout("main"),
            written_state: String::new(),
            picked_theme: None,
            layout_file_ok: true,
            written_view: String::new(),
            person: settings::Person::default(),
            reply: None,
            written_where: HashMap::new(),
            written_seen: HashMap::new(),
            looked: HashMap::new(),
            watched: Watched::default(),
            live: HashMap::new(),
            ended_acp: HashMap::new(),
            live_order: Vec::new(),
            shell_child_seen: HashSet::new(),
            peeked_agent: None,
            side_panel_auto_hidden: false,
            rename_focus: RenameFocus::Tab,
            manual_statuses: HashMap::new(),
            ai_terminal_states: HashMap::new(),
            ai_inflight: HashSet::new(),
            ai_last_attempt: HashMap::new(),
            ai_digest_inflight: HashSet::new(),
            ai_digest_attempted: HashMap::new(),
            last_agent_titles: HashMap::new(),
            ai_unavailable_until: None,
            file_open_seq: 0,
            listed_once: false,
            cwds: HashMap::new(),
            folders_checked: HashMap::new(),
            born: RefCell::new(HashMap::new()),
            core_generation: 0,
            checking_closed: false,
            documents_watching: false,
            draining: false,
            changing_terminals: HashSet::new(),
            closing_terminals: HashMap::new(),
            pending_terminals: Default::default(),
            documents: {
                let mut local = crate::mirror_sync::Slot::default();
                local.connection(true, None, None);
                [(String::new(), local)].into()
            },
            initial_view: std::fs::read_to_string(file("view.json")).ok().and_then(|s| serde_json::from_str(&s).ok()),
            pointer: Point::default(),
            drag_from: None,
            titlebar_applied: None,
            // Windows open opaque: leave the platform alone until a layout asks for blur.
            blur_applied: false,
            focused_pane: None,
            focused_block: None,
            synced_model_pane: None,
            window_terminal: None,
            tab_shown: true,
            pane_history: Vec::new(),
            window_active: true,
            blurred_at: None,
            away_statuses: HashMap::new(),
            system_dark: is_dark(window.appearance()),
            file_events,
            polls: 0,
            update_ready: None,
            update_dismissed: None,
            core_update_error_dismissed: None,
            update_success: false,
            update_status: String::new(),
            focus: cx.focus_handle(),
            _tasks: Vec::new(),
            _focus_lost: None,
            _place: None,
            place_write: None,
        };
        w._focus_lost = Some(cx.on_focus_lost(window, |this, window, cx| this.restore_focus(window, cx)));
        w._place = Some(cx.observe_window_bounds(window, |this, window, cx| {
            if let Some(t) = crate::place::changed(window, cx) {
                this.place_write = Some(t);
            }
        }));
        let closing = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            // macOS: the red button hides the app instead, so alerts keep ringing;
            // the Dock icon or a new launch brings it back, Cmd+Q quits.
            if cfg!(target_os = "macos") {
                cx.hide();
                return false;
            }
            let _ = closing.update(cx, |this, cx| {
                this.drain_documents(window, cx);
                this.close_mods_now(std::time::Instant::now() + Duration::from_millis(250), window, cx);
            });
            crate::place::save(window, cx);
            true
        });
        settings::install_themes();
        crate::programs::install_icons();
        w.reload_config(window, cx);
        install_layouts();
        let name = mods::shared("layout").and_then(|v| v.as_str().map(str::to_owned)).filter(|n| layout_path(n).exists());
        // A layout whose mods are gone (Zen, T4, ADE after the update took
        // theirs) starts on Herdr's instead of a wall of "did not load".
        let gone = name.as_deref().map(missing_mods).unwrap_or_default();
        w.load_layout(if gone.is_empty() { name.as_deref().unwrap_or("main") } else { "main" });
        #[cfg(target_os = "macos")]
        crate::macos::refresh_menus(cx);
        w.apply_look(window, cx);
        w.show_layout_notes(window, cx);
        if let (Some(n), false) = (&name, gone.is_empty()) {
            let note = json!({ "title": format!("{n} needs mods that aren't here"), "body": format!("Showing Herdr instead. Missing: {}. Your {n} layout file is still there.", gone.join(", ")), "level": "info", "timeout": 12000 });
            w.open_overlay(mods::new_overlay_id(), json!({ "kind": "toast", "notification": note }), window, cx);
        }

        w._tasks.push(cx.spawn_in(window, async move |this, cx| {
            loop {
                if this.update_in(cx, |this, window, cx| this.poll(window, cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(400)).await;
            }
        }));
        if let Some(events) = w.file_events.clone() {
            w._tasks.push(cx.spawn_in(window, async move |this, cx| {
                while let Ok(mut changed) = events.changes.recv().await {
                    // An editor's save is several events: take them together.
                    cx.background_executor().timer(Duration::from_millis(80)).await;
                    while let Ok(more) = events.changes.try_recv() {
                        changed.extend(more);
                    }
                    if this.update_in(cx, |this, window, cx| this.files_changed(changed, window, cx)).is_err() {
                        break;
                    }
                }
            }));
        }
        w._tasks.push(cx.spawn_in(window, async move |this, cx| {
            while let Ok(req) = requests.recv().await {
                let req = match req {
                    Request::Control { cmd, reply } => {
                        if let Some(path) = cmd.get("shot").and_then(Value::as_str) {
                            if this.upgrade().is_none() {
                                break;
                            }
                            (**cx).update(crate::terminal::flush_output_clock);
                            let body = match crate::control::save_shot(
                                cx.window_handle(),
                                cx,
                                std::path::Path::new(path),
                            ) {
                                Ok(()) => json!({ "path": path }),
                                Err(error) => {
                                    json!({ "error": format!("cannot save screenshot: {error:#}") })
                                }
                            };
                            let _ = reply.send(body);
                            continue;
                        }
                        Request::Control { cmd, reply }
                    }
                    other => other,
                };
                if this.update_in(cx, |this, window, cx| this.handle(req, window, cx)).is_err() {
                    break;
                }
            }
        }));
        w._tasks.push(cx.spawn_in(window, async move |this, cx| {
            // The terminals come from the core's events (a snapshot, then each
            // change), not from listing them all every tick; machines, which
            // send no events, are asked now and then.
            let mut subscription = None;
            let mut subscribed_generation = u64::MAX;
            let mut event_lines = crate::hc::EventLines::default();
            let mut known_machines = Value::Null;
            let (mut tick, mut relist) = (0u64, false);
            loop {
                let Ok((core, generation)) = this.update(cx, |this, _| {
                    (this.core.clone(), this.core_generation)
                }) else { break };
                if generation != subscribed_generation {
                    drop(subscription.take());
                    event_lines = crate::hc::EventLines::default();
                    subscribed_generation = generation;
                    let local_core = core.clone();
                    let (sub, result) = cx.background_executor().spawn(async move {
                        let sub = local_core.subscribe().ok();
                        let result = crate::mirror_sync::round(&local_core, crate::mirror_sync::Work {
                            machine: String::new(), version: 0, write: None,
                        });
                        (sub, result)
                    }).await;
                    subscription = sub.map(|(channel, events, snapshot)| {
                        event_lines = crate::hc::EventLines::new(&snapshot);
                        (channel, events)
                    });
                    (tick, relist) = (0, true);
                    if this.update_in(cx, |this, window, cx| {
                        if generation != this.core_generation { return; }
                        this.documents.entry(String::new()).or_default().connection(true, None, None);
                        this.documents_updated(None, vec![result], window, cx);
                        this.sync(window, cx);
                    }).is_err() { break; }
                }
                let ask_machines = tick.is_multiple_of(5);
                tick += 1;
                let relisting = std::mem::take(&mut relist);
                let (sub, machines, refresh, listed, lines, machine_names) = cx.background_executor().spawn(async move {
                    // The local stream forwards remote doc events too.
                    let mut sub = subscription;
                    let (mut refresh, mut terminals) = (false, relisting);
                    if sub.is_none() && let Ok((channel, events, snapshot)) = core.subscribe() {
                        event_lines = crate::hc::EventLines::new(&snapshot);
                        sub = Some((channel, events));
                        (refresh, terminals) = (true, true);
                    }
                    let mut closed = false;
                    if let Some((channel, events)) = &sub {
                        while let Ok(event) = events.try_recv() {
                            if let crate::hc::Stream::Data(bytes) = event {
                                let pushed = event_lines.push(&bytes);
                                refresh |= pushed.docs;
                                terminals |= pushed.terminals;
                                core.credit(*channel, bytes.len());
                            } else { (refresh, closed) = (true, true); }
                        }
                    }
                    // A stream that ended is subscribed to again, with a fresh snapshot.
                    if closed { sub = None; }
                    let machines = if ask_machines || terminals { core.call("machine.list", json!({})).ok() } else { None };
                    let machines_changed = machines.as_ref().is_some_and(|m| m != &known_machines);
                    if let Some(m) = &machines { known_machines = m.clone(); }
                    let listed = (terminals || machines_changed).then(|| event_lines.live_for(&core.hello, &known_machines));
                    (sub, machines, refresh, listed, event_lines, known_machines)
                }).await;
                let watching = sub.is_some();
                subscription = sub;
                event_lines = lines;
                known_machines = machine_names;
                let (seen, resynced) = event_lines.take_seen();
                if this.update_in(cx, |this, window, cx| {
                    if generation != this.core_generation { return; }
                    // Our seen writes the core has applied (or, after a fresh
                    // snapshot, can't tell): its labels decide again.
                    if resynced {
                        this.written_seen.clear();
                    } else {
                        this.written_seen.retain(|a, w| seen.get(a).is_none_or(|s| s < w));
                    }
                    this.documents_watching = watching;
                    if watching { this.documents.entry(String::new()).or_default().connection(true, None, None); }
                    let changed = this.documents_updated(machines, vec![], window, cx);
                    match listed {
                        Some(v) => this.terminals_listed(v, window, cx),
                        None => this.tend_terminals(false, window, cx),
                    }
                    if changed { this.sync(window, cx); }
                    this.fit_hidden_tabs(window, cx);
                    this.flush_documents(refresh, window, cx);
                }).is_err() { break; }
                cx.background_executor().timer(Duration::from_millis(500)).await;
            }
        }));
        w._tasks.push(cx.spawn_in(window, async move |this, cx| {
            // Cores restart (upgrades, crashes) and their programs survive: keep
            // the panes, reconnect, reattach the same terminals.
            loop {
                let Ok(core) = this.update(cx, |this, _| this.core.clone()) else { break };
                core.closed().await;
                tracing::warn!("lost the core; reconnecting");
                crate::telemetry::count("core_lost");
                let fresh = loop {
                    cx.background_executor().timer(Duration::from_millis(1000)).await;
                    if let Ok(core) = cx.background_executor().spawn(async { Core::connect() }).await {
                        break core;
                    }
                };
                if this.update(cx, |this, cx| this.reconnected(fresh, cx)).is_err() {
                    break;
                }
            }
        }));
        Self::note_core(&w.core);
        w.sync(window, cx);
        w.restore_focus(window, cx);
        // The first time the app opens: the frame's welcome (it sets
        // "frame.welcomed" when you start or skip).
        cx.defer_in(window, |this, window, cx| this.open_kept_windows(window, cx));
        // 0.1.0 kept space colors under its own word.
        if mods::shared("space.colors").is_none()
            && let Some(c) = mods::shared("project.colors")
        {
            mods::set_shared("space.colors".into(), c);
        }
        // Upstream Herdr release spotlights ("What's new") are temporarily disabled
        // until Herdr2 maintains its own release notes.
        if mods::shared("frame.welcomed").is_none() {
            cx.defer_in(window, |this, window, cx| this.open_welcome(window, cx));
        }
        crate::telemetry::window_open();
        w
    }

    /// What telemetry.rs counts: how much is open, never what's in it.
    fn observe(&self) {
        if !crate::telemetry::collecting() {
            return;
        }
        let m = &self.model;
        let agents: Vec<&model::Agent> = m.agents.iter().filter(|(a, _)| self.live.contains_key(*a) && m.status(a).is_some()).map(|(_, a)| a).collect();
        crate::telemetry::observe(crate::telemetry::Observation {
            foreground: self.window_active,
            agents_working: agents.iter().filter(|a| a.word.as_deref() == Some("working")).count(),
            agents: agents.iter().map(|a| a.app.clone()).collect(),
            spaces: m.spaces.len(),
            tabs: m.spaces.iter().map(|p| p.tabs.len()).sum(),
            terminals: m.spaces.iter().flat_map(|p| &p.tabs).map(|c| model::addresses(&c.root).len()).sum(),
            layout: self.layout_name.clone(),
            mods: self.visible().1.into_iter().map(|(_, i)| i.name).collect(),
            theme: settings::get().theme.clone(),
            welcome: mods::shared("frame.welcomed").and_then(|v| v.as_str().map(str::to_owned)),
        });
    }

    /// Disabled while original Herdr update/whats-new UI is suppressed.
    #[allow(dead_code)]
    fn open_whats_new(&mut self, _asked: bool, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn open_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A window over the live app (nothing dims), centered and movable:
        // each step changes what's behind it, and a click beside it doesn't end it.
        let spec = json!({ "mod": "frame-welcome", "kind": "window", "width": 660, "height": 600 });
        self.open_overlay(mods::new_overlay_id(), spec, window, cx);
    }

    /// Tells the updater which core this is and who owns it (off the UI thread).
    fn note_core(core: &Arc<Core>) {
        mods::set_live_core(core);
        let core = core.clone();
        std::thread::spawn(move || {
            gpui_shell::warm_syntax();
            if let Ok(s) = core.call("server.status", json!({})) {
                let text = |k: &str| s.get(k).and_then(Value::as_str).map(str::to_owned);
                crate::update::set_core(text("version").map(|v| v.trim_start_matches("h2 ").trim_start_matches("hc ").to_owned()), text("installed_by"));
            }
        });
    }

    fn reconnected(&mut self, core: Arc<Core>, cx: &mut Context<Self>) {
        tracing::info!("reconnected to the core");
        self.core_generation += 1;
        self.ended_acp.clear();
        self.documents_watching = false;
        for (machine, slot) in &mut self.documents {
            slot.available = false;
            slot.connected = machine.is_empty();
            slot.read_attempted = false;
            slot.pending = None;
            slot.blocked = false;
        }
        Self::note_core(&core);
        self.core = core.clone();
        self.publish(cx);
        for t in self.terms.values() {
            t.view.update(cx, |v, cx| v.reattach(core.clone(), cx));
        }
        cx.notify();
    }

    fn report(&self, what: &str, message: &str) {
        tracing::warn!("{what}.json not applied: {message}");
        let _ = std::fs::write(file(&format!("{what}.error")), format!("{what}.json was not applied: {message}\n"));
    }

    // ---- config files ------------------------------------------------------

    /// Changes settings.json from inside the app (the zoom keys). If the file
    /// is broken mid-edit, the change applies for now without touching it.
    fn write_person(&mut self, change: impl FnOnce(&mut settings::Person), window: &mut Window, cx: &mut Context<Self>) {
        let broken = settings::load_person().is_err();
        change(&mut self.person);
        if !broken {
            if let Err(e) = settings::write_person(&self.person) {
                tracing::warn!("cannot write settings.json: {e}");
            }
            self.watched.settings = mtime(&settings::path());
        }
        self.apply_look(window, cx);
    }

    /// Runs with the shown layout's look (its root node) at the person's zoom.
    fn apply_look(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A layout's fonts arrive with its mod: added since the start, they load now.
        if settings::load_home_fonts(cx) {
            mods::note_fonts(cx);
        }
        let s = settings::Settings::compose(&self.person, &self.chrome.look, self.system_dark);
        let problems = s.problems();
        if !problems.is_empty() {
            self.report_layout(&problems.join("; "));
        }
        if settings::set(s) {
            for term in self.terms.values() {
                term.view.read(cx).refresh_theme();
            }
            mods::apply_theme(cx);
            self.refresh_mods(cx);
            window.refresh();
        }
    }

    fn reload_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s_time = mtime(&settings::path());
        if s_time.is_none() || s_time != self.watched.settings {
            let loaded = settings::load_person();
            crate::telemetry::set_setting(settings::telemetry_setting(&loaded));
            match loaded {
                Ok(p) => {
                    self.person = p;
                    let _ = std::fs::remove_file(file("settings.error"));
                }
                Err(e) => self.report("settings", &e),
            }
            self.watched.settings = mtime(&settings::path());
            self.apply_look(window, cx);
        }
        let k_time = mtime(&crate::keys::path());
        if k_time.is_none() || k_time != self.watched.keys {
            match crate::keys::load() {
                Ok(bindings) => {
                    crate::keys::install(bindings, cx);
                    let _ = std::fs::remove_file(file("keys.error"));
                }
                Err(e) => {
                    self.report("keys", &e);
                    if self.watched.keys.is_none() {
                        crate::keys::install(vec![], cx);
                    }
                }
            }
            self.watched.keys = mtime(&crate::keys::path());
        }
        let p_time = mtime(&crate::programs::path());
        if p_time != self.watched.programs {
            match crate::programs::load() {
                Ok(()) => {
                    let _ = std::fs::remove_file(file("programs.error"));
                }
                Err(e) => self.report("programs", &e),
            }
            self.watched.programs = p_time;
            self.publish(cx);
        }
    }

    /// Makes `name` the shown layout (its file, or its default if missing or
    /// broken), with the person's clicks on it.
    fn load_layout(&mut self, name: &str) {
        self.layout_name = name.to_owned();
        self.written_layout.clear();
        let mut from_file = false;
        // A file that's there but broken or unreadable is shown as shipped for
        // now and never written to (nor its clicks) until it's fixed.
        self.layout_file_ok = true;
        let mut chrome = match std::fs::read_to_string(layout_path(name)) {
            Ok(text) => match serde_json::from_str::<Node>(&text) {
                Ok(n) => {
                    self.written_layout = text;
                    from_file = true;
                    let _ = std::fs::remove_file(file("layout.error"));
                    n
                }
                Err(e) => {
                    self.report_layout(&e.to_string());
                    self.layout_file_ok = false;
                    default_layout(name)
                }
            },
            Err(e) => {
                self.layout_file_ok = e.kind() == std::io::ErrorKind::NotFound;
                default_layout(name)
            }
        };
        if from_file {
            self.keep_ids(&mut chrome);
        } else {
            fill_ids(&mut chrome, &mut self.model);
        }
        self.file_chrome = chrome.clone();
        let mut state = read_state(name);
        if self.layout_file_ok {
            // Clicks on fields the file changed since (an edit while the app
            // was closed or another layout showed, an update) give way.
            state.forget_since_saved(&chrome);
        }
        self.picked_theme = state.theme.clone();
        state.apply(&mut chrome);
        self.chrome = chrome;
        self.written_state.clear();
        if self.layout_file_ok {
            self.write_state(&state);
        }
        mods::set_shared("layout".into(), json!(name));
    }

    fn report_layout(&self, message: &str) {
        let name = &self.layout_name;
        tracing::warn!("layouts/{name}.json not applied: {message}");
        let _ = std::fs::write(file("layout.error"), format!("layouts/{name}.json was not applied: {message}\n"));
    }

    /// Shows the layout `name`, or the next one after the current.
    fn switch_layout(&mut self, name: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let names = layout_names();
        let next = match name {
            Some(n) if mods::valid_name(n) && layout_path(n).exists() => n.to_owned(),
            Some(n) => {
                tracing::warn!("no layout {n:?} in layouts/");
                return;
            }
            None => {
                let i = names.iter().position(|n| *n == self.layout_name).map_or(0, |i| i + 1);
                match names.get(i % names.len().max(1)) {
                    Some(n) => n.clone(),
                    None => return,
                }
            }
        };
        self.save();
        self.float_drag = None;
        self.pane_drag = None;
        self.float_anims.clear();
        self.float_reveal.clear();
        self.float_hover.clear();
        let other = next != self.layout_name;
        if other {
            self.close_windows(window, cx);
            self.drop_layout_copies(window, cx);
        }
        self.load_layout(&next);
        #[cfg(target_os = "macos")]
        crate::macos::refresh_menus(cx);
        self.apply_look(window, cx);
        // Whatever the new layout shows takes the keyboard.
        self.focused_pane = None;
        self.agent_focused = None;
        self.sync(window, cx);
        if other {
            self.open_kept_windows(window, cx);
        }
    }

    fn check_layout_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(text) = std::fs::read_to_string(layout_path(&self.layout_name)) else { return };
        if text == self.written_layout {
            return;
        }
        match serde_json::from_str::<Node>(&text) {
            Ok(mut n) => {
                self.written_layout = text;
                let was_broken = !self.layout_file_ok;
                self.layout_file_ok = true;
                self.keep_ids(&mut n);
                // What the file now sets on purpose wins over a click on it.
                // Coming back from a broken file, the clicks are the saved ones
                // (what showed meanwhile was only the shipped stand-in).
                let state = if was_broken {
                    let mut s = read_state(&self.layout_name);
                    s.forget_since_saved(&n);
                    s
                } else {
                    let mut s = layout_sync::State::between(&self.file_chrome, &self.chrome);
                    s.theme = self.picked_theme.clone();
                    s.forget_edited(&self.file_chrome, &n);
                    s
                };
                self.file_chrome = n.clone();
                self.chrome = n;
                self.picked_theme = state.theme.clone();
                state.apply(&mut self.chrome);
                self.write_state(&state);
                let _ = std::fs::remove_file(file("layout.error"));
                self.apply_look(window, cx);
                self.sync(window, cx);
            }
            Err(e) => {
                self.written_layout = text;
                self.layout_file_ok = false;
                self.report_layout(&e.to_string());
            }
        }
    }

    /// Live folders used for unnamed spaces; not restart state.
    fn note_pane_folders(&mut self) {
        let panes: Vec<(String, String)> =
            self.model.tabs().flat_map(|c| c.root.terminals()).filter_map(|n| Some((n.id.clone(), n.terminal()?.to_owned()))).collect();
        self.model.pane_folders.retain(|p, _| panes.iter().any(|(id, _)| id == p));
        for (pane, addr) in panes {
            if let Some(cwd) = self.cwds.get(&addr) {
                self.model.pane_folders.insert(pane, cwd.clone());
            }
        }
    }

    /// Writes the clicks to the layout's state file and, only when something
    /// else changed (a mod docked, `layout.set`), the layout file.
    fn save(&mut self) {
        // Quitting (also the OS ending the session): programs may end in any
        // order now. Their panes aren't closing; the next start brings them back.
        if QUITTING.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        self.note_pane_folders();
        let mut state = layout_sync::State::between(&self.file_chrome, &self.chrome);
        state.theme = self.picked_theme.clone();
        if self.layout_file_ok {
            self.write_state(&state);
        }
        let edited = layout_sync::without_clicks(&self.file_chrome, &self.chrome);
        if edited != self.file_chrome && self.layout_file_ok && !shipped_layouts().contains(&self.layout_name.as_str()) {
            let path = layout_path(&self.layout_name);
            // Someone (an agent) changed the file since we read it: theirs
            // wins; the watcher loads it next.
            let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
            if !self.written_layout.is_empty() && on_disk != self.written_layout {
                tracing::info!("layouts/{}.json changed meanwhile: not overwritten", self.layout_name);
            } else {
                let layout = layout_sync::text_of(&edited);
                if write_atomic(&path, &layout) {
                    self.written_layout = layout;
                    self.file_chrome = edited;
                }
            }
        }
        self.save_view();
    }

    fn save_view(&mut self) {
        let mut view = crate::mirror::View::capture(&self.model);
        if let Some(saved) = &self.initial_view {
            view.preserve_unloaded(saved, |machine| self.documents.get(machine.unwrap_or("")).is_some_and(|s| s.document.is_some()));
        }
        let view = serde_json::to_string_pretty(&view).unwrap_or_default() + "\n";
        if self.documents.values().any(|s| s.document.is_some()) && view != self.written_view && write_atomic(&file("view.json"), &view) {
            self.written_view = view;
        }
    }

    /// What installing layouts kept of the person's own, as toasts.
    fn show_layout_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for clone in mods::take_sync_notes() {
            let n = json!({ "title": format!("Your edits are in {clone}"), "body": "Herdr's own layouts and mods can't be changed: clone one to make it yours.", "level": "info", "timeout": 12000 });
            self.open_overlay(mods::new_overlay_id(), json!({ "kind": "toast", "notification": n }), window, cx);
        }
    }

    /// Writes the clicks, with the file they were made on (so a later change
    /// to the file can win over them).
    fn write_state(&mut self, state: &layout_sync::State) {
        let mut state = state.clone();
        if !state.is_empty() {
            state.file = serde_json::to_value(&self.file_chrome).ok();
        }
        let text = state_text(&state);
        if text == self.written_state {
            return;
        }
        let path = state_path(&self.layout_name);
        let ok = if state.is_empty() {
            std::fs::remove_file(&path).is_ok() || !path.exists()
        } else {
            let _ = std::fs::create_dir_all(file("layouts/.state"));
            write_atomic(&path, &text)
        };
        if ok {
            self.written_state = text;
        }
    }

    /// Gives nodes without an id one and writes it into the file, so clicks
    /// on them (keyed by id) still find them next time.
    fn keep_ids(&mut self, n: &mut Node) {
        let before = n.clone();
        // Never one the tree (or yesterday's generated ids in it) already has.
        let taken: std::collections::HashSet<String> = n.walk().iter().map(|x| x.id.clone()).collect();
        let mut i = 0;
        n.fill_ids(&mut || loop {
            i += 1;
            let id = format!("node-{i}");
            if !taken.contains(&id) {
                break id;
            }
        });
        if *n != before {
            let text = layout_sync::text_of(n);
            if write_atomic(&layout_path(&self.layout_name), &text) {
                self.written_layout = text;
            }
        }
    }

    // ---- making the screen match the model ---------------------------------

    fn is_structured_terminal(&self, addr: &str) -> bool {
        !is_acp_addr(addr)
            && self.live.get(addr).and_then(|r| r.pointer("/labels/herdr.view")).and_then(Value::as_str) == Some("peek")
    }

    fn resolve_peer_addr(raw: &str, sibling: &str) -> String {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.contains('/') {
            return trimmed.to_owned();
        }
        match sibling.rfind('/') {
            Some(idx) => format!("{}{}", &sibling[..=idx], trimmed),
            None => trimmed.to_owned(),
        }
    }

    fn focused_main_terminal(&self) -> Option<String> {
        self.model
            .focused_pane()
            .and_then(|p| self.model.any_address_of(&p))
            .or_else(|| self.shown_agent())
    }

    fn is_side_chat(&self, addr: &str) -> bool {
        self.live
            .get(addr)
            .or_else(|| self.ended_acp.get(addr))
            .is_some_and(is_side_chat_record)
    }

    /// Removes a side-chat agent from any tab/pane and clears tab-mounting labels so it only lives
    /// in its parent agent's right-side side-chat panel.
    fn evict_side_chat_from_tabs(&mut self, addr: &str, cx: &mut Context<Self>) -> bool {
        let mut evicted = false;
        while let Some(pane) = self.model.any_pane_of(addr) {
            self.swapped.remove(&pane);
            self.model.remove_pane(&pane);
            evicted = true;
        }
        let has_tab_labels = self.live.get(addr).is_some_and(|r| {
            r.pointer("/labels/herdr.space").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                || r.pointer("/labels/herdr.tab").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
        });
        if evicted || has_tab_labels {
            let is_acp = is_acp_addr(addr);
            if let Some(obj) = self.live.get_mut(addr).and_then(Value::as_object_mut) {
                if !is_acp {
                    obj.insert("keep".into(), json!(false));
                }
                if let Some(labels) = obj.entry("labels").or_insert_with(|| json!({})).as_object_mut() {
                    labels.insert("herdr.space".into(), json!(""));
                    labels.insert("herdr.tab".into(), json!(""));
                    labels.insert("herdr.parked".into(), json!("1"));
                }
            }
            let core = self.core.clone();
            let address = addr.to_owned();
            let labels = json!({
                "herdr.space": "",
                "herdr.tab": "",
                "herdr.parked": "1",
            });
            cx.background_executor()
                .spawn(async move {
                    let (l_method, l_key) = if is_acp {
                        ("agent.label", "agent")
                    } else {
                        ("terminal.label", "terminal")
                    };
                    let _ = core.call(l_method, json!({ l_key: address, "labels": labels }));
                    if !is_acp {
                        let _ = core.call("terminal.keep", json!({ "terminal": address, "keep": false }));
                    }
                })
                .detach();
        }
        evicted
    }

    fn side_chats_for(&self, main_addr: &str) -> Vec<String> {
        let ordered = self.ordered_live_values();
        ordered
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                if r.get("state").and_then(Value::as_str) != Some("live") {
                    return None;
                }
                if r.pointer("/labels/herdr.trashed").is_some_and(|v| {
                    v.as_bool() == Some(true) || v.as_str().is_some_and(|s| !s.is_empty())
                }) {
                    return None;
                }
                let kind = r
                    .pointer("/lineage/kind")
                    .and_then(Value::as_str)
                    .map(|k| k.to_ascii_lowercase().replace('_', "-"));
                if kind.as_deref() != Some("side-chat") {
                    return None;
                }
                let addr = r.get("address").and_then(Value::as_str)?;
                let raw_parent = r
                    .pointer("/lineage/parent")
                    .or_else(|| r.get("parent"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let parent = Self::resolve_peer_addr(raw_parent, addr);
                (parent == main_addr).then(|| addr.to_owned())
            })
            .collect()
    }

    fn sync_side_chat_panel(&mut self) {
        let Some(body_active) = self
            .chrome
            .find("panel-body")
            .and_then(|n| n.active_child())
            .map(|c| c.id.clone())
        else {
            return;
        };
        let panel_hidden = self.chrome.find("panel").map(|n| n.hidden).unwrap_or(true);
        let main_addr = self.focused_main_terminal();
        let candidates = main_addr.as_deref().map(|m| self.side_chats_for(m)).unwrap_or_default();
        if candidates.is_empty() {
            if !panel_hidden && body_active == "side-chat" {
                if let Some(panel) = self.chrome.find_mut("panel") {
                    panel.hidden = true;
                }
                self.side_panel_auto_hidden = true;
            }
        } else {
            let saved = mods::shared("side-chat.active").and_then(|v| v.as_str().map(str::to_owned));
            if !saved.as_deref().is_some_and(|s| candidates.iter().any(|c| c == s))
                && let Some(latest) = candidates.last()
            {
                mods::set_shared("side-chat.active".to_string(), json!(latest));
            }
            if self.side_panel_auto_hidden && panel_hidden && body_active == "side-chat" {
                if let Some(panel) = self.chrome.find_mut("panel") {
                    panel.hidden = false;
                }
                self.side_panel_auto_hidden = false;
            }
        }
    }

    fn side_chat_terminal_mode(&self) -> Option<String> {
        let main_addr = self.focused_main_terminal()?;
        let candidates = self.side_chats_for(&main_addr);
        if candidates.is_empty() {
            return None;
        }
        let saved = mods::shared("side-chat.active").and_then(|v| v.as_str().map(str::to_owned));
        let active = saved
            .filter(|addr| candidates.iter().any(|c| c == addr))
            .or_else(|| candidates.last().cloned())?;
        let is_acp = is_acp_addr(&active)
            || self.live.get(&active).and_then(|r| r.get("acp")).and_then(Value::as_bool) == Some(true);
        if is_acp || self.is_structured_terminal(&active) {
            None
        } else {
            Some(active)
        }
    }

    fn pane_is_structured(&self, pane: &str) -> bool {
        !self.swapped.contains_key(pane)
            && self.model.tab_of(pane).is_some()
            && self.model.address_of(pane).is_some_and(|a| self.is_structured_terminal(&a))
    }

    fn structured_peek_mod(pane: &str, terminal: &str) -> (String, Instance) {
        (
            format!("{pane}#peek"),
            Instance {
                name: "peek".into(),
                props: json!({ "terminal": terminal, "acp": true, "pty": true, "tab": true, "pane": pane }),
            },
        )
    }

    /// Visible slots: terminal blocks (id → address) and mod slots (key → instance).
    fn visible(&self) -> (Vec<(String, String)>, Vec<(String, Instance)>) {
        let mut terms = Vec::new();
        let mut mods = Vec::new();
        let visit = |n: &Node, terms: &mut Vec<(String, String)>, mods: &mut Vec<(String, Instance)>| match n.content() {
            Some(Content::Terminal { terminal }) => terms.push((n.id.clone(), terminal.clone())),
            Some(Content::Mod { name, props }) => mods.push((n.id.clone(), Instance { name: name.clone(), props: props.clone() })),
            _ => {}
        };
        let stacks = |root: &Node, mods: &mut Vec<(String, Instance)>| {
            for n in visible_nodes(root) {
                if let Kind::Stack { header: Some(h), .. } = &n.kind
                    && let HeaderContent::Mod { name, props } = &h.content
                {
                    mods.push((format!("{}#header", n.id), Instance { name: name.clone(), props: props.clone() }));
                }
            }
        };
        for n in self.chrome.visible_blocks() {
            visit(n, &mut terms, &mut mods);
            if n.id == "side-chat"
                && let Some(addr) = self.side_chat_terminal_mode()
            {
                terms.push(("side-chat#term".into(), addr));
                mods.push((
                    "side-chat#term-header".into(),
                    Instance {
                        name: "peek".into(),
                        props: json!({ "sidePanel": true, "sideTermHeader": true }),
                    },
                ));
            }
            // An agent block with an agent outside every tab gets its own view.
            if matches!(n.content(), Some(Content::Agent { .. }))
                && let Some(addr) = self.shown_agent()
                && self.model.pane_of(&addr).is_none()
            {
                terms.push((n.id.clone(), addr));
            }
        }
        stacks(&self.chrome, &mut mods);
        for n in visible_nodes(&self.chrome) {
            for f in n.floating.iter().filter(|f| !f.hidden) {
                match &f.content {
                    Content::Mod { name, props } => mods.push(float_mod(&f.id, name, props)),
                    Content::Terminal { terminal } => terms.push((floats::slot(&f.id), terminal.clone())),
                    _ => {}
                }
            }
        }
        if self.shows_tab()
            && let Some(c) = self.model.active_tab()
        {
            let header = self.pane_frame().and_then(|f| f.header.as_ref()).and_then(|h| match &h.content {
                HeaderContent::Mod { name, props } => Some((name.clone(), props.clone())),
                HeaderContent::Builtin(_) => None,
            });
            for n in c.shown().visible_blocks() {
                if let Some((name, props)) = &header
                    && n.is_pane()
                {
                    mods.push(pane_header_mod(n, name, props));
                }
                if !self.swapped.contains_key(&n.id)
                    && let Some(terminal) = n.terminal()
                    && self.is_structured_terminal(terminal)
                {
                    mods.push(Self::structured_peek_mod(&n.id, terminal));
                } else {
                    visit(n, &mut terms, &mut mods);
                }
                if let Some(a) = self.swapped.get(&n.id) {
                    terms.push((format!("{}{SWAP}", n.id), a.clone()));
                }
            }
        }
        for o in &self.overlays {
            if let Some(c) = self.copies.get(&o.slot) {
                mods.push((o.slot.clone(), c.inst.clone()));
            }
            if let Some(l) = &o.layout {
                for n in l.visible_blocks() {
                    visit(n, &mut terms, &mut mods);
                }
            }
        }
        (terms, mods)
    }

    /// Mod slots whose copy may stay alive: every one in the layout (hidden
    /// nodes, inactive stack children, hidden floats), every pane mod of every
    /// tab (zoomed or not shown), and the overlays.
    fn owned_mods(&self) -> HashMap<String, Instance> {
        let mut out = HashMap::new();
        let inst = |name: &String, props: &Value| Instance { name: name.clone(), props: props.clone() };
        for n in self.chrome.walk() {
            if let Some(Content::Mod { name, props }) = n.content() {
                out.insert(n.id.clone(), inst(name, props));
                if n.id == "side-chat" {
                    out.insert(
                        "side-chat#term-header".into(),
                        Instance {
                            name: "peek".into(),
                            props: json!({ "sidePanel": true, "sideTermHeader": true }),
                        },
                    );
                }
            }
            if let Kind::Stack { header: Some(h), .. } = &n.kind
                && let HeaderContent::Mod { name, props } = &h.content
            {
                out.insert(format!("{}#header", n.id), inst(name, props));
            }
            for f in &n.floating {
                if let Content::Mod { name, props } = &f.content {
                    let (slot, i) = float_mod(&f.id, name, props);
                    out.insert(slot, i);
                }
            }
        }
        // The layout's pane header, shown or not (`pane_frame` says what's drawn now).
        let header = self
            .chrome
            .walk()
            .into_iter()
            .find_map(|n| match n.content() {
                Some(Content::Context { pane: Some(f), .. }) => f.header.as_ref(),
                _ => None,
            })
            .and_then(|h| match &h.content {
                HeaderContent::Mod { name, props } => Some((name.clone(), props.clone())),
                HeaderContent::Builtin(_) => None,
            });
        for c in self.model.tabs() {
            for n in c.root.walk() {
                if let Some(Content::Mod { name, props }) = n.content() {
                    out.insert(n.id.clone(), inst(name, props));
                }
                if !self.swapped.contains_key(&n.id)
                    && let Some(terminal) = n.terminal()
                    && self.is_structured_terminal(terminal)
                {
                    let (slot, i) = Self::structured_peek_mod(&n.id, terminal);
                    out.insert(slot, i);
                }
                if let Some((name, props)) = &header
                    && n.is_pane()
                {
                    let (slot, i) = pane_header_mod(n, name, props);
                    out.insert(slot, i);
                }
            }
        }
        for o in &self.overlays {
            if let Some(c) = self.copies.get(&o.slot) {
                out.insert(o.slot.clone(), c.inst.clone());
            }
            if let Some(l) = &o.layout {
                for n in l.visible_blocks() {
                    if let Some(Content::Mod { name, props }) = n.content() {
                        out.insert(n.id.clone(), inst(name, props));
                    }
                }
            }
        }
        out
    }

    /// Another layout: its mods start fresh. Pane mods (they belong to their
    /// tab) and overlays stay.
    fn drop_layout_copies(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let gone: Vec<_> = self.copies.keys().filter(|s| {
            self.model.tab_of(s.strip_suffix("#peek").unwrap_or(s)).is_none()
                && !self.overlays.iter().any(|o| o.slot == **s || o.layout.as_ref().is_some_and(|l| l.find(s).is_some()))
        }).cloned().collect();
        for slot in gone {
            self.remove_copy(&slot, window, cx);
        }
    }

    fn remove_copy(&mut self, slot: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(copy) = self.copies.remove(slot)
            && let (Some(runtime), Some(root)) = (&self.runtime, copy.root)
        {
            runtime.close(&root, std::time::Instant::now() + Duration::from_millis(250), window, cx).detach();
        }
    }

    pub(crate) fn close_mods_now(&self, deadline: std::time::Instant, window: &mut Window, cx: &mut Context<Self>) {
        let Some(runtime) = &self.runtime else { return };
        mods::final_cleanup(self.core.clone(), deadline, || {
            for root in self.copies.values().filter_map(|copy| copy.root.as_ref()) {
                runtime.close_now(root, deadline, window, cx);
            }
        });
    }

    fn shows_tab(&self) -> bool {
        self.chrome.visible_blocks().iter().any(|n| matches!(n.content(), Some(Content::Context { .. })))
    }

    fn agent_block(&self) -> Option<String> {
        self.chrome.visible_blocks().iter().find(|n| matches!(n.content(), Some(Content::Agent { .. }))).map(|n| n.id.clone())
    }

    fn shows_agent(&self) -> bool {
        self.agent_block().is_some()
    }

    /// The mod listing agents chooses navigation order; model order is the fallback.
    fn agent_order(&self) -> Vec<String> {
        let m = &self.model;
        mods::agent_order(m.all_addresses().into_iter().filter(|a| !is_acp_addr(a) && m.status(a).is_some()).collect())
    }

    /// The agent an agent block shows: the chosen one while it lives, else the
    /// focused pane if it's an agent, else the first agent.
    fn shown_agent(&self) -> Option<String> {
        let focused = self.model.focused_pane().and_then(|p| self.model.address_of(&p));
        self.agent
            .clone()
            .filter(|a| !is_acp_addr(a) && self.live.contains_key(a))
            .or_else(|| focused.filter(|a| !is_acp_addr(a) && self.model.status(a).is_some()))
            .or_else(|| self.agent_order().into_iter().next())
    }

    /// The terminal view showing `addr` in an agent block: its pane's, or the block's own.
    fn agent_view(&self, block: &str, addr: &str) -> Option<&Term> {
        self.model.pane_of(addr).and_then(|p| self.terms.get(&p)).or_else(|| self.terms.get(block).filter(|t| t.address == addr))
    }

    /// What's on screen now, for done-vs-seen.
    fn visible_addresses(&self) -> Vec<String> {
        let (terms, _) = self.visible();
        // A swapped-in terminal is drawn instead of its pane's own.
        let covered: HashSet<&str> = terms.iter().filter_map(|(slot, _)| slot.strip_suffix(SWAP)).collect();
        let mut out: Vec<String> = terms.iter().filter(|(slot, _)| !covered.contains(slot.as_str())).map(|(_, a)| a.clone()).collect();
        if self.shows_agent() {
            out.extend(self.shown_agent());
        }
        if self.shows_tab()
            && let Some(tab) = self.model.active_tab()
        {
            for block in tab.shown().visible_blocks() {
                if let Some(addr) = block.acp_terminal() {
                    out.push(addr.to_owned());
                }
            }
        }
        if self.overlays.iter().any(|o| o.id == "terminal-peek")
            && let Some(addr) = &self.peeked_agent
        {
            out.push(addr.clone());
        }
        out
    }

    /// Makes `addr` the shown agent; a pane showing it also becomes the focused pane.
    fn focus_agent(&mut self, addr: &str) {
        if let Some(p) = self.model.pane_of(addr) {
            self.model.focus_pane(&p);
        }
        self.focused_pane = None;
        self.agent = Some(addr.to_owned());
    }

    /// The parent chain's top: the agent a sub-agent belongs to in the agent order.
    fn root_agent(&self, addr: &str) -> String {
        let mut cur = addr.to_owned();
        for _ in 0..32 {
            let parent = self.live.get(&cur).and_then(|r| r.get("parent")).and_then(Value::as_str);
            match parent {
                Some(p) if self.live.contains_key(p) => cur = p.to_owned(),
                _ => break,
            }
        }
        cur
    }

    /// Creates what's visible, drops what isn't, publishes state, saves, focuses.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let had_structured_focus = self.focused_pane.as_deref().is_some_and(|p| self.copies.contains_key(&format!("{p}#peek")));
        self.model.order_spaces();
        self.sync_side_chat_panel();
        let (model, live) = (&self.model, &self.live);
        self.swapped.retain(|p, a| model.address_of(p).is_some() && live.contains_key(a));
        let (mut terms, mods_visible) = self.visible();
        let covered: HashSet<&str> = terms.iter().filter_map(|(slot, _)| slot.strip_suffix(SWAP)).collect();
        let mut displayed: HashSet<String> = terms.iter().filter(|(slot, _)| !covered.contains(slot.as_str())).map(|(slot, _)| slot.clone()).collect();
        // Agent blocks reuse their agent's pane view even when its tab isn't shown.
        if self.shows_agent()
            && let Some(addr) = self.shown_agent()
            && let Some(pane) = self.model.pane_of(&addr)
        {
            displayed.insert(pane);
        }
        let no_dialog_now = !self.overlays.iter().any(|o| o.kind.modal_keys());
        for (id, t) in &self.terms {
            if !displayed.contains(id) {
                t.view.update(cx, |v, _| v.blur());
                if no_dialog_now && t.view.read(cx).owns_keyboard_focus(window, cx) {
                    self.focus.focus(window, cx);
                }
            }
        }
        // Every tab's terminals stay attached (only visible ones are drawn): tab
        // switches are instant and kitty images survive, which a repaint can't bring back.
        for c in self.model.tabs() {
            for n in c.root.terminals() {
                if let Some(a) = n.terminal()
                    && !terms.iter().any(|(id, _)| *id == n.id)
                {
                    terms.push((n.id.clone(), a.to_owned()));
                }
            }
        }
        let want_terms: HashSet<String> = terms.iter().map(|(id, _)| id.clone()).collect();
        for (id, address) in terms {
            if self.terms.get(&id).is_some_and(|t| t.address == address) {
                continue;
            }
            let rec = self.live.get(&address);
            let dim = |k: &str, d: u64| rec.and_then(|r| r.get(k)).and_then(Value::as_u64).unwrap_or(d) as u16;
            let size_hint = (dim("cols", 120), dim("rows", 40));
            let view = cx.new(|cx| TerminalView::attach(self.core.clone(), address.clone(), size_hint, cx));
            self.tell_term(&view, &address, cx);
            mods::track_pane_view(id.clone(), &view);
            let (block, addr) = (id.clone(), address.clone());
            let sub = cx.subscribe_in(&view, window, move |this, _, ev: &TerminalEvent, window, cx| match ev {
                TerminalEvent::Peek(address) => this.peek_terminal(address, window, cx),
                TerminalEvent::OpenFile { path, line, end_line, on } => {
                    this.open_file_in_panel(path, *line, *end_line, on.as_deref(), None, window, cx);
                }
                TerminalEvent::TitleChanged => this.publish(cx),
                TerminalEvent::ScrollChanged => {
                    mods::panes_scrolled();
                    this.refresh_snapshot_mods(cx);
                }
                TerminalEvent::SelectionChanged => this.publish_selection(&addr, window, cx),
                TerminalEvent::Ended => this.terminal_ended(&block, window, cx),
                TerminalEvent::ContextMenu(at) => {
                    this.open_menu("pane", json!({ "pane": block.trim_end_matches(SWAP), "terminal": addr }), Some(*at), window, cx)
                }
                TerminalEvent::Notice { title, body } => {
                    let spec = json!({"kind":"toast", "notification":{"title":title, "body":body, "level":"error"}});
                    this.open_overlay(mods::new_overlay_id(), spec, window, cx)
                }
            });
            let focus = view.read(cx).focus_handle().clone();
            if let Some(previous) = self.terms.insert(id, Term { view, address, focus, _sub: sub }) {
                self.publish_selection(&previous.address, window, cx);
            }
        }
        let gone: Vec<String> = self.terms.keys().filter(|id| !want_terms.contains(*id)).cloned().collect();
        for id in gone {
            if let Some(t) = self.terms.remove(&id) {
                t.view.read(cx).detach();
                self.publish_selection(&t.address, window, cx);
            }
        }

        // A mod copy lives while its slot is in the layout, hidden or not (an
        // inactive stack child, a hidden column, a pane in another tab), so it
        // keeps its place; only shown slots start a copy. Gone or changed: dropped.
        let owned = self.owned_mods();
        let gone: Vec<_> = self.copies.iter().filter(|(slot, copy)| owned.get(*slot).is_none_or(|inst| *inst != copy.inst)).map(|(slot, _)| slot.clone()).collect();
        for slot in gone {
            self.remove_copy(&slot, window, cx);
        }
        let want: HashMap<String, Instance> = mods_visible.into_iter().collect();
        for (slot, inst) in want {
            if !self.copies.contains_key(&slot) {
                let copy = self.load(inst, window, cx);
                if (slot.starts_with("float:") || slot == "side-chat#term-header")
                    && let Some(root) = &copy.root
                {
                    root.update(cx, |r, cx| r.set_fit_content(true, cx));
                }
                self.copies.insert(slot, copy);
            }
        }
        self.publish_shown_copies();

        // Keyboard to the focused pane when it changes (never steal it from an open dialog).
        let model_pane = self.model.focused_pane();
        let block = terminal_block(&self.chrome, self.focused_pane.as_deref());
        let focused = if block.is_some() && model_pane == self.synced_model_pane {
            self.focused_pane.clone()
        } else {
            model_pane.clone()
        };
        let pane_changed = focused != self.focused_pane;
        // A pane focused some other way, or the block leaving, ends it.
        if (pane_changed && self.focused_block != focused) || self.focused_block.as_ref().is_some_and(|b| !self.mod_block_shown(b)) {
            self.focused_block = None;
        }
        // Mod blocks too, so `pane.go` goes back to a mod it came from.
        self.pane_history.retain(|id| {
            self.copies.contains_key(id) || self.model.spaces.iter().any(|s| s.tabs.iter().any(|t| t.root.find(id).is_some()))
        });
        if let Some(id) = self.focused_block.as_ref().or(model_pane.as_ref()) {
            remember_pane(&mut self.pane_history, id);
        }
        self.synced_model_pane = model_pane;
        self.focused_pane = focused.clone();
        let no_dialog = !self.overlays.iter().any(|o| o.kind.modal_keys());
        // The tab shown again (`layout.select` back from another stack child): its
        // focused pane gets the keyboard, which went to the window while it was hidden.
        let tab_shown = self.shows_tab();
        let revealed = tab_shown && !self.tab_shown;
        self.tab_shown = tab_shown;
        if terminal_block(&self.chrome, focused.as_deref()).is_some() {
            if pane_changed && no_dialog && let Some(f) = focused.as_deref().and_then(|p| self.pane_focus(p, cx)) {
                f.focus(window, cx);
            }
        } else if let Some(block) = self.agent_block() {
            let shown = self.shown_agent();
            if shown != self.agent_focused || pane_changed {
                self.agent_focused = shown.clone();
                if no_dialog && let Some(t) = shown.and_then(|a| self.agent_view(&block, &a)) {
                    t.focus.focus(window, cx);
                }
            }
        } else if pane_changed {
            // Outside an agent view, the agent shown later follows the focused pane.
            self.agent = None;
            // A mod pane takes the keyboard on a click; until then the window holds
            // it, so another tab's terminal can't keep it (and pull its tab back).
            if no_dialog {
                focused.and_then(|p| self.pane_focus(&p, cx)).unwrap_or_else(|| self.focus.clone()).focus(window, cx);
            }
        } else if had_structured_focus && focused.as_deref().is_some_and(|p| !self.pane_is_structured(p)) {
            if no_dialog && let Some(f) = focused.as_deref().and_then(|p| self.pane_focus(p, cx)) {
                f.focus(window, cx);
            }
        } else if revealed {
            self.refocus_pane(window, cx);
        }
        if self.window_active {
            let visible = self.visible_addresses();
            self.model.mark_seen(&visible);
        }
        self.share_seen(cx);
        self.publish(cx);
        self.save();
        cx.notify();
    }

    /// Shares what the person looks at as `herdr.seen` (docs/labels.md): an
    /// agent on screen in the active window, once per turn while it stays
    /// there, so a phone marking it unread isn't undone by a screen nobody
    /// watches. Only ever raises it. True when a status changed.
    fn share_seen(&mut self, cx: &mut Context<Self>) -> bool {
        let looking: HashSet<String> = if self.window_active { self.visible_addresses().into_iter().collect() } else { HashSet::new() };
        self.looked.retain(|a, _| looking.contains(a));
        let mut changed = false;
        for addr in looking {
            let Some(a) = self.model.agents.get(&addr) else { continue };
            let turn = a.turn;
            if a.word.is_none() || !self.live.contains_key(&addr) || self.looked.get(&addr) == Some(&turn) {
                continue;
            }
            self.looked.insert(addr.clone(), turn);
            if a.seen_turn.is_some_and(|s| s >= turn) {
                continue;
            }
            // Shown as seen now, not after the core's round trip.
            changed |= self.model.set_turn(&addr, turn, Some(turn));
            self.written_seen.insert(addr.clone(), turn);
            let core = self.core.clone();
            cx.spawn(async move |this, cx| {
                let (method, params) = if is_acp_addr(&addr) {
                    ("agent.label", json!({ "agent": addr, "labels": { "herdr.seen": turn.to_string() } }))
                } else {
                    ("terminal.label", json!({ "terminal": addr, "labels": { "herdr.seen": turn.to_string() } }))
                };
                let Err(e) = cx.background_executor().spawn(async move { core.call(method, params) }).await else { return };
                tracing::warn!("seen label for {addr}: {e:#}");
                let _ = this.update(cx, |this, _| {
                    if this.written_seen.get(&addr) == Some(&turn) {
                        this.written_seen.remove(&addr);
                        this.looked.remove(&addr);
                    }
                });
            })
            .detach();
        }
        changed
    }

    /// The keyboard never ends up nowhere. When what had it leaves the screen (a
    /// mod hidden but kept alive, a closed pane, a hidden mod focusing itself on a
    /// reload), it goes to the top dialog, else the shown agent or focused pane,
    /// else the window, where the app's keys work.
    fn restore_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = self.overlays.iter().rev().find(|o| o.kind.modal_keys()).map(|o| o.focus.clone());
        // A mod block holding the keyboard gets it back (a popup over it closed).
        if dialog.is_none() && self.focused_block.as_ref().is_some_and(|b| self.mod_block_shown(b)) {
            self.focus.focus(window, cx);
            return self.hand_keyboard();
        }
        let shown = if terminal_block(&self.chrome, self.focused_pane.as_deref()).is_some() {
            self.focused_pane.as_deref().and_then(|p| self.pane_focus(p, cx))
        } else if let Some(block) = self.agent_block() {
            self.shown_agent().and_then(|a| self.agent_view(&block, &a)).map(|t| t.focus.clone())
        } else if self.shows_tab() {
            self.model.focused_pane().and_then(|p| self.pane_focus(&p, cx))
        } else {
            None
        };
        dialog.or(shown).unwrap_or_else(|| self.focus.clone()).focus(window, cx);
    }

    /// The terminal view a pane shows: the swapped-in one, else its own.
    fn pane_term(&self, pane: &str) -> Option<&Term> {
        self.swapped.get(pane).and_then(|_| self.terms.get(&format!("{pane}{SWAP}"))).or_else(|| self.terms.get(pane))
    }

    /// Key path for `terminal.copy`. Copies and clears when there is a selection.
    /// Returns whether it copied. False means the key should reach the program.
    fn copy_key(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        let Some(t) = self.focused_terminal(window, cx) else { return true };
        t.view.update(cx, |v, cx| {
            if !v.copy(cx) {
                return false;
            }
            v.clear_selection(cx);
            true
        })
    }

    /// Peeks, floats and swapped-in views own keyboard focus without changing the model pane.
    fn focused_terminal(&self, window: &Window, cx: &App) -> Option<&Term> {
        self.terms.values().find(|t| t.view.read(cx).owns_keyboard_focus(window, cx))
            .or_else(|| self.focused_pane.as_deref().filter(|p| !self.pane_is_structured(p)).and_then(|p| self.pane_term(p)))
            .or_else(|| self.model.focused_pane().filter(|p| !self.pane_is_structured(p)).and_then(|p| self.pane_term(&p)))
    }

    fn shown_terminal_view(&self, address: &str, window: &Window, cx: &App) -> Result<Entity<TerminalView>, String> {
        if !self.live.contains_key(address) {
            return Err(format!("no running terminal {address:?}"));
        }

        let own_pane = self.model.pane_of(address);
        // The view the person sees wins over a hidden one, such as a peek over its own tab's pane.
        let best = self.terms.iter().filter(|(_, t)| t.address == address).max_by_key(|(pane, t)| {
            let view = t.view.read(cx);
            (view.owns_keyboard_focus(window, cx), view.on_screen_this_frame(), own_pane.as_deref() == Some(pane.as_str()))
        });
        best.map(|(_, t)| t.view.clone()).ok_or_else(|| format!("terminal {address:?} is running but not shown in this window, so it has no view"))
    }

    fn publish_selection(&self, address: &str, window: &Window, cx: &mut Context<Self>) {
        let selection = self.shown_terminal_view(address, window, cx).ok().and_then(|view| view.read(cx).selection());
        let changed = mods::publish_selection(address, json!(selection));
        self.publish_focused_selection(window, cx);
        if changed {
            self.refresh_snapshot_mods(cx);
        }
    }

    fn publish_focused_selection(&self, window: &Window, cx: &mut Context<Self>) {
        let selection = self.focused_terminal(window, cx).and_then(|t| t.view.read(cx).selection());
        if mods::publish_focused_selection(json!(selection)) {
            self.refresh_snapshot_mods(cx);
        }
    }

    fn scroll_terminal(&self, args: &Value, window: &Window, cx: &mut Context<Self>) -> Result<(), String> {
        let target = scroll_target(args)?;

        let view = match Self::owned(args, "terminal") {
            Some(address) => self.shown_terminal_view(&address, window, cx)?,
            None => self.focused_terminal(window, cx).map(|t| t.view.clone()).ok_or("no focused terminal to scroll")?,
        };

        view.update(cx, |view, cx| view.scroll_history(target, cx));
        Ok(())
    }

    /// Gives the focused pane the keyboard, unless something other than the
    /// window holds it: a dialog, a mod block, an input.
    fn refocus_pane(&self, window: &mut Window, cx: &mut App) {
        let dialog = self.overlays.iter().any(|o| o.kind.modal_keys());
        let window_holds_keys = window.focused(cx).is_none() || self.focus.is_focused(window);
        if dialog || self.focused_block.is_some() || !window_holds_keys {
            return;
        }
        if let Some(f) = self.focused_pane.as_ref().and_then(|p| self.pane_focus(p, cx)) {
            f.focus(window, cx);
        }
    }

    fn pane_focus(&self, pane: &str, cx: &App) -> Option<FocusHandle> {
        if self.pane_is_structured(pane) {
            return None;
        }
        self.pane_term(pane).map(|t| t.view.read(cx).focus_target(cx))
    }

    fn load(&self, inst: Instance, window: &mut Window, cx: &mut Context<Self>) -> Copy {
        let mut copy = Copy { inst, id: 0, root: None, _watch: None, error: None };
        let (Some(runtime), Some(folder)) = (&self.runtime, self.folders.get(&copy.inst.name)) else {
            copy.error = Some(format!("no mod folder ~/.config/herdr2-app/mods/{}", copy.inst.name));
            return copy;
        };
        copy.id = mods::prepare_load(&copy.inst);
        self.mod_stamps.borrow_mut().entry(copy.inst.name.clone()).or_insert_with(|| crate::files::mod_stamp(&folder.dir));
        match runtime.try_load(&folder.dir, window, cx) {
            Ok(root) => {
                if self.file_events.is_none() {
                    copy._watch = runtime.watch(&root, window, cx).map_err(|e| tracing::warn!("{}: no hot reload: {e:#}", copy.inst.name)).ok();
                }
                copy.root = Some(root);
            }
            Err(e) => {
                tracing::warn!("mod {} failed to load: {e:#}", copy.inst.name);
                copy.error = Some(format!("{e:#}"));
                if !self.restarting_mods && (out_of_memory(&e) || runtime.memory_share() >= RESTART_MODS_AT) {
                    // Finish mounting before retrying all copies. Other failures on
                    // this VM must not restart the replacement again.
                    let failed = Rc::downgrade(runtime);
                    let name = copy.inst.name.clone();
                    cx.defer_in(window, move |this, window, cx| {
                        if failed.upgrade().is_some_and(|failed| this.runtime.as_ref().is_some_and(|current| Rc::ptr_eq(&failed, current))) {
                            this.recover_mods(&BTreeSet::from([name]), window, cx);
                        }
                    });
                }
            }
        }
        copy
    }

    fn refresh_mods(&self, cx: &mut Context<Self>) {
        self.refresh_mods_matching(|_| true, cx);
    }

    fn refresh_snapshot_mods(&self, cx: &mut Context<Self>) {
        self.refresh_mods_matching(mods::snapshot_changed, cx);
    }

    fn refresh_mods_matching(&self, changed: impl Fn(u64) -> bool, cx: &mut Context<Self>) {
        if let Some(runtime) = &self.runtime {
            for c in self.copies.values() {
                if let Some(root) = &c.root
                    && changed(c.id)
                {
                    let _ = runtime.refresh(root, cx);
                }
            }
        }
    }

    /// How a terminal or ACP agent shows (`program` for mods): its agent, else what it runs.
    fn program(&self, addr: &str) -> Value {
        if let Some(p) = self.live.get(addr).or_else(|| self.ended_acp.get(addr)).and_then(|r| r.get("program")).filter(|v| !v.is_null()) {
            return p.clone();
        }
        let rec = self.live.get(addr).or_else(|| self.ended_acp.get(addr)).and_then(Value::as_object);
        program_of(rec.and_then(|r| r.get("report")).unwrap_or(&Value::Null), rec.and_then(running_argv))
    }

    fn normalize_ended_acp(addr: &str, mut v: Value) -> Value {
        let Some(obj) = v.as_object_mut() else {
            return json!({
                "address": addr,
                "acp": true,
                "reported": true,
                "state": "finished",
                "status": "ended",
            });
        };
        let report = obj.remove("status").unwrap_or(Value::Null);
        let app_hint = report
            .get("app")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| obj.get("agent").and_then(Value::as_str).filter(|s| !s.is_empty()));
        let mut prog = program_of(&json!({ "app": app_hint }), obj.get("argv"));
        prog["agent"] = json!(true);
        let raw_title = obj
            .get("title")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| obj.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()))
            .or(app_hint)
            .unwrap_or(addr)
            .to_owned();
        let shown = crate::terminal::shown_title(&raw_title).into_owned();
        obj.insert("acp".into(), json!(true));
        obj.insert("reported".into(), json!(true));
        obj.insert("state".into(), json!("finished"));
        obj.insert("status".into(), json!("ended"));
        obj.insert("report".into(), report);
        obj.insert("program".into(), prog);
        obj.insert("raw_title".into(), raw_title.into());
        obj.insert("title".into(), shown.into());
        v
    }

    /// Tells mods what changed (they re-render only if something did).
    fn publish(&mut self, cx: &mut Context<Self>) {
        let m = &self.model;
        let home = home();
        let spaces: Vec<Value> = m
            .spaces
            .iter()
            .enumerate()
            .map(|(i, p)| {
                // The branch of the terminal the space shows first.
                let first = p.active_tab().and_then(|c| c.focused_pane()).and_then(|pane| m.address_of(&pane));
                let branch = first.and_then(|a| m.folder_info.get(&a)).and_then(|f| f.branch.clone());
                model::space_json(m, p, i, branch.as_deref(), &home)
            })
            .collect();
        // A window's terminal with the keyboard first, then a layout terminal block.
        let block = overlay_terminal_block(&self.overlays, self.window_terminal.as_deref())
            .or_else(|| terminal_block(&self.chrome, self.focused_pane.as_deref()));
        let focused_pane = if block.is_some() { None } else { m.focused_pane() };
        let mut agents = Vec::new();
        let mut panes = Vec::new();
        let pane_addrs: HashSet<String> = m.spaces.iter().flat_map(|pr| pr.tabs.iter().flat_map(|ctx| ctx.root.panes().into_iter().filter_map(|p| p.any_terminal().map(str::to_owned)))).collect();
        let mut acp_by_space: HashMap<String, Vec<String>> = HashMap::new();
        let mut acp_unmatched: Vec<(String, String, String)> = Vec::new();
        for (addr, rec) in &self.live {
            let is_acp = rec.get("acp").and_then(Value::as_bool) == Some(true) || is_acp_addr(addr);
            let is_cli_agent = !is_acp && self.is_agent_pty(addr);
            if self.closing_terminals.contains_key(addr)
                || is_trashed_record(rec)
                || (!is_acp && !is_cli_agent)
                || rec.get("state").and_then(Value::as_str) != Some("live")
            {
                continue;
            }
            if pane_addrs.contains(addr) || has_pane_ancestor(addr, &self.live, &pane_addrs) || has_live_parent(addr, &self.live, &self.closing_terminals) {
                continue;
            }
            let machine = addr.split_once('/').map(|(mc, _)| mc);
            let cwd = self.cwds.get(addr).map(String::as_str).or_else(|| rec.get("cwd").and_then(Value::as_str));
            if let Some(sp) = m.spaces.iter().find(|pr| self.acp_matches_space(addr, rec, pr)) {
                acp_by_space.entry(sp.id.clone()).or_default().push(addr.clone());
            } else {
                let space_id = match machine {
                    Some(mc) => format!("headless:{mc}"),
                    None => "headless:local".into(),
                };
                let space_name = cwd
                    .and_then(|c| std::path::Path::new(c).file_name().and_then(|n| n.to_str()).filter(|s| !s.is_empty()))
                    .map(str::to_owned)
                    .or_else(|| machine.map(str::to_owned))
                    .unwrap_or_else(|| "ACP Agents".into());
                acp_unmatched.push((space_id, space_name, addr.clone()));
            }
        }
        let acp_meta = |addr: &str| {
            let rec = self.live.get(addr).or_else(|| self.ended_acp.get(addr));
            let a = m.agents.get(addr);
            let title = rec
                .and_then(|r| r.get("title"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| addr.to_owned());
            let pinned = rec.and_then(|r| r.pointer("/labels/herdr.pinned")).and_then(Value::as_str).is_some_and(|v| !v.is_empty());
            let custom_name = rec.and_then(|r| r.pointer("/labels/herdr.name")).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned);
            let app = a
                .and_then(|a| a.app.clone())
                .or_else(|| rec.and_then(|r| r.get("agent")).and_then(Value::as_str).map(str::to_owned));
            let cwd = self.cwds.get(addr).cloned().or_else(|| rec.and_then(|r| r.get("cwd")).and_then(Value::as_str).map(str::to_owned));
            let context_usage = rec.and_then(|r| r.get("context_usage")).cloned().unwrap_or(Value::Null);
            (rec.is_some(), a, title, pinned, custom_name, app, cwd, context_usage)
        };
        let agent_timing = |addr: &str| -> (Option<u64>, Option<u64>, u64) {
            let rec = self.live.get(addr).or_else(|| self.ended_acp.get(addr));
            let a = m.agents.get(addr);
            let is_ended = !self.live.contains_key(addr) && self.ended_acp.contains_key(addr);
            let working_since = if is_ended {
                None
            } else {
                a.and_then(|a| a.working_since)
                    .or_else(|| rec.and_then(|r| r.get("working_since")).and_then(Value::as_u64))
            };
            let mut work_ms = a
                .and_then(|a| a.work_ms)
                .or_else(|| rec.and_then(|r| r.get("work_ms")).and_then(Value::as_u64));
            if is_ended
                && work_ms.is_none()
                && rec.and_then(|r| r.get("turn")).and_then(Value::as_u64).unwrap_or(0) > 0
                && let (Some(c), Some(f)) = (
                    rec.and_then(|r| r.get("created_ms")).and_then(Value::as_u64),
                    rec.and_then(|r| r.get("finished_ms")).and_then(Value::as_u64),
                )
            {
                work_ms = Some(f.saturating_sub(c));
            }
            let total_work_ms = a
                .map(|a| a.total_work_ms)
                .unwrap_or(0)
                .max(rec.and_then(|r| r.get("total_work_ms")).and_then(Value::as_u64).unwrap_or(0))
                .max(work_ms.unwrap_or(0));
            (working_since, work_ms, total_work_ms)
        };
        let agent_appeared_ms = |addr: &str| -> u64 {
            let rec = self.live.get(addr).or_else(|| self.ended_acp.get(addr));
            rec.and_then(|r| r.get("program_started_ms"))
                .and_then(Value::as_u64)
                .filter(|&v| v > 0)
                .or_else(|| rec.and_then(|r| r.get("created_ms")).and_then(Value::as_u64))
                .unwrap_or(0)
        };
        let appearance_sort_key = |ts: u64| if ts > 0 { ts } else { u64::MAX };
        let push_acp = |agents: &mut Vec<Value>, addr: &str, space_id: &str, space_name: &str| {
            let (has_rec, a, title, pinned, custom_name, app, _, context_usage) = acp_meta(addr);
            if !has_rec { return; }
            let is_acp = is_acp_addr(addr)
                || self.live.get(addr).and_then(|r| r.get("acp")).and_then(Value::as_bool) == Some(true);
            let (working_since, work_ms, total_work_ms) = agent_timing(addr);
            let created_ms = agent_appeared_ms(addr);
            let live_rec = self.live.get(addr).or_else(|| self.ended_acp.get(addr));
            let semantic_title = live_rec.and_then(|r| r.get("semantic_title")).and_then(Value::as_str).filter(|s| !s.is_empty());
            let micro_summary = live_rec.and_then(|r| r.get("micro_summary")).and_then(Value::as_str).filter(|s| !s.is_empty());
            agents.push(json!({
                "address": addr, "acp": is_acp, "pane": Value::Null, "space": space_id, "space_name": space_name,
                "tab": Value::Null, "tab_index": 0, "tab_name": "headless",
                "status": m.status(addr).unwrap_or("idle"),
                "manual_status": a.and_then(|a| a.manual_status.clone()),
                "app": app, "program": self.program(addr), "title": title,
                "custom_name": custom_name,
                "semantic_title": semantic_title,
                "micro_summary": micro_summary,
                "context_usage": context_usage,
                "created_ms": created_ms,
                "working_since": working_since,
                "work_ms": work_ms,
                "total_work_ms": total_work_ms,
                "seq": a.map(|a| a.seq).unwrap_or(0),
                "pinned": pinned,
                "focused": self.peeked_agent.as_deref() == Some(addr),
            }));
        };
        for pr in &m.spaces {
            let sp_start = agents.len();
            for (ci, ctx) in pr.tabs.iter().enumerate() {
                for p in ctx.root.panes() {
                    let focused = m.active.as_deref() == Some(pr.id.as_str()) && focused_pane.as_deref() == Some(p.id.as_str());
                    let zoomed = ctx.zoomed.as_deref() == Some(p.id.as_str());
                    if let Some(addr) = p.acp_terminal() {
                        let (_, a, title, pinned, custom_name, app, cwd, context_usage) = acp_meta(addr);
                        let is_ended = !self.live.contains_key(addr) && self.ended_acp.contains_key(addr);
                        let status = if is_ended { "ended" } else { m.status(addr).unwrap_or("idle") };
                        let (working_since, work_ms, total_work_ms) = agent_timing(addr);
                        let live_rec = self.live.get(addr).or_else(|| self.ended_acp.get(addr));
                        let semantic_title = live_rec.and_then(|r| r.get("semantic_title")).and_then(Value::as_str).filter(|s| !s.is_empty());
                        let micro_summary = live_rec.and_then(|r| r.get("micro_summary")).and_then(Value::as_str).filter(|s| !s.is_empty());
                        panes.push(json!({
                            "pane": p.id, "acp": true, "mod": "peek", "terminal": addr, "tab": ctx.id, "title": title,
                            "custom_name": custom_name,
                            "semantic_title": semantic_title,
                            "micro_summary": micro_summary,
                            "cwd": cwd, "branch": m.folder_info.get(addr).and_then(|f| f.branch.clone()),
                            "status": status, "app": app, "program": self.program(addr),
                            "context_usage": context_usage,
                            "working_since": working_since,
                            "work_ms": work_ms,
                            "total_work_ms": total_work_ms,
                            "focused": focused, "zoomed": zoomed, "movable": self.pane_movable(&p.id),
                        }));
                        if !is_ended && !self.closing_terminals.contains_key(addr) && !self.live.get(addr).is_some_and(is_trashed_record) {
                            let created_ms = agent_appeared_ms(addr);
                            agents.push(json!({
                                "address": addr, "acp": true, "pane": p.id, "space": pr.id, "space_name": m.space_name(pr, &home),
                                "tab": ctx.id, "tab_index": ci + 1, "tab_name": Model::tab_name(pr, ctx),
                                "status": status,
                                "manual_status": a.and_then(|a| a.manual_status.clone()),
                                "app": app, "program": self.program(addr), "title": title,
                                "custom_name": custom_name,
                                "semantic_title": semantic_title,
                                "micro_summary": micro_summary,
                                "context_usage": context_usage,
                                "created_ms": created_ms,
                                "working_since": working_since,
                                "work_ms": work_ms,
                                "total_work_ms": total_work_ms,
                                "seq": a.map(|a| a.seq).unwrap_or(0),
                                "pinned": pinned,
                                "focused": focused || self.peeked_agent.as_deref() == Some(addr),
                            }));
                        }
                        continue;
                    }
                    // A mod pane: what it shows, no terminal.
                    if let Some(Content::Mod { name, .. }) = p.content() {
                        panes.push(json!({ "pane": p.id, "terminal": null, "mod": name, "tab": ctx.id, "title": name, "focused": focused, "zoomed": zoomed, "movable": self.pane_movable(&p.id) }));
                        continue;
                    }
                    let Some(addr) = p.terminal() else { continue };
                    let a = m.agents.get(addr);
                    let title = self.terms.get(&p.id).map(|t| t.view.read(cx).title.clone()).or_else(|| {
                        self.live.get(addr).and_then(|r| r.get("title")).and_then(Value::as_str).map(str::to_owned)
                    });
                    let structured = !self.swapped.contains_key(&p.id) && self.is_structured_terminal(addr);
                    let custom_name = self.live.get(addr).and_then(|r| r.pointer("/labels/herdr.name")).and_then(Value::as_str).filter(|s| !s.is_empty());
                    let semantic_title = self.live.get(addr).and_then(|r| r.get("semantic_title")).and_then(Value::as_str).filter(|s| !s.is_empty());
                    let micro_summary = self.live.get(addr).and_then(|r| r.get("micro_summary")).and_then(Value::as_str).filter(|s| !s.is_empty());
                    let (working_since, work_ms, total_work_ms) = agent_timing(addr);
                    panes.push(json!({
                        "pane": p.id, "terminal": addr, "tab": ctx.id, "title": title.clone().unwrap_or_default(),
                        "custom_name": custom_name,
                        "semantic_title": semantic_title,
                        "micro_summary": micro_summary,
                        "cwd": self.cwds.get(addr), "branch": m.folder_info.get(addr).and_then(|f| f.branch.clone()),
                        "status": m.status(addr), "app": a.and_then(|a| a.app.clone()), "program": self.program(addr),
                        "structured": structured,
                        "working_since": working_since,
                        "work_ms": work_ms,
                        "total_work_ms": total_work_ms,
                        "focused": focused, "zoomed": zoomed, "movable": self.pane_movable(&p.id),
                    }));
                    if self.closing_terminals.contains_key(addr) || self.live.get(addr).is_some_and(is_trashed_record) {
                        continue;
                    }
                    let Some(status) = m.status(addr).or_else(|| has_live_child(addr, &self.live, &self.closing_terminals).then_some("idle")) else { continue };
                    let created_ms = agent_appeared_ms(addr);
                    agents.push(json!({
                        "address": addr, "pane": p.id, "space": pr.id, "space_name": m.space_name(pr, &home),
                        "tab": ctx.id, "tab_index": ci + 1, "tab_name": Model::tab_name(pr, ctx),
                        "status": status,
                        "manual_status": a.and_then(|a| a.manual_status.clone()),
                        "app": a.and_then(|a| a.app.clone()), "program": self.program(addr), "title": title.unwrap_or_default(),
                        "custom_name": custom_name,
                        "semantic_title": semantic_title,
                        "micro_summary": micro_summary,
                        "created_ms": created_ms,
                        "working_since": working_since,
                        "work_ms": work_ms,
                        "total_work_ms": total_work_ms,
                        "seq": a.map(|a| a.seq).unwrap_or(0),
                        "pinned": self.live.get(addr).and_then(|r| r.pointer("/labels/herdr.pinned")).and_then(Value::as_str).is_some_and(|v| !v.is_empty()),
                        "focused": m.active.as_deref() == Some(pr.id.as_str()) && focused_pane.as_deref() == Some(p.id.as_str()),
                    }));
                }
            }
            if let Some(mut matched) = acp_by_space.remove(&pr.id) {
                matched.sort_by(|a, b| {
                    appearance_sort_key(agent_appeared_ms(a))
                        .cmp(&appearance_sort_key(agent_appeared_ms(b)))
                        .then_with(|| a.cmp(b))
                });
                let sp_name = m.space_name(pr, &home);
                for addr in matched {
                    push_acp(&mut agents, &addr, &pr.id, &sp_name);
                }
            }
            agents[sp_start..].sort_by_key(|v| appearance_sort_key(v.get("created_ms").and_then(Value::as_u64).unwrap_or(0)));
        }
        acp_unmatched.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| appearance_sort_key(agent_appeared_ms(&a.2)).cmp(&appearance_sort_key(agent_appeared_ms(&b.2))))
                .then_with(|| a.2.cmp(&b.2))
        });
        for (space_id, space_name, addr) in acp_unmatched {
            push_acp(&mut agents, &addr, &space_id, &space_name);
        }
        let focused = m.active_space().and_then(|p| {
            let ctx = p.active_tab()?;
            if let Some(block) = block {
                let address = block.terminal()?;
                return Some(json!({ "space": p.id, "tab": ctx.id, "pane": null, "terminal": address,
                    "shown": null, "agent": self.shown_agent(), "cwd": self.cwds.get(address) }));
            }
            let pane = ctx.focused_pane()?;
            let cwd = (if self.shows_agent() { self.shown_agent() } else { m.any_address_of(&pane) }).and_then(|a| self.cwds.get(&a).cloned());
            Some(json!({ "space": p.id, "tab": ctx.id, "terminal": m.any_address_of(&pane), "shown": self.swapped.get(&pane),
                "pane": pane, "agent": self.shown_agent(), "cwd": cwd }))
        });
        let machines = self.documents.iter().map(|(name, slot)| json!({
            "name": (!name.is_empty()).then_some(name), "layout": slot.layout(),
        })).collect();
        let mut changed = mods::publish_machines(Value::Array(machines));
        self.sync_trashed_agents();
        changed |= mods::publish_spaces(Value::Array(spaces));
        changed |= mods::publish_agents(Value::Array(agents));
        changed |= mods::publish_panes(Value::Array(panes));
        changed |= mods::publish_focused(focused.unwrap_or(Value::Null));
        changed |= mods::publish_floats(self.float_placements());
        changed |= mods::publish_layout(&self.chrome);
        if changed {
            self.refresh_snapshot_mods(cx);
        }
        self.write_where(cx);
    }

    /// Tells the core where each terminal lives, in words: the `where` label,
    /// `<space> / <tab>`, which hc shows in its picker and `ls`.
    /// Sub-agents get their parent's. The ids (`herdr.space`,
    /// `herdr.tab`) are what the app groups by; only this follows renames
    /// and moves. Written only when it changes.
    fn write_where(&mut self, cx: &mut Context<Self>) {
        let m = &self.model;
        let home = home();
        let mut want: HashMap<String, String> = HashMap::new();
        for p in &m.spaces {
            let name = m.space_name(p, &home);
            for c in &p.tabs {
                for a in model::any_addresses(&c.root) {
                    want.insert(a, format!("{name} / {}", Model::tab_name(p, c)));
                }
            }
        }
        for addr in self.live.keys() {
            if want.contains_key(addr) {
                continue;
            }
            let mut cur = addr.clone();
            for _ in 0..32 {
                let Some(p) = self.live.get(&cur).and_then(|r| r.get("parent")).and_then(Value::as_str) else { break };
                if let Some(place) = want.get(p).cloned() {
                    want.insert(addr.clone(), place);
                    break;
                }
                cur = p.to_owned();
            }
        }
        self.written_where.retain(|a, _| self.live.contains_key(a));
        for (addr, place) in want {
            if self.written_where.get(&addr) == Some(&place) || !self.live.contains_key(&addr) {
                continue;
            }
            // Off the UI thread (a terminal on a slow machine answers late); a
            // failed write is forgotten so the next publish tries again.
            self.written_where.insert(addr.clone(), place.clone());
            let core = self.core.clone();
            cx.spawn(async move |this, cx| {
                let (method, params) = if is_acp_addr(&addr) {
                    ("agent.label", json!({ "agent": addr, "labels": { "where": place } }))
                } else {
                    ("terminal.label", json!({ "terminal": addr, "labels": { "where": place } }))
                };
                let Err(e) = cx.background_executor().spawn(async move { core.call(method, params) }).await else { return };
                tracing::warn!("where label for {addr}: {e:#}");
                let _ = this.update(cx, |this, _| {
                    if this.written_where.get(&addr) == Some(&place) {
                        this.written_where.remove(&addr);
                    }
                });
            })
            .detach();
        }
    }

    fn flush_documents(&mut self, refresh: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.draining || !self.documents_watching { return; }
        let generation = self.core_generation;
        for (machine, slot) in &mut self.documents {
            slot.dirty |= refresh;
            if !slot.ready() { continue; }
            let doc = crate::mirror::Document::capture(&self.model, (!machine.is_empty()).then_some(machine.as_str()));
            let write = (self.drag.is_none() && slot.available && slot.document.as_ref() != Some(&doc)).then_some(doc);
            if write.is_none() && slot.available && !slot.dirty { continue; }
            let (tx, rx) = std::sync::mpsc::channel();
            slot.pending = Some(rx);
            slot.dirty = false;
            let work = crate::mirror_sync::Work { machine: machine.clone(), version: slot.version, write };
            let core = self.core.clone();
            // Quit can block the foreground before the continuation gets a turn.
            let job = cx.background_executor().spawn(async move {
                let (machine, outcome) = crate::mirror_sync::round(&core, work);
                let _ = tx.send(outcome);
                machine
            });
            cx.spawn_in(window, async move |this, cx| {
                let machine = job.await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if generation != this.core_generation || this.draining { return; }
                    let result = this.documents.get_mut(&machine).and_then(|s| s.pending.take()).and_then(|rx| rx.try_recv().ok());
                    if let Some(result) = result
                        && this.documents_updated(None, vec![(machine, result)], window, cx) { this.sync(window, cx); }
                });
            }).detach();
        }
    }

    fn layout_notice(&mut self, title: &str, body: &str, window: &mut Window, cx: &mut Context<Self>) {
        let spec = json!({"kind":"toast", "notification":{"title":title,"body":body,"level":"info"}});
        self.open_overlay(mods::new_overlay_id(), spec, window, cx);
    }

    /// Finish the accepted edit and any newer projection before the window goes.
    pub fn drain_documents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.draining { return; }
        self.save();
        self.draining = true;
        QUITTING.store(true, std::sync::atomic::Ordering::SeqCst);
        while let Some(address) = self.pending_terminals.keys().next().cloned() {
            let pending = self.pending_terminals.remove(&address).unwrap();
            let timeout = Duration::from_secs(if address.contains('/') { 2 } else { 10 });
            let result = pending.reply.recv_timeout(timeout).unwrap_or_else(|e| Err(format!("terminal change did not finish before quit: {e}")));
            self.apply_terminal_change(address, pending.change, result, window, cx);
        }
        let machines = self.documents.keys().cloned().collect::<Vec<_>>();
        for machine in machines {
            let timeout = Duration::from_secs(if machine.is_empty() { 10 } else { 2 });
            let deadline = Instant::now() + timeout;
            let pending = self.documents.get_mut(&machine).and_then(|s| s.pending.take());
            if let Some(pending) = pending {
                match pending.recv_timeout(timeout) {
                    Ok(result) => { self.documents_updated(None, vec![(machine.clone(), result)], window, cx); }
                    Err(e) => {
                        tracing::error!("layout on {machine:?} could not finish saving before quit: {e}");
                        continue;
                    }
                }
            }
            let slot = &self.documents[&machine];
            let doc = crate::mirror::Document::capture(&self.model, (!machine.is_empty()).then_some(machine.as_str()));
            if slot.document.as_ref() == Some(&doc) { continue; }
            if !slot.available {
                tracing::error!("layout on {machine:?} could not save before quit: {}", slot.error.as_deref().unwrap_or("machine unavailable"));
                continue;
            }
            let work = crate::mirror_sync::Work { machine: machine.clone(), version: slot.version, write: Some(doc) };
            let result = crate::mirror_sync::round_timeout(&self.core, work, deadline.saturating_duration_since(Instant::now()));
            self.documents_updated(None, vec![result], window, cx);
        }
        self.save_view();
    }

    fn documents_updated(&mut self, machines: Option<Value>, results: Vec<(String, crate::mirror_sync::Outcome)>, window: &mut Window, cx: &mut Context<Self>) -> bool {
        use crate::mirror_sync::Outcome;
        let mut changed = false;
        // The listing every 500 ms brings the machines alone: redraw only when
        // one's state moved, or the whole window draws twice a second for nothing.
        let shown = |d: &std::collections::BTreeMap<String, crate::mirror_sync::Slot>| {
            d.iter().map(|(m, s)| (m.clone(), s.connected, s.available, s.blocked, s.error.clone(), s.layout())).collect::<Vec<_>>()
        };
        let (listing, was) = (results.is_empty(), shown(&self.documents));
        for (machine, result) in results {
            let Some(slot) = self.documents.get_mut(&machine) else { continue };
            let first = slot.document.is_none();
            slot.read_attempted = true;
            let (document, version, replace, conflict) = match result {
                Outcome::Read(d, v) => {
                    let replace = !slot.available || slot.version != v || slot.document.is_none();
                    let projection = crate::mirror::Document::capture(&self.model, (!machine.is_empty()).then_some(machine.as_str()));
                    let unsent = slot.document.as_ref().is_some_and(|old| old != &projection);
                    let conflict = replace && unsent && projection != d;
                    (d, v, replace, conflict)
                }
                Outcome::Written(d, v) => (d, v, false, false),
                Outcome::Conflict(d, v) => (d, v, true, true),
                error @ (Outcome::Unavailable(_) | Outcome::Undecodable(_) | Outcome::Blocked(_)) => {
                    let blocked = !matches!(&error, Outcome::Unavailable(_));
                    let error = match error {
                        Outcome::Unavailable(e) | Outcome::Undecodable(e) | Outcome::Blocked(e) => e,
                        _ => unreachable!(),
                    };
                    let notify = slot.connected && blocked && slot.notified_error.as_ref() != Some(&error);
                    slot.available = false;
                    slot.blocked = blocked;
                    if slot.connected || slot.error.is_none() { slot.error = Some(error.clone()); }
                    if notify {
                        tracing::warn!("layout on {machine:?} unavailable: {error}");
                        slot.notified_error = Some(error.clone());
                        self.layout_notice("Layout unavailable", &format!("{}: {error}", if machine.is_empty() { "This machine" } else { &machine }), window, cx);
                    }
                    continue;
                }
            };
            slot.version = version;
            slot.available = slot.connected;
            slot.blocked = false;
            if slot.connected { slot.error = None; slot.notified_error = None; }
            slot.document = Some(document.clone());
            if replace {
                let on = (!machine.is_empty()).then_some(machine.as_str());
                document.replace(&mut self.model, on);
                if first && let Some(view) = &self.initial_view { view.apply_machine(&mut self.model, on); }
                changed = true;
            }
            if conflict {
                self.layout_notice("Layout changed elsewhere", "The other screen's layout is shown.", window, cx);
            }
        }
        if let Some(machines) = machines {
            // Temporary machines are agents' short-lived sandboxes: the app leaves them out.
            let kept = || machines["machines"].as_array().into_iter().flatten().filter(|m| m["temporary"] != true);
            let states = kept().filter_map(|m| Some((m["name"].as_str()?, m["state"].as_str()?))).collect::<HashMap<_, _>>();
            for machine in kept() {
                let Some(name) = machine["name"].as_str() else { continue };
                let state = machine["state"].as_str().unwrap_or("unavailable");
                let reason = (state != "connected").then(|| machine["why"].as_str().map(str::to_owned).unwrap_or_else(|| match state {
                    "needs_update" => format!("{name} needs a core update; run h2 machine update {name}"),
                    "needs_login" => format!("{name} needs login; run h2 machine login {name}"),
                    _ => format!("{name} is {state}"),
                }));
                self.documents.entry(name.to_owned()).or_default().connection(state == "connected", machine["since_ms"].as_u64(), reason);
            }
            if self.model.spaces.iter().any(|s| s.machine.as_deref().is_some_and(|m| !states.contains_key(m))) {
                let mut view = crate::mirror::View::capture(&self.model);
                if let Some(saved) = &self.initial_view {
                    view.preserve_unloaded(saved, |m| self.documents.get(m.unwrap_or("")).is_some_and(|s| s.document.is_some()));
                }
                self.initial_view = Some(view);
            }
            self.documents.retain(|m, _| m.is_empty() || states.contains_key(m.as_str()));
            let before = self.model.spaces.len();
            self.model.spaces.retain(|s| s.machine.as_deref().is_none_or(|m| states.contains_key(m)));
            changed |= before != self.model.spaces.len();
        }
        if !listing || changed || shown(&self.documents) != was {
            self.publish(cx);
            cx.notify();
        }
        changed
    }

    // ---- core facts --------------------------------------------------------

    /// What a terminal view takes from the core's facts: its folder, where right-clicks go.
    fn tell_term(&self, view: &Entity<TerminalView>, addr: &str, cx: &mut Context<Self>) {
        let cwd = self.cwds.get(addr).map(std::path::PathBuf::from);
        let to_program = self.live.get(addr).and_then(|r| r.pointer("/labels/herdr.right-click")).and_then(Value::as_str) == Some("program");
        view.update(cx, |v, _| {
            v.cwd = cwd;
            v.right_click_to_program = to_program;
        });
    }

    fn ordered_live_values(&self) -> Value {
        Value::Array(
            self.live_order
                .iter()
                .filter(|a| !self.closing_terminals.contains_key(*a))
                .filter_map(|a| self.live.get(a).cloned())
                .collect(),
        )
    }

    /// Folders associated with `pr` across its explicit root and its open panes.
    fn space_folders(&self, pr: &model::Space) -> Vec<String> {
        let mut folders = Vec::new();
        let mut push = |f: &str| {
            if !f.is_empty() && !folders.iter().any(|x| x == f) {
                folders.push(f.to_owned());
            }
        };
        push(&pr.folder);
        for c in &pr.tabs {
            for n in c.root.panes() {
                if let Some(f) = self.model.pane_folders.get(&n.id) {
                    push(f);
                }
                if let Some(addr) = n.any_terminal() {
                    if let Some(f) = self.model.folder_info.get(addr) {
                        push(&f.top);
                    }
                    if let Some(f) = self.cwds.get(addr) {
                        push(f);
                    }
                }
            }
        }
        folders
    }

    fn space_matches_worktree(&self, pr: &model::Space, wt_path: &str, machine: Option<&str>) -> bool {
        if pr.machine.as_deref() != machine {
            return false;
        }
        if same_path(&pr.folder, wt_path) {
            return true;
        }
        if pr.folder.is_empty()
            && let Some(first) = pr.tabs.iter().flat_map(|c| c.root.panes()).next()
        {
            if self.model.pane_folders.get(&first.id).is_some_and(|f| same_path(f, wt_path)) {
                return true;
            }
            if let Some(addr) = first.any_terminal()
                && (self.model.folder_info.get(addr).is_some_and(|f| same_path(&f.top, wt_path))
                    || self.cwds.get(addr).is_some_and(|f| same_path(f, wt_path)))
            {
                return true;
            }
        }
        false
    }

    fn acp_matches_space(&self, addr: &str, rec: &Value, pr: &model::Space) -> bool {
        let machine = addr.split_once('/').map(|(mc, _)| mc);
        if pr.machine.as_deref() != machine {
            return false;
        }
        if let Some(wt_path) = rec
            .pointer("/lineage/worktree_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if self.space_matches_worktree(pr, wt_path, machine) {
                return true;
            }
            if self.model.spaces.iter().any(|s| self.space_matches_worktree(s, wt_path, machine)) {
                return false;
            }
        }
        if rec.pointer("/labels/herdr.space").and_then(Value::as_str) == Some(crate::mirror::local(&pr.id, machine)) {
            return true;
        }
        if let Some(parent_local) = rec.get("parent").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            let parent_addr = crate::mirror::qualify(parent_local, machine);
            if self.model.any_pane_of(&parent_addr).and_then(|p| self.model.space_of(&p)).is_some_and(|s| s.id == pr.id) {
                return true;
            }
        }
        let local_home = home();
        let cwd = self.cwds.get(addr).map(String::as_str).or_else(|| rec.get("cwd").and_then(Value::as_str));
        let wt_origin = rec.pointer("/lineage/worktree_origin").and_then(Value::as_str).filter(|s| !s.is_empty());
        let top = self.model.folder_info.get(addr).map(|f| f.top.as_str());
        self.space_folders(pr).iter().any(|root| {
            let allow_descendants = !(machine.is_none() && path_within_or_eq(root, &local_home) && path_within_or_eq(&local_home, root));
            [cwd, wt_origin, top].into_iter().flatten().any(|cand| path_matches_root(cand, root, allow_descendants))
        })
    }

    fn space_acp_agents(&self, space_id: &str) -> Vec<String> {
        let Some(closing_sp) = self.model.space(space_id) else { return Vec::new() };
        self.live
            .iter()
            .filter(|(addr, rec)| {
                !self.closing_terminals.contains_key(*addr)
                    && rec.get("acp").and_then(Value::as_bool) == Some(true)
                    && rec.get("state").and_then(Value::as_str) == Some("live")
                    && self.acp_matches_space(addr, rec, closing_sp)
                    && !self.model.spaces.iter().any(|other| other.id != space_id && self.acp_matches_space(addr, rec, other))
            })
            .map(|(addr, _)| addr.clone())
            .collect()
    }

    fn write_core_label(core: Arc<Core>, addr: String, key: &'static str, value: String, cx: &App) {
        let labels = json!({ key: value });
        cx.background_executor()
            .spawn(async move {
                let (method, params) = if is_acp_addr(&addr) {
                    ("agent.label", json!({ "agent": addr, "labels": labels }))
                } else {
                    ("terminal.label", json!({ "terminal": addr, "labels": labels }))
                };
                if let Err(e) = core.call(method, params) {
                    tracing::warn!("{key} label: {e:#}");
                }
            })
            .detach();
    }

    /// Resolves and auto-releases any manual status override (`herdr.status`) for `addr`.
    fn resolve_manual_status(&mut self, addr: &str, cx: &App) -> Option<String> {
        let rec = self.live.get(addr)?;
        let turn = rec.get("turn").and_then(Value::as_u64).unwrap_or(0);
        let reported = rec.get("reported").and_then(Value::as_bool) == Some(true);
        let rep_state = rec.pointer("/report/state").and_then(Value::as_str).map(str::to_owned);
        let label_raw = rec.pointer("/labels/herdr.status").and_then(Value::as_str).unwrap_or_default().to_owned();
        let now = Instant::now();

        if !self.manual_statuses.contains_key(addr) && !label_raw.is_empty()
            && let Some((word, parsed_turn)) = parse_manual_status_label(&label_raw)
        {
            self.manual_statuses.insert(addr.to_owned(), ManualStatus {
                status: Some(word),
                turn: parsed_turn.unwrap_or(turn),
                reported_state: rep_state.clone(),
                set_at: now,
                label_synced: true,
            });
        } else if let Some(entry) = self.manual_statuses.get_mut(addr) {
            if !label_raw.is_empty() {
                if let Some((word, parsed_turn)) = parse_manual_status_label(&label_raw) {
                    if entry.status.as_deref() == Some(word.as_str()) {
                        entry.label_synced = true;
                    } else if now.saturating_duration_since(entry.set_at) > Duration::from_millis(800) {
                        entry.status = Some(word);
                        entry.turn = parsed_turn.unwrap_or(turn);
                        entry.reported_state = rep_state.clone();
                        entry.set_at = now;
                        entry.label_synced = true;
                    }
                }
            } else if entry.status.is_some()
                && (entry.label_synced || now.saturating_duration_since(entry.set_at) > Duration::from_millis(800))
            {
                // Core cleared `herdr.status` (user interaction or inter-agent communication).
                self.manual_statuses.remove(addr);
                return None;
            }
        }

        let Some((st_opt, entry_turn, entry_rep, entry_set_at)) = self
            .manual_statuses
            .get(addr)
            .map(|e| (e.status.clone(), e.turn, e.reported_state.clone(), e.set_at))
        else {
            return None;
        };

        let Some(st) = st_opt else {
            if label_raw.is_empty() {
                self.manual_statuses.remove(addr);
            }
            return None;
        };

        let mut release = turn > entry_turn
            || (reported && entry_rep.is_some() && rep_state.is_some() && rep_state != entry_rep);
        if !release && !is_acp_addr(addr)
            && let Some(view) = self.best_terminal_view(addr, cx)
        {
            let v = view.read(cx);
            if v.last_typed_at().is_some_and(|t| t > entry_set_at + Duration::from_millis(50)) {
                release = true;
            }
        }

        if release {
            if let Some(entry) = self.manual_statuses.get_mut(addr) {
                entry.status = None;
            }
            if !label_raw.is_empty() {
                Self::write_core_label(self.core.clone(), addr.to_owned(), "herdr.status", String::new(), cx);
            }
            return None;
        }
        Some(st)
    }

    /// Infer execution state on live non-7501 PTY terminals running an interactive coding CLI agent.
    /// Deterministic rules always run synchronously at 0ms; when quiet + ambiguous (`idle`), dispatches
    /// a debounced async fallback to Core's `ai.classify_terminal_state` RPC.
    fn refresh_inferred_statuses(&mut self, cx: &mut Context<Self>) -> bool {
        let now = Instant::now();
        struct ArbitrationJob {
            addr: String,
            program: Option<String>,
            raw_title: String,
            screen_hash: u64,
            output_at: Option<Instant>,
            typed_at: Option<Instant>,
            tail_lines: Vec<String>,
            next_rechecks: u8,
        }
        let mut updates: Vec<(String, Value, Value, Value)> = Vec::new();
        let mut clear_verdicts: Vec<String> = Vec::new();
        let mut jobs: Vec<ArbitrationJob> = Vec::new();
        let can_ai = self.core.supports_ai() && self.ai_unavailable_until.is_none_or(|until| now >= until);

        for (addr, rec) in &self.live {
            let manually_held = match self.manual_statuses.get(addr) {
                Some(entry) => entry.status.is_some(),
                None => rec.pointer("/labels/herdr.status").and_then(Value::as_str).is_some_and(|s| !s.is_empty()),
            };
            if rec.get("acp").and_then(Value::as_bool) == Some(true)
                || rec.get("reported").and_then(Value::as_bool) == Some(true)
                || manually_held
            {
                if self.ai_terminal_states.contains_key(addr) {
                    clear_verdicts.push(addr.clone());
                }
                continue;
            }
            let is_agent = rec.pointer("/program/agent").and_then(Value::as_bool) == Some(true);
            let (want, want_summary, want_kind) = if is_agent {
                let raw_title = rec.get("raw_title").and_then(Value::as_str).unwrap_or_default();
                let view = self.best_terminal_view(addr, cx);
                let det = view.as_ref().map(|v| v.read(cx).inferred_agent_state(raw_title, now)).unwrap_or("idle");
                if det != "idle" {
                    if self.ai_terminal_states.contains_key(addr) {
                        clear_verdicts.push(addr.clone());
                    }
                    (Value::String(det.to_owned()), Value::Null, Value::Null)
                } else if let Some(view) = view {
                    let v = view.read(cx);
                    let output_at = v.last_output_at();
                    let typed_at = v.last_typed_at();
                    let cached = self.ai_terminal_states.get(addr).filter(|c| c.output_at == output_at && c.typed_at == typed_at);
                    if cached.is_none() && self.ai_terminal_states.contains_key(addr) {
                        clear_verdicts.push(addr.clone());
                    }
                    let (eff_state, eff_summary, eff_kind, eff_sem_title, eff_micro, need_recheck, next_rechecks) = match cached {
                        Some(c) => {
                            let recheck = c.state == "working"
                                && c.working_rechecks < 2
                                && now.saturating_duration_since(c.decided_at) >= Duration::from_secs(15);
                            let sum = if c.summary.is_empty() { Value::Null } else { Value::String(c.summary.clone()) };
                            let kind = if c.kind.is_empty() { Value::Null } else { Value::String(c.kind.clone()) };
                            let st = (!c.semantic_title.is_empty()).then(|| c.semantic_title.clone());
                            let ms = (!c.micro_summary.is_empty()).then(|| c.micro_summary.clone());
                            (c.state, sum, kind, st, ms, recheck, c.working_rechecks.saturating_add(1))
                        }
                        None => ("idle", Value::Null, Value::Null, None, None, true, 0),
                    };
                    let _ = (eff_sem_title, eff_micro);
                    if need_recheck
                        && can_ai
                        && !self.ai_inflight.contains(addr)
                        && self.ai_inflight.len() + jobs.len() < 2
                        && self.ai_last_attempt.get(addr).is_none_or(|t| now.saturating_duration_since(*t) >= Duration::from_secs(5))
                        && output_at.is_some_and(|t| now.saturating_duration_since(t) >= Duration::from_secs(2))
                        && typed_at.is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_millis(1500))
                    {
                        let (screen_hash, tail_lines) = v.tail_snapshot_for_arbitration(15);
                        if !tail_lines.is_empty()
                            && cached.is_none_or(|c| c.screen_hash != screen_hash || c.state == "working")
                        {
                            jobs.push(ArbitrationJob {
                                addr: addr.clone(),
                                program: rec.pointer("/program/key").and_then(Value::as_str).map(str::to_owned),
                                raw_title: raw_title.to_owned(),
                                screen_hash,
                                output_at,
                                typed_at,
                                tail_lines,
                                next_rechecks,
                            });
                        }
                    }
                    (Value::String(eff_state.to_owned()), eff_summary, eff_kind)
                } else {
                    (Value::String("idle".into()), Value::Null, Value::Null)
                }
            } else {
                if self.ai_terminal_states.contains_key(addr) {
                    clear_verdicts.push(addr.clone());
                }
                (Value::Null, Value::Null, Value::Null)
            };
            let cur_status = rec.get("status").unwrap_or(&Value::Null);
            let cur_summary = rec.get("ai_status_summary").unwrap_or(&Value::Null);
            let cur_kind = rec.get("ai_status_kind").unwrap_or(&Value::Null);
            if cur_status != &want || cur_summary != &want_summary || cur_kind != &want_kind {
                updates.push((addr.clone(), want, want_summary, want_kind));
            }
        }
        for addr in clear_verdicts {
            self.ai_terminal_states.remove(&addr);
        }
        let changed = !updates.is_empty();
        for (addr, want, want_summary, want_kind) in updates {
            if let Some(obj) = self.live.get_mut(&addr).and_then(Value::as_object_mut) {
                obj.insert("status".into(), want);
                obj.insert("ai_status_summary".into(), want_summary);
                obj.insert("ai_status_kind".into(), want_kind);
            }
        }
        for job in jobs {
            self.ai_inflight.insert(job.addr.clone());
            self.ai_last_attempt.insert(job.addr.clone(), now);
            let core = self.core.clone();
            let language = self.person.language.clone();
            let params = json!({
                "program": job.program,
                "rawTitle": job.raw_title,
                "tailLines": job.tail_lines,
                "language": language,
            });
            let addr = job.addr;
            let screen_hash = job.screen_hash;
            let output_at = job.output_at;
            let typed_at = job.typed_at;
            let next_rechecks = job.next_rechecks;
            cx.spawn(async move |this, cx| {
                let res = cx.background_executor().spawn(async move { core.classify_terminal_state(params) }).await;
                let _ = this.update(cx, |this, cx| {
                    this.ai_inflight.remove(&addr);
                    let Ok(val) = res else {
                        return;
                    };
                    if val.get("usedAi").and_then(Value::as_bool) != Some(true) {
                        this.ai_unavailable_until = Some(Instant::now() + Duration::from_secs(30));
                        return;
                    }
                    // Guard against stale results: discard if terminal received new output or user input.
                    let Some(view) = this.best_terminal_view(&addr, cx) else {
                        return;
                    };
                    let v = view.read(cx);
                    if v.last_output_at() != output_at || v.last_typed_at() != typed_at {
                        return;
                    }
                    let Some(rec) = this.live.get(&addr) else {
                        return;
                    };
                    if rec.get("reported").and_then(Value::as_bool) == Some(true)
                        || rec.get("manual_status").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                        || rec.pointer("/program/agent").and_then(Value::as_bool) != Some(true)
                    {
                        return;
                    }
                    let accepted = val.get("accepted").and_then(Value::as_bool) == Some(true);
                    let semantic_title = val.get("semanticTitle").and_then(Value::as_str).unwrap_or("").trim().to_owned();
                    let micro_summary = val.get("microSummary").and_then(Value::as_str).unwrap_or("").trim().to_owned();
                    let (mapped_state, kind, summary) = if accepted {
                        let raw_state = val.get("state").and_then(Value::as_str).unwrap_or("idle");
                        let mapped = match raw_state {
                            "working" => "working",
                            "blocked" | "error" => "blocked",
                            _ => "idle",
                        };
                        let k = val.get("kind").and_then(Value::as_str).unwrap_or(raw_state).to_owned();
                        let s = val.get("summary").and_then(Value::as_str).unwrap_or("").trim().to_owned();
                        (mapped, k, s)
                    } else {
                        ("idle", String::new(), String::new())
                    };
                    let verdict = AiTerminalVerdict {
                        screen_hash,
                        output_at,
                        typed_at,
                        state: mapped_state,
                        kind: kind.clone(),
                        summary: summary.clone(),
                        semantic_title: semantic_title.clone(),
                        micro_summary: micro_summary.clone(),
                        decided_at: Instant::now(),
                        working_rechecks: next_rechecks,
                    };
                    this.ai_terminal_states.insert(addr.clone(), verdict);
                    if let Some(obj) = this.live.get_mut(&addr).and_then(Value::as_object_mut) {
                        obj.insert("status".into(), Value::String(mapped_state.to_owned()));
                        obj.insert(
                            "ai_status_kind".into(),
                            if kind.is_empty() { Value::Null } else { Value::String(kind) },
                        );
                        obj.insert(
                            "ai_status_summary".into(),
                            if summary.is_empty() { Value::Null } else { Value::String(summary) },
                        );
                        if !semantic_title.is_empty() {
                            obj.insert("semantic_title".into(), Value::String(semantic_title));
                        }
                        if !micro_summary.is_empty() {
                            obj.insert("micro_summary".into(), Value::String(micro_summary));
                        }
                    }
                    let app = this
                        .live
                        .get(&addr)
                        .and_then(|r| r.pointer("/program/key").and_then(Value::as_str))
                        .map(str::to_owned);
                    let looking = this.visible_addresses().iter().any(|x| x == &addr) && this.window_active;
                    this.model.set_word(&addr, Some(mapped_state.to_owned()), app, looking);
                    mods::publish_terminals(this.ordered_live_values());
                    this.publish(cx);
                    this.refresh_snapshot_mods(cx);
                    cx.notify();
                });
            })
            .detach();
        }
        changed
    }

    /// Lazily backfills and continuously refreshes `semantic_title` / `micro_summary` via Core's
    /// `ai.summarize_agent` for live ACP and PTY CLI agent sessions as conversation content progresses.
    fn refresh_missing_semantic_digests(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        if !self.core.supports_ai() {
            return;
        }
        let include_ai = self.ai_unavailable_until.is_none_or(|until| now >= until);
        let mut jobs: Vec<(String, DigestWatermark, Option<String>)> = Vec::new();
        for addr in &self.live_order {
            if self.ai_digest_inflight.len() + jobs.len() >= 3 {
                break;
            }
            if self.closing_terminals.contains_key(addr) || self.ai_digest_inflight.contains(addr) {
                continue;
            }
            let Some(rec) = self.live.get(addr) else {
                continue;
            };
            if rec.pointer("/labels/herdr.trashed").and_then(Value::as_str).is_some_and(|s| !s.is_empty()) {
                continue;
            }
            let is_acp = is_acp_addr(addr) || rec.get("acp").and_then(Value::as_bool) == Some(true);
            let is_shell = !is_acp
                && rec.pointer("/program/shell").and_then(Value::as_bool) == Some(true)
                && rec.pointer("/program/agent").and_then(Value::as_bool) != Some(true);
            let is_agent = !is_shell
                && (is_acp
                    || rec.pointer("/program/agent").and_then(Value::as_bool) == Some(true)
                    || rec.get("reported").and_then(Value::as_bool) == Some(true));
            if !is_agent {
                continue;
            }
            let has_title = rec
                .get("semantic_title")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty());
            let has_summary = rec
                .get("micro_summary")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty());
            let missing = !has_title || !has_summary;
            let turn = rec.get("turn").and_then(Value::as_u64).unwrap_or(0);
            let transcript_seq = rec.get("transcript_seq").and_then(Value::as_u64).unwrap_or(0);
            let status = rec.get("status").and_then(Value::as_str).map(str::to_owned);
            let (output_at, typed_at) = if is_acp {
                (None, None)
            } else if let Some(view) = self.best_terminal_view(addr, cx) {
                let v = view.read(cx);
                (v.last_output_at(), v.last_typed_at())
            } else {
                (None, None)
            };

            let should_refresh = match self.ai_digest_attempted.get(addr) {
                None => missing || (!is_acp && (output_at.is_some() || typed_at.is_some() || turn > 0)),
                Some(prev) => {
                    let since_last = now.saturating_duration_since(prev.attempted_at);
                    let turn_changed = prev.turn != turn;
                    let seq_changed = prev.transcript_seq != transcript_seq;
                    let status_changed = prev.status != status;
                    let activity_changed = prev.output_at != output_at || prev.typed_at != typed_at;
                    let output_quiet = output_at.is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_millis(1200));

                    if missing {
                        if turn_changed || seq_changed || status_changed || activity_changed {
                            since_last >= Duration::from_secs(3)
                                && (is_acp || output_quiet || since_last >= Duration::from_secs(6))
                        } else {
                            since_last >= Duration::from_secs(30)
                        }
                    } else if is_acp {
                        // Core pushes deterministic + AI digest events on ACP turns; app only refreshes
                        // as a safety net when transcript/turn/status moved and enough time passed.
                        (turn_changed || seq_changed || status_changed) && since_last >= Duration::from_secs(12)
                    } else if status_changed || turn_changed {
                        since_last >= Duration::from_secs(3)
                    } else if activity_changed {
                        (output_quiet && since_last >= Duration::from_secs(5))
                            || since_last >= Duration::from_secs(12)
                    } else if status.as_deref() == Some("working") && output_at.is_none() {
                        // Background/unattached PTY CLI with no local view timestamps.
                        since_last >= Duration::from_secs(15)
                    } else {
                        false
                    }
                }
            };
            if !should_refresh {
                continue;
            }
            let wm = DigestWatermark {
                turn,
                transcript_seq,
                status: status.clone(),
                output_at,
                typed_at,
                attempted_at: now,
            };
            jobs.push((addr.clone(), wm, status));
        }

        for (addr, wm, status) in jobs {
            self.ai_digest_inflight.insert(addr.clone());
            self.ai_digest_attempted.insert(addr.clone(), wm);
            let core = self.core.clone();
            let language = self.person.language.clone();
            let mut params = json!({
                "target": addr,
                "language": language,
                "includeAi": include_ai,
            });
            if let Some(st) = status {
                params["status"] = Value::String(st);
            }
            cx.spawn(async move |this, cx| {
                let res = cx.background_executor().spawn(async move { core.summarize_agent(params) }).await;
                let _ = this.update(cx, |this, cx| {
                    this.ai_digest_inflight.remove(&addr);
                    let Ok(val) = res else {
                        return;
                    };
                    let semantic_title = val
                        .get("semanticTitle")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);
                    let micro_summary = val
                        .get("microSummary")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);
                    if semantic_title.is_none() && micro_summary.is_none() {
                        return;
                    }
                    let mut updated = false;
                    if let Some(obj) = this.live.get_mut(&addr).and_then(Value::as_object_mut) {
                        let is_shell = obj.get("program").and_then(|p| p.get("shell")).and_then(Value::as_bool) == Some(true)
                            && obj.get("program").and_then(|p| p.get("agent")).and_then(Value::as_bool) != Some(true);
                        let still_agent = !is_shell
                            && (obj.get("acp").and_then(Value::as_bool) == Some(true)
                                || obj.get("program").and_then(|p| p.get("agent")).and_then(Value::as_bool) == Some(true)
                                || obj.get("reported").and_then(Value::as_bool) == Some(true));
                        if !still_agent {
                            return;
                        }
                        if let Some(st) = semantic_title {
                            if obj.get("semantic_title").and_then(Value::as_str) != Some(st.as_str()) {
                                obj.insert("semantic_title".into(), Value::String(st));
                                updated = true;
                            }
                        }
                        if let Some(ms) = micro_summary {
                            if obj.get("micro_summary").and_then(Value::as_str) != Some(ms.as_str()) {
                                obj.insert("micro_summary".into(), Value::String(ms));
                                updated = true;
                            }
                        }
                    }
                    if updated {
                        mods::publish_terminals(this.ordered_live_values());
                        this.publish(cx);
                        this.refresh_snapshot_mods(cx);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    fn best_terminal_view(&self, address: &str, cx: &App) -> Option<Entity<TerminalView>> {
        let own_pane = self.model.pane_of(address);
        self.terms
            .iter()
            .filter(|(_, t)| t.address == address)
            .max_by_key(|(pane, t)| {
                let view = t.view.read(cx);
                (view.on_screen_this_frame(), own_pane.as_deref() == Some(pane.as_str()))
            })
            .map(|(_, t)| t.view.clone())
    }

    fn terminals_listed(&mut self, v: Value, window: &mut Window, cx: &mut Context<Self>) {
        let mut live = HashMap::new();
        let mut order = Vec::new();
        for mut t in v.get("terminals").and_then(Value::as_array).cloned().unwrap_or_default() {
            let Some(obj) = t.as_object_mut() else { continue };
            let addr = obj.get("address").and_then(Value::as_str).map(str::to_owned);
            if let (Some(a), Some(tp), Some(fp)) = (
                &addr,
                obj.get("pid").and_then(Value::as_u64),
                obj.get("foreground").and_then(|f| f.get("pid")).and_then(Value::as_u64),
            ) && tp != fp
            {
                self.shell_child_seen.insert(a.clone());
            }
            let child_seen = addr.as_deref().is_some_and(|a| self.shell_child_seen.contains(a));
            let report = obj.remove("status").unwrap_or(Value::Null);
            let word = report.get("state").cloned().unwrap_or_else(|| if report.is_string() { report.clone() } else { Value::Null });
            obj.insert("status".into(), word);
            let argv = effective_running_argv(obj, child_seen);
            obj.insert("program".into(), program_of(&report, argv.as_ref()));
            obj.insert("report".into(), report);
            // Preserve the raw title (with spinner/status markers) for non-7501 status inference.
            if let Some(raw_title) = obj.get("title").and_then(Value::as_str).map(str::to_owned) {
                let shown = crate::terminal::shown_title(&raw_title).into_owned();
                obj.insert("raw_title".into(), raw_title.into());
                obj.insert("title".into(), shown.into());
            }
            if let Some(a) = addr {
                order.push(a.clone());
                live.insert(a, Value::Object(obj.clone()));
            }
        }
        for mut agent in v.get("agents").and_then(Value::as_array).cloned().unwrap_or_default() {
            let Some(obj) = agent.as_object_mut() else { continue };
            let Some(addr) = obj.get("address").and_then(Value::as_str).map(str::to_owned) else { continue };
            let report = obj.remove("status").unwrap_or(Value::Null);
            let raw_state = report.get("state").and_then(Value::as_str).or_else(|| report.as_str()).map(str::to_owned);
            let word = match raw_state.as_deref() {
                Some("starting" | "working") => json!("working"),
                Some("blocked" | "error") => json!("blocked"),
                _ => json!("idle"),
            };
            let app_hint = report
                .get("app")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| obj.get("agent").and_then(Value::as_str).filter(|s| !s.is_empty()));
            let mut prog = program_of(&json!({ "app": app_hint }), obj.get("argv"));
            prog["agent"] = json!(true);
            let raw_title = obj
                .get("title")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| obj.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()))
                .or(app_hint)
                .unwrap_or(&addr)
                .to_owned();
            let shown = crate::terminal::shown_title(&raw_title).into_owned();
            obj.insert("acp".into(), json!(true));
            obj.insert("reported".into(), json!(true));
            obj.insert("raw_state".into(), raw_state.map_or(Value::Null, Value::String));
            obj.insert("status".into(), word);
            obj.insert("report".into(), report);
            obj.insert("program".into(), prog);
            obj.insert("raw_title".into(), raw_title.into());
            obj.insert("title".into(), shown.into());
            order.push(addr.clone());
            live.insert(addr, Value::Object(obj.clone()));
        }
        self.live = live;
        self.live_order = order;
        self.shell_child_seen.retain(|a| self.live.contains_key(a));
        self.manual_statuses.retain(|a, _| self.live.contains_key(a));
        self.ai_terminal_states.retain(|a, _| self.live.contains_key(a));
        self.ai_last_attempt.retain(|a, _| self.live.contains_key(a));
        self.ai_digest_attempted.retain(|a, _| self.live.contains_key(a));
        self.last_agent_titles.retain(|a, _| self.live.contains_key(a));
        self.closing_terminals.retain(|addr, completed| !*completed || self.live.contains_key(addr));
        // Seed `manual_status` on live records before inferring or publishing `terminals()`.
        // Also strip any lingering CLI agent task info when a PTY terminal has returned to its shell.
        let addrs: Vec<String> = self.live.keys().cloned().collect();
        for addr in addrs {
            let is_acp = is_acp_addr(&addr)
                || self.live.get(&addr).and_then(|r| r.get("acp")).and_then(Value::as_bool) == Some(true);
            let is_agent_prog = self.live.get(&addr).and_then(|r| r.pointer("/program/agent")).and_then(Value::as_bool) == Some(true);
            let is_shell_prog = !is_acp
                && !is_agent_prog
                && self.live.get(&addr).and_then(|r| r.pointer("/program/shell")).and_then(Value::as_bool) == Some(true);
            if !is_acp && is_agent_prog {
                let view_title = self
                    .best_terminal_view(&addr, cx)
                    .map(|v| v.read(cx).title.clone())
                    .filter(|s| !s.is_empty());
                let rec_title = self
                    .live
                    .get(&addr)
                    .and_then(|r| r.get("title"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                if let Some(title) = view_title.or(rec_title) {
                    self.last_agent_titles.insert(addr.clone(), title);
                }
            } else if is_shell_prog {
                self.ai_terminal_states.remove(&addr);
                self.ai_digest_attempted.remove(&addr);
                let had_manual_label = self
                    .live
                    .get(&addr)
                    .and_then(|r| r.pointer("/labels/herdr.status"))
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty());
                self.manual_statuses.remove(&addr);
                if had_manual_label {
                    Self::write_core_label(self.core.clone(), addr.clone(), "herdr.status", String::new(), cx);
                }
                if let Some(obj) = self.live.get_mut(&addr).and_then(Value::as_object_mut) {
                    obj.insert("semantic_title".into(), Value::Null);
                    obj.insert("micro_summary".into(), Value::Null);
                    obj.insert("ai_status_summary".into(), Value::Null);
                    obj.insert("ai_status_kind".into(), Value::Null);
                    obj.insert("working_since".into(), Value::Null);
                    obj.insert("work_ms".into(), Value::Null);
                    obj.insert("total_work_ms".into(), json!(0));
                    obj.insert("manual_status".into(), Value::Null);
                }
                if let Some(prev_agent_title) = self.last_agent_titles.remove(&addr) {
                    let matching_views: Vec<Entity<TerminalView>> = self
                        .terms
                        .values()
                        .filter(|t| t.address == addr && t.view.read(cx).title == prev_agent_title)
                        .map(|t| t.view.clone())
                        .collect();
                    for view in matching_views {
                        view.update(cx, |v, cx| v.clear_title(cx));
                    }
                }
                continue;
            }
            let manual = self.resolve_manual_status(&addr, cx);
            if let Some(obj) = self.live.get_mut(&addr).and_then(Value::as_object_mut) {
                obj.insert("manual_status".into(), manual.as_deref().map_or(Value::Null, |s| json!(s)));
            }
        }
        self.refresh_inferred_statuses(cx);
        self.refresh_missing_semantic_digests(cx);
        // A new sub-agent or updated inferred state changes `terminals()`.
        let terminals_changed = mods::publish_terminals(self.ordered_live_values());
        self.tend_terminals(true, window, cx);
        if terminals_changed {
            self.refresh_snapshot_mods(cx);
        }
    }

    /// Every tick, with or without news from the core: where the terminals go
    /// (a layout that arrived late can place one now), what they mean now (a
    /// finished agent you're looking at is no longer news), their folders, their
    /// streams. `listed`: the list itself changed.
    fn tend_terminals(&mut self, listed: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Place newly labeled terminals first: one that is blocked on its first
        // listing is ours, and must ring.
        let mut changed = self.reconcile(window, cx);
        let visible: HashSet<String> = self.visible_addresses().into_iter().collect();
        let mut alerts = Vec::new();
        let addrs: Vec<String> = self.live.keys().cloned().collect();
        let mut republished = false;
        let mut manual_released = false;
        for addr in &addrs {
            if self.closing_terminals.contains_key(addr) {
                continue;
            }
            let manual = self.resolve_manual_status(addr, cx);
            let prev_manual = self.live.get(addr).and_then(|r| r.get("manual_status")).and_then(Value::as_str).map(str::to_owned);
            if prev_manual != manual {
                republished = true;
                if manual.is_none() {
                    manual_released = true;
                }
                if let Some(obj) = self.live.get_mut(addr).and_then(Value::as_object_mut) {
                    obj.insert("manual_status".into(), manual.as_deref().map_or(Value::Null, |s| json!(s)));
                }
            }
        }
        if (!listed || manual_released) && self.refresh_inferred_statuses(cx) {
            republished = true;
        }
        if !listed {
            self.refresh_missing_semantic_digests(cx);
        }
        for addr in addrs {
            if self.closing_terminals.contains_key(&addr) {
                continue;
            }
            let Some(rec) = self.live.get(&addr) else { continue };
            let is_acp = is_acp_addr(&addr) || rec.get("acp").and_then(Value::as_bool) == Some(true);
            let is_agent_prog = rec.pointer("/program/agent").and_then(Value::as_bool) == Some(true);
            let manual = rec.get("manual_status").and_then(Value::as_str).map(str::to_owned);
            let word = rec.get("status").and_then(Value::as_str).map(str::to_owned);
            if !is_acp && !is_agent_prog && word.is_none() && manual.is_none() {
                changed |= self.model.clear_agent(&addr);
                if let Some(obj) = self.live.get_mut(&addr).and_then(Value::as_object_mut) {
                    let cleared_ws = obj.get("working_since").is_some_and(|v| !v.is_null());
                    let cleared_wm = obj.get("work_ms").is_some_and(|v| !v.is_null());
                    let cleared_tm = obj.get("total_work_ms").and_then(Value::as_u64).unwrap_or(0) != 0;
                    if cleared_ws || cleared_wm || cleared_tm {
                        obj.insert("working_since".into(), Value::Null);
                        obj.insert("work_ms".into(), Value::Null);
                        obj.insert("total_work_ms".into(), json!(0));
                        republished = true;
                    }
                }
                continue;
            }
            let app = rec
                .get("report")
                .and_then(|r| r.get("app"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| rec.get("agent").and_then(Value::as_str).filter(|s| !s.is_empty()))
                .or_else(|| word.as_ref().and_then(|_| rec.pointer("/program/key").and_then(Value::as_str)))
                .map(str::to_owned);
            let looking = visible.contains(&addr) && self.window_active;
            changed |= self.model.set_manual_status(&addr, manual, app.clone());
            let prev = self.model.agents.get(&addr).and_then(|a| a.word.clone());
            let child = rec.get("parent").and_then(Value::as_str).is_some_and(|p| !p.is_empty());
            if let Some(alert) = self.model.alert_for(&addr, child, prev.as_deref(), word.as_deref(), looking) {
                alerts.push((addr.clone(), alert, looking));
            }
            changed |= self.model.set_word(&addr, word, app, looking);
            let turn = rec.get("turn").and_then(Value::as_u64).unwrap_or(0);
            let label = rec.pointer("/labels/herdr.seen").and_then(Value::as_str).and_then(|s| s.parse::<u64>().ok());
            changed |= self.model.set_turn(&addr, turn, label.max(self.written_seen.get(&addr).copied()));
            let core_ws = rec.get("working_since").and_then(Value::as_u64);
            let core_work_ms = rec.get("work_ms").and_then(Value::as_u64);
            let core_total_ms = rec.get("total_work_ms").and_then(Value::as_u64);
            let core_status_ms = rec.get("status_ms").and_then(Value::as_u64);
            let is_starting = rec.get("raw_state").and_then(Value::as_str) == Some("starting") && turn == 0;
            changed |= self.model.sync_core_timing(&addr, core_ws, core_work_ms, core_total_ms, core_status_ms, is_starting);
            if let Some(a) = self.model.agents.get(&addr)
                && let Some(obj) = self.live.get_mut(&addr).and_then(Value::as_object_mut)
            {
                let want_ws = a.working_since.map_or(Value::Null, |v| json!(v));
                let want_wm = a.work_ms.map_or(Value::Null, |v| json!(v));
                let want_tm = json!(a.total_work_ms);
                if obj.get("working_since") != Some(&want_ws)
                    || obj.get("work_ms") != Some(&want_wm)
                    || obj.get("total_work_ms") != Some(&want_tm)
                {
                    obj.insert("working_since".into(), want_ws);
                    obj.insert("work_ms".into(), want_wm);
                    obj.insert("total_work_ms".into(), want_tm);
                    republished = true;
                }
            }
        }
        if republished {
            mods::publish_terminals(self.ordered_live_values());
        }
        self.written_seen.retain(|a, _| self.live.contains_key(a));
        changed |= self.share_seen(cx);
        // Changes already made before Herdr started aren't news.
        if self.listed_once {
            for (addr, alert, looking) in alerts {
                self.alert(&addr, alert, looking, window, cx);
            }
        }
        self.track_folders(cx);
        for t in self.terms.values() {
            if self.live.get(&t.address).is_some_and(|r| r["pid"].as_u64().is_some()) {
                t.view.update(cx, |v, cx| v.reattach_if_needed(self.core.clone(), cx));
            }
            self.tell_term(&t.view, &t.address, cx);
        }
        self.listed_once = true;
        changed |= self.model.tabs().any(|c| {
            c.root.terminals().into_iter().any(|n| {
                n.terminal().is_some_and(|a| {
                    (!self.swapped.contains_key(&n.id) && self.is_structured_terminal(a))
                        != self.copies.contains_key(&format!("{}#peek", n.id))
                })
            })
        });
        if changed {
            self.sync(window, cx);
        } else if listed || republished {
            self.publish(cx);
            if republished {
                self.refresh_snapshot_mods(cx);
            }
        }
    }

    /// Tells the person an agent needs them or finished: Herdr's sounds, and a
    /// toast (drawn by the `toast` mod; click it to go there) unless they're
    /// looking at that agent.
    fn alert(&mut self, addr: &str, alert: model::Alert, looking: bool, window: &mut Window, cx: &mut Context<Self>) {
        if settings::get().sounds.enabled {
            crate::sound::play(if alert == model::Alert::Blocked { "blocked" } else { "done" });
        }
        // Needing the person brings a settled tab back.
        if let Some(ctx) = self.model.pane_of(&self.root_agent(addr)).and_then(|p| self.model.tab_of(&p)).map(|c| c.id.clone())
            && self.model.unsettle(&ctx)
        {
            self.save();
            self.publish(cx);
        }
        if looking {
            return;
        }
        let rec = self.live.get(addr);
        let app = self.model.agents.get(addr).and_then(|a| a.app.clone());
        let name = rec
            .and_then(|r| r.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned))
            .or_else(|| app.as_deref().map(crate::programs::name))
            .unwrap_or_else(|| "An agent".into());
        // A sub-agent lives where its top agent does.
        let home = self.root_agent(addr);
        let place = self.model.pane_of(&home).and_then(|pane| {
            let (p, c) = (self.model.space_of(&pane)?, self.model.tab_of(&pane)?);
            Some(format!("{} / {}", self.model.space_name(p, &self::home()), Model::tab_name(p, c)))
        });
        let (title, level) = match alert {
            model::Alert::Blocked => (
                if settings::get().language == settings::Language::ZhCn { format!("{name} 需要你的确认") } else { format!("{name} needs you") },
                "error",
            ),
            model::Alert::Done => (
                if settings::get().language == settings::Language::ZhCn { format!("{name} 已完成") } else { format!("{name} finished") },
                "info",
            ),
        };
        let notification = json!({
            "title": title, "body": place, "level": level,
            "action": { "name": "agent.focus", "args": { "terminal": addr } },
        });
        self.open_overlay(mods::new_overlay_id(), json!({ "kind": "toast", "notification": notification }), window, cx);
    }

    /// Keeps the model true to core, on every listing: drops panes whose terminal
    /// is gone (ended while hidden, or while the app was closed) and places
    /// terminals labeled `herdr.space` that aren't shown (started by an
    /// agent, a script, or before a restart). Returns whether anything changed.
    fn reconcile(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // No fresh shells while quitting: the next start brings them back.
        if QUITTING.load(std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        self.ended_acp.retain(|a, rec| !self.live.contains_key(a) && self.model.acp_pane_of(a).is_some() && !is_side_chat_record(rec));
        // Side-chat agents belong strictly in their parent's right-side panel, never in tabs.
        let mounted_side_chats: Vec<String> = self
            .model
            .tabs()
            .flat_map(|c| c.root.panes())
            .filter_map(|n| n.any_terminal().map(str::to_owned))
            .filter(|a| self.is_side_chat(a))
            .collect();
        let mut evicted_side_chat = false;
        for addr in mounted_side_chats {
            evicted_side_chat |= self.evict_side_chat_from_tabs(&addr, cx);
        }
        let missing = self.model.tabs().flat_map(|c| c.root.panes()).chain(self.chrome.terminals())
            .filter_map(|n| n.any_terminal().filter(|a| !self.live.contains_key(*a) && !self.ended_acp.contains_key(*a) && !self.just_born(a)).map(|a| (n.id.clone(), a.to_owned())))
            .collect::<Vec<_>>();
        if !missing.is_empty() && !self.checking_closed {
            self.check_closed(missing, window, cx);
        }
        let mut orphans = self.live.iter()
            .filter(|(a, r)| !is_acp_addr(a) && r.get("acp").and_then(Value::as_bool) != Some(true) && !is_trashed_record(r) && !is_side_chat_record(r))
            .filter(|(a, _)| self.model.pane_of(a).is_none() && !self.changing_terminals.contains(*a) && !self.closing_terminals.contains_key(*a))
            .filter_map(|(a, r)| {
                let machine = a.split_once('/').map(|(m, _)| m);
                if !self.documents.get(machine.unwrap_or("")).is_some_and(|s| s.available) { return None; }
                let labels = r.get("labels")?;
                let space = labels.get("herdr.space")?.as_str().filter(|s| !s.is_empty())?;
                let tab = labels.get("herdr.tab").and_then(Value::as_str).filter(|s| !s.is_empty());
                Some((a.clone(), crate::mirror::qualify(space, machine), tab.map(|t| crate::mirror::qualify(t, machine))))
            }).collect::<Vec<_>>();
        orphans.sort();
        for (addr, space, tab) in orphans {
            self.start_adoption(addr, space, tab, window, cx);
        }
        let mut unmounted_acp: Vec<String> = self.live.iter()
            .filter(|(a, r)| is_acp_addr(a) || r.get("acp").and_then(Value::as_bool) == Some(true))
            .filter(|(a, r)| {
                self.model.acp_pane_of(a).is_none()
                    && !self.changing_terminals.contains(*a)
                    && !self.closing_terminals.contains_key(*a)
                    && should_auto_mount_acp(r)
            })
            .filter(|(a, _)| {
                let machine = a.split_once('/').map(|(m, _)| m);
                self.documents.get(machine.unwrap_or("")).is_some_and(|s| s.available)
            })
            .map(|(a, _)| a.clone())
            .collect();
        unmounted_acp.sort();
        for addr in unmounted_acp {
            let _ = self.keep_terminal_with_focus(&addr, "tab", false, window, cx);
        }
        evicted_side_chat
    }

    fn start_adoption(&mut self, addr: String, space: String, tab: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.changing_terminals.insert(addr.clone());
        let core = self.core.clone();
        let generation = self.core_generation;
        cx.spawn_in(window, async move |this, cx| {
            let (address, wanted_space, wanted_tab) = (addr.clone(), space.clone(), tab.clone());
            let ready = cx.background_executor().spawn(async move {
                let record = core.call("terminal.get", json!({"terminal":address}))?;
                let machine = address.split_once('/').map(|(m, _)| m);
                if !adoptable(&record, &wanted_space, wanted_tab.as_deref(), machine) { return Ok(false); }
                if record["keep"].as_bool() != Some(true) {
                    core.call("terminal.keep", json!({"terminal":address,"keep":true}))?;
                }
                anyhow::Ok(true)
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.changing_terminals.remove(&addr);
                if generation != this.core_generation || this.draining { return; }
                if matches!(ready, Ok(true)) && this.model.pane_of(&addr).is_none()
                    && this.require_document(addr.split_once('/').map(|(m, _)| m)).is_ok()
                {
                    this.adopt(&addr, &space, tab.as_deref());
                    this.sync(window, cx);
                    this.flush_documents(false, window, cx);
                }
            });
        }).detach();
    }

    /// Puts a live terminal where its labels say, without changing what's
    /// shown. A space or tab the app doesn't know is made under the label's
    /// id, unnamed (called Herdr's way).
    fn adopt(&mut self, addr: &str, space: &str, ctx: Option<&str>) {
        let machine = addr.split_once('/').map(|(m, _)| m.to_owned());
        let pane = self.model.fresh_id_on("p", machine.as_deref());
        let ctx_id = ctx.map(str::to_owned).unwrap_or_else(|| self.model.fresh_id_on("tb", machine.as_deref()));
        // Found by its labels: no names, so they're called Herdr's way.
        let new_ctx = Tab::new(&ctx_id, "", &pane, addr);
        if self.model.space(space).is_none() {
            // Its live folder may not be read yet: the core's record has it.
            let rec = self.live.get(addr);
            let folder = self
                .cwds
                .get(addr)
                .cloned()
                .or_else(|| rec.and_then(|r| r.get("foreground").and_then(|f| f.get("cwd")).or_else(|| r.get("cwd"))).and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_else(|| mods::home().to_string_lossy().into_owned());
            let active = self.model.active.clone();
            self.model.add_space(space, "", &folder, new_ctx);
            self.model.space_mut(space).unwrap().machine = machine;
            self.model.active = active.or(Some(space.to_owned()));
            return;
        }
        let target = self.model.tab(&ctx_id).and_then(|c| c.root.panes().last().map(|p| p.id.clone()));
        if let Some(target) = target {
            let tab = self.model.tab_of(&target).unwrap();
            let focused = tab.focused.clone();
            let before = tab.before.clone();
            let zoomed = tab.zoomed.clone();
            let id = tab.id.clone();
            self.model.split(&target, Axis::Row, &pane, addr);
            let tab = self.model.tab_mut(&id).unwrap();
            tab.focused = focused;
            tab.before = before;
            tab.zoomed = zoomed;
            return;
        }
        let keep = self.model.space(space).and_then(|p| p.active.clone());
        self.model.add_tab(space, new_ctx);
        if let Some(p) = self.model.space_mut(space) {
            p.active = keep;
        }
    }

    /// Reads each terminal's live folder, and its repo and branch for mods.
    fn track_folders(&mut self, cx: &mut Context<Self>) {
        let home = mods::home().to_string_lossy().into_owned();
        let mut todo: HashMap<(Option<String>, String), Vec<String>> = HashMap::new();
        let mut moved = false;
        for (addr, rec) in &self.live {
            // Core's live folder of what runs there (any machine), else the shell's (local).
            let cwd = rec
                .get("foreground")
                .and_then(|f| f.get("cwd"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                // (a pid on another machine means nothing here)
                .or_else(|| rec.get("pid").and_then(Value::as_u64).filter(|_| !addr.contains('/')).and_then(crate::process_cwd::read).map(|p| p.to_string_lossy().into_owned()))
                .or_else(|| rec.get("cwd").and_then(Value::as_str).map(str::to_owned));
            let Some(cwd) = cwd else { continue };
            let busy = busy(rec);
            let last = self.folders_checked.get_mut(addr);
            if recheck(last.as_deref(), &cwd, busy, self.window_active) {
                moved |= self.cwds.get(addr) != Some(&cwd);
                self.cwds.insert(addr.clone(), cwd.clone());
                let repo = last.as_ref().is_none_or(|l| l.cwd != cwd || l.repo); // until git says otherwise
                self.folders_checked.insert(addr.clone(), FolderCheck { at: Instant::now(), cwd: cwd.clone(), busy, repo });
                // Another machine's terminal (`vps/t7`): git runs there.
                todo.entry((addr.split_once('/').map(|(m, _)| m.to_owned()), cwd)).or_insert_with(Vec::new).push(addr.clone());
            } else if let Some(last) = last {
                last.busy = busy;
            }
        }
        self.folders_checked.retain(|a, _| self.live.contains_key(a));
        // Refresh unnamed spaces when a terminal moves.
        if moved {
            self.save();
        }
        // Once per machine and folder, however many terminals are there.
        for ((on, cwd), addrs) in todo {
            let home = home.clone();
            cx.spawn(async move |this, cx| {
                let asked = cwd.clone();
                let (info, repo) = cx.background_executor().spawn(async move { folder_info(&asked, &home, on.as_deref()) }).await;
                let _ = this.update(cx, |this, cx| {
                    let mut changed = false;
                    // Not one that moved on meanwhile: its own answer is coming.
                    for addr in addrs.into_iter().filter(|a| this.cwds.get(a) == Some(&cwd)) {
                        if let Some(check) = this.folders_checked.get_mut(&addr) {
                            check.repo = repo;
                        }
                        if this.model.folder_info.get(&addr) != Some(&info) {
                            this.model.folder_info.insert(addr, info.clone());
                            changed = true;
                        }
                    }
                    if changed {
                        this.publish(cx);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    fn terminal_ended(&mut self, block: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = block.strip_suffix(SWAP) {
            self.swapped.remove(pane);
            self.focused_pane = None;
            return self.sync(window, cx);
        }
        if let Some(address) = self.model.address_of(block) {
            self.notify_launch_failure(&address, window, cx);
        }
        if let Some(o) = self.overlays.iter().find(|o| o.layout.as_ref().is_some_and(|l| l.find(block).is_some())) {
            let id = o.id.clone();
            return self.close_overlay(&id, window, cx);
        }
        // How its stream ended, for `reconcile` (`hc run --rm` leaves no record).
        if let Some(t) = self.terms.get(block)
            && let Some(why) = t.view.read(cx).ended.clone()
        {
            // The program ended on its own (or was killed): its pane goes now. A
            // stream that just broke (the core died) says neither.
            let closed = matches!(why.as_str(), "exited" | "killed" | "finished");
            if closed && self.chrome.find(block).and_then(Node::terminal) == Some(t.address.as_str()) {
                self.chrome.remove_slot(block);
                return self.sync(window, cx);
            }
            if closed && self.model.address_of(block).as_deref() == Some(t.address.as_str()) {
                self.model.remove_pane(block);
                return self.sync(window, cx);
            }
        }
        // Otherwise the next listing checks closure; core owns lost terminals.
    }

    fn notify_launch_failure(&mut self, address: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.born.borrow_mut().remove(address).is_none() {
            return;
        }
        let core = self.core.clone();
        let address = address.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            let failure = cx.background_executor().spawn(async move {
                let record = core.call("terminal.get", json!({"terminal": address})).ok()?;
                launch_failure(&record, || {
                    let output = core.call("terminal.read", json!({"terminal": address, "lines": 5})).unwrap_or(Value::Null);
                    output["text"].as_str().unwrap_or_default().to_owned()
                })
            }).await;
            if let Some(body) = failure {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.open_overlay(mods::new_overlay_id(), json!({"kind":"toast", "notification":{"title":"Terminal failed to start", "body":body, "level":"error"}}), window, cx);
                });
            }
        }).detach();
    }

    // ---- creating terminals ------------------------------------------------

    /// The folder new terminals in `space` start in: where its shown tab's focused
    /// pane is now (Herdr's "follow"), else the folder the space was made in.
    fn folder_for(&self, space: &str) -> String {
        let p = self.model.space(space);
        let here = p.and_then(|p| p.active_tab()).and_then(|c| c.focused_pane()).and_then(|f| self.model.address_of(&f)).and_then(|a| self.cwds.get(&a).cloned());
        here.or_else(|| p.map(|p| p.folder.clone())).unwrap_or_else(home)
    }

    /// The machine a space's terminals run on, None for this one.
    fn require_document(&self, machine: Option<&str>) -> Result<(), String> {
        match self.documents.get(machine.unwrap_or("")) {
            Some(slot) if slot.available => Ok(()),
            Some(slot) => Err(slot.error.clone().unwrap_or_else(|| "This machine's layout is unavailable. Try again when it reconnects.".into())),
            None => Err(match machine {
                Some(m) => format!("Herdr2 has no machine called {m}. See h2 machine ls; machines added with --temp stay out of Herdr2."),
                None => "This machine's layout is unavailable. Try again when it reconnects.".into(),
            }),
        }
    }

    fn machine_of(&self, space: &str) -> Option<String> {
        self.model.space(space).and_then(|p| p.machine.clone())
    }

    /// Starts a terminal for `space`, on `on` (another machine) or here. An
    /// empty `cwd`: the machine's home. With no argv, that machine's core
    /// picks its own shell. `stay`: `argv` is an agent in a shell that stays
    /// (`agent_argv`), else a program its shell becomes (`split_argv`).
    fn spawn(&mut self, argv: Option<Vec<String>>, stay: bool, cwd: String, follow: bool, space: &str, ctx: &str, on: Option<&str>, size: Option<(u16, u16)>, window: &Window) -> Result<(String, String), String> {
        let argv = argv.unwrap_or_default();
        self.require_document(on)?;
        let mut labels = json!({ "herdr.space": crate::mirror::local(space, on), "herdr.tab": crate::mirror::local(ctx, on), "herdr.seen": "0" });
        // A split's wrapper shows it only when it can't start the program (`remote_shell::cannot_start`).
        if !stay && !argv.is_empty() {
            use std::hash::{BuildHasher, Hasher};
            labels["herdr.launch"] = format!("herdr-launch-{:016x}", std::collections::hash_map::RandomState::new().build_hasher().finish()).into();
        }
        let (addr, cwd) = run_following_folder(cwd, follow, on, |cwd| match self.core.run(argv.clone(), cwd.clone(), labels.clone(), on, size, crate::terminal::initial_display(window)) {
            // A start on a machine without `sh` (Windows): in its PowerShell, or as given.
            Err(e) if on.is_some() && argv.first().map(String::as_str) == Some("sh") && format!("{e:#}").contains("not found") => {
                let argv = if stay { Self::powershell_argv(&argv) } else { argv.get(4..).unwrap_or_default().to_vec() };
                self.core.run(argv, cwd, labels.clone(), on, size, crate::terminal::initial_display(window))
            }
            r => r,
        })
        .map_err(|e| format!("cannot start a terminal: {e:#}"))?;
        self.born.borrow_mut().insert(addr.clone(), Instant::now());
        // Follow the launch folder until the first live folder report arrives.
        if !cwd.is_empty() {
            self.cwds.insert(addr.clone(), cwd.clone());
        }
        Ok((addr, cwd))
    }

    /// The PowerShell form of a remote agent start (`agent_argv`): the program,
    /// then a shell that stays.
    fn powershell_argv(sh: &[String]) -> Vec<String> {
        let cmd = sh.get(4..).unwrap_or_default();
        vec![
            "powershell.exe".into(),
            "-NoLogo".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-NoExit".into(),
            "-Command".into(),
            crate::programs::powershell_line(cmd),
        ]
    }

    /// An agent with its first message, in a shell that stays when the agent exits
    /// (on another machine: that machine's shell).
    fn agent_argv(argv: Option<Vec<String>>, task: Option<&str>, on: Option<&str>) -> Option<Vec<String>> {
        let task = task.map(str::trim).filter(|t| !t.is_empty());
        argv.filter(|a| !a.is_empty()).map(|mut cmd| {
            cmd.extend(task.map(str::to_owned));
            if on.is_some() {
                return crate::remote_shell::launch(cmd);
            }
            let sh = shell();
            if cfg!(windows) {
                // PowerShell has no `-ic`: run it, then stay. `-ExecutionPolicy Bypass`
                // allows npm/Cursor `.ps1` shims even when system policy is Restricted.
                return vec![
                    sh,
                    "-ExecutionPolicy".into(),
                    "Bypass".into(),
                    "-NoExit".into(),
                    "-Command".into(),
                    crate::programs::powershell_line(&cmd),
                ];
            }
            // macOS terminal sessions load login profiles (including Homebrew's PATH).
            unix_agent_argv(sh, cmd, cfg!(target_os = "macos"), true)
        })
    }

    /// A split's program, in your shell (for your PATH) that then becomes it: the
    /// pane ends with it, keeping its exit code. Elsewhere (PowerShell, nushell) as given.
    fn split_argv(cmd: Vec<String>, on: Option<&str>) -> Vec<String> {
        if on.is_some() {
            return crate::remote_shell::exec(cmd);
        }
        if cfg!(windows) {
            return cmd;
        }
        unix_agent_argv(shell(), cmd, cfg!(target_os = "macos"), false)
    }

    /// Resolves local interactive shell argv from an explicit `shell` override or user settings.
    fn resolve_local_shell_argv(shell_override: Option<&str>, on: Option<&str>) -> Option<Vec<String>> {
        if on.is_some() {
            return None;
        }
        let s = settings::get();
        let choice = shell_override
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| s.shell.trim());
        if choice.is_empty() || choice.eq_ignore_ascii_case("auto") {
            return None;
        }
        if cfg!(windows) {
            return resolve_windows_shell_argv(choice);
        }
        Some(vec![choice.to_owned()])
    }

    /// Saves a single-pane tab's custom tab name onto its terminal's `herdr.name` label
    /// before merging multiple panes into one tab.
    fn preserve_single_pane_tab_name(&mut self, tab_id: &str, cx: &mut Context<Self>) -> bool {
        let Some((pane_id, tab_name)) = self.model.tab(tab_id).and_then(|c| {
            let panes = c.root.panes();
            (panes.len() == 1).then(|| (panes[0].id.clone(), c.name.trim().to_owned()))
        }) else {
            return false;
        };
        if !tab_name.is_empty()
            && let Some(addr) = self.model.any_address_of(&pane_id)
        {
            let has_custom = self
                .live
                .get(&addr)
                .or_else(|| self.ended_acp.get(&addr))
                .and_then(|r| r.pointer("/labels/herdr.name"))
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty());
            if !has_custom {
                for map in [&mut self.live, &mut self.ended_acp] {
                    if let Some(obj) = map.get_mut(&addr).and_then(Value::as_object_mut)
                        && let Some(labels) = obj.entry("labels").or_insert_with(|| json!({})).as_object_mut()
                    {
                        labels.insert("herdr.name".into(), json!(tab_name.clone()));
                    }
                }
                Self::write_core_label(self.core.clone(), addr, "herdr.name", tab_name, cx);
            }
        }
        true
    }

    /// A space with its first tab and terminal, shown at once unless focus is false.
    /// `machine`: another machine (`hc machine ls`); its space starts in that machine's home.
    fn new_space(&mut self, name: &str, folder: Option<String>, ctx_name: Option<&str>, argv: Option<Vec<String>>, task: Option<&str>, machine: Option<String>, shell: Option<String>, focus: bool, window: &Window) -> Result<Value, String> {
        let follow = folder.is_none();
        let on = machine.as_deref();
        // The focused pane's folder only means something on its own machine.
        let here = || self.model.focused_pane().and_then(|p| self.model.address_of(&p))
            .filter(|a| a.split_once('/').map(|(m, _)| m) == on)
            .and_then(|a| self.cwds.get(&a).cloned());
        let folder = folder.or_else(here).unwrap_or_else(|| if on.is_some() { String::new() } else { home() });
        let (space, ctx, pane) = (self.model.fresh_id_on("sp", on), self.model.fresh_id_on("tb", on), self.model.fresh_id_on("p", on));
        let size = self.initial_size(&Tab::new(&ctx, "", &pane, "").root, &pane, window);
        let argv = Self::agent_argv(argv.clone(), task, on)
            .or(argv)
            .or_else(|| Self::resolve_local_shell_argv(shell.as_deref(), on));
        let (addr, folder) = self.spawn(argv, true, folder, follow, &space, &ctx, on, size, window)?;
        let ctx_name = ctx_name.map(str::trim).unwrap_or_default();
        let tab = Tab::new(&ctx, ctx_name, &pane, &addr);
        if focus { self.model.add_space(&space, name.trim(), &folder, tab) } else { self.model.add_space_unshown(&space, name.trim(), &folder, tab) }
        if let Some(p) = self.model.space_mut(&space) {
            p.machine = machine;
        }
        Ok(json!({ "space": space, "tab": ctx, "terminal": addr }))
    }

    /// A tab in a space (default: the shown one), shown at once unless focus is false.
    fn new_tab(&mut self, space: Option<String>, name: &str, folder: Option<String>, argv: Option<Vec<String>>, task: Option<&str>, shell: Option<String>, focus: bool, window: &Window) -> Result<Value, String> {
        let space = space.or_else(|| self.model.active_space().map(|p| p.id.clone())).ok_or("no space yet: make one first (space.new)")?;
        if self.model.space(&space).is_none() {
            return Err(format!("no space {space:?}"));
        }
        let follow = folder.is_none();
        let folder = folder.unwrap_or_else(|| self.folder_for(&space));
        let on = self.machine_of(&space);
        let (ctx, pane) = (self.model.fresh_id_on("tb", on.as_deref()), self.model.fresh_id_on("p", on.as_deref()));
        let size = self.initial_size(&Tab::new(&ctx, "", &pane, "").root, &pane, window);
        let argv = Self::agent_argv(argv.clone(), task, on.as_deref())
            .or(argv)
            .or_else(|| Self::resolve_local_shell_argv(shell.as_deref(), on.as_deref()));
        let (addr, _) = self.spawn(argv, true, folder, follow, &space, &ctx, on.as_deref(), size, window)?;
        self.add_new_tab(&space, Tab::new(&ctx, name.trim(), &pane, &addr), focus);
        Ok(json!({ "space": space, "tab": ctx, "terminal": addr }))
    }

    /// Starts an ACP agent directly inside a dedicated tab and optionally focuses it.
    fn new_acp_tab(&mut self, space: Option<String>, name: &str, folder: Option<String>, agent: &str, task: Option<&str>, focus: bool, window: &mut Window, cx: &mut Context<Self>) -> Result<Value, String> {
        let agent_key = agent.trim();
        if agent_key.is_empty() {
            return Err("tab.new acp requires agent".into());
        }
        let create_space = space.is_none() && self.model.active_space().is_none();
        let (space_id, space_folder, on) = if create_space {
            let folder = folder.unwrap_or_else(home);
            (self.model.fresh_id("sp"), folder, None)
        } else {
            let sp = space
                .or_else(|| self.model.active_space().map(|p| p.id.clone()))
                .ok_or("no space yet: make one first (space.new)")?;
            if self.model.space(&sp).is_none() {
                return Err(format!("no space {sp:?}"));
            }
            let on = self.machine_of(&sp);
            let folder = folder.unwrap_or_else(|| self.folder_for(&sp));
            (sp, folder, on)
        };
        self.require_document(on.as_deref())?;
        let (ctx, pane) = (
            self.model.fresh_id_on("tb", on.as_deref()),
            self.model.fresh_id_on("p", on.as_deref()),
        );
        let space_label = crate::mirror::local(&space_id, on.as_deref()).to_owned();
        let tab_label = crate::mirror::local(&ctx, on.as_deref()).to_owned();
        let mut params = json!({
            "agent": agent_key,
            "dangerously_skip_permissions": true,
            "keep": true,
            "async_start": true,
            "labels": {
                "herdr.space": space_label,
                "herdr.tab": tab_label,
                "herdr.seen": "0",
                "herdr.parked": ""
            }
        });
        if let Some(prompt) = task.map(str::trim).filter(|s| !s.is_empty()) {
            params["prompt"] = json!(prompt);
        }
        if !space_folder.is_empty() {
            params["cwd"] = json!(space_folder);
        }
        if let Some(ref m) = on {
            params["on"] = json!(m);
        }
        let created = self
            .core
            .call("agent.run", params)
            .map_err(|e| format!("cannot start ACP agent {agent_key}: {e:#}"))?;
        let addr = created["address"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "agent.run did not return address".to_owned())?
            .to_owned();
        self.born.borrow_mut().insert(addr.clone(), Instant::now());
        if let Some(cwd) = created["cwd"].as_str().filter(|s| !s.is_empty()) {
            self.cwds.insert(addr.clone(), cwd.to_owned());
        } else if !space_folder.is_empty() {
            self.cwds.insert(addr.clone(), space_folder.clone());
        }
        self.live.insert(addr.clone(), created);
        let tab = Tab::with_pane(&ctx, name.trim(), Self::acp_pane_node(&pane, &addr));
        if create_space {
            self.model.add_space(&space_id, "", &space_folder, tab);
            self.model.active = Some(space_id.clone());
        } else {
            self.add_new_tab(&space_id, tab, focus);
        }
        if focus {
            self.focus_mounted_acp(&addr, window, cx);
        }
        Ok(json!({ "space": space_id, "tab": ctx, "pane": pane, "terminal": addr }))
    }

    /// Adds a new tab to its space and shows it, or (focus false) leaves what is shown as it is.
    fn add_new_tab(&mut self, space: &str, tab: Tab, focus: bool) {
        if focus {
            self.model.add_tab(space, tab);
            self.model.active = Some(space.to_owned());
        } else {
            self.model.add_tab_unselected(space, tab);
        }
    }

    /// A tab whose only pane is a mod (a file, a board), shown at once unless focus is false.
    fn new_mod_tab(&mut self, space: Option<String>, name: &str, m: String, props: Value, focus: bool) -> Result<Value, String> {
        let space = space.or_else(|| self.model.active_space().map(|p| p.id.clone())).ok_or("no space yet: make one first (space.new)")?;
        if self.model.space(&space).is_none() {
            return Err(format!("no space {space:?}"));
        }
        if !self.folders.contains_key(&m) {
            return Err(format!("no mod {m:?} in mods/"));
        }
        let machine = self.machine_of(&space);
        self.require_document(machine.as_deref())?;
        let (ctx, pane) = (self.model.fresh_id_on("tb", machine.as_deref()), self.model.fresh_id_on("p", machine.as_deref()));
        self.add_new_tab(&space, Tab::with_pane(&ctx, name.trim(), Node::block(pane.clone(), Content::Mod { name: m, props })), focus);
        Ok(json!({ "space": space, "tab": ctx, "pane": pane }))
    }

    fn split(&mut self, pane: Option<String>, axis: Axis, argv: Option<Vec<String>>, folder: Option<String>, shell: Option<String>, window: &Window) -> Result<Value, String> {
        let target = pane.or_else(|| self.model.focused_pane()).ok_or("no pane to split")?;
        let (Some(p), Some(c)) = (self.model.space_of(&target), self.model.tab_of(&target)) else { return Err(format!("no pane {target:?}")) };
        let (space, ctx, fallback) = (p.id.clone(), c.id.clone(), p.folder.clone());
        let target_addr = self.model.any_address_of(&target);
        let follow = folder.is_none();
        let cwd = folder.unwrap_or_else(|| {
            target_addr
                .as_ref()
                .and_then(|a| {
                    self.cwds.get(a).cloned().or_else(|| {
                        self.live
                            .get(a)
                            .and_then(|r| r.get("cwd"))
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                    })
                })
                .unwrap_or(fallback)
        });
        // A split stays on the machine of the pane it splits (else its space's).
        let on = target_addr.and_then(|a| a.split_once('/').map(|(m, _)| m.to_owned())).or_else(|| self.machine_of(&space));
        let new = self.model.fresh_id_on("p", on.as_deref());
        let mut root = self.model.tab(&ctx).expect("target tab").root.clone();
        root.split(&target, axis, Node::block(new.clone(), Content::Terminal { terminal: String::new() }), "initial-size".into());
        let size = self.model.active_tab().filter(|tab| tab.id == ctx)
            .and_then(|_| self.initial_size(&root, &new, window));
        let argv = argv
            .filter(|a| !a.is_empty())
            .map(|a| Self::split_argv(a, on.as_deref()))
            .or_else(|| Self::resolve_local_shell_argv(shell.as_deref(), on.as_deref()));
        let (addr, _) = self.spawn(argv, false, cwd, follow, &space, &ctx, on.as_deref(), size, window)?;
        self.model.split(&target, axis, &new, &addr);
        Ok(json!({ "pane": new, "terminal": addr }))
    }

    /// A mod as a pane beside the target (a viewer next to the shell).
    fn split_mod(&mut self, pane: Option<String>, axis: Axis, name: String, props: Value) -> Result<Value, String> {
        let target = pane.or_else(|| self.model.focused_pane()).ok_or("no pane to split")?;
        if self.model.tab_of(&target).is_none() {
            return Err(format!("no pane {target:?}"));
        }
        if !self.folders.contains_key(&name) {
            return Err(format!("no mod {name:?} in mods/"));
        }
        let machine = self.model.space_of(&target).and_then(|s| s.machine.clone());
        self.require_document(machine.as_deref())?;
        let new = self.model.fresh_id_on("p", machine.as_deref());
        self.model.split_with(&target, axis, Node::block(new.clone(), Content::Mod { name, props }));
        Ok(json!({ "pane": new }))
    }

    /// Ask only whether a missing terminal is closed. Core owns restarting it.
    fn check_closed(&mut self, missing: Vec<(String, String)>, window: &mut Window, cx: &mut Context<Self>) {
        self.checking_closed = true;
        let core = self.core.clone();
        let generation = self.core_generation;
        cx.spawn_in(window, async move |this, cx| {
            let checked = cx.background_executor().spawn(async move {
                missing.into_iter().map(|(pane, addr)| {
                    let result = if is_acp_addr(&addr) {
                        core.call("agent.get", json!({"agent": addr}))
                    } else {
                        core.call("terminal.get", json!({"terminal": addr}))
                    };
                    (pane, addr, result)
                }).collect::<Vec<_>>()
            }).await;
            let _ = this.update_in(cx, |this, window, cx| this.checked_closed(generation, checked, window, cx));
        }).detach();
    }

    fn checked_closed(&mut self, generation: u64, checked: Vec<(String, String, anyhow::Result<Value>)>, window: &mut Window, cx: &mut Context<Self>) {
        self.checking_closed = false;
        if generation != self.core_generation { return; }
        let mut changed = false;
        for (pane, addr, result) in checked {
            if self.chrome.find(&pane).and_then(Node::terminal) == Some(addr.as_str()) {
                // A failed lookup is not evidence the program exited.
                if result.as_ref().is_ok_and(|v| v["state"] == "finished") {
                    self.chrome.remove_slot(&pane);
                    changed = true;
                }
            } else if self.model.any_address_of(&pane).as_deref() == Some(&addr) {
                if is_acp_addr(&addr) && self.model.acp_pane_of(&addr).as_deref() == Some(pane.as_str()) {
                    let close = match &result {
                        Ok(v) => self.closing_terminals.contains_key(&addr)
                            || v["reason"].as_str() == Some("spawn_failed"),
                        Err(e) => crate::hc::error_kind(e) == Some("not_found"),
                    };
                    if close {
                        self.ended_acp.remove(&addr);
                        self.notify_launch_failure(&addr, window, cx);
                        self.model.remove_pane(&pane);
                        changed = true;
                    } else if let Ok(v) = result
                        && v["state"] == "finished"
                    {
                        self.ended_acp.insert(addr.clone(), Self::normalize_ended_acp(&addr, v));
                        changed = true;
                    }
                    continue;
                }
                let closed = match result {
                    Ok(v) => matches!(v["reason"].as_str(), Some("exited" | "killed"))
                        || (is_acp_addr(&addr) && v["state"] == "finished"),
                    Err(e) => crate::hc::error_kind(&e) == Some("not_found"),
                };
                if closed {
                    self.notify_launch_failure(&addr, window, cx);
                    self.model.remove_pane(&pane);
                    changed = true;
                }
            }
        }
        if changed { self.sync(window, cx); }
    }

    fn just_born(&self, addr: &str) -> bool {
        let mut born = self.born.borrow_mut();
        // Keep launch facts until hidden panes have passed through `check_closed`.
        born.retain(|_, t| t.elapsed() < Duration::from_secs(30));
        born.get(addr).is_some_and(|t| t.elapsed() < Duration::from_secs(5))
    }

    fn acp_pane_node(pane: &str, address: &str) -> Node {
        Node::block(
            pane,
            Content::Mod {
                name: "peek".into(),
                props: json!({ "terminal": address, "acp": true, "tab": true }),
            },
        )
    }

    fn default_tab_name_for_pane(&self, pane_id: &str) -> String {
        let Some(addr) = self.model.any_address_of(pane_id) else {
            return self
                .model
                .tab_of(pane_id)
                .and_then(|c| c.root.find(pane_id))
                .and_then(|n| match n.content() {
                    Some(Content::Mod { name, .. }) if name != "peek" => Some(name.clone()),
                    _ => None,
                })
                .unwrap_or_default();
        };
        let rec = self.live.get(&addr).or_else(|| self.ended_acp.get(&addr));
        if let Some(custom) = rec
            .and_then(|r| r.pointer("/labels/herdr.name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return custom.to_owned();
        }
        if !is_acp_addr(&addr) {
            return String::new();
        }
        rec.and_then(|r| r["name"].as_str().filter(|s| !s.is_empty()).map(str::to_owned))
            .or_else(|| rec.and_then(|r| r["title"].as_str().filter(|s| !s.is_empty()).map(str::to_owned)))
            .or_else(|| self.model.agents.get(&addr).and_then(|a| a.app.as_deref()).map(crate::programs::name))
            .or_else(|| rec.and_then(|r| r["agent"].as_str().filter(|s| !s.is_empty()).map(crate::programs::name)))
            .unwrap_or(addr)
    }

    fn focus_mounted_acp(&mut self, addr: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(pane) = self.model.acp_pane_of(addr) else { return false };
        if self.peeked_agent.as_deref() == Some(addr) {
            self.close_overlay("terminal-peek", window, cx);
        }
        if let Some(sp) = self.model.space_of(&pane).map(|s| s.id.clone()) {
            self.model.active = Some(sp);
        }
        self.focused_block = None;
        self.focused_pane = None;
        self.model.focus_pane(&pane);
        self.sync(window, cx);
        true
    }

    /// Gives a running terminal without a pane (a sub-agent, or an ACP agent
    /// mounted as a tab) one in a matching space and focuses it; already in a
    /// pane: focuses that.
    fn keep_terminal(&mut self, addr: &str, place: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<Value, String> {
        self.keep_terminal_with_focus(addr, place, true, window, cx)
    }

    fn keep_terminal_with_focus(&mut self, addr: &str, place: &str, focus: bool, window: &mut Window, cx: &mut Context<Self>) -> Result<Value, String> {
        if self.is_side_chat(addr) {
            if self.evict_side_chat_from_tabs(addr, cx) {
                self.sync(window, cx);
                self.flush_documents(false, window, cx);
            }
            if focus {
                self.open_side_chat_panel(addr, window, cx);
            }
            return Ok(json!({ "side_chat": addr }));
        }
        let is_acp = is_acp_addr(addr);
        let addr_machine = addr.split_once('/').map(|(m, _)| m);
        let wt_path = self.fork_worktree_info(addr).map(|(p, _, _)| p);
        if let Some(p) = self.model.any_pane_of(addr) {
            if is_acp && matches!(place, "right" | "down")
                && let Some(target_pane) = self.model.focused_pane()
                && target_pane != p
                && self.model.space_of(&target_pane).and_then(|s| s.machine.as_deref()) == addr_machine
                && (wt_path.is_none()
                    || self.model.space_of(&target_pane).map(|s| s.id.as_str())
                        == self.model.space_of(&p).map(|s| s.id.as_str()))
            {
                let edge = if place == "right" { crate::layout::Edge::Right } else { crate::layout::Edge::Bottom };
                let axis = if place == "right" { Axis::Row } else { Axis::Column };
                let same_tab = self.model.tab_of(&target_pane).map(|t| &t.id)
                    == self.model.tab_of(&p).map(|t| &t.id);
                if same_tab {
                    self.model.move_pane(&p, &target_pane, Some(edge));
                } else if let Some(target_space) = self.model.space_of(&target_pane).map(|s| s.id.clone())
                    && let Some(target_tab) = self.model.tab_of(&target_pane).map(|t| t.id.clone())
                {
                    let (old_tab_closed, old_space_closed) = self.model.remove_pane(&p);
                    if old_tab_closed && old_space_closed {
                        tracing::debug!("moved last pane of previous space {addr}");
                    }
                    self.model.split_with(&target_pane, axis, Self::acp_pane_node(&p, addr));
                    let space_label = crate::mirror::local(&target_space, addr_machine).to_owned();
                    let tab_label = crate::mirror::local(&target_tab, addr_machine).to_owned();
                    if let Some(labels) = self.live.get_mut(addr).and_then(|r| r.get_mut("labels")).and_then(Value::as_object_mut) {
                        labels.insert("herdr.space".into(), json!(space_label));
                        labels.insert("herdr.tab".into(), json!(tab_label));
                    }
                    let core = self.core.clone();
                    let agent = addr.to_owned();
                    let labels = json!({ "herdr.space": space_label, "herdr.tab": tab_label });
                    cx.background_executor()
                        .spawn(async move {
                            if let Err(e) = core.call("agent.label", json!({ "agent": agent, "labels": labels })) {
                                tracing::warn!("agent.label on split: {e:#}");
                            }
                        })
                        .detach();
                }
                self.focus_mounted_acp(addr, window, cx);
                self.flush_documents(false, window, cx);
                return Ok(json!({ "pane": p }));
            }
            if is_acp {
                self.focus_mounted_acp(addr, window, cx);
            } else {
                self.model.focus_pane(&p);
            }
            return Ok(json!({ "pane": p }));
        }
        if !self.live.contains_key(addr) { return Err(format!("no running terminal {addr:?}")); }
        if self.changing_terminals.contains(addr) {
            if focus
                && let Some(pending) = self.pending_terminals.get_mut(addr)
                && let TerminalChange::Keep { focus: ref mut f, .. } = pending.change
            {
                *f = true;
                return Ok(json!({ "pending": true, "terminal": addr }));
            }
            return Err("This terminal already has a pane change in progress.".into());
        }
        let (space, machine, create_space) = if let Some(wt_path) = wt_path {
            if let Some(sp) = self.model.spaces.iter().find(|pr| self.space_matches_worktree(pr, &wt_path, addr_machine)) {
                (sp.id.clone(), sp.machine.clone(), None)
            } else {
                let space = self.model.fresh_id_on("sp", addr_machine);
                (space, addr_machine.map(str::to_owned), Some((wt_path, addr_machine.map(str::to_owned))))
            }
        } else if !is_acp {
            let space = self.model.active_space().map(|p| p.id.clone()).ok_or("no space to keep it in")?;
            let machine = self.machine_of(&space);
            if addr_machine != machine.as_deref() {
                return Err("A terminal's pane must stay on its own machine.".into());
            }
            (space, machine, None)
        } else {
            let rec = &self.live[addr];
            let active_same = self.model.active_space().filter(|p| p.machine.as_deref() == addr_machine);
            let matched = self.model.spaces.iter().find(|pr| self.acp_matches_space(addr, rec, pr));
            let same_machine = self.model.spaces.iter().find(|pr| pr.machine.as_deref() == addr_machine);
            let chosen = if matches!(place, "right" | "down") {
                active_same.or(matched).or(same_machine)
            } else {
                matched.or(active_same).or(same_machine)
            };
            if let Some(sp) = chosen {
                (sp.id.clone(), sp.machine.clone(), None)
            } else {
                let space = self.model.fresh_id_on("sp", addr_machine);
                let folder = self.cwds.get(addr).cloned()
                    .or_else(|| rec.get("cwd").and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_else(|| if addr_machine.is_some() { String::new() } else { home() });
                (space, addr_machine.map(str::to_owned), Some((folder, addr_machine.map(str::to_owned))))
            }
        };
        self.require_document(machine.as_deref())?;
        let target = matches!(place, "right" | "down")
            .then(|| self.model.focused_pane())
            .flatten()
            .filter(|p| self.model.space_of(p).is_some_and(|s| s.id == space));
        let ctx = target.as_deref().and_then(|p| self.model.tab_of(p)).map(|t| t.id.clone())
            .unwrap_or_else(|| self.model.fresh_id_on("tb", machine.as_deref()));
        let pane = self.model.fresh_id_on("p", machine.as_deref());
        let rec = &self.live[addr];
        let name = rec["name"].as_str().filter(|s| !s.is_empty()).map(str::to_owned)
            .or_else(|| is_acp.then(|| rec["title"].as_str().filter(|s| !s.is_empty())).flatten().map(str::to_owned))
            .or_else(|| self.model.agents.get(addr).and_then(|a| a.app.as_deref()).map(crate::programs::name))
            .or_else(|| is_acp.then(|| rec["agent"].as_str().filter(|s| !s.is_empty()).map(crate::programs::name)).flatten())
            .unwrap_or_else(|| addr.to_owned());
        let (address, place) = (addr.to_owned(), place.to_owned());
        let reply = json!({"pending":true,"terminal":addr,"pane":pane,"tab":ctx});
        self.start_terminal_change(address, TerminalChange::Keep {
            space, tab: ctx, pane, name, target, axis: if place == "right" { Axis::Row } else { Axis::Column }, create_space, focus,
        }, window, cx);
        Ok(reply)
    }

    /// Parking changes labels/keep in the background; only a successful reply removes the pane.
    fn park_terminal(&mut self, addr: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<Value, String> {
        self.require_document(addr.split_once('/').map(|(m, _)| m))?;
        if self.changing_terminals.contains(addr) { return Err("This terminal already has a pane change in progress.".into()); }
        self.start_terminal_change(addr.to_owned(), TerminalChange::Park, window, cx);
        Ok(json!({"pending":true,"terminal":addr}))
    }

    fn start_terminal_change(&mut self, address: String, change: TerminalChange, window: &mut Window, cx: &mut Context<Self>) {
        let machine = address.split_once('/').map(|(m, _)| m);
        let (keep, parked, labels) = match &change {
            TerminalChange::Keep { space, tab, .. } => (true, "", json!({
                "herdr.space":crate::mirror::local(space, machine), "herdr.tab":crate::mirror::local(tab, machine), "herdr.parked":"",
            })),
            TerminalChange::Park => (false, "1", json!({"herdr.space":"","herdr.tab":"","herdr.parked":"1"})),
        };
        if let Some(labels_obj) = self.live.get_mut(&address).and_then(|r| r.get_mut("labels")).and_then(Value::as_object_mut) {
            labels_obj.insert("herdr.parked".into(), json!(parked));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.changing_terminals.insert(address.clone());
        self.pending_terminals.insert(address.clone(), PendingTerminalChange { change, reply: rx });
        let core = self.core.clone();
        let addr = address.clone();
        let is_acp = is_acp_addr(&addr);
        // The drain must not need the foreground continuation to start this work.
        let job = cx.background_executor().spawn(async move {
            let result = (|| {
                if is_acp {
                    core.call("agent.label", json!({"agent": addr, "labels": labels}))?;
                    core.call("agent.keep", json!({"agent": addr, "keep": keep}))?;
                } else {
                    core.call("terminal.label", json!({"terminal":addr,"labels":labels}))?;
                    core.call("terminal.keep", json!({"terminal":addr,"keep":keep}))?;
                }
                anyhow::Ok(())
            })().map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
        });
        cx.spawn_in(window, async move |this, cx| {
            job.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.draining { return; }
                let Some(pending) = this.pending_terminals.remove(&address) else { return };
                let result = pending.reply.try_recv().unwrap_or_else(|e| Err(format!("terminal change reply missing: {e}")));
                this.apply_terminal_change(address, pending.change, result, window, cx);
            });
        }).detach();
    }

    fn apply_terminal_change(&mut self, address: String, change: TerminalChange, result: Result<(), String>, window: &mut Window, cx: &mut Context<Self>) {
        self.changing_terminals.remove(&address);
        if let Err(error) = result {
            tracing::error!("terminal change for {address} failed: {error}");
            if !self.draining { self.layout_notice("Could not change terminal pane", &error, window, cx); }
            return;
        }
        let is_acp = is_acp_addr(&address);
        match change {
            TerminalChange::Keep { space, tab, pane, name, target, axis, create_space, focus } => {
                if !self.live.contains_key(&address) { return; }
                if self.model.space(&space).is_none() {
                    if let Some((folder, machine)) = create_space {
                        let block = if is_acp { Self::acp_pane_node(&pane, &address) } else { model::pane(&pane, &address) };
                        let tab_obj = Tab::with_pane(&tab, &name, block);
                        if focus {
                            self.model.add_space(&space, "", &folder, tab_obj);
                        } else {
                            self.model.add_space_unshown(&space, "", &folder, tab_obj);
                        }
                        if let Some(p) = self.model.space_mut(&space) {
                            p.machine = machine;
                        }
                    } else {
                        if let Err(error) = self.park_terminal(&address, window, cx) {
                            tracing::error!("cannot undo pane placement into closed space: {error}");
                        }
                        return;
                    }
                } else if self.model.any_pane_of(&address).is_none() {
                    let destination = target.filter(|p| self.model.tab_of(p).is_some_and(|t| t.id == tab))
                        .or_else(|| self.model.tab(&tab).and_then(|t| t.root.panes().first().map(|p| p.id.clone())));
                    let block = if is_acp { Self::acp_pane_node(&pane, &address) } else { model::pane(&pane, &address) };
                    if let Some(target) = destination {
                        self.model.split_with(&target, axis, block);
                    } else if focus {
                        self.model.add_tab(&space, Tab::with_pane(&tab, &name, block));
                    } else {
                        self.model.add_tab_unselected(&space, Tab::with_pane(&tab, &name, block));
                    }
                }
                if focus {
                    if is_acp {
                        self.focus_mounted_acp(&address, window, cx);
                    } else if let Some(pane) = self.model.pane_of(&address) {
                        self.model.focus_pane(&pane);
                    }
                }
            }
            TerminalChange::Park => {
                if let Some(pane) = self.model.any_pane_of(&address) { self.model.remove_pane(&pane); }
            }
        }
        if !self.draining {
            self.sync(window, cx);
            self.flush_documents(false, window, cx);
        }
    }

    /// Kills terminals and their sub-agents, all the way down. Core keeps a
    /// sub-agent running when its parent dies; closing one here means done with
    /// its whole tree. Kept sub-agents (given a tab) are the person's: spared.
    fn kill(&mut self, addresses: &[String], cx: &mut Context<Self>) {
        let mut doomed: Vec<String> = addresses.to_vec();
        let mut i = 0;
        while i < doomed.len() {
            let parent = doomed[i].clone();
            for (addr, rec) in &self.live {
                let child = rec.get("parent").and_then(Value::as_str) == Some(parent.as_str());
                let kept = rec.get("labels").and_then(|l| l.get("herdr.space")).and_then(Value::as_str).is_some_and(|w| !w.is_empty());
                if child && !kept && !doomed.contains(addr) {
                    doomed.push(addr.clone());
                }
            }
            i += 1;
        }
        self.closing_terminals.extend(doomed.iter().cloned().map(|a| (a, false)));
        if self.peeked_agent.as_ref().is_some_and(|p| doomed.contains(p)) {
            self.peeked_agent = None;
        }
        if mods::publish_terminals(self.ordered_live_values()) {
            self.refresh_snapshot_mods(cx);
        }
        // Off the UI thread: a kill waits for the program to go, and for slow machines.
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            for address in doomed {
                let core = core.clone();
                let addr = address.clone();
                let result = cx.background_executor().spawn(async move {
                    if is_acp_addr(&addr) {
                        core.call("agent.kill", json!({ "agent": addr }))
                    } else {
                        core.call("terminal.kill", json!({ "terminal": addr }))
                    }
                }).await;
                let _ = this.update(cx, |this, _| {
                    if result.is_ok() && this.live.contains_key(&address) {
                        // The live snapshot may still predate the kill's reply.
                        this.closing_terminals.insert(address, true);
                    } else {
                        this.closing_terminals.remove(&address);
                    }
                });
            }
        }).detach();
    }

    /// Terminates and permanently removes the given sessions on core (`kill` + `rm`),
    /// ensuring any associated fork worktree directory, Git branch, and matching worktree space
    /// are completely cleaned up.
    fn kill_and_remove(&mut self, addresses: &[String], cx: &mut Context<Self>) {
        if addresses.is_empty() {
            return;
        }
        let mut doomed: Vec<String> = addresses.to_vec();
        let mut i = 0;
        while i < doomed.len() {
            let parent = doomed[i].clone();
            for (addr, rec) in &self.live {
                let child = rec.get("parent").and_then(Value::as_str) == Some(parent.as_str());
                let kept = rec.get("labels").and_then(|l| l.get("herdr.space")).and_then(Value::as_str).is_some_and(|w| !w.is_empty());
                if child && !kept && !doomed.contains(addr) {
                    doomed.push(addr.clone());
                }
            }
            i += 1;
        }
        let wt_targets: Vec<(Option<String>, String)> = doomed
            .iter()
            .filter_map(|addr| {
                let (wt_path, _, _) = self.fork_worktree_info(addr)?;
                let machine = addr.split_once('/').map(|(m, _)| m.to_owned());
                Some((machine, wt_path))
            })
            .collect();
        let pre_kill_shells = self.close_worktree_spaces_for(&wt_targets, &doomed, cx);
        for addr in &doomed {
            self.ended_acp.remove(addr);
        }
        self.closing_terminals.extend(pre_kill_shells.iter().cloned().map(|a| (a, false)));
        self.closing_terminals.extend(doomed.iter().cloned().map(|a| (a, false)));
        if self.peeked_agent.as_ref().is_some_and(|p| doomed.contains(p)) {
            self.peeked_agent = None;
        }
        let mut trash_list = mods::shared("agents.trash")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let trash_before = trash_list.len();
        trash_list.retain(|v| v.get("address").and_then(Value::as_str).is_none_or(|a| !doomed.iter().any(|d| d == a)));
        if trash_list.len() != trash_before {
            self.save_trash_list(trash_list, cx);
        }
        if mods::publish_terminals(self.ordered_live_values()) {
            self.refresh_snapshot_mods(cx);
        }
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            // Kill any shells sitting inside worktree spaces first so the folder isn't locked during `rm`.
            for shell_addr in pre_kill_shells {
                let core = core.clone();
                let addr = shell_addr.clone();
                let result = cx.background_executor().spawn(async move {
                    if is_acp_addr(&addr) {
                        core.call("agent.kill", json!({ "agent": addr }))
                    } else {
                        core.call("terminal.kill", json!({ "terminal": addr }))
                    }
                }).await;
                let _ = this.update(cx, |this, _| {
                    if result.is_ok() && this.live.contains_key(&shell_addr) {
                        this.closing_terminals.insert(shell_addr, true);
                    } else {
                        this.closing_terminals.remove(&shell_addr);
                    }
                });
            }
            for address in doomed {
                let core = core.clone();
                let addr = address.clone();
                let result = cx.background_executor().spawn(async move {
                    if is_acp_addr(&addr) {
                        let _ = core.call("agent.kill", json!({ "agent": addr }));
                        core.call("agent.rm", json!({ "agent": addr }))
                    } else {
                        let _ = core.call("terminal.kill", json!({ "terminal": addr }));
                        core.call("terminal.rm", json!({ "terminal": addr }))
                    }
                }).await;
                let _ = this.update(cx, |this, _| {
                    if result.is_ok() && this.live.contains_key(&address) {
                        this.closing_terminals.insert(address, true);
                    } else {
                        this.closing_terminals.remove(&address);
                    }
                });
            }
        }).detach();
    }

    /// Closes any dedicated worktree spaces matching `wt_targets`, trashing unrelated live agents
    /// and returning plain shell PTYs to terminate before core deletes the worktree directories.
    fn close_worktree_spaces_for(
        &mut self,
        wt_targets: &[(Option<String>, String)],
        doomed: &[String],
        cx: &mut Context<Self>,
    ) -> Vec<String> {
        if wt_targets.is_empty() {
            return Vec::new();
        }
        let space_ids: Vec<String> = self
            .model
            .spaces
            .iter()
            .filter(|sp| {
                wt_targets
                    .iter()
                    .any(|(machine, wt_path)| self.space_matches_worktree(sp, wt_path, machine.as_deref()))
            })
            .map(|sp| sp.id.clone())
            .collect();
        let mut shells_to_kill = Vec::new();
        for sp_id in space_ids {
            let addrs = self.model.remove_space(&sp_id);
            for addr in addrs {
                if doomed.iter().any(|d| d == &addr) {
                    continue;
                }
                if is_acp_addr(&addr) || self.is_agent_pty(&addr) {
                    if let Some(rec) = self.live.get(&addr).or_else(|| self.ended_acp.get(&addr)).cloned()
                        && !is_trashed_record(&rec)
                    {
                        let snap = self.trash_snapshot(&addr, &rec);
                        self.trash_detached(&addr, snap, cx);
                    }
                } else if self.live.contains_key(&addr) && !shells_to_kill.contains(&addr) {
                    shells_to_kill.push(addr);
                }
            }
        }
        shells_to_kill
    }

    fn extract_worktree_info(rec: &Value) -> Option<(String, Option<String>, bool)> {
        let lin = rec.get("lineage")?;
        let wt_path = lin.get("worktree_path").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())?.to_owned();
        let wt_branch = lin.get("worktree_branch").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        let merged = lin.get("worktree_merged").and_then(Value::as_bool).unwrap_or(false);
        Some((wt_path, wt_branch, merged))
    }

    /// Returns `(worktree_path, worktree_branch, worktree_merged)` if `addr` is a forked agent/terminal
    /// owning an isolated Git worktree (checking live, ended ACP, and recycle-bin records).
    fn fork_worktree_info(&self, addr: &str) -> Option<(String, Option<String>, bool)> {
        if let Some(rec) = self.live.get(addr).or_else(|| self.ended_acp.get(addr))
            && let Some(info) = Self::extract_worktree_info(rec)
        {
            return Some(info);
        }
        let trash = mods::shared("agents.trash")?;
        let entry = trash
            .as_array()?
            .iter()
            .find(|v| v.get("address").and_then(Value::as_str) == Some(addr))?;
        Self::extract_worktree_info(entry)
    }

    fn parse_worktree_blocked_error(err_text: &str) -> Option<Value> {
        let marker = "WORKTREE_BLOCKED:";
        let idx = err_text.find(marker)?;
        let rest = &err_text[idx + marker.len()..];
        let mut de = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
        match de.next() {
            Some(Ok(v)) if v.is_object() => Some(v),
            _ => None,
        }
    }

    fn open_worktree_blocked_dialog(&mut self, source: &str, info: &Value, window: &mut Window, cx: &mut Context<Self>) {
        let zh = settings::get().language == settings::Language::ZhCn;
        let code = info.get("code").and_then(Value::as_str).unwrap_or("unknown");
        let cwd = info.get("cwd").and_then(Value::as_str).unwrap_or("").trim();
        let op = info.get("op").and_then(Value::as_str).unwrap_or("git");
        let files: Vec<String> = info
            .get("files")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        let total_dirty = info.get("total_dirty").and_then(Value::as_u64).unwrap_or(files.len() as u64);
        let source_name = self.worktree_agent_display_name(source, None);
        let target_label = if source_name == source {
            source.to_owned()
        } else {
            format!("{source_name} ({source})")
        };

        let op_zh = match op {
            "merge" => "合并（merge）",
            "rebase" => "变基（rebase）",
            "cherry-pick" => "拣选（cherry-pick）",
            "revert" => "还原（revert）",
            "bisect" => "二分排查（bisect）",
            "conflict" => "冲突合并",
            other => other,
        };

        let title = if zh { "无法创建 Worktree 分叉" } else { "Cannot Create Worktree Fork" };

        let (detail, fix_prompt): (String, Option<String>) = match code {
            "not_git_repo" => (
                if zh {
                    format!(
                        "原因：当前工作目录尚未初始化 Git 仓库。\n工作目录：{cwd}\n创建独立 Worktree 分叉前，需要先初始化 Git 仓库并完成至少一次初始提交。你可以点击下方按钮，让当前 Agent（{target_label}）帮你处理。"
                    )
                } else {
                    format!(
                        "Reason: the current working directory is not a Git repository.\nDirectory: {cwd}\nCreating an isolated Worktree fork requires initializing Git and making at least one initial commit. Click below to ask the current agent ({target_label}) to set it up for you."
                    )
                },
                Some(if zh {
                    format!(
                        "我需要基于当前工作目录（{cwd}）创建独立的 Git Worktree 分叉，但当前目录尚未初始化 Git 仓库。请帮我在当前目录执行 git init，按需添加合理的 .gitignore（忽略临时缓存与构建产物），并将现有代码文件做一次初始提交（initial commit），完成后告诉我以便我重新创建 Worktree 分叉。"
                    )
                } else {
                    format!(
                        "I want to create an isolated Git worktree fork from the current directory ({cwd}), but it is not a Git repository yet. Please run `git init`, add a sensible `.gitignore` for build/cache artifacts, and create an initial commit with the existing files. Let me know once it's ready so I can retry creating the Worktree fork."
                    )
                }),
            ),
            "no_commits" => (
                if zh {
                    format!(
                        "原因：当前目录虽然已执行 git init，但还没有任何提交记录（No commits yet）。\n工作目录：{cwd}\nGit Worktree 需要基于至少一个已有提交（HEAD）才能创建独立分支。你可以点击下方按钮，让当前 Agent（{target_label}）帮你完成初始提交。"
                    )
                } else {
                    format!(
                        "Reason: this directory was initialized with `git init`, but has no commits yet.\nDirectory: {cwd}\nGit worktree requires at least one existing commit (HEAD) to branch from. Click below to ask the current agent ({target_label}) to make the initial commit."
                    )
                },
                Some(if zh {
                    format!(
                        "我需要基于当前工作目录（{cwd}）创建独立的 Git Worktree 分叉，但当前 Git 仓库仅初始化了，还没有任何提交记录（No commits yet）。请帮我检查目录中的文件，通过 .gitignore 合理忽略临时缓存文件（如 __pycache__/、node_modules/ 等），然后完成一次初始提交（initial commit），完成后告诉我以便我重新创建 Worktree 分叉。"
                    )
                } else {
                    format!(
                        "I want to create an isolated Git worktree fork from the current directory ({cwd}), but this Git repository has no commits yet. Please review the files, ignore temporary cache artifacts via `.gitignore` (such as `__pycache__/`, `node_modules/`), and make an initial commit. Let me know once it's ready so I can retry creating the Worktree fork."
                    )
                }),
            ),
            "in_progress_op" => (
                if zh {
                    format!(
                        "原因：当前 Git 仓库存在未完成的{op_zh}操作或未解决的合并冲突。\n工作目录：{cwd}\n请先完成或中止当前 Git 操作并保持工作区干净，再创建 Worktree 分叉。你可以点击下方按钮，让当前 Agent（{target_label}）帮你处理。"
                    )
                } else {
                    format!(
                        "Reason: the current Git repository has an unfinished {op} operation or unresolved conflicts.\nDirectory: {cwd}\nPlease finish or abort the in-progress Git operation and leave the working tree clean before creating a Worktree fork. Click below to ask the current agent ({target_label}) to resolve it."
                    )
                },
                Some(if zh {
                    format!(
                        "我需要基于当前工作目录（{cwd}）创建独立的 Git Worktree 分叉，但当前 Git 仓库存在未完成的{op_zh}操作或冲突。请帮我查看 git status，安全地完成或中止该操作并使工作区恢复干净状态，完成后告诉我以便我重新创建 Worktree 分叉。"
                    )
                } else {
                    format!(
                        "I want to create an isolated Git worktree fork from the current directory ({cwd}), but the Git repository has an unfinished {op} operation or unresolved conflicts. Please inspect `git status`, safely finish or abort the operation so the working tree is clean, and let me know once ready."
                    )
                }),
            ),
            "dirty_worktree" => {
                let preview = if files.is_empty() {
                    String::new()
                } else if total_dirty as usize > files.len() {
                    format!("{} ...", files.join(", "))
                } else {
                    files.join(", ")
                };
                (
                    if zh {
                        format!(
                            "原因：当前 Git 仓库存在 {total_dirty} 项未提交的修改或未跟踪文件。\n工作目录：{cwd}\n为避免主干与 Worktree 仓库提交历史不一致，请先提交（或清理/忽略）这些未提交改动后再创建 Worktree 分叉。你可以点击下方按钮，让当前 Agent（{target_label}）帮你整理并提交。"
                        )
                    } else {
                        format!(
                            "Reason: the current Git repository has {total_dirty} uncommitted change(s) or untracked file(s).\nDirectory: {cwd}\nTo keep Git history consistent between main and the forked worktree, please commit (or clean/ignore) these changes first. Click below to ask the current agent ({target_label}) to handle it."
                        )
                    },
                    Some(if zh {
                        format!(
                            "我需要基于当前工作目录（{cwd}）创建独立的 Git Worktree 分叉，但当前 Git 仓库还有未提交的改动（{preview}）。请帮我检查 git status 和 diff，合理配置 .gitignore（忽略临时缓存文件）并将需要保留的改动提交到当前分支，使工作区保持干净后告诉我，以便我重新创建 Worktree 分叉。"
                        )
                    } else {
                        format!(
                            "I want to create an isolated Git worktree fork from the current directory ({cwd}), but the Git repository has uncommitted changes ({preview}). Please inspect `git status` and diff, ignore any temporary cache artifacts via `.gitignore`, and commit the intended changes so the working tree is clean. Let me know once ready."
                        )
                    }),
                )
            }
            "aosp_harness" => (
                if zh {
                    format!(
                        "原因：当前工作目录属于 AOSP Harness 工作区，已禁用 Git Worktree 隔离。\n工作目录：{cwd}\n请取消勾选「独立 Git Worktree 隔离」，改为在共享工作区中分叉。"
                    )
                } else {
                    format!(
                        "Reason: worktree isolation is disabled for AOSP harness workspaces.\nDirectory: {cwd}\nUncheck \"Isolated Git Worktree\" to fork in the shared workspace instead."
                    )
                },
                None,
            ),
            _ => (
                if zh {
                    format!("原因：当前工作目录（{cwd}）暂不满足创建 Git Worktree 的条件。")
                } else {
                    format!("Reason: the current directory ({cwd}) is not ready for creating a Git worktree.")
                },
                None,
            ),
        };

        let items: Vec<Value> = files
            .iter()
            .map(|entry| {
                let trimmed = entry.trim();
                json!({ "name": trimmed })
            })
            .collect();

        let mut props = json!({
            "title": title,
            "detail": detail,
            "items": items,
            "cancelLabel": if zh { "关闭" } else { "Close" },
        });
        if let Some(prompt_text) = fix_prompt {
            props["confirmLabel"] = json!(if zh {
                format!("发消息让 {source} 处理")
            } else {
                format!("Ask {source} to Fix")
            });
            props["command"] = json!({
                "name": "agent.send",
                "args": {
                    "agent": source,
                    "text": prompt_text,
                    "focus": true,
                }
            });
        } else {
            props["confirmLabel"] = json!(if zh { "知道了" } else { "Got It" });
        }

        self.open_overlay(
            "worktree-blocked".into(),
            json!({
                "mod": "confirm",
                "kind": "dialog",
                "width": 520,
                "props": props,
            }),
            window,
            cx,
        );
    }

    fn worktree_agent_display_name(&self, addr: &str, entry: Option<&Value>) -> String {
        let rec = self.live.get(addr).or_else(|| self.ended_acp.get(addr));
        let pick = |v: Option<&Value>, ptr: &str| {
            v.and_then(|x| x.pointer(ptr))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        pick(entry, "/custom_name")
            .or_else(|| pick(rec, "/labels/herdr.name"))
            .or_else(|| pick(entry, "/name"))
            .or_else(|| pick(rec, "/name"))
            .or_else(|| pick(entry, "/tab_name"))
            .or_else(|| pick(entry, "/title"))
            .or_else(|| pick(rec, "/title"))
            .unwrap_or_else(|| addr.to_owned())
    }

    fn worktree_confirm_item(
        &self,
        addr: &str,
        entry: Option<&Value>,
        wt_path: &str,
        wt_branch: Option<&str>,
    ) -> Value {
        let label = self.worktree_agent_display_name(addr, entry);
        let space_name = entry
            .and_then(|e| e.get("space_name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                std::path::Path::new(wt_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            });
        let live = !self.closing_terminals.contains_key(addr)
            && self
                .live
                .get(addr)
                .is_some_and(|r| r.get("state").and_then(Value::as_str) == Some("live"));
        json!({
            "address": addr,
            "name": label,
            "branch": wt_branch.unwrap_or("worktree"),
            "path": wt_path,
            "space_name": space_name,
            "live": live,
        })
    }

    /// Collects all worktree-type forked agents currently in the recycle bin as structured
    /// confirmation items (`address`, `name`, `branch`, `path`, `space_name`, `live`).
    fn trashed_fork_worktrees(&self) -> Vec<Value> {
        let list = mods::shared("agents.trash")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for entry in &list {
            let Some(addr) = entry.get("address").and_then(Value::as_str).map(str::to_owned) else {
                continue;
            };
            if self.closing_terminals.contains_key(&addr) || !seen.insert(addr.clone()) {
                continue;
            }
            if let Some((wt_path, wt_branch, _)) = self.fork_worktree_info(&addr) {
                out.push(self.worktree_confirm_item(&addr, Some(entry), &wt_path, wt_branch.as_deref()));
            }
        }
        let mut extra_addrs: Vec<String> = self
            .live
            .iter()
            .filter(|(addr, rec)| {
                !self.closing_terminals.contains_key(*addr)
                    && is_trashed_record(rec)
                    && !seen.contains(*addr)
            })
            .map(|(addr, _)| addr.clone())
            .collect();
        extra_addrs.sort();
        for addr in extra_addrs {
            if let Some((wt_path, wt_branch, _)) = self.fork_worktree_info(&addr) {
                out.push(self.worktree_confirm_item(&addr, None, &wt_path, wt_branch.as_deref()));
            }
        }
        out
    }

    /// Prompts a single confirmation dialog reminding the user that permanently deleting this
    /// worktree-type forked agent from the recycle bin will also remove its entire worktree directory.
    fn prompt_purge_fork_worktree(
        &mut self,
        addr: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((wt_path, wt_branch, _)) = self.fork_worktree_info(addr) else {
            return;
        };
        let entry = mods::shared("agents.trash")
            .and_then(|v| v.as_array().cloned())
            .and_then(|list| list.into_iter().find(|e| e.get("address").and_then(Value::as_str) == Some(addr)));
        let item = self.worktree_confirm_item(addr, entry.as_ref(), &wt_path, wt_branch.as_deref());
        let zh = settings::get().language == settings::Language::ZhCn;
        let title = if zh {
            "删除分叉 Agent 并移除 Worktree？"
        } else {
            "Delete forked agent & remove Worktree?"
        };
        let detail = if zh {
            "彻底删除该分叉 Agent 后，将会同时移除以下整个 Worktree 目录及其分支，目录内的所有文件改动将无法恢复："
        } else {
            "Permanently deleting this forked agent will also remove the entire Worktree directory and its branch below, and all files inside it will be lost:"
        };
        let spec = json!({
            "mod": "confirm",
            "kind": "dialog",
            "width": 480,
            "props": {
                "title": title,
                "detail": detail,
                "items": [item],
                "confirmLabel": if zh { "确认删除" } else { "Delete & Remove Worktree" },
                "confirmDanger": true,
                "command": {
                    "name": "agent.purge",
                    "args": { "terminal": addr, "confirmed": true }
                }
            }
        });
        self.open_overlay(mods::new_overlay_id(), spec, window, cx);
    }

    /// Prompts a single aggregated confirmation dialog when emptying the recycle bin while it
    /// contains one or more worktree-type forked agents.
    fn prompt_empty_trash_fork_worktrees(
        &mut self,
        wt_items: &[Value],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if wt_items.is_empty() {
            return;
        }
        let zh = settings::get().language == settings::Language::ZhCn;
        let count = wt_items.len();
        let title = if zh {
            if count == 1 {
                "清空回收站并移除 Worktree？".to_owned()
            } else {
                format!("清空回收站并移除 {count} 个 Worktree？")
            }
        } else if count == 1 {
            "Empty recycle bin & remove Worktree?".to_owned()
        } else {
            format!("Empty recycle bin & remove {count} Worktrees?")
        };
        let detail = if zh {
            format!(
                "回收站中包含 {count} 个 Worktree 类型的分叉 Agent，清空回收站后将会同时移除以下整个 Worktree 目录及其分支，目录内的所有文件改动将无法恢复："
            )
        } else {
            format!(
                "The recycle bin contains {count} forked agent(s) with isolated Worktrees. Emptying the recycle bin will also permanently remove the entire Worktree directories and branches below:"
            )
        };
        let spec = json!({
            "mod": "confirm",
            "kind": "dialog",
            "width": 520,
            "props": {
                "title": title,
                "detail": detail,
                "items": wt_items,
                "confirmLabel": if zh { "确认清空并删除" } else { "Empty & Remove Worktrees" },
                "confirmDanger": true,
                "command": {
                    "name": "agent.empty_trash",
                    "args": { "confirmed": true }
                }
            }
        });
        self.open_overlay(mods::new_overlay_id(), spec, window, cx);
    }

    /// Moves mounted ACP or CLI agents back to the background without killing or trashing them:
    /// clears `herdr.space`/`herdr.tab` labels, marks `herdr.parked`, and turns off `keep`.
    fn park_agent_addresses(&mut self, addresses: &[String], cx: &mut Context<Self>) {
        for addr in addresses {
            self.ended_acp.remove(addr);
        }
        let targets: Vec<(String, bool)> = addresses
            .iter()
            .filter(|a| self.live.contains_key(*a) && (is_acp_addr(a) || self.is_agent_pty(a)))
            .map(|a| (a.clone(), is_acp_addr(a)))
            .collect();
        if targets.is_empty() {
            return;
        }
        for (addr, _) in &targets {
            if self.peeked_agent.as_deref() == Some(addr.as_str()) {
                self.peeked_agent = None;
            }
            if let Some(obj) = self.live.get_mut(addr).and_then(Value::as_object_mut) {
                obj.insert("keep".into(), json!(false));
                let labels = obj.entry("labels").or_insert_with(|| json!({}));
                if let Some(labels) = labels.as_object_mut() {
                    labels.insert("herdr.space".into(), json!(""));
                    labels.insert("herdr.tab".into(), json!(""));
                    labels.insert("herdr.parked".into(), json!("1"));
                }
            }
        }
        if mods::publish_terminals(self.ordered_live_values()) {
            self.refresh_snapshot_mods(cx);
        }
        let core = self.core.clone();
        cx.spawn(async move |_, cx| {
            for (addr, is_acp) in targets {
                let core = core.clone();
                cx.background_executor()
                    .spawn(async move {
                        let labels = json!({ "herdr.space": "", "herdr.tab": "", "herdr.parked": "1" });
                        if is_acp {
                            if let Err(e) = core.call("agent.label", json!({ "agent": addr, "labels": labels })) {
                                tracing::warn!("park agent label {addr}: {e:#}");
                            }
                            if let Err(e) = core.call("agent.keep", json!({ "agent": addr, "keep": false })) {
                                tracing::warn!("park agent keep {addr}: {e:#}");
                            }
                        } else {
                            if let Err(e) = core.call("terminal.label", json!({ "terminal": addr, "labels": labels })) {
                                tracing::warn!("park terminal label {addr}: {e:#}");
                            }
                            if let Err(e) = core.call("terminal.keep", json!({ "terminal": addr, "keep": false })) {
                                tracing::warn!("park terminal keep {addr}: {e:#}");
                            }
                        }
                    })
                    .await;
            }
        })
        .detach();
    }

    fn is_agent_pty(&self, addr: &str) -> bool {
        !is_acp_addr(addr)
            && self.live.contains_key(addr)
            && (self.model.status(addr).is_some()
                || self.program(addr).get("agent").and_then(Value::as_bool) == Some(true))
    }

    fn trash_snapshot(&self, addr: &str, rec: &Value) -> Value {
        let machine = addr.split_once('/').map(|(m, _)| m.to_owned());
        let prog = self.program(addr);
        let acp = is_acp_addr(addr) || rec.get("acp").and_then(Value::as_bool) == Some(true);
        let custom_name = rec
            .pointer("/labels/herdr.name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let name = rec
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let title = rec
            .get("title")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let agent = rec
            .get("agent")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| self.model.agents.get(addr).and_then(|a| a.app.clone()))
            .or_else(|| prog.get("key").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned));
        let argv = rec.get("argv").cloned().unwrap_or_else(|| json!([]));
        let cwd = self
            .cwds
            .get(addr)
            .cloned()
            .or_else(|| rec.get("cwd").and_then(Value::as_str).map(str::to_owned));
        let home_dir = home();
        let pane_sp = self.model.any_pane_of(addr).and_then(|p| {
            let sp = self.model.space_of(&p)?;
            let tb = self.model.tab_of(&p)?;
            let tab_name = (!tb.name.is_empty()).then(|| tb.name.clone());
            Some((sp.id.clone(), self.model.space_name(sp, &home_dir), tab_name))
        });
        let (space, space_name, tab_name) = if let Some((sid, sname, tname)) = pane_sp {
            (Some(sid), Some(sname), tname)
        } else {
            let raw_sp = rec
                .pointer("/labels/herdr.trash_space")
                .or_else(|| rec.pointer("/labels/herdr.space"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|s| crate::mirror::qualify(s, machine.as_deref()));
            let matched = raw_sp
                .as_deref()
                .and_then(|sid| self.model.space(sid))
                .or_else(|| self.model.spaces.iter().find(|pr| self.acp_matches_space(addr, rec, pr)));
            let sid = matched.map(|s| s.id.clone()).or(raw_sp);
            let sname = matched
                .map(|s| self.model.space_name(s, &home_dir))
                .or_else(|| cwd.as_deref().and_then(|c| std::path::Path::new(c).file_name().and_then(|n| n.to_str()).filter(|s| !s.is_empty())).map(str::to_owned))
                .or_else(|| machine.clone());
            let tname = rec
                .pointer("/labels/herdr.trash_name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            (sid, sname, tname)
        };
        let trashed_at = rec
            .pointer("/labels/herdr.trashed")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or_else(model::now_ms);
        let live = rec.get("state").and_then(Value::as_str) == Some("live");
        let lineage = rec.get("lineage").cloned().unwrap_or(Value::Null);
        json!({
            "address": addr,
            "acp": acp,
            "lineage": lineage,
            "name": name,
            "custom_name": custom_name,
            "title": title,
            "program": prog,
            "agent": agent,
            "argv": argv,
            "cwd": cwd,
            "machine": machine,
            "space": space,
            "space_name": space_name,
            "tab_name": tab_name,
            "trashed_at": trashed_at,
            "live": live,
        })
    }

    fn save_trash_list(&mut self, mut list: Vec<Value>, cx: &mut Context<Self>) {
        if list.len() > 30 {
            let overflow: Vec<String> = list
                .drain(30..)
                .filter_map(|v| v.get("address").and_then(Value::as_str).map(str::to_owned))
                .filter(|a| self.live.contains_key(a))
                .collect();
            if !overflow.is_empty() {
                self.kill(&overflow, cx);
            }
        }
        mods::set_shared("agents.trash".into(), Value::Array(list));
    }

    fn sync_trashed_agents(&self) {
        let prev = mods::shared("agents.trash");
        let mut list = prev
            .as_ref()
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut seen = HashSet::new();
        for item in &mut list {
            let Some(addr) = item.get("address").and_then(Value::as_str).map(str::to_owned) else {
                continue;
            };
            seen.insert(addr.clone());
            let live_now = !self.closing_terminals.contains_key(&addr)
                && self
                    .live
                    .get(&addr)
                    .is_some_and(|r| r.get("state").and_then(Value::as_str) == Some("live"));
            if let Some(obj) = item.as_object_mut() {
                obj.insert("live".into(), json!(live_now));
                if obj.get("lineage").is_none_or(Value::is_null)
                    && let Some(lin) = self.live.get(&addr).and_then(|r| r.get("lineage")).filter(|v| !v.is_null())
                {
                    obj.insert("lineage".into(), lin.clone());
                }
            }
        }
        let mut extras: Vec<(String, Value)> = self
            .live
            .iter()
            .filter(|(addr, rec)| {
                !self.closing_terminals.contains_key(*addr)
                    && is_trashed_record(rec)
                    && !seen.contains(*addr)
            })
            .map(|(addr, rec)| (addr.clone(), self.trash_snapshot(addr, rec)))
            .collect();
        extras.sort_by(|a, b| {
            b.1["trashed_at"]
                .as_u64()
                .cmp(&a.1["trashed_at"].as_u64())
                .then_with(|| a.0.cmp(&b.0))
        });
        for (_, snap) in extras {
            list.insert(0, snap);
        }
        list.truncate(30);
        if list.is_empty() && prev.is_none() {
            return;
        }
        let next = Value::Array(list);
        if prev.as_ref() != Some(&next) {
            mods::set_shared("agents.trash".into(), next);
        }
    }

    fn trash_detached(&mut self, addr: &str, entry: Value, cx: &mut Context<Self>) {
        self.ended_acp.remove(addr);
        if self.peeked_agent.as_deref() == Some(addr) {
            self.peeked_agent = None;
        }
        let is_acp = entry.get("acp").and_then(Value::as_bool) == Some(true) || is_acp_addr(addr);
        let trashed_at = entry
            .get("trashed_at")
            .and_then(Value::as_u64)
            .unwrap_or_else(model::now_ms)
            .to_string();
        let machine = addr.split_once('/').map(|(m, _)| m);
        let trash_space = entry
            .get("space")
            .and_then(Value::as_str)
            .map(|s| crate::mirror::local(s, machine).to_owned())
            .unwrap_or_default();
        let trash_name = entry
            .get("tab_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if self.live.contains_key(addr) {
            if let Some(obj) = self.live.get_mut(addr).and_then(Value::as_object_mut) {
                obj.insert("keep".into(), json!(false));
                let labels = obj.entry("labels").or_insert_with(|| json!({}));
                if let Some(labels) = labels.as_object_mut() {
                    labels.insert("herdr.space".into(), json!(""));
                    labels.insert("herdr.tab".into(), json!(""));
                    labels.insert("herdr.parked".into(), json!("1"));
                    labels.insert("herdr.trashed".into(), json!(trashed_at));
                    labels.insert("herdr.trash_space".into(), json!(trash_space));
                    labels.insert("herdr.trash_name".into(), json!(trash_name));
                }
            }
            let core = self.core.clone();
            let address = addr.to_owned();
            let labels = json!({
                "herdr.space": "",
                "herdr.tab": "",
                "herdr.parked": "1",
                "herdr.trashed": trashed_at,
                "herdr.trash_space": trash_space,
                "herdr.trash_name": trash_name,
            });
            cx.background_executor()
                .spawn(async move {
                    let (l_method, l_key, k_method) = if is_acp {
                        ("agent.label", "agent", "agent.keep")
                    } else {
                        ("terminal.label", "terminal", "terminal.keep")
                    };
                    if let Err(e) = core.call(l_method, json!({ l_key: address, "labels": labels })) {
                        tracing::warn!("trash label {address}: {e:#}");
                    }
                    if let Err(e) = core.call(k_method, json!({ l_key: address, "keep": false })) {
                        tracing::warn!("trash keep {address}: {e:#}");
                    }
                })
                .detach();
        }
        let mut list = mods::shared("agents.trash")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        list.retain(|v| v.get("address").and_then(Value::as_str) != Some(addr));
        list.insert(0, entry);
        self.save_trash_list(list, cx);
        if mods::publish_terminals(self.ordered_live_values()) {
            self.refresh_snapshot_mods(cx);
        }
    }

    fn trash_agent(&mut self, addr: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let Some(rec) = self.live.get(addr).or_else(|| self.ended_acp.get(addr)).cloned() else {
            return Err(format!("no running terminal {addr:?}"));
        };
        let entry = self.trash_snapshot(addr, &rec);
        if self.peeked_agent.as_deref() == Some(addr) {
            self.close_overlay("terminal-peek", window, cx);
        }
        if let Some(pane) = self.model.any_pane_of(addr) {
            let machine = self.model.space_of(&pane).and_then(|s| s.machine.as_deref());
            self.require_document(machine)?;
            if let Some(sp) = self.model.space_of(&pane)
                && sp.tabs.len() == 1
                && self.model.tab_of(&pane).is_some_and(|t| t.root.panes().len() == 1)
            {
                let space_id = sp.id.clone();
                let _ = self.new_tab(Some(space_id), "", None, None, None, None, true, window);
            }
            self.model.remove_pane(&pane);
        }
        self.trash_detached(addr, entry, cx);
        Ok(())
    }

    fn clear_trash_labels_for_side_chat(&mut self, addr: &str, cx: &mut Context<Self>) {
        let is_acp = is_acp_addr(addr);
        if let Some(obj) = self.live.get_mut(addr).and_then(Value::as_object_mut) {
            obj.insert("keep".into(), json!(false));
            let labels = obj.entry("labels").or_insert_with(|| json!({}));
            if let Some(labels) = labels.as_object_mut() {
                labels.insert("herdr.space".into(), json!(""));
                labels.insert("herdr.tab".into(), json!(""));
                labels.insert("herdr.parked".into(), json!("1"));
                labels.insert("herdr.trashed".into(), json!(""));
                labels.insert("herdr.trash_space".into(), json!(""));
                labels.insert("herdr.trash_name".into(), json!(""));
            }
        }
        let core = self.core.clone();
        let address = addr.to_owned();
        let labels = json!({
            "herdr.space": "",
            "herdr.tab": "",
            "herdr.parked": "1",
            "herdr.trashed": "",
            "herdr.trash_space": "",
            "herdr.trash_name": "",
        });
        cx.background_executor()
            .spawn(async move {
                let (l_method, l_key, k_method) = if is_acp {
                    ("agent.label", "agent", "agent.keep")
                } else {
                    ("terminal.label", "terminal", "terminal.keep")
                };
                let _ = core.call(l_method, json!({ l_key: address, "labels": labels }));
                let _ = core.call(k_method, json!({ l_key: address, "keep": false }));
            })
            .detach();
        if mods::publish_terminals(self.ordered_live_values()) {
            self.refresh_snapshot_mods(cx);
        }
    }

    fn clear_trash_labels_and_keep(&mut self, addr: &str, space_id: &str, tab_id: &str, on: Option<&str>, cx: &mut Context<Self>) {
        let is_acp = is_acp_addr(addr);
        let local_sp = crate::mirror::local(space_id, on).to_owned();
        let local_tb = crate::mirror::local(tab_id, on).to_owned();
        if let Some(obj) = self.live.get_mut(addr).and_then(Value::as_object_mut) {
            obj.insert("keep".into(), json!(true));
            let labels = obj.entry("labels").or_insert_with(|| json!({}));
            if let Some(labels) = labels.as_object_mut() {
                labels.insert("herdr.space".into(), json!(local_sp));
                labels.insert("herdr.tab".into(), json!(local_tb));
                labels.insert("herdr.parked".into(), json!(""));
                labels.insert("herdr.trashed".into(), json!(""));
                labels.insert("herdr.trash_space".into(), json!(""));
                labels.insert("herdr.trash_name".into(), json!(""));
            }
        }
        let core = self.core.clone();
        let address = addr.to_owned();
        let labels = json!({
            "herdr.space": local_sp,
            "herdr.tab": local_tb,
            "herdr.parked": "",
            "herdr.trashed": "",
            "herdr.trash_space": "",
            "herdr.trash_name": "",
        });
        cx.background_executor()
            .spawn(async move {
                let (l_method, l_key, k_method) = if is_acp {
                    ("agent.label", "agent", "agent.keep")
                } else {
                    ("terminal.label", "terminal", "terminal.keep")
                };
                let _ = core.call(l_method, json!({ l_key: address, "labels": labels }));
                let _ = core.call(k_method, json!({ l_key: address, "keep": true }));
            })
            .detach();
        if mods::publish_terminals(self.ordered_live_values()) {
            self.refresh_snapshot_mods(cx);
        }
    }

    fn normalize_agent_program_key(raw: &str) -> &str {
        match raw.trim() {
            "claude-code" => "claude",
            "cursor-agent" => "cursor",
            "gemini-cli" => "gemini",
            "qwen-code" => "qwen",
            "kiro-cli" => "kiro",
            "antigravity-cli" | "agy" => "antigravity",
            "oh-my-pi" => "omp",
            other => other,
        }
    }

    fn unwrap_shell_agent_argv(argv: Vec<String>) -> Vec<String> {
        if argv.len() > 4 && argv[0] == "sh" && argv[1] == "-c" && argv[3] == "sh" {
            return argv[4..].to_vec();
        }
        argv
    }

    fn restore_trashed_agent(&mut self, addr: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let mut list = mods::shared("agents.trash")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let pos = list.iter().position(|v| v.get("address").and_then(Value::as_str) == Some(addr));
        let entry = if let Some(idx) = pos {
            list[idx].clone()
        } else if let Some(rec) = self.live.get(addr).filter(|r| is_trashed_record(r)) {
            self.trash_snapshot(addr, rec)
        } else {
            return Err(format!("no trashed agent {addr:?}"));
        };
        let machine = entry
            .get("machine")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| addr.split_once('/').map(|(m, _)| m.to_owned()));
        let on = machine.as_deref();
        self.require_document(on)?;

        let wt_path = self.fork_worktree_info(addr).map(|(p, _, _)| p);
        let target_space = if let Some(ref wt) = wt_path {
            self.model
                .spaces
                .iter()
                .find(|sp| self.space_matches_worktree(sp, wt, on))
                .map(|sp| sp.id.clone())
        } else {
            entry
                .get("space")
                .and_then(Value::as_str)
                .filter(|s| self.model.space(s).is_some_and(|sp| sp.machine.as_deref() == on))
                .map(str::to_owned)
                .or_else(|| {
                    self.model
                        .active_space()
                        .filter(|sp| sp.machine.as_deref() == on)
                        .map(|sp| sp.id.clone())
                })
                .or_else(|| {
                    self.model
                        .spaces
                        .iter()
                        .find(|sp| sp.machine.as_deref() == on)
                        .map(|sp| sp.id.clone())
                })
        };

        let custom_name = entry
            .get("custom_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let is_acp = entry.get("acp").and_then(Value::as_bool) == Some(true) || is_acp_addr(addr);
        let tab_name = if is_acp {
            custom_name
                .clone()
                .or_else(|| entry.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned))
                .or_else(|| entry.get("tab_name").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned))
                .or_else(|| entry.get("title").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned))
                .unwrap_or_else(|| addr.to_owned())
        } else {
            custom_name.clone().unwrap_or_default()
        };
        let cwd = wt_path.clone().or_else(|| {
            entry
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        });
        let is_live = self
            .live
            .get(addr)
            .is_some_and(|r| r.get("state").and_then(Value::as_str) == Some("live"));
        let is_side_chat = self
            .live
            .get(addr)
            .and_then(|r| r.pointer("/lineage/kind"))
            .or_else(|| entry.pointer("/lineage/kind"))
            .and_then(Value::as_str)
            .map(|k| k.to_ascii_lowercase().replace('_', "-"))
            .as_deref()
            == Some("side-chat");

        if is_live && is_side_chat {
            self.clear_trash_labels_for_side_chat(addr, cx);
            if let Some(idx) = pos {
                list.remove(idx);
                self.save_trash_list(list, cx);
            }
            self.open_side_chat_panel(addr, window, cx);
            return Ok(());
        }
        if is_side_chat {
            let zh = settings::get().language == settings::Language::ZhCn;
            return Err(if zh {
                "已结束的旁路会话不能恢复为标签页，请在其父 Agent 的右侧旁路面板重新发起旁路讨论".into()
            } else {
                "exited side-chat cannot be restored as a tab; start a new side-chat from its parent agent".into()
            });
        }

        if is_live {
            let ctx_id = self.model.fresh_id_on("tb", on);
            let pane_id = self.model.fresh_id_on("p", on);
            let tab = if is_acp {
                Tab::with_pane(&ctx_id, &tab_name, Self::acp_pane_node(&pane_id, addr))
            } else {
                Tab::new(&ctx_id, &tab_name, &pane_id, addr)
            };
            let space_id = match target_space {
                Some(ref sp) => {
                    if !self.model.add_tab(sp, tab) {
                        return Err(format!("no space {sp:?}"));
                    }
                    self.model.select_tab(&ctx_id);
                    self.model.focus_pane(&pane_id);
                    sp.clone()
                }
                None => {
                    let sp = self.model.fresh_id_on("sp", on);
                    let space_label = if wt_path.is_some() {
                        ""
                    } else {
                        entry.get("space_name").and_then(Value::as_str).unwrap_or("")
                    };
                    let folder = cwd.unwrap_or_else(|| if on.is_some() { String::new() } else { home() });
                    self.model.add_space(&sp, space_label, &folder, tab);
                    if let Some(space) = self.model.space_mut(&sp) {
                        space.machine = machine.clone();
                    }
                    sp
                }
            };
            self.clear_trash_labels_and_keep(addr, &space_id, &ctx_id, on, cx);
            if is_acp {
                self.focus_mounted_acp(addr, window, cx);
            }
            self.focused_block = None;
            self.focused_pane = None;
            if let Some(idx) = pos {
                list.remove(idx);
                self.save_trash_list(list, cx);
            }
            return Ok(());
        }

        // Dead-process fallback: relaunch using stored preset/argv and metadata.
        let prog_key = entry
            .pointer("/program/key")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| entry.get("agent").and_then(Value::as_str).filter(|s| !s.is_empty()))
            .map(Self::normalize_agent_program_key)
            .unwrap_or_default()
            .to_owned();
        let raw_argv = entry
            .get("argv")
            .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
            .map(Self::unwrap_shell_agent_argv)
            .filter(|a| !a.is_empty());
        if is_acp {
            let is_builtin = matches!(
                prog_key.as_str(),
                "claude" | "codex" | "gemini" | "opencode" | "cursor" | "kimi" | "qwen" | "kiro" | "goose"
            );
            let mut params = json!({ "keep": true });
            if is_builtin {
                params["agent"] = json!(prog_key);
            } else if let Some(argv) = raw_argv {
                params["argv"] = json!(argv);
            } else {
                return Err("cannot restore exited ACP agent: missing preset/command".into());
            }
            if let Some(name) = custom_name
                .clone()
                .or_else(|| entry.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned))
            {
                params["name"] = json!(name);
            }
            if let Some(c) = cwd {
                params["cwd"] = json!(c);
            }
            if let Some(m) = on {
                params["on"] = json!(m);
            }
            let mut labels = serde_json::Map::new();
            labels.insert("herdr.parked".into(), json!(""));
            if let Some(ref sp) = target_space {
                labels.insert("herdr.space".into(), json!(crate::mirror::local(sp, on)));
            }
            if let Some(ref cname) = custom_name {
                labels.insert("herdr.name".into(), json!(cname));
            }
            params["labels"] = Value::Object(labels);
            let core = self.core.clone();
            let old_addr = addr.to_owned();
            let rm_addr = old_addr.clone();
            cx.spawn_in(window, async move |this, cx| {
                let res = cx
                    .background_executor()
                    .spawn(async move {
                        let out = core.call("agent.run", params)?;
                        let _ = core.call("agent.rm", json!({ "agent": rm_addr }));
                        Ok::<Value, anyhow::Error>(out)
                    })
                    .await;
                let _ = this.update_in(cx, |this, window, cx| match res {
                    Ok(_) => {
                        let mut cur = mods::shared("agents.trash")
                            .and_then(|v| v.as_array().cloned())
                            .unwrap_or_default();
                        let before = cur.len();
                        cur.retain(|v| v.get("address").and_then(Value::as_str) != Some(old_addr.as_str()));
                        if cur.len() != before {
                            this.save_trash_list(cur, cx);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("restore relaunch ACP agent: {e:#}");
                        this.command_error("agent.restore", &format!("{e:#}"), window, cx);
                    }
                });
            })
            .detach();
            return Ok(());
        }

        let preset_argv = crate::programs::startable()
            .into_iter()
            .find(|p| p["key"].as_str() == Some(prog_key.as_str()))
            .and_then(|p| serde_json::from_value::<Vec<String>>(p["run"].clone()).ok())
            .filter(|a| !a.is_empty());
        let run_argv = preset_argv.or(raw_argv);
        let created = if let Some(sp) = target_space {
            self.new_tab(Some(sp), &tab_name, cwd, run_argv, None, None, true, window)?
        } else {
            let sname = entry.get("space_name").and_then(Value::as_str).unwrap_or("");
            self.new_space(
                sname,
                cwd,
                (!tab_name.is_empty()).then_some(tab_name.as_str()),
                run_argv,
                None,
                machine,
                None,
                true,
                window,
            )?
        };
        if let (Some(new_addr), Some(cname)) = (created.get("terminal").and_then(Value::as_str), custom_name) {
            if let Some(obj) = self.live.get_mut(new_addr).and_then(Value::as_object_mut)
                && let Some(labels) = obj.entry("labels").or_insert_with(|| json!({})).as_object_mut()
            {
                labels.insert("herdr.name".into(), json!(cname));
            }
            Self::write_core_label(self.core.clone(), new_addr.to_owned(), "herdr.name", cname, cx);
        }
        if !self.live.contains_key(addr) {
            let core = self.core.clone();
            let old_addr = addr.to_owned();
            cx.background_executor()
                .spawn(async move {
                    let _ = core.call("terminal.rm", json!({ "terminal": old_addr }));
                })
                .detach();
        }
        if let Some(idx) = pos {
            list.remove(idx);
            self.save_trash_list(list, cx);
        }
        Ok(())
    }

    fn purge_trashed_agent(&mut self, addr: &str, cx: &mut Context<Self>) -> Result<(), String> {
        let mut list = mods::shared("agents.trash")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let before = list.len();
        list.retain(|v| v.get("address").and_then(Value::as_str) != Some(addr));
        let in_list = list.len() != before;
        let has_wt = self.fork_worktree_info(addr).is_some();
        let in_live = self.live.contains_key(addr);
        let in_ended = self.ended_acp.contains_key(addr);
        if !in_list && !in_live && !in_ended {
            return Err(format!("no trashed agent {addr:?}"));
        }
        if has_wt {
            self.kill_and_remove(&[addr.to_owned()], cx);
            return Ok(());
        }
        self.ended_acp.remove(addr);
        if in_list {
            self.save_trash_list(list, cx);
        }
        if in_live {
            self.kill(&[addr.to_owned()], cx);
        } else if is_acp_addr(addr) {
            let core = self.core.clone();
            let a = addr.to_owned();
            cx.background_executor()
                .spawn(async move {
                    let _ = core.call("agent.rm", json!({ "agent": a }));
                })
                .detach();
        }
        Ok(())
    }

    fn empty_trash(&mut self, cx: &mut Context<Self>) {
        let list = mods::shared("agents.trash")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let mut addrs: Vec<String> = list
            .iter()
            .filter_map(|v| v.get("address").and_then(Value::as_str).map(str::to_owned))
            .collect();
        for (addr, rec) in &self.live {
            if !self.closing_terminals.contains_key(addr) && is_trashed_record(rec) && !addrs.contains(addr) {
                addrs.push(addr.clone());
            }
        }
        let (wt_addrs, normal_addrs): (Vec<String>, Vec<String>) = addrs
            .into_iter()
            .partition(|a| self.fork_worktree_info(a).is_some());
        if !wt_addrs.is_empty() {
            self.kill_and_remove(&wt_addrs, cx);
        }
        self.save_trash_list(Vec::new(), cx);
        let mut normal_live = Vec::new();
        let mut ended_acp_rm = Vec::new();
        for a in normal_addrs {
            self.ended_acp.remove(&a);
            if self.live.contains_key(&a) {
                normal_live.push(a);
            } else if is_acp_addr(&a) {
                ended_acp_rm.push(a);
            }
        }
        if !normal_live.is_empty() {
            self.kill(&normal_live, cx);
        }
        if !ended_acp_rm.is_empty() {
            let core = self.core.clone();
            cx.background_executor()
                .spawn(async move {
                    for a in ended_acp_rm {
                        let _ = core.call("agent.rm", json!({ "agent": a }));
                    }
                })
                .detach();
        }
    }

    // ---- commands ----------------------------------------------------------

    /// Runs a keys.json action string: `name`, `name:value` (the value fills
    /// the command's first argument), `send:<text>`, or `<mod>.<command>`.
    fn run_action(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        // `send:` is only meaningful in a terminal, which handles it before it gets here.
        if !action.starts_with("send:") {
            self.run_key(action, Value::Null, window, cx);
        }
    }

    fn run_key(&mut self, name: &str, args: Value, window: &mut Window, cx: &mut Context<Self>) {
        let modal_open = self.overlays.iter().any(|o| o.kind.modal_keys());
        // Close the dialog holding the keyboard, not the terminal behind it.
        let implicit_close = match crate::keys::current(name).as_str() {
            "tab.close" => Self::arg(&args, "id").is_none(),
            "pane.close" => Self::arg(&args, "pane").is_none(),
            _ => false,
        };
        if implicit_close && modal_open {
            self.run("overlay.close", json!({}), window, cx);
            return;
        }
        if !modal_open && crate::keys::current(name) == "tab.rename" && Self::arg(&args, "id").is_none() {
            match self.rename_focus.clone() {
                RenameFocus::Space => {
                    self.run("space.rename", json!({}), window, cx);
                    return;
                }
                RenameFocus::Agent(saved) => {
                    let target = saved
                        .or_else(|| mods::shared("agents.selected").and_then(|v| v.as_str().map(str::to_owned)))
                        .or_else(|| self.peeked_agent.clone())
                        .or_else(|| self.shown_agent());
                    if let Some(t) = target.filter(|t| self.live.contains_key(t)) {
                        self.run("agent.rename", json!({ "terminal": t }), window, cx);
                        return;
                    }
                }
                RenameFocus::Tab => {}
            }
        }
        self.run(name, args, window, cx);
    }

    fn argv(args: &Value) -> Option<Vec<String>> {
        args.get("argv").and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    fn arg<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
        args.get(k).and_then(Value::as_str)
    }

    fn owned(args: &Value, k: &str) -> Option<String> {
        Self::arg(args, k).map(str::to_owned)
    }

    /// Whether a new space or tab is shown: `focus`, true when omitted; None when it isn't a boolean.
    fn focus(args: &Value) -> Option<bool> {
        match args.get("focus") {
            None | Some(Value::Null) => Some(true),
            Some(v) => v.as_bool(),
        }
    }

    fn answer(&mut self, r: Result<Value, String>) {
        if let Err(e) = &r {
            tracing::warn!("{e}");
        }
        self.reply = Some(r.unwrap_or_else(|e| json!({ "error": e })));
    }

    fn fail(&mut self, message: impl Into<String>) {
        self.answer(Err(message.into()));
    }

    /// Runs a command for the person (keys, clicks, menus, mods): a missing
    /// name is asked for.
    fn run(&mut self, name: &str, args: Value, window: &mut Window, cx: &mut Context<Self>) {
        // A click or key has nobody to read the reply: show why it failed.
        if let Some(error) = self.run_as(name, args, true, window, cx).get("error").and_then(Value::as_str) {
            self.command_error(name, error, window, cx);
        }
    }

    fn command_error(&mut self, name: &str, error: &str, window: &mut Window, cx: &mut Context<Self>) {
        let spec = json!({"kind":"toast", "notification":{"title":format!("{name} failed"),"body":error,"level":"error"}});
        self.open_overlay(mods::new_overlay_id(), spec, window, cx);
    }

    /// Runs a command and says what it made or why not. Scripts and agents
    /// (`person: false`) never get a dialog: a missing argument is an error.
    fn run_as(&mut self, name: &str, mut args: Value, person: bool, window: &mut Window, cx: &mut Context<Self>) -> Value {
        // A mod someone edited under 0.1.0 still says project.* / context.*.
        let current = crate::keys::current(name);
        let name = current.as_str();
        // `name:value`, as in keys.json.
        if let Some((n, v)) = name.split_once(':')
            && let Some(c) = crate::commands::find(n)
        {
            if let Err(error) = crate::commands::validate_args(c, &args) {
                return json!({ "error": error });
            }
            if c.args.is_empty() {
                return json!({ "error": format!("{n}: unexpected shorthand argument; valid arguments: none") });
            }
            let mut short = crate::commands::shorthand(c, v);
            if let (Some(s), Some(extra)) = (short.as_object_mut(), args.as_object()) {
                s.extend(extra.clone());
            }
            args = short;
            return self.run_as(n, args, person, window, cx);
        }
        let Some(command) = crate::commands::find(name) else {
            // `<mod>.<command>` goes to that mod's nextCommand().
            if let Some((m, c)) = name.split_once('.')
                && mods::send_command(m, c)
            {
                return json!({ "ok": true });
            }
            tracing::warn!("unknown command {name:?}");
            return json!({ "error": format!("unknown command {name:?}: see `herdr2 commands`; a mod's are <mod>.<command> and need the mod loaded") });
        };
        if let Err(error) = crate::commands::validate_args(command, &args) {
            return json!({ "error": error });
        }
        // pane.go from a mod block starts at the block, not at the terminal block focused before it.
        if terminal_block(&self.chrome, self.focused_pane.as_deref()).is_some()
            && needs_model_pane(name, &args)
            && !(name == "pane.go" && self.focused_block.is_some())
        {
            return json!({ "error": "a terminal layout block is focused, not a pane; focus a pane or pass an explicit pane id" });
        }
        if person && name.ends_with(".rename") && args.get("name").is_none_or(Value::is_null) {
            self.ask_name(name, args, window, cx);
            return json!({ "asked": "name" });
        }
        if let Some(arg) = crate::commands::missing(command, &args) {
            return json!({ "error": format!("{name} needs {arg}") });
        }
        if person {
            crate::telemetry::command(command.name);
        }
        if name == "space.new" {
            let machine = new_space_machine(&args, person, &self.model);
            if !args.is_object() { args = json!({}); }
            args["machine"] = json!(machine);
        }
        if matches!(name, "space.new" | "space.close" | "space.rename" | "space.move" | "tab.new" | "tab.close" | "tab.rename" | "tab.move" | "tab.merge" | "pane.split" | "pane.close" | "pane.detach" | "pane.swap" | "terminal.keep" | "terminal.park") {
            let node = ["id", "space", "pane", "target", "source"].into_iter().find_map(|k| Self::arg(&args, k));
            let target = node.and_then(|id| self.model.space(id).or_else(|| self.model.space_of(id))).or_else(|| self.model.active_space());
            let machine = if name == "space.new" { Self::arg(&args, "machine") }
                else if name == "terminal.park" { Self::arg(&args, "terminal").and_then(|a| a.split_once('/').map(|(m, _)| m)) }
                else { target.and_then(|s| s.machine.as_deref()) };
            if let Err(error) = self.require_document(machine) { return json!({"error":error}); }
        }
        if name == "layout.resize"
            && let Some(space) = Self::arg(&args, "id").and_then(|id| self.model.space_of(id))
            && let Err(error) = self.require_document(space.machine.as_deref())
        { return json!({"error":error}); }
        self.reply = None;
        self.dispatch(name, &args, person, window, cx);
        self.flush_documents(false, window, cx);
        self.reply.take().unwrap_or_else(|| json!({ "ok": true }))
    }

    /// Asks the person for a new name (space.rename, tab.rename), then runs it with it.
    /// The prompt starts with the name someone gave, empty for an automatic one.
    fn ask_name(&mut self, name: &str, mut args: Value, window: &mut Window, cx: &mut Context<Self>) {
        if !args.is_object() {
            args = json!({});
        }
        let zh = settings::get().language == settings::Language::ZhCn;
        let (title, hint, current) = match name {
            "space.rename" => {
                let id = Self::owned(&args, "id").or_else(|| self.model.active.clone());
                let current = id.as_deref().and_then(|i| self.model.space(i)).map(|p| p.name.clone());
                args["id"] = json!(id);
                (
                    if zh { "重命名工作区" } else { "rename space" },
                    if zh { "留空则自动使用目录名" } else { "empty: named after its folder" },
                    current,
                )
            }
            "agent.rename" => {
                let terminal = Self::owned(&args, "terminal")
                    .or_else(|| mods::shared("agents.selected").and_then(|v| v.as_str().map(str::to_owned)))
                    .or_else(|| self.peeked_agent.clone())
                    .or_else(|| self.model.focused_pane().and_then(|p| self.model.any_address_of(&p)))
                    .or_else(|| self.shown_agent());
                let rec = terminal.as_deref().and_then(|t| self.live.get(t).or_else(|| self.ended_acp.get(t)));
                let mut is_split_or_shell = false;
                let tab_name = terminal.as_deref().and_then(|t| {
                    let pane = self.model.any_pane_of(t)?;
                    let tab_id = self.model.tab_of(&pane)?.id.clone();
                    let tab = self.model.tab(&tab_id)?;
                    let pane_count = tab.root.panes().len();
                    if pane_count > 1 || (!is_acp_addr(t) && !self.is_agent_pty(t)) {
                        is_split_or_shell = true;
                    }
                    (pane_count == 1 && !tab.name.is_empty()).then(|| tab.name.clone())
                });
                let current = rec
                    .and_then(|r| r.pointer("/labels/herdr.name").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned))
                    .or(tab_name)
                    .or_else(|| rec.and_then(|r| r.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned)));
                args["terminal"] = json!(terminal);
                (
                    if is_split_or_shell {
                        if zh { "重命名分栏" } else { "rename pane" }
                    } else if zh {
                        "重命名智能体"
                    } else {
                        "rename agent"
                    },
                    if zh { "留空则恢复默认名称" } else { "empty: automatic name" },
                    current,
                )
            }
            _ => {
                let id = Self::owned(&args, "id").or_else(|| self.model.active_tab().map(|c| c.id.clone()));
                let current = id.as_deref().and_then(|i| self.model.tab(i)).map(|c| c.name.clone());
                args["id"] = json!(id);
                (
                    if zh { "重命名标签页" } else { "rename tab" },
                    if zh { "留空则显示序号" } else { "empty: its number" },
                    current,
                )
            }
        };
        // Once: a second rename press closes the open prompt instead of stacking another.
        if self.overlays.iter().any(|o| o.id == "rename") {
            self.close_overlay("rename", window, cx);
            return;
        }
        let spec = json!({ "mod": "prompt", "kind": "dialog", "width": 380,
            "props": { "title": title, "value": current.as_deref().unwrap_or(""), "hint": hint, "command": { "name": name, "args": args } } });
        self.open_overlay("rename".into(), spec, window, cx);
    }

    fn dispatch(&mut self, name: &str, a: &Value, person: bool, window: &mut Window, cx: &mut Context<Self>) {
        let active_ctx = self.model.active_tab().map(|c| c.id.clone());
        match name {
            // ---- spaces
            "space.new" => {
                let Some(focus) = Self::focus(a) else { return self.fail("space.new focus is true or false") };
                let name = Self::arg(a, "name").unwrap_or_default();
                let r = self.new_space(name, Self::owned(a, "folder"), Self::arg(a, "tab"), Self::argv(a), Self::arg(a, "task"), Self::owned(a, "machine").filter(|m| !m.is_empty()), Self::owned(a, "shell"), focus, window);
                self.answer(r);
            }
            "space.close" => {
                let Some(id) = Self::owned(a, "id").or_else(|| self.model.active.clone()) else { return self.fail("no space to close") };
                let Some(p) = self.model.space(&id) else { return self.fail(format!("no space {id:?}")) };
                if a.get("confirmed").and_then(Value::as_bool) != Some(true) {
                    if !person {
                        return self.fail("space.close ends its terminals: pass confirmed: true");
                    }
                    let panes: usize = p.tabs.iter().map(|c| c.root.panes().len()).sum();
                    let zh = settings::get().language == settings::Language::ZhCn;
                    let detail = if zh {
                        format!("{} — {} 个标签页，{} 个终端", self.model.space_name(p, &home()), p.tabs.len(), panes)
                    } else {
                        format!("{} \u{2014} {} tabs, {} terminals", self.model.space_name(p, &home()), p.tabs.len(), panes)
                    };
                    self.confirm(if zh { "关闭工作区？" } else { "Close space?" }, &detail, "space.close", json!({ "id": id, "confirmed": true }), window, cx);
                    return;
                }
                let space_acp = self.space_acp_agents(&id);
                let mut addrs = self.model.remove_space(&id);
                for addr in space_acp {
                    if !addrs.contains(&addr) {
                        addrs.push(addr);
                    }
                }
                let mut agent_snaps: Vec<(String, Value)> = Vec::new();
                let mut shell_ptys: Vec<String> = Vec::new();
                for addr in addrs {
                    if is_acp_addr(&addr) || self.is_agent_pty(&addr) {
                        if let Some(rec) = self.live.get(&addr).or_else(|| self.ended_acp.get(&addr)).cloned() {
                            agent_snaps.push((addr.clone(), self.trash_snapshot(&addr, &rec)));
                        }
                    } else if self.live.contains_key(&addr) {
                        shell_ptys.push(addr);
                    }
                }
                for (addr, snap) in agent_snaps {
                    self.trash_detached(&addr, snap, cx);
                }
                if !shell_ptys.is_empty() {
                    self.kill(&shell_ptys, cx);
                }
            }
            "space.rename" => {
                let Some(id) = Self::owned(a, "id").or_else(|| self.model.active.clone()) else { return self.fail("no space to rename") };
                let Some(name) = Self::arg(a, "name") else { return self.fail("space.rename needs name (empty: automatic)") };
                let Some(p) = self.model.space_mut(&id) else { return self.fail(format!("no space {id:?}")) };
                p.name = name.trim().to_owned();
            }
            "space.move" => {
                let (Some(id), Some(i)) = (Self::arg(a, "id"), a.get("index").and_then(Value::as_u64)) else { return self.fail("space.move needs id and index") };
                if !self.model.move_space(id, i as usize) { return self.fail(format!("no space {id:?}")); }
            }
            "space.select" => {
                let id = Self::owned(a, "id").or_else(|| {
                    let i = a.get("index").and_then(Value::as_u64)? as usize;
                    self.model.spaces.get(i.checked_sub(1)?).map(|p| p.id.clone())
                });
                match id.filter(|id| self.model.space(id).is_some()) {
                    Some(id) => self.model.active = Some(id),
                    None => return self.fail("no such space"),
                }
            }
            "space.next" | "space.prev" => {
                let n = self.model.spaces.len();
                if let Some(i) = self.model.active_index().filter(|_| n > 0) {
                    let j = if name == "space.next" { (i + 1) % n } else { (i + n - 1) % n };
                    self.model.active = Some(self.model.spaces[j].id.clone());
                }
            }
            // ---- tabs
            "tab.new" => {
                let Some(focus) = Self::focus(a) else { return self.fail("tab.new focus is true or false") };
                let name = Self::arg(a, "name").unwrap_or_default();
                let acp = a.get("acp").and_then(Value::as_bool) == Some(true);
                let shell = Self::owned(a, "shell");
                let r = match Self::owned(a, "mod") {
                    Some(m) => self.new_mod_tab(Self::owned(a, "space"), name, m, a.get("props").cloned().unwrap_or(Value::Null), focus),
                    None if acp => {
                        let Some(agent) = Self::owned(a, "agent") else { return self.fail("tab.new acp needs agent") };
                        self.new_acp_tab(Self::owned(a, "space"), name, Self::owned(a, "folder"), &agent, Self::arg(a, "task"), focus, window, cx)
                    }
                    // A person's new tab with no space open starts one, as space.new does.
                    None if person && a.get("space").is_none() && self.model.active_space().is_none() => {
                        self.new_space("", Self::owned(a, "folder"), Some(name), Self::argv(a), Self::arg(a, "task"), None, shell, focus, window)
                    }
                    None => self.new_tab(Self::owned(a, "space"), name, Self::owned(a, "folder"), Self::argv(a), Self::arg(a, "task"), shell, focus, window),
                };
                self.answer(r);
            }
            "tab.close" => {
                let Some(id) = Self::owned(a, "id").or(active_ctx) else { return self.fail("no tab to close") };
                let Some(p) = self.model.space_of(&id) else { return self.fail(format!("no tab {id:?}")) };
                let tab_addrs = self.model.tab(&id).map(|c| model::any_addresses(&c.root)).unwrap_or_default();
                let (agent_addrs, shell_ptys): (Vec<String>, Vec<String>) = tab_addrs
                    .into_iter()
                    .partition(|addr| is_acp_addr(addr) || self.is_agent_pty(addr));
                // Closing a tab NEVER kills or trashes any agent (ACP or CLI, including worktree forks):
                // agents are parked cleanly in the background; only plain shells end with their tab.
                // Closing the last tab keeps its space alive with a fresh tab if it held an agent.
                if p.tabs.len() == 1 {
                    if agent_addrs.is_empty() {
                        let mut args = json!({ "id": p.id });
                        args["confirmed"] = a.get("confirmed").cloned().unwrap_or(Value::Null);
                        return self.dispatch("space.close", &args, person, window, cx);
                    }
                    let space_id = p.id.clone();
                    let _ = self.new_tab(Some(space_id), "", None, None, None, None, true, window);
                    let _ = self.model.remove_tab(&id);
                    if !shell_ptys.is_empty() {
                        self.kill(&shell_ptys, cx);
                    }
                    self.park_agent_addresses(&agent_addrs, cx);
                } else {
                    let (addrs, _) = self.model.remove_tab(&id);
                    let (removed_agents, removed_shells): (Vec<String>, Vec<String>) = addrs
                        .into_iter()
                        .partition(|addr| is_acp_addr(addr) || self.is_agent_pty(addr));
                    if !removed_shells.is_empty() {
                        self.kill(&removed_shells, cx);
                    }
                    self.park_agent_addresses(&removed_agents, cx);
                }
            }
            "tab.rename" => {
                let Some(id) = Self::owned(a, "id").or(active_ctx) else { return self.fail("no tab to rename") };
                let Some(name) = Self::arg(a, "name") else { return self.fail("tab.rename needs name (empty: its number)") };
                let Some(c) = self.model.tab_mut(&id) else { return self.fail(format!("no tab {id:?}")) };
                c.name = name.trim().to_owned();
            }
            "tab.move" => {
                let Some(id) = Self::owned(a, "id").or(active_ctx) else { return self.fail("no tab") };
                let moved = match (a.get("index").and_then(Value::as_u64), a.get("by").and_then(Value::as_i64)) {
                    (Some(i), None) => self.model.move_tab(&id, i as usize),
                    (None, Some(by)) => self.model.shift_tab(&id, by),
                    _ => return self.fail("tab.move needs index or by"),
                };
                if !moved { return self.fail(format!("no tab {id:?}")); }
            }
            "tab.merge" => {
                let Some(source) = Self::owned(a, "source") else { return self.fail("tab.merge needs source") };
                let Some(target) = Self::owned(a, "target") else { return self.fail("tab.merge needs target") };
                let edge = match Self::arg(a, "direction").unwrap_or("right") {
                    "right" => crate::layout::Edge::Right,
                    "left" => crate::layout::Edge::Left,
                    "down" => crate::layout::Edge::Bottom,
                    "up" => crate::layout::Edge::Top,
                    _ => return self.fail("direction must be \"right\", \"left\", \"down\" or \"up\""),
                };
                let machine = self.model.space_of(&target).and_then(|s| s.machine.clone());
                self.preserve_single_pane_tab_name(&source, cx);
                let dst_was_single = self.preserve_single_pane_tab_name(&target, cx);
                let moved_addrs = match self.model.merge_tabs(&source, &target, edge) {
                    Ok(v) => v,
                    Err(e) => return self.fail(e),
                };
                if dst_was_single && self.require_document(machine.as_deref()).is_ok()
                    && let Some(c) = self.model.tab_mut(&target)
                {
                    c.name.clear();
                }
                let tab_label = crate::mirror::local(&target, machine.as_deref()).to_owned();
                for addr in moved_addrs {
                    if let Some(labels) = self.live.get_mut(&addr).and_then(|r| r.get_mut("labels")).and_then(Value::as_object_mut) {
                        labels.insert("herdr.tab".into(), json!(tab_label.clone()));
                    }
                    Self::write_core_label(self.core.clone(), addr, "herdr.tab", tab_label.clone(), cx);
                }
                self.focused_block = None;
                self.focused_pane = None;
            }
            "tab.settle" | "tab.unsettle" => {
                let Some(id) = Self::owned(a, "id").or(active_ctx) else { return self.fail("no tab") };
                let found = if name == "tab.settle" { self.model.settle(&id, model::now_ms()) } else { self.model.tab(&id).is_some() && { self.model.unsettle(&id); true } };
                if !found {
                    return self.fail(format!("no tab {id:?}"));
                }
            }
            "tab.select" => {
                // By position among the tabs the lists show (settled ones are put away).
                let id = Self::owned(a, "id").or_else(|| {
                    let i = a.get("index").and_then(Value::as_u64)? as usize;
                    self.model.active_space()?.tabs.iter().filter(|c| c.settled.is_none()).nth(i.checked_sub(1)?).map(|c| c.id.clone())
                });
                if !id.is_some_and(|id| self.model.select_tab(&id)) {
                    return self.fail("no such tab");
                }
            }
            "tab.next" | "tab.prev" => {
                if let Some(p) = self.model.active_space()
                    && let Some(i) = active_ctx.as_ref().and_then(|c| p.tabs.iter().position(|x| x.id == *c))
                {
                    let n = p.tabs.len();
                    let step = |k: usize| if name == "tab.next" { (i + k) % n } else { (i + n * k - k) % n };
                    if let Some(id) = (1..n).map(|k| &p.tabs[step(k)]).find(|c| c.settled.is_none()).map(|c| c.id.clone()) {
                        self.model.select_tab(&id);
                    }
                }
            }
            // ---- panes
            "pane.split" => {
                let axis = match Self::arg(a, "direction") {
                    Some("right") => Axis::Row,
                    Some("down") => Axis::Column,
                    other => return self.fail(format!("pane.split direction is \"right\" or \"down\", not {other:?}")),
                };
                let r = match Self::owned(a, "mod") {
                    Some(name) => self.split_mod(Self::owned(a, "pane"), axis, name, a.get("props").cloned().unwrap_or(Value::Null)),
                    None => self.split(Self::owned(a, "pane"), axis, Self::argv(a), None, Self::owned(a, "shell"), window),
                };
                self.answer(r);
            }
            "pane.close" => {
                let Some(pane) = Self::owned(a, "pane").or_else(|| self.model.focused_pane()) else { return self.fail("no pane to close") };
                let Some(c) = self.model.tab_of(&pane) else { return self.fail(format!("no pane {pane:?}")) };
                if c.root.find(&pane).is_some_and(|n| !n.closable()) {
                    return;
                }
                let confirmed = a.get("confirmed").and_then(Value::as_bool) == Some(true);
                if c.root.panes().len() == 1 {
                    let id = c.id.clone();
                    let mut args = json!({ "id": id });
                    if confirmed {
                        args["confirmed"] = json!(true);
                    }
                    return self.dispatch("tab.close", &args, person, window, cx);
                }
                let addr = self.model.any_address_of(&pane);
                self.model.remove_pane(&pane);
                if let Some(addr) = addr {
                    if is_acp_addr(&addr) || self.is_agent_pty(&addr) {
                        self.park_agent_addresses(&[addr], cx);
                    } else {
                        self.kill(&[addr], cx);
                    }
                }
            }
            "pane.zoom" => {
                if let Some(pane) = Self::owned(a, "pane").or_else(|| self.model.focused_pane()) {
                    self.model.toggle_zoom(&pane);
                }
            }
            "pane.detach" => {
                let Some(pane) = Self::owned(a, "pane").or_else(|| self.model.focused_pane()) else {
                    return self.fail("no pane to detach");
                };
                let (src_tab_id, old_src_name, pane_count) = {
                    let Some(c) = self.model.tab_of(&pane) else {
                        return self.fail(format!("no pane {pane:?}"));
                    };
                    (c.id.clone(), c.name.clone(), c.root.panes().len())
                };
                if pane_count < 2 {
                    if let Some(c) = self.model.tab_mut(&src_tab_id) {
                        c.zoomed = None;
                    }
                    if !person {
                        return self.fail("pane is already in its own tab");
                    }
                    return;
                }
                let new_tab_name = self.default_tab_name_for_pane(&pane);
                let (new_tab_id, moved_addr) = match self.model.detach_pane(&pane, &new_tab_name) {
                    Ok(res) => res,
                    Err(e) => return self.fail(e),
                };
                if !old_src_name.is_empty() && old_src_name == new_tab_name
                    && let Some(rem_pane) = self.model.tab(&src_tab_id).and_then(|c| c.focused_pane())
                {
                    let rem_name = self.default_tab_name_for_pane(&rem_pane);
                    if let Some(c) = self.model.tab_mut(&src_tab_id) {
                        c.name = rem_name;
                    }
                }
                let machine = self.model.space_of(&new_tab_id).and_then(|s| s.machine.clone());
                let tab_label = crate::mirror::local(&new_tab_id, machine.as_deref()).to_owned();
                if let Some(addr) = moved_addr {
                    if let Some(labels) = self.live.get_mut(&addr).and_then(|r| r.get_mut("labels")).and_then(Value::as_object_mut) {
                        labels.insert("herdr.tab".into(), json!(tab_label.clone()));
                    }
                    Self::write_core_label(self.core.clone(), addr.clone(), "herdr.tab", tab_label, cx);
                    if is_acp_addr(&addr) {
                        self.focus_mounted_acp(&addr, window, cx);
                    }
                }
                self.focused_block = None;
                self.focused_pane = None;
                self.reply = Some(json!({ "tab": new_tab_id, "pane": pane }));
            }
            "pane.focus" => {
                if let Some(terminal) = Self::arg(a, "terminal")
                    && self.is_side_chat(terminal)
                {
                    let addr = terminal.to_owned();
                    self.evict_side_chat_from_tabs(&addr, cx);
                    self.open_side_chat_panel(&addr, window, cx);
                    return;
                }
                if Self::arg(a, "pane").is_none()
                    && let Some(terminal) = Self::arg(a, "terminal")
                    && let Some(block) = self.chrome.visible_blocks().into_iter().find(|n| n.terminal() == Some(terminal))
                {
                    let id = block.id.clone();
                    self.focused_block = None;
                    self.focused_pane = Some(id.clone());
                    self.synced_model_pane = self.model.focused_pane();
                    if matches!(self.rename_focus, RenameFocus::Agent(_)) {
                        self.rename_focus = RenameFocus::Agent(Some(terminal.to_owned()));
                    }
                    if !self.overlays.iter().any(|o| o.kind.modal_keys()) && let Some(f) = self.pane_focus(&id, cx) {
                        f.focus(window, cx);
                    }
                    self.publish(cx);
                    cx.notify();
                    return;
                }
                let pane = Self::owned(a, "pane").or_else(|| Self::arg(a, "terminal").and_then(|t| self.model.any_pane_of(t)));
                let Some(p) = pane.filter(|p| self.model.tab_of(p).is_some()) else { return self.fail("no such pane") };
                if let Some(addr) = self.model.any_address_of(&p)
                    && self.is_side_chat(&addr)
                {
                    self.evict_side_chat_from_tabs(&addr, cx);
                    self.open_side_chat_panel(&addr, window, cx);
                    return;
                }
                self.focused_block = None;
                self.model.focus_pane(&p);
                self.focused_pane = None;
                let resolved_addr = self.model.any_address_of(&p);
                if matches!(self.rename_focus, RenameFocus::Agent(_)) {
                    self.rename_focus = RenameFocus::Agent(resolved_addr.clone());
                }
                self.agent = resolved_addr;
                self.swapped.remove(&p);
            }
            "pane.go" => {
                let dir = Self::arg(a, "direction").unwrap_or_default();
                if !matches!(dir, "left" | "right" | "up" | "down") {
                    return self.fail(format!("{name} direction is left, right, up or down, not {dir:?}"));
                }
                return self.go(dir, window, cx);
            }
            "pane.swap" => {
                let by = match swap_by(a) {
                    Ok(by) => by,
                    Err(e) => return self.fail(e),
                };
                let explicit = Self::arg(a, "pane").is_some();
                let pane = Self::owned(a, "pane").or_else(|| self.model.focused_pane());
                let r = match (by, pane) {
                    (SwapBy::Direction(dir), Some(pane)) => self.swap_pane(&pane, explicit, dir),
                    (SwapBy::Direction(_), None) => Ok(()),
                    (SwapBy::Target(target), Some(pane)) => self.model.swap_panes(&pane, target),
                    (SwapBy::Target(_), None) => Err("no pane to swap".into()),
                };
                if let Err(e) = r {
                    return self.fail(e);
                }
            }
            // ---- terminals
            "terminal.open" => {
                let argv = Self::argv(a);
                let shell = Self::owned(a, "shell");
                let r = match Self::arg(a, "place").unwrap_or("tab") {
                    "right" => self.split(None, Axis::Row, argv, Self::owned(a, "folder"), shell, window),
                    "down" => self.split(None, Axis::Column, argv, Self::owned(a, "folder"), shell, window),
                    _ => {
                        let name = Self::owned(a, "name")
                            .or_else(|| argv.as_ref().and_then(|v| v.first()).map(|p| p.rsplit('/').next().unwrap_or(p).to_owned()))
                            .or_else(|| shell.as_deref().filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto")).map(|s| match s.to_ascii_lowercase().as_str() {
                                "powershell" => "PowerShell".to_owned(),
                                "pwsh" => "PowerShell 7".to_owned(),
                                "git-bash" => "Git Bash".to_owned(),
                                "cmd" => "CMD".to_owned(),
                                _ => s.to_owned(),
                            }))
                            .unwrap_or_else(|| "shell".into());
                        self.new_tab(None, &name, Self::owned(a, "folder"), argv, None, shell, true, window)
                    }
                };
                self.answer(r);
            }
            "terminal.keep" => {
                let r = self.keep_terminal(Self::arg(a, "terminal").unwrap_or_default(), Self::arg(a, "place").unwrap_or("tab"), window, cx);
                self.answer(r);
            }
            "terminal.park" => {
                let r = self.park_terminal(Self::arg(a, "terminal").unwrap_or_default(), window, cx);
                self.answer(r);
            }
            "terminal.right_click" => {
                let to = match Self::arg(a, "to") {
                    Some("program") => "program",
                    Some("menu") => "",
                    other => return self.fail(format!("terminal.right_click to is \"program\" or \"menu\", not {other:?}")),
                };
                let t = Self::owned(a, "terminal").or_else(|| self.focused_terminal(window, cx).map(|t| t.address.clone()));
                let Some(t) = t.filter(|t| self.live.contains_key(t)) else { return self.fail("no such running terminal") };
                // Off the UI thread: a terminal on a slow machine answers late.
                let core = self.core.clone();
                let params = json!({ "terminal": t, "labels": { "herdr.right-click": to } });
                cx.background_executor()
                    .spawn(async move {
                        if let Err(e) = core.call("terminal.label", params) {
                            tracing::warn!("right-click label: {e:#}");
                        }
                    })
                    .detach();
            }
            "terminal.view" => {
                let view = match Self::arg(a, "view") {
                    Some(v @ ("peek" | "terminal" | "toggle")) => v,
                    other => return self.fail(format!("terminal.view view is \"peek\", \"terminal\" or \"toggle\", not {other:?}")),
                };
                let targets: Vec<String> = if let Some(tab_id) = Self::arg(a, "tab") {
                    let Some(tab) = self.model.tab(tab_id) else {
                        return self.fail(format!("no such tab {tab_id:?}"));
                    };
                    let addrs: Vec<String> = tab
                        .root
                        .panes()
                        .into_iter()
                        .filter_map(|n| n.terminal())
                        .filter(|addr| !is_acp_addr(addr) && self.live.contains_key(*addr))
                        .map(str::to_owned)
                        .collect();
                    if addrs.is_empty() {
                        return self.fail("no running terminal pane in tab");
                    }
                    addrs
                } else {
                    let t = Self::owned(a, "terminal")
                        .or_else(|| self.model.focused_pane().and_then(|p| self.model.address_of(&p)))
                        .or_else(|| self.focused_terminal(window, cx).map(|t| t.address.clone()));
                    let Some(t) = t.filter(|t| !is_acp_addr(t) && self.live.contains_key(t)) else {
                        return self.fail("no such running terminal");
                    };
                    vec![t]
                };
                let next = match view {
                    "peek" => "peek",
                    "terminal" => "",
                    "toggle" => {
                        if targets.iter().all(|t| self.is_structured_terminal(t)) { "" } else { "peek" }
                    }
                    _ => unreachable!(),
                };
                for addr in targets {
                    if let Some(obj) = self.live.get_mut(&addr).and_then(Value::as_object_mut)
                        && let Some(labels) = obj.entry("labels").or_insert_with(|| json!({})).as_object_mut()
                    {
                        if next.is_empty() {
                            labels.remove("herdr.view");
                        } else {
                            labels.insert("herdr.view".into(), json!(next));
                        }
                    }
                    Self::write_core_label(self.core.clone(), addr, "herdr.view", next.to_owned(), cx);
                }
                mods::publish_terminals(self.ordered_live_values());
            }
            "terminal.pin" => {
                let Some(pinned) = a.get("pinned").and_then(Value::as_bool) else { return self.fail("terminal.pin pinned is true or false") };
                let t = Self::arg(a, "terminal").unwrap_or_default().to_owned();
                if !self.live.contains_key(&t) {
                    return self.fail(format!("no running terminal {t:?}"));
                }
                let core = self.core.clone();
                let labels = json!({ "herdr.pinned": if pinned { "1" } else { "" } });
                cx.background_executor()
                    .spawn(async move {
                        let (method, params) = if is_acp_addr(&t) {
                            ("agent.label", json!({ "agent": t, "labels": labels }))
                        } else {
                            ("terminal.label", json!({ "terminal": t, "labels": labels }))
                        };
                        if let Err(e) = core.call(method, params) {
                            tracing::warn!("pin label: {e:#}");
                        }
                    })
                    .detach();
            }
            "terminal.kill" => {
                let t = Self::arg(a, "terminal").unwrap_or_default().to_owned();
                if !self.live.contains_key(&t) {
                    return self.fail(format!("no running terminal {t:?}"));
                }
                if is_acp_addr(&t) || self.is_agent_pty(&t) {
                    let r = self.trash_agent(&t, window, cx).map(|()| json!({ "trashed": t }));
                    self.answer(r);
                    return;
                }
                if self.peeked_agent.as_deref() == Some(t.as_str()) {
                    self.close_overlay("terminal-peek", window, cx);
                }
                self.kill(&[t], cx);
            }
            "terminal.peek" => {
                let t = Self::arg(a, "terminal").unwrap_or_default();
                if !self.live.contains_key(t) {
                    return self.fail(format!("no running terminal {t:?}"));
                }
                if matches!(self.rename_focus, RenameFocus::Agent(_)) {
                    self.rename_focus = RenameFocus::Agent(Some(t.to_owned()));
                }
                self.peek_terminal(t, window, cx);
            }
            "terminal.show" => {
                let pane = Self::owned(a, "pane").or_else(|| self.model.focused_pane());
                let target = Self::arg(a, "terminal").filter(|t| self.live.contains_key(*t)).map(str::to_owned);
                if let Some(p) = pane {
                    match target {
                        Some(t) if is_acp_addr(&t) => {
                            if self.focus_mounted_acp(&t, window, cx) {
                                return;
                            }
                            self.peek_terminal(&t, window, cx);
                        }
                        // One with a pane of its own is shown there.
                        Some(t) if let Some(own) = self.model.pane_of(&t) => {
                            self.swapped.remove(&p);
                            self.model.focus_pane(&own);
                        }
                        Some(t) => {
                            self.swapped.insert(p.clone(), t);
                            self.model.focus_pane(&p);
                        }
                        None => {
                            self.swapped.remove(&p);
                        }
                    }
                    self.focused_pane = None;
                }
            }
            "terminal.search" => {
                let Some(t) = self.focused_terminal(window, cx) else { return self.fail("no focused terminal to search") };
                t.view.update(cx, |view, cx| view.open_search(window, cx));
                return;
            }
            "terminal.copy" | "terminal.paste" | "terminal.paste_image" => {
                if let Some(t) = self.focused_terminal(window, cx) {
                    t.view.update(cx, |v, cx| match name {
                        "terminal.paste" => v.paste(cx),
                        "terminal.paste_image" => v.paste_image(cx),
                        // A command, not a key: copy and leave the selection.
                        _ => { let _ = v.copy(cx); }
                    });
                }
                return;
            }
            "terminal.select_all" => {
                let view = match Self::owned(a, "terminal") {
                    Some(address) => self.shown_terminal_view(&address, window, cx),
                    None => self.focused_terminal(window, cx).map(|t| t.view.clone()).ok_or_else(|| "no focused terminal to select".into()),
                };
                match view {
                    Ok(view) => view.update(cx, |view, cx| view.select_all(cx)),
                    Err(error) => self.fail(error),
                }
                return;
            }
            "terminal.select" | "terminal.adjust_selection" | "terminal.clear_selection" => {
                let view = match Self::owned(a, "terminal") {
                    Some(address) => self.shown_terminal_view(&address, window, cx),
                    None => self.focused_terminal(window, cx).map(|t| t.view.clone()).ok_or_else(|| "no focused terminal to select".into()),
                };
                let result = view.and_then(|view| view.update(cx, |view, cx| view.selection_command(name, a, cx)));
                if let Err(error) = result {
                    return self.fail(error);
                }
            }
            "terminal.scroll" => {
                if let Err(error) = self.scroll_terminal(a, window, cx) {
                    self.fail(error);
                }
            }
            // ---- agents
            "agent.focus" => {
                let t = Self::arg(a, "terminal").unwrap_or_default().to_owned();
                if !self.live.contains_key(&t) {
                    if self.ended_acp.contains_key(&t) && self.focus_mounted_acp(&t, window, cx) {
                        return;
                    }
                    return self.fail(format!("no running terminal {t:?}"));
                }
                if matches!(self.rename_focus, RenameFocus::Agent(_)) {
                    self.rename_focus = RenameFocus::Agent(Some(t.clone()));
                }
                if self.is_side_chat(&t) {
                    self.close_overlay("terminal-peek", window, cx);
                    if self.evict_side_chat_from_tabs(&t, cx) {
                        self.sync(window, cx);
                        self.flush_documents(false, window, cx);
                    }
                    self.open_side_chat_panel(&t, window, cx);
                    return;
                }
                let is_acp = is_acp_addr(&t)
                    || self.live.get(&t).and_then(|r| r.get("acp")).and_then(Value::as_bool) == Some(true);
                if is_acp {
                    self.close_overlay("terminal-peek", window, cx);
                    if !self.focus_mounted_acp(&t, window, cx) {
                        if self.live.get(&t).is_some_and(is_acp_subagent) {
                            let root = self.root_agent(&t);
                            if root != t {
                                let root_acp = is_acp_addr(&root)
                                    || self.live.get(&root).and_then(|r| r.get("acp")).and_then(Value::as_bool) == Some(true);
                                if root_acp {
                                    let _ = self.focus_mounted_acp(&root, window, cx);
                                } else {
                                    self.focus_agent(&root);
                                }
                            }
                            self.peek_terminal(&t, window, cx);
                        } else {
                            let _ = self.keep_terminal(&t, "tab", window, cx);
                        }
                    }
                    return;
                }
                self.close_overlay("terminal-peek", window, cx);
                self.focus_agent(&t);
            }
            "agent.rename" => {
                let t = Self::owned(a, "terminal")
                    .or_else(|| mods::shared("agents.selected").and_then(|v| v.as_str().map(str::to_owned)))
                    .or_else(|| self.peeked_agent.clone())
                    .or_else(|| self.model.focused_pane().and_then(|p| self.model.any_address_of(&p)))
                    .or_else(|| self.shown_agent())
                    .unwrap_or_default();
                let in_live = self.live.contains_key(&t);
                let in_ended = self.ended_acp.contains_key(&t);
                if !in_live && !in_ended {
                    return self.fail(format!("no running terminal {t:?}"));
                }
                let Some(raw) = Self::arg(a, "name") else {
                    return self.fail("agent.rename needs name (empty: automatic)");
                };
                let trimmed = raw.trim().to_owned();
                for map in [&mut self.live, &mut self.ended_acp] {
                    if let Some(obj) = map.get_mut(&t).and_then(Value::as_object_mut)
                        && let Some(labels) = obj.entry("labels").or_insert_with(|| json!({})).as_object_mut()
                    {
                        if trimmed.is_empty() {
                            labels.remove("herdr.name");
                        } else {
                            labels.insert("herdr.name".into(), json!(trimmed));
                        }
                    }
                }
                if let Some(pane) = self.model.any_pane_of(&t)
                    && let Some(tab_id) = self.model.tab_of(&pane).map(|c| c.id.clone())
                    && self.model.tab(&tab_id).is_some_and(|c| c.root.panes().len() == 1)
                {
                    let machine = self.model.space_of(&tab_id).and_then(|s| s.machine.clone());
                    if self.require_document(machine.as_deref()).is_ok()
                        && let Some(c) = self.model.tab_mut(&tab_id)
                    {
                        c.name = trimmed.clone();
                    }
                }
                mods::publish_terminals(self.ordered_live_values());
                Self::write_core_label(self.core.clone(), t, "herdr.name", trimmed, cx);
            }
            "agent.trash" => {
                let t = Self::owned(a, "terminal")
                    .or_else(|| mods::shared("agents.selected").and_then(|v| v.as_str().map(str::to_owned)))
                    .or_else(|| self.peeked_agent.clone())
                    .or_else(|| self.model.focused_pane().and_then(|p| self.model.any_address_of(&p)))
                    .or_else(|| self.shown_agent())
                    .unwrap_or_default();
                let r = self.trash_agent(&t, window, cx).map(|()| json!({ "trashed": t }));
                self.answer(r);
            }
            "agent.restore" => {
                let Some(t) = Self::owned(a, "terminal") else { return self.fail("agent.restore needs terminal") };
                let r = self.restore_trashed_agent(&t, window, cx).map(|()| json!({ "restored": t }));
                self.answer(r);
            }
            "agent.purge" => {
                let Some(t) = Self::owned(a, "terminal") else { return self.fail("agent.purge needs terminal") };
                let confirmed = a.get("confirmed").and_then(Value::as_bool) == Some(true);
                if person && !confirmed && self.fork_worktree_info(&t).is_some() {
                    self.prompt_purge_fork_worktree(&t, window, cx);
                    return;
                }
                let r = self.purge_trashed_agent(&t, cx).map(|()| json!({ "purged": t }));
                self.answer(r);
            }
            "agent.empty_trash" => {
                let confirmed = a.get("confirmed").and_then(Value::as_bool) == Some(true);
                if person && !confirmed {
                    let wt_items = self.trashed_fork_worktrees();
                    if !wt_items.is_empty() {
                        self.prompt_empty_trash_fork_worktrees(&wt_items, window, cx);
                        return;
                    }
                }
                self.empty_trash(cx);
            }
            "agent.status" => {
                let raw = Self::arg(a, "status").unwrap_or_default();
                let Some(next) = normalize_manual_status(raw) else {
                    return self.fail("agent.status requires --status idle|working|blocked|done|auto");
                };
                let t = Self::owned(a, "terminal")
                    .or_else(|| self.peeked_agent.clone())
                    .or_else(|| self.model.focused_pane().and_then(|p| self.model.any_address_of(&p)))
                    .or_else(|| self.shown_agent())
                    .unwrap_or_default();
                let Some(rec) = self.live.get(&t) else {
                    return self.fail(format!("no running terminal {t:?}"));
                };
                let turn = rec.get("turn").and_then(Value::as_u64).unwrap_or(0);
                let rep_state = rec.pointer("/report/state").and_then(Value::as_str).map(str::to_owned);
                let app = rec
                    .pointer("/report/app")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .or_else(|| rec.get("agent").and_then(Value::as_str).filter(|s| !s.is_empty()))
                    .or_else(|| rec.pointer("/program/key").and_then(Value::as_str))
                    .map(str::to_owned);
                let next_owned = next.map(str::to_owned);
                self.manual_statuses.insert(t.clone(), ManualStatus {
                    status: next_owned.clone(),
                    turn,
                    reported_state: rep_state,
                    set_at: Instant::now(),
                    label_synced: next.is_none(),
                });
                if let Some(obj) = self.live.get_mut(&t).and_then(Value::as_object_mut) {
                    obj.insert("manual_status".into(), next.map_or(Value::Null, |s| json!(s)));
                }
                self.model.set_manual_status(&t, next_owned, app.clone());
                if next.is_none() {
                    self.refresh_inferred_statuses(cx);
                    let word = self.live.get(&t).and_then(|r| r.get("status")).and_then(Value::as_str).map(str::to_owned);
                    let looking = self.visible_addresses().iter().any(|v| v == &t) && self.window_active;
                    self.model.set_word(&t, word, app, looking);
                }
                if let Some(a) = self.model.agents.get(&t)
                    && let Some(obj) = self.live.get_mut(&t).and_then(Value::as_object_mut)
                {
                    obj.insert("working_since".into(), a.working_since.map_or(Value::Null, |v| json!(v)));
                    obj.insert("work_ms".into(), a.work_ms.map_or(Value::Null, |v| json!(v)));
                    obj.insert("total_work_ms".into(), json!(a.total_work_ms));
                }
                mods::publish_terminals(self.ordered_live_values());
                self.sync(window, cx);
                self.refresh_snapshot_mods(cx);
                Self::write_core_label(
                    self.core.clone(),
                    t,
                    "herdr.status",
                    next.map_or_else(String::new, |s| format!("{s}@{turn}")),
                    cx,
                );
            }
            "agent.briefing" => {
                let away_minutes = a.get("away_minutes").and_then(Value::as_u64);
                mods::set_shared("briefing.away".into(), Value::Null);
                let mut props = json!({ "briefing": true });
                if let Some(mins) = away_minutes {
                    props["awayMinutes"] = json!(mins);
                }
                let spec = json!({
                    "mod": "agents",
                    "kind": "dialog",
                    "key": "catchup-briefing",
                    "width": 740,
                    "height": 580,
                    "props": props,
                });
                self.open_overlay(mods::new_overlay_id(), spec, window, cx);
            }
            "agent.handoff" => {
                let source = Self::owned(a, "source")
                    .or_else(|| Self::owned(a, "terminal"))
                    .or_else(|| self.peeked_agent.clone())
                    .or_else(|| self.model.focused_pane().and_then(|p| self.model.any_address_of(&p)))
                    .or_else(|| self.shown_agent())
                    .unwrap_or_default();
                if source.is_empty() {
                    return self.fail("agent.handoff needs a source session");
                }
                let preset = Self::owned(a, "preset").filter(|s| !s.is_empty());
                let into = Self::owned(a, "into").filter(|s| !s.is_empty());
                let kind_arg = Self::owned(a, "kind").filter(|s| !s.is_empty());
                if preset.is_none() && into.is_none() {
                    let kind = kind_arg.unwrap_or_else(|| "handoff".into());
                    let explicit = a.get("kind").is_some()
                        || a.get("source").is_some()
                        || a.get("terminal").is_some()
                        || a.get("to_turn").is_some();
                    let already_open = self.overlays.iter().any(|o| o.id == "handoff");
                    if already_open {
                        self.close_overlay("handoff", window, cx);
                    }
                    if !already_open || explicit {
                        let mut dialog_props = json!({ "source": source, "kind": kind });
                        if let Some(ft) = a.get("from_turn").and_then(Value::as_u64) {
                            dialog_props["from_turn"] = json!(ft);
                        }
                        if let Some(tt) = a.get("to_turn").and_then(Value::as_u64) {
                            dialog_props["to_turn"] = json!(tt);
                        }
                        let spec = json!({
                            "mod": "handoff",
                            "kind": "dialog",
                            "width": 580,
                            "props": dialog_props
                        });
                        self.open_overlay("handoff".into(), spec, window, cx);
                    }
                    return;
                }
                if self.overlays.iter().any(|o| o.id == "handoff") {
                    self.close_overlay("handoff", window, cx);
                }
                let is_merge = into.is_some();
                let kind = kind_arg.unwrap_or_else(|| if is_merge { "merge-back".into() } else { "handoff".into() });
                let rpc_kind = if kind == "merge-back" { "handoff" } else { kind.as_str() };
                let is_worktree = a.get("worktree").and_then(Value::as_bool) == Some(true);
                let archive_source = a.get("archive_source").and_then(Value::as_bool).unwrap_or(false);
                let close_ov = Self::owned(a, "close_overlay").filter(|s| !s.is_empty());
                let open = Self::owned(a, "open").unwrap_or_else(|| {
                    if is_merge {
                        "focus".into()
                    } else if kind == "side-chat" {
                        "panel".into()
                    } else if kind == "fork" && !is_worktree {
                        "split".into()
                    } else {
                        "tab".into()
                    }
                });
                let mut params = json!({
                    "source": source,
                    "kind": rpc_kind,
                    "display": crate::terminal::initial_display(window),
                    "language": if settings::get().language == settings::Language::ZhCn { "zh-CN" } else { "en" },
                });
                if let Some(p) = preset { params["preset"] = json!(p); }
                if let Some(dst) = into { params["into"] = json!(dst); }
                if let Some(m) = Self::owned(a, "mode").filter(|s| !s.is_empty()) {
                    if m.eq_ignore_ascii_case("pty") {
                        params["pty"] = json!(true);
                    }
                    params["mode"] = json!(m);
                }
                if let Some(prompt) = Self::owned(a, "prompt").filter(|s| !s.is_empty()) { params["prompt"] = json!(prompt); }
                if let Some(rt) = a.get("recent_turns").and_then(Value::as_u64) { params["recent_turns"] = json!(rt); }
                if let Some(ft) = a.get("from_turn").and_then(Value::as_u64) { params["from_turn"] = json!(ft); }
                if let Some(tt) = a.get("to_turn").and_then(Value::as_u64) { params["to_turn"] = json!(tt); }
                if let Some(mb) = a.get("max_bytes").and_then(Value::as_u64) { params["max_bytes"] = json!(mb); }
                if let Some(model) = Self::owned(a, "model").filter(|s| !s.is_empty()) { params["model"] = json!(model); }
                if let Some(effort) = Self::owned(a, "effort").filter(|s| !s.is_empty()) { params["effort"] = json!(effort); }
                if a.get("worktree").and_then(Value::as_bool) == Some(true) { params["worktree"] = json!(true); }
                let explicit_merge_wt = a.get("merge_worktree").and_then(Value::as_bool);
                if explicit_merge_wt == Some(true) || (is_merge && explicit_merge_wt.is_none() && self.fork_worktree_info(&source).is_some()) {
                    params["merge_worktree"] = json!(true);
                }

                let core = self.core.clone();
                let generation = self.core_generation;
                let source_for_ui = source.clone();
                cx.spawn_in(window, async move |this, cx| {
                    let res = cx
                        .background_executor()
                        .spawn(async move {
                            let out = core.call_timeout("agent.handoff", params, Duration::from_secs(30))?;
                            let target = out.get("target").and_then(Value::as_str).unwrap_or_default().to_owned();
                            let target_kind = out.get("target_kind").and_then(Value::as_str).unwrap_or("agent").to_owned();
                            let fetched = if target.is_empty() {
                                None
                            } else if target_kind == "terminal" {
                                core.call("terminal.get", json!({ "terminal": target })).ok()
                            } else {
                                core.call("agent.get", json!({ "agent": target })).ok()
                            };
                            Ok::<(Value, String, String, Option<Value>), anyhow::Error>((out, target, target_kind, fetched))
                        })
                        .await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        if generation != this.core_generation || this.draining {
                            return;
                        }
                        match res {
                            Ok((out, target, target_kind, fetched)) => {
                                if !target.is_empty() {
                                    if let Some(mut rec) = fetched
                                        && !this.live.contains_key(&target)
                                        && let Some(obj) = rec.as_object_mut()
                                    {
                                        if target_kind == "agent" {
                                            let report = obj.remove("status").unwrap_or(Value::Null);
                                            let raw_state = report.get("state").and_then(Value::as_str).or_else(|| report.as_str()).map(str::to_owned);
                                            let word = match raw_state.as_deref() {
                                                Some("starting" | "working") => json!("working"),
                                                Some("blocked" | "error") => json!("blocked"),
                                                _ => json!("idle"),
                                            };
                                            let app_hint = report
                                                .get("app")
                                                .and_then(Value::as_str)
                                                .filter(|s| !s.is_empty())
                                                .or_else(|| obj.get("agent").and_then(Value::as_str).filter(|s| !s.is_empty()));
                                            let mut prog = program_of(&json!({ "app": app_hint }), obj.get("argv"));
                                            prog["agent"] = json!(true);
                                            let raw_title = obj
                                                .get("title")
                                                .and_then(Value::as_str)
                                                .filter(|s| !s.is_empty())
                                                .or_else(|| obj.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()))
                                                .or(app_hint)
                                                .unwrap_or(&target)
                                                .to_owned();
                                            let shown = crate::terminal::shown_title(&raw_title).into_owned();
                                            obj.insert("acp".into(), json!(true));
                                            obj.insert("reported".into(), json!(true));
                                            obj.insert("raw_state".into(), raw_state.map_or(Value::Null, Value::String));
                                            obj.insert("status".into(), word);
                                            obj.insert("report".into(), report);
                                            obj.insert("program".into(), prog);
                                            obj.insert("raw_title".into(), raw_title.into());
                                            obj.insert("title".into(), shown.into());
                                        } else {
                                            let report = obj.remove("status").unwrap_or(Value::Null);
                                            let word = report.get("state").cloned().unwrap_or_else(|| if report.is_string() { report.clone() } else { Value::Null });
                                            obj.insert("status".into(), word);
                                            let argv = effective_running_argv(obj, false);
                                            obj.insert("program".into(), program_of(&report, argv.as_ref()));
                                            obj.insert("report".into(), report);
                                        }
                                        if !this.live_order.iter().any(|a| a == &target) {
                                            this.live_order.push(target.clone());
                                        }
                                        this.live.insert(target.clone(), rec);
                                    }
                                    if is_merge {
                                        let wt_merge_requested = out.get("worktree_merge_requested").and_then(Value::as_bool) == Some(true);
                                        let wt_branch_out = out.get("worktree_branch").and_then(Value::as_str).map(str::to_owned);
                                        if wt_merge_requested {
                                            for map in [&mut this.live, &mut this.ended_acp] {
                                                if let Some(lin) = map
                                                    .get_mut(&source_for_ui)
                                                    .and_then(|r| r.get_mut("lineage"))
                                                    .and_then(Value::as_object_mut)
                                                {
                                                    lin.insert("worktree_merged".into(), json!(true));
                                                    if let Some(ref br) = wt_branch_out
                                                        && !lin.contains_key("worktree_branch")
                                                    {
                                                        lin.insert("worktree_branch".into(), json!(br));
                                                    }
                                                }
                                            }
                                        }
                                        if let Some(obj) = this.live.get_mut(&target).and_then(Value::as_object_mut) {
                                            let next_seq = obj.get("transcript_seq").and_then(Value::as_u64).unwrap_or(0) + 1;
                                            obj.insert("transcript_seq".into(), json!(next_seq));
                                            if out.get("prompt_sent").and_then(Value::as_bool) == Some(true)
                                                && target_kind == "agent"
                                            {
                                                obj.insert("status".into(), json!("working"));
                                            }
                                        }
                                        mods::publish_terminals(this.ordered_live_values());
                                        let merged_files = out
                                            .get("merged_files")
                                            .and_then(Value::as_array)
                                            .map(|arr| arr.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                                            .unwrap_or_default();
                                        let is_zh = settings::get().language == settings::Language::ZhCn;
                                        let title = if wt_merge_requested {
                                            if is_zh {
                                                format!("已通知 {target} 合并 Worktree 代码")
                                            } else {
                                                format!("Notified {target} to merge Worktree")
                                            }
                                        } else if is_zh {
                                            format!("已合回上下文至 {target}")
                                        } else {
                                            format!("Merged context into {target}")
                                        };
                                        let body = if wt_merge_requested {
                                            let br_suffix = wt_branch_out.as_deref().map(|b| format!(" ({b})")).unwrap_or_default();
                                            if is_zh {
                                                format!("已向 {target} 发送合并指令，由主 Agent 合并 {source_for_ui} 的 Worktree 分支{br_suffix}代码（保留 Worktree 与分支）。")
                                            } else {
                                                format!("Instructed {target} to merge worktree branch{br_suffix} from {source_for_ui} (keeping worktree & branch).")
                                            }
                                        } else if !merged_files.is_empty() {
                                            if is_zh {
                                                format!("{source_for_ui} 的结论与 {} 个 Worktree 文件改动已合回 {target}。", merged_files.len())
                                            } else {
                                                format!("Merged context and {} worktree file(s) from {source_for_ui} into {target}.", merged_files.len())
                                            }
                                        } else if is_zh {
                                            format!("{source_for_ui} 的接力上下文已注入 {target}，正在继续处理。")
                                        } else {
                                            format!("Delivered context from {source_for_ui} to {target}.")
                                        };
                                        let notification = json!({
                                            "title": title,
                                            "body": body,
                                            "level": "info",
                                            "action": { "name": "agent.focus", "args": { "terminal": target } },
                                        });
                                        this.open_overlay(mods::new_overlay_id(), json!({ "kind": "toast", "notification": notification }), window, cx);
                                        if let Some(ref ov_id) = close_ov {
                                            this.close_overlay(ov_id, window, cx);
                                        }
                                        // Also close any floating side-chat window or hide the right side-chat panel for `source_for_ui`.
                                        let side_ov_id = format!("side-chat:{source_for_ui}");
                                        if this.overlays.iter().any(|o| o.id == side_ov_id) {
                                            this.close_overlay(&side_ov_id, window, cx);
                                        }
                                        if mods::shared("side-chat.active").and_then(|v| v.as_str().map(str::to_owned)).as_deref() == Some(source_for_ui.as_str()) {
                                            this.dispatch("layout.hide", &json!({ "id": "panel" }), false, window, cx);
                                        }
                                        if open != "none" {
                                            if this.peeked_agent.as_deref() == Some(source_for_ui.as_str()) && target != source_for_ui {
                                                this.close_overlay("terminal-peek", window, cx);
                                            }
                                            if open == "peek" {
                                                this.dispatch("terminal.peek", &json!({ "terminal": target }), false, window, cx);
                                            } else {
                                                this.dispatch("agent.focus", &json!({ "terminal": target }), false, window, cx);
                                            }
                                        }
                                    } else {
                                        let wt_path = out
                                            .get("worktree_path")
                                            .and_then(Value::as_str)
                                            .map(str::trim)
                                            .filter(|s| !s.is_empty())
                                            .map(str::to_owned)
                                            .or_else(|| this.fork_worktree_info(&target).map(|(p, _, _)| p));
                                        if let Some(ref wt) = wt_path {
                                            this.cwds.entry(target.clone()).or_insert_with(|| wt.clone());
                                        }
                                        mods::publish_terminals(this.ordered_live_values());
                                        let created_side_chat = kind.eq_ignore_ascii_case("side-chat")
                                            || kind.eq_ignore_ascii_case("side_chat")
                                            || this.is_side_chat(&target);
                                        if created_side_chat {
                                            this.evict_side_chat_from_tabs(&target, cx);
                                            if open != "none" {
                                                this.open_side_chat_panel(&target, window, cx);
                                            }
                                        } else if let Some(ref wt) = wt_path {
                                            let target_machine = target.split_once('/').map(|(m, _)| m);
                                            match open.as_str() {
                                                "none" => {
                                                    if !this.model.spaces.iter().any(|sp| this.space_matches_worktree(sp, wt, target_machine)) {
                                                        let _ = this.new_space(
                                                            "",
                                                            Some(wt.clone()),
                                                            None,
                                                            None,
                                                            None,
                                                            target_machine.map(str::to_owned),
                                                            None,
                                                            false,
                                                            window,
                                                        );
                                                    }
                                                }
                                                "peek" => {
                                                    if !this.model.spaces.iter().any(|sp| this.space_matches_worktree(sp, wt, target_machine)) {
                                                        let _ = this.new_space(
                                                            "",
                                                            Some(wt.clone()),
                                                            None,
                                                            None,
                                                            None,
                                                            target_machine.map(str::to_owned),
                                                            None,
                                                            false,
                                                            window,
                                                        );
                                                    }
                                                    this.dispatch("terminal.peek", &json!({ "terminal": target }), false, window, cx);
                                                }
                                                "panel" => this.open_side_chat_panel(&target, window, cx),
                                                "window" | "float" => this.open_side_chat_window(&target, &source_for_ui, window, cx),
                                                _ => this.dispatch("terminal.keep", &json!({ "terminal": target, "place": "tab" }), false, window, cx),
                                            }
                                        } else {
                                            match open.as_str() {
                                                "none" => {}
                                                "panel" => this.open_side_chat_panel(&target, window, cx),
                                                "window" | "float" => this.open_side_chat_window(&target, &source_for_ui, window, cx),
                                                "peek" => this.dispatch("terminal.peek", &json!({ "terminal": target }), false, window, cx),
                                                "split" | "right" => this.dispatch("terminal.keep", &json!({ "terminal": target, "place": "right" }), false, window, cx),
                                                "down" => this.dispatch("terminal.keep", &json!({ "terminal": target, "place": "down" }), false, window, cx),
                                                _ => this.dispatch("terminal.keep", &json!({ "terminal": target, "place": "tab" }), false, window, cx),
                                            }
                                        }
                                        if archive_source && kind == "handoff" && source_for_ui != target {
                                            this.dispatch("terminal.park", &json!({ "terminal": source_for_ui }), false, window, cx);
                                        }
                                    }
                                    this.sync(window, cx);
                                }
                            }
                            Err(e) => {
                                let err_msg = format!("{e:#}");
                                tracing::warn!("agent.handoff failed: {err_msg}");
                                if let Some(info) = Self::parse_worktree_blocked_error(&err_msg) {
                                    this.open_worktree_blocked_dialog(&source_for_ui, &info, window, cx);
                                } else {
                                    this.command_error("agent.handoff", &err_msg, window, cx);
                                }
                            }
                        }
                    });
                })
                .detach();
                return;
            }
            "agent.send" => {
                let target = Self::owned(a, "agent")
                    .or_else(|| Self::owned(a, "terminal"))
                    .filter(|s| !s.is_empty())
                    .or_else(|| self.peeked_agent.clone())
                    .or_else(|| self.model.focused_pane().and_then(|p| self.model.any_address_of(&p)))
                    .or_else(|| self.shown_agent())
                    .unwrap_or_default();
                if target.is_empty() {
                    return self.fail("agent.send needs a target agent or terminal");
                }
                let text = Self::owned(a, "text").unwrap_or_default();
                if text.trim().is_empty() {
                    return self.fail("agent.send needs non-empty text");
                }
                let steer = a.get("steer").and_then(Value::as_bool).unwrap_or(false);
                let focus = a.get("focus").and_then(Value::as_bool).unwrap_or(false);
                if focus {
                    self.dispatch("agent.focus", &json!({ "terminal": target }), false, window, cx);
                }
                let core = self.core.clone();
                let generation = self.core_generation;
                let target_for_ui = target.clone();
                let params = json!({
                    "agent": target,
                    "text": text,
                    "steer": steer,
                });
                cx.spawn_in(window, async move |this, cx| {
                    let res = cx
                        .background_executor()
                        .spawn(async move { core.call_timeout("agent.send", params, Duration::from_secs(15)) })
                        .await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        if generation != this.core_generation || this.draining {
                            return;
                        }
                        match res {
                            Ok(_) => {
                                if let Some(obj) = this.live.get_mut(&target_for_ui).and_then(Value::as_object_mut) {
                                    let next_seq = obj.get("transcript_seq").and_then(Value::as_u64).unwrap_or(0) + 1;
                                    obj.insert("transcript_seq".into(), json!(next_seq));
                                    if is_acp_addr(&target_for_ui) || obj.get("acp").and_then(Value::as_bool) == Some(true) {
                                        obj.insert("status".into(), json!("working"));
                                    }
                                }
                                mods::publish_terminals(this.ordered_live_values());
                                let zh = settings::get().language == settings::Language::ZhCn;
                                let notification = json!({
                                    "title": if zh { format!("已向 {target_for_ui} 发送消息") } else { format!("Message sent to {target_for_ui}") },
                                    "body": if zh { "当前 Agent 正在处理你的请求，处理完成后可重新创建 Worktree 分叉。" } else { "The current agent is handling your request; retry creating the Worktree fork once finished." },
                                    "level": "info",
                                    "action": { "name": "agent.focus", "args": { "terminal": target_for_ui } },
                                });
                                this.open_overlay(mods::new_overlay_id(), json!({ "kind": "toast", "notification": notification }), window, cx);
                                this.sync(window, cx);
                            }
                            Err(e) => {
                                let err_msg = format!("{e:#}");
                                tracing::warn!("agent.send failed: {err_msg}");
                                this.command_error("agent.send", &err_msg, window, cx);
                            }
                        }
                    });
                })
                .detach();
                return;
            }
            "agent.send_review" => {
                let req_id = Self::owned(a, "request_id").unwrap_or_default();
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let target = Self::owned(a, "target")
                    .filter(|s| !s.is_empty())
                    .or_else(|| self.peeked_agent.clone())
                    .or_else(|| self.model.focused_pane().and_then(|p| self.model.any_address_of(&p)))
                    .or_else(|| self.shown_agent())
                    .unwrap_or_default();
                if target.is_empty() {
                    let msg = "agent.send_review needs a target agent or terminal";
                    if !req_id.is_empty() {
                        mods::set_shared(
                            "diff_review.last_result".into(),
                            json!({ "request_id": req_id, "ok": false, "error": msg, "ts": now_ms }),
                        );
                    }
                    return self.fail(msg);
                }
                let Some(comments) = a.get("comments").filter(|v| v.as_array().is_some_and(|arr| !arr.is_empty())).cloned() else {
                    let msg = "agent.send_review requires a non-empty comments array";
                    if !req_id.is_empty() {
                        mods::set_shared(
                            "diff_review.last_result".into(),
                            json!({ "request_id": req_id, "ok": false, "target": target, "error": msg, "ts": now_ms }),
                        );
                    }
                    return self.fail(msg);
                };
                let is_zh = settings::get().language == settings::Language::ZhCn;
                let mut params = json!({
                    "target": target,
                    "comments": comments,
                    "steer": a.get("steer").and_then(Value::as_bool).unwrap_or(false),
                    "language": if is_zh { "zh-CN" } else { "en" },
                });
                if let Some(src) = Self::owned(a, "source").filter(|s| !s.is_empty()) {
                    params["source"] = json!(src);
                }
                if let Some(turn) = a.get("checkpoint_turn").and_then(Value::as_u64) {
                    params["checkpoint_turn"] = json!(turn);
                }
                if let Some(root) = Self::owned(a, "repo_root").filter(|s| !s.is_empty()) {
                    params["repo_root"] = json!(root);
                }
                if let Some(note) = Self::owned(a, "note").filter(|s| !s.is_empty()) {
                    params["note"] = json!(note);
                }

                let focus_after = a.get("focus").and_then(Value::as_bool).unwrap_or(false);
                let core = self.core.clone();
                let generation = self.core_generation;
                let target_for_ui = target.clone();
                cx.spawn_in(window, async move |this, cx| {
                    let res = cx
                        .background_executor()
                        .spawn(async move { core.call_timeout("agent.review", params, Duration::from_secs(20)) })
                        .await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        let ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        match res {
                            Ok(out) => {
                                if this.core_generation == generation {
                                    let returned_target = out.get("target").and_then(Value::as_str).unwrap_or_default();
                                    let resolved_target = if target_for_ui.contains('/') || returned_target.is_empty() {
                                        target_for_ui.clone()
                                    } else {
                                        returned_target.to_owned()
                                    };
                                    let target_kind = out.get("target_kind").and_then(Value::as_str).unwrap_or("agent").to_owned();
                                    let comment_count = out.get("comment_count").and_then(Value::as_u64).unwrap_or(0);
                                    let file_count = out.get("file_count").and_then(Value::as_u64).unwrap_or(0);
                                    let staged_path = out.get("staged_path").cloned().unwrap_or(Value::Null);
                                    mods::set_shared(
                                        "diff_review.last_result".into(),
                                        json!({
                                            "request_id": req_id,
                                            "ok": true,
                                            "target": resolved_target,
                                            "target_kind": target_kind,
                                            "comment_count": comment_count,
                                            "file_count": file_count,
                                            "staged_path": staged_path,
                                            "ts": ts,
                                        }),
                                    );
                                    let is_zh = settings::get().language == settings::Language::ZhCn;
                                    let title = if is_zh { "已发送文件行级消息" } else { "Line notes sent" };
                                    let body = if is_zh {
                                        format!("已将 {comment_count} 条行级消息（{file_count} 个文件）发送至 {resolved_target}，点击跳转查看。")
                                    } else {
                                        let nw = if comment_count == 1 { "note" } else { "notes" };
                                        let fw = if file_count == 1 { "file" } else { "files" };
                                        format!("Sent {comment_count} {nw} ({file_count} {fw}) to {resolved_target}. Click to focus.")
                                    };
                                    let notification = json!({
                                        "title": title,
                                        "body": body,
                                        "level": "success",
                                        "action": { "name": "agent.focus", "args": { "terminal": resolved_target } },
                                    });
                                    this.open_overlay(mods::new_overlay_id(), json!({ "kind": "toast", "notification": notification }), window, cx);
                                    if focus_after {
                                        this.dispatch("agent.focus", &json!({ "terminal": resolved_target }), false, window, cx);
                                    }
                                    this.sync(window, cx);
                                }
                            }
                            Err(e) => {
                                let err_msg = format!("{e:#}");
                                tracing::warn!("agent.send_review failed: {err_msg}");
                                mods::set_shared(
                                    "diff_review.last_result".into(),
                                    json!({
                                        "request_id": req_id,
                                        "ok": false,
                                        "target": target_for_ui,
                                        "error": err_msg,
                                        "ts": ts,
                                    }),
                                );
                                this.command_error("agent.send_review", &err_msg, window, cx);
                                cx.notify();
                            }
                        }
                    });
                })
                .detach();
                return;
            }
            "agent.next" | "agent.prev" => {
                let order = self.agent_order();
                if order.is_empty() {
                    return;
                }
                let current = self.shown_agent().map(|a| self.root_agent(&a));
                let n = order.len();
                let next = match current.and_then(|c| order.iter().position(|a| *a == c)) {
                    Some(i) if name == "agent.next" => (i + 1) % n,
                    Some(i) => (i + n - 1) % n,
                    None if name == "agent.next" => 0,
                    None => n - 1,
                };
                let addr = order[next].clone();
                self.focus_agent(&addr);
            }
            // ---- layout
            "layout.switch" => {
                self.switch_layout(Self::arg(a, "name"), window, cx);
                return;
            }
            "layout.clone" => {
                self.save();
                let name = Self::owned(a, "name").unwrap_or_else(|| self.layout_name.clone());
                let to = Self::owned(a, "as").unwrap_or_else(|| format!("{name}-copy"));
                if let Err(e) = clone_layout(&name, &to) {
                    return self.fail(format!("cannot clone layout: {e}"));
                }
                self.switch_layout(Some(&to), window, cx);
                return;
            }
            "mod.clone" => {
                let name = Self::arg(a, "name").unwrap_or_default();
                let to = Self::owned(a, "as").unwrap_or_else(|| format!("{name}-copy"));
                if let Err(e) = mods::clone_mod(name, &to) {
                    return self.fail(format!("cannot clone mod: {e}"));
                }
                self.folders = mods::scan().into_iter().map(|f| (f.id.clone(), f)).collect();
            }
            "layout.set" if shipped_layouts().contains(&self.layout_name.as_str()) => {
                let n = match serde_json::from_value::<Node>(a.get("layout").cloned().unwrap_or_default()) {
                    Ok(n) => n,
                    Err(e) => return self.fail(format!("layout: {e}")),
                };
                if layout_sync::without_clicks(&self.chrome, &n) != self.chrome {
                    return self.fail("Only supported click/look fields can change on Herdr's own layouts: clone one for other changes");
                }
                let mut state = layout_sync::State::between(&self.file_chrome, &n);
                if n.look.theme == self.chrome.look.theme {
                    state.theme = self.picked_theme.clone();
                }
                self.chrome = self.file_chrome.clone();
                state.apply(&mut self.chrome);
                self.picked_theme = state.theme;
                self.apply_look(window, cx);
            }
            // A deliberate edit: the file becomes this tree, clicks start over.
            "layout.set" => match serde_json::from_value::<Node>(a.get("layout").cloned().unwrap_or_default()) {
                Ok(mut n) => {
                    fill_ids(&mut n, &mut self.model);
                    let text = layout_sync::text_of(&n);
                    if !write_atomic(&layout_path(&self.layout_name), &text) {
                        return self.fail(format!("cannot write layouts/{}.json", self.layout_name));
                    }
                    self.written_layout = text;
                    self.layout_file_ok = true;
                    self.file_chrome = n.clone();
                    self.chrome = n;
                    self.picked_theme = None;
                    self.write_state(&layout_sync::State::default());
                    self.apply_look(window, cx);
                }
                Err(e) => return self.fail(format!("layout: {e}")),
            },
            "layout.toggle" | "layout.show" | "layout.hide" => {
                let id = Self::arg(a, "id").unwrap_or_default();
                let Some(n) = self.chrome.find_mut(id) else { return self.fail(format!("no node {id:?} in the shown layout")) };
                n.hidden = match name {
                    "layout.toggle" => !n.hidden,
                    "layout.show" => false,
                    _ => true,
                };
                if id == "panel" {
                    self.side_panel_auto_hidden = false;
                }
            }
            "layout.resize" => {
                let id = Self::arg(a, "id").unwrap_or_default();
                let Some(n) = self.find_node_mut(id) else { return self.fail(format!("no node {id:?}")) };
                if let Some(px) = a.get("px").and_then(Value::as_f64) {
                    n.size = Some(Span::Px { px: px as f32 });
                } else if let Some(w) = a.get("weight").and_then(Value::as_f64) {
                    n.size = Some(Span::Weight { weight: w as f32 });
                }
            }
            "layout.reset" => {
                let name = Self::owned(a, "name").unwrap_or_else(|| self.layout_name.clone());
                let Some((_, text)) = LAYOUTS.iter().find(|(n, _)| *n == name) else {
                    return self.fail(format!("{name:?} isn't a layout Herdr ships"));
                };
                // Theirs is kept under a name nothing else has (never over an older one).
                if layout_path(&name).exists() {
                    if let Err(e) = std::fs::rename(layout_path(&name), aside(&layout_path(&name), "old")) {
                        return self.fail(format!("cannot set layouts/{name}.json aside: {e}"));
                    }
                }
                if !write_atomic(&layout_path(&name), text) {
                    return self.fail(format!("cannot write layouts/{name}.json"));
                }
                let _ = std::fs::remove_file(state_path(&name));
                if name == self.layout_name {
                    self.load_layout(&name);
                    self.apply_look(window, cx);
                }
            }
            "layout.select" => {
                let (s, ch) = (Self::arg(a, "stack").unwrap_or_default(), Self::arg(a, "child").unwrap_or_default());
                if !self.chrome.select(s, ch) {
                    return self.fail(format!("no stack {s:?} with a child {ch:?}"));
                }
                if s == "panel-body" && ch != "side-chat" {
                    self.side_panel_auto_hidden = false;
                }
            }
            "files.open" => {
                let raw_path = Self::arg(a, "path").unwrap_or_default();
                if raw_path.trim().is_empty() {
                    return self.fail("files.open: path is required");
                }
                let line = a.get("line").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0);
                let end_line = a.get("end_line").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0);
                let on = Self::arg(a, "on").filter(|s| !s.is_empty());
                let cwd = Self::arg(a, "cwd").filter(|s| !s.is_empty());
                self.open_file_in_panel(raw_path, line, end_line, on, cwd, window, cx);
                return;
            }
            "mod.open" => {
                let name = Self::arg(a, "name").unwrap_or_default();
                let Some(f) = self.folders.get(name) else { return self.fail(format!("no mod folder {name:?}")) };
                let mut spec = mod_overlay(&f.dir);
                spec["mod"] = json!(name);
                // One popup per mod: running mod.open again toggles it, like app.settings.
                let id = format!("mod-{name}");
                if self.overlays.iter().any(|o| o.id == id) {
                    self.close_overlay(&id, window, cx);
                } else {
                    self.open_overlay(id, spec, window, cx);
                }
            }
            "overlay.close" => {
                let id = Self::owned(a, "id").or_else(|| self.overlays.iter().rev().find(|o| o.kind.modal_keys()).map(|o| o.id.clone()));
                if let Some(id) = id {
                    self.close_overlay(&id, window, cx);
                }
                return;
            }
            // ---- the app
            "app.welcome" => {
                self.open_welcome(window, cx);
                return;
            }
            "app.palette" => {
                // Once: a second press closes the palette instead of stacking another.
                if self.overlays.iter().any(|o| o.id == "palette") {
                    self.close_overlay("palette", window, cx);
                } else {
                    let spec = json!({ "mod": "frame-palette", "kind": "popup", "at": "top", "offset": 60, "width": 620, "height": 0.85 });
                    self.open_overlay("palette".into(), spec, window, cx);
                }
                return;
            }
            "app.feedback" => {
                return self.fail("feedback is disabled in Herdr2");
            }
            "app.settings" => {
                // Once: a second press doesn't stack another; asking for a section opens it there.
                let section = Self::owned(a, "section");
                let open = self.overlays.iter().any(|o| o.id == "settings");
                if open && section.is_some() {
                    self.close_overlay("settings", window, cx);
                }
                if !open || section.is_some() {
                    let spec = json!({ "mod": "frame-settings", "kind": "dialog", "width": 920, "props": { "section": section } });
                    self.open_overlay("settings".into(), spec, window, cx);
                }
                return;
            }
            "app.help" => {
                if self.overlays.iter().any(|o| o.id == "help") {
                    self.close_overlay("help", window, cx);
                } else {
                    let spec = json!({ "mod": "help", "kind": "dialog", "width": 640, "height": 520 });
                    self.open_overlay("help".into(), spec, window, cx);
                }
                return;
            }
            "app.reset" => self.reset(window, cx),
            "app.theme" => {
                let name = Self::arg(a, "name").unwrap_or_default().to_owned();
                if !settings::theme_exists(&name) {
                    return self.fail(format!("no theme {name:?}"));
                }
                // A layout is a look: the theme goes on the shown layout's root,
                // as its light or its dark half (the other half stays).
                let current = crate::layout::ThemeChoice::Pair {
                    light: settings::get().theme_light.clone(),
                    dark: settings::get().theme_dark.clone(),
                };
                self.chrome.look.theme = Some(with_theme(Some(current), name));
                self.picked_theme = self.chrome.look.theme.clone();
                self.save();
                self.apply_look(window, cx);
            }
            "app.language" => {
                let raw = Self::arg(a, "language").unwrap_or_default();
                let Some(lang) = settings::Language::parse_arg(raw) else {
                    return self.fail(format!("app.language language is \"en\" or \"zh-CN\", not {raw:?}"));
                };
                self.write_person(|p| p.language = lang, window, cx);
            }
            "app.zoom_in" | "app.zoom_out" | "app.zoom_reset" => {
                let zoom = settings::next_zoom(match name {
                    "app.zoom_in" => 1,
                    "app.zoom_out" => -1,
                    _ => 0,
                });
                self.write_person(|p| p.zoom = zoom, window, cx);
            }
            "app.quit" => cx.quit(),
            "window.close" | "window.minimize" | "window.maximize" => return self.window_command(name, window, cx),
            // Hiding apps is macOS's (bound natively there).
            "app.hide" | "app.hide_others" | "app.show_all" => return,
            other => {
                tracing::warn!("command {other:?} is listed but not handled");
                return;
            }
        }
        self.sync(window, cx);
    }

    fn find_node_mut(&mut self, id: &str) -> Option<&mut Node> {
        if self.chrome.find(id).is_some() {
            return self.chrome.find_mut(id);
        }
        self.model.spaces.iter_mut().flat_map(|p| p.tabs.iter_mut()).find_map(|c| c.root.find_mut(id))
    }

    /// The nearest visible pane, also considering focusable mods for `pane.go`.
    fn neighbor(&self, from: &str, dir: &str, use_history: bool) -> Option<String> {
        let bounds = self.bounds.borrow();
        let cur = *bounds.get(from)?;
        let mut candidates: Vec<String> = self.model.active_tab().into_iter()
            .flat_map(|c| c.shown().visible_blocks().into_iter().filter(|n| n.is_pane()).map(|n| n.id.clone()))
            .collect();
        if use_history {
            candidates.extend(self.chrome.visible_blocks().into_iter().filter(|n| self.focusable(&n.id)).map(|n| n.id.clone()));
        }
        let candidates = candidates.into_iter().filter(|id| id != from).filter_map(|id| Some((id.clone(), *bounds.get(&id)?)));
        nearest(cur, dir, candidates, if use_history { &self.pane_history } else { &[] })
    }

    /// `pane.go`: the nearest pane or focusable mod.
    fn go(&mut self, dir: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.focused_block.clone().or_else(|| self.model.focused_pane()) else { return };
        let to = self.model.active_tab().filter(|tab| tab.zoomed.as_deref() == Some(from.as_str()))
            .and_then(|tab| self.unzoomed_neighbor(tab, &from, dir))
            .or_else(|| self.neighbor(&from, dir, true));
        let Some(to) = to else { return };
        if self.focusable(&to) {
            return self.focus_block(&to, window, cx);
        }
        self.focused_block = None;
        self.model.focus_pane(&to);
        self.focused_pane = None;
        self.sync(window, cx);
    }

    /// Until the mod takes the keyboard, the window holds the keys.
    fn focus_block(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.tab_of(id).is_some() {
            self.model.focus_pane(id);
            self.focused_pane = None;
        }
        self.focused_block = Some(id.to_owned());
        self.sync(window, cx);
        if !self.overlays.iter().any(|o| o.kind.modal_keys()) {
            self.focus.focus(window, cx);
            self.hand_keyboard();
        }
    }

    /// Its copy gets `focus` from `nextCommand()`.
    fn hand_keyboard(&self) {
        if let Some(copy) = self.focused_block.as_ref().and_then(|b| self.copies.get(b)) {
            mods::send_command_to(copy.id, "focus");
        }
    }

    /// A mod block on screen: in the layout, or a mod pane of the shown tab.
    fn mod_block_shown(&self, id: &str) -> bool {
        self.copies.contains_key(id)
            && (self.chrome.visible_blocks().iter().any(|n| n.id == id && matches!(n.content(), Some(Content::Mod { .. })))
                || self.model.active_tab().is_some_and(|t| t.shown().visible_blocks().iter().any(|n| n.id == id)))
    }

    /// Its mod.json says `"focusable": true` (`go` asks only of visible blocks).
    fn focusable(&self, id: &str) -> bool {
        self.copies.get(id).and_then(|c| self.folders.get(&c.inst.name)).is_some_and(|f| f.hint.focusable)
    }

    fn swap_pane(&mut self, from: &str, explicit: bool, dir: &str) -> Result<(), String> {
        let neighbor = self.neighbor(from, dir, false);
        swap_with_neighbor(&mut self.model, from, explicit, neighbor)
    }

    // ---- overlays ----------------------------------------------------------

    fn confirm(&mut self, title: &str, detail: &str, command: &str, args: Value, window: &mut Window, cx: &mut Context<Self>) {
        let spec = json!({ "mod": "confirm", "kind": "dialog", "width": 380,
            "props": { "title": title, "detail": detail, "command": { "name": command, "args": args } } });
        self.open_overlay(mods::new_overlay_id(), spec, window, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn open_menu(&mut self, tab: &str, target: Value, at: Option<Point<Pixels>>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = at {
            self.pointer = p;
        }
        let requested = self.chrome.terminal_menu.as_deref().unwrap_or("menu");
        let name = if self.folders.contains_key(requested) {
            requested
        } else {
            tracing::warn!(mod_name = requested, "terminal menu mod not found; using menu");
            "menu"
        };
        let defaults = self.folders.get(name).map(|f| mod_overlay(&f.dir)).unwrap_or_else(|| json!({}));
        let spec = terminal_menu_spec(name, &defaults, tab, target);
        self.open_overlay(mods::new_overlay_id(), spec, window, cx);
    }

    /// The layout's notification mod (`"toast"` on the root), else the shipped `toast`.
    fn toast_mod(&self) -> &str {
        let requested = self.chrome.toast.as_deref().unwrap_or("toast");
        if self.folders.contains_key(requested) {
            requested
        } else {
            tracing::warn!(mod_name = requested, "toast mod not found; using toast");
            "toast"
        }
    }

    fn open_overlay(&mut self, id: String, spec: Value, window: &mut Window, cx: &mut Context<Self>) {
        let kind = match spec.get("kind").and_then(Value::as_str) {
            Some("popup") => OverlayKind::Popup,
            Some("toast") => OverlayKind::Toast,
            Some("window") => OverlayKind::Window,
            _ => OverlayKind::Dialog,
        };
        let key = spec.get("key").and_then(Value::as_str).filter(|k| !k.is_empty() && kind == OverlayKind::Window).map(str::to_owned);
        // One window per name: opening it again replaces it, in its place.
        let mut place = key.as_deref().and_then(movable::place_of);
        if let Some(k) = &key
            && let Some(old) = self.overlays.iter().find(|o| o.key.as_ref() == Some(k)).map(|o| (o.id.clone(), o.place))
        {
            place = old.1.or(place);
            self.remove_overlay(&old.0, window, cx);
        }
        let layout = spec.get("layout").filter(|_| kind != OverlayKind::Toast).and_then(|l| match serde_json::from_value::<Node>(l.clone()) {
            Ok(n) => Some(overlay_layout(n, &id)),
            Err(e) => {
                tracing::warn!("overlay layout: {e}");
                None
            }
        });
        let (name, mut props) = if kind == OverlayKind::Toast {
            (self.toast_mod().to_owned(), spec.get("notification").cloned().unwrap_or(Value::Null))
        } else {
            (spec.get("mod").and_then(Value::as_str).unwrap_or_default().to_owned(), spec.get("props").cloned().unwrap_or(json!({})))
        };
        if !props.is_object() {
            props = json!({});
        }
        props["overlay"] = json!(id);
        let num = |k: &str, d: f32| spec.get(k).and_then(Value::as_f64).map_or(d, |v| v as f32);
        let resizable = kind == OverlayKind::Window && spec.get("resizable").and_then(Value::as_bool).unwrap_or(false);
        let min_size = size(num("min_width", 320.0), num("min_height", 240.0));
        let mut size = match kind {
            OverlayKind::Toast => size(340.0, num("height", 72.0)),
            // No size: as big as its content (0 = unset).
            OverlayKind::Window => size(num("width", 0.0), num("height", 0.0)),
            // No height: as tall as the content, up to most of the window.
            _ => size(num("width", 420.0), num("height", 0.85)),
        };
        if resizable
            && let Some(saved) = key.as_deref().and_then(movable::size_of)
        {
            size = gpui::size(saved.width.max(min_size.width), saved.height.max(min_size.height));
        }
        let at = match spec.get("at") {
            Some(v) if v.get("x").is_some() => point(px(num_in(v, "x")), px(num_in(v, "y"))),
            _ => self.pointer,
        };
        let top = (spec.get("at").and_then(Value::as_str) == Some("top")).then(|| num("offset", 0.0));
        let fit = !resizable
            && layout.is_none()
            && (kind == OverlayKind::Popup || (matches!(kind, OverlayKind::Dialog | OverlayKind::Window) && spec.get("height").is_none()));
        if kind == OverlayKind::Window && place.is_none() && spec.get("at").is_some_and(|v| v.get("x").is_some()) {
            place = Some(at);
        }
        let timeout = props.get("timeout").and_then(Value::as_f64).unwrap_or(match props.get("level").and_then(Value::as_str) {
            Some("error" | "warning") => 8000.0,
            _ => 5000.0,
        });
        let slot = format!("overlay:{id}");
        // The frame takes the keyboard first, so a mod that focuses itself while loading wins.
        let focus = cx.focus_handle();
        if kind.modal_keys() {
            focus.focus(window, cx);
        }
        if layout.is_none() {
            let copy = self.load(Instance { name, props }, window, cx);
            // Popups (menus) are as tall as what's in them; `height` is the most.
            if fit
                && let Some(root) = &copy.root
            {
                root.update(cx, |r, cx| r.set_fit_content(true, cx));
            }
            self.copies.insert(slot.clone(), copy);
        }
        let first_term = layout.as_ref().and_then(|l| l.visible_blocks().into_iter().find(|n| matches!(n.content(), Some(Content::Terminal { .. }))).map(|n| n.id.clone()));
        let has_layout = layout.is_some();
        self.overlays.push(Overlay {
            id,
            kind,
            slot,
            at,
            size,
            expires: (kind == OverlayKind::Toast).then(|| Instant::now() + Duration::from_millis(timeout as u64)),
            focus,
            layout,
            top,
            fit,
            modal: spec.get("modal").and_then(Value::as_bool).unwrap_or(false),
            bare: spec.get("frame").and_then(Value::as_str) == Some("none"),
            place,
            key: key.clone(),
            resizable,
            min_size,
            frame: spec.get("mod").and_then(Value::as_str).is_some_and(|m| m.starts_with("frame-")),
        });
        if let Some(k) = &key {
            if spec.get("keep").and_then(Value::as_bool) == Some(true) {
                movable::keep(&self.layout_name, k, &spec);
            } else {
                movable::forget(&self.layout_name, k);
            }
        }
        if has_layout {
            self.sync(window, cx);
            if kind.modal_keys()
                && let Some(t) = first_term.and_then(|b| self.terms.get(&b))
            {
                t.focus.focus(window, cx);
            }
        } else {
            self.publish_shown_copies();
        }
        cx.notify();
    }

    fn open_side_chat_panel(&mut self, address: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.side_panel_auto_hidden = false;
        let parent_addr = self
            .live
            .get(address)
            .or_else(|| self.ended_acp.get(address))
            .and_then(|r| r.pointer("/lineage/parent").or_else(|| r.get("parent")))
            .and_then(Value::as_str)
            .map(|p| Self::resolve_peer_addr(p, address))
            .filter(|p| !p.is_empty());
        if let Some(ref p_addr) = parent_addr
            && p_addr != address
            && !self.is_side_chat(p_addr)
            && self.focused_main_terminal().as_deref() != Some(p_addr.as_str())
            && (self.live.contains_key(p_addr) || self.ended_acp.contains_key(p_addr))
        {
            self.dispatch("agent.focus", &json!({ "terminal": p_addr }), false, window, cx);
        }
        mods::set_shared("side-chat.active".to_string(), json!(address));
        let has_side_block = self.chrome.find("side-chat").is_some();
        if has_side_block {
            self.dispatch("layout.select", &json!({ "stack": "panel-body", "child": "side-chat" }), false, window, cx);
            self.dispatch("layout.show", &json!({ "id": "panel" }), false, window, cx);
            if self.side_chat_terminal_mode().as_deref() == Some(address)
                && let Some(t) = self.terms.get("side-chat#term")
            {
                t.focus.focus(window, cx);
            }
            self.refresh_snapshot_mods(cx);
            self.publish(cx);
        } else {
            let parent = parent_addr.unwrap_or_default();
            self.open_side_chat_window(address, &parent, window, cx);
        }
    }

    fn open_file_in_panel(
        &mut self,
        raw_path: &str,
        line: Option<u32>,
        end_line: Option<u32>,
        on: Option<&str>,
        cwd: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let trimmed = raw_path.trim();
        if trimmed.is_empty() {
            return;
        }
        let active_space = self.model.active_space();
        let effective_on = on
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| active_space.and_then(|s| s.machine.clone()));
        let base_cwd = cwd
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                self.focused_pane
                    .as_deref()
                    .and_then(|pid| self.terms.get(pid))
                    .and_then(|t| self.cwds.get(&t.address).cloned())
            })
            .or_else(|| active_space.map(|s| s.folder.clone()));

        let resolved = if effective_on.is_none() {
            let expanded = if let Some(rest) = trimmed.strip_prefix("~/") {
                std::env::home_dir()
                    .map(|h| h.join(rest))
                    .unwrap_or_else(|| std::path::PathBuf::from(trimmed))
            } else {
                std::path::PathBuf::from(trimmed)
            };
            let joined = if expanded.is_absolute() {
                expanded
            } else if let Some(ref base) = base_cwd {
                std::path::Path::new(base).join(expanded)
            } else {
                expanded
            };
            let mut normalized = std::path::PathBuf::new();
            for comp in joined.components() {
                match comp {
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        normalized.pop();
                    }
                    other => normalized.push(other),
                }
            }
            if normalized.is_dir() {
                crate::links::open(&crate::links::Target::File(normalized));
                return;
            }
            normalized.to_string_lossy().into_owned()
        } else if trimmed.starts_with('/') || trimmed.starts_with("~/") {
            trimmed.to_owned()
        } else if let Some(ref base) = base_cwd {
            format!("{}/{}", base.trim_end_matches('/'), trimmed.trim_start_matches("./"))
        } else {
            trimmed.to_owned()
        };

        self.file_open_seq = self.file_open_seq.wrapping_add(1).max(1);
        mods::set_shared(
            "files.open_request".to_string(),
            json!({
                "seq": self.file_open_seq,
                "path": resolved,
                "line": line,
                "end_line": end_line,
                "on": effective_on,
            }),
        );
        if self.chrome.find("files").is_some() {
            self.dispatch("layout.select", &json!({ "stack": "panel-body", "child": "files" }), false, window, cx);
            self.dispatch("layout.show", &json!({ "id": "panel" }), false, window, cx);
            self.refresh_snapshot_mods(cx);
            self.publish(cx);
            cx.notify();
        } else {
            self.open_overlay(
                "files-preview".to_string(),
                json!({
                    "kind": "window",
                    "key": "files-preview",
                    "resizable": true,
                    "width": 720,
                    "height": 640,
                    "mod": "files",
                    "props": {
                        "file": resolved,
                        "line": line,
                        "end_line": end_line,
                        "on": effective_on,
                    }
                }),
                window,
                cx,
            );
        }
    }

    fn open_side_chat_window(&mut self, address: &str, parent: &str, window: &mut Window, cx: &mut Context<Self>) {
        let is_acp = is_acp_addr(address)
            || self.live.get(address).and_then(|r| r.get("acp")).and_then(Value::as_bool) == Some(true);
        let ov_id = format!("side-chat:{address}");
        if is_acp {
            self.open_overlay(
                ov_id,
                json!({
                    "kind": "window",
                    "key": "side-chat",
                    "resizable": true,
                    "width": 520,
                    "height": 640,
                    "min_width": 360,
                    "min_height": 340,
                    "mod": "peek",
                    "props": { "terminal": address, "acp": true, "floating": true, "parent": parent }
                }),
                window,
                cx,
            );
        } else {
            self.open_overlay(
                ov_id,
                json!({
                    "kind": "window",
                    "key": "side-chat",
                    "resizable": true,
                    "width": 620,
                    "height": 520,
                    "min_width": 380,
                    "min_height": 320,
                    "layout": {
                        "type": "column",
                        "children": [
                            { "type": "block", "size": { "px": 36 }, "content": { "mod": "peek", "props": { "terminal": address, "floating": true, "parent": parent } } },
                            { "type": "block", "content": { "terminal": address } }
                        ]
                    }
                }),
                window,
                cx,
            );
        }
        self.publish(cx);
    }

    fn peek_terminal(&mut self, address: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlay("terminal-peek", window, cx);
        let is_acp = is_acp_addr(address)
            || self.live.get(address).and_then(|r| r.get("acp")).and_then(Value::as_bool) == Some(true);
        if self.is_side_chat(address) {
            if self.evict_side_chat_from_tabs(address, cx) {
                self.sync(window, cx);
                self.flush_documents(false, window, cx);
            }
            self.open_side_chat_panel(address, window, cx);
            return;
        }
        if is_acp {
            if self.focus_mounted_acp(address, window, cx) {
                return;
            }
            self.peeked_agent = Some(address.to_owned());
            self.open_overlay("terminal-peek".into(), json!({
                "kind": "dialog",
                "frame": "none",
                "width": 0.8,
                "height": 0.85,
                "mod": "peek",
                "props": { "terminal": address, "acp": true }
            }), window, cx);
            self.publish(cx);
            return;
        }
        self.open_overlay("terminal-peek".into(), json!({
            "kind": "dialog",
            "width": 0.8,
            "height": 0.8,
            "layout": {
                "type": "column",
                "children": [
                    { "type": "block", "size": { "px": 34 }, "content": { "mod": "peek", "props": { "terminal": address } } },
                    { "type": "block", "content": { "terminal": address } }
                ]
            }
        }), window, cx);
    }

    /// Which copies are on screen, for `<mod>.<command>` (`mods::send_command`).
    fn publish_shown_copies(&self) {
        let (_, shown) = self.visible();
        mods::set_shown_copies(shown.iter().filter_map(|(s, i)| self.copies.get(s).filter(|c| c.inst == *i)).map(|c| c.id).collect());
    }

    /// Closes an overlay; a kept window stays closed on the next start too.
    fn close_overlay(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if id == "core-update-error" {
            self.core_update_error_dismissed = crate::update::core().error;
            cx.notify();
            return;
        }
        if let Some(k) = self.overlays.iter().find(|o| o.id == id).and_then(|o| o.key.clone()) {
            movable::forget(&self.layout_name, &k);
        }
        self.remove_overlay(id, window, cx);
    }

    fn remove_overlay(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if id == "terminal-peek" {
            self.peeked_agent = None;
        }
        if let Some(i) = self.overlays.iter().position(|o| o.id == id) {
            let o = self.overlays.remove(i);
            if self.win_drag.as_ref().is_some_and(|d| d.id == o.id) {
                self.win_drag = None;
            }
            self.bounds.borrow_mut().remove(&o.slot);
            self.remove_copy(&o.slot, window, cx);
            if o.layout.is_some() {
                self.sync(window, cx);
            } else {
                self.publish_shown_copies();
            }
            // The keyboard never ends up nowhere: back to a dialog still open,
            // else the pane, else the window (where the app's keys work). A
            // window in the background had none to give back (one that had it:
            // `restore_focus` runs on its own when its focus goes).
            if o.kind.modal_keys() {
                self.restore_focus(window, cx);
            }
            cx.notify();
        }
    }

    /// The escape hatch: set the files aside and go back to the default screen.
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let layout = format!("layouts/{}.json", self.layout_name);
        for name in [layout.as_str(), "keys.json"] {
            if file(name).exists() {
                let _ = std::fs::rename(file(name), aside(&file(name), "reset"));
            }
        }
        let _ = std::fs::remove_file(state_path(&self.layout_name));
        tracing::warn!("escape hatch: {layout} and keys.json set aside as *.reset-<time>");
        self.watched.keys = None;
        install_layouts();
        self.drop_layout_copies(window, cx);
        self.load_layout("main");
        self.apply_look(window, cx);
        self.focused_pane = None;
        self.agent_focused = None;
        let ids: Vec<String> = self.overlays.iter().map(|o| o.id.clone()).collect();
        for id in ids {
            self.close_overlay(&id, window, cx);
        }
        self.show_layout_notes(window, cx);
        self.reload_config(window, cx);
    }

    fn handle(&mut self, req: Request, window: &mut Window, cx: &mut Context<Self>) {
        match req {
            Request::Run { name, args } => self.run(&name, args, window, cx),
            Request::OpenOverlay { id, spec } => self.open_overlay(id, spec, window, cx),
            Request::CloseOverlay { id } => self.close_overlay(&id, window, cx),
            Request::SetWindowTitle(title) => window.set_window_title(title.as_deref().unwrap_or("Herdr2")),
            Request::SetBadge(text) => crate::badge::set(text.as_deref()),
            Request::Refresh => self.refresh_mods(cx),
            Request::Control { cmd, reply } => {
                let _ = reply.send(self.control(cmd, window, cx));
            }
        }
    }

    fn control(&mut self, cmd: Value, window: &mut Window, cx: &mut Context<Self>) -> Value {
        if let Some(name) = cmd.get("do").and_then(Value::as_str) {
            let args = cmd.get("args").cloned().unwrap_or(Value::Null);
            return self.run_as(name, args, false, window, cx);
        }
        if cmd.get("commands").is_some() {
            return crate::commands::listing();
        }
        // Herdr was launched again: this window comes to the front instead.
        if cmd.get("raise").is_some() {
            cx.activate(true);
            window.activate_window();
            return json!({ "ok": true });
        }
        if cmd.get("ls").is_some() {
            return self.world();
        }
        if cmd.get("status").is_some() {
            let round = |v: f32| (v as f64 * 100.0).round() / 100.0;
            let errors: Vec<String> = ["settings", "keys", "layout", "programs"]
                .iter()
                .filter_map(|w| std::fs::read_to_string(file(&format!("{w}.error"))).ok())
                .chain(self.copies.values().filter_map(|c| Some(format!("mod {}: {}", c.inst.name, c.error.as_ref()?))))
                .chain(self.documents.iter().filter_map(|(m, s)| s.error.as_ref().map(|e| format!("layout {}: {e}", if m.is_empty() { "local" } else { m }))))
                .collect();
            return json!({
                "zoom": round(settings::zoom()),
                "font_size": round(settings::font_size()),
                "ui_font_size": round(settings::ui_font_size()),
                "space": self.model.active,
                "tab": self.model.active_tab().map(|c| c.id.clone()),
                "focused_pane": self.focused_pane,
                "layout": self.layout_name,
                "agent": self.shown_agent(),
                "floats": self.float_status(),
                "overlays": self.overlays.iter().map(|o| match &o.layout {
                    Some(_) => Some("layout".to_owned()),
                    None => self.copies.get(&o.slot).map(|c| c.inst.name.clone()),
                }).collect::<Vec<_>>(),
                "errors": errors,
            });
        }
        json!({ "error": "expected { do }, { ls }, { commands } or { status }" })
    }

    /// Spaces, their tabs and terminals, for `herdr-app ls`. Sub-agents are listed
    /// under the terminal that started them.
    fn world(&self) -> Value {
        let m = &self.model;
        let children = |addr: &str| -> Vec<String> {
            let mut out: Vec<String> = self
                .live
                .iter()
                .filter(|(a, r)| !self.closing_terminals.contains_key(*a) && !is_trashed_record(r) && r.get("parent").and_then(Value::as_str) == Some(addr))
                .map(|(a, _)| a.clone())
                .collect();
            out.sort();
            out
        };
        let terminal = |pane: &str, addr: &str| {
            let rec = self.live.get(addr);
            json!({
                "terminal": addr, "pane": pane, "status": m.status(addr),
                "app": m.agents.get(addr).and_then(|a| a.app.clone()),
                "program": self.program(addr),
                "name": rec.and_then(|r| r.get("name")).cloned(),
                "sub_agents": children(addr),
            })
        };
        let pane_addrs: HashSet<String> = m.spaces.iter().flat_map(|p| p.tabs.iter().flat_map(|c| c.root.panes().into_iter().filter_map(|n| n.any_terminal().map(str::to_owned)))).collect();
        let spaces: Vec<Value> = m
            .spaces
            .iter()
            .map(|p| {
                let active = p.active_tab().map(|c| c.id.clone());
                let tabs: Vec<Value> = p
                    .tabs
                    .iter()
                    .map(|c| {
                        let terms: Vec<Value> = c.root.panes().iter().filter_map(|n| Some(terminal(&n.id, n.any_terminal()?))).collect();
                        json!({ "id": c.id, "name": Model::tab_name(p, c), "active": Some(&c.id) == active.as_ref(), "settled": c.settled.is_some(), "terminals": terms })
                    })
                    .collect();
                json!({ "id": p.id, "name": m.space_name(p, &home()), "folder": p.folder, "machine": p.machine, "active": m.active.as_deref() == Some(&p.id), "tabs": tabs })
            })
            .collect();
        let mut headless: Vec<Value> = self
            .live
            .iter()
            .filter(|(addr, r)| {
                !self.closing_terminals.contains_key(*addr)
                    && !is_trashed_record(r)
                    && r.get("acp").and_then(Value::as_bool) == Some(true)
                    && r.get("state").and_then(Value::as_str) == Some("live")
                    && !pane_addrs.contains(*addr)
                    && !has_pane_ancestor(addr, &self.live, &pane_addrs)
            })
            .map(|(addr, r)| {
                json!({
                    "address": addr,
                    "name": r.get("name").cloned().unwrap_or(Value::Null),
                    "agent": r.get("agent").cloned().unwrap_or(Value::Null),
                    "status": m.status(addr),
                    "program": self.program(addr),
                    "parent": r.get("parent").cloned().unwrap_or(Value::Null),
                    "cwd": r.get("cwd").cloned().unwrap_or(Value::Null),
                    "sub_agents": children(addr),
                })
            })
            .collect();
        headless.sort_by(|a, b| a["address"].as_str().cmp(&b["address"].as_str()));
        json!({ "spaces": spaces, "headless_agents": headless, "recent": m.recent })
    }

    // ---- mods on disk ----------------------------------------------------

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_documents(false, window, cx);
        let ready = Self::update_label();
        if ready != self.update_ready {
            self.update_ready = ready;
            cx.notify();
        }
        // The settings screen shows versions and progress: redraw mods when they move.
        let status = crate::update::status().to_string();
        if status != self.update_status {
            reconcile_core_update_error(&mut self.core_update_error_dismissed, &crate::update::core());
            self.update_status = status;
            self.refresh_mods(cx);
        }
        if let Some(r) = &self.runtime {
            crate::bench::note_mods(&r.read_metrics());
        }
        // With file events the disk is only checked now and then, in case one
        // was missed.
        self.polls += 1;
        if self.polls.is_multiple_of(25) {
            self.observe();
        }
        if self.file_events.is_none() || self.polls.is_multiple_of(25) {
            self.reload_config(window, cx);
            self.check_layout_file(window, cx);
            self.sync_folders(window, cx);
        }
        let now = Instant::now();
        let expired: Vec<String> = self.overlays.iter().filter(|o| o.expires.is_some_and(|e| e <= now)).map(|o| o.id.clone()).collect();
        for id in expired {
            self.close_overlay(&id, window, cx);
        }
        let active = window.is_window_active();
        if active != self.window_active {
            self.window_active = active;
            if active {
                if let Some(blurred_at) = self.blurred_at.take() {
                    let away_secs = now.saturating_duration_since(blurred_at).as_secs();
                    const DEFAULT_AWAY_SECS: u64 = 300;
                    if away_secs >= DEFAULT_AWAY_SECS {
                        let mut changed_count = 0usize;
                        for (addr, rec) in &self.live {
                            if is_trashed_record(rec) {
                                continue;
                            }
                            let cur = self
                                .model
                                .status(addr)
                                .map(str::to_owned)
                                .or_else(|| rec.get("status").and_then(Value::as_str).map(str::to_owned))
                                .unwrap_or_default();
                            if matches!(cur.as_str(), "blocked" | "done" | "error")
                                && self.away_statuses.get(addr).map(String::as_str) != Some(cur.as_str())
                            {
                                changed_count += 1;
                            }
                        }
                        if changed_count > 0 {
                            let away_minutes = (away_secs / 60).max(1);
                            let triggered_at = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |d| d.as_millis() as u64);
                            mods::set_shared(
                                "briefing.away".into(),
                                json!({
                                    "away_ms": away_secs * 1000,
                                    "away_minutes": away_minutes,
                                    "changed_count": changed_count,
                                    "triggered_at": triggered_at,
                                }),
                            );
                        }
                    }
                    self.away_statuses.clear();
                }
                self.sync(window, cx);
            } else {
                self.blurred_at = Some(now);
                self.away_statuses.clear();
                for (addr, rec) in &self.live {
                    if is_trashed_record(rec) {
                        continue;
                    }
                    let cur = self
                        .model
                        .status(addr)
                        .map(str::to_owned)
                        .or_else(|| rec.get("status").and_then(Value::as_str).map(str::to_owned))
                        .unwrap_or_default();
                    if !cur.is_empty() {
                        self.away_statuses.insert(addr.clone(), cur);
                    }
                }
                self.looked.clear();
            }
            // Tell mods now: render publishes window() too, but a mod it refreshes
            // waits for another frame, which an idle window that just lost focus
            // may not draw for a long time.
            if mods::publish_window(self.window_json(window)) {
                self.refresh_snapshot_mods(cx);
            }
        }
        let dark = is_dark(window.appearance());
        if dark != self.system_dark {
            self.system_dark = dark;
            self.apply_look(window, cx);
        }
    }

    /// Files people or agents edited: apply what they change.
    fn files_changed(&mut self, changed: Vec<std::path::PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        for path in changed_asset_paths(&mods::app_dir(), &changed) {
            invalidate_asset(&path, window, cx);
        }
        let top = |p: &std::path::PathBuf| p.components().next().and_then(|c| c.as_os_str().to_str().map(str::to_owned)).unwrap_or_default();
        if changed.iter().any(|p| matches!(top(p).as_str(), "settings.json" | "keys.json" | "programs.json" | "themes")) {
            self.reload_config(window, cx);
        }
        // The theme showing may be the file that changed.
        if changed.iter().any(|p| top(p) == "themes") {
            self.apply_look(window, cx);
        }
        if changed.iter().any(|p| top(p) == "layouts") {
            self.check_layout_file(window, cx);
        }
        // The View menu lists the layouts (not their saved clicks in layouts/.state).
        #[cfg(target_os = "macos")]
        if changed.iter().any(|p| p.parent() == Some(std::path::Path::new("layouts"))) {
            crate::macos::refresh_menus(cx);
        }
        let changed_mods: BTreeSet<String> = changed.iter().filter_map(|p| crate::files::mod_of(p)).collect();
        let edited: BTreeSet<String> = changed_mods.into_iter().filter(|name| {
            let Some(folder) = self.folders.get(name) else { return true };
            let stamp = crate::files::mod_stamp(&folder.dir);
            // An incomplete scan cannot prove that the source is unchanged.
            stamp.is_none() || self.mod_stamps.borrow_mut().insert(name.clone(), stamp.clone()).as_ref() != Some(&stamp)
        }).collect();
        if edited.is_empty() {
            return;
        }
        self.failed_loads.retain(|name| !edited.contains(name));
        // Copies that load now (a new folder, a fixed failed load) already run the
        // saved code: reloading them too would start them twice.
        let loaded: BTreeSet<u64> = self.copies.values().filter(|c| c.root.is_some()).map(|c| c.id).collect();
        let count = self.copies.len();
        self.copies.retain(|_, c| c.root.is_some() || !edited.contains(&c.inst.name));
        let retry = self.copies.len() != count;
        self.sync_folders(window, cx);
        if retry {
            self.sync(window, cx);
        }
        let Some(runtime) = self.runtime.clone() else { return };
        // A reload keeps the code it replaces: QuickJS frees evaluated modules
        // only with the whole context. Before the window's VM runs out, start a
        // new one, which also loads the saved code.
        if runtime.memory_share() >= RESTART_MODS_AT
            && self.copies.values().any(|c| edited.contains(&c.inst.name) && loaded.contains(&c.id))
        {
            if self.load_recovery.is_none_or(|last| last.elapsed() >= Duration::from_secs(60)) && self.copies.values().all(|c| c.error.is_none()) {
                self.restart_mods(window, cx);
                return;
            }
            if self.recover_mods(&edited, window, cx) {
                return;
            }
        }
        let mut exhausted = BTreeSet::new();
        for copy in self.copies.values_mut().filter(|c| edited.contains(&c.inst.name) && loaded.contains(&c.id)) {
            let Some(root) = &copy.root else { continue };
            let near_limit = runtime.memory_share() >= RESTART_MODS_AT;
            let error = runtime.reload_now(root, window, cx).err();
            if error.as_ref().is_some_and(|e| near_limit || out_of_memory(e) || runtime.memory_share() >= RESTART_MODS_AT) {
                exhausted.insert(copy.inst.name.clone());
            }
            let error = error.map(|e| format!("{e:#}"));
            if let Some(e) = &error {
                tracing::warn!("mod reload failed: {e}");
            }
            copy.error = error;
        }
        if !exhausted.is_empty() {
            self.recover_mods(&exhausted, window, cx);
        }
    }

    fn recover_mods(&mut self, names: &BTreeSet<String>, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.load_recovery.is_some_and(|last| last.elapsed() < Duration::from_secs(60))
            || names.iter().all(|name| self.failed_loads.contains(name))
        {
            return false;
        }
        if !self.restart_mods(window, cx) {
            return false;
        }
        self.load_recovery = Some(Instant::now());
        true
    }

    /// The window's mods on a new VM, as when the window opens: every copy
    /// closes on the old one and the shown ones load again, open popups,
    /// dialogs and windows too (toasts close). Hidden copies start fresh when
    /// shown. Terminals, the core and the layout stay.
    fn restart_mods(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let fresh = match ShellRuntime::replace_default(cx) {
            Ok(fresh) => fresh,
            Err(e) => {
                tracing::error!("could not start a new script runtime: {e:#}");
                return false;
            }
        };
        tracing::warn!("mod memory was nearly used up; restarting this window's mods");
        if let Some(events) = &self.file_events {
            fresh.set_source_revision_provider(events.revision_provider());
        }
        // A toast would show (or send) its notification again: it closes.
        let toasts: Vec<String> = self.overlays.iter().filter(|o| o.kind == OverlayKind::Toast).map(|o| o.id.clone()).collect();
        for id in toasts {
            self.remove_overlay(&id, window, cx);
        }
        // An overlay's own mod loads when it opens, not in `sync`: it loads
        // again here, in its frame.
        let reopen: Vec<(String, Instance, bool)> = self
            .overlays
            .iter()
            .filter(|o| o.layout.is_none())
            .filter_map(|o| self.copies.get(&o.slot).map(|c| (o.slot.clone(), c.inst.clone(), o.fit)))
            .collect();
        let slots: Vec<String> = self.copies.keys().cloned().collect();
        for slot in slots {
            self.remove_copy(&slot, window, cx);
        }
        self.runtime = Some(fresh);
        // A failure in the fresh VM gets reported, not retried forever.
        self.restarting_mods = true;
        // The frame takes the keyboard first, as when it opened.
        if let Some(focus) = self.overlays.iter().rev().find(|o| o.kind.modal_keys()).map(|o| o.focus.clone()) {
            focus.focus(window, cx);
        }
        for (slot, inst, fit) in reopen {
            let copy = self.load(inst, window, cx);
            if fit
                && let Some(root) = &copy.root
            {
                root.update(cx, |r, cx| r.set_fit_content(true, cx));
            }
            self.copies.insert(slot, copy);
        }
        self.sync(window, cx);
        self.restarting_mods = false;
        if self.copies.values().any(|c| c.error.is_some()) {
            self.load_recovery = Some(Instant::now());
            self.failed_loads.extend(self.copies.values().filter(|c| c.error.is_some()).map(|c| c.inst.name.clone()));
        }
        true
    }

    fn sync_folders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.runtime.is_none() {
            return;
        }
        let found: BTreeMap<String, Found> = mods::scan().into_iter().map(|f| (f.id.clone(), f)).collect();
        let edited: BTreeSet<String> = found.values().filter(|f| self.folders.get(&f.id).is_some_and(|o| o.stamp != f.stamp)).map(|f| f.id.clone()).collect();
        let added = found.keys().any(|k| !self.folders.contains_key(k));
        let removed = self.folders.keys().any(|k| !found.contains_key(k));
        let fresh: Vec<Found> = found.values().filter(|f| !self.seen.contains(&f.id)).cloned().collect();
        self.mod_stamps.borrow_mut().retain(|name, _| found.contains_key(name));
        self.folders = found;

        // Copies that failed get another try when their folder changes; loaded ones hot-reload.
        let retry: Vec<String> = self.copies.iter().filter(|(_, c)| c.root.is_none() && (edited.contains(&c.inst.name) || added)).map(|(s, _)| s.clone()).collect();
        for slot in &retry {
            self.copies.remove(slot);
        }
        // A mod seen for the first time goes where it asked (unless the layout already shows it).
        for f in fresh {
            if shipped_layouts().contains(&self.layout_name.as_str()) && f.hint.place.as_deref().unwrap_or("right") != "none" {
                continue;
            }
            self.seen.insert(f.id.clone());
            write_atomic(&file("seen.json"), &serde_json::to_string(&self.seen).unwrap_or_default());
            let shown = self.chrome.walk().iter().any(|n| matches!(n.content(), Some(Content::Mod { name, .. }) if *name == f.id));
            let place = f.hint.place.as_deref().unwrap_or("right");
            if !shown && place != "none" && !shipped_layouts().contains(&self.layout_name.as_str()) {
                let mut block = Node::block(self.model.fresh_id("b"), Content::Mod { name: f.id.clone(), props: Value::Null });
                block.size = Some(Span::Px { px: f.hint.size.unwrap_or(320.0) });
                dock(&mut self.chrome, block, place, self.model.fresh_id("r"));
            }
        }
        if !retry.is_empty() || added || removed {
            self.sync(window, cx);
        }
    }

    // ---- rendering ---------------------------------------------------------

    fn focused_path(&self, window: &Window, cx: &App) -> HashSet<String> {
        let mut out = HashSet::new();
        // A mod block in the layout holding the keyboard: its `focus` looks, not the pane's.
        if let Some(path) = self.focused_block.as_ref().and_then(|b| self.chrome.path_to(b)) {
            out.extend(path);
            return out;
        }
        if let Some(p) = &self.focused_pane {
            // Pane selection survives an input, float or dialog taking the keyboard.
            if self.pane_term(p).is_some_and(|t| !t.view.read(cx).owns_keyboard_focus(window, cx)) {
                return out;
            }
            if let Some(c) = self.model.active_tab()
                && let Some(path) = c.root.path_to(p)
            {
                out.extend(path);
            }
            out.insert(p.clone());
        }
        out
    }

    /// The shown tab block's pane frame, if its panes are framed now: when
    /// the tab is split, or always with `lone`.
    fn pane_frame(&self) -> Option<&PaneFrame> {
        let frame = self.chrome.visible_blocks().into_iter().find_map(|n| match n.content() {
            Some(Content::Context { pane, .. }) => pane.as_ref(),
            _ => None,
        })?;
        let ctx = self.model.active_tab()?;
        (frame.lone || ctx.root.panes().len() > 1).then_some(frame)
    }

    /// A pane in its frame: the look, and the header mod above or below it
    /// (on hover: drawn over the terminal, so it never resizes).
    fn frame_pane(&self, n: &Node, f: &PaneFrame, on: bool, inner: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        let line = pane_size::frame_border(f, on);
        let group = SharedString::from(format!("pane-{}", n.id));
        let mut el = div().size_full().relative().flex().flex_col().overflow_hidden().group(group.clone());
        if let Some(r) = f.radius {
            el = el.rounded(settings::z(r));
        }
        if let Some(bg) = &f.background {
            el = paint(el, bg);
        }
        if let Some(l) = line {
            el = el.border(settings::z(l.width)).border_color(c(&l.color));
        }
        el = shadow(el, f.shadow.as_deref());
        let body = div().flex_1().min_h_0().flex().child(inner);
        let Some(h) = &f.header else { return el.child(body).into_any_element() };
        let head = div().flex_none().h(settings::z(pane_size::header_height(h))).w_full().flex()
            .child(self.render_slot(&format!("{}#frame", n.id), cx));
        if h.hover() {
            let head = head.absolute().left_0().right_0().opacity(0.).group_hover(group, |s| s.opacity(1.));
            let head = if h.bottom() { head.bottom_0() } else { head.top_0() };
            return el.child(body).child(head).into_any_element();
        }
        if h.bottom() { el.child(body).child(head) } else { el.child(head).child(body) }.into_any_element()
    }

    /// `frame`: how panes are wrapped here (inside a framed tab), else None.
    fn render_node(&self, n: &Node, focused: &HashSet<String>, frame: Option<&PaneFrame>, cx: &mut Context<Self>) -> AnyElement {
        let mut el = div().relative().overflow_hidden().size_full().flex();
        let bounds_map = self.bounds.clone();
        let id = n.id.clone();
        el = el.child(canvas(move |b, _, _| { bounds_map.borrow_mut().insert(id, b); }, |_, _, _, _| {}).absolute().size_full());
        el = apply_look(el, n, focused.contains(&n.id));
        if n.window_drag {
            el = self.window_drag(el, cx);
        }
        let el = match &n.kind {
            Kind::Block { content } => {
                let inner = self.render_block(n, content, focused, cx);
                match frame.filter(|_| n.is_pane()) {
                    Some(f) => el.child(self.frame_pane(n, f, focused.contains(&n.id), inner, cx)),
                    None => el.child(inner),
                }
            }
            Kind::Row { children } | Kind::Column { children } => {
                let axis = n.axis().unwrap();
                el = if axis == Axis::Row { el.flex_row() } else { el.flex_col() };
                let shown: Vec<&Node> = children.iter().filter(|c| !c.hidden).collect();
                let gap = pane_size::gap(n, frame);
                // Weights are shares of what the fixed-size siblings leave, so they always fill it.
                let total: f32 = shown
                    .iter()
                    .map(|c| pane_size::weight(c))
                    .sum::<f32>()
                    .max(f32::EPSILON);
                for (i, child) in shown.iter().enumerate() {
                    if i > 0 {
                        el = el.child(self.separator(n, axis, gap, i - 1, cx));
                    }
                    // No clipping here: a child's shadow may fall outside it (the child clips itself).
                    let mut slot = div().relative().flex().min_w_0().min_h_0();
                    slot = sized(slot, child, axis, total);
                    el = el.child(slot.child(self.render_node(child, focused, frame, cx)));
                }
                el
            }
            Kind::Stack { header, .. } => {
                el = el.flex_col();
                let head = header.as_ref().map(|h| {
                    let height = pane_size::header_height(h);
                    let head: AnyElement = match &h.content {
                        HeaderContent::Mod { .. } => self.render_slot(&format!("{}#header", n.id), cx),
                        HeaderContent::Builtin(_) => self.builtin_tabs(n, cx),
                    };
                    (h.bottom(), div().flex_none().h(settings::z(height)).w_full().flex().child(head))
                });
                let body = match n.active_child() {
                    Some(child) => self.render_node(child, focused, frame, cx),
                    None => div().into_any_element(),
                };
                let body = div().flex_1().min_h_0().flex().child(body);
                match head {
                    Some((true, head)) => el.child(body).child(head),
                    Some((false, head)) => el.child(head).child(body),
                    None => el.child(body),
                }
            }
        };
        let row = frame.and_then(|f| f.header.as_ref()).filter(|h| !h.bottom() && !h.hover()).map(pane_size::header_height);
        el.children(self.render_floats(n, cx)).children(self.pane_grip(&n.id, row, cx)).into_any_element()
    }

    fn render_block(&self, n: &Node, content: &Content, focused: &HashSet<String>, cx: &mut Context<Self>) -> AnyElement {
        match content {
            Content::Terminal { .. } => {
                if self.pane_is_structured(&n.id) {
                    return self.render_slot(&format!("{}#peek", n.id), cx);
                }
                match self.pane_term(&n.id).map(|t| shown_terminal(&t.view, cx)) {
                    Some(inner) => {
                        let id = n.id.clone();
                        div()
                            .size_full()
                            .on_any_mouse_down(cx.listener(move |this, _, _, cx| {
                                // Overlay terminals keep their own focus; chrome blocks share the pane focus API.
                                if this.focused_pane.as_deref() != Some(id.as_str())
                                    && (this.model.tab_of(&id).is_some() || terminal_block(&this.chrome, Some(&id)).is_some())
                                {
                                    this.model.focus_pane(&id);
                                    this.focused_pane = Some(id.clone());
                                    this.synced_model_pane = this.model.focused_pane();
                                    this.publish(cx);
                                    this.save();
                                    cx.notify();
                                }
                            }))
                            .child(inner)
                            .into_any_element()
                    }
                    None => div().into_any_element(),
                }
            }
            Content::Mod { .. } => {
                if n.id == "side-chat" && self.side_chat_terminal_mode().is_some() {
                    let term_el = match self.terms.get("side-chat#term").map(|t| (t.focus.clone(), shown_terminal(&t.view, cx))) {
                        Some((focus, inner)) => div()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .bg(c("background"))
                            .on_any_mouse_down(cx.listener(move |_this, _, window, cx| {
                                focus.focus(window, cx);
                            }))
                            .child(inner)
                            .into_any_element(),
                        None => div().flex_1().min_h_0().w_full().bg(c("background")).into_any_element(),
                    };
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .min_h_0()
                        .bg(c("background"))
                        .child(div().flex_none().w_full().child(self.render_slot("side-chat#term-header", cx)))
                        .child(term_el)
                        .into_any_element()
                } else {
                    self.render_slot(&n.id, cx)
                }
            }
            Content::Agent { .. } => match self.shown_agent() {
                Some(addr) => match self.agent_view(&n.id, &addr) {
                    Some(t) => div().size_full().child(shown_terminal(&t.view, cx)).into_any_element(),
                    None => div().into_any_element(),
                },
                None => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(c("dim"))
                    .child("no agents yet. start claude, codex or pi in a terminal")
                    .into_any_element(),
            },
            Content::Context { .. } => match self.model.active_tab() {
                Some(c) => self.render_node(c.shown(), focused, self.pane_frame(), cx),
                None if self.documents.get("").is_some_and(|s| s.layout() == "loading") => div().size_full().into_any_element(),
                None => {
                    let key = crate::keys::listing().into_iter().find(|(_, _, a)| a == "space.new").map(|(_, k, _)| format!(" ({k})"));
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(c("dim"))
                        .child(format!("No space open. Make one{}.", key.unwrap_or_default()))
                        .into_any_element()
                }
            },
        }
    }

    fn render_slot(&self, slot: &str, cx: &mut Context<Self>) -> AnyElement {
        let in_overlay = slot.starts_with("overlay:") || self.overlays.iter().any(|o| o.layout.as_ref().is_some_and(|l| l.find(slot).is_some()));
        // A mod that is a pane in a tab: a click makes it the focused pane, and the
        // keyboard is its own (an editor, a board), like a terminal's.
        let pane_slot = slot.strip_suffix("#peek").unwrap_or(slot);
        let in_pane = self.model.tab_of(pane_slot).is_some();
        match self.copies.get(slot) {
            Some(Copy { root: Some(root), inst, .. }) => div()
                .size_full()
                .key_context(format!("Mod mod = {}", inst.name).as_str())
                .when(in_pane, |d| {
                    let id = pane_slot.to_owned();
                    d.on_any_mouse_down(cx.listener(move |this, _, _, cx| {
                        if this.focused_pane.as_deref() != Some(id.as_str()) {
                            this.model.focus_pane(&id);
                            this.focused_pane = Some(id.clone());
                            this.synced_model_pane = this.model.focused_pane();
                            this.publish(cx);
                            this.save();
                            cx.notify();
                        }
                    }))
                })
                // Bare layout clicks leave the keyboard with the pane, but an input
                // or other focusable control owns the keyboard once it takes focus.
                .when(!in_overlay && !in_pane, |d| {
                    d.capture_any_mouse_down(cx.listener(|_, _, window, cx| {
                        cx.defer_in(window, |this, window, cx| this.refocus_pane(window, cx));
                    }))
                })
                // Popups size to their content, up to the window: a column, so a mod
                // root with `min_h(0)` can shrink and scroll. Layout mods fill their block.
                // Not cached here: ShellRoot caches the mod's view itself and draws its
                // spinners over it, waking only itself (GPUI rebuilds all under a cached
                // view that changed, so a spinner step would rebuild the mod).
                .map(|d| if in_overlay { d.h_auto().min_h_0().flex().flex_col().child(root.clone()) } else { d.child(root.clone()) })
                .into_any_element(),
            Some(Copy { error, inst, .. }) => div()
                .id(SharedString::from(format!("err-{slot}")))
                .size_full()
                .p_2()
                .text_xs()
                .text_color(c("blocked"))
                .overflow_y_scroll()
                .child(format!("{} did not load:\n\n{}\n\nFix the files; Herdr2 retries on save.", inst.name, error.as_deref().unwrap_or("?")))
                .into_any_element(),
            None => div().into_any_element(),
        }
    }

    /// Plain fallback for `"header": { "content": "tabs" }`.
    fn builtin_tabs(&self, n: &Node, cx: &mut Context<Self>) -> AnyElement {
        let active = n.active_child().map(|c| c.id.clone());
        let stack = n.id.clone();
        let mut row = div().size_full().flex().flex_row().bg(c("surface")).text_xs();
        for (i, ch) in n.children().iter().enumerate() {
            let (s, id) = (stack.clone(), ch.id.clone());
            let on = active.as_deref() == Some(ch.id.as_str());
            row = row.child(
                div()
                    .id(SharedString::from(format!("tab-{}", ch.id)))
                    .px_3()
                    .flex()
                    .items_center()
                    .text_color(if on { c("foreground") } else { c("muted") })
                    .when(on, |d| d.bg(c("raised")))
                    .on_click(cx.listener(move |this, _, w, cx| this.run("layout.select", json!({ "stack": s, "child": id }), w, cx)))
                    .child(ch.title.clone().unwrap_or_else(|| (i + 1).to_string())),
            );
        }
        row.into_any_element()
    }

    fn separator(&self, n: &Node, axis: Axis, gap: f32, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let row = axis == Axis::Row;
        let zoom = settings::zoom();
        let line = n.divider.as_ref().map(|d| (d.width * zoom, c(&d.color)));
        let width = pane_size::separator_width(n, gap) * zoom;
        let mut sep = div().flex_none().relative().flex().items_center().justify_center();
        sep = if row { sep.w(px(width)).h_full() } else { sep.h(px(width)).w_full() };
        if let Some((w, color)) = line {
            let top = traffic_light_divider_inset(n, axis, index, cfg!(target_os = "macos"), zoom) * zoom;
            if top > 0.0
                && let Some(Paint::Color(background)) = n.children().iter().filter(|child| !child.hidden).nth(index).and_then(|child| child.background.as_ref())
            {
                sep = sep.bg(c(background));
            }
            sep = sep.child(separator_line(row, w, color, top));
        }
        if n.resizable() {
            let container = n.id.clone();
            let hit = HANDLE.max(width);
            sep = sep.child(
                div()
                    .absolute()
                    .when(row, |d| d.top_0().bottom_0().left(px((width - hit) / 2.0)).w(px(hit)).cursor_col_resize())
                    .when(!row, |d| d.left_0().right_0().top(px((width - hit) / 2.0)).h(px(hit)).cursor_row_resize())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            if let Some(space) = this.model.space_of(&container)
                                && this.require_document(space.machine.as_deref()).is_err() { return; }
                            this.drag = Some(Drag { container: container.clone(), index, axis });
                            cx.stop_propagation();
                        }),
                    ),
            );
        }
        sep.into_any_element()
    }

    fn drag_to(&mut self, pos: Point<Pixels>) {
        let Some(d) = &self.drag else { return };
        let (container, index, axis) = (d.container.clone(), d.index, d.axis);
        let Some(node) = self.find_node_mut(&container).map(|n| n.clone()) else { return };
        if !node.resizable() { return; }
        let shown: Vec<&Node> = node.children().iter().filter(|c| !c.hidden).collect();
        let (Some(a), Some(b)) = (shown.get(index), shown.get(index + 1)) else { return };
        let bounds = self.bounds.borrow();
        let (Some(ba), Some(bb)) = (bounds.get(&a.id).copied(), bounds.get(&b.id).copied()) else { return };
        drop(bounds);
        let (start, p) = match axis {
            Axis::Row => (f32::from(ba.left()), f32::from(pos.x)),
            Axis::Column => (f32::from(ba.top()), f32::from(pos.y)),
        };
        let (la, lb) = match axis {
            Axis::Row => (f32::from(ba.size.width), f32::from(bb.size.width)),
            Axis::Column => (f32::from(ba.size.height), f32::from(bb.size.height)),
        };
        // Measured on screen; sizes are stored as designed (unzoomed).
        let zoom = settings::zoom();
        let (la, lb, start, p) = (la / zoom, lb / zoom, start / zoom, p / zoom);
        let total = la + lb;
        // Squeezed and zero-size siblings cannot both keep the drag minimum.
        if total <= 48.0 { return; }
        let mut new_a = (p - start).clamp(24.0, total - 24.0);
        if let Some(min) = a.min {
            new_a = new_a.max(min);
        }
        if let Some(max) = a.max {
            new_a = new_a.min(max);
        }
        let new_a = new_a.min(total - 24.0);
        let (a_id, b_id) = (a.id.clone(), b.id.clone());
        let (sa, sb) = (a.size, b.size);
        let Some(parent) = self.find_node_mut(&container) else { return };
        let Some(children) = parent.children_mut() else { return };
        let set = |children: &mut Vec<Node>, id: &str, s: Span| {
            if let Some(n) = children.iter_mut().find(|n| n.id == id) {
                n.size = Some(s);
            }
        };
        match (sa, sb) {
            (Some(Span::Px { .. }), _) => set(children, &a_id, Span::Px { px: new_a.round() }),
            (_, Some(Span::Px { .. })) => set(children, &b_id, Span::Px { px: (total - new_a).round() }),
            _ => {
                let wa = match sa { Some(Span::Weight { weight }) => weight, _ => 1.0 };
                let wb = match sb { Some(Span::Weight { weight }) => weight, _ => 1.0 };
                let sum = wa + wb;
                set(children, &a_id, Span::Weight { weight: sum * new_a / total });
                set(children, &b_id, Span::Weight { weight: sum * (total - new_a) / total });
            }
        }
    }

    fn render_overlays(&self, viewport: Size<Pixels>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let mut toast_y = 16.0;
        // Drawn last, so it stays clickable over an open dialog.
        let pill = self.render_update_pill(cx);
        if pill.is_some() {
            toast_y += UPDATE_PILL_HEIGHT + 8.0;
        }
        // Windows under dialogs, popups and toasts; each kind in the order opened
        // (a window pressed last is last: on top).
        let order = self.overlays.iter().filter(|o| o.kind == OverlayKind::Window).chain(self.overlays.iter().filter(|o| o.kind != OverlayKind::Window));
        for o in order {
            let id = o.id.clone();
            // Up to 1: a share of the window (terminals in overlays want room, not px).
            let span = |v: f32, full: Pixels| if v <= 1.0 { full * v } else { settings::z(v) };
            let (w, h) = (span(o.size.width, viewport.width), span(o.size.height, viewport.height));
            let sized = |v: f32| v > 0.0 || o.kind != OverlayKind::Window;
            let content = match &o.layout {
                Some(l) => self.render_node(l, &HashSet::new(), None, cx),
                None => self.render_slot(&o.slot, cx),
            };
            let body = div()
                .id(SharedString::from(format!("ov-{}", o.id)))
                .track_focus(&o.focus)
                // Esc stays with terminals (agents use it to interrupt) and windows.
                .when(o.layout.is_none() && o.kind.modal_keys(), |d| d.key_context("Overlay"))
                .when(sized(o.size.width), |d| d.w(w))
                .when(sized(o.size.height), |d| if o.fit { d.max_h(h) } else { d.h(h) })
                .when(!o.bare, |d| d.bg(c("popover")).border_1().border_color(c("border")).rounded(settings::z(8.0)).shadow_lg())
                .overflow_hidden()
                .flex()
                // Scroll, hover and cursor stop here, not on the pane behind.
                .occlude()
                .when(o.kind != OverlayKind::Window, |d| d.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()))
                .child(content);
            match o.kind {
                OverlayKind::Window => out.push(self.render_window(o, body, viewport, cx)),
                OverlayKind::Dialog => out.push(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(hsla(0.0, 0.0, 0.0, 0.35))
                        .occlude()
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(!o.modal, |d| d.on_mouse_down(MouseButton::Left, cx.listener(move |this, _, w, cx| this.close_overlay(&id, w, cx))))
                        .child(body)
                        .into_any_element(),
                ),
                OverlayKind::Popup => {
                    let at = match o.top {
                        // Centered, hanging from `offset` px below the top (a drop-down from a bar).
                        Some(off) => point((viewport.width - w) / 2.0, settings::z(off)),
                        None => o.at,
                    };
                    out.push(
                        div()
                            .absolute()
                            .inset_0()
                            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, w, cx| this.close_overlay(&id, w, cx)))
                            .on_mouse_down(MouseButton::Right, cx.listener({
                                let id = o.id.clone();
                                move |this, _, w, cx| this.close_overlay(&id, w, cx)
                            }))
                            .child(position_popup(at, body))
                            .into_any_element(),
                    );
                }
                OverlayKind::Toast => {
                    out.push(div().absolute().right(settings::z(16.0)).bottom(settings::z(toast_y)).child(body).into_any_element());
                    toast_y += o.size.height + 8.0;
                }
            }
        }
        out.extend(pill);
        out
    }
}

const UPDATE_PILL_HEIGHT: f32 = 34.0;

impl AppWindow {
    /// Original Herdr update banner and pill are disabled.
    fn update_label() -> Option<String> {
        None
    }

    fn render_update_pill(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        None
    }
}

/// A view drawn again only when it changed: a terminal's output then
/// redraws that terminal, not every mod and pane in the window.
pub(crate) fn cached<V: Render>(view: &Entity<V>) -> AnyElement {
    view.clone().cached(StyleRefinement::default().size_full()).into_any_element()
}

pub(crate) fn shown_terminal(view: &Entity<TerminalView>, cx: &App) -> AnyElement {
    view.read(cx).mark_shown();
    cached(view)
}

fn remember_pane(history: &mut Vec<String>, id: &str) {
    if history.last().is_some_and(|last| last == id) {
        return;
    }
    history.retain(|pane| pane != id);
    history.push(id.to_owned());
}

/// Prefer the most recently focused pane along the nearest edge. Unvisited
/// panes fall back to overlap and layout order, ignoring subpixel split noise.
fn nearest(cur: Bounds<Pixels>, dir: &str, candidates: impl IntoIterator<Item = (String, Bounds<Pixels>)>, history: &[String]) -> Option<String> {
    let overlap = |a0: Pixels, a1: Pixels, b0: Pixels, b1: Pixels| f32::from(a1.min(b1) - a0.max(b0)).max(0.0);
    let cmp = |a: f32, b: f32| if (a - b).abs() < 1.0 { std::cmp::Ordering::Equal } else { a.total_cmp(&b) };
    candidates
        .into_iter()
        .filter_map(|(id, r)| {
            let (gap, ov) = match dir {
                "left" => (f32::from(cur.left() - r.right()), overlap(cur.top(), cur.bottom(), r.top(), r.bottom())),
                "right" => (f32::from(r.left() - cur.right()), overlap(cur.top(), cur.bottom(), r.top(), r.bottom())),
                "up" => (f32::from(cur.top() - r.bottom()), overlap(cur.left(), cur.right(), r.left(), r.right())),
                _ => (f32::from(r.top() - cur.bottom()), overlap(cur.left(), cur.right(), r.left(), r.right())),
            };
            let recent = history.iter().position(|pane| pane == &id);
            (gap > -2.0 && ov > 0.0).then_some((gap, recent, ov, id))
        })
        .reduce(|best, next| if cmp(next.0, best.0).then(best.1.cmp(&next.1)).then(cmp(best.2, next.2)).is_lt() { next } else { best })
        .map(|(_, _, _, id)| id)
}

fn position_popup(at: Point<Pixels>, body: Stateful<Div>) -> Anchored {
    anchored()
        .position(point(at.x.max(px(8.0)), at.y.max(px(8.0))))
        .snap_to_window_with_margin(px(8.0))
        .child(body)
}

/// An overlay's blocks get ids of their own (`<overlay>/<id>`), and its mods learn the overlay.
fn overlay_layout(mut n: Node, overlay: &str) -> Node {
    fn walk(n: &mut Node, overlay: &str, seq: &mut usize) {
        *seq += 1;
        n.id = if n.id.is_empty() { format!("ov{overlay}/{seq}") } else { format!("ov{overlay}/{}", n.id) };
        if let Kind::Block { content: Content::Mod { props, .. } } = &mut n.kind {
            if !props.is_object() {
                *props = json!({});
            }
            props["overlay"] = json!(overlay);
        }
        for c in n.children_mut().into_iter().flatten() {
            walk(c, overlay, seq);
        }
    }
    walk(&mut n, overlay, &mut 0);
    n
}

/// What a terminal's folder last said about its repo, and what ran there then.
struct FolderCheck {
    at: Instant,
    cwd: String,
    busy: bool,
    /// A repo, or git couldn't say (a machine unreachable): worth a look again.
    repo: bool,
}

/// Changes made outside Herdr (a commit in another app): looked for this often,
/// in repos, while the window is in front.
const FOLDER_FALLBACK: Duration = Duration::from_secs(60);

/// An agent working, or a program other than the shell in front (an agent
/// between turns isn't, even started from a shell).
fn busy(rec: &Value) -> bool {
    let program = |k: &str| rec["program"][k].as_bool() == Some(true);
    rec["status"] == "working" || !(program("shell") || program("agent"))
}

/// Whether to ask a terminal's folder for its repo and branch again: only when
/// the answer could have changed. It moved, a command or an agent's turn just
/// ended there (a checkout, a commit), or a repo's fallback is due.
fn recheck(last: Option<&FolderCheck>, cwd: &str, busy: bool, active: bool) -> bool {
    let Some(last) = last else { return true };
    last.cwd != cwd || (last.busy && !busy) || (active && last.repo && last.at.elapsed() > FOLDER_FALLBACK)
}

/// Set once the app starts quitting, the OS ending the session included: the
/// saved layout stays as it was.
pub static QUITTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The repo a folder is in (a worktree counts as its main checkout's), else the
/// folder itself, and the branch; and whether it is a repo (or git couldn't say).
fn folder_info(cwd: &str, home: &str, on: Option<&str>) -> (model::FolderInfo, bool) {
    let none = model::FolderInfo { top: cwd.into(), branch: None };
    if on.is_none() && cwd == home {
        return (none, false);
    }
    let args = ["-C", cwd, "rev-parse", "--show-toplevel", "--abbrev-ref", "HEAD"].map(str::to_owned);
    let ran = match on {
        Some(m) => mods::run_remote(m, "git", &args, None).ok().map(|(code, out, err)| (code == 0, out, err)),
        None => crate::spawn::command("git").args(&args).output().ok().map(|o| (o.status.success(), o.stdout, o.stderr)),
    };
    let Some((true, out, _)) = ran else {
        // Only git's own "no" means no repo; a failed run, a repo without commits
        // or one git won't read are asked again.
        let no = ran.is_some_and(|(_, _, err)| String::from_utf8_lossy(&err).to_lowercase().contains("not a git repository"));
        return (none, !no);
    };
    let out: Vec<String> = String::from_utf8_lossy(&out).lines().map(str::to_owned).collect();
    let [top, head] = out.as_slice() else { return (none, true) };
    let branch = (head != "HEAD").then(|| head.clone());
    (model::FolderInfo { top: top.clone(), branch }, true)
}

fn terminal_block<'a>(chrome: &'a Node, focused: Option<&str>) -> Option<&'a Node> {
    let id = focused?;
    chrome.visible_blocks().into_iter().find(|n| n.id == id && n.terminal().is_some())
}

/// The terminal block `id` in a window's layout (an overlay with blocks).
fn overlay_terminal_block<'a>(overlays: &'a [Overlay], id: Option<&str>) -> Option<&'a Node> {
    let id = id?;
    overlays.iter().filter_map(|o| o.layout.as_ref()).find_map(|l| l.visible_blocks().into_iter().find(|n| n.id == id && n.terminal().is_some()))
}

#[derive(Debug, PartialEq)]
enum SwapBy<'a> {
    Direction(&'a str),
    Target(&'a str),
}

fn swap_with_neighbor(model: &mut Model, from: &str, explicit: bool, neighbor: Option<String>) -> Result<(), String> {
    if explicit && model.tab_of(from).and_then(|c| c.root.find(from)).is_none_or(|n| !n.is_pane()) {
        return Err(format!("no pane {from:?}"));
    }
    let shown = model.active_tab().is_some_and(|c| c.shown().visible_blocks().iter().any(|n| n.id == from));
    if explicit && !shown {
        return Err(format!("pane.swap direction needs a pane in the shown tab, not {from:?}"));
    }
    if let Some(to) = neighbor
        && let Some(ctx) = model.tab_of(from).map(|c| c.id.clone())
        && let Some(c) = model.tab_mut(&ctx)
    {
        c.root.swap(from, &to);
    }
    Ok(())
}

fn swap_by(args: &Value) -> Result<SwapBy<'_>, String> {
    if args.get("direction").is_some_and(|d| !d.is_string() && !d.is_null()) {
        return Err("pane.swap direction must be a string (or null to omit it)".into());
    }
    match (AppWindow::arg(args, "direction"), AppWindow::arg(args, "target")) {
        (Some(_), Some(_)) => Err("pane.swap takes direction or target, not both".into()),
        (None, None) => Err("pane.swap needs direction or target".into()),
        (None, Some(target)) => Ok(SwapBy::Target(target)),
        (Some(dir @ ("left" | "right" | "up" | "down")), None) => Ok(SwapBy::Direction(dir)),
        (Some(dir), None) => Err(format!("pane.swap direction is left, right, up or down, not {dir:?}")),
    }
}

fn needs_model_pane(name: &str, args: &Value) -> bool {
    match name {
        "pane.split" | "pane.close" | "pane.detach" | "pane.zoom" | "pane.swap" | "terminal.show" => AppWindow::arg(args, "pane").is_none(),
        "pane.go" => true,
        "terminal.open" | "terminal.keep" => matches!(AppWindow::arg(args, "place"), Some("right" | "down")),
        _ => false,
    }
}

/// A failed implicit folder gets one retry in the machine's home, without local probes.
fn run_following_folder(mut folder: String, follow: bool, on: Option<&str>, mut run: impl FnMut(Option<String>) -> anyhow::Result<String>) -> anyhow::Result<(String, String)> {
    let result = run(Some(folder.clone()).filter(|f| !f.is_empty()));
    let address = match result {
        // A missing folder and a missing program both report spawn_failed. The holder's
        // timeout may still create a terminal, so never retry that ambiguous outcome.
        Err(e) if follow && !folder.is_empty() && crate::hc::error_kind(&e) == Some("spawn_failed") && !format!("{e:#}").contains("the holder did not answer") => {
            folder = if on.is_some() { String::new() } else { home() };
            run(Some(folder.clone()).filter(|f| !f.is_empty()))?
        }
        result => result?,
    };
    Ok((address, folder))
}

fn new_space_machine(args: &Value, person: bool, model: &Model) -> Option<String> {
    args.get("machine").and_then(Value::as_str)
        .or_else(|| if person { model.active_space().and_then(|s| s.machine.as_deref()) } else { None })
        .filter(|m| !m.is_empty()).map(str::to_owned)
}

fn home() -> String {
    mods::home().to_string_lossy().into_owned()
}

fn mod_overlay(dir: &std::path::Path) -> Value {
    let meta: Value = std::fs::read_to_string(dir.join("mod.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    meta.get("overlay").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}))
}

fn terminal_menu_spec(name: &str, defaults: &Value, tab: &str, target: Value) -> Value {
    json!({ "mod": name, "kind": "popup",
        "width": defaults.get("width").and_then(Value::as_f64).unwrap_or(220.0),
        "height": defaults.get("height").and_then(Value::as_f64).unwrap_or(260.0),
        "props": { "tab": tab, "target": target } })
}

fn num_in(v: &Value, k: &str) -> f32 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32
}

/// A floating mod's slot and copy (it's told its float id). `visible` and
/// `owned_mods` must build the same, or a kept copy is dropped.
fn float_mod(id: &str, name: &str, props: &Value) -> (String, Instance) {
    let mut props = if props.is_object() { props.clone() } else { json!({}) };
    props["float"] = json!(id);
    (floats::slot(id), Instance { name: name.to_owned(), props })
}

/// A pane header's slot and copy, told which pane it sits on (a mod pane has no terminal).
fn pane_header_mod(n: &Node, name: &str, props: &Value) -> (String, Instance) {
    let mut props = if props.is_object() { props.clone() } else { json!({}) };
    props["pane"] = json!(n.id);
    props["terminal"] = json!(n.any_terminal());
    (format!("{}#frame", n.id), Instance { name: name.to_owned(), props })
}

/// Every node shown on screen (hidden ones and inactive stack children skipped).
fn visible_nodes(n: &Node) -> Vec<&Node> {
    if n.hidden {
        return vec![];
    }
    let mut out = vec![n];
    match &n.kind {
        Kind::Stack { .. } => {
            if let Some(c) = n.active_child() {
                out.extend(visible_nodes(c));
            }
        }
        _ => {
            for c in n.children() {
                out.extend(visible_nodes(c));
            }
        }
    }
    out
}

fn fill_ids(n: &mut Node, m: &mut Model) {
    n.fill_ids(&mut || m.fresh_id("n"));
}

/// Puts a block along one side of the chrome.
fn dock(root: &mut Node, block: Node, place: &str, wrapper_id: String) {
    let axis = if matches!(place, "top" | "bottom") { Axis::Column } else { Axis::Row };
    let first = matches!(place, "left" | "top");
    if root.axis() == Some(axis) {
        let c = root.children_mut().unwrap();
        if first { c.insert(0, block) } else { c.push(block) }
        return;
    }
    let mut old = std::mem::replace(root, Node::new("", Kind::Row { children: vec![] }));
    let toast = old.toast.take();
    let terminal_menu = old.terminal_menu.take();
    let children = if first { vec![block, old] } else { vec![old, block] };
    let mut wrapper = Node::container(wrapper_id, axis, children);
    wrapper.toast = toast;
    wrapper.terminal_menu = terminal_menu;
    wrapper.divider = Some(Line { width: 1.0, color: "border".into() });
    *root = wrapper;
}

fn sized(slot: Div, n: &Node, axis: Axis, total_weight: f32) -> Div {
    let mut slot = slot;
    let row = axis == Axis::Row;
    match n.size {
        Some(Span::Px { px: v }) => {
            slot = slot.flex_none();
            slot = if row { slot.w(settings::z(v)).h_full() } else { slot.h(settings::z(v)).w_full() };
        }
        _ => {
            let w = pane_size::weight(n);
            let s = slot.style();
            s.flex_grow = Some(w / total_weight);
            s.flex_shrink = Some(1.0);
            s.flex_basis = Some(relative(0.).into());
            slot = if row { slot.h_full() } else { slot.w_full() };
        }
    }
    if let Some(min) = n.min {
        slot = if row { slot.min_w(settings::z(min)) } else { slot.min_h(settings::z(min)) };
    }
    if let Some(max) = n.max {
        slot = if row { slot.max_w(settings::z(max)) } else { slot.max_h(settings::z(max)) };
    }
    slot
}

/// Whether `addr` is a headless ACP agent address (`a1`..`a9zz`, optionally `machine/a*`).
pub(crate) fn is_acp_addr(addr: &str) -> bool {
    const SYMBOLS: &[u8; 30] = b"0123456789bcdfghjkmnpqrstvwxyz";
    let local = addr.rsplit_once('/').map_or(addr, |(_, rest)| rest);
    let Some(rest) = local.strip_prefix('a') else { return false };
    rest.starts_with(|c: char| matches!(c, '1'..='9')) && rest.bytes().all(|b| SYMBOLS.contains(&b))
}

fn trim_path_sep(p: &str) -> &str {
    p.trim_end_matches(['/', '\\'])
}

fn normalize_path_cmp(p: &str) -> String {
    let mut s = p.trim().replace('\\', "/");
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

fn same_path(a: &str, b: &str) -> bool {
    let a = normalize_path_cmp(a);
    let b = normalize_path_cmp(b);
    !a.is_empty() && a == b
}

pub(crate) fn path_within_or_eq(child: &str, root: &str) -> bool {
    let child = trim_path_sep(child);
    let root = trim_path_sep(root);
    if child.is_empty() || root.is_empty() {
        return false;
    }
    if child == root {
        return true;
    }
    child.strip_prefix(root).is_some_and(|rest| rest.starts_with('/') || rest.starts_with('\\'))
}

fn path_matches_root(child: &str, root: &str, allow_descendants: bool) -> bool {
    let c = trim_path_sep(child);
    let r = trim_path_sep(root);
    if c.is_empty() || r.is_empty() {
        return false;
    }
    if c == r {
        return true;
    }
    let segments = r.split(['/', '\\']).filter(|s| !s.is_empty()).count();
    allow_descendants && segments >= 3 && path_within_or_eq(c, r)
}

fn is_trashed_record(rec: &Value) -> bool {
    rec.pointer("/labels/herdr.trashed")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
}

/// Returns `true` for sub-agents spawned by a parent session or Drove worker pool.
fn is_acp_subagent(rec: &Value) -> bool {
    rec.get("parent")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
        || rec
            .pointer("/labels/drove.worker")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
        || rec
            .pointer("/labels/drove.run")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
}

fn is_side_chat_record(rec: &Value) -> bool {
    rec.pointer("/lineage/kind")
        .and_then(Value::as_str)
        .map(|k| k.to_ascii_lowercase().replace('_', "-"))
        .as_deref()
        == Some("side-chat")
}

/// Returns whether `reconcile` should auto-mount an unmounted live ACP agent into its own tab.
/// Sub-agents stay in the background under their main agent unless explicitly labeled with `herdr.space`.
fn should_auto_mount_acp(rec: &Value) -> bool {
    if rec["state"] != "live" || is_trashed_record(rec) {
        return false;
    }
    if rec.pointer("/labels/herdr.parked").and_then(Value::as_str) == Some("1") {
        return false;
    }
    if is_side_chat_record(rec) {
        return false;
    }
    let has_explicit_space = rec
        .pointer("/labels/herdr.space")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    !is_acp_subagent(rec) || has_explicit_space
}

fn has_live_child(addr: &str, live: &HashMap<String, Value>, closing: &HashMap<String, bool>) -> bool {
    live.iter().any(|(a, r)| !closing.contains_key(a) && !is_trashed_record(r) && r.get("state").and_then(Value::as_str) == Some("live") && r.get("parent").and_then(Value::as_str) == Some(addr))
}

fn has_live_parent(addr: &str, live: &HashMap<String, Value>, closing: &HashMap<String, bool>) -> bool {
    live.get(addr)
        .and_then(|r| r.get("parent"))
        .and_then(Value::as_str)
        .is_some_and(|p| !closing.contains_key(p) && live.get(p).is_some_and(|pr| !is_trashed_record(pr) && pr.get("state").and_then(Value::as_str) == Some("live")))
}

fn has_pane_ancestor(addr: &str, live: &HashMap<String, Value>, pane_addrs: &HashSet<String>) -> bool {
    let mut cur = addr;
    for _ in 0..32 {
        let Some(p) = live.get(cur).and_then(|r| r.get("parent")).and_then(Value::as_str) else {
            return false;
        };
        if pane_addrs.contains(p) {
            return true;
        }
        cur = p;
    }
    false
}

/// What a terminal runs now (`foreground`), falling back to launch `argv` unless a Windows
/// `-NoExit -Command` wrapper has already returned from its child back to the shell prompt.
fn running_argv(rec: &serde_json::Map<String, Value>) -> Option<&Value> {
    rec.get("foreground").and_then(|f| f.get("argv")).filter(|a| a.as_array().is_some_and(|a| !a.is_empty())).or_else(|| rec.get("argv"))
}

fn effective_running_argv(rec: &serde_json::Map<String, Value>, child_seen: bool) -> Option<Value> {
    let term_pid = rec.get("pid").and_then(Value::as_u64);
    let fg_pid = rec.get("foreground").and_then(|f| f.get("pid")).and_then(Value::as_u64);
    if child_seen
        && term_pid.is_some()
        && fg_pid == term_pid
        && let Some(fg0) = rec.get("foreground").and_then(|f| f.get("argv")).and_then(|a| a.get(0)).and_then(Value::as_str)
    {
        return Some(json!([fg0]));
    }
    running_argv(rec).cloned()
}

/// Whether the core's record still puts a terminal in this space and tab and
/// it still runs. A listing taken just before it ended still shows it running,
/// and labels outlive the program: putting it back would show a dead pane.
fn adoptable(record: &Value, space: &str, tab: Option<&str>, machine: Option<&str>) -> bool {
    if record["acp"] == true || record["address"].as_str().is_some_and(is_acp_addr) || is_side_chat_record(record) {
        return false;
    }
    let label = record.pointer("/labels/herdr.space").and_then(Value::as_str);
    let labeled_tab = record.pointer("/labels/herdr.tab").and_then(Value::as_str).filter(|s| !s.is_empty());
    record["state"] == "live"
        && !is_trashed_record(record)
        && label == Some(crate::mirror::local(space, machine))
        && labeled_tab == tab.map(|t| crate::mirror::local(t, machine))
}

fn program_of(report: &Value, argv: Option<&Value>) -> Value {
    let app = report.get("app").and_then(Value::as_str);
    let argv: Vec<String> = argv.and_then(Value::as_array).into_iter().flatten().filter_map(|a| a.as_str().map(str::to_owned)).collect();
    crate::programs::of(app, &argv)
}

fn apply_look(el: Div, n: &Node, focused: bool) -> Div {
    let mut el = el;
    let focus = n.focus.as_ref().filter(|_| focused);
    if let Some(bg) = focus.and_then(|f| f.background.as_ref()) {
        el = el.bg(c(bg));
    } else if let Some(bg) = &n.background {
        el = paint(el, bg);
    }
    if let Some(line) = pane_size::node_border(n, focused) {
        el = el.border(settings::z(line.width)).border_color(c(&line.color));
    }
    if let Some(r) = n.radius {
        el = el.rounded(settings::z(r));
    }
    if n.padding.is_some() {
        let (y, x) = pane_size::padding(n);
        el = el.py(settings::z(y)).px(settings::z(x));
    }
    shadow(el, n.shadow.as_deref())
}

/// A color or gradient paints the element; an image is a layer behind
/// whatever comes after it (call before adding content).
fn paint(el: Div, bg: &Paint) -> Div {
    match bg {
        Paint::Color(name) => el.bg(c(name)),
        Paint::Gradient { gradient: [a, b], angle } => el.bg(linear_gradient(*angle, linear_color_stop(c(a), 0.), linear_color_stop(c(b), 1.))),
        Paint::Image { image, fit } => {
            let fit = match fit {
                Fit::Cover => ObjectFit::Cover,
                Fit::Contain => ObjectFit::Contain,
            };
            el.child(img(mods::app_dir().join(image)).absolute().top_0().left_0().size_full().object_fit(fit))
        }
    }
}

fn shadow(el: Div, s: Option<&str>) -> Div {
    match s {
        Some("sm") => el.shadow_sm(),
        Some("md") => el.shadow_md(),
        Some("lg") => el.shadow_lg(),
        _ => el,
    }
}

impl Render for AppWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::bench::count(&crate::bench::WINDOW);
        let _timed = crate::bench::timed(&crate::bench::WINDOW_US);
        crate::bench::frame_start();
        crate::terminal::next_frame();
        window.set_rem_size(px(settings::ui_font_size()));
        // A terminal the person clicked into becomes the focused pane.
        let clicked = self
            .terms
            .iter()
            .find(|(_, t)| t.focus.is_focused(window))
            .map(|(id, _)| id.clone());
        if clicked.is_some() && self.focused_block.take().is_some() {
            self.publish(cx);
        }
        // A window's terminal with the keyboard is what focused() reports; the pane under it stays.
        let in_window = overlay_terminal_block(&self.overlays, clicked.as_deref()).map(|n| n.id.clone());
        if in_window != self.window_terminal {
            self.window_terminal = in_window;
            self.publish(cx);
        }
        if let Some(id) = clicked
            && self.focused_pane.as_deref() != Some(id.as_str())
            && (self.model.tab_of(&id).is_some() || terminal_block(&self.chrome, Some(&id)).is_some())
        {
            self.model.focus_pane(&id);
            self.focused_pane = Some(id);
            self.synced_model_pane = self.model.focused_pane();
            self.publish(cx);
        }
        self.publish_focused_selection(window, cx);
        self.tick_floats(window);
        self.apply_titlebar(window);
        self.apply_blur(window);
        if mods::publish_window(self.window_json(window)) {
            self.refresh_snapshot_mods(cx);
        }
        let focused = self.focused_path(window, cx);
        let tree = self.render_node(&self.chrome, &focused, None, cx);
        let tree = self.window_frame(tree, window, cx);
        let overlays = self.render_overlays(window.viewport_size(), cx);
        let tracker = self.float_tracker(cx);
        let win_tracker = self.window_tracker(cx);
        let pane_drag = self.pane_drag_layer(cx);
        div()
            .id("herdr")
            .track_focus(&self.focus)
            // `layout:<name>` key sections apply while that layout shows.
            .key_context(format!("Herdr layout = {}", self.layout_name).as_str())
            .size_full()
            .relative()
            .bg(c("background"))
            .text_color(c("foreground"))
            .font_family(settings::get().ui_font_family.clone())
            .on_action(cx.listener(|this, a: &ShellAction, w, cx| {
                #[cfg(any(target_os = "macos", test))]
                if matches!(a.id(), "terminal.copy" | "terminal.paste")
                    && crate::macos::dispatch_edit(a, this.terms.values().any(|t| t.view.read(cx).owns_keyboard_focus(w, cx)), w, cx) {
                    return;
                }
                // Nothing selected: the copy chord goes to the program (#216).
                if a.id() == "terminal.copy" {
                    if !this.copy_key(w, cx) {
                        cx.propagate();
                    }
                    return;
                }
                this.run_action(a.id(), w, cx);
            }))
            .on_action(cx.listener(|this, a: &crate::keys::CommandBinding, w, cx| {
                match a.command() {
                    Ok((name, args)) => {
                        #[cfg(target_os = "macos")]
                        if let Some(action) = crate::macos::action(&name) {
                            w.dispatch_action(action, cx);
                            return;
                        }
                        this.run_key(&name, args, w, cx);
                    }
                    Err(error) => this.command_error("Key binding", &error, w, cx),
                }
            }))
            // Ctrl (Cmd) alone shows links in the pane under the pointer, focused or not.
            .on_modifiers_changed(cx.listener(|this, _: &ModifiersChangedEvent, _, cx| {
                for t in this.terms.values() {
                    if t.view.read(cx).hovered() {
                        t.view.update(cx, |_, cx| cx.notify());
                    }
                }
            }))
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    if this.pane_drag.is_some() {
                        this.cancel_pane_drag(cx);
                        cx.stop_propagation();
                    } else if cx.stop_active_drag(window) {
                        cx.stop_propagation();
                    }
                }
            }))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let m = &ev.keystroke.modifiers;
                if ev.keystroke.key == "escape" && !m.control && !m.alt && !m.platform {
                    let window_holds_keys = window.focused(cx).is_none() || this.focus.is_focused(window);
                    let overlay_open = this.overlays.iter().any(|o| o.kind.modal_keys() || o.kind == OverlayKind::Popup);
                    if window_holds_keys
                        && this.focused_block.is_none()
                        && !overlay_open
                        && let Some(pane) = this.focused_pane.clone().or_else(|| this.model.focused_pane())
                        && let Some(addr) = this.model.any_address_of(&pane)
                        && (is_acp_addr(&addr) || this.pane_is_structured(&pane))
                        && this.model.status(&addr) == Some("working")
                    {
                        let core = this.core.clone();
                        cx.background_executor()
                            .spawn(async move {
                                if let Err(e) = core.call("agent.interrupt", json!({ "agent": addr })) {
                                    tracing::warn!("escape interrupt {addr}: {e:#}");
                                }
                            })
                            .detach();
                        cx.stop_propagation();
                    }
                }
            }))
            .capture_any_mouse_down(cx.listener(|this, ev: &MouseDownEvent, _, _| {
                this.pointer = ev.position;
                if ev.button == MouseButton::Left
                    && !this.overlays.iter().any(|o| o.kind.modal_keys() || o.kind == OverlayKind::Popup)
                {
                    let bounds = this.bounds.borrow();
                    if bounds.get("spaces").is_some_and(|b| b.contains(&ev.position)) {
                        this.rename_focus = RenameFocus::Space;
                    } else if bounds.get("agents").is_some_and(|b| b.contains(&ev.position)) {
                        if !matches!(this.rename_focus, RenameFocus::Agent(_)) {
                            this.rename_focus = RenameFocus::Agent(None);
                        }
                    } else {
                        this.rename_focus = RenameFocus::Tab;
                    }
                }
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| {
                this.drag_window(ev, window);
                if this.drag.is_none() {
                    return;
                }
                if ev.pressed_button != Some(MouseButton::Left) {
                    this.drag = None;
                    this.save();
                    this.publish(cx);
                    return;
                }
                this.drag_to(ev.position);
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, window, cx| {
                this.drag_from = None;
                if this.drag.take().is_some() {
                    this.save();
                    // Mods see the new sizes (the sidebar picks its rail by width).
                    this.publish(cx);
                    this.flush_documents(false, window, cx);
                }
            }))
            .child(tree)
            .children(overlays)
            .children(tracker)
            .children(win_tracker)
            .child(pane_drag)
            .child(crate::bench::frame_end())
    }
}

fn invalidate_asset(path: &std::path::Path, window: &mut Window, cx: &mut gpui::App) {
    let key = gpui_shell::AppAssets::cache_key(&path.to_string_lossy());
    invalidate_image(&gpui::Resource::Embedded(key.clone()), window, cx);
    window.invalidate_svg(&key);
}

fn invalidate_image(resource: &gpui::Resource, window: &mut Window, cx: &mut gpui::App) {
    if let Some(Ok(old)) = cx.cached_asset::<gpui::ImgResourceLoader>(resource) {
        cx.drop_image(old, Some(window));
    }
    cx.remove_asset::<gpui::ImgResourceLoader>(resource);
}

/// The changed paths, plus the files inside changed mod folders (a folder
/// renamed into place arrives as one event). Runs on the UI thread: skips what
/// the watcher ignores (hidden, `node_modules`) and stops at a mod-sized limit.
fn changed_asset_paths(home: &std::path::Path, changed: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    const MAX_FILES: usize = 2000;
    let mut paths: std::collections::BTreeSet<_> = changed.iter().cloned().collect();
    let mut seen = std::collections::HashSet::new();
    let mut pending: Vec<_> = changed.iter().filter(|p| p.starts_with("mods")).cloned().collect();
    while let Some(path) = pending.pop() {
        // Do not follow symlinks out of the mod folder (or into a cycle).
        if !seen.insert(path.clone()) || !home.join(&path).symlink_metadata().is_ok_and(|m| m.is_dir()) { continue; }
        let Ok(entries) = std::fs::read_dir(home.join(&path)) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name == "node_modules" { continue; }
            let child = path.join(&*name);
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                pending.push(child);
            } else if paths.len() < MAX_FILES {
                paths.insert(child);
            } else {
                return paths.into_iter().collect();
            }
        }
    }
    paths.into_iter().collect()
}

fn reconcile_core_update_error(dismissed: &mut Option<String>, core: &crate::update::CoreInfo) {
    if core.error.as_ref().is_some_and(|error| dismissed.as_ref() != Some(error)) || (!core.updating && core.error.is_none()) {
        *dismissed = None;
    }
}

#[cfg(test)]
mod tests {
    use gpui::{div, point, px, size, Context, IntoElement, Render, TestAppContext, Window};
    use gpui::prelude::*;

    #[test]
    fn core_error_dismissal_survives_failed_retries_but_resets_after_recovery() {
        let mut dismissed = Some("connection closed".to_owned());
        let mut core = crate::update::CoreInfo { updating: true, ..Default::default() };
        super::reconcile_core_update_error(&mut dismissed, &core);
        assert!(dismissed.is_some());
        core.updating = false;
        core.error = Some("connection closed".into());
        super::reconcile_core_update_error(&mut dismissed, &core);
        assert_eq!(dismissed, core.error);
        core.error = None;
        super::reconcile_core_update_error(&mut dismissed, &core);
        assert!(dismissed.is_none());
        core.error = Some("connection closed".into());
        super::reconcile_core_update_error(&mut dismissed, &core);
        assert_ne!(dismissed, core.error);
        dismissed = core.error.clone();
        core.error = Some("checksum mismatch".into());
        super::reconcile_core_update_error(&mut dismissed, &core);
        assert!(dismissed.is_none());
    }

    #[test]
    fn going_back_into_a_split_column_takes_its_top_pane_whatever_the_fractions() {
        let r = |x: f32, y: f32, w: f32, h: f32| gpui::Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
        // An odd height splits into halves a fraction apart, the bottom one larger.
        let column = || vec![("top".to_owned(), r(0.0, 0.0, 400.0, 300.4)), ("bottom".to_owned(), r(0.0, 300.4, 400.0, 300.6))];
        assert_eq!(super::nearest(r(401.0, 0.0, 399.0, 601.0), "left", column(), &[]).as_deref(), Some("top"));
        // More overlap by a pixel or more still decides.
        assert_eq!(super::nearest(r(401.0, 250.0, 399.0, 351.0), "left", column(), &[]).as_deref(), Some("bottom"));
        // A visited pane wins even when an unvisited pane has more overlap.
        assert_eq!(super::nearest(r(401.0, 250.0, 399.0, 351.0), "left", column(), &["top".to_owned()]).as_deref(), Some("top"));
    }

    #[test]
    fn directional_round_trips_return_to_the_recent_pane_in_either_split() {
        let r = |x: f32, y: f32, w: f32, h: f32| gpui::Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
        for (a, b, other, out, back) in [
            (r(0.0, 0.0, 400.0, 300.4), r(0.0, 300.4, 400.0, 300.6), r(401.0, 0.0, 399.0, 601.0), "right", "left"),
            (r(0.0, 0.0, 300.4, 400.0), r(300.4, 0.0, 300.6, 400.0), r(0.0, 401.0, 601.0, 399.0), "down", "up"),
        ] {
            let split = || vec![("a".to_owned(), a), ("b".to_owned(), b)];
            assert_eq!(super::nearest(other, back, split(), &[]).as_deref(), Some("a"));
            let mut history = Vec::new();
            for (id, bounds) in [("a", a), ("b", b), ("a", a)] {
                super::remember_pane(&mut history, id);
                assert_eq!(super::nearest(bounds, out, vec![("other".to_owned(), other)], &history).as_deref(), Some("other"));
                super::remember_pane(&mut history, "other");
                assert_eq!(super::nearest(other, back, split(), &history).as_deref(), Some(id));
                super::remember_pane(&mut history, id);
            }
            assert_eq!(history.len(), 3);
            // Recency cannot select a pane outside the requested direction.
            assert_eq!(super::nearest(other, out, split(), &history), None);
        }
    }

    #[test]
    fn terminal_folders_retry_only_definite_spawn_failures_in_home() {
        let failed = |message| anyhow::Error::new(crate::hc::RpcError(serde_json::json!({"message": message, "data": {"kind": "spawn_failed"}})));
        let mut folders = Vec::new();
        let result = super::run_following_folder("deleted".into(), true, None, |folder| {
            folders.push(folder.clone());
            if folder.as_deref() == Some("deleted") { Err(failed("invalid directory")) } else { Ok("t2".into()) }
        }).unwrap();
        assert_eq!(folders, vec![Some("deleted".into()), Some(super::home())]);
        assert_eq!(result, ("t2".into(), super::home()));
        let result = super::run_following_folder("/remote/deleted".into(), true, Some("remote"), |folder| {
            if folder.is_some() { Err(failed("invalid directory")) } else { Ok("remote/t2".into()) }
        }).unwrap();
        assert_eq!(result, ("remote/t2".into(), String::new()));
        for (follow, folder, message) in [(false, "explicit", "invalid directory"), (true, "", "invalid directory"), (true, "inherited", "the holder did not answer")] {
            let mut calls = 0;
            assert!(super::run_following_folder(folder.into(), follow, None, |_| { calls += 1; Err(failed(message)) }).is_err());
            assert_eq!(calls, 1);
        }
        let mut calls = 0;
        assert!(super::run_following_folder("inherited".into(), true, None, |_| { calls += 1; Err(anyhow::anyhow!("lost the core")) }).is_err());
        assert_eq!(calls, 1);
        let mut calls = 0;
        let result = super::run_following_folder("existing".into(), true, None, |_| { calls += 1; Ok("t1".into()) }).unwrap();
        assert_eq!(calls, 1);
        assert_eq!(result, ("t1".into(), "existing".into()));
        let mut calls = 0;
        let error = super::run_following_folder("deleted".into(), true, None, |_| { calls += 1; Err(failed(if calls == 1 { "first" } else { "second" })) }).unwrap_err();
        assert_eq!(calls, 2);
        assert!(error.to_string().contains("second"));
    }

    #[test]
    fn new_spaces_follow_the_persons_active_machine_but_keep_explicit_targets() {
        use serde_json::json;
        let mut model = crate::model::Model::default();
        assert_eq!(super::new_space_machine(&json!({}), true, &model), None);
        model.add_space("remote", "", "", crate::model::Tab::new("tab", "", "pane", "second/t1"));
        model.space_mut("remote").unwrap().machine = Some("second".into());
        for args in [json!({}), json!({"machine": null})] {
            assert_eq!(super::new_space_machine(&args, true, &model).as_deref(), Some("second"));
            assert_eq!(super::new_space_machine(&args, false, &model), None);
        }
        for person in [true, false] {
            assert_eq!(super::new_space_machine(&json!({"machine": "other"}), person, &model).as_deref(), Some("other"));
            assert_eq!(super::new_space_machine(&json!({"machine": ""}), person, &model), None);
        }
        model.add_space("local", "", "", crate::model::Tab::new("local-tab", "", "local-pane", "t1"));
        assert_eq!(super::new_space_machine(&json!({}), true, &model), None);
        model.active = Some("remote".into());
        assert_eq!(super::new_space_machine(&json!({}), true, &model).as_deref(), Some("second"));
    }

    #[gpui::test]
    fn image_invalidation_drops_cached_atlas_tiles_without_loading_missing_assets(cx: &mut TestAppContext) {
        use gpui::{ImgResourceLoader, Resource};
        let temp = if cfg!(target_os = "linux") { crate::test_tmp() } else { std::env::temp_dir() };
        let home = temp.join(format!("herdr-image-atlas-test-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let file = home.join("image.gif");
        let mut encoder = image::codecs::gif::GifEncoder::new(std::fs::File::create(&file).unwrap());
        for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
            encoder.encode_frame(image::Frame::new(image::RgbaImage::from_pixel(2, 2, image::Rgba(color)))).unwrap();
        }
        drop(encoder);
        let resource = Resource::Path(file.into());
        cx.update(|cx| { cx.fetch_asset::<ImgResourceLoader>(&resource); });
        cx.background_executor.run_until_parked();
        let old = cx.update(|cx| cx.cached_asset::<ImgResourceLoader>(&resource)).unwrap().unwrap();
        let context = cx.add_empty_window();
        let displayed = old.clone();
        assert_eq!(old.frame_count(), 1);
        context.draw(point(px(0.0), px(0.0)), size(px(20.0), px(20.0)), move |_, _| {
            gpui::canvas(|_, _, _| (), move |bounds, _, window, _| {
                for frame in 0..displayed.frame_count() {
                    window.paint_image(bounds, bounds, gpui::Corners::default(), displayed.clone(), frame, false).unwrap();
                }
            }).size_full()
        });
        context.update(|window, cx| {
            assert!(window.has_image_atlas_entry(&old));
            super::invalidate_image(&resource, window, cx);
            assert!(!window.has_image_atlas_entry(&old));
            assert!(!cx.has_asset::<ImgResourceLoader>(&resource));
            let missing = Resource::Embedded("mods/missing.png".into());
            super::invalidate_image(&missing, window, cx);
            assert!(!cx.has_asset::<ImgResourceLoader>(&missing));
        });
        std::fs::remove_dir_all(home).unwrap();
    }

    #[gpui::test]
    fn still_image_cache_is_bounded_and_avoids_repeat_downloads(cx: &mut TestAppContext) {
        use gpui::{ImgResourceLoader, Resource, http_client::{AsyncBody, FakeHttpClient, Response}};
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let bytes = bytes.into_inner();
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        cx.update(|cx| cx.set_http_client(FakeHttpClient::create(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            let bytes = bytes.clone();
            async move { Ok(Response::builder().status(200).body(AsyncBody::from(bytes)).unwrap()) }
        })));
        let context = cx.add_empty_window();
        context.deactivate_window();
        let sources: Vec<_> = (0..130).map(|id| Resource::Uri(format!("http://images.test/{id}.png").into())).collect();
        for source in &sources {
            context.update(|_, cx| { cx.fetch_asset::<ImgResourceLoader>(source); });
            context.run_until_parked();
        }
        context.update(|window, cx| { window.draw(cx).clear(cx); });
        assert_eq!(context.update(|_, cx| sources.iter().filter(|source| cx.has_asset::<ImgResourceLoader>(source)).count()), 128);
        let retained = sources.last().unwrap();
        assert!(context.update(|_, cx| cx.fetch_asset::<ImgResourceLoader>(retained)).unwrap().is_ok());
        assert_eq!(requests.load(Ordering::SeqCst), 130);
        assert!(!context.update(|_, cx| cx.has_asset::<ImgResourceLoader>(&sources[0])));
        context.update(|window, cx| {
            cx.set_quit_mode(gpui::QuitMode::Explicit);
            window.remove_window();
        });
    }

    #[gpui::test]
    fn closing_last_window_releases_many_animation_sources_in_one_call(cx: &mut TestAppContext) {
        use gpui::{ImgResourceLoader, Resource, img};
        use std::sync::Arc;
        struct Images(Vec<Resource>);
        impl Render for Images {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().children(self.0.iter().enumerate().map(|(id, source)| {
                    img(gpui::ImageSource::Resource(source.clone())).id(id).size_4()
                }))
            }
        }
        let mut gif = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
                encoder.encode_frame(image::Frame::new(image::RgbaImage::from_pixel(2, 2, image::Rgba(color)))).unwrap();
            }
        }
        let temp = if cfg!(target_os = "linux") { crate::test_tmp() } else { std::env::temp_dir() };
        let dir = temp.join(format!("herdr-many-images-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sources: Vec<_> = (0..24).map(|id| {
            let path = dir.join(format!("{id}.gif"));
            std::fs::write(&path, &gif).unwrap();
            Resource::Path(path.into())
        }).collect();
        let (_, context) = cx.add_window_view(|_, _| Images(sources.clone()));
        context.deactivate_window();
        context.run_until_parked();
        let mut images = context.update(|window, _| window.displayed_images());
        assert_eq!(images.iter().filter(|image| image.frame_count() == 2).count(), 24);
        images.extend(context.update(|_, cx| sources.iter().map(|source| cx.cached_asset::<ImgResourceLoader>(source).unwrap().unwrap()).collect::<Vec<_>>()));
        let weak: Vec<_> = images.iter().map(Arc::downgrade).collect();
        drop(images);
        context.update(|window, cx| {
            cx.set_quit_mode(gpui::QuitMode::Explicit);
            window.remove_window();
        });
        assert!(weak.iter().all(|image| image.upgrade().is_none()), "one close must release every animation and its source regardless of hash order");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[gpui::test]
    fn image_tiles_wait_for_both_scenes_before_retiring(cx: &mut TestAppContext) {
        use std::sync::Arc;
        let context = cx.add_empty_window();
        let image = Arc::new(gpui::RenderImage::new((0..3).map(|red| {
            image::Frame::new(image::RgbaImage::from_pixel(2, 2, image::Rgba([red, 0, 0, 255])))
        }).collect::<smallvec::SmallVec<[_; 1]>>()));
        for frame in 0..3 {
            let painted = image.clone();
            context.draw(point(px(0.), px(0.)), size(px(20.), px(20.)), move |_, _| {
                gpui::canvas(|_, _, _| (), move |bounds, _, window, _| {
                    window.paint_image(bounds, bounds, gpui::Corners::default(), painted.clone(), frame, false).unwrap();
                }).size_full()
            });
            for previous in 0..=frame {
                assert_eq!(context.update(|window, _| window.has_image_frame_atlas_entry(&image, previous)), previous + 1 >= frame);
            }
        }
    }

    #[gpui::test]
    fn clipped_images_do_not_upload(cx: &mut TestAppContext) {
        use std::sync::Arc;
        let context = cx.add_empty_window();
        let image = Arc::new(gpui::RenderImage::new(smallvec::smallvec![
            image::Frame::new(image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255])))
        ]));
        for mask in [
            gpui::Bounds::new(point(px(50.), px(50.)), size(px(10.), px(10.))),
            gpui::Bounds::new(point(px(0.), px(0.)), size(px(0.), px(0.))),
        ] {
            for _ in 0..2 {
                let painted = image.clone();
                context.draw(point(px(0.), px(0.)), size(px(20.), px(20.)), move |_, _| {
                    gpui::canvas(|_, _, _| (), move |bounds, _, window, _| {
                        window.with_content_mask(Some(gpui::ContentMask { bounds: mask }), |window| {
                            window.paint_image(bounds, bounds, gpui::Corners::default(), painted.clone(), 0, false).unwrap();
                        });
                    }).size_full()
                });
                assert!(!context.update(|window, _| window.has_image_frame_atlas_entry(&image, 0)));
            }
        }
        let painted = image.clone();
        context.draw(point(px(0.), px(0.)), size(px(20.), px(20.)), move |_, _| {
            gpui::canvas(|_, _, _| (), move |bounds, _, window, _| {
                window.paint_image(bounds, bounds, gpui::Corners::default(), painted.clone(), 0, false).unwrap();
            }).size_full()
        });
        assert!(context.update(|window, _| window.has_image_frame_atlas_entry(&image, 0)));
    }

    #[gpui::test]
    fn background_images_stay_warm_and_release_animations(cx: &mut TestAppContext) {
        use gpui::{AssetSource as _, ImgResourceLoader, Resource, img};
        use std::sync::Arc;
        struct ImageProbe { source: Option<Resource>, animate: bool }
        impl Render for ImageProbe {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let mut element = div().size_full();
                if let Some(source) = &self.source {
                    element = element.child(img(gpui::ImageSource::Resource(source.clone())).size_4());
                    if self.animate {
                        element = element.child(img(gpui::ImageSource::Resource(source.clone())).id("animated").size_4());
                    }
                }
                element
            }
        }
        let temp = if cfg!(target_os = "linux") { crate::test_tmp() } else { std::env::temp_dir() };
        let home = temp.join(format!("herdr-image-release-test-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let gif = home.join("animated.gif");
        let png = home.join("still.png");
        let mut encoder = image::codecs::gif::GifEncoder::new(std::fs::File::create(&gif).unwrap());
        for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
            encoder.encode_frame(image::Frame::new(image::RgbaImage::from_pixel(2, 2, image::Rgba(color)))).unwrap();
        }
        drop(encoder);
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255])).save(&png).unwrap();
        let assets = gpui_shell::AppAssets::new(home.clone());
        assert_eq!(
            assets.image_resource(&Resource::Path(gif.clone().into())),
            Resource::Path(gif.clone().into())
        );
        let (view, cx) = cx.add_window_view(|_, _| ImageProbe { source: None, animate: false });
        cx.deactivate_window();
        for file in [&gif, &png, &gif] {
            let resource = Resource::Path(file.clone().into());
            view.update(cx, |view, cx| { view.source = Some(resource.clone()); cx.notify(); });
            cx.run_until_parked();
            let image = cx.update(|_, cx| cx.cached_asset::<ImgResourceLoader>(&resource)).unwrap().unwrap();
            assert_eq!(image.frame_count(), 1);
            assert_eq!(&image.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255]);
            assert!(cx.update(|window, _| window.has_image_frame_atlas_entry(&image, 0)));
            if file == &gif {
                view.update(cx, |view, cx| { view.animate = true; cx.notify(); });
                cx.run_until_parked();
                let animation = cx.update(|window, _| window.displayed_images().into_iter().find(|image| image.frame_count() == 2)).unwrap();
                assert_eq!(animation.as_bytes(0).unwrap().as_ptr(), image.as_bytes(0).unwrap().as_ptr());
                let weak_animation = Arc::downgrade(&animation);
                drop(animation);
                view.update(cx, |view, cx| { view.animate = false; cx.notify(); });
                cx.run_until_parked();
                assert!(weak_animation.upgrade().is_none(), "a still background must not keep animation pixels");
                assert!(cx.update(|window, _| window.has_image_frame_atlas_entry(&image, 0)));
            }
            let weak = Arc::downgrade(&image);
            drop(image);
            view.update(cx, |view, cx| { view.source = None; cx.notify(); });
            cx.run_until_parked();
            assert!(weak.upgrade().is_some(), "still pixels stay warm for layout switches");
            view.update(cx, |view, cx| { view.source = Some(resource.clone()); cx.notify(); });
            cx.update(|window, cx| { window.draw(cx).clear(cx); });
            let warm = cx.update(|_, cx| cx.cached_asset::<ImgResourceLoader>(&resource)).unwrap().unwrap();
            assert!(Arc::ptr_eq(&warm, &weak.upgrade().unwrap()));
            assert!(cx.update(|window, _| window.has_image_frame_atlas_entry(&warm, 0)));
            drop(warm);
        }
        // Leave both the source and animation unowned in the same close operation.
        view.update(cx, |view, cx| { view.animate = true; cx.notify(); });
        cx.run_until_parked();
        let images = cx.update(|window, _| window.displayed_images());
        let weak: Vec<_> = images.iter().map(Arc::downgrade).collect();
        assert!(images.iter().any(|image| image.frame_count() == 2));
        drop(images);
        cx.update(|window, cx| {
            cx.set_quit_mode(gpui::QuitMode::Explicit);
            window.remove_window();
        });
        assert!(weak.iter().all(|image| image.upgrade().is_none()), "closing the last window must prune in dependency order without another draw");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn directory_changes_include_nested_assets() {
        let temp = if cfg!(target_os = "linux") { crate::test_tmp() } else { std::env::temp_dir() };
        let home = temp.join(format!("herdr-image-directory-test-{}", std::process::id()));
        std::fs::create_dir_all(home.join("mods/probe/nested")).unwrap();
        std::fs::write(home.join("mods/probe/nested/icon.svg"), "svg").unwrap();
        for ignored in ["node_modules/pkg", ".git"] {
            std::fs::create_dir_all(home.join("mods/probe").join(ignored)).unwrap();
            std::fs::write(home.join("mods/probe").join(ignored).join("logo.svg"), "svg").unwrap();
        }
        let changed = vec![std::path::PathBuf::from("mods/probe")];
        // A repeated root and a covered child are walked once.
        let events = [changed[0].clone(), changed[0].clone(), "mods/probe/nested".into()];
        let paths = super::changed_asset_paths(&home, &events);
        assert!(paths.contains(&std::path::PathBuf::from("mods/probe/nested/icon.svg")));
        assert!(paths.contains(&changed[0]));
        assert!(!paths.iter().any(|p| p.to_string_lossy().contains("node_modules") || p.to_string_lossy().contains(".git")));
        assert_eq!(paths.iter().filter(|p| p.ends_with("icon.svg")).count(), 1);
        std::fs::remove_dir_all(&home).unwrap();
        assert_eq!(super::changed_asset_paths(&home, &changed), changed);
    }

    #[test]
    fn asset_cache_keys_match_file_events_without_requiring_the_file() {
        use gpui_shell::AppAssets;
        let expected = AppAssets::cache_key("mods/probe/icon.svg");
        for alias in ["./mods/probe/icon.svg", "/mods/probe/icon.svg", "mods//probe/./icon.svg"] {
            assert_eq!(AppAssets::cache_key(alias), expected);
        }
        // Traversal remains invalid, rather than becoming a valid cache hit.
        assert_eq!(AppAssets::cache_key("mods/probe/../icon.svg").as_ref(), "mods/probe/../icon.svg");
        #[cfg(windows)]
        assert_eq!(AppAssets::cache_key(r"mods\probe\icon.svg"), expected);
    }

    #[test]
    fn svg_invalidation_removes_every_size_but_preserves_other_assets() {
        use gpui::{AtlasKey, DevicePixels, HeadlessAtlas, PlatformAtlas, RenderSvgParams, Size};
        let atlas = HeadlessAtlas::default();
        let key = |path: &str, size| AtlasKey::Svg(RenderSvgParams {
            path: path.to_owned().into(), size: Size::new(DevicePixels(size), DevicePixels(size)),
        });
        let keys = [key("icons/icon.svg", 16), key("icons/icon.svg", 32), key("icons/nested/icon.svg", 16), key("icons-other/icon.svg", 16), AtlasKey::Image(gpui::RenderImageParams { image_id: gpui::ImageId(1), frame_index: 0 })];
        let mut build = || Ok(Some((Size::new(DevicePixels(1), DevicePixels(1)), std::borrow::Cow::Borrowed(&[255u8][..]))));
        let tiles: Vec<_> = keys.iter().map(|key| atlas.get_or_insert_with(key.clone(), &mut build).unwrap().unwrap()).collect();
        atlas.remove_svg("icons");
        atlas.remove_svg("icons");
        for (index, key) in keys.iter().enumerate() {
            assert_eq!(atlas.contains(key), index >= 3);
            let tile = atlas.get_or_insert_with(key.clone(), &mut build).unwrap().unwrap();
            assert_eq!(tile == tiles[index], index >= 3);
        }
    }

    #[test]
    fn agent_commands_keep_arguments_out_of_shell_source() {
        let cmd = vec!["agent".to_owned(), "a 'quoted' task; $(not-a-command)\nnext line".to_owned(), "".to_owned()];
        for login in [false, true] {
            for sh in ["/bin/bash", "/bin/zsh", "/bin/sh", "/bin/dash", "/bin/fish"] {
                let run = super::unix_agent_argv(sh.into(), cmd.clone(), login, true);
                assert_eq!(run[0], sh);
                assert_eq!(run[1], if login { "-lic" } else { "-ic" });
                assert_eq!(run[3], sh);
                assert_eq!(run[4..], cmd);
                let expected = match (sh.ends_with("/fish"), login) {
                    (true, true) => "command $argv[2..]; exec $argv[1] -li",
                    (true, false) => "command $argv[2..]; exec $argv[1] -i",
                    (false, true) => "\"$@\"; exec \"$0\" -li",
                    (false, false) => "\"$@\"; exec \"$0\" -i",
                };
                assert_eq!(run[2], expected);
                // Whatever the wrapper, a pane shows the agent it runs, not the shell.
                let agent = super::unix_agent_argv(sh.into(), vec!["cursor-agent".into()], login, true);
                assert_eq!(crate::programs::of(None, &agent)["key"], "cursor", "{agent:?}");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn other_shells_run_the_command_on_the_app_path_then_return_to_the_shell() {
        use std::os::unix::fs::PermissionsExt;
        let temp = if cfg!(target_os = "linux") { std::path::PathBuf::from("/var/tmp") } else { std::env::temp_dir() };
        let dir = temp.join(format!("herdr-other-shell-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Stands in for nushell: it would fail on sh syntax, so it only records how it was started.
        let nu = dir.join("nu");
        std::fs::write(&nu, "#!/bin/sh\nprintf 'shell:%s\\n' \"$@\"\n").unwrap();
        std::fs::set_permissions(&nu, std::fs::Permissions::from_mode(0o755)).unwrap();
        let task = "a 'quoted' task; $(not-a-command)\nnext line";
        // `env` is found only on this PATH, not on the PATH the terminal starts with.
        let cmd = vec!["env".to_owned(), "printf".to_owned(), "[%s]\n".to_owned(), task.to_owned(), "".to_owned()];
        for login in [false, true] {
            let run = super::unix_agent_argv(nu.to_string_lossy().into(), cmd.clone(), login, true);
            let out = std::process::Command::new(&run[0]).args(&run[1..]).env("PATH", "/nonexistent").output().unwrap();
            assert_eq!(String::from_utf8(out.stdout).unwrap(), format!("[{task}]\n[]\nshell:-i\n"));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A split's program runs after your rc files (their PATH), then the shell
    /// becomes it: its exit code ends the pane, a missing one fails as the core's does.
    #[cfg(unix)]
    #[test]
    fn a_split_program_replaces_your_shell() {
        use std::os::unix::{fs::PermissionsExt, process::CommandExt};
        let temp = if cfg!(target_os = "linux") { std::path::PathBuf::from("/var/tmp") } else { std::env::temp_dir() };
        let home = temp.join(format!("herdr-split-shell-test-{}", std::process::id()));
        std::fs::create_dir_all(home.join("rc bin")).unwrap();
        std::fs::create_dir_all(home.join(".config/fish")).unwrap();
        let rc = "case $- in *i*) export PATH=\"$HOME/rc bin:$PATH\";; esac\n";
        for file in [".bashrc", ".zshrc", ".profile", ".bash_profile", ".zprofile"] {
            std::fs::write(home.join(file), rc).unwrap();
        }
        std::fs::write(home.join(".config/fish/config.fish"), "if status is-interactive\nset -gx PATH \"$HOME/rc bin\" $PATH\nend\n").unwrap();
        let tool = home.join("rc bin/rc-tool");
        std::fs::write(&tool, "#!/bin/sh\nprintf '<%s>\\n' \"$@\"\nexit 7\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let arg = "a 'quoted' $HOME; $(not-a-command)\nnext line";
        let run = |argv: Vec<String>| {
            let mut cmd = std::process::Command::new(&argv[0]);
            cmd.args(&argv[1..]).env_clear().env("HOME", &home).env("ZDOTDIR", &home)
                .env("PATH", "/usr/bin:/bin").env("TERM", "dumb").stdin(std::process::Stdio::null());
            unsafe extern "C" {
                fn setsid() -> std::ffi::c_int;
            }
            // Interactive shells otherwise take the test runner's terminal.
            unsafe { cmd.pre_exec(|| if setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) }) };
            cmd.output().unwrap()
        };
        for sh in ["/bin/bash", "/bin/sh", "/bin/zsh", "/usr/bin/fish"] {
            if !std::path::Path::new(sh).exists() {
                continue;
            }
            // dash reads no rc file unless it is a login shell.
            for login in [sh.ends_with("/sh"), true] {
                let out = run(super::unix_agent_argv(sh.into(), vec!["rc-tool".into(), arg.into(), "".into()], login, false));
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert!(stdout.ends_with(&format!("<{arg}>\n<>\n")), "{sh}: {stdout:?} {}", String::from_utf8_lossy(&out.stderr));
                assert_eq!(out.status.code(), Some(7), "{sh}");
                let out = run(super::unix_agent_argv(sh.into(), vec!["no-such-program".into()], login, false));
                assert_eq!(out.status.code(), Some(127), "{sh}");
                assert!(String::from_utf8_lossy(&out.stderr).contains("cannot start \"no-such-program\": No such file or directory"), "{sh}: {}", String::from_utf8_lossy(&out.stderr));
                // Until the shell becomes it, a pane shows the agent, not the shell.
                let agent = super::unix_agent_argv(sh.into(), vec!["cursor-agent".into()], login, false);
                assert_eq!(crate::programs::of(None, &agent)["key"], "cursor", "{agent:?}");
            }
        }
        // A shell that can't parse the wrapper (nushell): the program as given.
        let cmd = vec!["rc-tool".to_owned(), arg.to_owned()];
        assert_eq!(super::unix_agent_argv("/usr/bin/nu".into(), cmd.clone(), false, false), cmd);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn early_launch_failures_show_exit_code_and_last_output_lines() {
        let mut record = serde_json::json!({"reason":"exited", "exit_code":127, "created_ms":1000, "finished_ms":3000});
        assert_eq!(super::launch_failure(&record, || "old\none\ntwo\nthree\nfour\nfive\n\n".into()).as_deref(), Some("Exit code 127\none\ntwo\nthree\nfour\nfive"));
        assert_eq!(super::launch_failure(&record, String::new).as_deref(), Some("Exit code 127"));
        record["exit_code"] = 0.into();
        assert!(super::launch_failure(&record, String::new).is_none());
        record["exit_code"] = 1.into();
        record["finished_ms"] = 3001.into();
        assert!(super::launch_failure(&record, String::new).is_none());
        record["finished_ms"] = 999.into();
        assert!(super::launch_failure(&record, String::new).is_none());
        record["finished_ms"] = 2000.into();
        record["reason"] = "killed".into();
        assert!(super::launch_failure(&record, String::new).is_none());
        assert!(super::launch_failure(&serde_json::Value::Null, String::new).is_none());
    }

    /// The split wrapper's own "cannot start" comes after the person's shell config, so a
    /// slow config can't hide it. Its program's own exits keep the 2 s rule, even an exit 127
    /// that prints the same words: only the wrapper knows its launch token.
    #[cfg(unix)]
    #[test]
    fn a_split_that_cannot_start_its_program_fails_however_slow_the_shell() {
        use std::os::unix::process::CommandExt;
        let temp = if cfg!(target_os = "linux") { std::path::PathBuf::from("/var/tmp") } else { std::env::temp_dir() };
        let home = temp.join(format!("herdr-split-slow-test-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".config/fish")).unwrap();
        let rc = "case $- in *i*) [ -z \"$SLOW_RC\" ] || sleep 2.1;; esac\n";
        for file in [".bashrc", ".zshrc", ".profile", ".bash_profile", ".zprofile"] {
            std::fs::write(home.join(file), rc).unwrap();
        }
        std::fs::write(home.join(".config/fish/config.fish"), "if status is-interactive; and set -q SLOW_RC\nsleep 2.1\nend\n").unwrap();
        let token = "herdr-test-launch-token";
        // As the core records it: exit, start and end, argv, the last title the terminal set, labels.
        let run = |argv: Vec<String>, shell: &str, slow: bool| {
            let mut cmd = std::process::Command::new(&argv[0]);
            cmd.args(&argv[1..]).env_clear().env("HOME", &home).env("ZDOTDIR", &home).env("SHELL", shell)
                .env("PATH", "/usr/bin:/bin").env("TERM", "dumb").env("HERDR_LAUNCH", token).stdin(std::process::Stdio::null());
            if slow {
                cmd.env("SLOW_RC", "1");
            }
            unsafe extern "C" {
                fn setsid() -> std::ffi::c_int;
            }
            unsafe { cmd.pre_exec(|| if setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) }) };
            let start = std::time::Instant::now();
            let out = cmd.output().unwrap();
            let bytes = [out.stdout, out.stderr].concat();
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let title = text.rsplit("\x1b]2;").next().filter(|_| text.contains("\x1b]2;")).and_then(|t| t.split('\x07').next()).unwrap_or_default().to_owned();
            let record = serde_json::json!({"reason":"exited", "exit_code":out.status.code(), "created_ms":0, "finished_ms":start.elapsed().as_millis() as u64,
                "argv":argv, "title":title, "labels":{"herdr.launch":token}});
            (record, text.replace(&format!("\x1b]2;{title}\x07"), ""))
        };
        let missing = vec!["no-such-prog".to_owned()];
        // Astra's case: `sh` starts, runs past 2 s, says the wrapper's words itself, exits 127.
        let own_127 = ["sh", "-c", "sleep 2.1; printf 'cannot start \"sh\": No such file or directory\\n'; exit 127"].map(String::from).to_vec();
        let shells = ["/bin/bash", "/bin/zsh", "/usr/bin/fish"].into_iter().filter(|s| std::path::Path::new(s).exists());
        let wrapped = |cmd: &Vec<String>| shells.clone().flat_map(|sh| [(sh, super::unix_agent_argv(sh.into(), cmd.clone(), false, false)), (sh, crate::remote_shell::exec(cmd.clone()))]).collect::<Vec<_>>();
        std::thread::scope(|s| {
            let mut checks = vec![];
            for (sh, argv) in wrapped(&missing) {
                checks.push(s.spawn(move || {
                    let (record, output) = run(argv.clone(), sh, true);
                    assert!(record["finished_ms"].as_u64().unwrap() > 2000, "{argv:?}: {record}");
                    let toast = super::launch_failure(&record, || output.clone());
                    assert!(toast.as_deref().is_some_and(|t| t.starts_with("Exit code 127\n") && t.contains("\ncannot start \"no-such-prog\": No such file or directory")), "{argv:?}: {toast:?} {record} {output:?}");
                }));
            }
            for (sh, argv) in wrapped(&own_127) {
                checks.push(s.spawn(move || {
                    let (record, output) = run(argv.clone(), sh, false);
                    assert_eq!(record["exit_code"], 127, "{argv:?}: {output:?}");
                    assert!(output.contains("cannot start \"sh\": No such file or directory"), "{argv:?}: {output:?}");
                    assert!(record["finished_ms"].as_u64().unwrap() > 2000, "{argv:?}: {record}");
                    assert_eq!(super::launch_failure(&record, || output.clone()), None, "{argv:?}: {record} {output:?}");
                }));
            }
            // The program never sees the token, so it can't say it by accident.
            for (sh, argv) in wrapped(&vec!["env".to_owned()]) {
                checks.push(s.spawn(move || {
                    let (record, output) = run(argv.clone(), sh, false);
                    assert_eq!(record["exit_code"], 0, "{argv:?}: {output:?}");
                    assert!(output.contains("SHELL=") && !output.contains(token), "{argv:?}: {output:?}");
                }));
            }
            for check in checks {
                check.join().unwrap();
            }
        });
        // A late exit with the wrapper's token in the title but another code keeps the 2 s rule.
        let record = serde_json::json!({"reason":"exited", "exit_code":7, "created_ms":0, "finished_ms":9000, "title":token, "labels":{"herdr.launch":token}});
        assert!(super::launch_failure(&record, || "cannot start \"x\": No such file or directory\n".into()).is_none());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[gpui::test]
    fn traffic_light_dividers_start_below_the_toolbar(cx: &mut TestAppContext) {
        use std::{cell::Cell, rc::Rc};
        let mut layout: crate::layout::Node = serde_json::from_str(include_str!("../defaults/layouts/main.json")).unwrap();
        let context = cx.add_empty_window();
        for (macos, titlebar, width, zoom, expected) in [
            (true, crate::layout::Titlebar::Layout, 44.0, 1.0, 42.0),
            (true, crate::layout::Titlebar::Layout, 90.0, 0.75, 42.0),
            (true, crate::layout::Titlebar::Layout, 60.0, 1.5, 0.0),
            (true, crate::layout::Titlebar::Layout, 60.0, 1.0, 42.0),
            (true, crate::layout::Titlebar::Layout, 76.0, 1.0, 42.0),
            (true, crate::layout::Titlebar::Layout, 268.0, 1.0, 0.0),
            (false, crate::layout::Titlebar::Layout, 44.0, 1.0, 0.0),
            (true, crate::layout::Titlebar::System, 44.0, 1.0, 0.0),
        ] {
            layout.titlebar = Some(titlebar);
            layout.children_mut().unwrap()[0].size = Some(crate::layout::Size::Px { px: width });
            let top = super::traffic_light_divider_inset(&layout, crate::layout::Axis::Row, 0, macos, zoom);
            assert_eq!(top, expected);
            let bounds = Rc::new(Cell::new(gpui::Bounds::default()));
            let observed = bounds.clone();
            context.draw(point(px(0.0), px(0.0)), size(px(1.0), px(100.0)), move |_, _| {
                div().size_full().relative().flex().items_center().justify_center().child(
                    super::separator_line(true, 1.0, gpui::rgb(0xffffff).into(), top).child(
                        gpui::canvas(move |bounds, _, _| observed.set(bounds), |_, _, _, _| {}).size_full()
                    )
                )
            });
            assert_eq!(bounds.get().origin.y, px(expected));
            assert_eq!(bounds.get().size.height, px(100.0 - expected));
        }
        layout.traffic_lights = None;
        assert_eq!(super::traffic_light_divider_inset(&layout, crate::layout::Axis::Row, 0, true, 1.0), 0.0);
    }

    #[gpui::test]
    fn fitted_popups_use_their_rendered_size_at_the_window_edge(cx: &mut TestAppContext) {
        struct Popup {
            height: f32,
            edge: bool,
        }
        impl Render for Popup {
            fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let viewport = window.viewport_size();
                let at = if self.edge {
                    point(viewport.width - px(40.0), viewport.height - px(40.0))
                } else {
                    point(px(100.0), px(200.0))
                };
                div().size_full().child(super::position_popup(
                    at,
                    div()
                        .id("popup")
                        .debug_selector(|| "popup".into())
                        .w(px(220.0))
                        .max_h(viewport.height * 0.85)
                        .child(div().h(px(self.height))),
                ))
            }
        }
        let (view, cx) = cx.add_window_view(|_, _| Popup { height: 72.0, edge: false });
        for (height, edge) in [(72.0, false), (72.0, true), (300.0, true)] {
            view.update(cx, |view, cx| {
                view.height = height;
                view.edge = edge;
                cx.notify();
            });
            cx.run_until_parked();
            let bounds = cx.debug_bounds("popup").unwrap();
            assert_eq!(bounds.size, size(px(220.0), px(height)));
            if edge {
                let viewport = cx.update(|window, _| window.viewport_size());
                assert_eq!(bounds.bottom_right(), point(viewport.width - px(8.0), viewport.height - px(8.0)));
            } else {
                assert_eq!(bounds.origin, point(px(100.0), px(200.0)));
            }
        }
    }

    #[test]
    fn terminal_menu_is_a_root_layout_choice_that_roundtrips() {
        let mut root = super::default_layout("main");
        assert_eq!(root.terminal_menu, None);
        assert!(serde_json::to_value(&root).unwrap().get("terminal_menu").is_none());
        root.terminal_menu = Some("personal-menu".into());
        let restored: super::Node = serde_json::from_value(serde_json::to_value(&root).unwrap()).unwrap();
        assert_eq!(restored.terminal_menu.as_deref(), Some("personal-menu"));
    }

    /// Runs `window::tests::<name>` alone in a test process of its own, on a
    /// fresh app home: a window takes process-wide things (the app home, the
    /// mods' request channel) that tests beside it would otherwise share.
    fn in_own_process(name: &str) {
        let home = crate::test_tmp().join(format!("herdr2-window-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &format!("window::tests::{name}"), "--nocapture", "--test-threads=1"])
            .env("HERDR2_APP_HOME", &home)
            .env("HERDR2_TEST_WINDOW", name)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&home);
        let log = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success() && log.contains("1 passed"), "{log}");
    }

    /// In the process `in_own_process` started for `name`: whether to go on.
    fn running_alone(name: &str) -> bool {
        std::env::var("HERDR2_TEST_WINDOW").as_deref() == Ok(name)
    }

    /// The window on a core that's never there (`hc::offline`), showing
    /// `layout` with `mod_name` (its `main.js`) in the home, past the first-run
    /// welcome (it lists machines through the core). No other mods: the
    /// shipped ones run programs on threads of their own, which the test
    /// executor refuses.
    fn open_window<'a>(cx: &'a mut gpui::TestAppContext, mod_name: &str, main_js: &str, layout: serde_json::Value) -> (gpui::Entity<super::AppWindow>, &'a mut gpui::VisualTestContext) {
        open_window_on(cx, crate::hc::offline(), mod_name, main_js, layout)
    }

    fn open_window_on<'a>(cx: &'a mut gpui::TestAppContext, core: std::sync::Arc<crate::hc::Core>, mod_name: &str, main_js: &str, layout: serde_json::Value) -> (gpui::Entity<super::AppWindow>, &'a mut gpui::VisualTestContext) {
        use serde_json::json;
        let home = crate::mods::app_dir();
        crate::mods::write_workspace().unwrap();
        let folder = home.join("mods").join(mod_name);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("mod.json"), r#"{ "place": "none" }"#).unwrap();
        std::fs::write(folder.join("main.js"), main_js).unwrap();
        std::fs::create_dir_all(home.join("layouts")).unwrap();
        std::fs::write(home.join("layouts/test.json"), layout.to_string()).unwrap();
        cx.update(crate::mods::init);
        let requests = crate::mods::install_policy();
        crate::mods::set_shared("layout".into(), json!("test"));
        crate::mods::set_shared("frame.welcomed".into(), json!("test"));
        let (view, cx) = cx.add_window_view(|window, cx| super::AppWindow::with_file_events(core, requests, Some(crate::files::FileEvents::for_test(home.clone())), window, cx));
        assert_eq!(view.update(cx, |this, _| this.layout_name.clone()), "test");
        (view, cx)
    }

    /// A mod whose every version keeps `mib` MiB alive at module scope, in
    /// 1 MiB strings so no single allocation is large.
    fn heavy_mod(version: usize, mib: usize) -> String {
        format!("import {{ View, div }} from \"gpui-kit\";\n\
            const kept = Array.from({{ length: {mib} }}, (_, i) => \"{version}-\".padEnd(1 << 20, String(i)));\n\
            export default class Heavy extends View {{ render() {{ return div().child(String(kept.length)); }} }}\n")
    }

    /// Saves `heavy`'s main.js as `version` and lets the window apply it, then
    /// returns the mounted copies and their errors.
    fn edit_heavy(view: &gpui::Entity<super::AppWindow>, version: usize, mib: usize, cx: &mut gpui::VisualTestContext) -> (usize, Vec<String>) {
        std::fs::write(crate::mods::app_dir().join("mods/heavy/main.js"), heavy_mod(version, mib)).unwrap();
        cx.update(|window, cx| view.update(cx, |this, cx| this.files_changed(vec!["mods/heavy/main.js".into()], window, cx)));
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        view.update(cx, |this, _| (
            this.copies.values().filter(|c| c.root.is_some()).count(),
            this.copies.values().filter_map(|c| c.error.clone()).collect(),
        ))
    }

    fn open_heavy(cx: &mut gpui::TestAppContext, mib: usize) -> (gpui::Entity<super::AppWindow>, &mut gpui::VisualTestContext) {
        let (view, cx) = open_window(cx, "heavy", &heavy_mod(0, mib), serde_json::json!({ "id": "heavy", "type": "block", "content": { "mod": "heavy" } }));
        cx.run_until_parked();
        assert_eq!(view.update(cx, |this, _| this.copies.values().filter(|c| c.root.is_some()).count()), 1, "the mod loads");
        (view, cx)
    }

    #[test]
    fn repeated_mod_edits_start_a_new_vm_before_the_old_one_fills() {
        in_own_process("edits_restart_full_vm");
    }

    /// QuickJS frees evaluated modules only with their context, so each edit
    /// leaves the replaced version in the window's VM. At 16 MiB a version the
    /// 256 MiB ceiling fails a reload within twenty edits unless the window
    /// moves its mods to a new VM first. The full VM and its code cache go away.
    #[gpui::test]
    fn edits_restart_full_vm(cx: &mut gpui::TestAppContext) {
        if !running_alone("edits_restart_full_vm") {
            return;
        }
        let (view, cx) = open_heavy(cx, 16);
        let first = view.update(cx, |this, _| std::rc::Rc::downgrade(this.runtime.as_ref().unwrap()));
        for version in 1..=40 {
            assert_eq!(edit_heavy(&view, version, 16, cx), (1, vec![]), "edit {version}");
        }
        assert!(first.upgrade().is_none(), "the replaced VM, with its code cache, is freed");
        let share = view.update(cx, |this, _| this.runtime.as_ref().unwrap().memory_share());
        assert!(share < super::RESTART_MODS_AT, "the current VM has room: {share}");
    }

    #[test]
    fn a_reload_that_runs_out_of_memory_starts_a_new_vm() {
        in_own_process("out_of_memory_restarts_vm");
    }

    /// One version fills more than half the VM, so the first edit cannot fit
    /// beside it. The reload fails with out of memory. The window then loads
    /// the saved code in a new VM instead of leaving the failed view.
    #[gpui::test]
    fn out_of_memory_restarts_vm(cx: &mut gpui::TestAppContext) {
        if !running_alone("out_of_memory_restarts_vm") {
            return;
        }
        let (view, cx) = open_heavy(cx, 140);
        let first = view.update(cx, |this, _| std::rc::Rc::downgrade(this.runtime.as_ref().unwrap()));
        assert!(view.update(cx, |this, _| this.runtime.as_ref().unwrap().memory_share()) < super::RESTART_MODS_AT);
        assert_eq!(edit_heavy(&view, 1, 140, cx), (1, vec![]));
        assert!(first.upgrade().is_none(), "the exhausted VM is freed");
    }

    #[test]
    fn reload_errors_share_the_load_recovery_cap() {
        in_own_process("reload_recovery_cap");
    }

    #[gpui::test]
    fn reload_recovery_cap(cx: &mut gpui::TestAppContext) {
        if !running_alone("reload_recovery_cap") {
            return;
        }
        let (view, cx) = open_heavy(cx, 140);
        assert_eq!(edit_heavy(&view, 1, 140, cx), (1, vec![]));
        let fresh = view.update(cx, |this, _| std::rc::Rc::downgrade(this.runtime.as_ref().unwrap()));
        let (loaded, errors) = edit_heavy(&view, 2, 140, cx);
        assert_eq!(loaded, 1, "a failed reload keeps the previous view");
        assert_eq!(errors.len(), 1, "the capped reload error is reported");
        view.update(cx, |this, _| {
            assert!(std::rc::Rc::ptr_eq(&fresh.upgrade().unwrap(), this.runtime.as_ref().unwrap()));
            this.load_recovery = Some(std::time::Instant::now() - std::time::Duration::from_secs(61));
        });
        assert_eq!(edit_heavy(&view, 3, 140, cx), (1, vec![]));
        assert!(fresh.upgrade().is_none(), "a later edit can recover again");
    }

    #[test]
    fn unchanged_mod_events_do_not_restart_a_healthy_vm() {
        in_own_process("unchanged_mod_events");
    }

    #[gpui::test]
    fn unchanged_mod_events(cx: &mut gpui::TestAppContext) {
        if !running_alone("unchanged_mod_events") {
            return;
        }
        let (view, cx) = open_heavy(cx, 200);
        let first = view.update(cx, |this, _| std::rc::Rc::downgrade(this.runtime.as_ref().unwrap()));
        std::fs::write(crate::mods::app_dir().join("mods/heavy/gpui-kit.d.ts"), "generated").unwrap();
        cx.update(|window, cx| view.update(cx, |this, cx| {
            this.files_changed(vec!["mods/heavy".into(), "mods/heavy/main.js".into()], window, cx);
        }));
        cx.run_until_parked();
        view.update(cx, |this, _| {
            assert!(std::rc::Rc::ptr_eq(&first.upgrade().unwrap(), this.runtime.as_ref().unwrap()));
            assert!(this.copies.values().all(|c| c.error.is_none()));
        });
        assert_eq!(edit_heavy(&view, 1, 200, cx), (1, vec![]));
        assert!(first.upgrade().is_none(), "a real edit refreshes the healthy full VM");
        assert_eq!(edit_heavy(&view, 2, 200, cx), (1, vec![]), "healthy proactive refreshes are not capped");
    }

    #[test]
    fn failed_mass_mounts_retry_on_a_fresh_vm() {
        in_own_process("mounts_restart_full_vm");
    }

    #[gpui::test]
    fn mounts_restart_full_vm(cx: &mut gpui::TestAppContext) {
        if !running_alone("mounts_restart_full_vm") {
            return;
        }
        let (view, cx) = open_heavy(cx, 140);
        let first = view.update(cx, |this, _| {
            std::rc::Rc::downgrade(this.runtime.as_ref().unwrap())
        });
        cx.update(|window, cx| {
            view.update(cx, |this, cx| {
                this.chrome = serde_json::from_value(
                    serde_json::json!({"id":"empty","type":"column","children":[]}),
                )
                .unwrap();
                this.sync(window, cx);
            })
        });
        assert_eq!(edit_heavy(&view, 1, 140, cx), (0, vec![]));
        let share = view.update(cx, |this, _| this.runtime.as_ref().unwrap().memory_share());
        eprintln!("heap share before mount: {share}");
        assert!(share < super::RESTART_MODS_AT);
        cx.update(|window, cx| {
            view.update(cx, |this, cx| {
                this.chrome = serde_json::from_value(serde_json::json!({
                    "id":"many","type":"column","children": (0..20).map(|i| serde_json::json!({
                        "id":format!("copy{i}"),"type":"block","content":{"mod":"heavy"}
                    })).collect::<Vec<_>>()
                }))
                .unwrap();
                this.sync(window, cx);
                eprintln!(
                    "heap share after mount: {}",
                    this.runtime.as_ref().unwrap().memory_share()
                );
            })
        });
        cx.run_until_parked();
        let (loaded, errors) = view.update(cx, |this, _| {
            (
                this.copies.values().filter(|c| c.root.is_some()).count(),
                this.copies
                    .values()
                    .filter_map(|c| c.error.clone())
                    .collect::<Vec<_>>(),
            )
        });
        assert_eq!(loaded, 20, "{errors:?}");
        assert!(errors.is_empty());
        assert!(first.upgrade().is_none(), "the exhausted VM is freed");
    }

    #[test]
    fn opaque_load_errors_near_the_limit_retry_only_once() {
        in_own_process("opaque_load_recovery");
    }

    #[gpui::test]
    fn opaque_load_recovery(cx: &mut gpui::TestAppContext) {
        if running_alone("opaque_load_recovery") {
            check_failed_load_recovery(cx, 200, "throw null;");
        }
    }

    #[test]
    fn oversized_mods_do_not_restart_forever() {
        in_own_process("oversized_load_recovery");
    }

    #[gpui::test]
    fn oversized_load_recovery(cx: &mut gpui::TestAppContext) {
        if running_alone("oversized_load_recovery") {
            check_failed_load_recovery(cx, 1, &heavy_mod(0, 300));
        }
    }

    fn check_failed_load_recovery(cx: &mut gpui::TestAppContext, held_mib: usize, broken: &str) {
        let (view, cx) = open_heavy(cx, held_mib);
        let folder = crate::mods::app_dir().join("mods/broken");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("mod.json"), r#"{"place":"none"}"#).unwrap();
        std::fs::write(folder.join("main.js"), broken).unwrap();
        crate::mods::set_shared("vmdead.sentinel".into(), serde_json::json!(42));
        cx.update(|window, cx| view.update(cx, |this, cx| {
            this.open_overlay("dialog".into(), serde_json::json!({"kind":"dialog","mod":"heavy"}), window, cx);
        }));
        let first = view.update(cx, |this, _| {
            std::rc::Rc::downgrade(this.runtime.as_ref().unwrap())
        });
        cx.update(|window, cx| {
            view.update(cx, |this, cx| {
                this.sync_folders(window, cx);
                this.chrome = serde_json::from_value(serde_json::json!({
                    "id":"root","type":"row","children":[
                        {"id":"heavy","type":"block","content":{"mod":"heavy"}},
                        {"id":"broken","type":"block","content":{"mod":"broken"}}
                    ]
                }))
                .unwrap();
                this.sync(window, cx);
            })
        });
        cx.run_until_parked();
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        assert!(first.upgrade().is_none(), "a failed load gets one fresh VM");
        let fresh = view.update(cx, |this, _| {
            std::rc::Rc::downgrade(this.runtime.as_ref().unwrap())
        });
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        view.update(cx, |this, _| {
            assert!(std::rc::Rc::ptr_eq(
                &fresh.upgrade().unwrap(),
                this.runtime.as_ref().unwrap()
            ));
            assert_eq!(this.copies.values().filter(|c| c.root.is_some()).count(), 2);
            assert_eq!(crate::mods::shared("vmdead.sentinel"), Some(serde_json::json!(42)));
            let errors: Vec<_> = this
                .copies
                .values()
                .filter_map(|c| c.error.as_ref())
                .collect();
            assert_eq!(errors.len(), 1, "the broken mod's error is still reported");
            if held_mib == 200 {
                assert!(this.runtime.as_ref().unwrap().memory_share() >= super::RESTART_MODS_AT);
                assert!(!errors[0].contains("out of memory"), "{errors:?}");
            }
        });
        cx.update(|window, cx| {
            let this = view.read(cx);
            let dialog = this.overlays.iter().find(|o| o.id == "dialog").unwrap();
            assert!(this.copies[&dialog.slot].root.is_some());
            assert!(dialog.focus.contains_focused(window, cx), "the reopened dialog keeps the keyboard");
        });
    }


    #[test]
    fn persistent_load_errors_do_not_restart_on_layout_switches() {
        in_own_process("load_recovery_survives_switches");
    }

    #[gpui::test]
    fn load_recovery_survives_switches(cx: &mut gpui::TestAppContext) {
        if !running_alone("load_recovery_survives_switches") {
            return;
        }
        let home = crate::mods::app_dir();
        std::fs::create_dir_all(home.join("layouts")).unwrap();
        for name in ["broken", "other"] {
            let folder = home.join("mods").join(name);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("mod.json"), r#"{"place":"none"}"#).unwrap();
            std::fs::write(folder.join("main.js"), "export default class {").unwrap();
        }
        for (layout, bad) in [("bad-a", "broken"), ("bad-b", "broken"), ("bad-other", "other")] {
            std::fs::write(home.join(format!("layouts/{layout}.json")), serde_json::json!({
                "id":"root","type":"row","children":[
                    {"id":"heavy","type":"block","content":{"mod":"heavy"}},
                    {"id":"bad","type":"block","content":{"mod":bad}}
                ]
            }).to_string()).unwrap();
        }
        let (view, cx) = open_heavy(cx, 200);
        let first = view.update(cx, |this, _| std::rc::Rc::downgrade(this.runtime.as_ref().unwrap()));
        cx.update(|window, cx| view.update(cx, |this, cx| {
            this.sync_folders(window, cx);
            this.switch_layout(Some("bad-a"), window, cx);
        }));
        cx.run_until_parked();
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        assert!(first.upgrade().is_none(), "the initial load failure gets one recovery");
        let fresh = view.update(cx, |this, _| std::rc::Rc::downgrade(this.runtime.as_ref().unwrap()));
        for (layout, expired) in [("bad-b", false), ("bad-a", false), ("bad-other", false), ("bad-b", true)] {
            if expired {
                view.update(cx, |this, _| this.load_recovery = Some(std::time::Instant::now() - std::time::Duration::from_secs(61)));
            }
            cx.update(|window, cx| view.update(cx, |this, cx| {
                this.switch_layout(Some(layout), window, cx);
                this.files_changed(vec!["layouts/.state/bad-a.json".into()], window, cx);
                this.files_changed(vec!["mods/heavy".into()], window, cx);
            }));
            cx.run_until_parked();
            if layout == "bad-b" && !expired {
                let (_, errors) = edit_heavy(&view, 1, 200, cx);
                assert_eq!(errors.len(), 2, "a real edit reports its reload error during the cooldown");
                let (_, errors) = edit_heavy(&view, 2, 1, cx);
                assert_eq!(errors.len(), 1, "a small repair can still reload in the current VM");
            }
            view.update(cx, |this, _| {
                assert!(std::rc::Rc::ptr_eq(&fresh.upgrade().unwrap(), this.runtime.as_ref().unwrap()), "{layout} must not restart the VM");
                assert!(this.runtime.as_ref().unwrap().memory_share() >= super::RESTART_MODS_AT);
                assert_eq!(this.copies.values().filter(|c| c.error.is_some()).count(), 1);
                assert_eq!(this.copies.values().filter(|c| c.root.is_some()).count(), 1);
            });
        }
        std::fs::write(home.join("mods/broken/main.js"), "throw null;").unwrap();
        cx.update(|window, cx| view.update(cx, |this, cx| {
            this.files_changed(vec!["mods/broken/main.js".into()], window, cx);
        }));
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        let state = view.update(cx, |this, _| format!("cooldown={:?} suppressed={:?} copies={:?}", this.load_recovery.map(|t| t.elapsed()), this.failed_loads, this.copies.values().map(|c| (&c.inst.name, &c.error)).collect::<Vec<_>>()));
        assert!(fresh.upgrade().is_none(), "its own source edit allows another recovery: {state}");
        view.update(cx, |this, _| {
            assert_eq!(this.copies.values().filter(|c| c.error.is_some()).count(), 1);
        });
    }

    #[test]
    fn layout_terminals_survive_stream_loss_until_confirmed_closed() {
        in_own_process("layout_terminal_stream_loss");
    }

    #[gpui::test]
    fn layout_terminal_stream_loss(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("layout_terminal_stream_loss") {
            return;
        }
        let (view, cx) = open_window(cx, "neighbor", "", json!({
            "id": "root", "type": "row", "children": [
                { "id": "left", "type": "column", "children": [
                    { "id": "terminal", "type": "block", "content": { "terminal": "t1" } }
                ] },
                { "id": "right", "type": "column", "children": [
                    { "id": "neighbor", "type": "block", "content": { "mod": "neighbor" } }
                ] }
            ]
        }));
        cx.update(|window, cx| view.update(cx, |this, cx| {
            let saved = |this: &mut super::AppWindow| -> crate::layout::Node {
                this.save();
                serde_json::from_str(&std::fs::read_to_string(super::layout_path("test")).unwrap()).unwrap()
            };
            let original = this.chrome.clone();
            this.terms["terminal"].view.update(cx, |v, _| v.ended = Some("holder_lost".into()));
            this.terminal_ended("terminal", window, cx);
            assert_eq!(this.chrome, original);
            assert_eq!(saved(this), original, "saving while disconnected must retain the slot");
            this.live.insert("t1".into(), json!({"pid": 1, "state": "live"}));
            this.tend_terminals(false, window, cx);
            assert!(this.terms["terminal"].view.read(cx).ended.is_none(), "recovered terminal reattaches");

            let mut closed = original.clone();
            closed.remove_slot("terminal");
            this.terms["terminal"].view.update(cx, |v, _| v.ended = Some("exited".into()));
            this.terminal_ended("terminal", window, cx);
            assert_eq!(this.chrome, closed, "exit removes only the slot");
            assert_eq!(saved(this), closed);

            let error = |kind| Err(crate::hc::RpcError(json!({"data": {"kind": kind}})).into());
            for (result, stale, removed) in [
                (Ok(json!({"state": "finished", "reason": "holder_lost"})), false, true),
                (error("not_found"), false, false),
                (error("unavailable"), false, false),
                (Ok(json!({"state": "finished", "reason": "holder_lost"})), true, false),
                (Ok(json!({"state": "live", "reason": "holder_lost"})), false, false),
            ] {
                this.chrome = original.clone();
                this.sync(window, cx);
                let generation = this.core_generation;
                if stale { this.core_generation += 1; }
                this.checking_closed = true;
                this.checked_closed(generation, vec![("terminal".into(), "t1".into(), result)], window, cx);
                assert!(!this.checking_closed);
                let expected = if removed { &closed } else { &original };
                assert_eq!(&this.chrome, expected);
                assert_eq!(&saved(this), expected);
            }
        }));
    }

    #[test]
    fn a_new_vm_loads_the_open_popup_again() {
        in_own_process("new_vm_keeps_popup");
    }

    /// A popup's copy loads when it opens, not with the layout's. When the
    /// window moves its mods to a new VM, the open popup loads there too
    /// instead of staying an empty frame that holds the keyboard.
    #[gpui::test]
    fn new_vm_keeps_popup(cx: &mut gpui::TestAppContext) {
        if !running_alone("new_vm_keeps_popup") {
            return;
        }
        let (view, cx) = open_heavy(cx, 1);
        cx.update(|window, cx| view.update(cx, |this, cx| this.open_overlay("menu".into(), serde_json::json!({ "kind": "popup", "mod": "heavy" }), window, cx)));
        cx.run_until_parked();
        let popup = |view: &gpui::Entity<super::AppWindow>, cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                let this = view.read(cx);
                let o = this.overlays.iter().find(|o| o.id == "menu").expect("the popup stays open");
                let copy = this.copies.get(&o.slot);
                (copy.is_some_and(|c| c.root.is_some() && c.error.is_none()), o.focus.contains_focused(window, cx))
            })
        };
        assert_eq!(popup(&view, cx), (true, true), "the popup shows its mod and holds the keyboard");
        let note = serde_json::json!({ "kind": "toast", "notification": { "title": "Saved", "body": "", "level": "info" } });
        cx.update(|window, cx| view.update(cx, |this, cx| this.open_overlay("note".into(), note, window, cx)));
        let first = view.update(cx, |this, _| std::rc::Rc::downgrade(this.runtime.as_ref().unwrap()));
        cx.update(|window, cx| view.update(cx, |this, cx| this.restart_mods(window, cx)));
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        assert!(first.upgrade().is_none(), "the old VM is freed");
        assert_eq!(popup(&view, cx), (true, true), "on the new VM the popup shows its mod again");
        assert!(view.update(cx, |this, _| !this.overlays.iter().any(|o| o.id == "note")), "a toast is not shown twice");
        assert_eq!(view.update(cx, |this, _| this.copies.values().filter(|c| c.root.is_some()).count()), 2, "the block and the popup");
    }

    #[test]
    fn showing_the_tab_again_gives_its_pane_the_keyboard() {
        in_own_process("tab_shown_again");
    }

    /// A home page in a stack beside the tab: a space made while it shows
    /// gets the keyboard once `layout.select` shows the tab again.
    #[gpui::test]
    fn tab_shown_again(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("tab_shown_again") {
            return;
        }
        let (view, cx) = open_window(cx, "home", "", json!({
            "id": "center", "type": "stack", "active": "main", "children": [
                { "id": "main", "type": "block", "content": { "tab": "active" } },
                { "id": "home", "type": "block", "content": { "mod": "home" } }
            ]
        }));
        let select = |child: &str, cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| view.update(cx, |this, cx| this.run("layout.select", json!({ "stack": "center", "child": child }), window, cx)));
            cx.run_until_parked();
        };
        select("home", cx);
        cx.update(|window, cx| view.update(cx, |this, cx| {
            this.model.add_space("sp1", "", "/tmp", crate::model::Tab::new("tb1", "", "p1", "t1"));
            this.sync(window, cx);
        }));
        cx.run_until_parked();
        let pane_focused = |cx: &mut gpui::VisualTestContext| cx.update(|window, cx| view.read(cx).pane_focus("p1", cx).unwrap().is_focused(window));
        assert!(!pane_focused(cx), "hidden, the pane can't hold the keyboard");
        select("main", cx);
        assert!(pane_focused(cx));
    }

    #[test]
    fn close_keys_dismiss_the_top_dialog_without_closing_the_tab() {
        in_own_process("close_keys_dismiss_dialog");
    }

    #[gpui::test]
    fn close_keys_dismiss_dialog(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("close_keys_dismiss_dialog") {
            return;
        }
        let (view, cx) = open_window(cx, "unused", "", json!({
            "id": "root", "type": "block", "content": { "tab": "active" }
        }));
        cx.update(|window, cx| view.update(cx, |this, cx| {
            this.model.add_space("sp1", "", "/tmp", crate::model::Tab::new("tb1", "", "p1", "t1"));
            this.sync(window, cx);
            this.open_overlay("bottom".into(), json!({"kind": "dialog"}), window, cx);
            for kind in ["dialog", "popup"] {
                for name in ["tab.close", "pane.close"] {
                    let mut bindings = vec![None, Some(json!({}))];
                    if name == "tab.close" {
                        bindings.extend([Some(json!({"confirmed": true})), Some(json!({"id": null})), Some(json!({"id": null, "confirmed": true}))]);
                    } else {
                        bindings.push(Some(json!({"pane": null})));
                    }
                    for args in bindings {
                        this.open_overlay("top".into(), json!({"kind": kind}), window, cx);
                        if let Some(args) = args {
                            this.run_key(name, args, window, cx);
                        } else {
                            this.run_action(name, window, cx);
                        }
                        assert!(!this.overlays.iter().any(|o| o.id == "top"));
                        assert!(this.overlays.iter().any(|o| o.id == "bottom"));
                        assert_eq!(this.model.focused_pane().as_deref(), Some("p1"));
                        assert!(this.model.tab("tb1").is_some());
                    }
                }
            }
        }));
    }

    #[test]
    fn a_second_rename_press_closes_the_prompt() {
        in_own_process("rename_prompt_opens_once");
    }

    #[gpui::test]
    fn rename_prompt_opens_once(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("rename_prompt_opens_once") {
            return;
        }
        let (view, cx) = open_window(cx, "unused", "", json!({
            "id": "root", "type": "block", "content": { "tab": "active" }
        }));
        cx.update(|window, cx| view.update(cx, |this, cx| {
            this.model.add_space("sp1", "", "/tmp", crate::model::Tab::new("tb1", "", "p1", "t1"));
            this.sync(window, cx);
            let prompts = |this: &super::AppWindow| this.overlays.iter().filter(|o| o.id == "rename").count();
            this.run("tab.rename", json!({}), window, cx);
            assert_eq!(prompts(this), 1);
            this.run("tab.rename", json!({}), window, cx);
            assert_eq!(prompts(this), 0, "a second press closes the prompt");
            this.run("tab.rename", json!({}), window, cx);
            assert_eq!(prompts(this), 1, "and a third opens it again");
        }));
    }

    #[test]
    fn pane_ring_follows_keyboard_focus() {
        in_own_process("pane_ring_focus");
    }

    #[gpui::test]
    fn pane_ring_focus(cx: &mut gpui::TestAppContext) {
        if !running_alone("pane_ring_focus") {
            return;
        }
        let (view, cx) = open_window(cx, "unused", "", serde_json::json!({
            "id": "pane", "type": "block", "content": { "terminal": "t1" }
        }));
        cx.update(|window, cx| view.update(cx, |this, cx| {
            this.focused_pane = Some("pane".into());
            let pane = this.pane_focus("pane", cx).unwrap();
            pane.focus(window, cx);
            assert!(this.focused_path(window, cx).contains("pane"));
            let input = cx.focus_handle();
            input.focus(window, cx);
            assert!(this.focused_path(window, cx).is_empty());
            assert_eq!(this.focused_pane.as_deref(), Some("pane"));
            pane.focus(window, cx);
            assert!(this.focused_path(window, cx).contains("pane"));
        }));
        cx.deactivate_window();
        cx.update(|window, cx| view.update(cx, |this, cx| {
            assert!(this.focused_path(window, cx).contains("pane"));
        }));
    }

    #[test]
    fn new_tabs_without_focus_leave_the_shown_tab() {
        in_own_process("tab_new_without_focus");
    }

    #[test]
    fn a_split_starts_a_program_in_your_shell() {
        in_own_process("split_in_shell");
    }

    /// `terminal.open --argv` runs the program in your shell whatever the place, so
    /// it reads your rc files (and so your PATH). A split's shell becomes the program.
    #[gpui::test]
    fn split_in_shell(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("split_in_shell") {
            return;
        }
        let (core, runs) = crate::hc::answering();
        let (view, cx) = open_window_on(cx, core, "board", "", json!({
            "id": "main", "type": "block", "content": { "tab": "active" }
        }));
        cx.update(|window, cx| view.update(cx, |this, cx| {
            this.documents.entry(String::new()).or_default().available = true;
            this.model.add_space("s1", "one", "", crate::model::Tab::new("a", "a", "pa", "t1"));
            this.model.select_tab("a");
            this.sync(window, cx);
        }));
        let mut started = Vec::new();
        for place in ["tab", "right", "down"] {
            let r = cx.update(|window, cx| view.update(cx, |this, cx| {
                this.run_as("terminal.open", json!({ "argv": ["node", "tool.js"], "place": place }), false, window, cx)
            }));
            assert!(r.get("terminal").is_some(), "{place}: {r}");
            started.push(runs.try_iter().last().unwrap_or_else(|| panic!("{place}: no terminal.run")));
        }
        let cmd = vec!["node".to_owned(), "tool.js".to_owned()];
        let split = json!(if cfg!(windows) { cmd } else { super::unix_agent_argv(super::shell(), cmd, cfg!(target_os = "macos"), false) });
        assert_ne!(started[0], json!(["node", "tool.js"]), "a tab runs it in a shell");
        assert_eq!(started[1], split, "right");
        assert_eq!(started[2], split, "down");
    }

    /// `tab.new {mod, focus: false}` adds the tab at the end of its space,
    /// in the shown space or another, and keeps what is shown; without focus the
    /// new tab is shown.
    #[gpui::test]
    fn tab_new_without_focus(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("tab_new_without_focus") {
            return;
        }
        let (view, cx) = open_window(cx, "board", "", json!({
            "id": "main", "type": "block", "content": { "tab": "active" }
        }));
        let shown = |cx: &mut gpui::VisualTestContext| cx.update(|_, cx| view.read_with(cx, |this, _| {
            (this.model.active.clone().unwrap_or_default(), this.model.active_tab().map(|c| c.id.clone()).unwrap_or_default())
        }));
        let new_tab = |args: serde_json::Value, cx: &mut gpui::VisualTestContext| cx.update(|window, cx| view.update(cx, |this, cx| {
            // No core here: count its layout as read, as a connected core's would be.
            this.documents.entry(String::new()).or_default().available = true;
            this.run_as("tab.new", args, false, window, cx)
        }));
        cx.update(|window, cx| view.update(cx, |this, cx| {
            this.model.add_space("s1", "one", "", crate::model::Tab::new("a", "a", "pa", "t1"));
            this.model.add_space("s2", "two", "", crate::model::Tab::new("b", "b", "pb", "t2"));
            this.model.select_tab("a");
            this.sync(window, cx);
        }));
        assert_eq!(shown(cx), ("s1".into(), "a".into()));

        let here = new_tab(json!({ "space": "s1", "mod": "board", "focus": false }), cx);
        assert_eq!(here["space"], "s1", "{here}");
        assert_eq!(shown(cx), ("s1".into(), "a".into()));
        let s1_tabs = cx.update(|_, cx| view.read_with(cx, |this, _| this.model.space("s1").unwrap().tabs.len()));
        assert_eq!(s1_tabs, 2);
        let refused = new_tab(json!({ "space": "s1", "mod": "board", "focus": "false" }), cx);
        assert!(refused["error"].as_str().is_some_and(|e| e.contains("true or false")), "{refused}");
        let there = new_tab(json!({ "space": "s2", "mod": "board", "focus": false }), cx);
        assert_eq!(there["space"], "s2", "{there}");
        assert_eq!(shown(cx), ("s1".into(), "a".into()));
        let s2_selected = cx.update(|_, cx| view.read_with(cx, |this, _| this.model.space("s2").unwrap().active.clone()));
        assert_eq!(s2_selected.as_deref(), Some("b"), "s2 still opens on its own tab");

        let shown_now = new_tab(json!({ "space": "s2", "mod": "board" }), cx);
        let tab = shown_now["tab"].as_str().unwrap().to_owned();
        assert_eq!(shown(cx), ("s2".into(), tab));
    }

    #[test]
    fn find_focus_survives_tab_and_space_switches() {
        in_own_process("find_focus_switches");
    }

    #[gpui::test]
    fn find_focus_switches(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("find_focus_switches") {
            return;
        }
        cx.update(gpui_base::init);
        let (view, cx) = open_window(cx, "unused", "", json!({
            "id": "main", "type": "block", "content": { "tab": "active" }
        }));
        cx.update(|window, cx| view.update(cx, |this, cx| {
            this.model.add_space("s1", "one", "", crate::model::Tab::new("a", "a", "pa", "t1"));
            this.model.add_tab("s1", crate::model::Tab::new("b", "b", "pb", "t2"));
            this.model.add_space("s2", "two", "", crate::model::Tab::new("c", "c", "pc", "t3"));
            this.model.select_tab("a");
            this.sync(window, cx);
            this.dispatch("terminal.search", &json!({}), true, window, cx);
        }));
        cx.run_until_parked();
        cx.simulate_keystrokes("n e e d l e");
        for (action, args) in [
            ("tab.select", json!({"id": "b"})),
            ("tab.select", json!({"id": "a"})),
            ("space.select", json!({"id": "s2"})),
            ("space.select", json!({"id": "s1"})),
        ] {
            cx.update(|window, cx| view.update(cx, |this, cx| {
                this.dispatch(action, &args, true, window, cx);
            }));
            cx.run_until_parked();
            if args["id"] == "a" || args["id"] == "s1" {
                cx.update(|window, cx| view.update(cx, |this, cx| {
                    let term = this.pane_term("pa").unwrap();
                    assert!(!term.focus.is_focused(window), "returning must not send a find query to the program");
                    assert!(this.pane_focus("pa", cx).unwrap().is_focused(window));
                    assert!(this.focused_path(window, cx).contains("pa"), "Find focus must keep the pane ring");
                }));
                cx.simulate_keystrokes("x");
            }
        }
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.update(|window, cx| view.update(cx, |this, _| {
            assert!(this.pane_term("pa").unwrap().focus.is_focused(window));
        }));
    }

    #[test]
    fn the_toast_clock_runs_only_while_a_toast_is_up() {
        in_own_process("toast_clock");
    }

    /// No mod root wakes for toasts it doesn't have: the clock starts with a
    /// toast and stops once the last one has left.
    #[gpui::test]
    fn toast_clock(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("toast_clock") {
            return;
        }
        let (view, cx) = open_window(cx, "probe", "import { View, div } from \"gpui-kit\";\nexport default class P extends View { render() { return div(); } }", json!({ "id": "probe", "type": "block", "content": { "mod": "probe" } }));
        cx.run_until_parked();
        let roots: Vec<_> = view.update(cx, |this, _| this.copies.values().filter_map(|c| c.root.clone()).collect());
        assert!(!roots.is_empty(), "no mod loaded");
        let running = |cx: &mut gpui::VisualTestContext| roots.iter().filter(|r| r.read_with(cx, |r, _| r.toast_clock_running())).count();
        assert_eq!(running(cx), 0, "no toasts, no clock");
        let root = roots[0].clone();
        cx.update(|window, cx| root.update(cx, |r, cx| r.push_toast(gpui_shell::ToastRequest::new("Saved"), window, cx)));
        assert_eq!(running(cx), 1, "a toast starts its root's clock, and only its");
        cx.executor().advance_clock(gpui_shell::ToastRequest::DEFAULT_TIMEOUT / 2);
        cx.run_until_parked();
        assert_eq!((running(cx), root.read_with(cx, |r, _| r.toast_count())), (1, 1), "still up, still ticking");
        cx.executor().advance_clock(gpui_shell::ToastRequest::DEFAULT_TIMEOUT * 2);
        cx.run_until_parked();
        assert_eq!(root.read_with(cx, |r, _| r.toast_count()), 0, "the toast timed out and left");
        assert_eq!(running(cx), 0, "and the clock stopped with it");
    }

    /// Mods see window().active follow the focus, losing it too: a probe mod
    /// notes every value it renders with while the window is focused and
    /// blurred, and the poll that checks the focus runs in between.
    #[test]
    fn mods_see_the_window_lose_and_regain_focus() {
        in_own_process("focus_window");
    }

    #[gpui::test]
    fn focus_window(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("focus_window") {
            return;
        }
        let probe = r#"import { View, div } from "gpui-kit";
import { window, shared, setShared } from "herdr";

export default class Probe extends View {
  render() {
    const active = window().active;
    if (active !== this.last) {
      this.last = active;
      setShared("probe.seen", [...(shared("probe.seen") ?? []), active]);
    }
    return div();
  }
}
"#;
        let layout = json!({ "id": "root", "type": "row", "children": [
            { "id": "probe", "type": "block", "content": { "mod": "active-probe" } }
        ] });
        let (_view, cx) = open_window(cx, "active-probe", probe, layout);
        // What the probe saw last, after the focus poll (every 400 ms) had its turn.
        let seen = |cx: &mut gpui::VisualTestContext| {
            cx.executor().advance_clock(std::time::Duration::from_secs(1));
            cx.run_until_parked();
            crate::mods::shared("probe.seen").and_then(|v| v.as_array()?.last().cloned())
        };
        for _ in 0..2 {
            cx.update(|window, _| window.activate_window());
            assert_eq!(seen(cx), Some(json!(true)), "focused: {:?}", crate::mods::shared("probe.seen"));
            cx.deactivate_window();
            assert_eq!(seen(cx), Some(json!(false)), "blurred: {:?}", crate::mods::shared("probe.seen"));
        }
    }

    #[test]
    fn focused_selection_follows_the_terminal_receiving_commands() {
        in_own_process("selection_view_focus");
    }

    #[gpui::test]
    fn selection_view_focus(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("selection_view_focus") {
            return;
        }
        let probe = r#"import { View, div } from "gpui-kit";
import { focused, setShared } from "herdr";
export default class Probe extends View {
  render() {
    const value = focused();
    const text = JSON.stringify(value);
    if (text !== this.last) {
      this.last = text;
      setShared("probe.focused", value);
    }
    return div();
  }
}
"#;
        let (view, cx) = open_window(cx, "selection-probe", probe, json!({
            "id": "root", "type": "row", "children": [
                {"id": "main", "type": "block", "content": {"tab": "active"}},
                {"id": "agent", "type": "block", "content": {"agent": "focused"}},
                {"id": "probe", "type": "block", "content": {"mod": "selection-probe"}}
            ],
            "floating": [{"id": "float", "content": {"terminal": "t2"}}]
        }));
        cx.update(|window, cx| view.update(cx, |this, cx| {
            this.model.add_space("s1", "one", "", crate::model::Tab::new("a", "a", "pa", "t1"));
            for address in ["t1", "t2", "t3"] {
                this.live.insert(address.into(), json!({"address": address}));
            }
            this.agent = Some("t3".into());
            this.sync(window, cx);
        }));
        cx.run_until_parked();
        for (slot, col) in [("pa", 1), ("float:float", 2), ("agent", 3)] {
            cx.update(|window, cx| view.update(cx, |this, cx| {
                this.terms[slot].focus.focus(window, cx);
                let result = this.run_as("terminal.select", json!({"at": {"row": 0, "col": col}}), false, window, cx);
                assert!(result.get("error").is_none(), "{result}");
                assert_eq!(this.focused_terminal(window, cx).unwrap().address, this.terms[slot].address);
            }));
            cx.run_until_parked();
            let focused = crate::mods::shared("probe.focused").unwrap();
            assert_eq!(focused["terminal"], "t1", "logical identity stays with the model pane");
            assert_eq!(focused["selection"]["end"], json!({"row": 0, "col": col}), "{slot}: {focused}");
        }
        // Moving the keyboard alone must publish the destination's existing selection.
        for (slot, col) in [("float:float", 2), ("pa", 1), ("agent", 3)] {
            cx.update(|window, cx| view.update(cx, |this, cx| this.terms[slot].focus.focus(window, cx)));
            cx.run_until_parked();
            let focused = crate::mods::shared("probe.focused").unwrap();
            assert_eq!(focused["selection"]["end"], json!({"row": 0, "col": col}), "{slot}: {focused}");
        }
    }

    #[test]
    fn mods_set_the_window_title() {
        in_own_process("window_title");
    }

    #[gpui::test]
    fn window_title(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("window_title") {
            return;
        }
        let probe = r#"import { View, div } from "gpui-kit";
import { setWindowTitle, shared } from "herdr";

export default class Probe extends View {
  render() {
    const title = shared("probe.title");
    if (title !== undefined) setWindowTitle(title);
    return div();
  }
}
"#;
        let layout = json!({ "id": "root", "type": "row", "children": [
            { "id": "probe", "type": "block", "content": { "mod": "title-probe" } }
        ] });
        let (_view, cx) = open_window(cx, "title-probe", probe, layout);
        let title = |value, cx: &mut gpui::VisualTestContext| {
            crate::mods::set_shared("probe.title".into(), value);
            cx.run_until_parked();
            cx.window_title()
        };
        assert_eq!(title(json!("acorn"), cx).as_deref(), Some("acorn"));
        assert_eq!(title(json!(null), cx).as_deref(), Some("Herdr2"));
    }

    include!("window/pane_size_tests.rs");

    /// What a `{"command":"tab.move","args":{"by":-1}}` key runs.
    #[test]
    fn tab_move_by_steps_the_active_tab() {
        in_own_process("tab_move_by");
    }

    #[gpui::test]
    fn tab_move_by(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("tab_move_by") { return; }
        let (view, cx) = open_window(cx, "unused", "", json!({
            "id": "context", "type": "block", "content": { "tab": "active" }
        }));
        let order = |this: &super::AppWindow|this.model.active_space().unwrap().tabs.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        view.update_in(cx, |this, window, cx| {
            this.model.add_space("space", "test", "/", super::Tab::new("t1", "", "a", "term-a"));
            this.model.add_tab_unselected("space", super::Tab::new("t2", "", "b", "term-b"));
            this.model.add_tab_unselected("space", super::Tab::new("t3", "", "c", "term-c"));
            this.documents.entry(String::new()).or_default().available = true;
            this.sync(window, cx);
            for (by, expected) in [(1, ["t2", "t1", "t3"]), (1, ["t2", "t3", "t1"]), (1, ["t1", "t2", "t3"]), (-1, ["t2", "t3", "t1"])] {
                let out = this.run_as("tab.move", json!({ "by": by }), false, window, cx);
                assert!(out.get("error").is_none(), "{out}");
                assert_eq!(order(this), expected, "by {by}");
                assert_eq!(this.model.active_tab().unwrap().id, "t1");
            }
            let out = this.run_as("tab.move", json!({ "index": 1 }), false, window, cx);
            assert!(out.get("error").is_none(), "{out}");
            assert_eq!(order(this), ["t1", "t2", "t3"]);
            for args in [json!({}), json!({ "index": 1, "by": 1 })] {
                let out = this.run_as("tab.move", args.clone(), false, window, cx);
                assert!(out.get("error").is_some(), "{args}: {out}");
                assert_eq!(order(this), ["t1", "t2", "t3"]);
            }
        });
    }

    #[test]
    fn acp_pane_split_and_close_move_acp_agents_to_background() {
        in_own_process("acp_tab_and_pane_close_park");
    }

    #[gpui::test]
    fn acp_tab_and_pane_close_park(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("acp_tab_and_pane_close_park") { return; }
        let (view, cx) = open_window(cx, "unused", "", json!({
            "id": "context", "type": "block", "content": { "tab": "active" }
        }));
        view.update_in(cx, |this, window, cx| {
            let mut tb2 = super::Tab::new("tb2", "work", "pb", "term-b");
            tb2.root.split("pb", super::Axis::Row, super::AppWindow::acp_pane_node("acp-p1", "a1"), "row".into());
            this.model.add_space("space", "test", "/", super::Tab::new("tb1", "", "pa", "term-a"));
            this.model.add_tab_unselected("space", tb2);
            this.model.add_tab_unselected("space", super::Tab::with_pane("tb3", "Codex", super::AppWindow::acp_pane_node("acp-p2", "a2")));
            for (addr, tab) in [("a1", "tb2"), ("a2", "tb3")] {
                this.live.insert(addr.into(), json!({
                    "address": addr,
                    "acp": true,
                    "state": "live",
                    "keep": true,
                    "labels": { "herdr.space": "space", "herdr.tab": tab }
                }));
            }
            for addr in ["term-a", "term-b"] {
                this.live.insert(addr.into(), json!({ "address": addr, "state": "live" }));
            }
            this.documents.entry(String::new()).or_default().available = true;
            this.model.select_tab("tb2");
            this.model.focus_pane("pb");
            this.sync(window, cx);

            // Sub-agents spawned by a main agent (or Drove workers) do not auto-create tabs on reconcile.
            this.live.insert("a3".into(), json!({
                "address": "a3",
                "acp": true,
                "parent": "term-a",
                "state": "live",
                "labels": {}
            }));
            this.live.insert("a5".into(), json!({
                "address": "a5",
                "acp": true,
                "state": "live",
                "labels": { "drove.worker": "wrk_1" }
            }));
            this.reconcile(window, cx);
            assert!(this.model.acp_pane_of("a3").is_none());
            assert!(this.model.acp_pane_of("a5").is_none());

            // Clicking or focusing an unmounted ACP sub-agent opens ACP Peek instead of creating a tab.
            let out = this.run_as("terminal.peek", json!({ "terminal": "a3" }), false, window, cx);
            assert!(out.get("error").is_none(), "{out}");
            assert!(this.model.acp_pane_of("a3").is_none());
            assert!(!this.changing_terminals.contains("a3"));
            assert_eq!(this.peeked_agent.as_deref(), Some("a3"));
            assert!(this.overlays.iter().any(|o| o.id == "terminal-peek"));
            this.close_overlay("terminal-peek", window, cx);
            assert!(this.peeked_agent.is_none());

            let out = this.run_as("agent.focus", json!({ "terminal": "a3" }), false, window, cx);
            assert!(out.get("error").is_none(), "{out}");
            assert!(this.model.acp_pane_of("a3").is_none());
            assert!(!this.changing_terminals.contains("a3"));
            assert_eq!(this.peeked_agent.as_deref(), Some("a3"));
            this.close_overlay("terminal-peek", window, cx);
            this.live.remove("a3");
            this.live.remove("a5");
            this.model.select_tab("tb2");
            this.model.focus_pane("pb");

            // Moving already-mounted ACP agent a2 beside focused pane pb splits tb2 horizontally.
            assert!(this.keep_terminal("a2", "right", window, cx).is_ok());
            assert_eq!(this.model.tab_of("acp-p2").map(|t| t.id.as_str()), Some("tb2"));

            // Closing a split ACP pane parks a1 to background without killing it.
            let out = this.run_as("pane.close", json!({ "pane": "acp-p1" }), false, window, cx);
            assert!(out.get("error").is_none(), "{out}");
            assert!(this.model.acp_pane_of("a1").is_none());
            assert!(!this.closing_terminals.contains_key("a1"));
            assert_eq!(this.live["a1"]["keep"], json!(false));
            assert_eq!(this.live["a1"]["labels"]["herdr.space"], json!(""));

            // Closing a tab containing ACP agent a2 and shell term-b parks a2 without killing a2.
            let out = this.run_as("tab.close", json!({ "id": "tb2" }), false, window, cx);
            assert!(out.get("error").is_none(), "{out}");
            assert!(this.model.acp_pane_of("a2").is_none());
            assert!(!this.closing_terminals.contains_key("a2"));
            assert_eq!(this.live["a2"]["keep"], json!(false));
            assert_eq!(this.live["a2"]["labels"]["herdr.tab"], json!(""));

            // Externally finished ACP tab stays mounted as read-only "ended" until manually closed.
            let mut tb4 = super::Tab::with_pane("tb4", "codex-arch-svg", super::AppWindow::acp_pane_node("acp-p4", "a4"));
            tb4.focus("acp-p4");
            this.model.add_tab("space", tb4);
            this.checked_closed(
                this.core_generation,
                vec![(
                    "acp-p4".into(),
                    "a4".into(),
                    Ok(json!({
                        "address": "a4",
                        "agent": "codex",
                        "name": "codex-arch-svg",
                        "state": "finished",
                        "reason": "killed"
                    })),
                )],
                window,
                cx,
            );
            assert_eq!(this.model.acp_pane_of("a4").as_deref(), Some("acp-p4"));
            assert_eq!(this.ended_acp["a4"]["status"], json!("ended"));
            let out = this.run_as("tab.close", json!({ "id": "tb4" }), false, window, cx);
            assert!(out.get("error").is_none(), "{out}");
            assert!(this.model.acp_pane_of("a4").is_none());
            assert!(!this.ended_acp.contains_key("a4"));
        });
    }

    #[test]
    fn directional_navigation_moves_zoom_and_keyboard_focus() {
        in_own_process("zoomed_navigation");
    }

    #[gpui::test]
    fn zoomed_navigation(cx: &mut gpui::TestAppContext) {
        use crate::model::pane;
        use serde_json::json;
        if !running_alone("zoomed_navigation") { return; }
        let (view, cx) = open_window(cx, "unused", "", json!({
            "id": "context", "type": "block", "content": { "tab": "active" }
        }));
        let mut tab = super::Tab::new("tab", "", "a", "term-a");
        tab.root.split("a", super::Axis::Row, pane("b", "term-b"), "row".into());
        tab.root.split("b", super::Axis::Column, pane("c", "term-c"), "column".into());
        tab.focus("c");
        tab.zoomed = Some("c".into());
        view.update_in(cx, |this, window, cx| {
            this.model.add_space("space", "test", "/", tab);
            this.sync(window, cx);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        view.update(cx, |this, _| {
            assert!(!this.bounds.borrow().contains_key("a"), "start without cached split bounds");
            assert!(!this.bounds.borrow().contains_key("b"));
        });
        for (direction, expected) in [("up", "b"), ("down", "c"), ("left", "a"), ("right", "c"), ("right", "c")] {
            view.update_in(cx, |this, window, cx| this.run("pane.go", json!({"direction": direction}), window, cx));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            view.update_in(cx, |this, window, _| {
                let tab = this.model.active_tab().unwrap();
                assert_eq!(tab.zoomed.as_deref(), Some(expected), "{direction}");
                assert_eq!(tab.focused.as_deref(), Some(expected));
                assert_eq!(tab.shown().id, expected);
                assert!(this.terms[expected].focus.is_focused(window));
            });
        }
        let (left, right) = if cfg!(target_os = "macos") { ("cmd-alt-left", "cmd-alt-right") } else { ("alt-h", "alt-l") };
        cx.simulate_keystrokes(left);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        view.update_in(cx, |this, window, cx| {
            assert_eq!(this.model.active_tab().unwrap().zoomed.as_deref(), Some("a"));
            assert!(this.terms["a"].focus.is_focused(window));
            this.run("pane.zoom", json!({}), window, cx);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes(right);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        view.update_in(cx, |this, window, _| {
            let tab = this.model.active_tab().unwrap();
            assert_eq!(tab.zoomed, None);
            assert_eq!(tab.focused.as_deref(), Some("c"));
            assert!(this.terms["c"].focus.is_focused(window));
        });
    }

    include!("window/drag_tests.rs");

    /// The whole path, in a real window: a layout names the toast mod, a
    /// notification opens it with its props, and it can close itself (as one
    /// that hands notifications to the system does). Unknown: the shipped toast.
    #[test]
    fn a_layout_s_toast_mod_gets_every_notification() {
        in_own_process("toast_window");
    }

    #[gpui::test]
    fn toast_window(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("toast_window") {
            return;
        }
        let forward = r#"import { View, div } from "gpui-kit";
import { props, closeOverlay, shared, setShared } from "herdr";

export default class SystemToast extends View {
  render() {
    const p = props();
    // A real one would exec("notify-send", [p.title, p.body]) here.
    setShared("toast-test.seen", [...(shared("toast-test.seen") ?? []), `${p.title}: ${p.body}`]);
    closeOverlay(p.overlay);
    return div();
  }
}
"#;
        let layout = json!({ "id": "root", "type": "row", "toast": "system-toast", "children": [] });
        let (view, cx) = open_window(cx, "system-toast", forward, layout);
        let notify = |cx: &mut gpui::VisualTestContext| {
            view.update_in(cx, |this, window, cx| {
                this.sync_folders(window, cx);
                let note = json!({ "kind": "toast", "notification": { "title": "Build finished", "body": "All green", "level": "success" } });
                let id = crate::mods::new_overlay_id();
                this.open_overlay(id.clone(), note, window, cx);
                (id.clone(), this.copies[&format!("overlay:{id}")].inst.name.clone())
            })
        };
        // Herdr's own notes ("Your edits are in …") may show up too.
        let shown = |id: &str, cx: &mut gpui::VisualTestContext| {
            cx.run_until_parked();
            view.update(cx, |this, _| this.overlays.iter().any(|o| o.id == id))
        };

        let (id, name) = notify(cx);
        assert_eq!(name, "system-toast");
        assert!(!shown(&id, cx), "the layout's mod closed the notification");
        let seen = crate::mods::shared("toast-test.seen").unwrap_or_default();
        assert!(seen.as_array().unwrap().contains(&json!("Build finished: All green")), "the mod got the notification's props: {seen}");

        view.update(cx, |this, _| this.chrome.toast = Some("no-such-mod".into()));
        let (id, name) = notify(cx);
        assert_eq!(name, "toast");
        assert!(shown(&id, cx), "an unknown mod falls back to the shipped toast, which stays");
    }

    #[test]
    fn docking_keeps_the_layouts_notification_and_menu_mods() {
        in_own_process("dock_root_choices");
    }

    #[gpui::test]
    fn dock_root_choices(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("dock_root_choices") {
            return;
        }
        let layout = json!({
            "id": "root", "type": "row", "toast": "chosen", "terminal_menu": "chosen", "children": []
        });
        let (view, cx) = open_window(cx, "chosen", "", layout);
        let check_handlers = |cx: &mut gpui::VisualTestContext| {
            view.update_in(cx, |this, window, cx| {
                let id = crate::mods::new_overlay_id();
                this.open_overlay(id.clone(), json!({
                    "kind": "toast", "notification": { "title": "After docking", "body": "Still chosen" }
                }), window, cx);
                assert_eq!(this.copies[&format!("overlay:{id}")].inst.name, "chosen");
                this.close_overlay(&id, window, cx);
                this.open_menu("tab", json!({}), None, window, cx);
                let id = this.overlays.last().unwrap().id.clone();
                assert_eq!(this.copies[&format!("overlay:{id}")].inst.name, "chosen");
                this.close_overlay(&id, window, cx);
            });
        };
        check_handlers(cx);
        let folder = crate::mods::app_dir().join("mods/new-top");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("mod.json"), r#"{"place":"top"}"#).unwrap();
        std::fs::write(folder.join("main.js"), "").unwrap();
        view.update_in(cx, |this, window, cx| {
            this.sync_folders(window, cx);
            assert_eq!(this.chrome.axis(), Some(crate::layout::Axis::Column));
            assert!(this.chrome.walk().iter().any(|n| matches!(
                n.content(), Some(crate::layout::Content::Mod { name, .. }) if name == "new-top"
            )));
            this.save();
        });
        check_handlers(cx);
        view.update_in(cx, |this, window, cx| {
            this.load_layout("test");
            this.sync(window, cx);
        });
        check_handlers(cx);
    }

    #[test]
    fn toast_is_a_root_layout_choice_that_roundtrips() {
        let mut root = super::default_layout("main");
        assert_eq!(root.toast, None);
        assert!(serde_json::to_value(&root).unwrap().get("toast").is_none());
        root.toast = Some("system-toast".into());
        let restored: super::Node = serde_json::from_value(serde_json::to_value(&root).unwrap()).unwrap();
        assert_eq!(restored.toast.as_deref(), Some("system-toast"));
    }

    #[test]
    fn terminal_menu_preserves_clicked_target_and_uses_mod_dimensions() {
        use serde_json::json;
        let target = json!({ "pane": "p2", "terminal": "t8" });
        let stock = super::terminal_menu_spec("menu", &json!({}), "pane", target.clone());
        assert_eq!(stock["width"], 220.0);
        assert_eq!(stock["height"], 260.0);
        let custom = super::terminal_menu_spec("personal-menu", &json!({"kind": "dialog", "width": 310, "height": 120}), "pane", target.clone());
        assert_eq!(custom["mod"], "personal-menu");
        assert_eq!(custom["kind"], "popup");
        assert_eq!(custom["width"], 310.0);
        assert_eq!(custom["height"], 120.0);
        assert_eq!(custom["props"], json!({"tab": "pane", "target": target}));
        let partial = super::terminal_menu_spec("personal-menu", &json!({"width": 0.4}), "pane", json!({}));
        assert_eq!(partial["width"], 0.4);
        assert_eq!(partial["height"], 260.0);
    }

    #[test]
    fn composition_focus_only_uses_visible_terminal_blocks() {
        let chrome: super::Node = serde_json::from_value(serde_json::json!({
            "id": "composition", "type": "row", "children": [
                { "id": "left", "type": "block", "content": {"terminal": "t1"} },
                { "id": "right", "type": "block", "content": {"terminal": "other/t2"} },
                { "id": "hidden", "type": "block", "hidden": true, "content": {"terminal": "t3"} },
                { "id": "tabs", "type": "block", "content": {"tab": "active"} }
            ]
        })).unwrap();
        assert_eq!(super::terminal_block(&chrome, Some("left")).and_then(|n| n.terminal()), Some("t1"));
        assert_eq!(super::terminal_block(&chrome, Some("right")).and_then(|n| n.terminal()), Some("other/t2"));
        for id in [None, Some("hidden"), Some("tabs"), Some("source-pane"), Some("overlay:term")] {
            assert!(super::terminal_block(&chrome, id).is_none());
        }
    }

    #[test]
    fn pane_swap_refuses_a_direction_that_is_not_a_string() {
        use serde_json::json;
        for direction in [json!(false), json!(1), json!(["left"]), json!({"to": "left"})] {
            for args in [json!({"direction": direction}), json!({"direction": direction, "target": "p2"})] {
                assert_eq!(super::swap_by(&args), Err("pane.swap direction must be a string (or null to omit it)".into()), "{args}");
            }
        }
        assert_eq!(super::swap_by(&json!({"direction": null, "target": "p2"})), Ok(super::SwapBy::Target("p2")));
    }

    fn swap_model() -> super::Model {
        use crate::layout::Axis;
        let mut m = super::Model::default();
        m.add_space("s1", "api", "/src", super::Tab::new("c1", "", "x1", "t1"));
        m.split("x1", Axis::Row, "x2", "t2");
        m.add_tab("s1", super::Tab::new("c2", "", "x3", "t3"));
        m.select_tab("c1");
        m.focus_pane("x1");
        m
    }

    fn swap_state(m: &super::Model) -> (Vec<String>, Vec<String>, Option<String>) {
        let order = |tab: &str| m.tab(tab).unwrap().root.panes().iter().map(|n| n.id.clone()).collect();
        (order("c1"), order("c2"), m.focused_pane())
    }

    #[test]
    fn an_explicit_pane_swaps_with_its_neighbor_in_the_shown_tab() {
        let mut m = swap_model();
        assert_eq!(super::swap_with_neighbor(&mut m, "x2", true, Some("x1".into())), Ok(()));
        assert_eq!(swap_state(&m), (vec!["x2".into(), "x1".into()], vec!["x3".into()], Some("x1".into())));
        assert_eq!(super::swap_with_neighbor(&mut m, "x2", true, None), Ok(()));
        assert_eq!(swap_state(&m).0, ["x2", "x1"]);
    }

    #[test]
    fn an_explicit_pane_swap_by_direction_refuses_panes_it_cannot_place() {
        let mut m = swap_model();
        let container = m.tab("c1").unwrap().root.id.clone();
        let before = swap_state(&m);
        let cases = [
            ("nope".to_owned(), "no pane \"nope\"".to_owned()),
            (container.clone(), format!("no pane {container:?}")),
            ("x3".to_owned(), "pane.swap direction needs a pane in the shown tab, not \"x3\"".to_owned()),
        ];
        for (pane, error) in cases {
            assert_eq!(super::swap_with_neighbor(&mut m, &pane, true, Some("x1".into())), Err(error), "{pane}");
            assert_eq!(swap_state(&m), before, "{pane}");
        }
    }

    #[test]
    fn pane_swap_takes_exactly_one_of_direction_or_target() {
        use super::SwapBy;
        use serde_json::json;
        for dir in ["left", "right", "up", "down"] {
            assert_eq!(super::swap_by(&json!({"direction": dir})), Ok(SwapBy::Direction(dir)));
        }
        assert_eq!(super::swap_by(&json!({"target": "p2", "pane": "p1"})), Ok(SwapBy::Target("p2")));
        assert_eq!(super::swap_by(&json!({"direction": "left", "target": "p2"})), Err("pane.swap takes direction or target, not both".into()));
        for neither in [json!({}), json!(null), json!({"pane": "p1"}), json!({"direction": null, "target": null})] {
            assert_eq!(super::swap_by(&neither), Err("pane.swap needs direction or target".into()), "{neither}");
        }
        assert_eq!(super::swap_by(&json!({"direction": "sideways"})), Err("pane.swap direction is left, right, up or down, not \"sideways\"".into()));
    }

    #[test]
    fn composition_commands_never_default_to_a_source_pane() {
        use serde_json::json;
        for name in ["pane.split", "pane.close", "pane.detach", "pane.zoom", "pane.go", "pane.swap", "terminal.show"] {
            assert!(super::needs_model_pane(name, &json!({})), "{name}");
            assert!(super::needs_model_pane(name, &json!({"pane": null})), "{name}");
        }
        for name in ["pane.split", "pane.close", "pane.detach", "pane.zoom", "pane.swap", "terminal.show"] {
            assert!(!super::needs_model_pane(name, &json!({"pane": "explicit-pane"})), "{name}");
        }
        assert!(super::needs_model_pane("pane.go", &json!({"pane": "explicit-pane"})));
        for name in ["terminal.open", "terminal.keep"] {
            for place in ["right", "down"] {
                assert!(super::needs_model_pane(name, &json!({"place": place})), "{name} {place}");
            }
            assert!(!super::needs_model_pane(name, &json!({})), "{name}");
            assert!(!super::needs_model_pane(name, &json!({"place": "tab"})), "{name}");
        }
        for name in ["pane.focus", "terminal.copy", "terminal.paste", "terminal.paste_image", "terminal.right_click"] {
            assert!(!super::needs_model_pane(name, &json!({})), "{name}");
        }
    }

    #[test]
    fn a_folder_is_asked_again_only_when_its_answer_could_have_changed() {
        use super::{FOLDER_FALLBACK, FolderCheck, recheck};
        use std::time::{Duration, Instant};
        let checked = |busy, repo, ago: Duration| FolderCheck { at: Instant::now() - ago, cwd: "/src".into(), busy, repo };
        let (now, later) = (Duration::ZERO, FOLDER_FALLBACK + Duration::from_secs(1));
        assert!(recheck(None, "/src", false, false), "never asked");
        assert!(!recheck(Some(&checked(false, true, now)), "/src", false, true), "nothing happened");
        assert!(recheck(Some(&checked(false, true, now)), "/elsewhere", false, false), "it moved");
        assert!(recheck(Some(&checked(true, true, now)), "/src", false, false), "a command or turn ended");
        assert!(!recheck(Some(&checked(false, true, now)), "/src", true, true), "one started");
        assert!(recheck(Some(&checked(false, true, later)), "/src", false, true), "a repo, a minute on, in front");
        assert!(!recheck(Some(&checked(false, true, later)), "/src", false, false), "not while the window is behind");
        assert!(!recheck(Some(&checked(false, false, later)), "/src", false, true), "not a repo: never on a timer");
    }

    #[test]
    fn busy_is_an_agent_working_or_a_program_in_front_of_the_shell() {
        let rec = |status: &str, shell: bool, agent: bool| {
            serde_json::json!({ "status": status, "program": { "shell": shell, "agent": agent } })
        };
        assert!(super::busy(&rec("working", false, true)), "an agent's turn");
        assert!(!super::busy(&rec("done", false, true)), "an agent started from a shell, its turn over");
        assert!(super::busy(&rec("", false, false)), "vim in front of the shell");
        assert!(!super::busy(&rec("", true, false)), "the shell's prompt");
    }

    #[test]
    fn shipped_layouts_parse_and_show_what_they_promise() {
        let main = super::default_layout("main");
        assert!(main.find("main").is_some() && main.find("sidebar").is_some());
        assert!(main.walk().iter().any(|n| matches!(n.content(), Some(super::Content::Context { .. }))));
        let terminals = super::default_layout("terminals");
        assert!(terminals.find("tabs").is_some());
        assert!(matches!(terminals.find("main").and_then(|n| n.content()), Some(super::Content::Context { .. })));
    }

    #[test]
    fn a_finished_terminal_is_not_put_back_in_its_tab() {
        let record = |state: &str| serde_json::json!({ "state": state, "labels": { "herdr.space": "sp1", "herdr.tab": "tb1" } });
        assert!(super::adoptable(&record("live"), "sp1", Some("tb1"), None));
        assert!(!super::adoptable(&record("finished"), "sp1", Some("tb1"), None));
        assert!(!super::adoptable(&record("live"), "sp2", Some("tb1"), None));
        assert!(!super::adoptable(&record("live"), "sp1", Some("tb2"), None));
        assert!(!super::adoptable(&serde_json::json!({ "state": "live", "address": "a1", "labels": { "herdr.space": "sp1" } }), "sp1", None, None));
    }

    #[test]
    fn acp_address_and_powershell_child_exit_helpers() {
        assert!(super::is_acp_addr("a1"));
        assert!(super::is_acp_addr("a1b"));
        assert!(super::is_acp_addr("wsl/a6"));
        assert!(!super::is_acp_addr("t1"));
        assert!(!super::is_acp_addr("wsl/t2"));
        assert!(!super::is_acp_addr("agent"));

        let rec: serde_json::Map<String, serde_json::Value> = serde_json::from_value(serde_json::json!({
            "pid": 100,
            "argv": ["powershell.exe", "-NoExit", "-Command", "& 'claude'"],
            "foreground": { "pid": 100, "argv": ["powershell.exe", "-NoExit", "-Command", "& 'claude'"] }
        })).unwrap();
        // Before seeing a child process, initial launch argv is preserved so launching CLI is recognized.
        assert_eq!(super::effective_running_argv(&rec, false), Some(serde_json::json!(["powershell.exe", "-NoExit", "-Command", "& 'claude'"])));
        // After child agent exits back to the shell PID, revert to shell only.
        assert_eq!(super::effective_running_argv(&rec, true), Some(serde_json::json!(["powershell.exe"])));

        assert!(super::path_within_or_eq("/home/u/Work/herdr2", "/home/u/Work/herdr2/"));
        assert!(super::path_within_or_eq("/home/u/Work/herdr2/herdr-app", "/home/u/Work/herdr2"));
        assert!(!super::path_within_or_eq("/home/u/Work/herdr20", "/home/u/Work/herdr2"));
        assert!(super::path_matches_root("/home/u/Work/herdr2/sub", "/home/u/Work/herdr2", true));
        assert!(!super::path_matches_root("/home/u/Work/herdr2", "/home/u", true));
        assert!(super::path_matches_root("/home/u", "/home/u", false));
        assert!(!super::path_matches_root("/home/u/Work/herdr2", "/home/u/Work/herdr2/sub", true));
    }

    #[test]
    fn acp_subagents_do_not_auto_mount_tabs_by_default() {
        use serde_json::json;
        let root_acp = json!({ "address": "a1", "state": "live", "parent": null, "labels": {} });
        assert!(!super::is_acp_subagent(&root_acp));
        assert!(super::should_auto_mount_acp(&root_acp));

        let cli_subagent = json!({ "address": "a2", "state": "live", "parent": "t1", "labels": {} });
        assert!(super::is_acp_subagent(&cli_subagent));
        assert!(!super::should_auto_mount_acp(&cli_subagent));

        let remote_subagent = json!({ "address": "wsl/a2", "state": "live", "parent": "wsl/t13", "labels": {} });
        assert!(super::is_acp_subagent(&remote_subagent));
        assert!(!super::should_auto_mount_acp(&remote_subagent));

        let acp_subagent = json!({ "address": "a3", "state": "live", "parent": "a1", "labels": {} });
        assert!(super::is_acp_subagent(&acp_subagent));
        assert!(!super::should_auto_mount_acp(&acp_subagent));

        let drove_worker = json!({ "address": "a4", "state": "live", "parent": null, "labels": { "drove.worker": "wrk_1" } });
        assert!(super::is_acp_subagent(&drove_worker));
        assert!(!super::should_auto_mount_acp(&drove_worker));

        let explicit_mounted = json!({ "address": "a2", "state": "live", "parent": "t1", "labels": { "herdr.space": "sp1", "herdr.parked": "" } });
        assert!(super::should_auto_mount_acp(&explicit_mounted));

        let parked_subagent = json!({ "address": "a2", "state": "live", "parent": "t1", "labels": { "herdr.space": "", "herdr.parked": "1" } });
        assert!(!super::should_auto_mount_acp(&parked_subagent));
    }

    #[test]
    fn resolve_windows_shell_argv_with_picks_configured_shells() {
        let dir = std::env::temp_dir().join(format!("herdr-win-shell-test-{}", std::process::id()));
        let pwsh_dir = dir.join("pwsh-bin");
        let sys32_dir = dir.join("System32");
        let git_bin_dir = dir.join("ProgramFiles").join("Git").join("bin");
        let _ = std::fs::create_dir_all(&pwsh_dir);
        let _ = std::fs::create_dir_all(&sys32_dir);
        let _ = std::fs::create_dir_all(&git_bin_dir);
        std::fs::write(pwsh_dir.join("pwsh.exe"), b"").unwrap();
        std::fs::write(sys32_dir.join("bash.exe"), b"").unwrap();
        let git_bash = git_bin_dir.join("bash.exe");
        std::fs::write(&git_bash, b"").unwrap();

        let path_joined = std::env::join_paths([&sys32_dir, &pwsh_dir]).unwrap();
        let pf = dir.join("ProgramFiles");
        let pf_str = pf.to_str().unwrap();

        assert_eq!(
            super::resolve_windows_shell_argv_with("auto", Some(&path_joined), Some(pf_str), None, None, Some("C:\\Windows\\System32\\cmd.exe")),
            None,
        );
        let ps = super::resolve_windows_shell_argv_with("powershell", Some(&path_joined), Some(pf_str), None, None, None).unwrap();
        assert_eq!(ps[0], "powershell.exe");
        assert_eq!(ps[1], "-NoExit");
        assert_eq!(ps[2], "-Command");

        let pwsh = super::resolve_windows_shell_argv_with("pwsh", Some(&path_joined), Some(pf_str), None, None, None).unwrap();
        assert_eq!(pwsh[0], pwsh_dir.join("pwsh.exe").to_string_lossy());
        assert_eq!(pwsh[1], "-NoExit");

        let gb = super::resolve_windows_shell_argv_with("git-bash", Some(&path_joined), Some(pf_str), None, None, None).unwrap();
        assert_eq!(gb, vec![git_bash.to_string_lossy().into_owned(), "--login".into(), "-i".into()]);

        let gb_path = super::resolve_windows_shell_argv_with("git-bash", Some(&path_joined), None, None, None, None).unwrap();
        // Should skip System32\bash.exe shim when ProgramFiles has nothing.
        assert_eq!(gb_path, vec!["bash.exe".to_owned(), "--login".into(), "-i".into()]);

        let cmd = super::resolve_windows_shell_argv_with("cmd", None, None, None, None, Some("C:\\Windows\\System32\\cmd.exe")).unwrap();
        assert_eq!(cmd, vec!["C:\\Windows\\System32\\cmd.exe".to_owned()]);

        let wsl = super::resolve_windows_shell_argv_with("wsl", None, None, None, None, None);
        assert_eq!(wsl, None);

        let _ = std::fs::remove_dir_all(&dir);
    }



    #[test]
    fn agent_recycle_bin_preserves_and_restores_deleted_agents() {
        in_own_process("agent_trash_restore_and_purge");
    }

    #[gpui::test]
    fn agent_trash_restore_and_purge(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("agent_trash_restore_and_purge") { return; }
        let (view, cx) = open_window(cx, "unused", "", json!({
            "id": "context", "type": "block", "content": { "tab": "active" }
        }));
        view.update_in(cx, |this, window, cx| {
            this.model.add_space("space", "test", "/tmp", super::Tab::new("tb1", "shell", "pa", "term-a"));
            this.model.add_tab_unselected("space", super::Tab::with_pane("tb2", "Codex", super::AppWindow::acp_pane_node("acp-p1", "a1")));
            this.live.insert("term-a".into(), json!({ "address": "term-a", "state": "live", "argv": ["zsh"] }));
            this.live.insert("a1".into(), json!({
                "address": "a1",
                "acp": true,
                "agent": "codex",
                "state": "live",
                "keep": true,
                "cwd": "/tmp",
                "labels": { "herdr.space": "space", "herdr.tab": "tb2", "herdr.name": "coder" }
            }));
            this.documents.entry(String::new()).or_default().available = true;
            this.sync(window, cx);

            let out = this.run_as("agent.trash", json!({ "terminal": "a1" }), true, window, cx);
            assert!(out.get("error").is_none(), "{out}");
            assert!(this.model.acp_pane_of("a1").is_none());
            assert!(this.live["a1"]["labels"]["herdr.trashed"].as_str().is_some_and(|s| !s.is_empty()));
            assert_eq!(this.live["a1"]["keep"], json!(false));
            let trash = crate::mods::shared("agents.trash").and_then(|v| v.as_array().cloned()).unwrap_or_default();
            assert_eq!(trash.len(), 1);
            assert_eq!(trash[0]["address"], json!("a1"));
            assert_eq!(trash[0]["custom_name"], json!("coder"));

            let out = this.run_as("agent.restore", json!({ "terminal": "a1" }), true, window, cx);
            assert!(out.get("error").is_none(), "{out}");
            assert!(this.model.acp_pane_of("a1").is_some());
            assert_eq!(this.live["a1"]["labels"]["herdr.trashed"], json!(""));
            assert_eq!(this.live["a1"]["keep"], json!(true));
            let trash = crate::mods::shared("agents.trash").and_then(|v| v.as_array().cloned()).unwrap_or_default();
            assert!(trash.is_empty());

            // Trash again, then permanently purge.
            assert!(this.run_as("agent.trash", json!({ "terminal": "a1" }), true, window, cx).get("error").is_none());
            assert!(this.run_as("agent.purge", json!({ "terminal": "a1" }), true, window, cx).get("error").is_none());
            assert!(this.closing_terminals.contains_key("a1"));
            let trash = crate::mods::shared("agents.trash").and_then(|v| v.as_array().cloned()).unwrap_or_default();
            assert!(trash.is_empty());

            // Restoring a trashed side-chat agent opens it in the right side-chat panel, NOT as a top tab.
            this.live.insert("a9".into(), json!({
                "address": "a9",
                "acp": true,
                "agent": "claude",
                "parent": "term-a",
                "lineage": { "kind": "side-chat", "parent": "term-a" },
                "state": "live",
                "keep": false,
                "cwd": "/tmp",
                "labels": {}
            }));
            let tabs_before = this.model.space("space").unwrap().tabs.len();
            assert!(this.run_as("agent.trash", json!({ "terminal": "a9" }), true, window, cx).get("error").is_none());
            assert!(this.live["a9"]["labels"]["herdr.trashed"].as_str().is_some_and(|s| !s.is_empty()));
            assert!(this.run_as("agent.restore", json!({ "terminal": "a9" }), true, window, cx).get("error").is_none());
            assert_eq!(this.live["a9"]["labels"]["herdr.trashed"], json!(""));
            assert!(this.model.any_pane_of("a9").is_none(), "restored side-chat must not open as a top tab");
            assert_eq!(this.model.space("space").unwrap().tabs.len(), tabs_before);
            assert_eq!(crate::mods::shared("side-chat.active"), Some(json!("a9")));

            // terminal.keep / agent.focus / terminal.peek on a side-chat must never mount it in a tab.
            for (cmd, args) in [
                ("terminal.keep", json!({ "terminal": "a9", "place": "tab" })),
                ("terminal.keep", json!({ "terminal": "a9", "place": "right" })),
                ("agent.focus", json!({ "terminal": "a9" })),
                ("terminal.peek", json!({ "terminal": "a9" })),
            ] {
                let res = this.run_as(cmd, args, true, window, cx);
                assert!(res.get("error").is_none(), "{cmd}: {res}");
                assert!(this.model.any_pane_of("a9").is_none(), "{cmd} mounted side-chat in a tab");
                assert_eq!(this.model.space("space").unwrap().tabs.len(), tabs_before);
                assert_eq!(crate::mods::shared("side-chat.active"), Some(json!("a9")));
            }

            // If a side-chat ever slipped into a tab (e.g. from a persisted layout), reconcile evicts it.
            this.model.add_tab("space", super::Tab::with_pane("tb-sc", "side", super::AppWindow::acp_pane_node("p-sc", "a9")));
            assert!(this.model.any_pane_of("a9").is_some());
            assert!(this.reconcile(window, cx));
            assert!(this.model.any_pane_of("a9").is_none(), "reconcile must evict side-chat from tabs");

            // PTY side-chats are never adoptable into tabs even if labeled with herdr.space.
            let pty_side_chat = json!({
                "address": "t9",
                "state": "live",
                "parent": "term-a",
                "lineage": { "kind": "side-chat", "parent": "term-a" },
                "labels": { "herdr.space": "space" }
            });
            assert!(!super::adoptable(&pty_side_chat, "space", None, None));

            // Dead/exited agent key and shell-wrapped argv normalization.
            assert_eq!(super::AppWindow::normalize_agent_program_key("claude-code"), "claude");
            assert_eq!(super::AppWindow::normalize_agent_program_key("cursor-agent"), "cursor");
            assert_eq!(
                super::AppWindow::unwrap_shell_agent_argv(vec![
                    "sh".into(),
                    "-c".into(),
                    "exec".into(),
                    "sh".into(),
                    "claude".into(),
                    "--dangerously-skip-permissions".into(),
                ]),
                vec!["claude".to_string(), "--dangerously-skip-permissions".to_string()],
            );
        });
    }

    #[test]
    fn fork_worktree_close_confirmation_handles_unmerged_and_merged() {
        in_own_process("fork_worktree_close_prompts_for_unmerged_and_merged_states");
    }

    #[gpui::test]
    fn fork_worktree_close_prompts_for_unmerged_and_merged_states(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("fork_worktree_close_prompts_for_unmerged_and_merged_states") { return; }
        let (view, cx) = open_window(cx, "unused", "", json!({
            "id": "context", "type": "block", "content": { "tab": "active" }
        }));
        view.update_in(cx, |this, window, cx| {
            // Main repo space + dedicated worktree space.
            this.model.add_space("sp-main", "repo", "/tmp/repo", super::Tab::new("tb1", "shell", "pa", "term-a"));
            this.model.add_space("sp-wt1", "", "/tmp/wt-fork-1", super::Tab::new("tb-wt1-sh", "shell", "p-wt1-sh", "term-wt1"));
            this.model.add_tab("sp-wt1", super::Tab::with_pane("tb2", "Fork 1", super::AppWindow::acp_pane_node("acp-fk1", "a2")));
            this.live.insert("term-a".into(), json!({ "address": "term-a", "state": "live", "argv": ["zsh"], "cwd": "/tmp/repo" }));
            this.live.insert("term-wt1".into(), json!({ "address": "term-wt1", "state": "live", "argv": ["zsh"], "cwd": "/tmp/wt-fork-1" }));
            this.live.insert("a2".into(), json!({
                "address": "a2",
                "acp": true,
                "agent": "claude",
                "state": "live",
                "keep": true,
                "cwd": "/tmp/wt-fork-1",
                "lineage": {
                    "kind": "fork",
                    "parent": "a1",
                    "worktree_path": "/tmp/wt-fork-1",
                    "worktree_origin": "/tmp/repo",
                    "worktree_branch": "herdr/fork-fk1",
                    "worktree_merged": false
                },
                "labels": { "herdr.space": "sp-wt1", "herdr.tab": "tb2", "herdr.name": "fork-one" }
            }));
            this.documents.entry(String::new()).or_default().available = true;
            this.sync(window, cx);

            // 1. Mounted worktree forked agent belongs to its dedicated worktree space in agents snapshot.
            let published_agents = crate::mods::agents().as_array().cloned().unwrap_or_default();
            let a2_entry = published_agents.iter().find(|v| v["address"] == json!("a2")).expect("a2 in agents");
            assert_eq!(a2_entry["space"], json!("sp-wt1"));
            assert_eq!(a2_entry["space_name"], json!("wt-fork-1"));

            // 2. Closing a fork worktree tab parks the agent in background, AND parked worktree agent STILL
            //    belongs to its dedicated worktree space (never hijacked by the parent origin space).
            let out = this.run_as("tab.close", json!({ "id": "tb2" }), true, window, cx);
            assert!(out.get("error").is_none(), "{out}");
            assert!(this.model.tab("tb2").is_none(), "tab should close cleanly");
            assert!(!this.closing_terminals.contains_key("a2"), "tab.close must never kill agent");
            assert_eq!(this.live["a2"]["labels"]["herdr.parked"], json!("1"));
            assert!(this.overlays.is_empty(), "tab.close must not prompt deletion");
            let sp_main = this.model.space("sp-main").unwrap();
            let sp_wt1 = this.model.space("sp-wt1").unwrap();
            assert!(!this.acp_matches_space("a2", &this.live["a2"], sp_main), "worktree fork must not match parent space when worktree space exists");
            assert!(this.acp_matches_space("a2", &this.live["a2"], sp_wt1), "worktree fork must match its own worktree space");
            let published_agents = crate::mods::agents().as_array().cloned().unwrap_or_default();
            let a2_parked = published_agents.iter().find(|v| v["address"] == json!("a2")).expect("parked a2 in agents");
            assert_eq!(a2_parked["space"], json!("sp-wt1"));

            // 3. Keeping an unmounted worktree fork with NO existing worktree space targets a brand-new worktree space.
            this.live.insert("a3".into(), json!({
                "address": "a3",
                "acp": true,
                "agent": "claude",
                "state": "live",
                "keep": false,
                "cwd": "/tmp/wt-fork-2",
                "lineage": {
                    "kind": "fork",
                    "parent": "a1",
                    "worktree_path": "/tmp/wt-fork-2",
                    "worktree_origin": "/tmp/repo",
                    "worktree_branch": "herdr/fork-fk2",
                    "worktree_merged": false
                },
                "labels": { "herdr.name": "fork-two" }
            }));
            this.model.active = Some("sp-main".into());
            let keep_out = this.keep_terminal_with_focus("a3", "tab", true, window, cx).unwrap();
            assert_eq!(keep_out["pending"], json!(true));
            let pending = this.pending_terminals.remove("a3").expect("pending keep for a3");
            this.apply_terminal_change("a3".into(), pending.change, Ok(()), window, cx);
            let wt2_space = this.model.spaces.iter().find(|s| s.folder == "/tmp/wt-fork-2").expect("dedicated worktree space created for a3");
            let wt2_id = wt2_space.id.clone();
            assert_eq!(wt2_space.name, "", "worktree space stays unnamed so spaces mod nests it under the repo");
            assert_eq!(this.model.active.as_deref(), Some(wt2_id.as_str()));
            let published_agents = crate::mods::agents().as_array().cloned().unwrap_or_default();
            let a3_entry = published_agents.iter().find(|v| v["address"] == json!("a3")).expect("a3 in agents");
            assert_eq!(a3_entry["space"], json!(wt2_id));
            assert_eq!(a3_entry["space_name"], json!("wt-fork-2"));

            // 4. Trashing a worktree fork when its space closes, then restoring it recreates its dedicated worktree space
            //    (NOT restoring into sp-main).
            assert!(this.run_as("agent.trash", json!({ "terminal": "a3" }), true, window, cx).get("error").is_none());
            assert!(this.model.space(&wt2_id).is_none(), "test without live core closes 1-tab space");
            this.model.active = Some("sp-main".into());
            assert!(this.run_as("agent.restore", json!({ "terminal": "a3" }), true, window, cx).get("error").is_none());
            let wt2_restored = this.model.spaces.iter().find(|s| s.folder == "/tmp/wt-fork-2").expect("restore must recreate worktree space");
            let wt2_restored_id = wt2_restored.id.clone();
            assert_eq!(wt2_restored.name, "", "recreated worktree space stays unnamed");
            this.model.add_tab_unselected(&wt2_restored_id, super::Tab::new("tb-wt2-sh", "shell", "p-wt2-sh", "term-wt2"));
            this.live.insert("term-wt2".into(), json!({ "address": "term-wt2", "state": "live", "argv": ["zsh"], "cwd": "/tmp/wt-fork-2" }));

            // 5. Move both fork worktree agents to recycle bin (preserving them in trash without killing).
            assert!(this.run_as("agent.trash", json!({ "terminal": "a2" }), true, window, cx).get("error").is_none());
            assert!(this.run_as("agent.trash", json!({ "terminal": "a3" }), true, window, cx).get("error").is_none());
            assert!(!this.closing_terminals.contains_key("a2"));
            assert!(!this.closing_terminals.contains_key("a3"));

            // 6. Permanently purging an unmerged fork worktree from recycle bin shows a single-step deletion warning
            //    (NO merge-back prompt, NO secondary button, NO nextStep).
            let out = this.run_as("agent.purge", json!({ "terminal": "a2" }), true, window, cx);
            assert!(out.get("error").is_none(), "{out}");
            assert!(!this.closing_terminals.contains_key("a2"), "unconfirmed purge must keep agent in trash");
            assert_eq!(this.overlays.len(), 1);
            let props1 = &this.copies.get(&this.overlays[0].slot).unwrap().inst.props;
            assert!(props1.get("secondary").is_none(), "purge must NOT ask to merge back: {props1}");
            assert!(props1.get("nextStep").is_none(), "purge must be single-step: {props1}");
            assert_eq!(props1["confirmDanger"], json!(true));
            let items1 = props1["items"].as_array().expect("purge dialog includes structured items");
            assert_eq!(items1.len(), 1);
            assert_eq!(items1[0]["address"], json!("a2"));
            assert_eq!(items1[0]["name"], json!("fork-one"));
            assert_eq!(items1[0]["branch"], json!("herdr/fork-fk1"));
            assert_eq!(items1[0]["path"], json!("/tmp/wt-fork-1"));
            assert_eq!(props1["command"]["name"], json!("agent.purge"));
            assert_eq!(props1["command"]["args"]["confirmed"], json!(true));
            this.overlays.clear();

            // 7. Emptying recycle bin with multiple worktree forked agents shows ONE aggregated confirmation dialog
            //    listing both worktrees instead of prompting one by one.
            let out = this.run_as("agent.empty_trash", json!({}), true, window, cx);
            assert!(out.get("error").is_none(), "{out}");
            assert!(!this.closing_terminals.contains_key("a2"));
            assert!(!this.closing_terminals.contains_key("a3"));
            assert_eq!(this.overlays.len(), 1, "must aggregate multiple worktrees into a single dialog");
            let empty_props = &this.copies.get(&this.overlays[0].slot).unwrap().inst.props;
            assert!(empty_props.get("secondary").is_none());
            assert!(empty_props.get("nextStep").is_none());
            assert_eq!(empty_props["confirmDanger"], json!(true));
            let empty_items = empty_props["items"].as_array().expect("empty_trash dialog includes structured items");
            assert_eq!(empty_items.len(), 2, "must list both worktree forked agents: {empty_items:?}");
            assert!(empty_items.iter().any(|it| it["address"] == json!("a2") && it["path"] == json!("/tmp/wt-fork-1") && it["branch"] == json!("herdr/fork-fk1")));
            assert!(empty_items.iter().any(|it| it["address"] == json!("a3") && it["path"] == json!("/tmp/wt-fork-2") && it["branch"] == json!("herdr/fork-fk2")));
            assert_eq!(empty_props["command"]["name"], json!("agent.empty_trash"));
            assert_eq!(empty_props["command"]["args"]["confirmed"], json!(true));
            this.overlays.clear();

            // 8. Confirmed empty_trash permanently removes both worktree agents, closes their worktree spaces,
            //    and schedules their worktree shells to terminate before removal.
            let out = this.run_as("agent.empty_trash", json!({ "confirmed": true }), true, window, cx);
            assert!(out.get("error").is_none(), "{out}");
            assert!(this.closing_terminals.contains_key("a2"));
            assert!(this.closing_terminals.contains_key("a3"));
            assert!(this.closing_terminals.contains_key("term-wt1"));
            assert!(this.closing_terminals.contains_key("term-wt2"));
            assert!(this.model.space("sp-wt1").is_none(), "worktree space sp-wt1 should close on purge");
            assert!(this.model.space(&wt2_restored_id).is_none(), "worktree space wt2 should close on purge");
            assert!(this.model.space("sp-main").is_some(), "parent repo space must remain intact");

            // 9. Empty trash with only non-worktree agents executes immediately without prompting.
            this.live.insert("a4".into(), json!({
                "address": "a4",
                "acp": true,
                "agent": "claude",
                "state": "live",
                "keep": false,
                "cwd": "/tmp/repo",
                "labels": {}
            }));
            assert!(this.run_as("agent.trash", json!({ "terminal": "a4" }), true, window, cx).get("error").is_none());
            let out = this.run_as("agent.empty_trash", json!({}), true, window, cx);
            assert!(out.get("error").is_none(), "{out}");
            assert!(this.overlays.is_empty(), "non-worktree empty_trash should not prompt");
            assert!(this.closing_terminals.contains_key("a4"));
        });
    }

    #[gpui::test]
    fn worktree_blocked_dialog_explains_reason_and_offers_agent_send_fix(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        if !running_alone("worktree_blocked_dialog_explains_reason_and_offers_agent_send_fix") { return; }
        let parsed = super::AppWindow::parse_worktree_blocked_error(
            r#"agent.handoff: wsl: WORKTREE_BLOCKED:{"code":"dirty_worktree","cwd":"/home/u/test","op":null,"files":["M calc.py"],"total_dirty":1}"#,
        )
        .expect("should parse WORKTREE_BLOCKED JSON");
        assert_eq!(parsed["code"], json!("dirty_worktree"));
        assert_eq!(parsed["total_dirty"], json!(1));

        let (view, cx) = open_window(cx, "unused", "", json!({
            "id": "context", "type": "block", "content": { "tab": "active" }
        }));
        view.update_in(cx, |this, window, cx| {
            this.live.insert("t2t".into(), json!({
                "address": "t2t",
                "title": "Claude Code",
                "state": "live",
                "cwd": "/home/u/test",
                "labels": {}
            }));
            this.open_worktree_blocked_dialog("t2t", &parsed, window, cx);
            let ov = this.overlays.iter().find(|o| o.id == "worktree-blocked").expect("worktree-blocked dialog must open");
            let props = &this.copies.get(&ov.slot).unwrap().inst.props;
            assert_eq!(props["command"]["name"], json!("agent.send"));
            assert_eq!(props["command"]["args"]["agent"], json!("t2t"));
            assert_eq!(props["command"]["args"]["focus"], json!(true));
            let prompt = props["command"]["args"]["text"].as_str().unwrap_or("");
            assert!(prompt.contains("/home/u/test") && prompt.contains("M calc.py"), "unexpected fix prompt: {prompt}");
            assert_eq!(props["items"].as_array().map(Vec::len), Some(1));
        });
    }
    /// with the 8 px margin: its body may not paint past the window border
    /// (the reported right-edge menu clip).
    #[gpui::test]
    fn popup_at_right_edge_snaps_inside(cx: &mut TestAppContext) {
        struct PopupView { at: gpui::Point<gpui::Pixels> }
        impl Render for PopupView {
            fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
                div().size_full().child(super::position_popup(
                    self.at,
                    div().id("popup-body").debug_selector(|| "POPUP".into()).w(px(220.)).h(px(140.)).bg(gpui::rgb(0x00ff00)),
                ))
            }
        }
        let window = cx.open_window(size(px(800.), px(600.)), |_, _| PopupView {
            at: point(px(770.), px(100.)),   // 220 wide: 770 + 220 = 990, well past the 800 px edge
        });
        cx.run_until_parked();
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        let bounds = visual.debug_bounds("POPUP").expect("the popup body never laid out");
        // The right edge sits exactly 8 px inside the window: SwitchAnchor alone
        // would turn the anchor around and stop 220 px short (right at 770), so
        // this range pins the margin, not just "somewhere inside". A pixel of
        // rounding is allowed.
        assert!(
            bounds.right() >= gpui::px(780.) && bounds.right() <= gpui::px(793.),
            "popup is not snapped to the right margin: right = {:?}",
            bounds.right(),
        );
        assert!(
            bounds.left() >= gpui::px(8.),
            "popup lost its left margin: left = {:?}",
            bounds.left(),
        );
    }
}
