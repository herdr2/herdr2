//! What runs in a terminal, as people see it: a name, an icon and a tile.
//! One table for every mod: `defaults/programs.json` (built in), with the
//! person's `programs.json` in the home folder on top (entry by entry). The
//! shipped icons are written to `icons/` in the home folder (the app's;
//! rewritten when they change). Mods read the result as `program` on
//! panes, agents and terminals.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::BTreeMap;

const BUILT_IN: &str = include_str!("../defaults/programs.json");
include!(concat!(env!("OUT_DIR"), "/default_icons.rs"));

#[derive(Deserialize, Default)]
struct Table {
    #[serde(default)]
    shells: Vec<String>,
    #[serde(default)]
    interpreters: Vec<String>,
    #[serde(default)]
    programs: BTreeMap<String, Entry>,
}

#[derive(Deserialize, Serialize, Default, Clone)]
struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// A path in the home folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    /// A one-color glyph, drawn in `color` (or the text color); else a picture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<String>,
    /// Other process names (or agents' 7501 app names) for this entry.
    #[serde(default, rename = "match", skip_serializing_if = "Vec::is_empty")]
    also: Vec<String>,
    /// How to start it, for an agent people can start from a menu (`knownAgents()`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    run: Vec<String>,
}

impl Entry {
    fn over(&mut self, mine: Entry) {
        self.name = mine.name.or(self.name.take());
        self.icon = mine.icon.or(self.icon.take());
        self.tint = mine.tint.or(self.tint);
        self.tile = mine.tile.or(self.tile.take());
        self.color = mine.color.or(self.color.take());
        if !mine.also.is_empty() {
            self.also = mine.also;
        }
        if !mine.run.is_empty() {
            self.run = mine.run;
        }
    }
}

/// The agents people pick most, first in menus; the rest follow by name.
const AGENTS_FIRST: &[&str] = &["claude", "codex", "antigravity", "pi", "opencode", "cursor", "copilot", "amp", "grok", "kimi", "qwen", "hermes", "glm", "zcode", "trae"];

/// Every agent the table can start (entries with `run`), most picked first:
/// `{ key, name, icon, tint, tile, color, run }`.
pub fn startable() -> Vec<Value> {
    TABLE.with(|t| {
        let t = t.borrow();
        let mut keys: Vec<&String> = t.programs.iter().filter(|(_, e)| !e.run.is_empty()).map(|(k, _)| k).collect();
        keys.sort_by_key(|k| (AGENTS_FIRST.iter().position(|f| f == k).unwrap_or(AGENTS_FIRST.len()), k.to_string()));
        keys.into_iter()
            .map(|k| {
                let e = &t.programs[k];
                let mut v = shown(k, e, None, false, true);
                v["run"] = json!(e.run);
                v
            })
            .collect()
    })
}

/// Which of `commands` this machine can start, asked of the shell agents start
/// in (so an app opened from the Dock finds what the person's terminal finds).
/// On another machine (`on`): its login/interactive shell, as remote agents start.
pub fn installed(commands: &[String], on: Option<&str>) -> Result<Vec<String>, String> {
    if commands.is_empty() {
        return Ok(Vec::new());
    }
    // Each found one as a marked line: an interactive shell may print its own
    // things (a prompt's escapes, a greeting) around the answer.
    let result = match on {
        Some(m) => {
            let argv = crate::remote_shell::check(commands);
            let result = match crate::mods::run_remote(m, &argv[0], &argv[1..], None) {
                Err(e) if e.contains("cannot start sh: program not found") => {
                    crate::mods::run_remote(m, "powershell.exe", &installed_powershell(commands), None)
                }
                r => r,
            };
            result.map(|(code, out, err)| (code, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned()))
        }
        None if cfg!(windows) => return Ok(installed_windows_path(commands)),
        None => return installed_here(&crate::window::shell(), commands),
    };
    installed_result(commands, result)
}

fn is_ide_shim_str(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    if lower.contains(".windsurf") || lower.contains(".codeium") {
        return true;
    }
    lower.split(['/', '\\']).any(|seg| seg == "copilot-shim")
}

fn is_ide_shim_candidate(path: &std::path::Path) -> bool {
    if is_ide_shim_str(&path.to_string_lossy()) {
        return true;
    }
    if let Ok(link) = std::fs::read_link(path)
        && is_ide_shim_str(&link.to_string_lossy())
    {
        return true;
    }
    path.canonicalize().ok().is_some_and(|resolved| is_ide_shim_str(&resolved.to_string_lossy()))
}

fn installed_windows_path(commands: &[String]) -> Vec<String> {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|e| !e.is_empty())
        .map(str::to_owned)
        .collect();
    let dirs: Vec<std::path::PathBuf> = std::env::split_paths(&path_env).collect();
    let is_real_file = |p: &std::path::Path| p.is_file() && !is_ide_shim_candidate(p);
    commands
        .iter()
        .filter(|cmd| {
            let p = std::path::Path::new(cmd.as_str());
            if p.components().count() > 1 || p.is_absolute() {
                if p.extension().is_some() && is_real_file(p) {
                    return true;
                }
                return exts
                    .iter()
                    .any(|ext| is_real_file(&std::path::PathBuf::from(format!("{}{ext}", p.display()))));
            }
            dirs.iter().any(|dir| {
                let candidate = dir.join(cmd.as_str());
                if candidate.extension().is_some() && is_real_file(&candidate) {
                    return true;
                }
                exts.iter()
                    .any(|ext| is_real_file(&std::path::PathBuf::from(format!("{}{ext}", candidate.display()))))
            })
        })
        .cloned()
        .collect()
}

