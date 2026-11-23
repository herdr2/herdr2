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

/// Replaces the message body while preserving an outer `<h2 ...>...</h2>` envelope when present.
pub fn replace_prompt_body(existing: &str, new_body: &str) -> String {
    if new_body.trim_start().starts_with("<h2") {
        return new_body.to_string();
    }
    let trimmed = existing.trim();
    if trimmed.starts_with("<h2")
        && let Some(gt) = trimmed.find('>')
        && trimmed.ends_with("</h2>")
    {
        return format!("{}\n{}\n</h2>", &trimmed[..=gt], new_body);
    }
    new_body.to_string()
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
    /// Queues a plain-text prompt into the agent's ACP inbox (or cancels & steers ahead of queued turns).
    pub async fn send(
        &self,
        id: u32,
        text: &str,
        steer: bool,
        only_if: Option<&str>,
        from: Option<&str>,
    ) -> Result<Sent, RpcError> {
        self.send_with_attachments(id, text, Vec::new(), steer, only_if, from).await
    }

    /// Queues a prompt with optional staged multimodal attachments into the agent's ACP inbox.
    pub async fn send_with_attachments(
        &self,
        id: u32,
        text: &str,
        attachments: Vec<crate::transcript::Attachment>,
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

            let was_working = working(a);
            let turn = a.turn;
            let agent_addr = a.address.clone();
            let queued_before = st.inboxes.get(&id).map_or(0, std::collections::VecDeque::len);
            let after_turn = target_after_turn(turn, was_working, queued_before, steer);
            let prompt = QueuedPrompt::with_attachments(text.to_string(), attachments, from.map(String::from));

            let Some(session) = st.sessions.get(&id).map(SessionSender::clone_cmd) else {
                if st.recovering.contains(&id) {
                    let q = st.inboxes.entry(id).or_default();
                    if steer {
                        q.push_front(prompt);
                    } else {
                        q.push_back(prompt);
                    }
                    self.sync_inbox(&mut st, id, true);
                    let len = st.reg.agents.get(&id).map_or(0, |a| a.inbox_len);
                    self.save(&st);
                    self.changed.notify_waiters();
                    return Ok(Sent { status: word, turn, started: Some(false), after_turn, inbox_len: len });
                }
                return Err(RpcError::new("unavailable", format!("{agent_addr} has no active ACP session"), ""));
            };

            let inbox_len = if steer {
                let _ = session.send(SessionCommand::Steer(prompt));
                queued_before + 1
            } else {
                st.inboxes.entry(id).or_default().push_back(prompt);
                self.sync_inbox(&mut st, id, true);
                let len = st.reg.agents.get(&id).map_or(0, |a| a.inbox_len);
                self.save_soon();
                self.changed.notify_waiters();
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

    /// Updates a queued inbox item's prompt text in place before its turn begins.
    pub fn edit_inbox(&self, id: u32, item_id: u64, text: &str) -> Result<Agent, RpcError> {
        if text.trim().is_empty() {
            return Err(RpcError::invalid_params("queued prompt text cannot be empty"));
        }
        let mut st = self.lock();
        let addr = {
            let a = st
                .reg
                .agents
                .get(&id)
                .ok_or_else(|| RpcError::new("not_found", format!("no agent {}", address(id)), ""))?;
            if !a.is_live() {
                return Err(RpcError::new("finished", format!("{} already exited", a.address), ""));
            }
            a.address.clone()
        };
        let Some(item) = st.inboxes.get_mut(&id).and_then(|q| q.iter_mut().find(|item| item.id == item_id)) else {
            return Err(RpcError::new(
                "not_found",
                format!("queued message #{item_id} is no longer in {addr}'s inbox"),
                "it may have already started; refresh transcript",
            ));
        };
        item.text = replace_prompt_body(&item.text, text);
        self.sync_inbox(&mut st, id, true);
        self.save(&st);
        self.changed.notify_waiters();
        Ok(st.reg.agents.get(&id).expect("checked agent").clone())
    }

    /// Removes a queued inbox item before its turn begins.
    pub fn remove_inbox(&self, id: u32, item_id: u64) -> Result<Agent, RpcError> {
        let mut st = self.lock();
        let addr = {
            let a = st
                .reg
                .agents
                .get(&id)
                .ok_or_else(|| RpcError::new("not_found", format!("no agent {}", address(id)), ""))?;
            if !a.is_live() {
                return Err(RpcError::new("finished", format!("{} already exited", a.address), ""));
            }
            a.address.clone()
        };
        let removed = if let Some(q) = st.inboxes.get_mut(&id) {
            if let Some(pos) = q.iter().position(|item| item.id == item_id) {
                q.remove(pos);
                true
            } else {
                false
            }
        } else {
            false
        };
        if !removed {
            return Err(RpcError::new(
                "not_found",
                format!("queued message #{item_id} is no longer in {addr}'s inbox"),
                "it may have already started; refresh transcript",
            ));
        }
        if st.inboxes.get(&id).is_some_and(std::collections::VecDeque::is_empty) {
            st.inboxes.remove(&id);
        }
        self.sync_inbox(&mut st, id, true);
        self.save(&st);
        self.changed.notify_waiters();
        Ok(st.reg.agents.get(&id).expect("checked agent").clone())
    }

    /// Removes a queued inbox item and immediately steers it ahead of the queue (cancelling any active turn).
    pub fn send_inbox_now(&self, id: u32, item_id: u64) -> Result<Agent, RpcError> {
        let mut st = self.lock();
        let addr = {
            let a = st
                .reg
                .agents
                .get(&id)
                .ok_or_else(|| RpcError::new("not_found", format!("no agent {}", address(id)), ""))?;
            a.check_program()?;
            if !a.is_live() {
                return Err(RpcError::new("finished", format!("{} already exited", a.address), ""));
            }
            a.address.clone()
        };
        let removed = if let Some(q) = st.inboxes.get_mut(&id) {
            if let Some(pos) = q.iter().position(|item| item.id == item_id) {
                q.remove(pos)
            } else {
                None
            }
        } else {
            None
        };
        let Some(item) = removed else {
            return Err(RpcError::new(
                "not_found",
                format!("queued message #{item_id} is no longer in {addr}'s inbox"),
                "it may have already started; refresh transcript",
            ));
        };
        let session = st.sessions.get(&id).map(SessionSender::clone_cmd);
        if let Some(session) = session {
            if st.inboxes.get(&id).is_some_and(std::collections::VecDeque::is_empty) {
                st.inboxes.remove(&id);
            }
            self.sync_inbox(&mut st, id, true);
            self.save(&st);
            self.changed.notify_waiters();
            let _ = session.send(SessionCommand::Steer(item));
        } else if st.recovering.contains(&id) {
            st.inboxes.entry(id).or_default().push_front(item);
            self.sync_inbox(&mut st, id, true);
            self.save(&st);
            self.changed.notify_waiters();
        } else {
            st.inboxes.entry(id).or_default().push_front(item);
            self.sync_inbox(&mut st, id, true);
            return Err(RpcError::new("unavailable", format!("{addr} has no active ACP session"), ""));
        }
        Ok(st.reg.agents.get(&id).expect("checked agent").clone())
    }

    /// Moves all paused inbox items (saved across an unexpected stop/reboot) back into the live
    /// inbox queue and wakes the ACP actor to process them in FIFO order.
    pub fn resume_paused_inbox(&self, id: u32) -> Result<Agent, RpcError> {
        let mut st = self.lock();
        let paused = {
            let a = st
                .reg
                .agents
                .get_mut(&id)
                .ok_or_else(|| RpcError::new("not_found", format!("no agent {}", address(id)), ""))?;
            a.check_program()?;
            if !a.is_live() {
                return Err(RpcError::new("finished", format!("{} already exited", a.address), ""));
            }
            std::mem::take(&mut a.paused_inbox)
        };
        if !paused.is_empty() {
            let queue = st.inboxes.entry(id).or_default();
            for item in paused {
                let mut prompt = QueuedPrompt::with_attachments(item.text, item.attachments, item.from);
                prompt.queued_ms = item.queued_ms;
                queue.push_back(prompt);
            }
        }
        let session = st.sessions.get(&id).map(SessionSender::clone_cmd);
        let payload = st.reg.agents.get(&id).map(|a| {
            serde_json::json!({
                "status": a.status,
                "records": a.records,
                "reported": a.reported,
                "pending_permission": a.pending_permission,
                "inbox_len": st.inboxes.get(&id).map_or(0, std::collections::VecDeque::len),
                "inbox": a.inbox,
                "paused_inbox": a.paused_inbox,
            })
        });
        self.sync_inbox(&mut st, id, true);
        if let Some(p) = payload {
            self.emit(&mut st, "status", id, p);
        }
        self.save(&st);
        self.changed.notify_waiters();
        if let Some(session) = session {
            let _ = session.send(SessionCommand::WakeInbox);
        }
        Ok(st.reg.agents.get(&id).expect("checked agent").clone())
    }

    /// Discards all paused inbox items preserved from before a restart/reboot.
    pub fn clear_paused_inbox(&self, id: u32) -> Result<Agent, RpcError> {
        let mut st = self.lock();
        let changed = {
            let a = st
                .reg
                .agents
                .get_mut(&id)
                .ok_or_else(|| RpcError::new("not_found", format!("no agent {}", address(id)), ""))?;
            if !a.is_live() {
                return Err(RpcError::new("finished", format!("{} already exited", a.address), ""));
            }
            let had = !a.paused_inbox.is_empty();
            a.paused_inbox.clear();
            had
        };
        if changed {
            if let Some(a) = st.reg.agents.get(&id) {
                let payload = serde_json::json!({
                    "status": a.status,
                    "records": a.records,
                    "reported": a.reported,
                    "pending_permission": a.pending_permission,
                    "inbox_len": a.inbox_len,
                    "inbox": a.inbox,
                    "paused_inbox": a.paused_inbox,
                });
                self.emit(&mut st, "status", id, payload);
            }
            self.save(&st);
            self.changed.notify_waiters();
        }
        Ok(st.reg.agents.get(&id).expect("checked agent").clone())
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

    #[tokio::test]
    async fn queues_prompts_while_acp_session_is_starting() {
        let dir = std::env::temp_dir().join(format!("h2-input-queue-starting-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = crate::paths::Paths {
            socket: dir.join("core.sock"),
            holder_socket: dir.join("core.holder.sock"),
            state: dir.join("state"),
            config: dir.join("config.toml"),
        };
        paths.create_dirs().unwrap();
        let core = Core::start(
            paths,
            crate::api::CoreInfo {
                id: "test-id".into(),
                name: "test".into(),
            },
        )
        .unwrap();
        let id = {
            let mut st = core.lock();
            let id = st
                .reg
                .create(
                    crate::agents::NewAgent {
                        template: None,
                        die_with_parent: false,
                        parent: None,
                        notify: true,
                        rm: false,
                        keep: false,
                        name: None,
                        labels: Default::default(),
                        argv: vec!["agy".into(), "acp".into()],
                        cwd: dir.display().to_string(),
                        agent: Some("agy".into()),
                        system_prompt: None,
                        model: None,
                        mode: None,
                        inject_mcp: false,
                        permission_mode: crate::permissions::PermissionMode::ApproveAll,
                        lineage: None,
                    },
                    crate::runtime::now_ms(),
                )
                .unwrap();
            st.recovering.insert(id);
            id
        };

        let sent = core.send(id, "first queued", false, None, None).await.unwrap();
        assert_eq!(sent.inbox_len, 1);
        assert_eq!(sent.started, Some(false));
        assert_eq!(sent.after_turn, 0);

        let sent2 = core.send(id, "second queued", false, None, Some("a9")).await.unwrap();
        assert_eq!(sent2.inbox_len, 2);
        assert_eq!(sent2.after_turn, 1);

        {
            let st = core.lock();
            let q = st.inboxes.get(&id).unwrap();
            assert_eq!(q.len(), 2);
            assert_eq!(q[0].text, "first queued");
            assert_eq!(q[1].text, "second queued");
            assert_eq!(st.reg.agents.get(&id).unwrap().inbox_len, 2);
            assert_eq!(st.reg.agents.get(&id).unwrap().inbox.len(), 2);
        }

        let item2_id = core.lock().inboxes[&id][1].id;
        let updated = core.edit_inbox(id, item2_id, "edited second").unwrap();
        assert_eq!(updated.inbox[1].text, "edited second");

        let err = core.edit_inbox(id, item2_id, "   ").unwrap_err();
        assert_eq!(err.kind, "invalid_params");

        let after_rm = core.remove_inbox(id, item2_id).unwrap();
        assert_eq!(after_rm.inbox_len, 1);
        assert_eq!(after_rm.inbox[0].text, "first queued");

        let err = core.remove_inbox(id, item2_id).unwrap_err();
        assert_eq!(err.kind, "not_found");

        // Re-enqueue a second item and verify send_inbox_now promotes it to front while recovering.
        core.send(id, "urgent second", false, None, Some("a7")).await.unwrap();
        let urgent_id = core.lock().inboxes[&id][1].id;
        let promoted = core.send_inbox_now(id, urgent_id).unwrap();
        assert_eq!(promoted.inbox_len, 2);
        assert_eq!(promoted.inbox[0].id, urgent_id);
        assert_eq!(promoted.inbox[0].text, "urgent second");
        assert_eq!(promoted.inbox[0].from.as_deref(), Some("a7"));

        let missing = core.send_inbox_now(id, 999_999).unwrap_err();
        assert_eq!(missing.kind, "not_found");

        // If session is unavailable (not recovering), send_inbox_now preserves the item instead of dropping it.
        core.lock().recovering.remove(&id);
        let unavail = core.send_inbox_now(id, urgent_id).unwrap_err();
        assert_eq!(unavail.kind, "unavailable");
        assert_eq!(core.lock().inboxes[&id][0].id, urgent_id);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replaces_prompt_body_preserving_h2_envelope() {
        assert_eq!(replace_prompt_body("plain old", "plain new"), "plain new");
        assert_eq!(
            replace_prompt_body("<h2 from=\"a1\">\nold body\n</h2>", "new body"),
            "<h2 from=\"a1\">\nnew body\n</h2>"
        );
        assert_eq!(
            replace_prompt_body("<h2 from=\"a1\">\nold\n</h2>", "<h2 from=\"a2\">\ncustom\n</h2>"),
            "<h2 from=\"a2\">\ncustom\n</h2>"
        );
    }
}
