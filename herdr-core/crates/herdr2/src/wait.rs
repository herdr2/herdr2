//! `wait`: until an agent or terminal is not working, driven by its events so
//! even a quick flicker counts. Already not working: it returns at once.

use crate::events::{Filter, Sink};
use crate::runtime::{Core, TargetKey};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Output kept for `output` matching.
const SEEN_MAX: usize = 64 * 1024;

pub struct WaitSpec {
    /// Ends when output produced after the wait started contains this; then
    /// only blocked, error or exit end it otherwise.
    pub output: Option<String>,
    pub timeout: Option<Duration>,
    /// End at once if this turn already ended (a wait right after a send).
    pub after_turn: Option<u64>,
}

#[derive(Serialize, Deserialize, JsonSchema, Debug)]
pub struct WaitResult {
    pub agent: String,
    /// False on timeout: a check-in, the agent is still running.
    pub ended: bool,
    /// status, exit (including loss in a kept agent), output, timeout,
    /// or not_started (after a send: it did not start working; a plain wait
    /// still follows that send's turn); with `children`: children;
    /// message (someone messaged the waiter, any wait); for each child in
    /// `children`: status, exit, or running (still working).
    pub reason: String,
    /// The current state word (`idle`, `working`, `blocked`, `error`).
    pub status: Option<String>,
    pub turn: u64,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// With `children`: one result per child it reports on.
    pub children: Vec<WaitResult>,
    /// With a message: the sender's address (queued in your inbox);
    /// null when a person sent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// Whether a status change from `prev` to `new` ends the wait.
fn ends(prev: Option<&str>, new: Option<&str>) -> bool {
    new != Some("working") && (prev == Some("working") || new != prev)
}

/// Blocked or errored: no output is coming, the caller has to look.
fn stuck(word: Option<&str>) -> bool {
    matches!(word, Some("blocked" | "error"))
}

/// The text of a chunk of output after what the last one `held`: a character
/// cut at the chunk's end waits there for the rest instead of becoming U+FFFD.
fn text_of(held: &mut Vec<u8>, bytes: &[u8]) -> String {
    held.extend_from_slice(bytes);
    let open =
        |at: usize| std::str::from_utf8(&held[at..]).is_err_and(|e| e.valid_up_to() == 0 && e.error_len().is_none());
    let at = (held.len().saturating_sub(3)..held.len()).find(|at| open(*at)).unwrap_or(held.len());
    let rest = held.split_off(at);
    let text = strip(held);
    *held = rest;
    text
}

/// Output without escape sequences, for matching what a person would read.
fn strip(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if !c.is_control() || c == '\n' {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            Some('[') => while chars.next().is_some_and(|c| !('@'..='~').contains(&c)) {},
            Some(']') | Some('P') | Some('_') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

enum OutTx {
    Agent(tokio::sync::mpsc::UnboundedSender<String>),
    Terminal(tokio::sync::mpsc::UnboundedSender<Vec<u8>>),
}

enum OutRx {
    Agent(tokio::sync::mpsc::UnboundedReceiver<String>),
    Terminal(tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>),
}

async fn recv_chunk(rx: &mut OutRx, held: &mut Vec<u8>) -> Option<String> {
    match rx {
        OutRx::Agent(rx) => rx.recv().await,
        OutRx::Terminal(rx) => rx.recv().await.map(|b| text_of(held, &b)),
    }
}

/// A wait's subscription and output watch, gone with it however it returns.
struct Registered<'a> {
    core: &'a Core,
    target: TargetKey,
    events: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
    output: OutTx,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        let mut st = self.core.lock();
        self.core.unsubscribe_waiter(&mut st, &self.events);
        match (&self.target, &self.output) {
            (TargetKey::Agent(id), OutTx::Agent(tx)) => {
                if let Some(watchers) = st.output_watch.get_mut(id) {
                    watchers.retain(|w| !w.same_channel(tx));
                    if watchers.is_empty() {
                        st.output_watch.remove(id);
                    }
                }
            }
            (TargetKey::Terminal(id), OutTx::Terminal(tx)) => {
                if let Some(watchers) = st.pty_output_watch.get_mut(id) {
                    watchers.retain(|w| !w.same_channel(tx));
                    if watchers.is_empty() {
                        st.pty_output_watch.remove(id);
                    }
                }
            }
            _ => {}
        }
    }
}

impl Core {
    /// A wait's answer from the agent's or terminal's current facts; `word` overrides its status.
    pub fn wait_result(&self, target: TargetKey, ended: bool, reason: &str, word: Option<Option<String>>) -> WaitResult {
        let st = self.lock();
        match target {
            TargetKey::Agent(id) => {
                let a = st.reg.agents.get(&id);
                WaitResult {
                    agent: crate::agents::address(id),
                    ended,
                    reason: reason.into(),
                    status: word.unwrap_or_else(|| a.and_then(|a| a.status.as_ref().map(|s| s.state.clone()))),
                    turn: a.map_or(0, |a| a.turn),
                    exit_code: a.and_then(|a| a.exit_code),
                    signal: a.and_then(|a| a.signal),
                    children: Vec::new(),
                    from: None,
                }
            }
            TargetKey::Terminal(id) => {
                let t = st.pty_reg.terminals.get(&id);
                WaitResult {
                    agent: crate::terminals::address(id),
                    ended,
                    reason: reason.into(),
                    status: word.unwrap_or_else(|| t.and_then(|t| t.status.as_ref().map(|s| s.state.clone()))),
                    turn: t.map_or(0, |t| t.turn),
                    exit_code: t.and_then(|t| t.exit_code),
                    signal: t.and_then(|t| t.signal),
                    children: Vec::new(),
                    from: None,
                }
            }
        }
    }