fn installed_here(shell: &str, commands: &[String]) -> Result<Vec<String>, String> {
    let limit = std::time::Duration::from_secs(30);
    let mode = if cfg!(target_os = "macos") { "-lic" } else { "-ic" };
    let line = installed_line(commands, base(shell) == "fish");
    installed_result(commands, crate::mods::run_blocking(shell, &[mode.into(), line], None, limit)).or_else(|err| {
        // A shell that can't run the check (nushell): sh searches the app's PATH,
        // which is where agents for such shells start (`window::unix_agent_argv`).
        let sh = crate::mods::run_blocking("/bin/sh", &["-c".into(), installed_line(commands, false)], None, limit);
        installed_result(commands, sh).map_err(|_| err)
    })
}

#[allow(dead_code)]
fn installed_result(commands: &[String], result: Result<(i32, String, String), String>) -> Result<Vec<String>, String> {
    let (code, out, err) = result?;
    if code != 0 || !out.lines().any(|line| line == "herdr-check-complete") {
        return Err(format!("could not check installed agents (exit {code}): {}", err.trim()));
    }
    let found: std::collections::HashSet<&str> = out.split("herdr-has:").skip(1).filter_map(|s| s.split_whitespace().next()).collect();
    Ok(commands.iter().filter(|c| found.contains(c.as_str())).cloned().collect())
}

#[allow(dead_code)]
fn installed_powershell(commands: &[String]) -> Vec<String> {
    let list = commands.iter().map(|c| powershell_quote(c)).collect::<Vec<_>>().join(",");
    let script = format!(
        "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); foreach($c in @({list})){{ $hits = @(Get-Command $c -All -ErrorAction SilentlyContinue | Where-Object {{ $s = \"$($_.Source) $($_.Target)\"; $s -notmatch '(?i)((^|[\\\\/])copilot-shim([\\\\/]|$)|\\.windsurf|\\.codeium)' }}); if($hits.Count -gt 0){{ \"herdr-has:$c\" }} }}; \"herdr-check-complete\""
    );
    vec!["-NoLogo".into(), "-Command".into(), script]
}

