//! Waiting: on one agent or terminal, or on children here and on other machines.

use super::*;
use crate::runtime::TargetKey;
use crate::terminals::Terminal;

/// The given seconds, else config `wait_timeout`, else 180.
pub(super) fn wait_timeout(core: &Core, given: Option<f64>) -> Result<Duration, RpcError> {
    let secs = match given {
        Some(s) => s,
        None => crate::config::delivery(&core.paths.config).wait_timeout.unwrap_or(180.0),
    };
    Ok(seconds(Some(secs))?.unwrap_or_default())
}

pub(super) async fn agent_wait(core: Ctx<'_>, p: WaitParams) -> Result<WaitResult, RpcError> {
    wait_impl(core, p.agent, p.children, p.output, p.timeout, p.no_timeout, p.caller, false).await
}

pub(super) async fn wait_impl(
    core: Ctx<'_>,
    target_name: Option<String>,
    children: Option<Children>,
    output: Option<String>,
    timeout: Option<f64>,
    no_timeout: bool,
    caller: Option<String>,
    pty_only: bool,
) -> Result<WaitResult, RpcError> {
    let timeout = if no_timeout { None } else { Some(wait_timeout(core, timeout)?) };
    if let Some(mode) = children {
        let result = wait_children_everywhere(
            core,
            target_name,
            children,
            output,
            timeout.map(|t| t.as_secs_f64()),
            caller,
            &word(&mode),
            timeout,
        )
        .await;
        core.flush_saved().await?;
        return result;
    }
    let name = target_name.as_deref().ok_or_else(|| {
        RpcError::invalid_params(if pty_only { "give a terminal, or children" } else { "give an agent, or children" })
    })?;
    let target = if pty_only { TargetKey::Terminal(resolve_pty(core, name)?) } else { resolve_any(core, name)? };
    let bare = output.is_none();
    let key = caller.clone().filter(|_| bare).map(|c| (c, target));
    let after_turn = key.as_ref().and_then(|k| core.lock().last_send.get(k).copied());
    let spec = WaitSpec { output, timeout, after_turn };
    let r = watched(core, caller.clone(), target, spec).await;
    if r.ended
        && let Some(k) = key
    {
        let mut st = core.lock();
        if st.last_send.get(&k).copied() == after_turn {
            st.last_send.remove(&k);
        }
    }
    core.flush_saved().await?;
    Ok(r)
}

/// A caller's wait in progress: while it runs, its parent needs no note about it.
pub(super) struct Watching {
    core: Arc<Core>,
    key: Option<(String, TargetKey)>,
}

impl Drop for Watching {
    fn drop(&mut self) {
        if let Some(key) = &self.key {
            let mut st = self.core.lock();
            if let Some(n) = st.waiting.get_mut(key) {
                *n -= 1;
                if *n == 0 {
                    st.waiting.remove(key);
                }
            }
        }
    }
}

/// A wait the caller is watching; afterwards it has seen the result, and a
/// `--rm` child it was waiting for can go.
pub(super) async fn watched(core: &Arc<Core>, caller: Option<String>, target: TargetKey, spec: WaitSpec) -> WaitResult {
    let result = watch(core, caller.clone(), target, spec).await;
    has_seen(core, caller.as_deref(), target);
    result
}

/// `watched` without removing a finished `--rm` child.
pub(super) async fn watch(core: &Arc<Core>, caller: Option<String>, target: TargetKey, spec: WaitSpec) -> WaitResult {
    let key = caller.clone().map(|c| (c, target));
    if let Some(key) = &key {
        *core.lock().waiting.entry(key.clone()).or_default() += 1;
    }
    let guard = Watching { core: core.clone(), key };
    let waiter = caller.as_deref().and_then(TargetKey::parse);
    let result = core.wait(target, spec, waiter).await;
    match target {
        TargetKey::Agent(id) => core.saw(caller.as_deref(), id),
        TargetKey::Terminal(id) => core.pty_saw(caller.as_deref(), id),
    }
    drop(guard);
    result
}

/// The caller has this child's news now; a finished `--rm` child of its can go.
pub(super) fn has_seen(core: &Core, caller: Option<&str>, target: TargetKey) {
    match target {
        TargetKey::Agent(id) => {
            core.saw(caller, id);
            let mut st = core.lock();
            if st.reg.agents.get(&id).is_some_and(|a| !a.is_live() && a.rm && a.parent.as_deref() == caller) {
                core.remove(&mut st, id);
            }
        }
        TargetKey::Terminal(id) => {
            core.pty_saw(caller, id);
            let mut st = core.lock();
            if st.pty_reg.terminals.get(&id).is_some_and(|t| !t.is_live() && t.rm && t.parent.as_deref() == caller) {
                core.pty_remove(&mut st, id);
            }
        }
    }
}

#[derive(Clone)]
enum Child {
    Agent(Agent),
    Terminal(Terminal),
}

impl Child {
    fn key(&self) -> TargetKey {
        match self {
            Self::Agent(a) => TargetKey::Agent(a.id()),
            Self::Terminal(t) => TargetKey::Terminal(t.id()),
        }
    }

