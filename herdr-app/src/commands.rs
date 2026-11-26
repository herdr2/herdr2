//! Every command the app runs, defined once. Keys, menus, mods and
//! `herdr2 do` all run these; key checks, `herdr2 commands` and the docs come
//! from this list. Mods' own commands are `<mod>.<command>`.

use serde_json::{Value, json};

pub struct Arg {
    pub name: &'static str,
    pub required: bool,
}

pub struct Command {
    pub name: &'static str,
    pub args: &'static [Arg],
    pub about: &'static str,
}

const fn req(name: &'static str) -> Arg {
    Arg { name, required: true }
}

const fn opt(name: &'static str) -> Arg {
    Arg { name, required: false }
}

const fn cmd(name: &'static str, args: &'static [Arg], about: &'static str) -> Command {
    Command { name, args, about }
}

pub const COMMANDS: &[Command] = &[
    // What exists.
    cmd("space.new", &[opt("name"), opt("folder"), opt("tab"), opt("argv"), opt("task"), opt("model"), opt("effort"), opt("machine"), opt("shell"), opt("focus")],
        "Make a space: its name (default: none, so it's called after where its first terminal is), the folder its terminals start in (default: the focused terminal's), its first tab's name (default: none, so it's called by its number), optionally an agent (argv), model, reasoning effort, and its first message (task), and the machine it lives on (`h2 machine ls`; default the focused space's machine for keys and clicks, this computer for scripts; empty string: this computer; its terminals start in that machine's home unless folder). It is shown at once; focus false keeps the shown space and tab (with no space shown, the new space is shown)."),
    cmd("space.close", &[opt("id"), opt("confirmed")], "Close a space and end its terminals (asks first unless confirmed)."),
    cmd("space.rename", &[opt("id"), opt("name")], "Rename a space (asks the person when no name; an empty name: called after where its first terminal is again)."),
    cmd("space.move", &[req("id"), req("index")], "Move a space to a position (1 = first)."),
    cmd("tab.new", &[opt("name"), opt("space"), opt("folder"), opt("argv"), opt("task"), opt("model"), opt("effort"), opt("mod"), opt("props"), opt("acp"), opt("agent"), opt("shell"), opt("focus")],
        "Make a tab in a space (default: the selected one; name default: none, so it's called by its number) with a terminal in folder (default: the focused terminal's), optionally an agent (argv), model, reasoning effort, and its first message (task); or with a mod as its only pane (`mod`, its `props`: a file, a board); or an ACP agent (`acp: true`, `agent`, optional `model` and `effort`). It is shown at once; focus false adds it last in its space and keeps the shown space and the target space's selected tab. With no space open, a person's new tab starts a space (as space.new)."),
    cmd("tab.close", &[opt("id"), opt("confirmed")], "Close a tab and end its terminals."),
    cmd("tab.rename", &[opt("id"), opt("name")], "Rename a tab (asks the person when no name; an empty name: called by its number again)."),
    cmd("tab.move", &[opt("id"), opt("index"), opt("by")], "Move a tab (the active one if no id) to a position in its space (1 = first), or `by` places left (-) or right (+), wrapping."),
    cmd("tab.merge", &[req("source"), req("target"), opt("direction")], "Merge all panes of the source tab beside the target tab's panes (direction: \"right\", default, \"left\", \"down\" or \"up\"), removing the source tab and selecting the target tab."),
    cmd("pane.split", &[req("direction"), opt("pane"), opt("argv"), opt("mod"), opt("props"), opt("shell")], "Split a pane (default: the focused one) \"right\" or \"down\" with a new terminal, or with a mod (`mod`, its `props`) beside it."),
    cmd("pane.close", &[opt("pane"), opt("confirmed")], "Close a pane and end its terminal."),
    cmd("pane.detach", &[opt("pane")], "Move a pane (default: the focused one) out of a split tab into its own tab beside it and select it."),
    cmd("terminal.open", &[opt("argv"), opt("folder"), opt("place"), opt("name"), opt("shell")], "Start a terminal: as a new tab (place \"tab\", default), or \"right\"/\"down\" of the focused pane."),
    cmd("terminal.keep", &[req("terminal"), opt("place")], "Give a running terminal without a pane (a sub-agent) a pane: a new tab, or \"right\"/\"down\"."),
    cmd("terminal.park", &[req("terminal")], "Take a terminal's pane away and disable core keep; it keeps running (a sub-agent shows under its parent again)."),
    cmd("terminal.right_click", &[req("to"), opt("terminal")], "Where right-clicks in a terminal (default: the focused one) go when its program takes the mouse: \"program\" or \"menu\" (Herdr2's, the default). Shift+right-click is always the menu."),
    cmd("terminal.view", &[req("view"), opt("terminal"), opt("tab")], "Switch how a terminal displays in its tab pane: \"peek\" (structured conversation view, like ACP), \"terminal\" (terminal grid), or \"toggle\". Targets terminal, all non-ACP terminals in tab, or the focused terminal."),
    cmd("terminal.pin", &[req("terminal"), req("pinned")], "Pin an agent to the top of the agent lists (pinned true) or unpin it (false). Pins are kept with the terminal, so every Herdr2 client shows them, and end with it."),
    cmd("terminal.kill", &[req("terminal")], "End a terminal and its sub-agents."),
    // What you see.
    cmd("space.select", &[opt("index"), opt("id")], "Show a space (by position or id)."),
    cmd("space.next", &[], "Show the next space."),
    cmd("space.prev", &[], "Show the previous space."),
    cmd("tab.select", &[opt("index"), opt("id")], "Show a tab (by position in the space, or id)."),
    cmd("tab.settle", &[opt("id")], "Put a tab away on this screen (default: the shown one): out of the lists, still running. It comes back when shown or when an agent in it needs you."),
    cmd("tab.unsettle", &[opt("id")], "Bring a settled tab back to the lists."),
    cmd("tab.next", &[], "Show the next tab."),
    cmd("tab.prev", &[], "Show the previous tab."),
    cmd("pane.zoom", &[opt("pane")], "Show only this pane of its tab, or all again."),
    cmd("pane.focus", &[opt("pane"), opt("terminal")], "Focus a pane (or the pane showing a terminal)."),
    cmd("pane.go", &[req("direction")], "Focus the pane or focusable mod to the \"left\", \"right\", \"up\" or \"down\"."),
    cmd("pane.swap", &[opt("direction"), opt("pane"), opt("target")], "Swap a pane (default: the focused one) with its neighbor in a direction, or with another pane of its tab (target). Give exactly one of direction or target."),
    cmd("terminal.peek", &[req("terminal")], "Peek at a running terminal over the tab, with keep-as-tab and close controls."),
    cmd("terminal.show", &[opt("terminal"), opt("pane")], "Show a terminal in a pane for now, in place of its own (no terminal: back)."),
    cmd("terminal.select_all", &[opt("terminal")], "Select a terminal's whole screen and scrollback (default: the focused one), ready to copy. On the alternate screen, select only that screen."),
    cmd("terminal.select", &[opt("terminal"), opt("at"), opt("mode")], "Start a terminal selection (default: the focused one), replacing the old selection. at defaults to \"cursor\" or takes {row, col}: zero-based cells, rows from the top of the history including the active screen. mode is \"cell\" (default), \"word\" or \"line\" (like double/triple click). Mods read the inclusive endpoints from focused().selection or terminals(); selection.end is the moving end."),
    cmd("terminal.adjust_selection", &[opt("terminal"), req("direction")], "Move the selection's moving end: left, right, up, down, page_up, page_down, home, end, line_start or line_end. The view follows the moving end. Fails if there is no selection. Defaults to the focused terminal."),
    cmd("terminal.clear_selection", &[opt("terminal")], "Clear a terminal's selection (default: the focused one)."),
    cmd("terminal.copy", &[], "Copy the focused terminal's selection. With nothing selected, that key goes to the program."),
    cmd("terminal.scroll", &[opt("lines"), opt("pages"), opt("to"), opt("terminal")], "Scroll a terminal's view of its history (default: the focused one). Give exactly one of: lines (a whole number; negative goes up, toward history), pages (a number, fractions allowed; one page is the terminal's rows), to: \"top\" (oldest history) or \"bottom\" (the live screen). This scrolls Herdr2's view, not the program."),
    cmd("terminal.paste", &[], "Paste into the focused terminal (copied files as if dropped; an image alone as terminal.paste_image pastes it)."),
    cmd("terminal.paste_image", &[], "Ctrl+V: the clipboard's image as a file on the focused terminal's machine, when that's another one; on Windows, paste clipboard text; otherwise Ctrl+V goes to the terminal."),
    cmd("terminal.search", &[], "Search the focused terminal's screen and scrollback; the find bar and query stay with that terminal."),
    cmd("agent.focus", &[req("terminal")], "Show an agent (sub-agents too)."),
    cmd("agent.rename", &[opt("terminal"), opt("name")], "Rename an agent (asks the person when no name; an empty name returns to its automatic label; default target: the focused or shown agent)."),
    cmd("agent.trash", &[opt("terminal"), opt("confirmed")], "Move an agent (default: the focused or selected agent) to the recycle bin, closing its pane while preserving its session so it can be restored."),
    cmd("agent.restore", &[req("terminal")], "Restore a deleted agent from the recycle bin back into its space and tab."),
    cmd("agent.purge", &[req("terminal"), opt("confirmed")], "Permanently delete an agent from the recycle bin and end its session."),
    cmd("agent.empty_trash", &[opt("confirmed")], "Permanently delete all agents in the recycle bin."),
    cmd("agent.status", &[req("status"), opt("terminal")], "Set an agent's status manually (\"idle\", \"working\", \"blocked\", \"done\"), or \"auto\" to return to automatic detection (default target: the focused terminal or shown agent)."),
    cmd("agent.briefing", &[opt("away_minutes")], "Open the Catch-up Briefing overlay summarizing critical blockers, batchable permission approvals, and completed agents."),
    cmd(
        "agent.handoff",
        &[
            opt("source"),
            opt("preset"),
            opt("into"),
            opt("kind"),
            opt("mode"),
            opt("prompt"),
            opt("recent_turns"),
            opt("from_turn"),
            opt("to_turn"),
            opt("max_bytes"),
            opt("model"),
            opt("effort"),
            opt("worktree"),
            opt("merge_worktree"),
            opt("archive_source"),
            opt("close_overlay"),
            opt("open"),
        ],
        "Hand off, fork (optionally in an isolated git worktree), open a floating side-chat, or merge context from an agent or CLI session into a new or existing session (opens the Handoff dialog when neither preset nor into is given).",
    ),
    cmd(
        "agent.send",
        &[opt("agent"), opt("terminal"), req("text"), opt("steer"), opt("focus")],
        "Send a message to a live ACP agent or CLI terminal (default target: focused or active session).",
    ),
    cmd(
        "agent.send_review",
        &[
            opt("target"),
            req("comments"),
            opt("source"),
            opt("checkpoint_turn"),
            opt("repo_root"),
            opt("note"),
            opt("steer"),
            opt("focus"),
            opt("request_id"),
        ],
        "Send structured line-level diff review comments to a live ACP agent or PTY terminal (default target: focused or active session).",
    ),
    cmd("agent.next", &[], "Show the next agent, in the order the agents list shows them."),
    cmd("agent.prev", &[], "Show the previous agent."),
    cmd("prompt.polish", &[opt("terminal")], "Polish the active prompt with Core AI (inline in ACP/CLI chat, Flash Note, or directly in-place in a CLI terminal)."),
    cmd("prompt.polish_undo", &[opt("terminal")], "Undo the last AI prompt polish and restore the pre-polish input (inline in ACP/CLI chat, Flash Note, Prompt modal, or directly in-place in a CLI terminal)."),
    cmd("prompt.polish_cancel", &[], "Cancel an in-flight in-place CLI prompt polish."),
    cmd("layout.switch", &[opt("name")], "Show a layout (layouts/<name>.json), or the next one."),
    cmd("layout.clone", &[req("name"), opt("as")], "Duplicate a layout as <name>-copy (or as), then show it."),
    cmd("mod.clone", &[req("name"), opt("as")], "Duplicate a mod as <name>-copy (or as)."),
    cmd("layout.set", &[req("layout")], "Replace a personal layout tree; shipped layouts accept only click/look fields (not structure)."),
    cmd("layout.toggle", &[req("id")], "Hide or show a layout node."),
    cmd("layout.show", &[req("id")], "Show a layout node."),
    cmd("layout.hide", &[req("id")], "Hide a layout node."),
    cmd("layout.resize", &[req("id"), opt("px"), opt("weight")], "Size a layout node."),
    cmd("layout.select", &[req("stack"), req("child")], "Show one child of a stack."),
    cmd("layout.reset", &[opt("name")], "Reset a shipped layout and its clicks (the old file is kept aside)."),
    cmd("mod.open", &[req("name")], "Toggle a mod as the overlay its mod.json describes; calling again closes it."),
    cmd(
        "files.open",
        &[req("path"), opt("line"), opt("end_line"), opt("on"), opt("cwd")],
        "Open a file in the Files panel (works for workspace and external paths), optionally jumping to a line.",
    ),
    cmd("overlay.close", &[opt("id")], "Close an overlay (default: the top dialog or popup)."),
    // The app.
    cmd("app.help", &[], "Toggle the keys dialog."),
    cmd("app.welcome", &[], "Show the welcome again."),
    cmd("app.palette", &[], "Toggle the command palette: every command by name, with its key; calling again closes it."),
    cmd("app.settings", &[opt("section")], "Open the settings (section: look, keyboard, machines, ai, drove, alerts, updates)."),
    cmd("app.reset", &[], "The escape hatch: set the shown layout and keys aside, back to the default screen."),
    cmd("app.reload_mods", &[], "Hot-reload all UI mods in a fresh script VM without restarting the window or terminals."),
    cmd("app.restart", &[], "Restart the Herdr2 window cleanly (terminals stay alive in h2 core)."),
    cmd("app.check_updates", &[], "Check for Herdr2 app, UI bundle, and h2 core updates now."),
    cmd("app.apply_ui_update", &[], "Apply a staged UI bundle hot-update live without restarting."),
    cmd("app.update", &[], "Apply a staged Herdr2 app update and restart."),
    cmd("app.update_core", &[], "Update h2 core in the background without interrupting terminals."),
    cmd("app.theme", &[req("name")], "Use a theme (a preset or themes/<name>.json)."),
    cmd("app.language", &[req("language")], "Set the interface language (\"en\" or \"zh-CN\")."),
    cmd("app.shell", &[req("shell")], "Set the default local terminal shell (\"auto\", \"powershell\", \"pwsh\", \"git-bash\", \"cmd\", or a custom executable path) used for both terminals and CLI agents."),
    cmd("app.zoom_in", &[], "Make everything bigger."),
    cmd("app.zoom_out", &[], "Make everything smaller."),
    cmd("app.zoom_reset", &[], "Back to normal size."),
    cmd("app.quit", &[], "Quit (macOS)."),
    cmd("app.hide", &[], "Hide the app (macOS)."),
    cmd("app.hide_others", &[], "Hide other apps (macOS)."),
    cmd("app.show_all", &[], "Show all apps (macOS)."),
    cmd("window.close", &[], "Close the window (on Linux, quits)."),
    cmd("window.minimize", &[], "Minimize the window."),
    cmd("window.maximize", &[], "Maximize the window, or back."),
];

pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// A key's `name:value` shorthand: the value fills the first argument (a
/// number fills `index` when there is one).
pub fn shorthand(c: &Command, value: &str) -> Value {
    let index = value.parse::<u64>().ok().filter(|_| c.args.iter().any(|a| a.name == "index"));
    match (index, c.args.first()) {
        (Some(i), _) => json!({ "index": i }),
        (None, Some(a)) => json!({ a.name: value }),
        (None, None) => Value::Null,
    }
}

/// Reject misspelled arguments before a command can use its current-target default.
/// Null is the argument-free form used by keys and mods.
pub fn validate_args(c: &Command, args: &Value) -> Result<(), String> {
    let valid = if c.args.is_empty() { "none".into() } else { c.args.iter().map(|a| a.name).collect::<Vec<_>>().join(", ") };
    match args {
        Value::Null => Ok(()),
        Value::Object(args) => {
            let unknown: Vec<_> = args.keys().filter(|key| !c.args.iter().any(|a| a.name == key.as_str())).map(String::as_str).collect();
            if !unknown.is_empty() {
                return Err(format!("{}: unknown argument(s): {}; valid arguments: {valid}", c.name, unknown.join(", ")));
            }
            for (key, value) in args {
                if matches!(key.as_str(), "id" | "pane" | "space" | "terminal" | "target" | "source") && !value.is_string() && !value.is_null() {
                    return Err(format!("{}: argument {key} must be a string (or null to omit it)", c.name));
                }
            }
            Ok(())
        }
        _ => Err(format!("{}: arguments must be an object; valid arguments: {valid}", c.name)),
    }
}

