//! Parents and children: news about a child is queued into its parent's ACP
//! inbox as a marked note, only when the parent can take it and hasn't seen it.

use crate::acp::{QueuedPrompt, SessionCommand};
use crate::agents::{Agent, parse_id};
use crate::rpc::RpcError;
use crate::runtime::{Core, State, now_ms};
use std::sync::Arc;
use std::time::Duration;

/// Work that can't happen on the thread that noticed it.
pub enum Job {
    /// Deliver pending notes into this parent's ACP inbox.
    Notes(u32),
    /// The parent finished: end children that asked to die with it.
    Orphaned(u32),
    /// News for a parent on a core that dialed in (`desktop/a3`).
    NotesAfar(String),
}

fn parent_id(a: &Agent) -> Option<u32> {
    a.parent.as_deref().and_then(parse_id)
}

fn says(a: &Agent, word: &str) -> bool {
    a.is_live() && a.status.as_ref().is_some_and(|s| s.state == word)
}

fn blocked(a: &Agent) -> bool {
    says(a, "blocked")
}

/// Blocked or errored: stuck in a way it can't report itself.
fn stuck(a: &Agent) -> bool {
    blocked(a) || says(a, "error")
}

/// Trailing lines of a child transcript/error when it exits without reporting.
fn last_lines(text: &str) -> Vec<String> {
    const LINES: usize = 20;
    const WIDTH: usize = 300;
    let mut lines: Vec<String> = text
        .lines()
        .rev()
        .map(|l| l.chars().filter(|c| !c.is_control() || *c == '\t').take(WIDTH).collect::<String>())
        .filter(|l| !l.trim().is_empty())
        .take(LINES)
        .collect();
    lines.reverse();
    lines
}

/// A parent can take a note when live, initialized, not blocked on an approval,
/// and backed by an active ACP session.
fn can_take_note(a: &Agent) -> bool {
    a.is_live() && a.reported && !blocked(a) && a.check_program().is_ok()
}

/// `a7 reviewer`: address, then name or template.
pub fn who(a: &Agent) -> String {
    match a.name.as_ref().or(a.template.as_ref()) {
        Some(name) => format!("{} {name}", a.address),
        None => a.address.clone(),
    }
}

/// What a child can't say itself: it is blocked, it hit an error, or it is
/// gone. Everything else it reports with `h2 send`.
fn note(a: &Agent, address: &str, last: &[String]) -> String {
    let status = a.status.as_ref();
    let (what, look) = if blocked(a) {
        let kind = status.and_then(|s| s.kind.as_deref()).unwrap_or("waiting");
        (format!("is blocked ({kind})"), "See")
    } else if stuck(a) {
        match status.and_then(|s| s.msg.lines().next()).map(|m| m.trim_end_matches('.')).filter(|m| !m.is_empty()) {
            Some(msg) => (format!("hit an error: {}", msg.chars().take(200).collect::<String>()), "See"),
            None => ("hit an error".into(), "See"),
        }
    } else {
        let how = match (a.exit_code, a.signal) {
            (Some(code), _) => format!("exit {code}"),
            (_, Some(sig)) => format!("signal {sig}"),
            _ => a.reason.clone().unwrap_or_else(|| "ended".into()),
        };
        (format!("exited ({how})"), "Read")
    };
    let name = a.name.as_ref().or(a.template.as_ref()).map(|n| format!(" {n}")).unwrap_or_default();
    let gap = match last.is_empty() {
        true => ". ".to_string(),
        false => format!(". Last lines:\n{}\n", last.join("\n")),
    };
    format!("<h2>{address}{name} {what}{gap}{look}: h2 read {address}</h2>")
}

impl Core {
    /// The note about child `id`, called `address` by its parent.
    fn note_for(&self, st: &State, id: u32, address: &str) -> String {
        let a = &st.reg.agents[&id];
        let last = if !a.is_live() && a.exit_code.is_some_and(|c| c != 0) {
            let text = st.transcripts.get(&id).map(|t| t.render(None, false).0).unwrap_or_default();
            last_lines(&text)
        } else {
            Vec::new()
        };
        note(a, address, &last)
    }

    /// Something notable happened to `id` (a turn ended, it got stuck, it exited).
    pub fn news(&self, st: &mut State, id: u32) {
        let Some(a) = st.reg.agents.get_mut(&id) else { return };
        a.news += 1;
        if std::mem::take(&mut a.told_parent) && !stuck(a) && a.is_live() {
            a.seen = a.news;
        }
        match (parent_id(a), &a.parent) {
            (Some(p), _) if a.notify => drop(self.jobs.send(Job::Notes(p))),
            (None, Some(afar)) if a.notify => drop(self.jobs.send(Job::NotesAfar(afar.clone()))),
            _ => {}
        }
    }