/// Check in the shell itself for functions, but not aliases (`"$@"` cannot run them),
/// and skip IDE placeholder shims such as VS Code's `copilot-shim` or Windsurf/Codeium wrappers.
fn installed_line(commands: &[String], fish: bool) -> String {
    if fish {
        let checks = commands
            .iter()
            .map(|c| {
                let word = format!("'{}'", c.replace('\'', "'\\''"));
                format!(
                    "if _herdr_has {word}; printf 'herdr-has:%s\\n' {word}; end;"
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        return format!(
            "function _herdr_shim; set -l rp (readlink -f -- $argv[1] 2>/dev/null; or readlink -- $argv[1] 2>/dev/null; or echo $argv[1]); string match -qi -r '(/copilot-shim/|\\.windsurf|\\.codeium)' -- \"/$argv[1]/|/$rp/\"; end; function _herdr_has; if functions -q -- $argv[1]; return 0; end; for p in (type -aP -- $argv[1] 2>/dev/null); if not _herdr_shim $p; return 0; end; end; return 1; end; {checks} printf '\\nherdr-check-complete\\n'"
        );
    }
    let checks = commands
        .iter()
        .map(|c| {
            let word = format!("'{}'", c.replace('\'', "'\\''"));
            format!("_herdr_has {word} && printf 'herdr-has:%s\\n' {word};")
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "_herdr_shim() {{ _rp=$(readlink -f \"$1\" 2>/dev/null || readlink \"$1\" 2>/dev/null || printf '%s' \"$1\"); case \"/$1/|/$_rp/\" in */copilot-shim/*|*/COPILOT-SHIM/*|*.windsurf*|*.codeium*) return 0 ;; esac; return 1; }}; _herdr_has() {{ unalias -- \"$1\" 2>/dev/null; _p=$(command -v -- \"$1\" 2>/dev/null) || return 1; case \"$_p\" in /*) _herdr_shim \"$_p\" || return 0 ;; *) return 0 ;; esac; _rest=$PATH:; while [ -n \"$_rest\" ]; do _d=${{_rest%%:*}}; _rest=${{_rest#*:}}; [ -n \"$_d\" ] || _d=.; if [ -f \"$_d/$1\" ] && [ -x \"$_d/$1\" ] && ! _herdr_shim \"$_d/$1\"; then return 0; fi; done; return 1; }}; {checks} printf '\\nherdr-check-complete\\n'"
    )
}

thread_local! {
    static TABLE: RefCell<Table> = RefCell::new(serde_json::from_str(BUILT_IN).expect("defaults/programs.json"));
}

pub fn path() -> std::path::PathBuf {
    crate::mods::app_dir().join("programs.json")
}

/// Writes the shipped icons into the home folder.
pub fn install_icons() {
    let dir = crate::mods::app_dir().join("icons");
    let _ = std::fs::create_dir_all(&dir);
    for (name, bytes) in DEFAULT_ICONS {
        let file = dir.join(name);
        if std::fs::read(&file).ok().as_deref() != Some(*bytes) {
            let _ = crate::persist::write_atomic(&file, bytes);
        }
    }
}

/// Reads the person's table on top of the built-in one. Err: what's wrong with their file.
pub fn load() -> Result<(), String> {
    let mut table: Table = serde_json::from_str(BUILT_IN).expect("defaults/programs.json");
    let result = match std::fs::read_to_string(path()) {
        Err(_) => Ok(()),
        Ok(text) => match serde_json::from_str::<Table>(&text) {
            Err(e) => Err(format!("programs.json: {e}")),
            Ok(mine) => {
                table.shells.extend(mine.shells);
                table.interpreters.extend(mine.interpreters);
                for (key, entry) in mine.programs {
                    table.programs.entry(key).or_default().over(entry);
                }
                Ok(())
            }
        },
    };
    TABLE.with(|t| *t.borrow_mut() = table);
    result
}

/// How a terminal shows: from the agent's own report (`app`), else the
/// program it runs (`argv`). `{ key, name, icon, tint, tile, color, shell, agent }`.
pub fn of(app: Option<&str>, argv: &[String]) -> Value {
    TABLE.with(|t| resolve(&t.borrow(), app, argv))
}

/// How the app calls an agent ("Kiro finished"), from its 7501 `app`: the
/// name `of` shows for it (the person's `programs.json` included), else the `app`.
pub fn name(app: &str) -> String {
    TABLE.with(|t| find(&t.borrow(), app).map(|(key, e)| e.name.clone().unwrap_or_else(|| key.to_owned()))).unwrap_or_else(|| app.to_owned())
}

fn base(s: &str) -> &str {
    let s = s.rsplit(['/', '\\']).next().unwrap_or(s);
    let s = s.trim_start_matches('-');
    // Windows: `pwsh.exe` is `pwsh`.
    match s.len().checked_sub(4) {
        Some(i) if s.is_char_boundary(i) && s[i..].eq_ignore_ascii_case(".exe") => &s[..i],
        _ => s,
    }
}

/// PowerShell's single quotes, which include the curly ones.
const PS_QUOTES: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'];

/// `& 'prog' 'arg 1'`: a command for PowerShell's `-Command`, each word quoted.
#[cfg_attr(not(test), allow(dead_code))]
pub fn powershell_line(cmd: &[String]) -> String {
    format!("& {}", cmd.iter().map(|w| powershell_quote(w)).collect::<Vec<_>>().join(" "))
}

fn powershell_quote(word: &str) -> String {
    let mut s = String::from("'");
    for ch in word.chars() {
        if PS_QUOTES.contains(&ch) {
            s.push(ch);
        }
        s.push(ch);
    }
    s + "'"
}

/// The words back from `powershell_line`.
pub(crate) fn powershell_words(line: &str) -> Option<Vec<String>> {
    let mut chars = line.strip_prefix("& ")?.chars().peekable();
    let mut words = Vec::new();
    while let Some(c) = chars.next() {
        if c == ' ' {
            continue;
        }
        if !PS_QUOTES.contains(&c) {
            return None;
        }
        let mut w = String::new();
        while let Some(ch) = chars.next() {
            if PS_QUOTES.contains(&ch) {
                if chars.peek().is_some_and(|n| PS_QUOTES.contains(n)) {
                    w.push(chars.next().unwrap_or(ch));
                    continue;
                }
                break;
            }
            w.push(ch);
        }
        words.push(w);
    }
    Some(words)
}

/// Formats `cmd` into a command line suitable for `cmd.exe /d /s /k "<line>"` or `/c "<line>"`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn cmd_line(cmd: &[String]) -> String {
    cmd.iter().map(|w| cmd_quote(w)).collect::<Vec<_>>().join(" ")
}

fn cmd_quote(word: &str) -> String {
    if word.is_empty() {
        return "\"\"".into();
    }
    let needs_quote = word.chars().any(|c| {
        c.is_whitespace() || matches!(c, '"' | '&' | '|' | '<' | '>' | '^' | '(' | ')' | '%' | '!')
    });
    if !needs_quote {
        return word.to_owned();
    }
    let mut out = String::from("\"");
    let mut slashes = 0usize;
    for ch in word.chars() {
        match ch {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            c => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

/// The words back from a `cmd.exe /k` or `/c` command string.
pub(crate) fn cmd_words(line: &str) -> Option<Vec<String>> {
    let mut raw = line.trim();
    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') && raw[1..raw.len() - 1].contains('"') {
        raw = &raw[1..raw.len() - 1];
    }
    let mut chars = raw.chars().peekable();
    let mut words = Vec::new();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut word = String::new();
        let mut in_quotes = false;
        while let Some(ch) = chars.next() {
            if in_quotes {
                if ch == '\\' {
                    let mut slashes = 1usize;
                    while chars.peek() == Some(&'\\') {
                        chars.next();
                        slashes += 1;
                    }
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        word.push_str(&"\\".repeat(slashes / 2));
                        if slashes % 2 == 1 {
                            word.push('"');
                        } else {
                            in_quotes = false;
                        }
                    } else {
                        word.push_str(&"\\".repeat(slashes));
                    }
                } else if ch == '"' {
                    in_quotes = false;
                } else {
                    word.push(ch);
                }
            } else if ch == '"' {
                in_quotes = true;
            } else if ch.is_whitespace() {
                break;
            } else {
                word.push(ch);
            }
        }
        words.push(word);
    }
    (!words.is_empty()).then_some(words)
}

fn find<'a>(t: &'a Table, name: &str) -> Option<(&'a str, &'a Entry)> {
    let lower = name.to_lowercase();
    let by_key = t.programs.iter().find(|(k, _)| k.as_str() == name || k.to_lowercase() == lower);
    let by_match = t.programs.iter().find(|(_, e)| e.also.iter().any(|a| a == name || a.to_lowercase() == lower));
    // An explicit key wins — including a person's own `programs.json` entry —
    // except the generic `agent` fallback, which yields to a `match` (Cursor's
    // CLI process is named `agent`).
    match (by_key, by_match) {
        (Some((k, _)), Some((mk, me))) if k.as_str() == "agent" => Some((mk.as_str(), me)),
        (Some((k, e)), _) => Some((k.as_str(), e)),
        (None, Some((k, e))) => Some((k.as_str(), e)),
        (None, None) => None,
    }
}

fn is_agent_entry(key: &str, e: &Entry) -> bool {
    !e.run.is_empty() || key == "agent"
}

fn strip_real_suffix(s: &str) -> Option<&str> {
    let i = s.len().checked_sub(5)?;
    (s.is_char_boundary(i) && s[i..].eq_ignore_ascii_case(".real")).then_some(&s[..i])
}

fn find_with_real<'a>(t: &'a Table, name: &str) -> Option<(&'a str, &'a Entry)> {
    find(t, name).or_else(|| strip_real_suffix(name).and_then(|stem| find(t, stem)))
}

fn find_agent_segment<'a>(t: &'a Table, seg: &str) -> Option<(&'a str, &'a Entry)> {
    let seg = [".exe", ".cmd", ".bat", ".ps1", ".js", ".mjs", ".cjs", ".ts", ".py", ".rb"]
        .iter()
        .find_map(|ext| seg.strip_suffix(ext))
        .unwrap_or(seg);
    let seg = strip_real_suffix(seg).unwrap_or(seg);
    let trimmed = seg.strip_suffix("-cli").or_else(|| seg.strip_suffix("-ai"));
    for candidate in std::iter::once(seg).chain(trimmed) {
        if let Some((key, e)) = find(t, candidate).filter(|(k, e)| is_agent_entry(k, e)) {
            return Some((key, e));
        }
    }
    None
}

