//! Slash-command and skill discovery for ACP agents (fallback when an adapter does not emit
//! `available_commands_update`) and interactive PTY CLI agents in structured chat mode.

use crate::acp::presets;
use crate::acp::proto::AcpAvailableCommand;
use crate::pty_adapters::PtyCliKind;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_DISCOVERED: usize = 256;
const MAX_READ_BYTES: u64 = 16 * 1024;
const MAX_DESC_CHARS: usize = 160;

/// Normalizes an agent preset, command argv, and optional app hint into a canonical CLI family key.
pub fn detect_agent_kind(preset: Option<&str>, argv: &[String], app_hint: Option<&str>) -> String {
    if let Some(p) = preset.map(str::trim).filter(|s| !s.is_empty()) {
        return normalize_kind_token(p);
    }
    let pty_kind = PtyCliKind::detect(argv);
    if pty_kind != PtyCliKind::Generic {
        return pty_kind.as_str().to_string();
    }
    if presets::is_claude_agent(None, argv) {
        return "claude".into();
    }
    if presets::is_codex_agent(None, argv) {
        return "codex".into();
    }
    if presets::is_agy_agent(None, argv) {
        return "agy".into();
    }
    if presets::is_cursor_agent(None, argv) {
        return "cursor".into();
    }
    for arg in argv {
        let lower = arg.replace('\\', "/").to_ascii_lowercase();
        let stem = lower.rsplit('/').next().unwrap_or(&lower);
        let stem = stem
            .strip_suffix(".exe")
            .or_else(|| stem.strip_suffix(".cmd"))
            .or_else(|| stem.strip_suffix(".bat"))
            .or_else(|| stem.strip_suffix(".ps1"))
            .unwrap_or(stem);
        match stem {
            "claude" | "claude-code" | "claude-agent-acp" => return "claude".into(),
            "codex" | "codex-acp" => return "codex".into(),
            "agy" | "antigravity" | "antigravity-cli" => return "agy".into(),
            "gemini" | "gemini-cli" => return "gemini".into(),
            "opencode" => return "opencode".into(),
            "cursor" | "cursor-agent" => return "cursor".into(),
            "copilot" => return "copilot".into(),
            "kimi" => return "kimi".into(),
            "qwen" => return "qwen".into(),
            "pi" | "pi-acp" => return "pi".into(),
            "hermes" | "hermes-acp" => return "hermes".into(),
            "glm" | "glm-acp-agent" => return "glm".into(),
            "zcode" | "zcode-cli" | "zcode-acp-server" => return "zcode".into(),
            "dsh" => return "dsh".into(),
            _ => {}
        }
    }
    if let Some(app) = app_hint.map(str::trim).filter(|s| !s.is_empty()) {
        let norm = normalize_kind_token(app);
        if norm != "generic" {
            return norm;
        }
    }
    "generic".into()
}

fn normalize_kind_token(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.contains("claude") {
        "claude".into()
    } else if lower.contains("codex") {
        "codex".into()
    } else if lower == "agy" || lower.contains("antigravity") {
        "agy".into()
    } else if lower.contains("gemini") {
        "gemini".into()
    } else if lower.contains("cursor") {
        "cursor".into()
    } else if lower.contains("opencode") {
        "opencode".into()
    } else if lower.contains("copilot") {
        "copilot".into()
    } else if lower.contains("kimi") {
        "kimi".into()
    } else if lower.contains("qwen") {
        "qwen".into()
    } else if lower == "pi" || lower.starts_with("pi-") {
        "pi".into()
    } else if lower.contains("hermes") {
        "hermes".into()
    } else if lower.contains("zcode") {
        "zcode".into()
    } else if lower.contains("glm") {
        "glm".into()
    } else if lower == "dsh" {
        "dsh".into()
    } else {
        "generic".into()
    }
}

/// Ensures every command has a normalized `source` category (`command` / `skill` / `mcp`).
pub fn normalize_command_sources(cmds: &[AcpAvailableCommand]) -> Vec<AcpAvailableCommand> {
    cmds.iter()
        .cloned()
        .map(|mut c| {
            if c.source.is_none() {
                c.source = Some(infer_source(&c.name).into());
            }
            c
        })
        .collect()
}

