//! ACP message inbox enqueue, steer, and turn cancellation.

use crate::acp::{QueuedPrompt, SessionCommand};
use crate::agents::{Agent, address};
use crate::rpc::RpcError;
use crate::runtime::{Core, State};
use std::time::Duration;

fn working(a: &Agent) -> bool {
    a.status.as_ref().is_some_and(|s| s.state == "working")
}

fn state_word(a: &Agent) -> Option<String> {
    a.status.as_ref().map(|s| s.state.clone())
}

pub struct Sent {
    pub status: Option<String>,
    pub turn: u64,
    pub started: Option<bool>,
    pub after_turn: u64,
    pub inbox_len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupted {
    Stopped,
    Exited,
    StillWorking,
    Unknown,
}

/// Formats an incoming inter-agent prompt with `<h2>` metadata tags when `notes` are enabled.
pub fn format_prompt(text: &str, from: Option<&str>) -> String {
    match from.filter(|s| !s.is_empty()) {
        Some(sender) => format!("<h2 from=\"{sender}\">\n{text}\n</h2>"),
        None => text.to_string(),
    }
}

/// Computes the target `after_turn` threshold for waiting on a newly queued or steered prompt.
pub fn target_after_turn(current_turn: u64, was_working: bool, queued_before: usize, steer: bool) -> u64 {
    if steer {
        current_turn + u64::from(was_working)
    } else {
        current_turn + u64::from(was_working) + queued_before as u64
    }
}

impl Core {
    /// Queues a prompt into the agent's ACP inbox (or cancels & steers ahead of queued turns).
    pub async fn send(
        &self,
        id: u32,
        text: &str,
        steer: bool,
        only_if: Option<&str>,
        from: Option<&str>,
    ) -> Result<Sent, RpcError> {
        let (status, turn, after_turn, inbox_len, was_working, queued_before) = {
            let mut st = self.lock();
            let a = st
                .reg
                .agents
                .get(&id)
                .ok_or_else(|| RpcError::new("not_found", format!("no agent {}", address(id)), ""))?;
            a.check_program()?;
            if !a.is_live() {
                return Err(RpcError::new(
                    "finished",
                    format!("{} already exited", a.address),
                    format!("see `h2 read {}`", a.address),
                ));
            }
            let word = state_word(a);
            if word.as_deref() == Some("blocked") && !steer {
                let kind = a.status.as_ref().and_then(|s| s.kind.as_deref()).unwrap_or("waiting");
                return Err(RpcError::new(
                    "blocked",
                    format!("{} is blocked ({kind})", a.address),
                    format!("approval needed: run `h2 approve {0}` / `h2 deny {0}`, or use --steer", a.address),
                ));
            }
            if let Some(want) = only_if
                && word.as_deref() != Some(want)
            {
                return Err(RpcError::new(
                    "status_mismatch",
                    format!("{} is {}, not {want}", a.address, word.as_deref().unwrap_or("idle")),
                    "nothing was sent",
                ));
            }

            let session = st
                .sessions
                .get(&id)
                .ok_or_else(|| {
                    RpcError::new("unavailable", format!("{} has no active ACP session", a.address), "")
                })?
                .clone_cmd();
            let was_working = working(a);
            let turn = a.turn;
            let queued_before = st.inboxes.get(&id).map_or(0, std::collections::VecDeque::len);
            let after_turn = target_after_turn(turn, was_working, queued_before, steer);
            let prompt = QueuedPrompt { text: text.to_string(), from: from.map(String::from) };

            let inbox_len = if steer {
                let _ = session.send(SessionCommand::Steer(prompt));
                queued_before + 1
            } else {
                st.inboxes.entry(id).or_default().push_back(prompt);
                let len = st.inboxes.get(&id).map_or(0, std::collections::VecDeque::len);
                if let Some(agent) = st.reg.agents.get_mut(&id) {
                    agent.inbox_len = len;
                }
                let _ = session.send(SessionCommand::WakeInbox);
                len
            };
            (word, turn, after_turn, inbox_len, was_working, queued_before)
        };

        let started = if !was_working && queued_before == 0 {
            let went = |st: &State| st.reg.agents.get(&id).is_none_or(|a| a.turn > turn || working(a) || !a.is_live());
            self.wait_for(Duration::from_millis(500), went).await;
            let st = self.lock();
            Some(st.reg.agents.get(&id).is_some_and(|a| a.turn > turn || working(a)))
        } else {
            Some(true)
        };

        Ok(Sent { status, turn, started, after_turn, inbox_len })
    }

    /// Cancels the active ACP turn via `session/cancel` and waits up to `timeout` for idle/exit.
    pub async fn interrupt(&self, id: u32, timeout: Duration) -> Result<(Interrupted, String), RpcError> {
        {
            let st = self.lock();
            let a = st
                .reg
                .agents
                .get(&id)
                .ok_or_else(|| RpcError::new("not_found", format!("no agent {}", address(id)), ""))?;
            a.check_program()?;
            if !a.is_live() {
                return Ok((Interrupted::Exited, "session/cancel".into()));
            }
            if let Some(session) = st.sessions.get(&id) {
                let _ = session.cmd_tx.send(SessionCommand::Interrupt);
            }
        }
        let settled = |st: &State| st.reg.agents.get(&id).is_none_or(|a| !a.is_live() || !working(a));
        self.wait_for(timeout, settled).await;
        let st = self.lock();
        let outcome = match st.reg.agents.get(&id) {
            Some(a) if !a.is_live() => Interrupted::Exited,
            Some(a) if working(a) => Interrupted::StillWorking,
            _ => Interrupted::Stopped,
        };
        Ok((outcome, "session/cancel".into()))
    }
}

trait SessionSender {
    fn clone_cmd(&self) -> tokio::sync::mpsc::UnboundedSender<SessionCommand>;
}

impl SessionSender for crate::acp::SessionHandle {
    fn clone_cmd(&self) -> tokio::sync::mpsc::UnboundedSender<SessionCommand> {
        self.cmd_tx.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_after_turn_for_idle_queued_and_steered_prompts() {
        assert_eq!(target_after_turn(1, false, 0, false), 1);
        assert_eq!(target_after_turn(1, true, 0, false), 2);
        assert_eq!(target_after_turn(1, true, 2, false), 4);
        assert_eq!(target_after_turn(1, true, 2, true), 2);
        assert_eq!(target_after_turn(1, false, 2, true), 1);
    }

    #[test]
    fn formats_inter_agent_prompts() {
        assert_eq!(format_prompt("hi", None), "hi");
        assert_eq!(format_prompt("hi", Some("a1")), "<h2 from=\"a1\">\nhi\n</h2>");
    }
}
