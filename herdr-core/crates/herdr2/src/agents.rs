//! Agent records: ids, names, labels, ACP configuration, lifecycle facts and retention.
//! Pure state, no IO; the core persists `Registry` as JSON.

use crate::permissions::{PermissionMode, PublicPermission};
use crate::rpc::RpcError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const RETAIN_MS: u64 = 24 * 60 * 60 * 1000;
pub const MAX_FINISHED: usize = 500;
pub const KEEP_RETRY_MS: u64 = 1500;

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct QueuedInput {
    /// Monotonic queue item id used for race-safe edit and removal.
    pub id: u64,
    /// Raw prompt text queued for delivery (may carry an `<h2 ...>` envelope when delegated).
    pub text: String,
    /// Optional sender address (`a3` / `t1`) when sent by another agent or terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Epoch milliseconds when this item entered the inbox queue.
    pub queued_ms: u64,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct Agent {
    /// `a7`: this agent forever on this core.
    pub address: String,
    pub name: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub argv: Vec<String>,
    pub cwd: String,
    /// Configured agent preset name (e.g. `claude`, `codex`), if launched via preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Agent identity reported by ACP `initialize` (`agentInfo.name version`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_info: Option<String>,
    /// ACP session id returned by `session/new`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_session_id: Option<String>,
    /// Whether the adapter supports `session/load` on `--keep` recovery.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub can_load_session: bool,
    /// Optional system prompt passed via `_meta.systemPrompt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Requested model override (`session/set_config_option`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Requested mode override (`session/set_mode`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Live session configuration options (model, reasoning effort, fast mode, session mode) advertised by the ACP agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_options: Vec<crate::acp::proto::AcpConfigOption>,
    /// User-applied session configuration overrides preserved across `--keep` reloads.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config_overrides: BTreeMap<String, serde_json::Value>,
    /// Latest context window token usage reported by ACP `usage_update`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<crate::acp::proto::AcpContextUsage>,
    /// Whether built-in `herdr2` MCP server is injected into ACP `mcpServers`.
    #[serde(default = "default_true")]
    pub inject_mcp: bool,
    /// Permission policy for `session/request_permission`.
    #[serde(default)]
    pub permission_mode: PermissionMode,
    /// Current pending approval request when `status.state == "blocked"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_permission: Option<PublicPermission>,
    /// Number of user messages queued behind the active ACP turn.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub inbox_len: usize,
    /// Prompts queued behind the active ACP turn, in FIFO execution order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inbox: Vec<QueuedInput>,
    /// Restart a crashed or stopped kept agent cleanly (`session/load` when supported). Exit with a code and kill remain final.
    #[serde(default)]
    pub keep: bool,
    /// Only caller overrides, never the inherited core environment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub restart_env: BTreeMap<String, String>,
    #[serde(default)]
    pub restart_after_ms: u64,
    #[serde(default)]
    pub restart_delay_ms: u64,
    #[serde(default)]
    pub program_started_ms: Option<u64>,
    /// "live" or "finished".
    pub state: String,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// Why its process ended or is pending: exited, killed, server_stop,
    /// host_shutdown, signaled, crashed, stopped_by_restart.
    pub reason: Option<String>,
    pub created_ms: u64,
    pub finished_ms: Option<u64>,
    /// Agent execution state: idle, working, done, blocked, or error.
    #[serde(default)]
    pub status: Option<crate::status::Status>,
    /// When that state last changed, ms since the epoch.
    #[serde(default)]
    pub status_ms: Option<u64>,
    /// Epoch ms when the current active working run began (adjusted for prior paused blocked segments in the same run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_since: Option<u64>,
    /// Active execution duration (ms) of the current or most recently completed run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_ms: Option<u64>,
    /// Cumulative active execution duration (ms) across all runs of this agent.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub total_work_ms: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub records: BTreeMap<String, crate::status::Status>,
    /// Goes up each time the status leaves `working`.
    #[serde(default)]
    pub turn: u64,
    /// A sent launch task still awaiting its first completed turn (private).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_task_turn: Option<u64>,
    #[serde(default)]
    pub reported: bool,
    #[serde(default)]
    pub title: String,
    /// The agent that started it (`a3`), set once at start.
    #[serde(default)]
    pub parent: Option<String>,
    /// Its parent gets notes about it (see `--no-notify`).
    #[serde(default)]
    pub notify: bool,
    #[serde(default)]
    pub die_with_parent: bool,
    /// Removed right after it exits (once a waiting parent has the exit code).
    #[serde(default)]
    pub rm: bool,
    /// Notable events so far: turns that ended, blocks, the exit.
    #[serde(default)]
    pub news: u64,
    /// How many of those its parent has seen (read, get, wait, send, approve, deny, kill).
    #[serde(default)]
    pub seen: u64,
    /// How many of those a note already told its parent about.
    #[serde(default)]
    pub noted: u64,
    /// It messaged its parent during this turn: the turn's end tells nothing new.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub told_parent: bool,
    /// Crash-only cleanup token: used if the process escapes normal termination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default)]
    pub seen_ms: Option<u64>,
    /// The template that started it.
    #[serde(default)]
    pub template: Option<String>,
    /// A template first message that was not sent; retained for later inspection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_message: Option<crate::templates::FirstMessage>,
    /// Machines (core ids) it started work on: they may call back to it.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub afar: std::collections::BTreeSet<String>,
    /// Context handoff / fork / side-chat parent lineage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<crate::handoff::Lineage>,
}

impl Agent {
    pub fn id(&self) -> u32 {
        parse_id(&self.address).expect("addresses are a<number>")
    }
    pub fn is_live(&self) -> bool {
        self.state == "live"
    }

    pub(crate) fn program_lost(&self) -> bool {
        self.is_live() && self.pid.is_none() && self.reason.as_deref().is_some_and(|r| r != "spawn_pending")
    }

    pub fn check_program(&self) -> Result<(), RpcError> {
        if self.is_live() && self.pid.is_none() && self.reason.is_some() {
            return Err(RpcError::new(
                "unavailable",
                format!("{} is lost, waiting for its restarted session", self.address),
                "retry after the restarted event",
            ));
        }
        Ok(())
    }
}

/// After `a` and a first digit 1-9, ids count in these 30: no vowels (no
/// words), no `l` (reads as 1).
const SYMBOLS: &[u8; 30] = b"0123456789bcdfghjkmnpqrstvwxyz";

/// Counter `n` (from 1) as written: a1..a9, a10..a9z, a100..a9zz, ...
pub fn address(n: u32) -> String {
    let (mut rest, mut size, mut len) = (u64::from(n) - 1, 9u64, 0);
    while rest >= size {
        rest -= size;
        size *= 30;
        len += 1;
    }
    let mut tail = vec![0u8; len];
    for b in tail.iter_mut().rev() {
        *b = SYMBOLS[(rest % 30) as usize];
        rest /= 30;
    }
    format!("a{}{}", rest + 1, String::from_utf8(tail).expect("ascii"))
}

/// Shaped like an id, whether or not the number fits: names may not be.
pub fn id_shaped(local: &str) -> bool {
    let Some(rest) = local.strip_prefix('a') else { return false };
    rest.starts_with(|c: char| matches!(c, '1'..='9')) && rest.bytes().all(|b| SYMBOLS.contains(&b))
}

pub fn parse_id(local: &str) -> Option<u32> {
    if !id_shaped(local) {
        return None;
    }
    let mut symbols = local[1..].bytes().map(|b| SYMBOLS.iter().position(|&s| s == b).unwrap() as u64);
    let mut rest = symbols.next()? - 1;
    let (mut before, mut size) = (0u64, 9u64);
    for v in symbols {
        rest = rest.checked_mul(30)?.checked_add(v)?;
        before = before.checked_add(size)?;
        size = size.checked_mul(30)?;
    }
    u32::try_from(before.checked_add(rest)? + 1).ok()
}

#[derive(Serialize, Deserialize, Default)]
pub struct Registry {
    pub next_id: u32,
    pub agents: BTreeMap<u32, Agent>,
}

pub struct NewAgent {
    pub template: Option<crate::templates::Template>,
    pub parent: Option<String>,
    pub notify: bool,
    pub die_with_parent: bool,
    pub rm: bool,
    pub keep: bool,
    pub name: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub argv: Vec<String>,
    pub cwd: String,
    pub agent: Option<String>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub inject_mcp: bool,
    pub permission_mode: PermissionMode,
    pub lineage: Option<crate::handoff::Lineage>,
}

impl Default for NewAgent {
    fn default() -> Self {
        Self {
            template: None,
            parent: None,
            notify: false,
            die_with_parent: false,
            rm: false,
            keep: false,
            name: None,
            labels: BTreeMap::new(),
            argv: Vec::new(),
            cwd: String::new(),
            agent: None,
            system_prompt: None,
            model: None,
            mode: None,
            inject_mcp: true,
            permission_mode: PermissionMode::default(),
            lineage: None,
        }
    }
}

impl Registry {
    /// Validates and records a new live agent.
    pub fn create(&mut self, new: NewAgent, now_ms: u64) -> Result<u32, RpcError> {
        if let Some(name) = &new.name {
            crate::addr::check_name(name)?;
            if self.live_named(name).is_some() {
                return Err(RpcError::new(
                    "name_taken",
                    format!("a live agent is already named {name:?}"),
                    "pick another name, or kill the one holding it",
                ));
            }
        }
        for key in new.labels.keys() {
            check_label_key(key)?;
        }
        if new.argv.is_empty() {
            return Err(RpcError::invalid_params("nothing to run"));
        }
        let id = self.next_id.max(1);
        self.next_id = id.checked_add(1).ok_or_else(|| {
            RpcError::new("unavailable", "this core has used every agent id", "wipe its state to start over")
        })?;
        self.agents.insert(
            id,
            Agent {
                address: address(id),
                name: new.name,
                labels: new.labels.into_iter().filter(|(_, v)| !v.is_empty()).collect(),
                argv: new.argv,
                cwd: new.cwd,
                agent: new.agent,
                agent_info: None,
                acp_session_id: None,
                can_load_session: false,
                system_prompt: new.system_prompt,
                model: new.model,
                mode: new.mode,
                config_options: Vec::new(),
                config_overrides: BTreeMap::new(),
                context_usage: None,
                inject_mcp: new.inject_mcp,
                permission_mode: new.permission_mode,
                pending_permission: None,
                inbox_len: 0,
                inbox: Vec::new(),
                keep: new.keep,
                restart_env: BTreeMap::new(),
                restart_after_ms: 0,
                restart_delay_ms: 0,
                program_started_ms: None,
                state: "live".into(),
                pid: None,
                exit_code: None,
                signal: None,
                reason: None,
                created_ms: now_ms,
                finished_ms: None,
                status: None,
                status_ms: None,
                working_since: None,
                work_ms: None,
                total_work_ms: 0,
                records: BTreeMap::new(),
                turn: 0,
                first_task_turn: None,
                reported: false,
                title: String::new(),
                parent: new.parent,
                notify: new.notify,
                die_with_parent: new.die_with_parent,
                rm: new.rm,
                news: 0,
                seen: 0,
                noted: 0,
                told_parent: false,
                scope: None,
                seen_ms: None,
                template: new.template.map(|t| t.name),
                first_message: None,
                afar: Default::default(),
                lineage: new.lineage,
            },
        );
        Ok(id)
    }

    fn live_named(&self, name: &str) -> Option<&Agent> {
        self.agents.values().find(|t| t.is_live() && t.name.as_deref() == Some(name))
    }

    /// An id means that agent forever; a name means whoever holds it now,
    /// else the most recent finished agent that had it.
    pub fn resolve(&self, local: &str) -> Result<u32, RpcError> {
        let found = match parse_id(local) {
            Some(id) => self.agents.get(&id),
            None => self
                .live_named(local)
                .or_else(|| self.agents.values().rev().find(|t| t.name.as_deref() == Some(local))),
        };
        found
            .map(Agent::id)
            .ok_or_else(|| RpcError::new("not_found", format!("no agent {local:?}"), "see `h2 ls --all`"))
    }

    pub fn finish(&mut self, id: u32, code: Option<i32>, signal: Option<i32>, reason: &str, now_ms: u64) {
        if let Some(a) = self.agents.get_mut(&id).filter(|t| t.is_live()) {
            let pending = a.keep
                && matches!(reason, "crashed" | "stopped_by_restart" | "server_stop" | "host_shutdown" | "signaled");
            a.state = if pending { "live" } else { "finished" }.into();
            if pending {
                a.schedule_restart(now_ms);
                a.pid = None;
            }
            a.pending_permission = None;
            a.inbox_len = 0;
            a.inbox.clear();
            a.exit_code = code;
            a.signal = signal;
            a.reason = Some(reason.into());
            a.finished_ms = (!pending).then_some(now_ms);
        }
    }

    /// Applies `key=value` changes; an empty value removes the key.
    pub fn label(&mut self, id: u32, changes: &BTreeMap<String, String>) -> Result<(), RpcError> {
        for key in changes.keys() {
            check_label_key(key)?;
        }
        let a = self.agents.get_mut(&id).expect("resolved id");
        for (k, v) in changes {
            if v.is_empty() {
                a.labels.remove(k);
            } else {
                a.labels.insert(k.clone(), v.clone());
            }
        }
        Ok(())
    }

    /// Finished agents past retention, for the caller to remove.
    /// Kept a day after its live parent has seen the result (no live parent:
    /// a day after it finished); kept while a live parent hasn't seen it.
    /// At most MAX_FINISHED, oldest first, whatever the rest says.
    pub fn expired(&self, now_ms: u64) -> Vec<u32> {
        let parent_live = |a: &Agent| {
            let id = a.parent.as_deref().and_then(parse_id);
            id.and_then(|id| self.agents.get(&id)).is_some_and(Agent::is_live)
        };
        let mut finished: Vec<(u64, u32, Option<u64>)> = self
            .agents
            .values()
            .filter_map(|t| {
                let at = t.finished_ms?;
                let clock = match parent_live(t) {
                    true if t.seen < t.news => None,
                    true => Some(t.seen_ms.unwrap_or(at).max(at)),
                    false => Some(at),
                };
                Some((at, t.id(), clock))
            })
            .collect();
        finished.sort();
        let over = finished.len().saturating_sub(MAX_FINISHED);
        finished
            .iter()
            .enumerate()
            .filter(|(i, (_, _, clock))| *i < over || clock.is_some_and(|c| now_ms.saturating_sub(c) > retain_ms()))
            .map(|(_, (_, id, _))| *id)
            .collect()
    }
}

/// The core's own bookkeeping in the record: saved with it, never shown.
const PRIVATE: &[&str] = &[
    "acp_session_id",
    "can_load_session",
    "config_overrides",
    "system_prompt",
    "first_task_turn",
    "news",
    "seen",
    "noted",
    "seen_ms",
    "told_parent",
    "scope",
    "afar",
    "restart_env",
    "restart_after_ms",
    "restart_delay_ms",
    "program_started_ms",
];

/// An agent as clients see it (API results, events): its facts, without
/// the core's bookkeeping, which would only make it harder to keep stable.
#[derive(Clone)]
pub struct Public(pub Agent);

impl Serialize for Public {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut value = serde_json::to_value(&self.0).map_err(serde::ser::Error::custom)?;
        if let Some(fields) = value.as_object_mut() {
            PRIVATE.iter().for_each(|k| drop(fields.remove(*k)));
        }
        value.serialize(s)
    }
}