fn infer_source(name: &str) -> &'static str {
    if name.starts_with('$') {
        "skill"
    } else if name.starts_with("mcp:") || name.ends_with(" (MCP)") {
        "mcp"
    } else {
        "command"
    }
}

/// Resolves the command/skill list for an ACP agent: prefers live `available_commands_update`
/// advertised by the adapter, falling back to preset built-ins + disk skills/commands when empty.
pub fn resolve_acp_commands(
    live: &[AcpAvailableCommand],
    preset: Option<&str>,
    argv: &[String],
    cwd: &str,
) -> Vec<AcpAvailableCommand> {
    if !live.is_empty() {
        return normalize_command_sources(live);
    }
    let kind = detect_agent_kind(preset, argv, None);
    discover_commands(&kind, cwd)
}

/// Discovers built-in slash commands plus project-local and user-global custom commands & skills
/// for `agent_kind` rooted at `cwd`.
pub fn discover_commands(agent_kind: &str, cwd: &str) -> Vec<AcpAvailableCommand> {
    let home = user_home();
    discover_commands_with_home(agent_kind, Path::new(cwd), home.as_deref())
}

pub(crate) fn discover_commands_with_home(
    agent_kind: &str,
    cwd: &Path,
    home: Option<&Path>,
) -> Vec<AcpAvailableCommand> {
    let kind = normalize_kind_token(agent_kind);
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut push_unique = |cmd: AcpAvailableCommand| {
        if out.len() >= MAX_DISCOVERED {
            return;
        }
        let key = cmd.name.trim().to_ascii_lowercase();
        if key.is_empty() || !seen.insert(key) {
            return;
        }
        out.push(cmd);
    };

    // 1. Built-in CLI slash commands first so primary CLI commands stay immediately reachable.
    for cmd in builtin_commands_for(&kind) {
        push_unique(cmd);
    }

    // 2. Project-local & user-global custom slash commands (markdown files).
    for dir in command_dirs_for(&kind, cwd, home) {
        for cmd in scan_command_dir(&dir) {
            push_unique(cmd);
        }
    }

    // 3. Project-local & user-global installed skills (`*/SKILL.md`).
    let prefix_dollar = kind == "codex";
    for dir in skill_dirs_for(&kind, cwd, home) {
        for cmd in scan_skills_dir(&dir, prefix_dollar) {
            push_unique(cmd);
        }
    }

    out
}

fn user_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn cmd(name: &str, description: &str, input_hint: Option<&str>) -> AcpAvailableCommand {
    AcpAvailableCommand {
        name: name.into(),
        description: description.into(),
        input_hint: input_hint.map(String::from),
        source: Some("command".into()),
    }
}

