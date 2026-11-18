//! Bounded deterministic context handoff & lightweight session lineage.

use std::collections::BTreeMap;
use std::path::Path;

use clap::ValueEnum;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::transcript::{Entry, strip_h2_envelope};

/// Default byte budget for generated context handoff packages (~24 KiB).
pub const DEFAULT_HANDOFF_MAX_BYTES: usize = 24 * 1024;

/// How a derived session was created from an ancestor session.
#[derive(Serialize, Deserialize, JsonSchema, ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum LineageKind {
    /// Continue the same line of work in a new or different agent/provider.
    #[default]
    Handoff,
    /// Branch off to explore or re-implement independently from a turn/state.
    Fork,
    /// Open a temporary side-channel discussion without replacing the main session.
    #[serde(alias = "side_chat")]
    #[value(name = "side-chat", alias = "side_chat")]
    SideChat,
}

impl LineageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Handoff => "handoff",
            Self::Fork => "fork",
            Self::SideChat => "side-chat",
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            Self::Handoff => "Handoff",
            Self::Fork => "Fork",
            Self::SideChat => "Side Chat",
        }
    }
}

/// Lightweight parent pointer stored on `Agent`, `Terminal`, and handoff results.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct Lineage {
    pub kind: LineageKind,
    /// Source address on this core (e.g. `a1` or `t2`).
    pub parent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_turn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_turn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<String>,
}

/// Inputs used to synthesize a deterministic cross-provider context handoff.
pub struct HandoffBuildInput<'a> {
    pub kind: LineageKind,
    pub source_address: &'a str,
    pub source_provider: Option<&'a str>,
    pub source_session_id: Option<&'a str>,
    pub source_model: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub entries: &'a [Entry],
    pub from_turn: Option<u64>,
    pub to_turn: Option<u64>,
    pub max_bytes: usize,
    pub note: Option<&'a str>,
}

/// Output from [`build_handoff_package`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HandoffPackage {
    pub markdown: String,
    pub included_turns: Vec<u64>,
    pub omitted_turns: Vec<u64>,
    pub total_turns: u64,
    pub bytes: usize,
}

#[derive(Default)]
struct TurnBucket {
    users: Vec<String>,
    assistants: Vec<String>,
    tools: Vec<String>,
    errors: Vec<String>,
    plan: Option<Vec<String>>,
}

/// Wraps a Markdown handoff body in `<herdr2-handoff ...>` tags for ACP turn-1 injection
/// so `strip_h2_envelope` removes it cleanly if re-read or re-forwarded later.
pub fn wrap_handoff_envelope(kind: LineageKind, source_address: &str, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return String::new();
    }
    format!(
        "<herdr2-handoff kind=\"{}\" from=\"{}\">\n{}\n</herdr2-handoff>",
        kind.as_str(),
        source_address.replace('"', ""),
        body
    )
}

/// Formats sorted turn numbers into compact range notation (e.g. `1-3, 5, 8-10`).
pub fn format_turn_ranges(turns: &[u64]) -> String {
    if turns.is_empty() {
        return String::new();
    }
    let mut sorted = turns.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut ranges = Vec::new();
    let mut start = sorted[0];
    let mut prev = sorted[0];
    for &t in &sorted[1..] {
        if t == prev + 1 {
            prev = t;
        } else {
            ranges.push(if start == prev {
                start.to_string()
            } else {
                format!("{start}-{prev}")
            });
            start = t;
            prev = t;
        }
    }
    ranges.push(if start == prev {
        start.to_string()
    } else {
        format!("{start}-{prev}")
    });
    ranges.join(", ")
}

/// Returns contiguous `(from_turn, to_turn)` ranges from sorted turn numbers.
pub fn contiguous_turn_ranges(turns: &[u64]) -> Vec<(u64, u64)> {
    if turns.is_empty() {
        return Vec::new();
    }
    let mut sorted = turns.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = Vec::new();
    let mut start = sorted[0];
    let mut prev = sorted[0];
    for &t in &sorted[1..] {
        if t == prev + 1 {
            prev = t;
        } else {
            out.push((start, prev));
            start = t;
            prev = t;
        }
    }
    out.push((start, prev));
    out
}

