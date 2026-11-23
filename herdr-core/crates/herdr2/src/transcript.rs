//! Structured per-agent ACP transcripts backed by append-only JSONL files.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 2000;
const MAX_BYTES: usize = 2 << 20;
const MAX_TOOL_DETAIL_CHARS: usize = 4000;

/// Trims and caps tool-call detail snippets (commands, diffs, or outputs) at a clean char boundary.
pub(crate) fn cap_tool_detail(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut chars = trimmed.chars();
    let capped: String = chars.by_ref().take(MAX_TOOL_DETAIL_CHARS).collect();
    if chars.next().is_some() { Some(format!("{capped}\n…")) } else { Some(capped) }
}

/// Lightweight metadata for a multimodal attachment (image, audio, embedded resource, or resource link).
/// Binary payloads live beside the agent under `<state>/files/<addr>/` so JSONL transcripts stay compact.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    /// Content kind: `"image"`, `"audio"`, `"resource"`, or `"resource_link"`.
    pub kind: String,
    /// MIME type when known (e.g. `"image/png"`, `"text/markdown"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Display name or basename (e.g. `"screenshot.png"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Staged path on the agent's machine for preview, reload, or tool fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Optional URI (`file:///...`, `https://...`, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Optional bounded text snippet or description (for text resources / links).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Raw byte length when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

impl Attachment {
    pub(crate) fn approx_bytes(&self) -> usize {
        self.kind.len()
            + self.mime_type.as_ref().map_or(0, String::len)
            + self.name.as_ref().map_or(0, String::len)
            + self.path.as_ref().map_or(0, String::len)
            + self.uri.as_ref().map_or(0, String::len)
            + self.text.as_ref().map_or(0, String::len)
            + 32
    }

    pub fn summary_line(&self) -> String {
        let label = self
            .name
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| self.uri.as_deref().filter(|s| !s.is_empty()))
            .or_else(|| self.path.as_deref().filter(|s| !s.is_empty()))
            .unwrap_or("attachment");
        match self.path.as_deref().filter(|p| !p.is_empty() && *p != label) {
            Some(path) => format!("[{}: {label} ({path})]", self.kind),
            None => format!("[{}: {label}]", self.kind),
        }
    }
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    User {
        turn: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Attachment>,
        ts_ms: u64,
    },
    Thought {
        turn: u64,
        text: String,
        ts_ms: u64,
    },
    Assistant {
        turn: u64,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Attachment>,
        ts_ms: u64,
    },
    ToolCall {
        turn: u64,
        id: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_kind: Option<String>,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Attachment>,
        ts_ms: u64,
    },
    Plan {
        turn: u64,
        items: Vec<String>,
        ts_ms: u64,
    },
    Permission {
        turn: u64,
        title: String,
        outcome: String,
        ts_ms: u64,
    },
    Error {
        turn: u64,
        message: String,
        ts_ms: u64,
    },
    Stderr {
        text: String,
        ts_ms: u64,
    },
    Checkpoint {
        turn: u64,
        ref_name: String,
        commit_oid: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head_oid: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<crate::checkpoint::CheckpointFileStat>,
        ts_ms: u64,
    },
    Rewind {
        to_turn: u64,
        restore_files: bool,
        ts_ms: u64,
    },
    /// Cross-provider/session context handoff, fork, or side-chat record.
    Handoff {
        turn: u64,
        handoff_kind: String,
        from_address: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_turns: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_markdown: Option<String>,
        ts_ms: u64,
    },
}

impl Entry {
    pub fn turn(&self) -> Option<u64> {
        match self {
            Self::User { turn, .. }
            | Self::Thought { turn, .. }
            | Self::Assistant { turn, .. }
            | Self::ToolCall { turn, .. }
            | Self::Plan { turn, .. }
            | Self::Permission { turn, .. }
            | Self::Error { turn, .. }
            | Self::Checkpoint { turn, .. }
            | Self::Handoff { turn, .. } => Some(*turn),
            Self::Rewind { to_turn, .. } => Some(*to_turn),
            Self::Stderr { .. } => None,
        }
    }

