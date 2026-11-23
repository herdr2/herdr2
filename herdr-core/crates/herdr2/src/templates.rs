//! Templates: `~/.config/herdr2/templates/<name>.md`, read fresh on every
//! use. Frontmatter says how to start the ACP agent; the body is its first message.

use crate::paths::Paths;
use crate::permissions::PermissionMode;
use crate::rpc::RpcError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    pub name: String,
    pub description: String,
    /// ACP command line or preset override (`command:` or `agent:`).
    pub command: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub permissions: Option<PermissionMode>,
    pub inject_mcp: Option<bool>,
    pub system_prompt: Option<String>,
    pub body: String,
    /// `report` (default): ready at its first status report. `quiet`: ready
    /// once its output goes quiet, for programs in interactive terminals.
    pub ready_when_quiet: bool,
    pub submit: Option<String>,
    pub stop: Option<String>,
    pub paste_gap_ms: Option<u64>,
}

#[derive(Serialize, JsonSchema)]
pub struct TemplateInfo {
    pub name: String,
    pub description: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Set when the file can't be used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn dir(paths: &Paths) -> PathBuf {
    paths.config.parent().expect("config file has a dir").join("templates")
}

fn parse(name: &str, text: &str) -> Result<Template, String> {
    let text = text.replace("\r\n", "\n");
    let rest = text.strip_prefix("---\n").ok_or("no frontmatter (start the file with ---)")?;
    let end = rest.find("\n---").ok_or("frontmatter never closes (---)")?;
    let (front, body) = (&rest[..end], &rest[end + 4..]);
    let mut t = Template {
        name: name.into(),
        description: String::new(),
        command: String::new(),
        agent: None,
        model: None,
        mode: None,
        permissions: None,
        inject_mcp: None,
        system_prompt: None,
        body: body.strip_prefix('\n').unwrap_or(body).trim_end().into(),
        ready_when_quiet: false,
        submit: None,
        stop: None,
        paste_gap_ms: None,
    };
    for line in front.lines().filter(|l| !l.trim().is_empty()) {
        let (key, value) = line.split_once(':').ok_or_else(|| format!("not `key: value`: {line:?}"))?;
        let value = value.trim().to_string();
        match key.trim() {
            "name" => {}
            "description" => t.description = value,
            "command" => t.command = value,
            "agent" => t.agent = (!value.is_empty()).then_some(value),
            "model" => t.model = (!value.is_empty()).then_some(value),
            "mode" => t.mode = (!value.is_empty()).then_some(value),
            "permissions" => t.permissions = Some(value.parse()?),
            "inject_mcp" => {
                t.inject_mcp = Some(value.parse::<bool>().map_err(|_| "inject_mcp must be true or false")?);
            }
            "system_prompt" => t.system_prompt = (!value.is_empty()).then_some(value),
            "ready" => t.ready_when_quiet = value == "quiet",
            "submit" => t.submit = Some(value),
            "stop" => t.stop = Some(value),
            "paste_gap_ms" => t.paste_gap_ms = Some(value.parse().map_err(|_| "paste_gap_ms must be a number")?),
            _ => {}
        }
    }
    if t.command.is_empty() && t.agent.is_none() {
        return Err("no agent or command".into());
    }
    Ok(t)
}

/// The template called `name`, if there is one.
pub fn load(paths: &Paths, name: &str) -> Result<Option<Template>, RpcError> {
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return Ok(None);
    }
    let path = dir(paths).join(format!("{name}.md"));
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(name, &text)
            .map(Some)
            .map_err(|e| RpcError::new("invalid_template", format!("{}: {e}", path.display()), "see h2 templates")),
        Err(_) => Ok(None),
    }
}

