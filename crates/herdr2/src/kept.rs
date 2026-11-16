//! Kept ACP agent recovery after crashes or planned core restarts (`session/load` when supported).

use crate::acp::{self, SpawnSpec};
use crate::agents::{Agent, KEEP_RETRY_MS};
use crate::runtime::{Core, Exit, now_ms};
use crate::status::StatusReport;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::Ordering;

impl Agent {
    pub(crate) fn schedule_restart(&mut self, now: u64) {
        let stable = self.program_started_ms.is_some_and(|at| now.saturating_sub(at) >= 10_000);
        self.restart_delay_ms = if stable {
            KEEP_RETRY_MS
        } else {
            match self.restart_delay_ms {
                0 => KEEP_RETRY_MS,
                KEEP_RETRY_MS => 3000,
                3000 => 6000,
                6000 => 12_000,
                12_000 => 30_000,
                _ => 60_000,
            }
        };
        self.restart_after_ms = now + self.restart_delay_ms;
        self.program_started_ms = None;
    }
}

impl Core {
    pub(crate) fn spawn_failed(&self, st: &mut crate::runtime::State, id: u32) {
        let Some(a) = st.reg.agents.get_mut(&id) else { return };
        a.pid = None;
        if st.killing.contains(&id) || !a.keep || a.reason.as_deref().is_none_or(|r| r == "spawn_pending") {
            let reason = a.reason.clone().filter(|r| r != "spawn_pending").unwrap_or_else(|| "crashed".into());
            self.record_finish(st, id, Exit { code: None, signal: None }, &reason);
        } else {
            a.schedule_restart(now_ms());
        }
        self.save(st);
        self.changed.notify_waiters();
    }

    pub(crate) async fn recover_kept(self: Arc<Self>, startup: bool) {
        if crate::platform::host_shutdown_in_progress()
            || self.sweeps.load(Ordering::SeqCst) != 0
            || self.lock().stopping
            || self.restart.load(Ordering::SeqCst)
        {
            return;
        }

        let candidates: Vec<(u32, String, Vec<String>, String, Option<String>, bool, Option<String>, Option<String>, Option<String>, bool, std::collections::BTreeMap<String, String>)> = {
            let mut st = self.lock();
            if st.stopping || self.restart.load(Ordering::SeqCst) {
                return;
            }
            let ids: Vec<u32> = st
                .reg
                .agents
                .values()
                .filter(|a| {
                    a.is_live()
                        && a.keep
                        && !st.sessions.contains_key(&a.id())
                        && !st.recovering.contains(&a.id())
                        && (startup || a.restart_after_ms <= now_ms())
                })
                .map(Agent::id)
                .collect();
            for &id in &ids {
                st.recovering.insert(id);
                if let Some(a) = st.reg.agents.get_mut(&id) {
                    a.pid = None;
                }
            }
            ids.into_iter()
                .filter_map(|id| {
                    let a = st.reg.agents.get(&id)?;
                    Some((
                        id,
                        a.address.clone(),
                        a.argv.clone(),
                        a.cwd.clone(),
                        a.acp_session_id.clone(),
                        a.can_load_session,
                        a.system_prompt.clone(),
                        a.model.clone(),
                        a.mode.clone(),
                        a.inject_mcp,
                        a.restart_env.clone(),
                    ))
                })
                .collect()
        };

        if candidates.is_empty() {
            return;
        }

        let base_env = crate::platform::program_env().await;
        let mcp_exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "herdr2".into());

        for (id, address, argv, orig_cwd, resume_session_id, can_load_session, system_prompt, model, mode, inject_mcp, restart_env) in candidates {
            if self.lock().stopping || self.restart.load(Ordering::SeqCst) {
                self.lock().recovering.remove(&id);
                continue;
            }

            let env = crate::api::child_env(base_env.clone(), restart_env);
            let cwd = if std::path::Path::new(&orig_cwd).is_dir() {
                orig_cwd
            } else {
                env.get("HOME").or_else(|| env.get("USERPROFILE")).cloned().unwrap_or_else(|| "/".into())
            };
            let (env, scope) = crate::api::agent_env(&self, id, &cwd, env);
            {
                let mut st = self.lock();
                if let Some(a) = st.reg.agents.get_mut(&id) {
                    a.scope = Some(scope);
                    a.cwd = cwd.clone();
                }
                self.save(&st);
            }

            let spec = SpawnSpec {
                address: address.clone(),
                argv,
                env,
                cwd,
                resume_session_id,
                can_load_session,
                system_prompt,
                model,
                mode,
                inject_mcp,
                mcp_exe: mcp_exe.clone(),
                socket: self.paths.socket.display().to_string(),
            };

            match acp::spawn_session(spec).await {
                Ok(started) => {
                    let mut st = self.lock();
                    st.recovering.remove(&id);
                    if !st.reg.agents.get(&id).is_some_and(Agent::is_live) || st.stopping || st.killing.contains(&id) {
                        started.transport.kill();
                        continue;
                    }
                    let agent_info = started.agent_info.clone();
                    let acp_sid = started.session_id.clone();
                    let can_load = started.can_load_session;
                    let handle = acp::spawn_actor(self.clone(), id, started);
                    let pid = handle.transport.pid();
                    st.sessions.insert(id, handle);
                    if let Some(a) = st.reg.agents.get_mut(&id) {
                        a.pid = pid;
                        a.acp_session_id = Some(acp_sid);
                        a.can_load_session = can_load;
                        if agent_info.is_some() {
                            a.agent_info = agent_info;
                        }
                        a.reason = None;
                        a.pending_permission = None;
                        a.program_started_ms = Some(now_ms());
                        a.reported = true;
                    }
                    self.apply_status(&mut st, id, &StatusReport::root("idle"));
                    let public = serde_json::to_value(crate::agents::Public(st.reg.agents[&id].clone())).unwrap_or_default();
                    self.emit(&mut st, "restarted", id, public);
                    self.save(&st);
                    self.changed.notify_waiters();
                }
                Err(err) => {
                    let _ = writeln!(io::stderr(), "core: kept agent {} restart failed: {}", address, err.message);
                    let mut st = self.lock();
                    st.recovering.remove(&id);
                    self.spawn_failed(&mut st, id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::agents::{NewAgent, Registry};

    #[test]
    fn retries_back_off_forever_and_reset_only_after_a_stable_program() {
        let mut reg = Registry::default();
        let id = reg.create(NewAgent { argv: vec!["sh".into()], keep: true, ..Default::default() }, 0).unwrap();
        let a = reg.agents.get_mut(&id).unwrap();
        for delay in [1500, 3000, 6000, 12_000, 30_000, 60_000, 60_000] {
            a.schedule_restart(100_000);
            assert_eq!(a.restart_delay_ms, delay);
            assert_eq!(a.restart_after_ms, 100_000 + delay);
        }
        a.program_started_ms = Some(90_001);
        a.schedule_restart(100_000);
        assert_eq!(a.restart_delay_ms, 60_000, "a short-lived program does not reset retries");
        a.program_started_ms = Some(90_000);
        a.schedule_restart(100_000);
        assert_eq!(a.restart_delay_ms, 1500);
        a.schedule_restart(100_000);
        assert_eq!(a.restart_delay_ms, 3000);
        assert!(a.is_live());
    }
}