    pub(crate) fn approx_bytes(&self) -> usize {
        match self {
            Self::User { text, from, attachments, .. } => {
                text.len()
                    + from.as_ref().map_or(0, String::len)
                    + attachments.iter().map(Attachment::approx_bytes).sum::<usize>()
            }
            Self::Thought { text, .. } | Self::Stderr { text, .. } => text.len(),
            Self::Assistant { text, attachments, .. } => {
                text.len() + attachments.iter().map(Attachment::approx_bytes).sum::<usize>()
            }
            Self::ToolCall { id, title, tool_kind, status, detail, attachments, .. } => {
                id.len()
                    + title.len()
                    + tool_kind.as_ref().map_or(0, String::len)
                    + status.len()
                    + detail.as_ref().map_or(0, String::len)
                    + attachments.iter().map(Attachment::approx_bytes).sum::<usize>()
            }
            Self::Plan { items, .. } => items.iter().map(String::len).sum(),
            Self::Permission { title, outcome, .. } => title.len() + outcome.len(),
            Self::Error { message, .. } => message.len(),
            Self::Checkpoint { ref_name, commit_oid, files, .. } => {
                ref_name.len() + commit_oid.len() + files.iter().map(|f| f.path.len() + 24).sum::<usize>()
            }
            Self::Rewind { .. } => 24,
            Self::Handoff {
                handoff_kind,
                from_address,
                from_provider,
                from_session_id,
                from_turns,
                artifact_path,
                context_markdown,
                ..
            } => {
                handoff_kind.len()
                    + from_address.len()
                    + from_provider.as_ref().map_or(0, String::len)
                    + from_session_id.as_ref().map_or(0, String::len)
                    + from_turns.as_ref().map_or(0, String::len)
                    + artifact_path.as_ref().map_or(0, String::len)
                    + context_markdown.as_ref().map_or(0, String::len)
            }
        }
    }

    fn render_line(&self, out: &mut Vec<String>) {
        match self {
            Self::User { from, text, attachments, .. } => {
                let prefix = match from {
                    Some(from) => format!("[user from {from}]"),
                    None => "[user]".into(),
                };
                if text.is_empty() && attachments.is_empty() {
                    out.push(prefix);
                } else if text.is_empty() {
                    for (i, att) in attachments.iter().enumerate() {
                        if i == 0 {
                            out.push(format!("{prefix} {}", att.summary_line()));
                        } else {
                            out.push(att.summary_line());
                        }
                    }
                } else {
                    for (i, line) in text.lines().enumerate() {
                        if i == 0 {
                            out.push(format!("{prefix} {line}"));
                        } else {
                            out.push(line.into());
                        }
                    }
                    for att in attachments {
                        out.push(att.summary_line());
                    }
                }
            }
            Self::Thought { .. } => {}
            Self::Assistant { text, attachments, .. } => {
                for line in text.lines() {
                    out.push(line.into());
                }
                for att in attachments {
                    out.push(att.summary_line());
                }
            }
            Self::ToolCall { title, tool_kind, status, attachments, .. } => {
                let kind = tool_kind.as_deref().filter(|k| !k.is_empty());
                let suffix = if attachments.is_empty() {
                    String::new()
                } else {
                    format!(" ({} attachment(s))", attachments.len())
                };
                match kind {
                    Some(k) => out.push(format!("[tool {k}: {status}] {title}{suffix}")),
                    None => out.push(format!("[tool: {status}] {title}{suffix}")),
                }
            }
            Self::Plan { items, .. } => {
                for item in items {
                    out.push(format!("[plan] {item}"));
                }
            }
            Self::Permission { title, outcome, .. } => {
                out.push(format!("[permission: {outcome}] {title}"));
            }
            Self::Error { message, .. } => {
                out.push(format!("[error] {message}"));
            }
            Self::Stderr { text, .. } => {
                for line in text.lines() {
                    out.push(format!("[stderr] {line}"));
                }
            }
            Self::Checkpoint { turn, files, .. } => {
                out.push(format!("[checkpoint turn {turn}: {} file(s)]", files.len()));
            }
            Self::Rewind { to_turn, restore_files, .. } => {
                out.push(format!("[rewind to turn {to_turn}: files={restore_files}]"));
            }
            Self::Handoff { handoff_kind, from_address, from_provider, from_turns, .. } => {
                let mut msg = format!("[{handoff_kind} from {from_address}");
                if let Some(p) = from_provider.as_deref().filter(|s| !s.is_empty()) {
                    msg.push_str(&format!(" ({p})"));
                }
                if let Some(t) = from_turns.as_deref().filter(|s| !s.is_empty()) {
                    msg.push_str(&format!(" turns {t}"));
                }
                msg.push(']');
                out.push(msg);
            }
        }
    }
}

