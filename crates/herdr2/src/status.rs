//! Agent execution status reports derived from ACP turns and permission prompts.
//! The turn goes up each time the root record leaves `working`, for any reason.

use crate::agents::Agent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const MAX_RECORDS: usize = 256;

/// Structured status update analogous to legacy status reports.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusReport {
    pub state: &'static str,
    pub kind: Option<&'static str>,
    pub progress: Option<u8>,
    pub id: String,
    pub app: String,
    pub title: String,
    pub message: String,
}

impl StatusReport {
    pub fn root(state: &'static str) -> Self {
        Self {
            state,
            kind: None,
            progress: None,
            id: String::new(),
            app: String::new(),
            title: String::new(),
            message: String::new(),
        }
    }
}

/// One status record. Words describe ACP execution state cleanly without translation.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct Status {
    /// idle, working, done, blocked or error.
    pub state: String,
    /// permission, question or auth (blocked only, when relevant).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// 0..100 (working or blocked, when reported).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub app: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub msg: String,
}

impl Status {
    fn from(r: &StatusReport) -> Status {
        Status {
            state: r.state.into(),
            kind: r.kind.map(Into::into),
            progress: r.progress,
            app: r.app.clone(),
            title: r.title.clone(),
            msg: r.message.clone(),
        }
    }
}

/// What a report changed, for events and waiters.
#[derive(Default, Debug, PartialEq)]
pub struct Change {
    pub status: bool,
    /// A fresh root report, not a child record or an unchanged recovery replay.
    pub root_report: bool,
    pub turn_ended: bool,
    /// It just got stuck: blocked on the user or errored (wasn't that before).
    pub stuck: bool,
}

fn working(a: &Agent) -> bool {
    a.status.as_ref().is_some_and(|s| s.state == "working")
}

fn end_turn(a: &mut Agent, was_working: bool) -> bool {
    let ended = was_working && !working(a);
    if ended {
        a.turn += 1;
    }
    ended
}

fn stuck(a: &Agent) -> Option<&str> {
    a.status.as_ref().map(|s| s.state.as_str()).filter(|w| *w == "blocked" || *w == "error")
}

fn word(a: &Agent) -> Option<String> {
    a.status.as_ref().map(|s| s.state.clone())
}

fn stamp(a: &mut Agent, was: Option<String>) {
    if word(a) != was {
        a.status_ms = Some(crate::runtime::now_ms());
    }
}

pub fn apply(a: &mut Agent, r: &StatusReport) -> Change {
    let (was_working, was_word) = (working(a), word(a));
    let was = stuck(a).map(String::from);
    a.reported = true;
    if r.state == "clear" {
        if r.id.is_empty() {
            a.status = None;
            a.records.clear();
        } else {
            let child = format!("{}/", r.id);
            a.records.retain(|id, _| *id != r.id && !id.starts_with(&child));
        }
    } else if r.id.is_empty() {
        a.status = Some(Status::from(r));
    } else {
        // Over the limit, the record updated longest ago goes (approximated by first id).
        if !a.records.contains_key(&r.id) && a.records.len() >= MAX_RECORDS - 1 {
            let oldest = a.records.keys().next().cloned().expect("non-empty");
            a.records.remove(&oldest);
        }
        a.records.insert(r.id.clone(), Status::from(r));
    }
    stamp(a, was_word);
    Change {
        status: true,
        root_report: r.id.is_empty(),
        turn_ended: end_turn(a, was_working),
        stuck: stuck(a).is_some() && stuck(a) != was.as_deref(),
    }
}

/// The agent exited: its status facts reset.
pub fn reset(a: &mut Agent) -> Change {
    let (was_working, was_word) = (working(a), word(a));
    let had = a.status.is_some() || !a.records.is_empty();
    a.status = None;
    a.records.clear();
    stamp(a, was_word);
    Change { status: had, turn_ended: end_turn(a, was_working), ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{NewAgent, Registry};

    fn agent() -> Agent {
        let mut r = Registry::default();
        let new = NewAgent { argv: vec!["x".into()], cwd: "/".into(), ..Default::default() };
        let id = r.create(new, 0).unwrap();
        r.agents.remove(&id).unwrap()
    }

    fn report(state: &'static str, id: &str) -> StatusReport {
        StatusReport { id: id.into(), ..StatusReport::root(state) }
    }

    #[test]
    fn the_turn_counts_every_way_of_leaving_working() {
        let mut a = agent();
        assert!(!apply(&mut a, &report("idle", "")).turn_ended);
        apply(&mut a, &report("working", ""));
        assert!(apply(&mut a, &report("done", "")).turn_ended); // finished
        apply(&mut a, &report("working", ""));
        assert!(apply(&mut a, &report("idle", "")).turn_ended); // interrupted
        apply(&mut a, &report("working", ""));
        assert!(apply(&mut a, &report("blocked", "")).turn_ended); // asks permission
        apply(&mut a, &report("working", ""));
        assert!(apply(&mut a, &report("clear", "")).turn_ended); // cleared
        apply(&mut a, &report("working", ""));
        assert!(!apply(&mut a, &report("working", "sub")).turn_ended, "child records don't count");
        assert!(reset(&mut a).turn_ended); // exited
        assert_eq!(a.turn, 5);
        assert!(a.status.is_none() && a.reported);
    }

    #[test]
    fn the_status_time_moves_only_when_the_word_changes() {
        let mut a = agent();
        assert_eq!(a.status_ms, None, "never said anything");
        apply(&mut a, &report("working", ""));
        let since = a.status_ms.expect("its first word");
        a.status_ms = Some(1);
        apply(&mut a, &report("working", ""));
        apply(&mut a, &report("done", "sub"));
        assert_eq!(a.status_ms, Some(1), "the same word again, or a child record");
        apply(&mut a, &report("done", ""));
        assert!(a.status_ms >= Some(since));
        a.status_ms = Some(1);
        reset(&mut a);
        assert!(a.status_ms > Some(1), "exiting clears the word");
    }

    #[test]
    fn getting_stuck_is_news_even_outside_a_turn() {
        let mut a = agent();
        apply(&mut a, &report("idle", ""));
        assert!(apply(&mut a, &report("error", "")).stuck, "e.g. it failed while starting");
        assert!(!apply(&mut a, &report("error", "")).stuck, "once");
        assert!(apply(&mut a, &report("blocked", "")).stuck);
        assert!(!apply(&mut a, &report("idle", "")).stuck);
    }

    #[test]
    fn clear_removes_a_record_and_its_children() {
        let mut a = agent();
        for id in ["build", "build/test", "buildx", "deploy"] {
            apply(&mut a, &report("working", id));
        }
        apply(&mut a, &report("clear", "build"));
        assert_eq!(a.records.keys().collect::<Vec<_>>(), ["buildx", "deploy"]);
    }
}