fn find_agent_in_path<'a>(t: &'a Table, path: &str) -> Option<(&'a str, &'a Entry)> {
    let segs: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let last = *segs.last()?;
    if let Some(hit) = find_agent_segment(t, last) {
        return Some(hit);
    }
    if segs.len() >= 3 && segs[segs.len() - 2].eq_ignore_ascii_case("versions")
        && let Some(hit) = find_agent_segment(t, segs[segs.len() - 3])
    {
        return Some(hit);
    }
    // Bundled versioned runtimes such as `<home>\AppData\Local\cursor-agent\versions\<ver>\index.js`
    // or `...\cursor-agent\versions\<ver>\node.exe`.
    for i in (1..segs.len().saturating_sub(1)).rev() {
        if segs[i].eq_ignore_ascii_case("versions")
            && segs.len().saturating_sub(i) <= 4
            && let Some(hit) = find_agent_segment(t, segs[i - 1])
        {
            return Some(hit);
        }
    }
    let last_stem = [".js", ".mjs", ".cjs", ".ts", ".py", ".rb"].iter().find_map(|ext| last.strip_suffix(ext)).unwrap_or(last);
    let generic_entry = matches!(last_stem.to_ascii_lowercase().as_str(), "cli" | "index" | "main" | "run" | "bin");
    for (i, seg) in segs[..segs.len().saturating_sub(1)].iter().enumerate().rev() {
        if seg.starts_with('@') || matches!(seg.to_ascii_lowercase().as_str(), "bin" | "dist" | "build" | "lib" | "src" | "node_modules") {
            continue;
        }
        let in_node_modules = segs[..i].iter().any(|s| *s == "node_modules" || s.starts_with('@'));
        let near_generic_entry = generic_entry && segs.len().saturating_sub(i) <= 3;
        if (in_node_modules || near_generic_entry) && let Some(hit) = find_agent_segment(t, seg) {
            return Some(hit);
        }
    }
    None
}