fn builtin_commands_for(kind: &str) -> Vec<AcpAvailableCommand> {
    match kind {
        "claude" => vec![
            cmd("compact", "Compact conversation with optional focus instructions", Some("[instructions]")),
            cmd("clear", "Clear conversation history and free up context", None),
            cmd("plan", "Switch to plan mode for structured design before editing", None),
            cmd("model", "Switch or inspect the active Claude model", Some("[model]")),
            cmd("effort", "Set thinking effort level", Some("[low|medium|high|max]")),
            cmd("fast", "Toggle fast output mode", None),
            cmd("review", "Review code changes or pull request", Some("[instructions]")),
            cmd("diff", "Show current git diff in the workspace", None),
            cmd("mcp", "Inspect or manage MCP servers", None),
            cmd("memory", "View or edit CLAUDE.md memory files", None),
            cmd("status", "Show session status, version, and account info", None),
            cmd("context", "Visualize current context window usage", None),
            cmd("cost", "Show token usage and session cost", None),
            cmd("init", "Initialize a project CLAUDE.md guide", None),
            cmd("help", "Show available slash commands and shortcuts", None),
            cmd("resume", "Resume a previous conversation", Some("[session]")),
            cmd("rewind", "Rewind conversation or workspace changes", None),
            cmd("rename", "Rename the current session", Some("<name>")),
            cmd("permissions", "View or update tool permission rules", None),
            cmd("config", "Open or inspect Claude Code settings", None),
            cmd("agents", "Manage custom subagents", None),
            cmd("skills", "List available skills", None),
            cmd("hooks", "Inspect configured lifecycle hooks", None),
            cmd("export", "Export conversation transcript", Some("[filename]")),
            cmd("doctor", "Check the health of your Claude Code installation", None),
        ],
        "codex" => vec![
            cmd("plan", "Turn plan mode on", None),
            cmd("compact", "Summarize conversation to avoid hitting the context limit", None),
            cmd("review", "Review uncommitted changes, or review with custom instructions", Some("[instructions]")),
            cmd("review-branch", "Review changes relative to a base branch", Some("<branch>")),
            cmd("review-commit", "Review a specific commit", Some("<commit sha>")),
            cmd("diff", "Show git diff (including untracked files)", None),
            cmd("status", "Display session configuration and token usage", None),
            cmd("model", "Choose what model and reasoning effort to use", Some("[model]")),
            cmd("mcp", "Show the status of MCP servers, or reconnect them", Some("[status|reconnect]")),
            cmd("skills", "List available skills", None),
            cmd("goal", "Set a goal to keep pursuing", Some("[<objective>|clear|pause|resume]")),
            cmd("new", "Start a new chat during a conversation", None),
            cmd("resume", "Resume a saved chat", None),
            cmd("init", "Create an AGENTS.md file with instructions for Codex", None),
            cmd("approvals", "Choose what Codex can do without approval", None),
            cmd("permissions", "Choose sandbox and approval permissions", None),
            cmd("rename", "Rename the current session", Some("<new name>")),
            cmd("mention", "Attach a file mention", Some("<path>")),
            cmd("logout", "Sign out of Codex", None),
        ],
        "agy" => vec![
            cmd("plan", "Switch to planning mode", None),
            cmd("compact", "Compact conversation context", None),
            cmd("clear", "Clear conversation history", None),
            cmd("model", "Switch active model", Some("[model]")),
            cmd("review", "Review current workspace changes", Some("[instructions]")),
            cmd("mcp", "Inspect configured MCP servers", None),
            cmd("skills", "List available skills", None),
            cmd("status", "Show current session status", None),
            cmd("help", "Show available commands", None),
        ],
        "gemini" => vec![
            cmd("help", "Display help on available commands", None),
            cmd("clear", "Clear the screen and conversation history", None),
            cmd("compress", "Compress chat context to save tokens", None),
            cmd("chat", "Save, resume, or list conversation checkpoints", Some("[save|resume|list]")),
            cmd("memory", "Manage GEMINI.md instructional context", Some("[show|refresh|add]")),
            cmd("mcp", "List configured MCP servers and tools", None),
            cmd("tools", "List available Gemini CLI tools", None),
            cmd("stats", "Check session token and model statistics", None),
            cmd("model", "Switch active Gemini model", Some("[model]")),
            cmd("init", "Analyze the project and create a GEMINI.md file", None),
            cmd("restore", "Restore project files to a previous checkpoint", None),
            cmd("settings", "View and edit Gemini CLI settings", None),
        ],
        "opencode" => vec![
            cmd("help", "Show available commands", None),
            cmd("clear", "Start a new session", None),
            cmd("compact", "Compact the current session", None),
            cmd("models", "List or switch available models", None),
            cmd("init", "Create or update AGENTS.md", None),
            cmd("review", "Review code changes", Some("[instructions]")),
            cmd("undo", "Undo the last message and file edits", None),
            cmd("redo", "Redo a previously undone change", None),
            cmd("sessions", "List and switch sessions", None),
            cmd("status", "Show current session status", None),
        ],
        "cursor" => vec![
            cmd("help", "Show available commands", None),
            cmd("clear", "Clear conversation history", None),
            cmd("compact", "Compact conversation context", None),
            cmd("plan", "Switch to plan mode", None),
            cmd("model", "Switch active model", Some("[model]")),
            cmd("review", "Review workspace changes", Some("[instructions]")),
            cmd("mcp", "Inspect MCP servers", None),
            cmd("status", "Show session status", None),
        ],
        _ => vec![
            cmd("help", "Show available commands", None),
            cmd("clear", "Clear conversation history", None),
            cmd("compact", "Compact conversation context", None),
            cmd("plan", "Switch to planning mode", None),
            cmd("review", "Review current changes", Some("[instructions]")),
            cmd("model", "Switch or inspect active model", Some("[model]")),
            cmd("mcp", "Show MCP servers", None),
            cmd("status", "Show session status", None),
        ],
    }
}