    /// A message sent into `waiter` while it runs wakes its wait too.
    pub async fn wait(&self, target: TargetKey, spec: WaitSpec, waiter: Option<TargetKey>) -> WaitResult {
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = match target {
            TargetKey::Agent(_) => {
                let (otx, orx) = tokio::sync::mpsc::unbounded_channel();
                (OutTx::Agent(otx), OutRx::Agent(orx))
            }
            TargetKey::Terminal(_) => {
                let (otx, orx) = tokio::sync::mpsc::unbounded_channel();
                (OutTx::Terminal(otx), OutRx::Terminal(orx))
            }
        };
        struct Snapshot {
            live: bool,
            turn: u64,
            reported: bool,
            status: Option<String>,
        }
        let start = {
            let mut st = self.lock();
            let filter = match target {
                TargetKey::Agent(id) => Filter { agent: Some(crate::agents::address(id)), ..Default::default() },
                TargetKey::Terminal(id) => Filter { terminal: Some(crate::terminals::address(id)), ..Default::default() },
            };
            self.subscribe(&mut st, filter, Sink::Waiter(tx.clone()));
            if spec.output.is_some() {
                match (&target, &out_tx) {
                    (TargetKey::Agent(id), OutTx::Agent(tx)) => st.output_watch.entry(*id).or_default().push(tx.clone()),
                    (TargetKey::Terminal(id), OutTx::Terminal(tx)) => {
                        st.pty_output_watch.entry(*id).or_default().push(tx.clone())
                    }
                    _ => {}
                }
            }
            match target {
                TargetKey::Agent(id) => st.reg.agents.get(&id).map(|a| Snapshot {
                    live: a.is_live() && !a.program_lost(),
                    turn: a.turn,
                    reported: a.reported,
                    status: a.status.as_ref().map(|s| s.state.clone()),
                }),
                TargetKey::Terminal(id) => st.pty_reg.terminals.get(&id).map(|t| Snapshot {
                    live: t.is_live() && !t.program_lost(),
                    turn: t.turn,
                    reported: t.reported,
                    status: t.status.as_ref().map(|s| s.state.clone()),
                }),
            }
        };
        let _registered = Registered { core: self, target, events: tx, output: out_tx };
        let result = |ended: bool, reason: &str, word: Option<Option<String>>| {
            self.wait_result(target, ended, reason, word)
        };
        let Some(start) = start.filter(|s| s.live) else {
            return result(true, "exit", None);
        };
        if spec.after_turn.is_some_and(|k| start.turn > k) {
            return result(true, "status", None);
        }
        let now = start.status.as_deref();
        let pending = spec.after_turn.is_some() && !stuck(now);
        let already =
            if spec.output.is_some() { stuck(now) } else { start.reported && !pending && ends(Some("working"), now) };
        if already {
            return result(true, "status", None);
        }
        let deadline = spec.timeout.map(|t| tokio::time::Instant::now() + t);
        let mut prev = start.status;
        let mut seen = String::new();
        let mut held = Vec::new();
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(from) = waiter.and_then(|w| self.lock().messages.remove(&w)) {
                return WaitResult { from, ..result(false, "message", None) };
            }
            tokio::select! {
                _ = changed, if waiter.is_some() => {}
                event = events.recv() => {
                    let Some(event) = event else { return result(true, "exit", None) };
                    match event["type"].as_str() {
                        Some("status") => {
                            let new = event["data"]["status"]["state"].as_str().map(String::from);
                            let stop = if spec.output.is_some() { stuck(new.as_deref()) } else { ends(prev.as_deref(), new.as_deref()) };
                            let (gone, cur_turn) = {
                                let st = self.lock();
                                match target {
                                    TargetKey::Agent(id) => {
                                        let a = st.reg.agents.get(&id);
                                        (!a.is_some_and(|a| a.is_live() && !a.program_lost()), a.map_or(0, |a| a.turn))
                                    }
                                    TargetKey::Terminal(id) => {
                                        let t = st.pty_reg.terminals.get(&id);
                                        (!t.is_some_and(|t| t.is_live() && !t.program_lost()), t.map_or(0, |t| t.turn))
                                    }
                                }
                            };
                            let turn_ok = spec.after_turn.is_none_or(|k| cur_turn > k) || stuck(new.as_deref());
                            if stop && turn_ok && !(new.is_none() && gone) {
                                return result(true, "status", Some(new));
                            }
                            prev = new;
                        }
                        Some("finished") | Some("lost") | Some("removed") => {
                            let mut r = result(true, "exit", Some(None));
                            let data = &event["data"];
                            r.exit_code = r.exit_code.or(data["exit_code"].as_i64().map(|c| c as i32));
                            r.signal = r.signal.or(data["signal"].as_i64().map(|s| s as i32));
                            r.turn = r.turn.max(start.turn);
                            return r;
                        }
                        _ => {}
                    }
                }
                Some(chunk) = recv_chunk(&mut out_rx, &mut held) => {
                    seen.push_str(&chunk);
                    if spec.output.as_deref().is_some_and(|needle| seen.contains(needle)) {
                        return result(true, "output", None);
                    }
                    if seen.len() > SEEN_MAX {
                        let cut = seen.len() - SEEN_MAX / 2;
                        let cut = (cut..seen.len()).find(|i| seen.is_char_boundary(*i)).unwrap_or(0);
                        seen.drain(..cut);
                    }
                }
                _ = async {
                    match deadline {
                        Some(d) => tokio::time::sleep_until(d).await,
                        None => std::future::pending::<()>().await,
                    }
                } => return result(false, "timeout", None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_ends_a_wait() {
        assert!(ends(Some("working"), Some("done")));
        assert!(ends(Some("working"), Some("blocked")));
        assert!(ends(Some("working"), Some("idle")));
        assert!(ends(Some("working"), Some("error")));
        assert!(ends(Some("working"), None), "cleared counts as not working");
        assert!(!ends(Some("idle"), Some("idle")), "a repeated word is no change");
        assert!(!ends(Some("idle"), Some("working")));
        assert!(ends(Some("idle"), Some("done")));
    }

    #[test]
    fn output_is_matched_without_escape_sequences() {
        assert_eq!(strip(b"\x1b[31mRE\x1b[0mADY\x1b]2;t\x07!\r\n"), "READY!\n");
    }

    #[test]
    fn a_character_cut_between_chunks_is_matched_whole() {
        let out = "é 준비 ✓ 완료 🚀\n".as_bytes();
        for cut in 0..=out.len() {
            let mut held = Vec::new();
            let text = text_of(&mut held, &out[..cut]) + &text_of(&mut held, &out[cut..]);
            assert_eq!(text, "é 준비 ✓ 완료 🚀\n", "cut at byte {cut}");
            assert!(held.is_empty());
        }
        let mut held = Vec::new();
        let text: String = out.iter().map(|b| text_of(&mut held, &[*b])).collect();
        assert_eq!(text, "é 준비 ✓ 완료 🚀\n", "one byte at a time");
        for bad in [&b"a\xc0"[..], b"a\xe0\x80", b"a\xf4\x90"] {
            assert_eq!(text_of(&mut held, bad), String::from_utf8_lossy(bad), "{bad:?} can never become a character");
            assert!(held.is_empty());
        }
    }
}