fn resolve(t: &Table, app: Option<&str>, argv: &[String]) -> Value {
    if let Some(app) = app.filter(|a| !a.is_empty()) {
        return match find_with_real(t, app) {
            Some((key, e)) => shown(key, e, None, false, true),
            None => shown("agent", t.programs.get("agent").unwrap_or(&Entry::default()), Some(app), false, true),
        };
    }
    // The app starts commands in the person's shell: `zsh -ic '"$@"; exec "$0" -i' zsh <cmd>...`,
    // or fish's `command $argv[2..]; exec $argv[1] -i`; on macOS both as login shells (`-lic`, `-li`).
    if argv.len() > 4
        && t.shells.iter().any(|s| s == base(&argv[0]))
        && argv[1].starts_with('-')
        && argv[1].ends_with('c')
        && (argv[2].contains("\"$@\"") || argv[2] == crate::remote_shell::FISH_EXEC || matches!(argv[2].as_str(), "command $argv[2..]; exec $argv[1] -i" | "command $argv[2..]; exec $argv[1] -li"))
    {
        return resolve(t, None, &argv[4..]);
    }
    // On Windows: `pwsh [-NoLogo] [-ExecutionPolicy Bypass] [-NoExit] -Command "& 'codex' ..."`.
    if argv.len() >= 3
        && t.shells.iter().any(|s| s == base(&argv[0]))
        && matches!(base(&argv[0]), "pwsh" | "powershell")
        && argv[argv.len() - 2].eq_ignore_ascii_case("-Command")
        && let Some(words) = powershell_words(&argv[argv.len() - 1])
    {
        return resolve(t, None, &words);
    }
    // On Windows: `cmd.exe [/d] [/s] /k|/c "claude --dangerously-skip-permissions"`.
    if argv.len() >= 3
        && base(&argv[0]).eq_ignore_ascii_case("cmd")
        && let Some(pos) = argv[1..].iter().position(|a| a.eq_ignore_ascii_case("/k") || a.eq_ignore_ascii_case("/c"))
    {
        let rest = &argv[pos + 2..];
        let words = if rest.len() == 1 {
            cmd_words(&rest[0])
        } else if !rest.is_empty() {
            Some(rest.to_vec())
        } else {
            None
        };
        if let Some(words) = words {
            return resolve(t, None, &words);
        }
    }
    let Some(first) = argv.first().map(|a| base(a)) else {
        return shown("shell", t.programs.get("shell").unwrap_or(&Entry::default()), None, true, false);
    };
    // `node .../vite.js dev`, `npx vite`, `uv run pytest`: the program they run, when it's known.
    if t.interpreters.iter().any(|i| i == first) {
        for arg in argv[1..].iter().filter(|a| !a.starts_with('-')) {
            // A script's name without its extension (`vite.js`), never a module path (`http.server`).
            let name = base(arg);
            let stem = [".js", ".mjs", ".cjs", ".ts", ".py", ".rb"].iter().find_map(|x| name.strip_suffix(x)).unwrap_or(name);
            if let Some((key, e)) = find_with_real(t, stem).filter(|(k, _)| !t.interpreters.iter().any(|i| i == k)) {
                return shown(key, e, None, false, is_agent_entry(key, e));
            }
            if (arg.contains('/') || arg.contains('\\'))
                && let Some((key, e)) = find_agent_in_path(t, arg)
            {
                return shown(key, e, None, false, true);
            }
        }
        // Bundled interpreters shipped inside an agent's own version directory
        // (e.g. `...\cursor-agent\versions\<ver>\node.exe`).
        if (argv[0].contains('/') || argv[0].contains('\\'))
            && let Some((key, e)) = find_agent_in_path(t, &argv[0])
        {
            return shown(key, e, None, false, true);
        }
    }
    if let Some((key, e)) = find_with_real(t, first) {
        return shown(key, e, None, false, is_agent_entry(key, e));
    }
    if !t.shells.iter().any(|s| s == first)
        && (argv[0].contains('/') || argv[0].contains('\\'))
        && let Some((key, e)) = find_agent_in_path(t, &argv[0])
    {
        return shown(key, e, None, false, true);
    }
    if t.shells.iter().any(|s| s == first) {
        return shown("shell", t.programs.get("shell").unwrap_or(&Entry::default()), Some(first), true, false);
    }
    let mut e = t.programs.get("program").cloned().unwrap_or_default();
    e.tile = Some(hash_color(first).into());
    e.color = Some("#ffffff".into());
    shown("program", &e, Some(first), false, false)
}

fn shown(key: &str, e: &Entry, name: Option<&str>, shell: bool, agent: bool) -> Value {
    json!({
        "key": key,
        "name": name.map(str::to_owned).or_else(|| e.name.clone()).unwrap_or_else(|| key.to_owned()),
        "icon": e.icon,
        "tint": e.tint.unwrap_or(true),
        "tile": e.tile,
        "color": e.color,
        "shell": shell,
        "agent": agent,
    })
}