fn push_dir(out: &mut Vec<PathBuf>, p: PathBuf) {
    if p.is_dir() && !out.contains(&p) {
        out.push(p);
    }
}

fn command_dirs_for(kind: &str, cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let has_cwd = !cwd.as_os_str().is_empty() && cwd.is_dir();
    match kind {
        "claude" => {
            if has_cwd {
                push_dir(&mut dirs, cwd.join(".claude/commands"));
            }
            if let Some(h) = home {
                push_dir(&mut dirs, h.join(".claude/commands"));
            }
        }
        "codex" => {
            if has_cwd {
                push_dir(&mut dirs, cwd.join(".codex/prompts"));
            }
            if let Some(h) = home {
                push_dir(&mut dirs, h.join(".codex/prompts"));
            }
        }
        "agy" | "gemini" => {
            if has_cwd {
                push_dir(&mut dirs, cwd.join(".gemini/commands"));
            }
            if let Some(h) = home {
                push_dir(&mut dirs, h.join(".gemini/commands"));
            }
        }
        "cursor" => {
            if has_cwd {
                push_dir(&mut dirs, cwd.join(".cursor/commands"));
            }
            if let Some(h) = home {
                push_dir(&mut dirs, h.join(".cursor/commands"));
            }
        }
        "opencode" => {
            if has_cwd {
                push_dir(&mut dirs, cwd.join(".opencode/commands"));
            }
            if let Some(h) = home {
                push_dir(&mut dirs, h.join(".config/opencode/commands"));
            }
        }
        _ => {
            if has_cwd {
                push_dir(&mut dirs, cwd.join(".claude/commands"));
                push_dir(&mut dirs, cwd.join(".agents/commands"));
            }
            if let Some(h) = home {
                push_dir(&mut dirs, h.join(".claude/commands"));
            }
        }
    }
    dirs
}

fn skill_dirs_for(kind: &str, cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let has_cwd = !cwd.as_os_str().is_empty() && cwd.is_dir();
    match kind {
        "claude" => {
            if has_cwd {
                push_dir(&mut dirs, cwd.join(".claude/skills"));
                push_dir(&mut dirs, cwd.join(".agents/skills"));
            }
            if let Some(h) = home {
                push_dir(&mut dirs, h.join(".claude/skills"));
                push_dir(&mut dirs, h.join(".agents/skills"));
            }
        }
        "codex" => {
            if has_cwd {
                push_dir(&mut dirs, cwd.join(".codex/skills"));
                push_dir(&mut dirs, cwd.join(".agents/skills"));
            }
            if let Some(h) = home {
                push_dir(&mut dirs, h.join(".codex/skills"));
                push_dir(&mut dirs, h.join(".agents/skills"));
            }
        }
        "agy" | "gemini" => {
            if has_cwd {
                push_dir(&mut dirs, cwd.join(".gemini/skills"));
                push_dir(&mut dirs, cwd.join(".agents/skills"));
            }
            if let Some(h) = home {
                push_dir(&mut dirs, h.join(".gemini/skills"));
                push_dir(&mut dirs, h.join(".agents/skills"));
            }
        }
        "cursor" => {
            if has_cwd {
                push_dir(&mut dirs, cwd.join(".cursor/skills"));
                push_dir(&mut dirs, cwd.join(".agents/skills"));
            }
            if let Some(h) = home {
                push_dir(&mut dirs, h.join(".cursor/skills"));
                push_dir(&mut dirs, h.join(".agents/skills"));
            }
        }
        "opencode" => {
            if has_cwd {
                push_dir(&mut dirs, cwd.join(".opencode/skills"));
                push_dir(&mut dirs, cwd.join(".agents/skills"));
            }
            if let Some(h) = home {
                push_dir(&mut dirs, h.join(".config/opencode/skills"));
                push_dir(&mut dirs, h.join(".agents/skills"));
            }
        }
        _ => {
            if has_cwd {
                push_dir(&mut dirs, cwd.join(".agents/skills"));
                push_dir(&mut dirs, cwd.join(".claude/skills"));
            }
            if let Some(h) = home {
                push_dir(&mut dirs, h.join(".agents/skills"));
                push_dir(&mut dirs, h.join(".claude/skills"));
            }
        }
    }
    dirs
}