/// Default interactive CLI argv for a preset name when spawning a PTY terminal target.
pub fn default_pty_argv(preset: &str) -> Vec<String> {
    match preset.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => {
            vec!["claude".into(), "--dangerously-skip-permissions".into()]
        }
        "codex" => vec![
            "codex".into(),
            "--no-daemon".into(),
            "--dangerously-bypass-approvals-and-sandbox".into(),
        ],
        "gemini" | "gemini-cli" => vec!["gemini".into(), "--yolo".into()],
        "agy" | "antigravity" | "antigravity-cli" => {
            vec!["agy".into(), "--dangerously-skip-permissions".into()]
        }
        "qwen" | "qwen-code" => vec!["qwen".into()],
        "pi" | "omp" | "oh-my-pi" => vec!["pi".into()],
        "opencode" => vec!["opencode".into(), "--auto".into()],
        "cursor" | "cursor-agent" => {
            vec!["cursor-agent".into(), "--yolo".into(), "--trust".into()]
        }
        "goose" => vec!["goose".into()],
        other if !other.is_empty() => vec![other.to_string()],
        _ => Vec::new(),
    }
}

/// Builds a bounded deterministic Markdown context handoff package from transcript entries
/// and live workspace git status.
///
/// Layout priority within `max_bytes`:
/// 1. Source metadata header + optional caller note
/// 2. Live workspace git branch & `git status --short`
/// 3. Latest `Plan` (if present in selected turns)
/// 4. Turn 1 (or earliest selected turn) original objective
/// 5. Newest turns packed backwards until budget is full
/// 6. Omitted turns footer pointing to `h2 read <source> --from-turn X --to-turn Y`
pub fn build_handoff_package(input: &HandoffBuildInput<'_>) -> HandoffPackage {
    let budget = input.max_bytes.max(512);
    let lo = input.from_turn.unwrap_or(1);
    let hi = input.to_turn.unwrap_or(u64::MAX);

    let mut total_turns = 0u64;
    let mut per_turn: BTreeMap<u64, TurnBucket> = BTreeMap::new();
    for entry in input.entries {
        let Some(turn) = entry.turn() else { continue };
        total_turns = total_turns.max(turn);
        if turn == 0 || turn < lo || turn > hi {
            continue;
        }
        let bucket = per_turn.entry(turn).or_default();
        match entry {
            Entry::User { text, .. } => {
                let clean = strip_h2_envelope(text);
                if !clean.is_empty() {
                    bucket.users.push(clean);
                }
            }
            Entry::Plan { items, .. } => {
                if !items.is_empty() {
                    bucket.plan = Some(items.clone());
                }
            }
            Entry::ToolCall { title, status, detail, .. } => {
                let mut line = format!("- `[{status}]` {title}");
                if status == "failed"
                    && let Some(d) = detail.as_deref().map(str::trim).filter(|s| !s.is_empty())
                {
                    let snippet = truncate_utf8_with_ellipsis(d, 200).replace('\n', " ");
                    line.push_str(&format!(" — {snippet}"));
                }
                bucket.tools.push(line);
            }
            Entry::Assistant { text, .. } => {
                let clean = text.trim();
                if !clean.is_empty() {
                    bucket.assistants.push(clean.to_string());
                }
            }
            Entry::Error { message, .. } => {
                let clean = message.trim();
                if !clean.is_empty() {
                    bucket.errors.push(clean.to_string());
                }
            }
            Entry::Thought { .. }
            | Entry::Permission { .. }
            | Entry::Stderr { .. }
            | Entry::Checkpoint { .. }
            | Entry::Rewind { .. }
            | Entry::Handoff { .. } => {}
        }
    }

    // Header
    let mut header = String::new();
    header.push_str(&format!(
        "# {} Context Package (from `{}`)\n\n",
        input.kind.display_label(),
        input.source_address
    ));
    header.push_str(&format!("- **Source address:** `{}`\n", input.source_address));
    header.push_str(&format!("- **Kind:** `{}`\n", input.kind.as_str()));
    if let Some(p) = input.source_provider.map(str::trim).filter(|s| !s.is_empty()) {
        header.push_str(&format!("- **Source provider:** `{p}`\n"));
    }
    if let Some(m) = input.source_model.map(str::trim).filter(|s| !s.is_empty()) {
        header.push_str(&format!("- **Source model:** `{m}`\n"));
    }
    if let Some(sid) = input.source_session_id.map(str::trim).filter(|s| !s.is_empty()) {
        header.push_str(&format!("- **Source session ID:** `{sid}`\n"));
    }
    if let Some(cwd) = input.cwd.map(str::trim).filter(|s| !s.is_empty()) {
        header.push_str(&format!("- **Working directory:** `{cwd}`\n"));
    }
    let selected_turns: Vec<u64> = per_turn.keys().copied().collect();
    if !selected_turns.is_empty() {
        header.push_str(&format!(
            "- **Selected turns:** {} (of {total_turns} total)\n",
            format_turn_ranges(&selected_turns)
        ));
    }
    if let Some(note) = input.note.map(str::trim).filter(|s| !s.is_empty()) {
        header.push_str("\n## Handoff Note\n\n");
        header.push_str(note);
        header.push('\n');
    }

    // Git workspace state
    let git_section = input
        .cwd
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|c| git_workspace_summary(Path::new(c)))
        .unwrap_or_default();

    // Latest plan across selected turns
    let latest_plan = per_turn.values().rev().find_map(|b| b.plan.as_ref());
    let plan_section = match latest_plan {
        Some(items) if !items.is_empty() => {
            let mut s = String::from("\n## Latest Plan\n\n");
            for item in items.iter().take(30) {
                s.push_str(&format!("- {item}\n"));
            }
            truncate_utf8_with_ellipsis(&s, 2048)
        }
        _ => String::new(),
    };

    let reserve_footer = 640usize;
    let mut fixed = String::new();
    fixed.push_str(&header);
    if fixed.len() + git_section.len() + reserve_footer < budget {
        fixed.push_str(&git_section);
    }
    if fixed.len() + plan_section.len() + reserve_footer < budget {
        fixed.push_str(&plan_section);
    }

    let first_turn = per_turn.keys().next().copied();
    let mut initial_section = String::new();
    let mut included_turns = Vec::new();

    if let Some(t1) = first_turn
        && let Some(b1) = per_turn.get(&t1)
    {
        let max_t1 = budget.saturating_sub(fixed.len() + reserve_footer).min(4096);
        let rendered_t1 = render_turn_section(t1, b1, max_t1, t1 == 1);
        if !rendered_t1.is_empty() && fixed.len() + rendered_t1.len() + reserve_footer <= budget {
            initial_section.push_str(&rendered_t1);
            included_turns.push(t1);
        }
    }

    let turns_budget = budget.saturating_sub(fixed.len() + initial_section.len() + reserve_footer);
    let mut recent_sections: Vec<(u64, String)> = Vec::new();
    let mut recent_used = 0usize;

    for (&turn, bucket) in per_turn.iter().rev() {
        if Some(turn) == first_turn && !initial_section.is_empty() {
            continue;
        }
        let avail = turns_budget.saturating_sub(recent_used);
        if avail < 120 {
            break;
        }
        let max_this = if recent_sections.is_empty() {
            avail
        } else {
            avail.min(6 * 1024)
        };
        let rendered = render_turn_section(turn, bucket, max_this, false);
        if rendered.is_empty() {
            continue;
        }
        if recent_used + rendered.len() <= turns_budget {
            recent_used += rendered.len();
            recent_sections.push((turn, rendered));
            included_turns.push(turn);
        } else if recent_sections.is_empty() {
            let clipped = truncate_utf8_with_ellipsis(&rendered, avail);
            if !clipped.is_empty() {
                recent_sections.push((turn, clipped));
                included_turns.push(turn);
            }
            break;
        } else {
            break;
        }
    }

    included_turns.sort_unstable();
    included_turns.dedup();
    let omitted_turns: Vec<u64> = per_turn
        .keys()
        .copied()
        .filter(|t| !included_turns.contains(t))
        .collect();

    recent_sections.reverse();

    let mut body = fixed;
    if !initial_section.is_empty() {
        body.push_str(&initial_section);
    }
    if !recent_sections.is_empty() {
        body.push_str("\n## Recent Turns\n");
        for (_, sec) in recent_sections {
            body.push_str(&sec);
        }
    }
    if !omitted_turns.is_empty() {
        body.push_str("\n## Omitted History (On-Demand Lookup)\n\n");
        body.push_str(&format!(
            "Middle turns ({}) were omitted to stay within the {} KiB handoff budget.\n",
            format_turn_ranges(&omitted_turns),
            budget / 1024
        ));
        body.push_str("Use `h2 read` (or MCP tool `h2_read`) to inspect specific omitted turn ranges:\n");
        for (a, b) in contiguous_turn_ranges(&omitted_turns) {
            body.push_str(&format!(
                "- `h2 read {} --from-turn {a} --to-turn {b}`\n",
                input.source_address
            ));
        }
    }

    if body.len() > budget {
        body = truncate_utf8_with_ellipsis(&body, budget);
    }
    let bytes = body.len();

    HandoffPackage {
        markdown: body,
        included_turns,
        omitted_turns,
        total_turns,
        bytes,
    }
}

