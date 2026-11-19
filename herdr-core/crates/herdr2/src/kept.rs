//! Kept ACP agent recovery after crashes or planned core restarts (`session/load` when supported).

use crate::acp::{self, SpawnSpec};
use crate::agents::{Agent, KEEP_RETRY_MS};
use crate::holder_proto as proto;
use crate::runtime::{Core, Exit, now_ms};
use crate::status::StatusReport;
use crate::terminals::{KEEP_RETRY_MS as PTY_KEEP_RETRY_MS, Terminal};
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::Ordering;

fn next_retry_delay(prev: u64, base: u64, started: Option<u64>, now: u64) -> u64 {
    let stable = started.is_some_and(|at| now.saturating_sub(at) >= 10_000);
    if stable {
        base
    } else {
        match prev {
            0 => base,
            x if x == base => 3000,
            3000 => 6000,
            6000 => 12_000,
            12_000 => 30_000,
            _ => 60_000,
        }
    }
}

impl Agent {
    pub(crate) fn schedule_restart(&mut self, now: u64) {
        self.restart_delay_ms = next_retry_delay(self.restart_delay_ms, KEEP_RETRY_MS, self.program_started_ms, now);
        self.restart_after_ms = now + self.restart_delay_ms;
        self.program_started_ms = None;
    }
}

impl Terminal {
    pub(crate) fn schedule_restart(&mut self, now: u64) {
        self.restart_delay_ms = next_retry_delay(self.restart_delay_ms, PTY_KEEP_RETRY_MS, self.program_started_ms, now);
        self.restart_after_ms = now + self.restart_delay_ms;
        self.program_started_ms = None;
    }
}

/// Under what a lost PTY program left on screen, so it isn't taken for something still running.
const RESTORED: &[u8] = b"\x1b[0m\r\n\x1b[2m-- the program above ended; this is a new shell --\x1b[0m\r\n";

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

    pub(crate) fn pty_spawn_failed(&self, st: &mut crate::runtime::State, id: u32) {
        let t = st.pty_reg.terminals.get_mut(&id).expect("failed spawn");
        t.holder = None;
        t.pid = None;
        if st.pty_killing.contains(&id) || !t.keep || t.reason.as_deref().is_none_or(|r| r == "spawn_pending") {
            let reason = t.reason.clone().filter(|r| r != "spawn_pending").unwrap_or_else(|| "holder_lost".into());
            self.pty_record_finish(st, id, Exit { code: None, signal: None }, &reason);
        } else {
            t.schedule_restart(now_ms());
        }
        self.save_pty(st);
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

        let candidates: Vec<_> = {
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
                        a.config_overrides.clone(),
                        a.inject_mcp,
                        a.restart_env.clone(),
                    ))
                })
                .collect()
        };

        if !candidates.is_empty() {
            let base_env = crate::platform::program_env().await;
            let mcp_exe = crate::client::exe_path().map(|p| p.display().to_string()).unwrap_or_else(|| "herdr2".into());

            for (id, address, argv, orig_cwd, resume_session_id, can_load_session, system_prompt, model, mode, config_overrides, inject_mcp, restart_env) in candidates {
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
                    dangerously_skip_permissions: false,
                    config_overrides,
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
                        let config_options = started.config_options.clone();
                        let context_usage = started.context_usage;
                        let handle = acp::spawn_actor(self.clone(), id, started);
                        let pid = handle.transport.pid();
                        st.sessions.insert(id, handle);
                        if let Some(a) = st.reg.agents.get_mut(&id) {
                            a.pid = pid;
                            a.acp_session_id = Some(acp_sid);
                            a.can_load_session = can_load;
                            a.config_options = config_options;
                            if let Some(usage) = context_usage {
                                a.context_usage = Some(usage);
                            }
                            acp::sync_config_to_agent(a);
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
        self.recover_kept_pty(startup).await;
    }

    pub(crate) async fn recover_kept_pty(self: Arc<Self>, startup: bool) {
        let pending = self.lock().pty_reg.terminals.values().any(|t| t.is_live() && t.keep && t.holder.is_none());
        if !pending || crate::platform::host_shutdown_in_progress() {
            return;
        }
        if self.sweeps.load(Ordering::SeqCst) != 0 || self.lock().stopping || self.restart.load(Ordering::SeqCst) {
            return;
        }
        if let Err(e) = self.ensure_holder().await {
            let _ = writeln!(io::stderr(), "core: kept terminals waiting for a holder: {e}");
            return;
        }
        let (shell, restore_screen) = match crate::config::load(&self.paths.config)
            .and_then(|settings| settings.shell().map(|shell| (shell, settings.restore_screen.unwrap_or(false))))
        {
            Ok(settings) => settings,
            Err(e) => {
                let _ = writeln!(io::stderr(), "core: kept terminals waiting for a shell: {}", e.message);
                return;
            }
        };
        let base_env = crate::platform::program_env().await;
        let mut st = self.lock();
        if st.stopping
            || self.restart.load(Ordering::SeqCst)
            || self.sweeps.load(Ordering::SeqCst) != 0
            || crate::platform::host_shutdown_in_progress()
        {
            return;
        }
        let ids: Vec<u32> = st
            .pty_reg
            .terminals
            .values()
            .filter(|t| {
                t.is_live()
                    && t.keep
                    && t.holder.is_none()
                    && !st.pty_respawning.contains(&t.id())
                    && (startup || t.restart_after_ms <= now_ms())
            })
            .map(Terminal::id)
            .collect();
        for id in ids {
            let Some(holder) = st.current() else { break };
            let instance = holder.instance.clone();
            let t = st.pty_reg.terminals.get_mut(&id).expect("pending");
            let env = crate::api::pty_child_env(base_env.clone(), t.restart_env.clone());
            let cwd = t
                .last_cwd
                .as_ref()
                .filter(|p| std::path::Path::new(p).is_dir())
                .or_else(|| std::path::Path::new(&t.cwd).is_dir().then_some(&t.cwd))
                .cloned()
                .unwrap_or_else(|| {
                    env.get("HOME").or_else(|| env.get("USERPROFILE")).cloned().unwrap_or_else(|| "/".into())
                });
            let (env, scope) = crate::api::terminal_env(&self, id, &cwd, env);
            t.scope = Some(scope);
            t.holder = Some(instance);
            t.argv = shell.clone();
            t.cwd = cwd.clone();
            t.template = None;
            t.reported = false;
            t.title.clear();
            t.full_screen = false;
            t.win32_input = false;
            t.printed = 0;
            t.screen = "exact".into();
            t.submit = None;
            t.stop = None;
            t.paste_gap_ms = None;
            let spawn = proto::Spawn { argv: t.argv.clone(), env, cwd, cols: t.cols, rows: t.rows };
            let identity = t.identity.clone();
            if !restore_screen {
                let _ = std::fs::remove_file(self.snapshot_path(id));
            }
            let mut screen = st.screens.remove(&id).filter(|_| restore_screen).unwrap_or_else(|| {
                let mut screen = ghostty_vt::Terminal::new(spawn.cols, spawn.rows, self.history_bytes());
                if restore_screen && let Some(old) = self.take_saved_screen(id) {
                    screen.write(&old.replay());
                    screen.write(RESTORED);
                    if cfg!(windows) {
                        screen.write(&b"\n".repeat(spawn.rows as usize));
                        screen.write(b"\x1b[H");
                    }
                }
                screen
            });
            screen.set_xtversion(&identity);
            st.screens.insert(id, screen);
            self.save_pty(&st);
            if let Some(link) = st.link(id) {
                link.send(&proto::json_frame(proto::SPAWN, id, &spawn));
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
