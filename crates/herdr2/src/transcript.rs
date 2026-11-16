//! Structured per-agent ACP transcripts backed by append-only JSONL files.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 2000;
const MAX_BYTES: usize = 2 << 20;

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    User {
        turn: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        text: String,
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
        ts_ms: u64,
    },
    ToolCall {
        turn: u64,
        id: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_kind: Option<String>,
        status: String,
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
}

impl Entry {
    fn turn(&self) -> Option<u64> {
        match self {
            Self::User { turn, .. }
            | Self::Thought { turn, .. }
            | Self::Assistant { turn, .. }
            | Self::ToolCall { turn, .. }
            | Self::Plan { turn, .. }
            | Self::Permission { turn, .. }
            | Self::Error { turn, .. } => Some(*turn),
            Self::Stderr { .. } => None,
        }
    }

    fn approx_bytes(&self) -> usize {
        match self {
            Self::User { text, from, .. } => text.len() + from.as_ref().map_or(0, String::len),
            Self::Thought { text, .. } | Self::Assistant { text, .. } | Self::Stderr { text, .. } => text.len(),
            Self::ToolCall { id, title, tool_kind, status, .. } => {
                id.len() + title.len() + tool_kind.as_ref().map_or(0, String::len) + status.len()
            }
            Self::Plan { items, .. } => items.iter().map(String::len).sum(),
            Self::Permission { title, outcome, .. } => title.len() + outcome.len(),
            Self::Error { message, .. } => message.len(),
        }
    }

    fn render_line(&self, out: &mut Vec<String>) {
        match self {
            Self::User { from, text, .. } => {
                let prefix = match from {
                    Some(from) => format!("[user from {from}]"),
                    None => "[user]".into(),
                };
                if text.is_empty() {
                    out.push(prefix);
                } else {
                    for (i, line) in text.lines().enumerate() {
                        if i == 0 {
                            out.push(format!("{prefix} {line}"));
                        } else {
                            out.push(line.into());
                        }
                    }
                }
            }
            Self::Thought { .. } => {}
            Self::Assistant { text, .. } => {
                for line in text.lines() {
                    out.push(line.into());
                }
            }
            Self::ToolCall { title, tool_kind, status, .. } => {
                let kind = tool_kind.as_deref().filter(|k| !k.is_empty());
                match kind {
                    Some(k) => out.push(format!("[tool {k}: {status}] {title}")),
                    None => out.push(format!("[tool: {status}] {title}")),
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
            Entry::Assistant { turn, text, ts_ms } => {
                if let Some(Entry::Assistant { turn: prev_turn, text: prev_text, ts_ms: prev_ts }) =
                    self.entries.last_mut()
                    && *prev_turn == turn
                {
                    prev_text.push_str(&text);
                    *prev_ts = ts_ms;
                    self.bytes += added_bytes;
                    self.evict_old();
                    return;
                }
                self.entries.push(Entry::Assistant { turn, text, ts_ms });
            }
            Entry::Thought { turn, text, ts_ms } => {
                if let Some(Entry::Thought { turn: prev_turn, text: prev_text, ts_ms: prev_ts }) =
                    self.entries.last_mut()
                    && *prev_turn == turn
                {
                    prev_text.push_str(&text);
                    *prev_ts = ts_ms;
                    self.bytes += added_bytes;
                    self.evict_old();
                    return;
                }
                self.entries.push(Entry::Thought { turn, text, ts_ms });
            }
            Entry::ToolCall { turn, id, title, tool_kind, status, ts_ms } => {
                if !id.is_empty()
                    && let Some(Entry::ToolCall {
                        title: old_title,
                        tool_kind: old_kind,
                        status: old_status,
                        ts_ms: old_ts,
                        ..
                    }) = self.entries.iter_mut().rev().find(|e| {
                        matches!(e, Entry::ToolCall { turn: t, id: eid, .. } if *t == turn && eid == &id)
                    })
                {
                    self.bytes = self.bytes.saturating_sub(
                        old_title.len() + old_kind.as_ref().map_or(0, String::len) + old_status.len(),
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
                    *old_ts = ts_ms;
                    self.bytes += old_title.len() + old_kind.as_ref().map_or(0, String::len) + old_status.len();
                    self.evict_old();
                    return;
                }
                self.entries.push(Entry::ToolCall { turn, id, title, tool_kind, status, ts_ms });
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
            let text = latest_turn
                .and_then(|t| {
                    self.entries
                        .iter()
                        .rev()
                        .filter(|e| e.turn() == Some(t))
                        .find_map(|e| match e {
                            Entry::Assistant { text, .. } if !text.is_empty() => Some(text.clone()),
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
                        Entry::Assistant { text, .. } if !text.is_empty() => Some(text.clone()),
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
        t.push(Entry::User { turn: 1, from: Some("a0".into()), text: "hi".into(), ts_ms: 1 });
        t.push(Entry::ToolCall {
            turn: 1,
            id: "tc1".into(),
            title: "Read foo".into(),
            tool_kind: Some("read".into()),
            status: "pending".into(),
            ts_ms: 2,
        });
        t.push(Entry::ToolCall {
            turn: 1,
            id: "tc1".into(),
            title: "".into(),
            tool_kind: None,
            status: "completed".into(),
            ts_ms: 3,
        });
        t.push(Entry::Assistant { turn: 1, text: "Hello ".into(), ts_ms: 4 });
        t.push(Entry::Assistant { turn: 1, text: "world".into(), ts_ms: 5 });

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
            t.push(Entry::User { turn, from: None, text: format!("q{turn}"), ts_ms: turn });
            t.push(Entry::Assistant { turn, text: format!("a{turn}"), ts_ms: turn });
        }
        assert_eq!(t.render(None, false), ("[user] q3\na3".into(), false, "turn"));
        assert_eq!(t.render(Some(2), false), ("[user] q3\na3".into(), true, "history"));
        assert_eq!(t.render(Some(10), false), ("[user] q1\na1\n[user] q2\na2\n[user] q3\na3".into(), false, "history"));
    }

    #[test]
    fn jsonl_round_trip_replays_saved_entries() {
        let dir = crate::test_tmp().join(format!("h2-transcript-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        let e1 = Entry::User { turn: 1, from: None, text: "hello".into(), ts_ms: 1 };
        let e2 = Entry::Assistant { turn: 1, text: "part1 ".into(), ts_ms: 2 };
        let e3 = Entry::Assistant { turn: 1, text: "part2".into(), ts_ms: 3 };
        append_disk(&dir, "a1", &e1).unwrap();
        append_disk(&dir, "a1", &e2).unwrap();
        append_disk(&dir, "a1", &e3).unwrap();
        let loaded = load(&dir, "a1").unwrap();
        assert_eq!(loaded.render(None, true), ("part1 part2".into(), false, "last"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