/// A color for a program nobody listed, the same every time.
fn hash_color(s: &str) -> &'static str {
    const PALETTE: [&str; 8] = ["#6d5bd0", "#2f80ed", "#0f9d8a", "#d9822b", "#c2417d", "#3a7d44", "#8a5a44", "#4e5d94"];
    let h = s.bytes().fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
    PALETTE[(h % PALETTE.len() as u32) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Table {
        serde_json::from_str(BUILT_IN).unwrap()
    }
    fn key(app: Option<&str>, argv: &[&str]) -> String {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        resolve(&t(), app, &argv)["key"].as_str().unwrap().to_owned()
    }

    #[test]
    fn powershell_words_survive_quotes() {
        let words: Vec<String> = ["pi", "it's \u{2019}fine\u{2019}", "", "a  b"].map(String::from).to_vec();
        let line = powershell_line(&words);
        assert_eq!(line, "& 'pi' 'it''s \u{2019}\u{2019}fine\u{2019}\u{2019}' '' 'a  b'");
        assert_eq!(powershell_words(&line), Some(words));
    }

    #[test]
    fn cmd_words_round_trip_and_resolve_windows_wrapped_agents() {
        let words: Vec<String> = ["claude", "--dangerously-skip-permissions", "fix \"bug\" & test", "C:\\work\\dir\\", ""].map(String::from).to_vec();
        let line = cmd_line(&words);
        assert_eq!(cmd_words(&line), Some(words.clone()));

        assert_eq!(key(None, &["C:\\Windows\\System32\\cmd.exe", "/d", "/s", "/k", &line]), "claude");
        assert_eq!(key(None, &["cmd.exe", "/d", "/s", "/c", "codex --no-daemon"]), "codex");
        assert_eq!(key(None, &["C:\\Windows\\System32\\cmd.exe"]), "shell");
        assert_eq!(
            key(None, &["C:\\Program Files\\Git\\bin\\bash.exe", "-lic", "\"$@\"; exec \"$0\" -li", "C:\\Program Files\\Git\\bin\\bash.exe", "claude", "--dangerously-skip-permissions"]),
            "claude"
        );
        assert_eq!(
            key(None, &["pwsh.exe", "-ExecutionPolicy", "Bypass", "-Command", "& 'codex' '--no-daemon'"]),
            "codex"
        );
    }

    #[test]
    fn powershell_checks_keep_command_names_literal() {
        let argv = installed_powershell(&["claude".into(), "missing'command; $x".into(), "O’Brien".into()]);
        assert_eq!(argv[..2], ["-NoLogo", "-Command"]);
        assert!(argv[2].contains("@('claude','missing''command; $x','O’’Brien')"));
        assert!(argv[2].contains("[Console]::OutputEncoding"));
    }

    #[cfg(windows)]
    #[test]
    fn powershell_finds_installed_commands() {
        let commands = ["cmd.exe".into(), "herdr_missing_agent_440".into()];
        let result = crate::mods::run_blocking("powershell.exe", &installed_powershell(&commands), None, std::time::Duration::from_secs(30));
        assert_eq!(installed_result(&commands, result).unwrap(), ["cmd.exe"]);
    }

    #[test]
    fn agents_shells_tools_and_what_interpreters_run() {
        assert_eq!(key(Some("claude-code"), &["/bin/zsh"]), "claude");
        assert_eq!(key(Some("grok-build"), &["/bin/zsh"]), "grok");
        assert_eq!(key(Some("dsh-tui"), &["/bin/zsh"]), "dsh-tui");
        assert_eq!(key(None, &["dsh-tui"]), "dsh-tui");
        assert_eq!(key(Some("goose"), &["/bin/zsh"]), "goose");
        assert_eq!(key(None, &["goose"]), "goose");
        assert_eq!(key(None, &["agent"]), "cursor");
        assert_eq!(key(Some("agent"), &[]), "cursor");
        assert_eq!(key(Some("cursor-agent"), &[]), "cursor");
        assert_eq!(key(Some("newagent"), &[]), "agent");
        // ZCode calls its process `zcode-cli`; its command is `zcode`.
        assert_eq!(key(None, &["zcode-cli"]), "zcode");
        assert_eq!(key(Some("zcode-cli"), &[]), "zcode");
        assert_eq!(key(None, &["zcode"]), "zcode");
        assert!(startable().iter().any(|a| a["key"] == "cursor" && a["run"] == json!(["cursor-agent", "--yolo", "--trust"])));
        assert!(startable().iter().any(|a| a["key"] == "zcode" && a["run"] == json!(["zcode", "--mode", "yolo"])));
        assert!(startable().iter().any(|a| a["key"] == "claude" && a["run"] == json!(["claude", "--dangerously-skip-permissions"])));
        assert!(startable().iter().any(|a| a["key"] == "codex" && a["run"] == json!(["codex", "--no-daemon", "--dangerously-bypass-approvals-and-sandbox"])));
        assert!(startable().iter().any(|a| a["key"] == "opencode" && a["run"] == json!(["opencode", "--auto"])));
        assert!(!startable().iter().any(|a| a["key"] == "gemini"));
        // A person's own key still beats a built-in `match` (python matches python3).
        let mut personal = t();
        personal.programs.insert(
            "python3".into(),
            Entry { name: Some("Project Python".into()), ..Entry::default() },
        );
        assert_eq!(resolve(&personal, None, &["python3".into()])["key"], "python3");
        assert_eq!(resolve(&personal, None, &["python3".into()])["name"], "Project Python");
        assert_eq!(key(None, &["/bin/zsh"]), "shell");
        assert_eq!(key(None, &["-zsh"]), "shell");
        assert_eq!(key(None, &["nvim", "x.rs"]), "nvim");
        assert_eq!(key(None, &["vi"]), "vim");
        assert_eq!(key(None, &["node", "/x/node_modules/.bin/vite", "dev"]), "vite");
        assert_eq!(key(None, &["npx", "vitest"]), "vitest");
        assert_eq!(key(None, &["node", "server.js"]), "node");
        assert_eq!(key(None, &["python3", "-m", "http.server", "8799"]), "python");
        assert_eq!(key(None, &["uv", "run", "pytest"]), "pytest");
        assert_eq!(key(None, &["mytool"]), "program");
        assert_eq!(key(None, &["/bin/zsh", "-ic", "\"$@\"; exec \"$0\" -i", "/bin/zsh", "vi", "x"]), "vim");
        assert_eq!(key(None, &["/usr/bin/fish", "-ic", "command $argv[2..]; exec $argv[1] -i", "/usr/bin/fish", "vi", "x"]), "vim");
        // macOS starts agents from a login shell (`-lic`, then `exec fish -li`).
        assert_eq!(key(None, &["/bin/zsh", "-lic", "\"$@\"; exec \"$0\" -li", "/bin/zsh", "cursor-agent"]), "cursor");
        assert_eq!(key(None, &["/opt/homebrew/bin/fish", "-lic", "command $argv[2..]; exec $argv[1] -li", "/opt/homebrew/bin/fish", "cursor-agent"]), "cursor");
        let line = powershell_line(&["codex".into(), "fix it".into()]);
        assert_eq!(key(None, &["pwsh.exe", "-NoExit", "-Command", &line]), "codex");
        assert_eq!(key(None, &[r"C:\Program Files\PowerShell\7\pwsh.exe", "-NoExit", "-Command", &line]), "codex");
        assert_eq!(key(None, &[r"C:\Program Files\PowerShell\7\pwsh.exe"]), "shell");
        assert_eq!(key(None, &[r"C:\Windows\powershell.exe"]), "shell");

        let is_agent = |argv: &[&str]| {
            let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
            resolve(&t(), None, &argv)["agent"].as_bool().unwrap()
        };
        assert!(is_agent(&["claude"]));
        assert!(is_agent(&["codex"]));
        assert!(is_agent(&["opencode"]));
        assert!(is_agent(&["agent"]));
        assert!(is_agent(&["zcode"]));
        assert!(is_agent(&["zcode-cli"]));
        assert!(!is_agent(&["vim"]));
        assert!(!is_agent(&["/bin/zsh"]));
        assert_eq!(key(None, &["node", "/home/u/.zcode/server/releases/3.19.1/runtime/zcode.cjs"]), "zcode");
        assert!(is_agent(&["node", "/home/u/.zcode/server/releases/3.19.1/runtime/zcode.cjs"]));
        assert_eq!(key(None, &["node", "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"]), "claude");
        assert!(is_agent(&["node", "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"]));
        assert_eq!(key(None, &[r"C:\Program Files\nodejs\node.exe", r"C:\Users\u\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.js"]), "codex");
        assert_eq!(key(None, &["/home/u/.local/share/claude/versions/2.1.325"]), "claude");
        assert!(is_agent(&["/home/u/.local/share/claude/versions/2.1.325"]));
        assert_eq!(
            key(
                None,
                &[
                    r"D:\Users\u\AppData\Local\cursor-agent\versions\2026.10.22-27dcf5f\node.exe",
                    r"D:\Users\u\AppData\Local\cursor-agent\versions\2026.10.22-27dcf5f\index.js",
                    "--yolo",
                    "--trust",
                ],
            ),
            "cursor"
        );
        assert!(is_agent(&[
            r"D:\Users\u\AppData\Local\cursor-agent\versions\2026.10.22-27dcf5f\node.exe",
            r"D:\Users\u\AppData\Local\cursor-agent\versions\2026.10.22-27dcf5f\index.js",
            "--yolo",
            "--trust",
        ]));
        assert_eq!(
            key(None, &[r"D:\Users\u\AppData\Local\cursor-agent\versions\2026.10.22-27dcf5f\node.exe"]),
            "cursor"
        );
        assert!(is_agent(&[
            "powershell.exe",
            "-ExecutionPolicy",
            "Bypass",
            "-NoExit",
            "-Command",
            "& 'cursor-agent' '--yolo' '--trust'",
        ]));
        assert_eq!(key(None, &[r"C:\Users\u\AppData\Roaming\npm\codex.cmd"]), "codex");
        assert_eq!(key(None, &["node", "/home/pi/server.js"]), "node");
        assert_eq!(key(None, &["/home/pi/bin/node", "server.js"]), "node");
        assert_eq!(
            key(None, &["powershell.exe", "-ExecutionPolicy", "Bypass", "-NoExit", "-Command", "& 'claude' '--dangerously-skip-permissions'"]),
            "claude"
        );
        assert!(is_agent(&["powershell.exe", "-ExecutionPolicy", "Bypass", "-NoExit", "-Command", "& 'claude' '--dangerously-skip-permissions'"]));
        // Wrapped `.real` binaries (e.g. proxy wrappers exec'ing `agy.real` / `claude.real`) resolve as their agent.
        assert_eq!(key(None, &["agy.real"]), "antigravity");
        assert!(is_agent(&["agy.real"]));
        assert_eq!(key(None, &["/home/u/.local/bin/agy.real", "--dangerously-skip-permissions"]), "antigravity");
        assert!(is_agent(&["/home/u/.local/bin/agy.real", "--dangerously-skip-permissions"]));
        assert_eq!(resolve(&t(), None, &["/home/u/.local/bin/agy.real".into()])["name"], "Antigravity");
        assert_eq!(key(None, &[r"C:\Users\u\agy.real.exe"]), "antigravity");
        assert!(is_agent(&[r"C:\Users\u\agy.real.exe"]));
        assert_eq!(key(None, &["/home/u/.local/bin/claude.real"]), "claude");
        assert!(is_agent(&["/home/u/.local/bin/claude.real"]));
        assert_eq!(key(None, &["mytool.real"]), "program");
        assert!(!is_agent(&["mytool.real"]));
    }

    #[cfg(unix)]
    #[test]
    fn installed_checks_functions_and_quoted_names_but_not_aliases() {
        let commands = ["issue57_function", "issue57_alias", "printf", "missing'command; false"].map(String::from);
        let line = installed_line(&commands, false);
        let out = std::process::Command::new("sh")
            .args(["-c", &format!("issue57_function() {{ :; }}; alias issue57_alias='printf alias'; {line}")])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(installed_result(&commands, Ok((0, String::from_utf8(out.stdout).unwrap(), String::new()))).unwrap(), ["issue57_function", "printf"]);
    }

    #[cfg(unix)]
    #[test]
    fn installed_skips_vscode_copilot_shim_and_finds_real_copilot_later_on_path() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = if cfg!(target_os = "linux") { std::path::PathBuf::from("/var/tmp") } else { std::env::temp_dir() };
        let dir = temp.join(format!("herdr-copilot-shim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let shim_dir = dir.join("share/code/bin/copilot-shim");
        let usr_bin = dir.join("usr/bin");
        let real_bin = dir.join("local/bin");
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::create_dir_all(&usr_bin).unwrap();
        std::fs::create_dir_all(&real_bin).unwrap();

        let shim_bin = shim_dir.join("copilot");
        std::fs::write(&shim_bin, "#!/bin/sh\nexit 127\n").unwrap();
        std::fs::set_permissions(&shim_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&shim_bin, usr_bin.join("copilot")).unwrap();

        let commands = ["copilot".to_string()];
        let line = installed_line(&commands, false);

        // Shim-only PATH: copilot must NOT be reported as installed.
        let out_shim_only = std::process::Command::new("sh")
            .args(["-c", &line])
            .env("PATH", format!("{}:/usr/bin:/bin", usr_bin.display()))
            .output()
            .unwrap();
        assert!(out_shim_only.status.success());
        assert_eq!(
            installed_result(&commands, Ok((0, String::from_utf8(out_shim_only.stdout).unwrap(), String::new()))).unwrap(),
            Vec::<String>::new()
        );

        // Real copilot later on PATH: must skip the shim and detect the real binary.
        let real_copilot = real_bin.join("copilot");
        std::fs::write(&real_copilot, "#!/bin/sh\necho real\n").unwrap();
        std::fs::set_permissions(&real_copilot, std::fs::Permissions::from_mode(0o755)).unwrap();

        let out_with_real = std::process::Command::new("sh")
            .args(["-c", &line])
            .env("PATH", format!("{}:{}:/usr/bin:/bin", usr_bin.display(), real_bin.display()))
            .output()
            .unwrap();
        assert!(out_with_real.status.success());
        assert_eq!(
            installed_result(&commands, Ok((0, String::from_utf8(out_with_real.stdout).unwrap(), String::new()))).unwrap(),
            vec!["copilot".to_string()]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_check_runs_in_the_persons_shell_and_falls_back_to_sh_only_when_it_cannot() {
        use std::os::unix::fs::PermissionsExt;
        let temp = if cfg!(target_os = "linux") { std::path::PathBuf::from("/var/tmp") } else { std::env::temp_dir() };
        let dir = temp.join(format!("herdr-other-shell-check-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("rc")).unwrap();
        let script = |name: &str, body: &str| {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.to_string_lossy().into_owned()
        };
        // Only the person's shell can find it: its rc adds `rc` to PATH.
        script("rc/issue240-rc-agent", "");
        // A sh-compatible shell under a name nothing knows; and nushell, which rejects sh syntax.
        let posix = script("mksh-custom", &format!("PATH={}:$PATH exec /bin/sh \"$@\"", dir.join("rc").display()));
        let nu = script("nu", "echo \"Error: nu::parser::shell_outerr\" >&2\nexit 1");
        let commands = ["sh", "issue240-rc-agent", "issue240-missing-agent"].map(String::from);
        let (posix, nu) = (installed_here(&posix, &commands), installed_here(&nu, &commands));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(posix.unwrap(), ["sh", "issue240-rc-agent"]);
        assert_eq!(nu.unwrap(), ["sh"]);
    }

    #[test]
    fn installed_check_failures_are_not_missing_agents() {
        let commands = ["claude".into()];
        assert_eq!(installed_result(&commands, Ok((0, "\nherdr-check-complete\n".into(), String::new()))).unwrap(), Vec::<String>::new());
        assert!(installed_result(&commands, Err("machine offline".into())).unwrap_err().contains("machine offline"));
        for (code, output) in [(1, "herdr-check-complete\n"), (0, ""), (0, "herdr-has:claude\n")] {
            assert!(installed_result(&commands, Ok((code, output.into(), "shell failed".into()))).is_err());
        }
    }

    #[test]
    fn every_entry_has_its_icon() {
        for (k, e) in t().programs {
            let icon = e.icon.unwrap_or_else(|| panic!("{k} has no icon"));
            let file = icon.strip_prefix("icons/").unwrap();
            assert!(DEFAULT_ICONS.iter().any(|(n, _)| *n == file), "{k}: {icon} not shipped");
        }
    }

    #[test]
    fn startable_includes_antigravity_among_top_agents() {
        let items = startable();
        let keys: Vec<&str> = items.iter().filter_map(|v| v["key"].as_str()).collect();
        let agy_pos = keys.iter().position(|k| *k == "antigravity").expect("antigravity in startable");
        let pi_pos = keys.iter().position(|k| *k == "pi").expect("pi in startable");
        assert!(agy_pos < pi_pos, "antigravity should appear before pi in {keys:?}");
    }
}
