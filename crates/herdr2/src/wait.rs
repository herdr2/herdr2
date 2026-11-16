//! `wait`: until an agent is not working, driven by its events so even a
//! quick flicker counts. Already not working: it returns at once.

use crate::events::{Filter, Sink};
use crate::runtime::Core;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Output kept for `output` matching.
const SEEN_MAX: usize = 64 * 1024;

pub struct WaitSpec {
    /// Ends when output produced after the wait started contains this; then
    /// only blocked, error or exit end it otherwise.
    pub output: Option<String>,
    pub timeout: Duration,
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

/// A wait's subscription and output watch, gone with it however it returns.
struct Registered<'a> {
    core: &'a Core,
    id: u32,
    events: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
    output: tokio::sync::mpsc::UnboundedSender<String>,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        let mut st = self.core.lock();
        self.core.unsubscribe_waiter(&mut st, &self.events);
        if let Some(watchers) = st.output_watch.get_mut(&self.id) {
            watchers.retain(|w| !w.same_channel(&self.output));
            if watchers.is_empty() {
                st.output_watch.remove(&self.id);
            }
        }
    }
}

impl Core {
    /// A wait's answer from the agent's current facts; `word` overrides its status.
    pub fn wait_result(&self, id: u32, ended: bool, reason: &str, word: Option<Option<String>>) -> WaitResult {
        let st = self.lock();
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

    /// A message sent into `waiter` while it runs wakes its wait too.
    pub async fn wait(&self, id: u32, spec: WaitSpec, waiter: Option<u32>) -> WaitResult {
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        let (out_tx, mut output) = tokio::sync::mpsc::unbounded_channel();
        let start = {
            let mut st = self.lock();
            self.subscribe(
                &mut st,
                Filter { agent: Some(crate::agents::address(id)), ..Default::default() },
                Sink::Waiter(tx.clone()),
            );
            if spec.output.is_some() {
                st.output_watch.entry(id).or_default().push(out_tx.clone());
            }
            st.reg.agents.get(&id).cloned()
        };
        let _registered = Registered { core: self, id, events: tx, output: out_tx };
        let result =
            |ended: bool, reason: &str, word: Option<Option<String>>| self.wait_result(id, ended, reason, word);
        let Some(start) = start.filter(|a| a.is_live() && !a.program_lost()) else {
            return result(true, "exit", None);
        };
        if spec.after_turn.is_some_and(|k| start.turn > k) {
            return result(true, "status", None);
        }
        let now = start.status.as_ref().map(|s| s.state.as_str());
        let pending = spec.after_turn.is_some() && !stuck(now);
        let already =
            if spec.output.is_some() { stuck(now) } else { start.reported && !pending && ends(Some("working"), now) };
        if already {
            return result(true, "status", None);
        }
        let deadline = tokio::time::Instant::now() + spec.timeout;
        let mut prev = start.status.map(|s| s.state);
        let mut seen = String::new();
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
                                let a = st.reg.agents.get(&id);
                                (
                                    !a.is_some_and(|a| a.is_live() && !a.program_lost()),
                                    a.map_or(0, |a| a.turn),
                                )
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
                Some(chunk) = output.recv() => {
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
                _ = tokio::time::sleep_until(deadline) => return result(false, "timeout", None),
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
}
