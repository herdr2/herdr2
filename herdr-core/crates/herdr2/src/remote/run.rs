//! Forward a call after granting a remote run access to its local parent.

use super::*;
use crate::runtime::TargetKey;

#[cfg(test)]
mod tests;

/// A new grant is shared by concurrent runs until each launch has an outcome.
/// Cancellation or a lost reply may hide a started child, so retain by default.
struct RunGrant<'a> {
    core: &'a Core,
    key: (TargetKey, String),
    keep: bool,
}

impl<'a> RunGrant<'a> {
    fn start(core: &'a Core, m: &Machine, method: &str, params: &Value) -> Option<Self> {
        // An explicit parent is resolved on the destination, not on this core.
        if !matches!(method, "agent.run" | "terminal.run") || params["no_parent"] == true || !params["parent"].is_null() {
            return None;
        }
        let caller = crate::addr::local(params["caller"].as_str()?, &core.info.id, &core.info.name).ok()?;
        let parent = TargetKey::parse(caller)?;
        let key = (parent, m.id.clone());
        let mut st = core.lock();
        match parent {
            TargetKey::Agent(id) => {
                let a = st.reg.agents.get(&id)?;
                if a.afar.contains(&m.id) && !st.afar_runs.contains_key(&key) {
                    return None;
                }
                st.afar_runs.entry(key.clone()).or_default().0 += 1;
                st.reg.agents.get_mut(&id)?.afar.insert(m.id.clone());
                core.save(&st);
            }
            TargetKey::Terminal(id) => {
                let t = st.pty_reg.terminals.get(&id)?;
                if t.afar.contains(&m.id) && !st.afar_runs.contains_key(&key) {
                    return None;
                }
                st.afar_runs.entry(key.clone()).or_default().0 += 1;
                st.pty_reg.terminals.get_mut(&id)?.afar.insert(m.id.clone());
                core.save_pty(&st);
            }
        }
        Some(Self { core, key, keep: true })
    }

    fn answered(mut self, result: &Result<Value, RpcError>) {
        if let Err(e) = result {
            self.keep = !not_started(e);
        }
    }
}

impl Drop for RunGrant<'_> {
    fn drop(&mut self) {
        let mut st = self.core.lock();
        let Some((pending, keep)) = st.afar_runs.get_mut(&self.key) else { return };
        *keep |= self.keep;
        *pending -= 1;
        if *pending != 0 {
            return;
        }
        let keep = *keep;
        st.afar_runs.remove(&self.key);
        if !keep {
            match self.key.0 {
                TargetKey::Agent(id) => {
                    if let Some(a) = st.reg.agents.get_mut(&id) {
                        a.afar.remove(&self.key.1);
                        self.core.save(&st);
                    }
                }
                TargetKey::Terminal(id) => {
                    if let Some(t) = st.pty_reg.terminals.get_mut(&id) {
                        t.afar.remove(&self.key.1);
                        self.core.save_pty(&st);
                    }
                }
            }
        }
    }
}

/// Only errors that prove no child started permit revoking a new grant.
/// In particular, spawn timeouts and persistence errors can follow a real spawn.
fn not_started(e: &RpcError) -> bool {
    match e.kind.as_str() {
        "invalid_params" | "invalid_template" | "config" | "name_taken" | "not_found" | "machine_unknown"
        | "unknown_method" | "spawn_failed" => true,
        "unavailable" => {
            e.message == "the core is stopping"
                || e.message == "this core has used every agent id"
                || e.message == "this core has used every terminal id"
        }
        _ => false,
    }
}

pub(super) async fn forward_once(core: &Core, m: &Machine, method: &str, mut params: Value) -> Result<Value, RpcError> {
    if matches!(method, "agent.run" | "terminal.run") {
        params["die_with_parent"] = json!(params["outlive_parent"] != true);
    }
    let deadline = if matches!(method, "agent.send" | "terminal.send") {
        Duration::MAX
    } else {
        deadline_for(core, method, &params)
    };
    let line = line(core, m).await?;
    let grant = RunGrant::start(core, m, method, &params);
    let result = line.call(&m.name, method, params, deadline).await;
    if let Some(grant) = grant {
        grant.answered(&result);
    }
    let reply = result.map_err(|e| match e.kind.as_str() {
        "unknown_method" => RpcError::new(
            "needs_update",
            format!("{}'s h2 is older and has no {method}", m.name),
            format!("h2 machine update {}", m.name),
        ),
        _ => e,
    })?;
    // Document and Drove contents are opaque: address-shaped keys belong to the client/subsystem.
    Ok(if method.starts_with("doc.") || method.starts_with("drove.") { reply } else { localize(core, &m.name, &[], reply) })
}