#[derive(Default, Clone, Debug)]
pub struct Transcript {
    entries: Vec<Entry>,
    bytes: usize,
    truncated_history: bool,
}

impl Transcript {
    /// Pushes one entry, coalescing consecutive assistant/thought chunks and updating matching tool calls in-place.
    pub fn push(&mut self, entry: Entry) {
        let added_bytes = entry.approx_bytes();
        match entry {
            Entry::Assistant { turn, text, attachments, ts_ms } => {
                if let Some(Entry::Assistant {
                    turn: prev_turn,
                    text: prev_text,
                    attachments: prev_attachments,
                    ts_ms: prev_ts,
                }) = self.entries.last_mut()
                    && *prev_turn == turn
                {
                    prev_text.push_str(&text);
                    prev_attachments.extend(attachments);
                    *prev_ts = ts_ms;
                    self.bytes += added_bytes;
                    self.evict_old();
                    return;
                }
                self.entries.push(Entry::Assistant { turn, text, attachments, ts_ms });
            }
            Entry::Thought { turn, text, ts_ms } => {
                if let Some(Entry::Thought { turn: prev_turn, text: prev_text, ts_ms: prev_ts }) =
                    self.entries.last_mut()
                    && *prev_turn == turn
                {
                    let sep = if prev_text.trim_end().ends_with("**")
                        && text.trim_start().starts_with("**")
                        && !prev_text.ends_with('\n')
                        && !text.starts_with('\n')
                    {
                        prev_text.push_str("\n\n");
                        2
                    } else {
                        0
                    };
                    prev_text.push_str(&text);
                    *prev_ts = ts_ms;
                    self.bytes += added_bytes + sep;
                    self.evict_old();
                    return;
                }
                self.entries.push(Entry::Thought { turn, text, ts_ms });
            }
            Entry::ToolCall { turn, id, title, tool_kind, status, detail, attachments, ts_ms } => {
                if !id.is_empty()
                    && let Some(Entry::ToolCall {
                        title: old_title,
                        tool_kind: old_kind,
                        status: old_status,
                        detail: old_detail,
                        attachments: old_attachments,
                        ts_ms: old_ts,
                        ..
                    }) = self
                        .entries
                        .iter_mut()
                        .rev()
                        .find(|e| matches!(e, Entry::ToolCall { turn: t, id: eid, .. } if *t == turn && eid == &id))
                {
                    self.bytes = self.bytes.saturating_sub(
                        old_title.len()
                            + old_kind.as_ref().map_or(0, String::len)
                            + old_status.len()
                            + old_detail.as_ref().map_or(0, String::len)
                            + old_attachments.iter().map(Attachment::approx_bytes).sum::<usize>(),
                    );
                    if !title.is_empty() {
                        *old_title = title;
                    }
                    if tool_kind.is_some() {
                        *old_kind = tool_kind;
                    }
                    if !status.is_empty() {
                        *old_status = status;
                    }
                    if let Some(d) = detail {
                        let d_trim = d.trim();
                        if !d_trim.is_empty() {
                            match old_detail {
                                Some(prev) if !prev.contains(d_trim) => {
                                    if prev.len() < 4000 {
                                        prev.push_str("\n\n");
                                        prev.push_str(d_trim);
                                    }
                                }
                                None => *old_detail = Some(d_trim.to_string()),
                                _ => {}
                            }
                        }
                    }
                    for att in attachments {
                        let dup = old_attachments.iter().any(|existing| {
                            existing.kind == att.kind && existing.path == att.path && existing.uri == att.uri
                        });
                        if !dup {
                            old_attachments.push(att);
                        }
                    }
                    *old_ts = ts_ms;
                    self.bytes += old_title.len()
                        + old_kind.as_ref().map_or(0, String::len)
                        + old_status.len()
                        + old_detail.as_ref().map_or(0, String::len)
                        + old_attachments.iter().map(Attachment::approx_bytes).sum::<usize>();
                    self.evict_old();
                    return;
                }
                self.entries.push(Entry::ToolCall { turn, id, title, tool_kind, status, detail, attachments, ts_ms });
            }
            other => {
                self.entries.push(other);
            }
        }
        self.bytes += added_bytes;
        self.evict_old();
    }

