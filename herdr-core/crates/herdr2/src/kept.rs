//! Kept ACP agent recovery after crashes or planned core restarts (`session/load` when supported).

use crate::acp::{self, SpawnSpec};
use crate::agents::{Agent, KEEP_RETRY_MS};
use crate::holder_proto as proto;
use crate::permissions::PermissionMode;
use crate::runtime::{Core, Exit, now_ms};
use crate::status::StatusReport;
use crate::terminals::{KEEP_RETRY_MS as PTY_KEEP_RETRY_MS, SavedCliResume, Terminal};
use std::io::{self, Write};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

/// Limits simultaneous ACP session handshakes during mass recovery after reboot.
const MAX_CONCURRENT_ACP_RECOVERIES: usize = 4;

fn acp_recovery_semaphore() -> Arc<tokio::sync::Semaphore> {
    static SEM: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(SEM.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_ACP_RECOVERIES))))
}

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
        self.restart_delay_ms =
            next_retry_delay(self.restart_delay_ms, PTY_KEEP_RETRY_MS, self.program_started_ms, now);
        self.restart_after_ms = now + self.restart_delay_ms;
        self.program_started_ms = None;
    }
}

/// Under what a lost PTY program left on screen, so it isn't taken for something still running.
const RESTORED: &[u8] = b"\x1b[0m\r\n\x1b[2m-- the program above ended; this is a new shell --\x1b[0m\r\n";

fn is_drove_worktree_path(path: &str) -> bool {
    let norm = path.replace('\\', "/");
    norm.contains("/drove/worktrees/")
}

/// Detects when a kept agent cannot be restarted because its deleted Drove worktree or a
/// `bwrap` bind source path no longer exists on disk.
fn missing_unrecoverable_workspace_path(a: &Agent) -> Option<String> {
    if !a.cwd.is_empty()
        && !std::path::Path::new(&a.cwd).is_dir()
        && (a.labels.contains_key("drove.worker") || is_drove_worktree_path(&a.cwd))
    {
        return Some(a.cwd.clone());
    }
    let uses_bwrap = a.argv.first().is_some_and(|bin| {
        bin == "bwrap" || bin.ends_with("/bwrap") || bin.ends_with("\\bwrap")
    });
    if uses_bwrap {
        let mut i = 1;
        while i < a.argv.len() {
            match a.argv[i].as_str() {
                "--bind" | "--ro-bind" | "--dev-bind" => {
                    if let Some(src) = a.argv.get(i + 1)
                        && !src.is_empty()
                        && !std::path::Path::new(src).exists()
                    {
                        return Some(src.clone());
                    }
                    i += 3;
                }
                "--chdir" => {
                    if let Some(dir) = a.argv.get(i + 1)
                        && !dir.is_empty()
                        && is_drove_worktree_path(dir)
                        && !std::path::Path::new(dir).is_dir()
                    {
                        return Some(dir.clone());
                    }
                    i += 2;
                }
                "--" => break,
                _ => i += 1,
            }
        }
    }
    None
}