fn read_head(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_READ_BYTES).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

struct DocMeta {
    name: Option<String>,
    description: String,
    input_hint: Option<String>,
}

fn unquote_yaml(raw: &str) -> String {
    let s = raw.trim();
    if s.len() >= 2 && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\''))) {
        s[1..s.len() - 1].trim().to_string()
    } else {
        s.to_string()
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() { format!("{}…", head.trim_end()) } else { head }
}

fn parse_markdown_frontmatter(text: &str) -> DocMeta {
    let mut name = None;
    let mut description = String::new();
    let mut input_hint = None;
    let normalized = text.replace("\r\n", "\n");
    let mut body = normalized.as_str();

    if let Some(after_open) = normalized.strip_prefix("---\n")
        && let Some(end_idx) = after_open.find("\n---")
    {
        let fm = &after_open[..end_idx];
        body = &after_open[end_idx + 4..];
        let lines: Vec<&str> = fm.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            if line.starts_with(' ') || line.starts_with('\t') || line.trim().is_empty() || line.trim().starts_with('#')
            {
                i += 1;
                continue;
            }
            let Some((k, v)) = line.split_once(':') else {
                i += 1;
                continue;
            };
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim();
            let resolved = if matches!(val, ">" | "|" | ">-" | "|-") {
                let mut block = Vec::new();
                i += 1;
                while i < lines.len() {
                    let sub = lines[i];
                    if sub.starts_with(' ') || sub.starts_with('\t') {
                        let t = sub.trim();
                        if !t.is_empty() {
                            block.push(t);
                        }
                        i += 1;
                    } else if sub.trim().is_empty() {
                        i += 1;
                    } else {
                        break;
                    }
                }
                block.join(" ")
            } else {
                i += 1;
                unquote_yaml(val)
            };
            if resolved.is_empty() {
                continue;
            }
            match key.as_str() {
                "name" => name = Some(resolved),
                "description" => description = truncate_chars(&resolved, MAX_DESC_CHARS),
                "argument-hint" | "argument_hint" | "hint" => input_hint = Some(resolved),
                _ => {}
            }
        }
    }

    if description.is_empty() {
        for line in body.lines() {
            let t = line.trim();
            if t.is_empty() || t == "---" || t.starts_with("```") {
                continue;
            }
            let cleaned = t.trim_start_matches('#').trim();
            if !cleaned.is_empty() {
                description = truncate_chars(cleaned, MAX_DESC_CHARS);
                break;
            }
        }
    }

    DocMeta { name, description, input_hint }
}

fn scan_command_dir(root: &Path) -> Vec<AcpAvailableCommand> {
    let mut out = Vec::new();
    scan_command_dir_inner(root, "", 0, &mut out);
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn scan_command_dir_inner(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<AcpAvailableCommand>) {
    if depth > 2 || out.len() >= MAX_DISCOVERED {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|de| de.path())).collect();
    paths.sort();
    for path in paths {
        let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if fname.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            let next_prefix = if prefix.is_empty() { fname.to_string() } else { format!("{prefix}:{fname}") };
            scan_command_dir_inner(&path, &next_prefix, depth + 1, out);
        } else if path.is_file()
            && path.extension().and_then(|s| s.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()).filter(|s| !s.is_empty()) else {
                continue;
            };
            let fallback_name = if prefix.is_empty() { stem.to_string() } else { format!("{prefix}:{stem}") };
            let meta = read_head(&path).map(|t| parse_markdown_frontmatter(&t));
            let (name, description, input_hint) = match meta {
                Some(m) => (m.name.unwrap_or(fallback_name), m.description, m.input_hint),
                None => (fallback_name, String::new(), None),
            };
            out.push(AcpAvailableCommand { name, description, input_hint, source: Some("command".into()) });
        }
    }
}