    /// The caller looked at `id`; if it is the parent, that news is seen.
    pub fn saw(&self, caller: Option<&str>, id: u32) {
        let mut st = self.lock();
        if let Some(a) = st.reg.agents.get_mut(&id)
            && caller.is_some()
            && a.parent.as_deref() == caller
            && a.seen < a.news
        {
            a.seen = a.news;
            a.seen_ms = Some(now_ms());
            self.save(&st);
        }
    }

    /// Text from the agent `from` (a caller address) to the agent `to`, wrapped in
    /// `<h2>` tags attributing the sender.
    pub fn tagged(&self, from: Option<&str>, to: u32, text: &str, reach: bool) -> String {
        if !self.notes_on() {
            return text.to_string();
        }
        let st = self.lock();
        let sender = from.and_then(parse_id).filter(|f| *f != to).and_then(|f| st.reg.agents.get(&f));
        let target = st.reg.agents.get(&to);
        if let Some(afar) = from.filter(|f| f.rsplit_once('/').is_some_and(|(_, local)| parse_id(local).is_some()))
            && target.is_some_and(|a| a.reported)
        {
            return format!("<h2 from=\"{afar}\">{text}</h2>");
        }
        match sender.filter(|s| s.reported && target.is_some_and(|a| a.reported)) {
            Some(s) if reach => {
                format!("<h2 from=\"{}\" reach=\"h2 send {} when done or stuck\">{text}</h2>", who(s), s.address)
            }
            Some(s) => format!("<h2 from=\"{}\">{text}</h2>", who(s)),
            None => text.to_string(),
        }
    }

    /// Children of `parent` that are stuck or gone with news it hasn't seen,
    /// been told, or isn't already waiting for.
    fn unseen(&self, st: &State, address: &str) -> Vec<u32> {
        let address = address.to_string();
        st.reg
            .agents
            .values()
            .filter(|a| a.parent.as_deref() == Some(address.as_str()) && a.notify)
            .filter(|a| a.news > a.seen.max(a.noted) && (stuck(a) || !a.is_live()))
            .filter(|a| !st.waiting.contains_key(&(address.clone(), a.id())))
            .map(Agent::id)
            .collect()
    }

    /// Runs jobs for as long as the core runs.
    pub async fn run_jobs(self: Arc<Self>, mut jobs: tokio::sync::mpsc::UnboundedReceiver<Job>) {
        while let Some(job) = jobs.recv().await {
            match job {
                Job::Notes(parent) => {
                    let start = self.lock().note_tasks.insert(parent);
                    if start {
                        tokio::spawn(self.clone().deliver_notes(parent));
                    }
                }
                Job::NotesAfar(parent) => {
                    let mut st = self.lock();
                    let Some(called) = crate::remote::called_there(&st, &parent) else { continue };
                    let kids = self.unseen(&st, &parent);
                    if kids.is_empty() {
                        continue;
                    }
                    let notes: Vec<String> = kids
                        .iter()
                        .map(|id| self.note_for(&st, *id, &format!("{called}/{}", crate::agents::address(*id))))
                        .collect();
                    for id in &kids {
                        let a = st.reg.agents.get_mut(id).expect("listed");
                        a.noted = a.noted.max(a.news);
                    }
                    self.save(&st);
                    crate::remote::tell_peer(&self, &mut st, &parent, &notes);
                }
                Job::Orphaned(parent) => {
                    let address = crate::agents::address(parent);
                    let kids: Vec<u32> = {
                        let mut st = self.lock();
                        let kids: Vec<u32> = st
                            .reg
                            .agents
                            .values()
                            .filter(|a| {
                                a.is_live() && a.die_with_parent && a.parent.as_deref() == Some(address.as_str())
                            })
                            .map(Agent::id)
                            .collect();
                        st.killing.extend(&kids);
                        kids
                    };
                    let core = self.clone();
                    tokio::spawn(async move { core.kill(&kids).await });
                }
            }
        }
    }

    fn notes_on(&self) -> bool {
        crate::config::load(&self.paths.config).map(|c| c.notes.unwrap_or(true)).unwrap_or(true)
    }

    async fn deliver_notes(self: Arc<Self>, parent: u32) {
        let enabled = self.notes_on();
        loop {
            let ready = |st: &State| st.reg.agents.get(&parent).is_none_or(|a| !a.is_live() || can_take_note(a));
            while !self.wait_for(Duration::from_secs(60), ready).await {}
            let alive = self.lock().reg.agents.get(&parent).is_some_and(|a| a.is_live());
            if !alive || !enabled {
                break;
            }
            if !self.queue_note_batch(parent) {
                let mut st = self.lock();
                if self.unseen(&st, &crate::agents::address(parent)).is_empty() && !st.afar_notes.contains_key(&parent) {
                    st.note_tasks.remove(&parent);
                    return;
                }
            }
        }
        self.lock().note_tasks.remove(&parent);
    }