pub fn list(paths: &Paths) -> Vec<TemplateInfo> {
    let mut names: Vec<String> = std::fs::read_dir(dir(paths))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".md").map(String::from))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| match load(paths, &name) {
            Ok(Some(t)) => TemplateInfo {
                name,
                description: t.description,
                command: if t.command.is_empty() { t.agent.clone().unwrap_or_default() } else { t.command },
                agent: t.agent,
                model: t.model,
                error: None,
            },
            Ok(None) => TemplateInfo {
                name,
                description: String::new(),
                command: String::new(),
                agent: None,
                model: None,
                error: Some("unreadable".into()),
            },
            Err(e) => TemplateInfo {
                name,
                description: String::new(),
                command: String::new(),
                agent: None,
                model: None,
                error: Some(e.message),
            },
        })
        .collect()
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone, PartialEq)]
pub struct FirstMessage {
    pub sent: bool,
    /// Whether the agent started working on it (see agent.send).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started: Option<bool>,
    /// Why it was not sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The turn when it went in (see agent.send).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
}

/// The first message: template instructions, then the caller prompt.
pub(crate) fn ready_timeout() -> std::time::Duration {
    let ms = std::env::var("H2_READY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(60_000);
    std::time::Duration::from_millis(ms)
}

/// Extended timeout while a CLI is parked on an interactive startup gate (e.g. Claude's folder trust check)
/// so the user has time to review and accept before the queued first/handoff message is delivered.
fn startup_dialog_timeout() -> std::time::Duration {
    let ms = std::env::var("H2_STARTUP_DIALOG_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(300_000);
    std::time::Duration::from_millis(ms)
}

fn visible_screen_tail(st: &crate::runtime::State, id: u32, max_lines: usize) -> String {
    let Some(screen) = st.screens.get(&id) else {
        return String::new();
    };
    let text = crate::stitch::text(&screen.screen_rows());
    let non_empty: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let start = non_empty.len().saturating_sub(max_lines);
    non_empty[start..].join("\n")
}

/// Returns true when the bottom of the visible viewport already shows an interactive CLI's
/// ready input status bar (meaning any startup dialog above it has already been dismissed).
pub(crate) fn is_cli_prompt_ready(visible_tail: &str) -> bool {
    let lines: Vec<&str> = visible_tail.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let bottom = lines[lines.len().saturating_sub(6)..].join("\n").to_ascii_lowercase();
    bottom.contains("? for shortcuts")
        || bottom.contains("shift+tab to cycle")
        || bottom.contains("bypass permissions on")
        || bottom.contains("accept edits on")
        || bottom.contains("plan mode on")
}

/// Returns true when the visible PTY viewport is parked on an interactive startup gate
/// (such as Claude Code's folder trust or bypass-permissions confirmation where option 1 is `No, exit`).
/// Sending a first/handoff message while this is visible would press Enter on `No, exit` and kill the CLI.
pub(crate) fn is_pty_startup_dialog(visible_tail: &str) -> bool {
    if visible_tail.trim().is_empty() || is_cli_prompt_ready(visible_tail) {
        return false;
    }
    let lower = visible_tail.to_ascii_lowercase();
    if lower.contains("didn't start because this folder isn't trusted") {
        return true;
    }
    // Claude Code folder trust gate ("Quick safety check: Is this a project you created or one you trust?")
    if lower.contains("yes, i trust this folder")
        || lower.contains("do you trust the files in this folder")
        || lower.contains("is this a project you created or one you trust")
        || (lower.contains("quick safety check")
            && (lower.contains("trust") || lower.contains("no, exit") || lower.contains("enter to confirm")))
    {
        return true;
    }
    // Claude Code `--dangerously-skip-permissions` / Bypass Permissions confirmation gate
    if (lower.contains("bypass permissions mode") && (lower.contains("yes, i accept") || lower.contains("no, exit")))
        || (lower.contains("yes, i accept") && lower.contains("no, exit"))
    {
        return true;
    }
    // Generic Ink/CLI onboarding or confirmation gates (login, theme, update prompt)
    if lower.contains("select login method") || lower.contains("choose the text style") {
        return true;
    }
    lower.contains("enter to confirm")
        && lower.contains("esc to cancel")
        && (lower.contains("no, exit")
            || lower.contains("trust")
            || lower.contains("yes, i accept")
            || lower.contains("update available"))
}

impl crate::runtime::Core {
    /// Sends a PTY template's or handoff's first message once its program is truly ready for input:
    /// waits for its first status report (or quiet output), and holds the message whenever an
    /// interactive startup dialog (e.g. Claude's folder trust check) is visible on screen so
    /// Enter is never sent into `No, exit`.
    pub async fn pty_first_message(&self, id: u32, quiet: bool, text: &str, from: Option<&str>) -> FirstMessage {
        let not_sent = |reason: String| FirstMessage { sent: false, started: None, reason: Some(reason), turn: None };
        let live = |st: &crate::runtime::State| st.pty_reg.terminals.get(&id).is_some_and(|t| t.is_live());
        let started = tokio::time::Instant::now();
        let base_deadline = started + ready_timeout();
        let dialog_deadline = started + startup_dialog_timeout();
        let mut saw_startup_dialog = false;
        let mut dialog_cleared_at: Option<tokio::time::Instant> = None;

        // Wait until the CLI is ready AND no startup confirmation dialog is on screen.
        loop {
            let (is_live, is_reported, last_out, tail) = {
                let st = self.lock();
                let t = st.pty_reg.terminals.get(&id);
                (
                    t.is_some_and(|t| t.is_live()),
                    t.is_some_and(|t| t.reported),
                    st.last_output.get(&id).copied(),
                    visible_screen_tail(&st, id, 24),
                )
            };
            if !is_live {
                return not_sent("it exited before it was ready".into());
            }

            let in_dialog = is_pty_startup_dialog(&tail);
            if in_dialog {
                saw_startup_dialog = true;
                dialog_cleared_at = None;
            } else if saw_startup_dialog && dialog_cleared_at.is_none() {
                dialog_cleared_at = Some(tokio::time::Instant::now());
            }

            let settle_need = if saw_startup_dialog {
                std::time::Duration::from_millis(1200)
            } else {
                crate::pty_input::OUTPUT_SETTLE
            };
            let output_quiet = last_out.is_some_and(|t| t.elapsed() >= settle_need);
            let post_dialog_settled =
                dialog_cleared_at.is_none_or(|at| at.elapsed() >= std::time::Duration::from_millis(1200));

            let ready = if in_dialog || !post_dialog_settled {
                false
            } else if quiet {
                output_quiet
            } else if is_reported {
                // If reported arrived early (e.g. from a hook before Ink painted a trust dialog),
                // let output settle (or confirm no output arrived) before sending.
                last_out.is_none() || output_quiet
            } else {
                // Fallback for CLIs that didn't emit an OSC 7501 startup report (or after clearing a
                // startup dialog): accept once the real interactive prompt footer is settled on screen.
                output_quiet && is_cli_prompt_ready(&tail)
            };

            if ready {
                break;
            }

            let active_deadline = if saw_startup_dialog { dialog_deadline } else { base_deadline };
            if tokio::time::Instant::now() >= active_deadline {
                if saw_startup_dialog {
                    return not_sent(format!(
                        "still waiting on a startup confirmation dialog; finish it in the terminal, then h2 send {0}",
                        crate::terminals::address(id)
                    ));
                }
                if quiet {
                    return not_sent(format!(
                        "its output never went quiet; check its screen (h2 read {0}), then h2 send {0}",
                        crate::terminals::address(id)
                    ));
                }
                return not_sent(format!(
                    "it never reported its status, so it may be showing a dialog; check its screen (h2 read {0}), then h2 send {0}",
                    crate::terminals::address(id)
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        }

        if !live(&self.lock()) {
            return not_sent("it exited before it was ready".into());
        }
        let text = self.pty_tagged(from, id, text, true);
        let send_deadline = if saw_startup_dialog { dialog_deadline } else { base_deadline };
        loop {
            // Re-verify right before typing in case a chained startup dialog (e.g. Bypass Permissions
            // right after folder trust) rendered in the meantime.
            let dialog_now = {
                let st = self.lock();
                if !live(&st) {
                    return not_sent("it exited before it was ready".into());
                }
                is_pty_startup_dialog(&visible_screen_tail(&st, id, 24))
            };
            if dialog_now {
                saw_startup_dialog = true;
                if tokio::time::Instant::now() >= dialog_deadline {
                    return not_sent(format!(
                        "still waiting on a startup confirmation dialog; finish it in the terminal, then h2 send {0}",
                        crate::terminals::address(id)
                    ));
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                continue;
            }
            match self.send_first_task(id, &text).await {
                Ok(sent) => {
                    return FirstMessage { sent: true, started: sent.started, reason: None, turn: Some(sent.turn) };
                }
                Err(e)
                    if e.kind == "unavailable"
                        && (e.message.ends_with("is still starting: its output has not settled")
                            || e.message.ends_with("is showing a startup confirmation dialog"))
                        && tokio::time::Instant::now()
                            < if saw_startup_dialog || e.message.ends_with("is showing a startup confirmation dialog") {
                                dialog_deadline
                            } else {
                                send_deadline
                            } =>
                {
                    if e.message.ends_with("is showing a startup confirmation dialog") {
                        saw_startup_dialog = true;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                Err(e) => return not_sent(format!("{}; {}", e.message, e.hint)),
            }
        }
    }
}

pub fn first_message(t: &Template, prompt: &str) -> String {
    [t.body.trim(), prompt.trim()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_frontmatter_and_body_match_lf() {
        let text = "---\nname: reviewer\ndescription: Review code\nagent: codex\nmodel: o3\npermissions: approve-reads\n---\nFirst line.\nSecond line.\n";
        let expected = parse("reviewer", text).unwrap();
        assert_eq!(expected.agent.as_deref(), Some("codex"));
        assert_eq!(expected.model.as_deref(), Some("o3"));
        assert_eq!(expected.permissions, Some(PermissionMode::ApproveReads));
        assert_eq!(expected.body, "First line.\nSecond line.");
        assert_eq!(parse("reviewer", &text.replace('\n', "\r\n")).unwrap(), expected);
        let mixed = text.replacen("---\n", "---\r\n", 1).replace("Second line.\n", "Second line.\r\n");
        assert_eq!(parse("reviewer", &mixed).unwrap(), expected);
    }

    #[test]
    fn frontmatter_and_body_parse_like_the_spec_example() {
        let text = "---\nname: reviewer\ndescription: Read-only code review. Give it file paths and scope.\n\
                    command: pi-acp --fast\n---\n\
                    You are a focused review agent.\nReturn findings.\n";
        let t = parse("reviewer", text).unwrap();
        assert_eq!(t.command, "pi-acp --fast");
        assert_eq!(t.description, "Read-only code review. Give it file paths and scope.");
        assert_eq!(
            first_message(&t, "review src/mcp.rs"),
            "You are a focused review agent.\nReturn findings.\n\nreview src/mcp.rs"
        );
        assert!(parse("x", "no frontmatter").is_err());
        assert!(parse("x", "---\ndescription: d\n---\n").unwrap_err().contains("no agent or command"));
    }

    #[test]
    fn detects_claude_trust_and_bypass_startup_dialogs_without_false_positives() {
        let trust_dialog = "\
Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source project, or work from your team).
Claude Code'll be able to read, edit, and execute files here.
Security guide
❯ No, exit
  Yes, I trust this folder
Enter to confirm · Esc to cancel";
        assert!(is_pty_startup_dialog(trust_dialog));
        assert!(!is_cli_prompt_ready(trust_dialog));

        let bypass_dialog = "\
WARNING: Claude Code running in Bypass Permissions mode
❯ 1. No, exit
  2. Yes, I accept
Enter to confirm · Esc to cancel";
        assert!(is_pty_startup_dialog(bypass_dialog));

        let ready_prompt = "\
╭─── Claude Code v2.1.328 ───╮
│ Welcome back!              │
╰────────────────────────────╯
❯
  ⏵⏵ bypass permissions on (shift+tab to cycle)   ? for shortcuts";
        assert!(is_cli_prompt_ready(ready_prompt));
        assert!(!is_pty_startup_dialog(ready_prompt));

        // Even if trust text lingered higher up in a tall viewport, a ready footer clears it.
        let cleared_in_tall_viewport = format!("{trust_dialog}\n{ready_prompt}");
        assert!(is_cli_prompt_ready(&cleared_in_tall_viewport));
        assert!(!is_pty_startup_dialog(&cleared_in_tall_viewport));
    }
}