/// The first required argument that's missing (an empty string counts as missing).
pub fn missing(c: &Command, args: &Value) -> Option<&'static str> {
    c.args.iter().filter(|a| a.required).map(|a| a.name).find(|k| match args.get(k) {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        _ => false,
    })
}

/// The list as data, for `herdr2 commands` and mods.
pub fn listing() -> Value {
    Value::Array(
        COMMANDS
            .iter()
            .map(|c| {
                let args: Vec<Value> = c.args.iter().map(|a| json!({ "name": a.name, "required": a.required })).collect();
                json!({ "name": c.name, "args": args, "about": c.about })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guide agents read (written to the home folder) names every command.
    #[test]
    fn the_guide_names_every_command() {
        let guide = include_str!("../defaults/AGENTS.md");
        let missing: Vec<&str> = COMMANDS.iter().map(|c| c.name).filter(|n| !guide.contains(&format!("`{n}"))).collect();
        assert!(missing.is_empty(), "defaults/AGENTS.md (## Commands) doesn't name: {missing:?}");
    }

    #[test]
    fn unknown_arguments_are_rejected_for_every_command() {
        for c in COMMANDS {
            let error = validate_args(c, &json!({ "typo": "target" })).unwrap_err();
            assert!(error.contains(c.name), "{error}");
            assert!(error.contains("unknown argument(s): typo"), "{error}");
            assert!(error.contains("valid arguments:"), "{error}");
            assert!(validate_args(c, &Value::Null).is_ok());
            assert!(validate_args(c, &json!({})).is_ok());
            assert!(validate_args(c, &json!(["target"])).is_err());
            let args: serde_json::Map<String, Value> = c.args.iter().map(|a| (a.name.into(), json!("target"))).collect();
            assert!(validate_args(c, &Value::Object(args)).is_ok());
        }
        let close = find("space.close").unwrap();
        assert_eq!(validate_args(close, &json!({"space": "X", "confirmed": true})),
            Err("space.close: unknown argument(s): space; valid arguments: id, confirmed".into()));
        assert!(validate_args(close, &json!({"id": "X", "confirmed": true})).is_ok());
    }

    #[test]
    fn malformed_targets_cannot_fall_back_to_the_current_work() {
        for (name, arg) in [("tab.close", "id"), ("pane.close", "pane"), ("pane.detach", "pane"), ("space.close", "id"), ("tab.new", "space"), ("terminal.show", "terminal"), ("pane.swap", "pane"), ("pane.swap", "target")] {
            let c = find(name).unwrap();
            for value in [json!(123), json!(false), json!([]), json!({"id": "target"})] {
                let error = validate_args(c, &json!({arg: value})).unwrap_err();
                assert!(error.contains(name) && error.contains(arg), "{error}");
            }
            // Null keeps its existing omitted-argument meaning for optional targets.
            for value in [Value::Null, json!("target")] {
                assert!(validate_args(c, &json!({arg: value})).is_ok());
            }
        }
    }

    #[test]
    fn names_are_unique_and_shorthand_fills_the_first_argument() {
        let mut names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), COMMANDS.len());
        assert_eq!(shorthand(find("tab.select").unwrap(), "3"), json!({ "index": 3 }));
        assert_eq!(shorthand(find("pane.split").unwrap(), "right"), json!({ "direction": "right" }));
        let move_ = find("space.move").unwrap();
        assert_eq!(missing(move_, &json!({ "id": " ", "index": 1 })), Some("id"));
        assert_eq!(missing(move_, &json!({ "id": "sp1", "index": 1 })), None);
        // Names are optional: none is Herdr2's automatic one.
        assert_eq!(missing(find("space.new").unwrap(), &json!({})), None);
    }
}