    fn is_live(&self) -> bool {
        match self {
            Self::Agent(a) => a.is_live(),
            Self::Terminal(t) => t.is_live(),
        }
    }

    fn busy(&self) -> bool {
        match self {
            Self::Agent(a) => {
                a.is_live()
                    && !a.program_lost()
                    && (!a.reported
                        || a.first_task_turn.is_some()
                        || a.status.as_ref().is_some_and(|s| s.state == "working"))
            }
            Self::Terminal(t) => {
                t.is_live()
                    && !t.program_lost()
                    && (!t.reported
                        || t.first_task_turn.is_some()
                        || t.status.as_ref().is_some_and(|s| s.state == "working"))
            }
        }
    }

    fn news(&self) -> u64 {
        match self {
            Self::Agent(a) => a.news,
            Self::Terminal(t) => t.news,
        }
    }

    fn seen_max_noted(&self) -> u64 {
        match self {
            Self::Agent(a) => a.seen.max(a.noted),
            Self::Terminal(t) => t.seen.max(t.noted),
        }
    }

    fn first_task_turn(&self) -> Option<u64> {
        match self {
            Self::Agent(a) => a.first_task_turn,
            Self::Terminal(t) => t.first_task_turn,
        }
    }

    fn as_is(&self, ended: bool) -> WaitResult {
        match self {
            Self::Agent(a) => {
                let reason = match (ended, a.is_live() && !a.program_lost()) {
                    (false, _) => "running",
                    (true, true) => "status",
                    (true, false) => "exit",
                };
                WaitResult {
                    agent: a.address.clone(),
                    ended,
                    reason: reason.into(),
                    status: a.status.as_ref().map(|s| s.state.clone()),
                    turn: a.turn,
                    exit_code: a.exit_code,
                    signal: a.signal,
                    children: Vec::new(),
                    from: None,
                }
            }
            Self::Terminal(t) => {
                let reason = match (ended, t.is_live() && !t.program_lost()) {
                    (false, _) => "running",
                    (true, true) => "status",
                    (true, false) => "exit",
                };
                WaitResult {
                    agent: t.address.clone(),
                    ended,
                    reason: reason.into(),
                    status: t.status.as_ref().map(|s| s.state.clone()),
                    turn: t.turn,
                    exit_code: t.exit_code,
                    signal: t.signal,
                    children: Vec::new(),
                    from: None,
                }
            }
        }
    }
}

/// Resolve whose children: a local agent or terminal, or a parent on a peer.
pub(super) fn children_of(
    core: &Core,
    target_name: Option<&str>,
    caller: Option<&str>,
) -> Result<(Option<TargetKey>, String), RpcError> {
    match (target_name, caller) {
        (Some(a), _) => match crate::remote::peer_address(core, a) {
            Some(afar) => Ok((None, afar)),
            None => resolve_any(core, a).map(|k| (Some(k), k.address())),
        },
        (None, Some(caller)) => TargetKey::parse(caller).map(|k| (Some(k), k.address())).ok_or_else(|| {
            RpcError::new("needs_target", "whose children?", "run it inside an agent or terminal, or name the parent")
        }),
        (None, None) => Err(RpcError::new("needs_target", "whose children?", "name the parent")),
    }
}

pub(super) async fn wait_children_everywhere(
    core: &Arc<Core>,
    target_name: Option<String>,
    children_mode: Option<Children>,
    _output: Option<String>,
    timeout_secs: Option<f64>,
    caller: Option<String>,
    mode: &str,
    timeout: Option<Duration>,
) -> Result<WaitResult, RpcError> {
    let (parent, parent_addr) = children_of(core, target_name.as_deref(), caller.as_deref())?;
    let afar: Vec<crate::machines::Machine> = {
        let st = core.lock();
        let ids = parent
            .and_then(|k| match k {
                TargetKey::Agent(id) => st.reg.agents.get(&id).map(|a| a.afar.clone()),
                TargetKey::Terminal(id) => st.pty_reg.terminals.get(&id).map(|t| t.afar.clone()),
            })
            .unwrap_or_default();
        st.machines.iter().filter(|m| ids.contains(&m.id)).cloned().collect()
    };
    let Some(parent) = parent.filter(|_| !afar.is_empty()) else {
        return wait_children(core, target_name, caller, None, mode, timeout, false).await;
    };
    let mut sides = tokio::task::JoinSet::new();
    let (c, m, tn, cal) = (core.clone(), mode.to_string(), target_name.clone(), caller.clone());
    sides.spawn(async move {
        let _ = (children_mode, timeout_secs);
        wait_children(&c, tn, cal, None, &m, timeout, true).await
    });
    for machine in afar {
        let (method, field) = match parent {
            TargetKey::Agent(_) => ("agent.wait", "agent"),
            TargetKey::Terminal(_) => ("terminal.wait", "terminal"),
        };
        let mut params = json!({
            field: format!("{}/{}", core.info.id, parent.address()),
            "children": mode,
            "caller": caller.as_ref().map(|c| format!("{}/{c}", core.info.id)),
        });
        if let Some(t) = timeout {
            params["timeout"] = json!(t.as_secs_f64());
        } else {
            params["no_timeout"] = json!(true);
        }
        let c = core.clone();
        sides.spawn(async move {
            let r = crate::remote::forward(&c, &machine, method, params).await?;
            serde_json::from_value::<WaitResult>(r).map_err(|e| RpcError::internal(e.to_string()))
        });
    }
    let mut merged: Option<WaitResult> = None;
    while let Some(side) = sides.join_next().await {
        let Ok(Ok(r)) = side else { continue };
        if r.reason == "message" || (mode == "any" && r.ended && !r.children.is_empty()) {
            return Ok(WaitResult { agent: parent_addr, ..r });
        }
        merged = Some(match merged {
            None => WaitResult { agent: parent_addr.clone(), ..r },
            Some(mut m) => {
                m.children.extend(r.children);
                if !r.ended {
                    (m.ended, m.reason) = (false, r.reason);
                }
                m
            }
        });
    }
    merged.ok_or_else(|| RpcError::new("unavailable", "no answer from any machine", ""))
}