fn render_turn_section(turn: u64, bucket: &TurnBucket, max_bytes: usize, is_turn_one: bool) -> String {
    if max_bytes < 64 {
        return String::new();
    }
    let mut s = if is_turn_one {
        String::from("\n## Original Objective (Turn 1)\n\n")
    } else {
        format!("\n### Turn {turn}\n\n")
    };
    if !bucket.users.is_empty() {
        s.push_str("**User:**\n");
        for u in &bucket.users {
            s.push_str(u.trim());
            s.push_str("\n\n");
        }
    }
    if !bucket.tools.is_empty() {
        s.push_str("**Tool Activity:**\n");
        let tool_cap = 12usize;
        for line in bucket.tools.iter().take(tool_cap) {
            s.push_str(line);
            s.push('\n');
        }
        if bucket.tools.len() > tool_cap {
            s.push_str(&format!("- ... ({} more tool calls omitted)\n", bucket.tools.len() - tool_cap));
        }
        s.push('\n');
    }
    if !bucket.errors.is_empty() {
        s.push_str("**Errors:**\n");
        for e in &bucket.errors {
            s.push_str(&format!("- {e}\n"));
        }
        s.push('\n');
    }
    if !bucket.assistants.is_empty() {
        s.push_str("**Assistant:**\n");
        for a in &bucket.assistants {
            s.push_str(a.trim());
            s.push_str("\n\n");
        }
    }
    truncate_utf8_with_ellipsis(&s, max_bytes)
}

