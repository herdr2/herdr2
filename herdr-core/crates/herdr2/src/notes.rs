//! Parents and children: news about a child is queued into its parent's ACP
//! inbox (or typed into a parent PTY terminal) as a marked note, only when the
//! parent can take it and hasn't seen it.

use crate::acp::{QueuedPrompt, SessionCommand};
use crate::agents::Agent;
#[allow(unused_imports)]
use crate::pty_input::{Step, paste};
#[allow(unused_imports)]
use crate::rpc::RpcError;
use crate::runtime::{Core, State, TargetKey, now_ms};
use crate::terminals::Terminal;
use std::sync::Arc;
use std::time::Duration;

/// Work that can't happen on the thread that noticed it.
pub enum Job {
    /// Deliver pending notes into this parent's ACP inbox or PTY terminal.
    Notes(TargetKey),
    /// The parent finished: end children that asked to die with it.
    Orphaned(TargetKey),
    /// News for a parent on a core that dialed in (`desktop/a3` or `desktop/t3`).
    NotesAfar(String),
    /// PTY terminal status transitioned (`working` or `turn_ended`): capture baseline/turn checkpoint on Tokio.
    PtyCheckpoint { id: u32, entered_working: bool, turn_ended: bool, turn: u64 },
}

fn parent_key(parent: Option<&str>) -> Option<TargetKey> {
    parent.and_then(TargetKey::parse)
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

fn pty_says(t: &Terminal, word: &str) -> bool {
    t.is_live() && t.status.as_ref().is_some_and(|s| s.state == word)
}

fn pty_blocked(t: &Terminal) -> bool {
    pty_says(t, "blocked")
}

fn pty_stuck(t: &Terminal) -> bool {
    pty_blocked(t) || pty_says(t, "error")
}

/// Trailing lines of a child transcript/screen when it exits without reporting.
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

/// A parent ACP agent can take a note when live, initialized, not blocked,
/// and backed by an active ACP session.
fn can_take_note(a: &Agent) -> bool {
    a.is_live() && a.reported && !blocked(a) && a.check_program().is_ok()
}

/// A parent PTY terminal can take a note when live, reported as an agent, not
/// on a permission dialog, and running its program.
#[allow(dead_code)]
fn pty_can_take_note(t: &Terminal) -> bool {
    t.is_live() && t.reported && !pty_blocked(t) && t.check_program().is_ok()
}

/// `a7 reviewer`: address, then name or template.
pub fn who(a: &Agent) -> String {
    match a.name.as_ref().or(a.template.as_ref()) {
        Some(name) => format!("{} {name}", a.address),
        None => a.address.clone(),
    }
}

/// `t7 reviewer`: address, then name or template.
pub fn pty_who(t: &Terminal) -> String {
    match t.name.as_ref().or(t.template.as_ref()) {
        Some(name) => format!("{} {name}", t.address),
        None => t.address.clone(),
    }
}

fn format_note(
    status: Option<&crate::status::Status>,
    is_blocked: bool,
    is_stuck: bool,
    exit_code: Option<i32>,
    signal: Option<i32>,
    reason: Option<&str>,
    name: Option<&str>,
    address: &str,
    last: &[String],
) -> String {
    let (what, look) = if is_blocked {
        let kind = status.and_then(|s| s.kind.as_deref()).unwrap_or("waiting");
        (format!("is blocked ({kind})"), "See")
    } else if is_stuck {
        match status.and_then(|s| s.msg.lines().next()).map(|m| m.trim_end_matches('.')).filter(|m| !m.is_empty()) {
            Some(msg) => (format!("hit an error: {}", msg.chars().take(200).collect::<String>()), "See"),
            None => ("hit an error".into(), "See"),
        }
    } else {
        let how = match (exit_code, signal) {
            (Some(code), _) => format!("exit {code}"),
            (_, Some(sig)) => format!("signal {sig}"),
            _ => reason.unwrap_or("ended").to_string(),
        };
        (format!("exited ({how})"), "Read")
    };
    let name = name.map(|n| format!(" {n}")).unwrap_or_default();
    let gap = match last.is_empty() {
        true => ". ".to_string(),
        false => format!(". Last lines:\n{}\n", last.join("\n")),
    };
    format!("<h2>{address}{name} {what}{gap}{look}: h2 read {address}</h2>")
}

/// What a child can't say itself: it is blocked, it hit an error, or it is gone.
fn note(a: &Agent, address: &str, last: &[String]) -> String {
    format_note(
        a.status.as_ref(),
        blocked(a),
        stuck(a),
        a.exit_code,
        a.signal,
        a.reason.as_deref(),
        a.name.as_ref().or(a.template.as_ref()).map(String::as_str),
        address,
        last,
    )
}

fn pty_note(t: &Terminal, address: &str, last: &[String]) -> String {
    format_note(
        t.status.as_ref(),
        pty_blocked(t),
        pty_stuck(t),
        t.exit_code,
        t.signal,
        t.reason.as_deref(),
        t.name.as_ref().or(t.template.as_ref()).map(String::as_str),
        address,
        last,
    )
}

impl Core {
    /// The note about ACP child `id`, called `address` by its parent.
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

    /// The note about PTY child `id`, called `address` by its parent.
    fn pty_note_for(&self, st: &State, id: u32, address: &str) -> String {
        let t = &st.pty_reg.terminals[&id];
        let last = if !t.is_live() && !t.reported {
            last_lines(&self.screen_text(st, id).unwrap_or_default())
        } else {
            Vec::new()
        };
        pty_note(t, address, &last)
    }

    /// Something notable happened to ACP child `id`.
    pub fn news(&self, st: &mut State, id: u32) {
        let Some(a) = st.reg.agents.get_mut(&id) else { return };
        a.news += 1;
        if std::mem::take(&mut a.told_parent) && !stuck(a) && a.is_live() {
            a.seen = a.news;
        }
        match (parent_key(a.parent.as_deref()), &a.parent) {
            // Only ACP parents receive background note injections into their inbox;
            // PTY terminal agents collect updates exclusively via `h2 wait` / `h2 read`.
            (Some(p @ TargetKey::Agent(_)), _) if a.notify => drop(self.jobs.send(Job::Notes(p))),
            (None, Some(afar)) if a.notify => drop(self.jobs.send(Job::NotesAfar(afar.clone()))),
            _ => {}
        }
    }

    /// Something notable happened to PTY child `id`.
    pub fn pty_news(&self, st: &mut State, id: u32) {
        let Some(t) = st.pty_reg.terminals.get_mut(&id) else { return };
        t.news += 1;
        if std::mem::take(&mut t.told_parent) && !pty_stuck(t) && t.is_live() {
            t.seen = t.news;
        }
        match (parent_key(t.parent.as_deref()), &t.parent) {
            (Some(p @ TargetKey::Agent(_)), _) if t.notify => drop(self.jobs.send(Job::Notes(p))),
            (None, Some(afar)) if t.notify => drop(self.jobs.send(Job::NotesAfar(afar.clone()))),
            _ => {}
        }
    }

    /// The caller looked at ACP child `id`; if it is the parent, that news is seen.
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
            self.clear_manual_status_caller_locked(&mut st, caller);
        }
    }

    /// The caller looked at PTY child `id`; if it is the parent, that news is seen.
    pub fn pty_saw(&self, caller: Option<&str>, id: u32) {
        let mut st = self.lock();
        if let Some(t) = st.pty_reg.terminals.get_mut(&id)
            && caller.is_some()
            && t.parent.as_deref() == caller
            && t.seen < t.news
        {
            t.seen = t.news;
            t.seen_ms = Some(now_ms());
            self.save_pty(&st);
            self.clear_manual_status_caller_locked(&mut st, caller);
        }
    }

    /// Clears a manual `herdr.status` override on ACP agent `id`, restoring automatic recognition.
    pub(crate) fn clear_manual_status_agent_locked(&self, st: &mut State, id: u32) -> bool {
        if !st.reg.agents.get(&id).is_some_and(|a| a.labels.contains_key("herdr.status")) {
            return false;
        }
        let was = st.reg.agents.get(&id).cloned();
        st.reg.agents.get_mut(&id).expect("checked").labels.remove("herdr.status");
        let labels = serde_json::to_value(&st.reg.agents[&id].labels).unwrap_or_default();
        self.save(st);
        self.emit_from(st, "labels", id, serde_json::json!({"labels": labels}), was.as_ref());
        true
    }

    /// Clears a manual `herdr.status` override on PTY terminal `id`, restoring automatic recognition.
    pub(crate) fn clear_manual_status_pty_locked(&self, st: &mut State, id: u32) -> bool {
        if !st.pty_reg.terminals.get(&id).is_some_and(|t| t.labels.contains_key("herdr.status")) {
            return false;
        }
        let was = st.pty_reg.terminals.get(&id).cloned();
        st.pty_reg.terminals.get_mut(&id).expect("checked").labels.remove("herdr.status");
        let labels = serde_json::to_value(&st.pty_reg.terminals[&id].labels).unwrap_or_default();
        self.save_pty(st);
        self.emit_pty_from(st, "labels", id, serde_json::json!({"labels": labels}), was.as_ref());
        true
    }

    /// Clears `herdr.status` on target `key`.
    pub(crate) fn clear_manual_status_key(&self, key: TargetKey) {
        let mut st = self.lock();
        match key {
            TargetKey::Agent(id) => {
                self.clear_manual_status_agent_locked(&mut st, id);
            }
            TargetKey::Terminal(id) => {
                self.clear_manual_status_pty_locked(&mut st, id);
            }
        }
    }

    fn local_target_key(&self, raw: Option<&str>) -> Option<TargetKey> {
        let s = raw.map(str::trim).filter(|s| !s.is_empty())?;
        if let Some((prefix, tail)) = s.split_once('/') {
            if prefix == self.info.name || prefix == self.info.id {
                return TargetKey::parse(tail);
            }
            return None;
        }
        TargetKey::parse(s)
    }

    /// Clears `herdr.status` on a local `caller`/`from` address while already holding `State`.
    pub(crate) fn clear_manual_status_caller_locked(&self, st: &mut State, caller: Option<&str>) {
        let Some(key) = self.local_target_key(caller) else { return };
        match key {
            TargetKey::Agent(id) => {
                self.clear_manual_status_agent_locked(st, id);
            }
            TargetKey::Terminal(id) => {
                self.clear_manual_status_pty_locked(st, id);
            }
        }
    }

    /// Clears `herdr.status` on a local `caller`/`from` address.
    pub(crate) fn clear_manual_status_caller(&self, caller: Option<&str>) {
        if let Some(key) = self.local_target_key(caller) {
            self.clear_manual_status_key(key);
        }
    }

    fn sender_label(&self, st: &State, from: Option<&str>, to: TargetKey) -> Option<(String, String)> {
        let from = from?;
        if let Some(key) = TargetKey::parse(from).filter(|k| *k != to) {
            return match key {
                TargetKey::Agent(f) => st.reg.agents.get(&f).filter(|s| s.reported).map(|s| (who(s), s.address.clone())),
                TargetKey::Terminal(f) => {
                    st.pty_reg.terminals.get(&f).filter(|s| s.reported).map(|s| (pty_who(s), s.address.clone()))
                }
            };
        }
        None
    }

    /// Text from `from` to ACP agent `to`, wrapped in `<h2>` tags attributing the sender.
    pub fn tagged(&self, from: Option<&str>, to: u32, text: &str, reach: bool) -> String {
        if !self.notes_on() {
            return text.to_string();
        }
        let st = self.lock();
        if !st.reg.agents.get(&to).is_some_and(|a| a.reported) {
            return text.to_string();
        }
        if let Some(afar) = from.filter(|f| f.rsplit_once('/').is_some_and(|(_, local)| TargetKey::parse(local).is_some())) {
            return format!("<h2 from=\"{afar}\">{text}</h2>");
        }
        match self.sender_label(&st, from, TargetKey::Agent(to)) {
            Some((label, addr)) if reach && TargetKey::parse(&addr).is_some_and(|k| matches!(k, TargetKey::Agent(_))) => {
                format!("<h2 from=\"{label}\" reach=\"h2 send {addr} when done or stuck\">{text}</h2>")
            }
            Some((label, _)) => format!("<h2 from=\"{label}\">{text}</h2>"),
            None => text.to_string(),
        }
    }

    /// Text from `from` to PTY terminal `to`, wrapped in `<h2>` tags if both reported as agents.
    pub fn pty_tagged(&self, from: Option<&str>, to: u32, text: &str, reach: bool) -> String {
        if !self.notes_on() {
            return text.to_string();
        }
        let st = self.lock();
        if !st.pty_reg.terminals.get(&to).is_some_and(|t| t.reported) {
            return text.to_string();
        }
        if let Some(afar) = from.filter(|f| f.rsplit_once('/').is_some_and(|(_, local)| TargetKey::parse(local).is_some())) {
            return format!("<h2 from=\"{afar}\">{text}</h2>");
        }
        match self.sender_label(&st, from, TargetKey::Terminal(to)) {
            Some((label, addr)) if reach => {
                format!("<h2 from=\"{label}\" reach=\"h2 send {addr} when done or stuck\">{text}</h2>")
            }
            Some((label, _)) => format!("<h2 from=\"{label}\">{text}</h2>"),
            None => text.to_string(),
        }
    }

    /// Children (ACP agents and PTY terminals) of `address` with unseen news.
    fn unseen(&self, st: &State, address: &str) -> (Vec<u32>, Vec<u32>) {
        let acp_kids = st
            .reg
            .agents
            .values()
            .filter(|a| a.parent.as_deref() == Some(address) && a.notify)
            .filter(|a| a.news > a.seen.max(a.noted) && (stuck(a) || !a.is_live()))
            .filter(|a| !st.waiting.contains_key(&(address.to_string(), TargetKey::Agent(a.id()))))
            .map(Agent::id)
            .collect();
        let pty_kids = st
            .pty_reg
            .terminals
            .values()
            .filter(|t| t.parent.as_deref() == Some(address) && t.notify)
            .filter(|t| t.news > t.seen.max(t.noted) && (pty_stuck(t) || !t.is_live()))
            .filter(|t| !st.waiting.contains_key(&(address.to_string(), TargetKey::Terminal(t.id()))))
            .map(Terminal::id)
            .collect();
        (acp_kids, pty_kids)
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
                    let (acp_kids, pty_kids) = self.unseen(&st, &parent);
                    if acp_kids.is_empty() && pty_kids.is_empty() {
                        continue;
                    }
                    let mut notes: Vec<String> = acp_kids
                        .iter()
                        .map(|id| self.note_for(&st, *id, &format!("{called}/{}", crate::agents::address(*id))))
                        .collect();
                    notes.extend(
                        pty_kids
                            .iter()
                            .map(|id| self.pty_note_for(&st, *id, &format!("{called}/{}", crate::terminals::address(*id)))),
                    );
                    for id in &acp_kids {
                        let a = st.reg.agents.get_mut(id).expect("listed");
                        a.noted = a.noted.max(a.news);
                    }
                    for id in &pty_kids {
                        let t = st.pty_reg.terminals.get_mut(id).expect("listed");
                        t.noted = t.noted.max(t.news);
                    }
                    if !acp_kids.is_empty() {
                        self.save(&st);
                    }
                    if !pty_kids.is_empty() {
                        self.save_pty(&st);
                    }
                    crate::remote::tell_peer(&self, &mut st, &parent, &notes);
                }
                Job::Orphaned(parent) => {
                    let address = parent.address();
                    let (acp_kids, pty_kids): (Vec<u32>, Vec<u32>) = {
                        let mut st = self.lock();
                        let acp_kids: Vec<u32> = st
                            .reg
                            .agents
                            .values()
                            .filter(|a| {
                                a.is_live() && a.die_with_parent && a.parent.as_deref() == Some(address.as_str())
                            })
                            .map(Agent::id)
                            .collect();
                        let pty_kids: Vec<u32> = st
                            .pty_reg
                            .terminals
                            .values()
                            .filter(|t| {
                                t.is_live() && t.die_with_parent && t.parent.as_deref() == Some(address.as_str())
                            })
                            .map(Terminal::id)
                            .collect();
                        st.killing.extend(&acp_kids);
                        st.pty_killing.extend(&pty_kids);
                        (acp_kids, pty_kids)
                    };
                    let core = self.clone();
                    tokio::spawn(async move {
                        core.kill(&acp_kids).await;
                        core.pty_kill(&pty_kids).await;
                    });
                }
                Job::PtyCheckpoint { id, entered_working, turn_ended, turn } => {
                    let core = self.clone();
                    tokio::spawn(async move {
                        crate::pty_adapters::process_pty_checkpoint_job(
                            &core,
                            id,
                            entered_working,
                            turn_ended,
                            turn,
                        )
                        .await;
                    });
                }
            }
        }
    }

    fn notes_on(&self) -> bool {
        crate::config::load(&self.paths.config).map(|c| c.notes.unwrap_or(true)).unwrap_or(true)
    }

    async fn deliver_notes(self: Arc<Self>, parent: TargetKey) {
        match parent {
            TargetKey::Agent(id) => self.deliver_acp_notes(id).await,
            TargetKey::Terminal(id) => {
                // Terminal parents must never receive automatic PTY stdin injections;
                // they receive messages/updates strictly via `h2 wait` / `h2 read`.
                let mut st = self.lock();
                st.afar_notes.remove(&TargetKey::Terminal(id));
                st.note_tasks.remove(&TargetKey::Terminal(id));
            }
        }
    }

    async fn deliver_acp_notes(self: Arc<Self>, parent: u32) {
        let enabled = self.notes_on();
        let key = TargetKey::Agent(parent);
        loop {
            let ready = |st: &State| st.reg.agents.get(&parent).is_none_or(|a| !a.is_live() || can_take_note(a));
            while !self.wait_for(Duration::from_secs(60), ready).await {}
            let alive = self.lock().reg.agents.get(&parent).is_some_and(|a| a.is_live());
            if !alive || !enabled {
                break;
            }
            if !self.queue_note_batch(parent) {
                let mut st = self.lock();
                let (acp_kids, pty_kids) = self.unseen(&st, &crate::agents::address(parent));
                if acp_kids.is_empty() && pty_kids.is_empty() && !st.afar_notes.contains_key(&key) {
                    st.note_tasks.remove(&key);
                    return;
                }
            }
        }
        self.lock().note_tasks.remove(&key);
    }

    fn queue_note_batch(&self, parent: u32) -> bool {
        let mut st = self.lock();
        let Some(p) = st.reg.agents.get(&parent) else {
            return false;
        };
        if !can_take_note(p) || !st.sessions.contains_key(&parent) {
            return false;
        }
        let key = TargetKey::Agent(parent);
        let addr = crate::agents::address(parent);
        let afar = st.afar_notes.remove(&key).unwrap_or_default();
        let (acp_kids, pty_kids) = self.unseen(&st, &addr);
        if acp_kids.is_empty() && pty_kids.is_empty() && afar.is_empty() {
            return false;
        }
        let mut text: Vec<String> =
            acp_kids.iter().map(|id| self.note_for(&st, *id, &st.reg.agents[id].address)).collect();
        text.extend(
            pty_kids.iter().map(|id| self.pty_note_for(&st, *id, &st.pty_reg.terminals[id].address)),
        );
        text.extend(afar);
        let told_acp: Vec<(u32, u64)> = acp_kids.iter().map(|id| (*id, st.reg.agents[id].news)).collect();
        let told_pty: Vec<(u32, u64)> = pty_kids.iter().map(|id| (*id, st.pty_reg.terminals[id].news)).collect();
        for (id, news) in &told_acp {
            if let Some(a) = st.reg.agents.get_mut(id) {
                a.noted = a.noted.max(*news);
            }
        }
        for (id, news) in &told_pty {
            if let Some(t) = st.pty_reg.terminals.get_mut(id) {
                t.noted = t.noted.max(*news);
            }
        }
        st.inboxes.entry(parent).or_default().push_back(QueuedPrompt::new(text.join("\n"), None));
        self.sync_inbox(&mut st, parent, true);
        if let Some(session) = st.sessions.get(&parent) {
            let _ = session.cmd_tx.send(SessionCommand::WakeInbox);
        }
        self.clear_manual_status_agent_locked(&mut st, parent);
        self.save(&st);
        if !told_pty.is_empty() {
            self.save_pty(&st);
        }
        true
    }

    #[allow(dead_code)]
    async fn deliver_pty_notes(self: Arc<Self>, parent: u32) {
        let enabled = self.notes_on();
        let key = TargetKey::Terminal(parent);
        loop {
            let ready = |st: &State| st.pty_reg.terminals.get(&parent).is_none_or(|t| !t.is_live() || pty_can_take_note(t));
            while !self.wait_for(Duration::from_secs(60), ready).await {}
            let alive = self.lock().pty_reg.terminals.get(&parent).is_some_and(|t| t.is_live());
            if !alive || !enabled {
                break;
            }
            match self.type_pty_note(parent).await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(_) => continue,
            }
            let mut st = self.lock();
            let (acp_kids, pty_kids) = self.unseen(&st, &crate::terminals::address(parent));
            if acp_kids.is_empty() && pty_kids.is_empty() && !st.afar_notes.contains_key(&key) {
                st.note_tasks.remove(&key);
                return;
            }
        }
        self.lock().note_tasks.remove(&key);
    }

    #[allow(dead_code)]
    async fn type_pty_note(&self, parent: u32) -> Result<bool, RpcError> {
        let quiet = crate::config::delivery(&self.paths.config).notes_quiet.unwrap_or_else(crate::pty_input::quiet);
        let key = TargetKey::Terminal(parent);
        let afar = self.lock().afar_notes.remove(&key).unwrap_or_default();
        let typed = self
            .type_in(parent, quiet, true, |st, t| {
                if !pty_can_take_note(t) {
                    return Err(RpcError::new("busy", "the parent got busy", ""));
                }
                let addr = crate::terminals::address(parent);
                let (acp_kids, pty_kids) = self.unseen(st, &addr);
                if acp_kids.is_empty() && pty_kids.is_empty() && afar.is_empty() {
                    return Ok(((Vec::new(), Vec::new()), Vec::new()));
                }
                let mut text: Vec<String> =
                    acp_kids.iter().map(|id| self.note_for(st, *id, &st.reg.agents[id].address)).collect();
                text.extend(
                    pty_kids.iter().map(|id| self.pty_note_for(st, *id, &st.pty_reg.terminals[id].address)),
                );
                text.extend(afar.iter().cloned());
                let bracketed = st.screens.get(&parent).is_some_and(|s| s.dec_mode(2004));
                let told_acp: Vec<(u32, u64)> = acp_kids.iter().map(|id| (*id, st.reg.agents[id].news)).collect();
                let told_pty: Vec<(u32, u64)> = pty_kids.iter().map(|id| (*id, st.pty_reg.terminals[id].news)).collect();
                let steps = vec![
                    Step::Write(paste(bracketed, &text.join("\n"))),
                    Step::Pause(crate::pty_input::PASTE_GAP),
                    Step::Write(b"\r".to_vec()),
                ];
                Ok(((told_acp, told_pty), steps))
            })
            .await;
        let (told_acp, told_pty) = match typed {
            Ok(told) => told,
            Err(e) => {
                if !afar.is_empty() {
                    self.lock().afar_notes.entry(key).or_default().splice(0..0, afar);
                }
                return Err(e);
            }
        };
        let had_afar = !afar.is_empty();
        let mut st = self.lock();
        for (id, news) in &told_acp {
            if let Some(a) = st.reg.agents.get_mut(id) {
                a.noted = a.noted.max(*news);
            }
        }
        for (id, news) in &told_pty {
            if let Some(t) = st.pty_reg.terminals.get_mut(id) {
                t.noted = t.noted.max(*news);
            }
        }
        if !told_acp.is_empty() {
            self.save(&st);
        }
        if !told_pty.is_empty() {
            self.save_pty(&st);
        }
        Ok(!told_acp.is_empty() || !told_pty.is_empty() || had_afar)
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