pub(super) async fn wait_children(
    core: &Arc<Core>,
    target_name: Option<String>,
    caller: Option<String>,
    output: Option<String>,
    mode: &str,
    timeout: Option<Duration>,
    others: bool,
) -> Result<WaitResult, RpcError> {
    if output.is_some() {
        return Err(RpcError::invalid_params("children waits take no output; wait on one agent for that"));
    }
    let (parent, address) = children_of(core, target_name.as_deref(), caller.as_deref())?;
    let done = |children: Vec<WaitResult>| WaitResult {
        agent: address.clone(),
        ended: true,
        reason: "children".into(),
        status: None,
        turn: 0,
        exit_code: None,
        signal: None,
        children,
        from: None,
    };
    let any = mode == "any";
    let listen = caller.as_deref() == Some(address.as_str());
    let kids_of = |st: &crate::runtime::State| -> Vec<Child> {
        let mut kids: Vec<Child> = st
            .reg
            .agents
            .values()
            .filter(|a| a.parent.as_deref() == Some(address.as_str()))
            .cloned()
            .map(Child::Agent)
            .collect();
        kids.extend(
            st.pty_reg
                .terminals
                .values()
                .filter(|t| t.parent.as_deref() == Some(address.as_str()))
                .cloned()
                .map(Child::Terminal),
        );
        kids
    };
    let (start, _watching): (std::collections::HashMap<TargetKey, u64>, Vec<Watching>) = {
        let mut st = core.lock();
        let kids = kids_of(&st);
        let watching = kids
            .iter()
            .filter_map(|k| caller.clone().map(|c| (c, k.key())))
            .map(|key| {
                *st.waiting.entry(key.clone()).or_default() += 1;
                Watching { core: core.clone(), key: Some(key) }
            })
            .collect();
        (kids.iter().map(|k| (k.key(), k.news())).collect(), watching)
    };
    let news = |k: &Child| match listen {
        true => k.news() > k.seen_max_noted(),
        false => k.news() > start.get(&k.key()).copied().unwrap_or(0),
    };
    let seen = |keys: &[TargetKey]| keys.iter().for_each(|&k| has_seen(core, caller.as_deref(), k));
    let deadline = timeout.map(|t| tokio::time::Instant::now() + t);
    loop {
        let changed = core.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let (kids, message) = {
            let mut st = core.lock();
            (kids_of(&st), parent.filter(|_| listen).and_then(|key| st.messages.remove(&key)))
        };
        let ready: Option<Vec<&Child>> = match any {
            true => {
                let fresh: Vec<&Child> =
                    kids.iter().filter(|k| news(k) && (!k.busy() || k.first_task_turn().is_none())).collect();
                let nothing_can_happen = !others && !kids.iter().any(Child::is_live) && message.is_none();
                (!fresh.is_empty() || message.is_some() || nothing_can_happen).then_some(fresh)
            }
            false => (!kids.iter().any(Child::busy)).then(|| kids.iter().collect()),
        };
        if let (None, Some(from)) = (&ready, &message) {
            let children = kids.iter().map(|k| k.as_is(!k.busy())).collect();
            return Ok(WaitResult { ended: false, reason: "message".into(), from: from.clone(), ..done(children) });
        }
        if let Some(results) = ready {
            seen(&results.iter().map(|k| k.key()).collect::<Vec<_>>());
            let mut r = done(results.iter().map(|k| k.as_is(true)).collect());
            if let Some(from) = message {
                if r.children.is_empty() {
                    r.reason = "message".into();
                }
                r.from = from;
            }
            return Ok(r);
        }
        if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) {
            let children = kids.iter().filter(|k| k.is_live()).map(|k| k.as_is(!k.busy())).collect();
            return Ok(WaitResult { ended: false, reason: "timeout".into(), ..done(children) });
        }
        match deadline {
            Some(d) => {
                let _ = tokio::time::timeout_at(d, changed).await;
            }
            None => changed.await,
        }
    }
}