fn scan_skills_dir(dir: &Path, prefix_dollar: bool) -> Vec<AcpAvailableCommand> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|de| de.path())).collect();
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        if out.len() >= MAX_DISCOVERED {
            break;
        }
        let Some(folder_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if folder_name.starts_with('.') {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let meta = read_head(&skill_md).map(|t| parse_markdown_frontmatter(&t));
        let (raw_name, description, input_hint) = match meta {
            Some(m) => (m.name.unwrap_or_else(|| folder_name.to_string()), m.description, m.input_hint),
            None => (folder_name.to_string(), String::new(), None),
        };
        let clean = raw_name.trim().trim_start_matches('/').trim_start_matches('$');
        if clean.is_empty() {
            continue;
        }
        let name = if prefix_dollar { format!("${clean}") } else { clean.to_string() };
        out.push(AcpAvailableCommand { name, description, input_hint, source: Some("skill".into()) });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_yaml_frontmatter_and_block_descriptions() {
        let md = "---\nname: drove\ndescription: >\n  Multi-agent DAG planning\n  and parallel execution.\nargument-hint: \"[goal]\"\n---\n# Ignored\n";
        let meta = parse_markdown_frontmatter(md);
        assert_eq!(meta.name.as_deref(), Some("drove"));
        assert_eq!(meta.description, "Multi-agent DAG planning and parallel execution.");
        assert_eq!(meta.input_hint.as_deref(), Some("[goal]"));

        let plain = "# My Custom Command\nDoes cool stuff.\n";
        let plain_meta = parse_markdown_frontmatter(plain);
        assert_eq!(plain_meta.name, None);
        assert_eq!(plain_meta.description, "My Custom Command");
    }

    #[test]
    fn discovers_claude_and_codex_builtins_commands_and_skills() {
        let tmp = std::env::temp_dir().join(format!("herdr2-cmd-disc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cwd = tmp.join("repo");
        let home = tmp.join("home");
        std::fs::create_dir_all(cwd.join(".claude/commands/ops")).unwrap();
        std::fs::create_dir_all(cwd.join(".claude/skills/my-skill")).unwrap();
        std::fs::create_dir_all(home.join(".codex/skills/drove")).unwrap();
        std::fs::write(
            cwd.join(".claude/commands/ops/deploy.md"),
            "---\ndescription: Deploy release artifacts\nargument-hint: <env>\n---\nRun deploy.\n",
        )
        .unwrap();
        std::fs::write(
            cwd.join(".claude/skills/my-skill/SKILL.md"),
            "---\nname: my-skill\ndescription: Custom project skill\n---\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".codex/skills/drove/SKILL.md"),
            "---\nname: drove\ndescription: Multi-agent DAG orchestration\n---\n",
        )
        .unwrap();

        let claude = discover_commands_with_home("claude", &cwd, Some(&home));
        assert!(claude.iter().any(|c| c.name == "compact" && c.source.as_deref() == Some("command")));
        let deploy = claude.iter().find(|c| c.name == "ops:deploy").expect("nested command");
        assert_eq!(deploy.description, "Deploy release artifacts");
        assert_eq!(deploy.input_hint.as_deref(), Some("<env>"));
        let skill = claude.iter().find(|c| c.name == "my-skill").expect("claude skill");
        assert_eq!(skill.source.as_deref(), Some("skill"));

        let codex = discover_commands_with_home("codex", &cwd, Some(&home));
        assert!(codex.iter().any(|c| c.name == "plan"));
        let drove = codex.iter().find(|c| c.name == "$drove").expect("codex skill with $ prefix");
        assert_eq!(drove.source.as_deref(), Some("skill"));
        assert_eq!(drove.description, "Multi-agent DAG orchestration");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_acp_commands_prefers_live_and_normalizes_sources() {
        let live = vec![
            AcpAvailableCommand {
                name: "plan".into(),
                description: "Turn plan mode on.".into(),
                input_hint: None,
                source: None,
            },
            AcpAvailableCommand {
                name: "$drove".into(),
                description: "DAG orchestration".into(),
                input_hint: None,
                source: None,
            },
            AcpAvailableCommand {
                name: "mcp:herdr2:ls".into(),
                description: "List agents".into(),
                input_hint: None,
                source: None,
            },
        ];
        let resolved = resolve_acp_commands(&live, Some("codex"), &[], "");
        assert_eq!(resolved.len(), 3);
        assert_eq!(resolved[0].source.as_deref(), Some("command"));
        assert_eq!(resolved[1].source.as_deref(), Some("skill"));
        assert_eq!(resolved[2].source.as_deref(), Some("mcp"));
    }
}