    fn queue_note_batch(&self, parent: u32) -> bool {
        let mut st = self.lock();
        let Some(p) = st.reg.agents.get(&parent) else {
            return false;
        };
        if !can_take_note(p) || !st.sessions.contains_key(&parent) {
            return false;
        }
        let afar = st.afar_notes.remove(&parent).unwrap_or_default();
        let kids = self.unseen(&st, &crate::agents::address(parent));
        if kids.is_empty() && afar.is_empty() {
            return false;
        }
        let mut text: Vec<String> =
            kids.iter().map(|id| self.note_for(&st, *id, &st.reg.agents[id].address)).collect();
        text.extend(afar);
        let told: Vec<(u32, u64)> = kids.iter().map(|id| (*id, st.reg.agents[id].news)).collect();
        for (id, news) in &told {
            if let Some(a) = st.reg.agents.get_mut(id) {
                a.noted = a.noted.max(*news);
            }
        }
        st.inboxes.entry(parent).or_default().push_back(QueuedPrompt { text: text.join("\n"), from: None });
        let inbox_len = st.inboxes.get(&parent).map_or(0, std::collections::VecDeque::len);
        if let Some(p) = st.reg.agents.get_mut(&parent) {
            p.inbox_len = inbox_len;
        }
        if let Some(session) = st.sessions.get(&parent) {
            let _ = session.cmd_tx.send(SessionCommand::WakeInbox);
        }
        self.save(&st);
        true
    }
}

/// Helper kept for compatibility with callers expecting an error when messaging absent targets.
pub fn missing_target_error(id: &str) -> RpcError {
    RpcError::new("not_found", format!("no agent {id:?}"), "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{NewAgent, Registry};

    #[test]
    fn notes_say_what_happened_and_where_to_look() {
        let mut r = Registry::default();
        let id = r
            .create(NewAgent { name: Some("reviewer".into()), argv: vec!["x".into()], ..Default::default() }, 0)
            .unwrap();
        let a = r.agents.get_mut(&id).unwrap();
        a.turn = 2;
        a.status = Some(crate::status::Status {
            state: "done".into(),
            kind: None,
            progress: None,
            app: String::new(),
            title: String::new(),
            msg: String::new(),
        });
        a.status.as_mut().unwrap().state = "blocked".into();
        a.status.as_mut().unwrap().kind = Some("permission".into());
        assert_eq!(note(a, "a1", &[]), "<h2>a1 reviewer is blocked (permission). See: h2 read a1</h2>");
        a.status.as_mut().unwrap().state = "error".into();
        a.status.as_mut().unwrap().msg = format!("rate limited\n{}", "x".repeat(500));
        assert_eq!(
            note(a, "a1", &[]),
            "<h2>a1 reviewer hit an error: rate limited. See: h2 read a1</h2>",
            "its first line"
        );
        a.status.as_mut().unwrap().msg.clear();
        assert_eq!(note(a, "a1", &[]), "<h2>a1 reviewer hit an error. See: h2 read a1</h2>");
        r.finish(id, Some(3), None, "exited", 1);
        assert_eq!(
            note(&r.agents[&id], "rohan/a1", &[]),
            "<h2>rohan/a1 reviewer exited (exit 3). Read: h2 read rohan/a1</h2>"
        );
        let last = last_lines("a\n\n  \nb\x1b[201~\tc\n");
        assert_eq!(last, ["a", "b[201~\tc"], "blanks and control characters dropped");
        assert_eq!(
            note(&r.agents[&id], "a1", &last),
            "<h2>a1 reviewer exited (exit 3). Last lines:\na\nb[201~\tc\nRead: h2 read a1</h2>"
        );
    }

    #[test]
    fn a_kept_parent_without_an_active_session_takes_no_note_until_restarted() {
        let mut r = Registry::default();
        let id = r.create(NewAgent { argv: vec!["x".into()], keep: true, ..Default::default() }, 0).unwrap();
        let a = r.agents.get_mut(&id).unwrap();
        a.reported = true;
        a.pid = Some(42);
        assert!(can_take_note(a));
        for reason in ["signaled", "spawn_pending"] {
            a.pid = None;
            a.reason = Some(reason.into());
            assert!(!can_take_note(a), "{reason}: queue would wait until session recovers");
        }
        a.pid = Some(43);
        a.reason = None;
        a.reported = false;
        assert!(!can_take_note(a));
        a.reported = true;
        assert!(can_take_note(a));
    }
}