impl JsonSchema for Public {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Agent".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = Agent::json_schema(generator);
        if let Some(props) = schema.get_mut("properties").and_then(|p| p.as_object_mut()) {
            PRIVATE.iter().for_each(|k| drop(props.remove(*k)));
        }
        if let Some(required) = schema.get_mut("required").and_then(|r| r.as_array_mut()) {
            required.retain(|k| !k.as_str().is_some_and(|k| PRIVATE.contains(&k)));
        }
        schema
    }
}

/// `RETAIN_MS`, or `H2_RETAIN_MS` (tests can't wait a day).
fn retain_ms() -> u64 {
    std::env::var("H2_RETAIN_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(RETAIN_MS)
}

fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

fn check_label_key(key: &str) -> Result<(), RpcError> {
    if key.is_empty() || key.contains('=') {
        return Err(RpcError::invalid_params(format!("bad label key {key:?}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(name: Option<&str>) -> NewAgent {
        NewAgent { name: name.map(Into::into), argv: vec!["true".into()], cwd: "/".into(), ..Default::default() }
    }

    #[test]
    fn keep_preserves_losses_without_retention_or_rm_but_exit_and_kill_are_final() {
        for reason in
            ["crashed", "stopped_by_restart", "host_shutdown", "server_stop", "signaled", "exited", "killed"]
        {
            for keep in [false, true] {
                let mut r = Registry::default();
                let id = r.create(NewAgent { keep, rm: true, ..new(None) }, 0).unwrap();
                r.finish(id, Some(3), None, reason, 5);
                let pending = keep && !matches!(reason, "exited" | "killed");
                assert_eq!(r.agents[&id].is_live(), pending, "{reason}, keep={keep}");
                assert_eq!(r.expired(RETAIN_MS * 5).is_empty(), pending);
                assert_eq!(r.agents[&id].finished_ms.is_none(), pending);
            }
        }
    }

    #[test]
    fn a_child_is_kept_until_its_live_parent_has_seen_it() {
        let mut r = Registry::default();
        let parent = r.create(new(None), 0).unwrap();
        let child = r.create(NewAgent { parent: Some(address(parent)), ..new(None) }, 0).unwrap();
        r.finish(child, Some(0), None, "exited", 0);
        r.agents.get_mut(&child).unwrap().news = 2;
        assert!(r.expired(RETAIN_MS * 5).is_empty(), "unseen by a live parent");
        let c = r.agents.get_mut(&child).unwrap();
        c.seen = 2;
        c.seen_ms = Some(RETAIN_MS * 5);
        assert!(r.expired(RETAIN_MS * 5 + 10).is_empty(), "a day after it was seen");
        assert_eq!(r.expired(RETAIN_MS * 6 + 10), vec![child]);
        // Without a live parent, the day runs from the finish.
        r.agents.get_mut(&child).unwrap().seen = 0;
        r.finish(parent, Some(0), None, "exited", 0);
        assert!(r.expired(RETAIN_MS + 10).contains(&child));
    }

    #[test]
    fn ids_are_never_reused_and_names_follow_their_live_holder() {
        let mut r = Registry::default();
        let a = r.create(new(Some("w")), 0).unwrap();
        assert_eq!(r.create(new(Some("w")), 0).unwrap_err().kind, "name_taken");
        r.finish(a, Some(0), None, "exited", 1);
        assert_eq!(r.resolve("w").unwrap(), a);
        let b = r.create(new(Some("w")), 2).unwrap();
        assert_eq!(r.resolve("w").unwrap(), b);
        r.agents.remove(&b);
        assert_eq!(r.create(new(None), 3).unwrap(), b + 1);
        assert_eq!(r.resolve("a99").unwrap_err().kind, "not_found");
    }

    #[test]
    fn ids_count_with_a_digit_then_digits_and_consonants() {
        for (n, a) in
            [(1, "a1"), (9, "a9"), (10, "a10"), (19, "a19"), (20, "a1b"), (39, "a1z"), (40, "a20"), (279, "a9z")]
        {
            assert_eq!(address(n), a);
        }
        for (n, a) in [(280, "a100"), (8379, "a9zz"), (8380, "a1000"), (148377, "a65jk")] {
            assert_eq!(address(n), a);
            assert_eq!(parse_id(a), Some(n), "{a}");
        }
        assert_eq!(parse_id(&address(u32::MAX)), Some(u32::MAX));
        for not_id in ["a", "a0", "a01", "ab", "a1l", "a1a", "A1", "a1B", "x1", "amp", "a3po"] {
            assert_eq!(parse_id(not_id), None, "{not_id}");
        }
        assert!(id_shaped("a9zzzzzzzzzz") && parse_id("a9zzzzzzzzzz").is_none(), "too big is still id-shaped");
        let mut r = Registry { next_id: u32::MAX - 1, ..Default::default() };
        r.create(new(None), 0).unwrap();
        assert_eq!(r.create(new(None), 0).unwrap_err().kind, "unavailable");
    }

    #[test]
    fn retention_keeps_a_day_and_at_most_500_finished() {
        let mut r = Registry::default();
        for i in 0..502u64 {
            let id = r.create(new(None), i).unwrap();
            r.finish(id, Some(0), None, "exited", i);
        }
        let live = r.create(new(None), 0).unwrap();
        assert_eq!(r.expired(1000), vec![1, 2]);
        // Over the cap, plus finished at 0..=99 now over a day old.
        let old = r.expired(RETAIN_MS + 100);
        assert_eq!(old.len(), 100);
        assert!(!old.contains(&live));
    }
}