fn is_permanent_missing_path_error(msg: &str) -> bool {
    let low = msg.to_ascii_lowercase();
    low.contains("bwrap: can't find source path")
        || (low.contains("bwrap") && low.contains("no such file or directory"))
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
            let mut retired_missing_ws: Vec<(u32, String, String)> = Vec::new();
            let mut active_candidates = Vec::new();
            for id in ids {
                let Some(a) = st.reg.agents.get(&id) else { continue };
                if let Some(missing_path) = missing_unrecoverable_workspace_path(a) {
                    retired_missing_ws.push((id, a.address.clone(), missing_path));
                    continue;
                }
                let skip_perms = a.permission_mode == PermissionMode::ApproveAll
                    || a.mode.as_deref().is_some_and(crate::permissions::is_bypass_mode_id);
                let fallback_handoff_context = if a.acp_session_id.is_some() && a.turn > 0 {
                    st.transcripts
                        .get(&id)
                        .and_then(|tr| crate::transcript::build_portable_handoff(tr.entries(), a.turn, 16 * 1024))
                } else {
                    None
                };
                active_candidates.push((
                    id,
                    a.address.clone(),
                    a.argv.clone(),
                    a.cwd.clone(),
                    a.acp_session_id.clone(),
                    a.can_load_session,
                    a.system_prompt.clone(),
                    fallback_handoff_context,
                    a.model.clone(),
                    a.mode.clone(),
                    skip_perms,
                    a.config_overrides.clone(),
                    a.inject_mcp,
                    a.restart_env.clone(),
                ));
            }
            for (id, address, missing_path) in retired_missing_ws {
                let _ = writeln!(
                    io::stderr(),
                    "core: kept agent {} retired (missing workspace/sandbox path: {})",
                    address,
                    missing_path
                );
                st.recovering.remove(&id);
                if let Some(a) = st.reg.agents.get_mut(&id) {
                    a.keep = false;
                }
                self.record_finish(&mut st, id, Exit { code: None, signal: None }, "workspace_removed");
                self.save(&st);
                self.changed.notify_waiters();
            }
            active_candidates
        };

        if !candidates.is_empty() {
            let base_env = crate::platform::program_env().await;
            let mcp_exe = crate::client::exe_path().map(|p| p.display().to_string()).unwrap_or_else(|| "herdr2".into());
            let sem = acp_recovery_semaphore();

            for (
                id,
                address,
                argv,
                orig_cwd,
                resume_session_id,
                can_load_session,
                system_prompt,
                fallback_handoff_context,
                model,
                mode,
                dangerously_skip_permissions,
                config_overrides,
                inject_mcp,
                restart_env,
            ) in candidates
            {
                let core = Arc::clone(&self);
                let base_env = base_env.clone();
                let mcp_exe = mcp_exe.clone();
                let sem = Arc::clone(&sem);
                tokio::spawn(async move {
                    let Ok(_permit) = sem.acquire_owned().await else {
                        core.lock().recovering.remove(&id);
                        return;
                    };
                    if core.lock().stopping || core.restart.load(Ordering::SeqCst) {
                        core.lock().recovering.remove(&id);
                        return;
                    }

                    let env = crate::api::child_env(base_env, restart_env);
                    let cwd = if std::path::Path::new(&orig_cwd).is_dir() {
                        orig_cwd
                    } else {
                        env.get("HOME").or_else(|| env.get("USERPROFILE")).cloned().unwrap_or_else(|| "/".into())
                    };
                    let (env, scope) = crate::api::agent_env(&core, id, &cwd, env);
                    {
                        let mut st = core.lock();
                        if let Some(a) = st.reg.agents.get_mut(&id) {
                            a.scope = Some(scope);
                            a.cwd = cwd.clone();
                        }
                        core.save(&st);
                    }

                    let spec = SpawnSpec {
                        address: address.clone(),
                        argv,
                        env,
                        cwd,
                        resume_session_id,
                        can_load_session,
                        system_prompt,
                        fallback_handoff_context,
                        model,
                        mode,
                        dangerously_skip_permissions,
                        config_overrides,
                        inject_mcp,
                        mcp_profile: None,
                        mcp_exe,
                        socket: core.paths.socket.display().to_string(),
                        handshake_timeout: None,
                    };

                    match acp::spawn_session(spec).await {
                        Ok(started) => {
                            let cleared_preset = {
                                let mut st = core.lock();
                                st.recovering.remove(&id);
                                if !st.reg.agents.get(&id).is_some_and(Agent::is_live)
                                    || st.stopping
                                    || st.killing.contains(&id)
                                {
                                    started.transport.kill();
                                    return;
                                }
                                let agent_info = started.agent_info.clone();
                                let acp_sid = started.session_id.clone();
                                let can_load = started.can_load_session;
                                let prompt_caps = started.prompt_capabilities;
                                let config_options = started.config_options.clone();
                                let available_commands = started.available_commands.clone();
                                let context_usage = started.context_usage;
                                let handle = acp::spawn_actor(core.clone(), id, started);
                                let pid = handle.transport.pid();
                                st.sessions.insert(id, handle);
                                let preset = st.reg.agents.get(&id).and_then(|a| a.agent.clone());
                                if let Some(a) = st.reg.agents.get_mut(&id) {
                                    a.pid = pid;
                                    a.acp_session_id = Some(acp_sid);
                                    a.can_load_session = can_load;
                                    a.prompt_capabilities = Some(prompt_caps);
                                    a.config_options = config_options;
                                    if !available_commands.is_empty() {
                                        a.available_commands = available_commands;
                                    }
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
                                core.apply_status(&mut st, id, &StatusReport::root("idle"));
                                let public = serde_json::to_value(crate::agents::Public(st.reg.agents[&id].clone()))
                                    .unwrap_or_default();
                                core.emit(&mut st, "restarted", id, public);
                                core.save(&st);
                                core.changed.notify_waiters();
                                preset
                            };
                            if let Some(ref preset) = cleared_preset {
                                core.drove.clear_probe_error_for_agent(preset);
                            }
                        }
                        Err(err) => {
                            let _ =
                                writeln!(io::stderr(), "core: kept agent {} restart failed: {}", address, err.message);
                            let mut st = core.lock();
                            st.recovering.remove(&id);
                            if is_permanent_missing_path_error(&err.message)
                                && let Some(a) = st.reg.agents.get_mut(&id)
                            {
                                a.keep = false;
                                a.reason = Some("workspace_removed".into());
                            }
                            core.spawn_failed(&mut st, id);
                        }
                    }
                });
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
        let (shell, restore_screen, restore_cli_sessions) =
            match crate::config::load(&self.paths.config).and_then(|settings| {
                settings
                    .shell()
                    .map(|shell| (shell, settings.restore_screen.unwrap_or(false), settings.restore_cli_sessions()))
            }) {
                Ok(settings) => settings,
                Err(e) => {
                    let _ = writeln!(io::stderr(), "core: kept terminals waiting for a shell: {}", e.message);
                    return;
                }
            };
        let base_env = crate::platform::program_env().await;

        struct PendingPty {
            id: u32,
            cwd: String,
            saved_cli: Option<SavedCliResume>,
        }

        let work: Vec<PendingPty> = {
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
            let mut items = Vec::with_capacity(ids.len());
            for id in ids {
                st.pty_respawning.insert(id);
                let Some(t) = st.pty_reg.terminals.get(&id).cloned() else { continue };
                let cwd = t
                    .last_cwd
                    .as_ref()
                    .filter(|p| std::path::Path::new(p).is_dir())
                    .or_else(|| std::path::Path::new(&t.cwd).is_dir().then_some(&t.cwd))
                    .cloned()
                    .unwrap_or_else(|| {
                        base_env
                            .get("HOME")
                            .or_else(|| base_env.get("USERPROFILE"))
                            .cloned()
                            .unwrap_or_else(|| "/".into())
                    });
                let mut saved_cli = if restore_cli_sessions { t.saved_cli.clone() } else { None };
                if restore_cli_sessions && saved_cli.is_none() {
                    // Backfill from checkpoints/argv if `saved_cli` was absent in an older state file.
                    let tc = self.target_checkpoints(&mut st, &t.address);
                    if let Some(extracted) = crate::pty_adapters::extract_saved_cli_from_argv(
                        &t.argv,
                        None,
                        tc.cli_session_id,
                        tc.cli_transcript_path,
                    ) {
                        saved_cli = Some(extracted);
                    }
                } else if let Some(ref mut sc) = saved_cli
                    && (sc.session_id.is_none() || sc.transcript_path.is_none())
                {
                    let tc = self.target_checkpoints(&mut st, &t.address);
                    if sc.session_id.is_none() {
                        sc.session_id = tc.cli_session_id;
                    }
                    if sc.transcript_path.is_none() {
                        sc.transcript_path = tc.cli_transcript_path;
                    }
                }
                items.push(PendingPty { id, cwd, saved_cli });
            }
            items
        };

        if work.is_empty() {
            return;
        }

        let hooks_dir = self.hooks_dir();
        let h2_exe = crate::pty_adapters::h2_exe_string();
        let shell_for_plan = shell.clone();
        let planned: Vec<(u32, String, Option<crate::pty_adapters::RecoveredCliPlan>)> =
            tokio::task::spawn_blocking(move || {
                work.into_iter()
                    .map(|item| {
                        let plan = item.saved_cli.as_ref().and_then(|sc| {
                            crate::pty_adapters::build_recovery_argv(
                                sc,
                                &item.cwd,
                                &shell_for_plan,
                                &hooks_dir,
                                &h2_exe,
                            )
                        });
                        (item.id, item.cwd, plan)
                    })
                    .collect()
            })
            .await
            .unwrap_or_default();

        let mut st = self.lock();
        if st.stopping
            || self.restart.load(Ordering::SeqCst)
            || self.sweeps.load(Ordering::SeqCst) != 0
            || crate::platform::host_shutdown_in_progress()
        {
            for (id, _, _) in &planned {
                st.pty_respawning.remove(id);
            }
            return;
        }

        for (id, cwd, cli_plan) in planned {
            st.pty_respawning.remove(&id);
            let Some(holder) = st.current() else { break };
            let instance = holder.instance.clone();
            let Some(t) = st.pty_reg.terminals.get_mut(&id).filter(|t| t.is_live() && t.holder.is_none()) else {
                continue;
            };
            let env = crate::api::pty_child_env(base_env.clone(), t.restart_env.clone());
            let (env, scope) = crate::api::terminal_env(&self, id, &cwd, env);
            let addr = t.address.clone();
            t.scope = Some(scope);
            t.holder = Some(instance);
            t.cwd = cwd.clone();
            t.template = None;
            t.reported = false;
            t.working_since = None;
            t.work_ms = None;
            t.status_ms = None;
            t.full_screen = false;
            t.win32_input = false;
            t.printed = 0;
            t.screen = "exact".into();
            t.submit = None;
            t.stop = None;
            t.paste_gap_ms = None;

            let is_cli_recovery = cli_plan.is_some();
            if let Some(plan) = cli_plan {
                t.argv = plan.argv;
                let kind_str =
                    t.saved_cli.as_ref().map(|s| s.kind.clone()).unwrap_or_else(|| {
                        crate::pty_adapters::PtyCliKind::detect(&plan.cli_argv).as_str().to_string()
                    });
                let prev_tp = t.saved_cli.as_ref().and_then(|s| s.transcript_path.clone());
                t.saved_cli = Some(SavedCliResume {
                    kind: kind_str.clone(),
                    session_id: plan.session_id.clone(),
                    transcript_path: if plan.resumed_existing { prev_tp } else { None },
                    cli_argv: plan.cli_argv,
                    wrapper_shell: Some(plan.wrapper_shell),
                });
                if !plan.resumed_existing {
                    t.turn = 0;
                    t.total_work_ms = 0;
                    t.semantic_title = None;
                    t.micro_summary = None;
                    t.title.clear();
                }
                let spawn = proto::Spawn { argv: t.argv.clone(), env, cwd, cols: t.cols, rows: t.rows };
                let identity = t.identity.clone();
                let _ = std::fs::remove_file(self.snapshot_path(id));
                let mut screen = ghostty_vt::Terminal::new(spawn.cols, spawn.rows, self.history_bytes());
                screen.set_xtversion(&identity);
                st.screens.insert(id, screen);
                let mut tc = self.target_checkpoints(&mut st, &addr);
                tc.cli_kind = Some(kind_str);
                tc.cli_fg_pid = None;
                if let Some(sid) = plan.session_id {
                    tc.cli_session_id = Some(sid);
                } else if !plan.resumed_existing {
                    tc.cli_session_id = None;
                }
                if !plan.resumed_existing {
                    tc.cli_transcript_path = None;
                }
                self.save_target_checkpoints(&mut st, tc);
                self.save_pty(&st);
                if let Some(link) = st.link(id) {
                    link.send(&proto::json_frame(proto::SPAWN, id, &spawn));
                }
            } else {
                t.argv = shell.clone();
                t.saved_cli = None;
                t.turn = 0;
                t.total_work_ms = 0;
                t.semantic_title = None;
                t.micro_summary = None;
                t.title.clear();
                let spawn = proto::Spawn { argv: t.argv.clone(), env, cwd, cols: t.cols, rows: t.rows };
                let identity = t.identity.clone();
                let replay_screen = restore_screen && !is_cli_recovery;
                if !replay_screen {
                    let _ = std::fs::remove_file(self.snapshot_path(id));
                }
                let mut screen = st.screens.remove(&id).filter(|_| replay_screen).unwrap_or_else(|| {
                    let mut screen = ghostty_vt::Terminal::new(spawn.cols, spawn.rows, self.history_bytes());
                    if replay_screen && let Some(old) = self.take_saved_screen(id) {
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

    #[test]
    fn restore_cli_sessions_config_defaults_on_and_can_be_disabled() {
        let default_cfg = crate::config::Config::default();
        assert!(default_cfg.restore_cli_sessions());
        assert!(!default_cfg.restore_screen.unwrap_or(false));

        let off: crate::config::Config = toml::from_str("restore_cli_sessions = false\n").unwrap();
        assert!(!off.restore_cli_sessions());
        assert!(!off.restore_screen.unwrap_or(false));
    }

    #[test]
    fn detects_missing_drove_worktree_and_bwrap_bind_paths() {
        use super::{is_permanent_missing_path_error, missing_unrecoverable_workspace_path};
        let mut reg = Registry::default();
        let id = reg
            .create(
                NewAgent {
                    argv: vec![
                        "bwrap".into(),
                        "--bind".into(),
                        "/nonexistent/drove/worktrees/run_x/w1".into(),
                        "/work".into(),
                    ],
                    cwd: "/nonexistent/drove/worktrees/run_x/w1".into(),
                    keep: true,
                    ..Default::default()
                },
                0,
            )
            .unwrap();
        let a = reg.agents.get_mut(&id).unwrap();
        assert!(missing_unrecoverable_workspace_path(a).is_some());

        a.cwd = "/tmp".into();
        // Still catches missing --bind source path in bwrap argv!
        assert_eq!(
            missing_unrecoverable_workspace_path(a).as_deref(),
            Some("/nonexistent/drove/worktrees/run_x/w1")
        );

        let id2 = reg
            .create(
                NewAgent { argv: vec!["codex".into()], cwd: "/tmp".into(), keep: true, ..Default::default() },
                0,
            )
            .unwrap();
        let normal = reg.agents.get(&id2).unwrap();
        assert!(missing_unrecoverable_workspace_path(normal).is_none());

        assert!(is_permanent_missing_path_error(
            "bwrap: Can't find source path /home/u/.local/state/herdr2/default/drove/worktrees/run_1/w: No such file or directory"
        ));
    }
}