fn git_workspace_summary(cwd: &Path) -> Option<String> {
    if !cwd.exists() {
        return None;
    }
    let inside = crate::drove::bg_std_command("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !inside.status.success() || String::from_utf8_lossy(&inside.stdout).trim() != "true" {
        return None;
    }
    let branch_out = crate::drove::bg_std_command("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let status_out = crate::drove::bg_std_command("git")
        .args(["status", "--short"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
        .unwrap_or_default();

    let mut s = String::from("\n## Workspace Git State\n\n");
    if let Some(b) = branch_out.filter(|b| !b.is_empty()) {
        s.push_str(&format!("- **Branch:** `{b}`\n"));
    }
    if status_out.is_empty() {
        s.push_str("- **Working tree:** clean\n");
    } else {
        let mut lines: Vec<&str> = status_out.lines().take(40).collect();
        let total = status_out.lines().count();
        s.push_str("```\n");
        s.push_str(&lines.join("\n"));
        if total > lines.len() {
            s.push_str(&format!("\n... ({} more modified paths)", total - lines.len()));
        }
        let _ = &mut lines;
        s.push_str("\n```\n");
    }
    Some(truncate_utf8_with_ellipsis(&s, 2560))
}

fn truncate_utf8_with_ellipsis(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    if max_bytes <= 20 {
        let mut end = max_bytes.min(s.len());
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        return s[..end].to_string();
    }
    let suffix = "\n... (truncated)\n";
    let cap = max_bytes.saturating_sub(suffix.len());
    let mut end = cap.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_keeps_turn_1_newest_turns_and_omitted_lookup_hint() {
        let mut entries = Vec::new();
        for t in 1..=12u64 {
            let padding = "x".repeat(1200);
            entries.push(Entry::User {
                turn: t,
                from: None,
                text: format!("user turn {t}: {padding}"),
                ts_ms: t,
            });
            entries.push(Entry::Assistant {
                turn: t,
                text: format!("assistant turn {t}: {padding}"),
                ts_ms: t + 100,
            });
        }
        entries.push(Entry::Plan {
            turn: 11,
            items: vec!["Step A".into(), "Step B".into()],
            ts_ms: 999,
        });

        let pkg = build_handoff_package(&HandoffBuildInput {
            kind: LineageKind::Handoff,
            source_address: "a1",
            source_provider: Some("claude"),
            source_session_id: Some("sess-123"),
            source_model: Some("claude-sonnet-4"),
            cwd: None,
            entries: &entries,
            from_turn: None,
            to_turn: None,
            max_bytes: 8 * 1024,
            note: Some("Continue implementation"),
        });

        assert!(pkg.bytes <= 8 * 1024, "package exceeded budget: {}", pkg.bytes);
        assert!(pkg.included_turns.contains(&1), "Turn 1 must be preserved");
        assert!(pkg.included_turns.contains(&12), "Latest turn must be preserved");
        assert!(!pkg.omitted_turns.is_empty(), "Middle turns should be omitted under 8 KiB budget");
        assert!(pkg.markdown.contains("Original Objective (Turn 1)"));
        assert!(pkg.markdown.contains("Step A"));
        assert!(pkg.markdown.contains("h2 read a1 --from-turn"));
    }

    #[test]
    fn turn_range_formatting_and_pty_defaults() {
        assert_eq!(format_turn_ranges(&[2, 3, 4, 7, 9, 10]), "2-4, 7, 9-10");
        assert_eq!(default_pty_argv("claude"), vec!["claude", "--dangerously-skip-permissions"]);
        assert_eq!(default_pty_argv("codex")[0], "codex");
        let wrapped = wrap_handoff_envelope(LineageKind::Fork, "t2", "# Body\nhi");
        assert_eq!(strip_h2_envelope(&format!("{wrapped}\n\nnext prompt")), "next prompt");
    }
}