    fn evict_old(&mut self) {
        while (self.entries.len() > MAX_ENTRIES || self.bytes > MAX_BYTES) && self.entries.len() > 1 {
            let removed = self.entries.remove(0);
            self.bytes = self.bytes.saturating_sub(removed.approx_bytes());
            self.truncated_history = true;
        }
    }

    /// Renders output for `agent.read`:
    /// - `last_only == true` -> latest assistant response (or error if no assistant text in latest turn), source `"last"`
    /// - `lines == None` -> latest turn transcript (or stderr/error before first turn), source `"turn"`
    /// - `lines == Some(n)` -> trailing `n` rendered lines across history, source `"history"`
    pub fn render(&self, lines: Option<usize>, last_only: bool) -> (String, bool, &'static str) {
        if last_only {
            let latest_turn = self.entries.iter().filter_map(Entry::turn).max();
            let format_assistant = |text: &str, attachments: &[Attachment]| -> Option<String> {
                if text.is_empty() && attachments.is_empty() {
                    return None;
                }
                if attachments.is_empty() {
                    return Some(text.to_string());
                }
                let mut lines = Vec::new();
                if !text.is_empty() {
                    lines.push(text.to_string());
                }
                for att in attachments {
                    lines.push(att.summary_line());
                }
                Some(lines.join("\n"))
            };
            let text = latest_turn
                .and_then(|t| {
                    self.entries
                        .iter()
                        .rev()
                        .filter(|e| e.turn() == Some(t))
                        .find_map(|e| match e {
                            Entry::Assistant { text, attachments, .. } => format_assistant(text, attachments),
                            _ => None,
                        })
                        .or_else(|| {
                            self.entries.iter().rev().filter(|e| e.turn() == Some(t)).find_map(|e| match e {
                                Entry::Error { message, .. } => Some(format!("[error] {message}")),
                                _ => None,
                            })
                        })
                })
                .or_else(|| {
                    self.entries.iter().rev().find_map(|e| match e {
                        Entry::Assistant { text, attachments, .. } => format_assistant(text, attachments),
                        Entry::Error { message, .. } => Some(format!("[error] {message}")),
                        _ => None,
                    })
                })
                .unwrap_or_default();
            return (text, false, "last");
        }

        if let Some(n) = lines {
            let mut rendered = Vec::new();
            for entry in &self.entries {
                entry.render_line(&mut rendered);
            }
            let truncated = self.truncated_history || rendered.len() > n;
            let start = rendered.len().saturating_sub(n);
            return (rendered[start..].join("\n"), truncated, "history");
        }

        let latest_turn = self.entries.iter().filter_map(Entry::turn).max();
        let mut rendered = Vec::new();
        for entry in &self.entries {
            let include = match latest_turn {
                Some(t) => entry.turn() == Some(t),
                None => matches!(entry, Entry::Stderr { .. } | Entry::Error { .. }),
            };
            if include {
                entry.render_line(&mut rendered);
            }
        }
        (rendered.join("\n"), false, "turn")
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Rewinds transcript state to `to_turn`, returning the retracted `(to_turn + 1)` user prompt if present.
    pub fn rewind_to_turn(&mut self, path: &Path, to_turn: u64, restore_files: bool, ts_ms: u64) -> Option<String> {
        let restored_prompt = self.entries.iter().find_map(|entry| match entry {
            Entry::User { turn, text, .. } if *turn == to_turn + 1 => {
                let clean = strip_h2_envelope(text);
                (!clean.is_empty()).then_some(clean)
            }
            _ => None,
        });

        let cutoff = self.entries.iter().position(|e| e.turn().is_some_and(|t| t > to_turn));
        if let Some(idx) = cutoff {
            self.entries.truncate(idx);
        }
        self.entries.retain(|e| e.turn().is_none_or(|t| t <= to_turn));
        // Deduplicate consecutive rewind markers so repeated rollbacks never stack dividers.
        while matches!(self.entries.last(), Some(Entry::Rewind { .. })) {
            self.entries.pop();
        }
        self.entries.push(Entry::Rewind { to_turn, restore_files, ts_ms });
        self.bytes = self.entries.iter().map(Entry::approx_bytes).sum();

        let mut buf = String::new();
        for entry in &self.entries {
            if let Ok(line) = serde_json::to_string(entry) {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = crate::paths::write_atomic(path, buf.as_bytes());
        restored_prompt
    }
}

fn strip_prefixed_tag_envelope(raw: &str, tag: &str) -> String {
    let trimmed = raw.trim_start();
    let open_prefix = format!("<{tag}");
    if !trimmed.starts_with(&open_prefix) {
        return raw.to_string();
    }
    let after_name = &trimmed[open_prefix.len()..];
    if !after_name.starts_with('>') && !after_name.starts_with(char::is_whitespace) {
        return raw.to_string();
    }
    let close = format!("</{tag}>");
    if let Some(open_end) = trimmed.find('>')
        && let Some(end) = trimmed[open_end + 1..].find(&close)
    {
        return trimmed[open_end + 1 + end + close.len()..].trim().to_string();
    }
    raw.to_string()
}

/// Removes `<herdr2-context-handoff>`, `<herdr2-handoff>`, and `<h2 ...>` inter-agent envelopes from user text.
pub fn strip_h2_envelope(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    loop {
        let before = s.clone();
        s = strip_prefixed_tag_envelope(&s, "herdr2-context-handoff");
        s = strip_prefixed_tag_envelope(&s, "herdr2-handoff");
        let trimmed = s.trim();
        if trimmed.starts_with("<h2")
            && trimmed.ends_with("</h2>")
            && let Some(open_end) = trimmed.find('>')
        {
            s = trimmed[open_end + 1..trimmed.len() - "</h2>".len()].trim().to_string();
        }
        if s == before {
            break;
        }
    }
    s
}

/// Slices transcript entries to turns within `[from_turn..=to_turn]` (when either bound is given).
pub fn slice_turn_range(entries: &[Entry], from_turn: Option<u64>, to_turn: Option<u64>) -> Vec<Entry> {
    if from_turn.is_none() && to_turn.is_none() {
        return entries.to_vec();
    }
    let lo = from_turn.unwrap_or(0);
    let hi = to_turn.unwrap_or(u64::MAX);
    entries.iter().filter(|e| e.turn().is_some_and(|t| t >= lo && t <= hi)).cloned().collect()
}

/// Renders structured entries into human-readable transcript text.
pub fn render_entries(entries: &[Entry]) -> String {
    let mut lines = Vec::new();
    for entry in entries {
        entry.render_line(&mut lines);
    }
    if lines.is_empty() {
        String::new()
    } else {
        let mut s = lines.join("\n");
        s.push('\n');
        s
    }
}

/// Builds a bounded portable handoff block from turns `1..=up_to_turn` for session rehydration.
pub fn build_portable_handoff(entries: &[Entry], up_to_turn: u64, max_bytes: usize) -> Option<String> {
    if up_to_turn == 0 || max_bytes < 128 {
        return None;
    }

    let mut per_turn: std::collections::BTreeMap<u64, Vec<String>> = std::collections::BTreeMap::new();
    for entry in entries {
        let Some(turn) = entry.turn() else { continue };
        if turn == 0 || turn > up_to_turn {
            continue;
        }
        let bucket = per_turn.entry(turn).or_default();
        match entry {
            Entry::User { text, attachments, .. } => {
                let clean = strip_h2_envelope(text);
                if !clean.is_empty() {
                    bucket.push(format!("User: {clean}"));
                }
                for att in attachments {
                    bucket.push(format!("User attachment: {}", att.summary_line()));
                }
            }
            Entry::ToolCall { title, status, detail, attachments, .. } => {
                let mut line = format!("- Tool `{title}` [{status}]");
                if let Some(d) = detail.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    let snippet = cap_chars(d, 180);
                    line.push_str(&format!(": {snippet}"));
                }
                if !attachments.is_empty() {
                    line.push_str(&format!(" ({} attachment(s))", attachments.len()));
                }
                bucket.push(line);
            }
            Entry::Assistant { text, attachments, .. } => {
                let clean = text.trim();
                if !clean.is_empty() {
                    bucket.push(format!("Assistant: {clean}"));
                }
                for att in attachments {
                    bucket.push(format!("Assistant attachment: {}", att.summary_line()));
                }
            }
            _ => {}
        }
    }

    per_turn.retain(|_, lines| !lines.is_empty());
    if per_turn.is_empty() {
        return None;
    }

    let header = format!(
        "<herdr2-context-handoff restored_turn=\"{up_to_turn}\">\nConversation history restored after checkpoint rollback to turn {up_to_turn}. Continue naturally without mentioning the rollback wrapper.\n\n"
    );
    let footer = "\n</herdr2-context-handoff>";
    let body_budget = max_bytes.saturating_sub(header.len() + footer.len());
    if body_budget == 0 {
        return None;
    }

    let mut turn_blocks: Vec<(u64, String)> =
        per_turn.into_iter().map(|(t, lines)| (t, format!("[Turn {t}]\n{}", lines.join("\n")))).collect();

    // Always prioritize preserving Turn 1 goal + newest turns backward to `up_to_turn`.
    let mut selected_indices = Vec::new();
    let mut used = 0usize;
    if !turn_blocks.is_empty() {
        let first_block = cap_chars(&turn_blocks[0].1, body_budget);
        if first_block.len() <= body_budget {
            used += first_block.len();
            turn_blocks[0].1 = first_block;
            selected_indices.push(0);
        }
    }
    let mut recent = Vec::new();
    for idx in (1..turn_blocks.len()).rev() {
        let sep = usize::from(!selected_indices.is_empty() || !recent.is_empty()) * 2;
        let blk = &turn_blocks[idx].1;
        if used + sep + blk.len() <= body_budget {
            used += sep + blk.len();
            recent.push(idx);
        } else if recent.is_empty() && body_budget > used + sep + 32 {
            let trimmed = cap_chars(blk, body_budget - used - sep);
            turn_blocks[idx].1 = trimmed;
            recent.push(idx);
            break;
        } else {
            break;
        }
    }
    recent.reverse();
    selected_indices.extend(recent);

    let mut body = String::new();
    let mut prev_idx: Option<usize> = None;
    for idx in selected_indices {
        if let Some(p) = prev_idx {
            if idx > p + 1 {
                body.push_str("\n\n[... earlier turns omitted ...]\n\n");
            } else {
                body.push_str("\n\n");
            }
        }
        body.push_str(&turn_blocks[idx].1);
        prev_idx = Some(idx);
    }
    if body.len() > body_budget {
        body = cap_chars(&body, body_budget);
    }
    Some(format!("{header}{body}{footer}"))
}

fn cap_chars(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let suffix = "... [truncated]";
    let mut end = max_bytes.saturating_sub(suffix.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}", &s[..end])
}

pub fn path(state: &Path, address: &str) -> PathBuf {
    state.join("transcripts").join(format!("{address}.jsonl"))
}

pub fn append_disk(state: &Path, address: &str, entry: &Entry) -> io::Result<()> {
    let p = path(state, address);
    if let Some(parent) = p.parent() {
        crate::platform::private_dir(parent)?;
    }
    let line = serde_json::to_string(entry).map_err(io::Error::other)?;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(p)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")
}

pub fn load(state: &Path, address: &str) -> io::Result<Transcript> {
    let p = path(state, address);
    let raw = match std::fs::read_to_string(&p) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Transcript::default()),
        Err(e) => return Err(e),
    };
    let mut t = Transcript::default();
    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        if let Ok(entry) = serde_json::from_str::<Entry>(line) {
            t.push(entry);
        }
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_assistant_chunks_and_updates_tool_calls() {
        let mut t = Transcript::default();
        t.push(Entry::User { turn: 1, from: Some("a0".into()), text: "hi".into(), attachments: vec![], ts_ms: 1 });
        t.push(Entry::ToolCall {
            turn: 1,
            id: "tc1".into(),
            title: "Read foo".into(),
            tool_kind: Some("read".into()),
            status: "pending".into(),
            detail: None,
            attachments: vec![],
            ts_ms: 2,
        });
        t.push(Entry::ToolCall {
            turn: 1,
            id: "tc1".into(),
            title: "".into(),
            tool_kind: None,
            status: "completed".into(),
            detail: Some("fn foo() {}".into()),
            attachments: vec![],
            ts_ms: 3,
        });
        t.push(Entry::Assistant { turn: 1, text: "Hello ".into(), attachments: vec![], ts_ms: 4 });
        t.push(Entry::Assistant { turn: 1, text: "world".into(), attachments: vec![], ts_ms: 5 });

        assert_eq!(t.render(None, true), ("Hello world".into(), false, "last"));
        assert_eq!(
            t.render(None, false),
            ("[user from a0] hi\n[tool read: completed] Read foo\nHello world".into(), false, "turn")
        );
    }

    #[test]
    fn history_mode_slices_lines_and_reports_truncation() {
        let mut t = Transcript::default();
        for turn in 1..=3 {
            t.push(Entry::User { turn, from: None, text: format!("q{turn}"), attachments: vec![], ts_ms: turn });
            t.push(Entry::Assistant { turn, text: format!("a{turn}"), attachments: vec![], ts_ms: turn });
        }
        assert_eq!(t.render(None, false), ("[user] q3\na3".into(), false, "turn"));
        assert_eq!(t.render(Some(2), false), ("[user] q3\na3".into(), true, "history"));
        assert_eq!(t.render(Some(10), false), ("[user] q1\na1\n[user] q2\na2\n[user] q3\na3".into(), false, "history"));
    }

    #[test]
    fn jsonl_round_trip_replays_saved_entries() {
        let dir = crate::test_tmp().join(format!("h2-transcript-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        let e1 = Entry::User { turn: 1, from: None, text: "hello".into(), attachments: vec![], ts_ms: 1 };
        let e2 = Entry::Assistant { turn: 1, text: "part1 ".into(), attachments: vec![], ts_ms: 2 };
        let e3 = Entry::Assistant { turn: 1, text: "part2".into(), attachments: vec![], ts_ms: 3 };
        append_disk(&dir, "a1", &e1).unwrap();
        append_disk(&dir, "a1", &e2).unwrap();
        append_disk(&dir, "a1", &e3).unwrap();
        let loaded = load(&dir, "a1").unwrap();
        assert_eq!(loaded.render(None, true), ("part1 part2".into(), false, "last"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn coalesces_adjacent_bold_thought_headings_with_spacing() {
        let mut t = Transcript::default();
        t.push(Entry::Thought { turn: 1, text: "**First heading**".into(), ts_ms: 1 });
        t.push(Entry::Thought { turn: 1, text: "**Second heading**".into(), ts_ms: 2 });
        let Some(Entry::Thought { text, .. }) = t.entries.first() else {
            panic!("expected coalesced thought entry");
        };
        assert_eq!(text, "**First heading**\n\n**Second heading**");
    }

    #[test]
    fn rewind_trims_turns_extracts_clean_prompt_and_builds_bounded_handoff() {
        let dir = crate::test_tmp().join(format!("h2-rewind-{}", crate::platform::random_hex()));
        let path = dir.join("a1.jsonl");
        let mut t = Transcript::default();
        t.push(Entry::User { turn: 1, from: None, text: "create lib.rs".into(), attachments: vec![], ts_ms: 1 });
        t.push(Entry::ToolCall {
            turn: 1,
            id: "tc1".into(),
            title: "Write lib.rs".into(),
            tool_kind: Some("edit".into()),
            status: "completed".into(),
            detail: Some("pub fn add(a: i32, b: i32) -> i32 { a + b }".into()),
            attachments: vec![],
            ts_ms: 2,
        });
        t.push(Entry::Assistant { turn: 1, text: "Created lib.rs".into(), attachments: vec![], ts_ms: 3 });
        t.push(Entry::Checkpoint {
            turn: 1,
            ref_name: "refs/herdr2/checkpoints/default/a1/1".into(),
            commit_oid: "abc".into(),
            head_oid: None,
            files: vec![],
            ts_ms: 4,
        });
        t.push(Entry::User {
            turn: 2,
            from: Some("a0".into()),
            text: "<h2 from=\"a0\">\nrefactor incorrectly\n</h2>".into(),
            attachments: vec![],
            ts_ms: 5,
        });
        t.push(Entry::Assistant { turn: 2, text: "Broken code".into(), attachments: vec![], ts_ms: 6 });

        assert!(build_portable_handoff(t.entries(), 0, 24 * 1024).is_none());
        let handoff = build_portable_handoff(t.entries(), 1, 24 * 1024).unwrap();
        assert!(handoff.contains("create lib.rs"));
        assert!(handoff.contains("Created lib.rs"));
        assert!(!handoff.contains("Broken code"));

        let restored = t.rewind_to_turn(&path, 1, true, 100);
        assert_eq!(restored.as_deref(), Some("refactor incorrectly"));
        assert!(t.entries.iter().any(|e| matches!(e, Entry::Rewind { to_turn: 1, restore_files: true, .. })));
        assert!(!t.entries.iter().any(|e| e.turn() == Some(2)));
        assert_eq!(
            strip_h2_envelope("<herdr2-context-handoff restored_turn=\"1\">\nx\n</herdr2-context-handoff>\n\nhello"),
            "hello"
        );
        assert_eq!(
            strip_h2_envelope("<herdr2-handoff kind=\"handoff\" from=\"a1\">\nmeta\n</herdr2-handoff>\n\ncontinue"),
            "continue"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entry_handoff_and_slice_turn_range() {
        let e0 = Entry::Handoff {
            turn: 0,
            handoff_kind: "handoff".into(),
            from_address: "a1".into(),
            from_provider: Some("claude".into()),
            from_session_id: Some("sess-1".into()),
            from_turns: Some("1-3".into()),
            artifact_path: None,
            context_markdown: None,
            ts_ms: 10,
        };
        let e1 = Entry::User { turn: 1, from: None, text: "turn one".into(), attachments: vec![], ts_ms: 11 };
        let e2 = Entry::Assistant { turn: 2, text: "turn two".into(), attachments: vec![], ts_ms: 12 };
        let e3 = Entry::User { turn: 3, from: None, text: "turn three".into(), attachments: vec![], ts_ms: 13 };
        let all = vec![e0, e1, e2, e3];

        let mut lines = Vec::new();
        all[0].render_line(&mut lines);
        assert_eq!(lines, vec!["[handoff from a1 (claude) turns 1-3]"]);

        let mid = slice_turn_range(&all, Some(2), Some(2));
        assert_eq!(mid.len(), 1);
        assert_eq!(mid[0].turn(), Some(2));

        let head = slice_turn_range(&all, None, Some(1));
        assert_eq!(head.len(), 2);
        assert_eq!(head[0].turn(), Some(0));
        assert_eq!(head[1].turn(), Some(1));
    }
}
