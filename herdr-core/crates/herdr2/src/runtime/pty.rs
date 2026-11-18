//! PTY holder links, Ghostty screens, adoption after restarts, and terminal lifecycle.

use super::{Core, Exit, State, TargetKey, finish_reason, now_ms};
use crate::holder_proto::{self as proto, Goodbye};
use crate::paths::write_atomic;
use crate::platform::{self, Stream as UnixStream, signal};
use crate::relay::Relay;
use ghostty_vt::Terminal as Screen;
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// At most one reconnect per holder in this long: a failure that comes back is not retried.
const RELINK_GAP: Duration = Duration::from_secs(5);
/// How long a reconnect waits for the holder's welcome.
const RELINK_WAIT: Duration = Duration::from_millis(if cfg!(test) { 200 } else { 5000 });

pub struct Link {
    /// Frames for the holder, written by their own thread: the core never
    /// blocks on the socket while holding its lock (the holder writes to us
    /// under its own lock; both waiting would freeze the session).
    writer: mpsc::Sender<Vec<u8>>,
    pub pid: u32,
    pub instance: String,
    /// Same build as this core: new terminals go here.
    pub current: bool,
    bye: mpsc::Sender<()>,
    bye_rx: Option<mpsc::Receiver<()>>,
}

impl State {
    /// The holder that runs terminal `id`.
    pub fn link(&self, id: u32) -> Option<&Link> {
        let instance = self.pty_reg.terminals.get(&id)?.holder.as_deref()?;
        self.holders.iter().find(|l| l.instance == instance)
    }

    /// The holder new terminals go to.
    pub fn current(&self) -> Option<&Link> {
        self.holders.iter().find(|l| l.current)
    }
}

impl Link {
    pub fn send(&self, frame: &[u8]) {
        let _ = self.writer.send(frame.to_vec());
    }

    pub fn input(&self, id: u32, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.send(&proto::frame(proto::INPUT, id, bytes));
        }
    }
}

impl Core {
    pub fn headless(&self) -> (u16, u16) {
        self.settings().headless().unwrap_or((200, 120))
    }

    pub fn history_bytes(&self) -> usize {
        self.settings().history_bytes()
    }

    pub(crate) fn snapshot_path(&self, id: u32) -> PathBuf {
        self.paths.state.join("screens").join(format!("{}.snap", crate::terminals::address(id)))
    }

    /// A live terminal's saved screen, read once: a later loss must not bring back an older one.
    pub(crate) fn take_saved_screen(&self, id: u32) -> Option<Screen> {
        let path = self.snapshot_path(id);
        let bytes = std::fs::read(&path).ok();
        let _ = std::fs::remove_file(&path);
        Screen::restore(&bytes?, self.history_bytes())
    }

    pub fn save_pty(&self, st: &State) {
        self.pty_dirty.store(false, std::sync::atomic::Ordering::SeqCst);
        let bytes = serde_json::to_vec(&st.pty_reg).expect("terminal registry serializes");
        self.pty_registry_writer.save(bytes);
    }

    /// For frequent terminal changes a crash may lose: saved within `SAVE_SOON`, once for a burst.
    pub fn save_pty_soon(&self) {
        if !self.pty_dirty.swap(true, std::sync::atomic::Ordering::SeqCst) {
            self.save_due.notify_one();
        }
    }

    /// A holder of our build for new terminals, started if there is none.
    pub async fn ensure_holder(self: &Arc<Self>) -> io::Result<()> {
        let core = self.clone();
        tokio::task::spawn_blocking(move || core.start_holder()).await.map_err(io::Error::other)?
    }

    fn start_holder(self: &Arc<Self>) -> io::Result<()> {
        let _starting = self.holder_start.lock().unwrap_or_else(|p| p.into_inner());
        if self.lock().stopping || self.restart.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(io::Error::other("the core is stopping or restarting"));
        }
        if self.lock().current().is_some() {
            return Ok(());
        }
        // 8 hex: unique enough among one session's holders, and socket paths are short (macOS: 104 bytes).
        let socket =
            format!("{}{}.sock", self.holder_prefix(), &crate::platform::random_hex()[..crate::paths::HOLDER_ID]);
        crate::client::spawn_detached(&self.paths, &["hold", "--socket", &socket], "holder.log")?;
        let stream = crate::client::wait_for_socket(socket.as_ref(), "holder").map_err(io::Error::other)?;
        self.take_over(stream, true, socket.as_ref(), None).map(drop)
    }

    /// Holder sockets are `<session>.holder.<id>.sock`, next to `<session>.holder.sock`.
    fn holder_prefix(&self) -> String {
        let base = self.paths.holder_socket.display().to_string();
        format!("{}.", base.strip_suffix(".sock").unwrap_or(&base))
    }

    /// At startup: adopt from every holder still running. Live terminal records
    /// no holder runs any more ended with it (a reboot, a killed holder).
    pub(crate) fn attach_all(self: &Arc<Self>) -> io::Result<()> {
        let prefix = self.holder_prefix();
        let dir = self.paths.holder_socket.parent().expect("socket has a dir");
        let mut sockets: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let p = p.display().to_string();
                p == self.paths.holder_socket.display().to_string() || (p.starts_with(&prefix) && p.ends_with(".sock"))
            })
            .collect();
        sockets.sort();
        let mut adopted = HashSet::new();
        for socket in sockets {
            match platform::connect(&socket) {
                Ok(stream) => adopted.extend(self.take_over(stream, false, &socket, None)?),
                Err(_) => {
                    platform::remove_endpoint(&socket);
                }
            }
        }
        let mut st = self.lock();
        let lost: Vec<u32> = st
            .pty_reg
            .terminals
            .values()
            .filter(|t| t.is_live() && !(t.program_lost() && t.holder.is_none()) && !adopted.contains(&t.id()))
            .map(|t| t.id())
            .collect();
        for id in lost {
            self.pty_record_finish(&mut st, id, Exit { code: None, signal: None }, "stopped_by_restart");
        }
        let leftovers = st
            .pty_reg
            .terminals
            .values()
            .filter(|t| matches!(t.reason.as_deref(), Some("holder_lost" | "stopped_by_restart")));
        self.sweep_in_background(leftovers.filter_map(|t| t.scope.clone()).collect());
        self.prune_pty(&mut st);
        self.save_pty(&st);
        Ok(())
    }

    /// Takes over one holder and adopts what it runs; returns the adopted ids.
    /// `answered` is dropped once the holder's welcome and tails are in.
    fn take_over(
        self: &Arc<Self>,
        mut stream: UnixStream,
        require_current: bool,
        path: &std::path::Path,
        answered: Option<mpsc::Sender<()>>,
    ) -> io::Result<Vec<u32>> {
        std::io::Write::write_all(
            &mut stream,
            &proto::json_frame(proto::HELLO, 0, &proto::Hello { version: proto::VERSION }),
        )?;
        let welcome: proto::Welcome = match proto::read(&mut stream)? {
            Some((proto::WELCOME, _, payload)) => proto::parse(&payload)?,
            _ => return Err(io::Error::other("holder sent no welcome")),
        };
        let mut tails = HashMap::new();
        for _ in &welcome.terminals {
            match proto::read(&mut stream)? {
                Some((proto::TAIL, id, bytes)) => tails.insert(id, bytes),
                _ => return Err(io::Error::other("holder sent no tail")),
            };
        }
        drop(answered);
        let (bye, bye_rx) = mpsc::channel();
        let build = crate::client::build_id();
        let disk_build = (welcome.build != build && require_current)
            .then(crate::client::disk_build_id)
            .flatten();
        let effective_build = disk_build.as_deref().filter(|d| *d == welcome.build).unwrap_or(build.as_str());
        let current = platform::holder_current(
            &welcome.build,
            effective_build,
            welcome.draining,
            self.login_session,
            welcome.login_session,
        );
        if welcome.build == build && self.login_session.is_some() && welcome.login_session.is_none() {
            let _ = writeln!(
                io::stderr(),
                "core: holder {} login identity unavailable; retaining build eligibility",
                welcome.pid
            );
        }
        if require_current && !current {
            io::Write::write_all(&mut stream, &proto::frame(proto::DRAIN, 0, &[]))?;
            return Err(io::Error::other(format!(
                "holder build {} / login {:?} is not current for core build {build} / login {:?} (draining={})",
                welcome.build, welcome.login_session, self.login_session, welcome.draining
            )));
        }
        let (writer, frames) = mpsc::channel::<Vec<u8>>();
        let mut socket = stream.try_clone()?;
        std::thread::spawn(move || {
            for frame in frames {
                if io::Write::write_all(&mut socket, &frame).is_err() {
                    break;
                }
            }
        });
        let link =
            Link { writer, pid: welcome.pid, instance: welcome.instance.clone(), current, bye, bye_rx: Some(bye_rx) };
        if !current {
            link.send(&proto::frame(proto::DRAIN, 0, &[]));
        }
        let (adopted, nudge) = {
            let mut st = self.lock();
            st.disconnected.remove(&welcome.instance);
            st.holders.push(link);
            let adopted = self.adopt(&mut st, &welcome, tails);
            self.save_pty(&st);
            adopted
        };
        let core = self.clone();
        let (instance, path) = (welcome.instance, path.to_path_buf());
        std::thread::spawn(move || core.read_holder(stream, instance, path));
        if !nudge.is_empty() {
            let core = self.clone();
            std::thread::spawn(move || core.nudge(nudge));
        }
        Ok(adopted)
    }

    /// Matches saved terminal records with what one holder runs.
    fn adopt(
        &self,
        st: &mut State,
        welcome: &proto::Welcome,
        mut tails: HashMap<u32, Vec<u8>>,
    ) -> (Vec<u32>, Vec<(u32, u16, u16)>) {
        let instance = welcome.instance.as_str();
        let handover = welcome.previous == "handover";
        let legacy = st.pty_reg.holder.clone();
        let ours =
            |t: &crate::terminals::Terminal| t.is_live() && t.holder.as_deref().or(legacy.as_deref()) == Some(instance);
        let mut nudge = Vec::new();
        let mut exited = Vec::new();
        let mut adopted = Vec::new();
        for h in &welcome.terminals {
            let id = h.id;
            if !st.pty_reg.terminals.get(&id).is_some_and(ours) {
                let link = st.holders.iter().find(|l| l.instance == instance).expect("attached");
                link.send(&proto::json_frame(proto::SIGNAL, id, &proto::Signal { signal: signal::KILL }));
                link.send(&proto::frame(proto::RELEASE, id, &[]));
                continue;
            }
            adopted.push(id);
            if let Some(saved) = st.pty_reg.drafts.remove(&id)
                && handover
            {
                st.gates.insert(id, crate::pty_input::Gate::restore(saved));
            }
            st.pty_reg.terminals.get_mut(&id).expect("ours").holder = Some(instance.to_string());
            let replies = st.pty_reg.replies.remove(&id).unwrap_or_default();
            if handover {
                let mut held = st.gates.entry(id).or_default().set_busy(false);
                held.extend(replies);
                if let Some(link) = st.link(id) {
                    link.input(id, &held);
                }
            }
            let tail = tails.remove(&id).unwrap_or_default();
            let saved = if handover {
                self.take_saved_screen(id)
            } else {
                let _ = std::fs::remove_file(self.snapshot_path(id));
                None
            };
            let exact = saved.is_some();
            let mut screen = saved.unwrap_or_else(|| Screen::new(h.cols, h.rows, self.history_bytes()));
            let mut relay = Relay::default();
            relay.feed(&mut screen, &tail);
            relay.flush(&mut screen);
            let replies = Relay::take_responses(&mut screen);
            if exact && let Some(link) = st.link(id) {
                link.input(id, &replies);
            }
            let reports = screen.take_status_reports();
            let rec = st.pty_reg.terminals.get_mut(&id).expect("live id");
            screen.set_xtversion(&rec.identity);
            rec.title = screen.title();
            if let Some(on) = relay.win32_input {
                rec.win32_input = on;
            }
            st.screens.insert(id, screen);
            st.relays.remove(&id);
            rec.pid = Some(h.pid);
            rec.cols = h.cols;
            rec.rows = h.rows;
            rec.screen = if exact { "exact" } else { "inexact" }.into();
            let spawning = rec.reason.is_some() || st.pending.contains_key(&id);
            if exact {
                for r in &reports {
                    let rec = st.pty_reg.terminals.get_mut(&id).expect("live id");
                    let change = crate::status::apply_terminal(rec, r);
                    self.pty_status_changed(st, id, change);
                }
            } else {
                let (turn, was, since, working_since, work_ms, total_work_ms) = (
                    rec.turn,
                    rec.status.as_ref().map(|s| s.state.clone()),
                    rec.status_ms,
                    rec.working_since,
                    rec.work_ms,
                    rec.total_work_ms,
                );
                let facts = |t: &crate::terminals::Terminal| (t.status.clone(), t.records.clone(), t.reported);
                let heard = facts(rec);
                for r in &reports {
                    crate::status::apply_terminal(rec, r);
                }
                let now = rec.status.as_ref().map(|s| s.state.clone());
                rec.turn = turn + (was.as_deref() == Some("working") && now.as_deref() != Some("working")) as u64;
                if now == was {
                    rec.status_ms = since;
                    rec.working_since = working_since;
                    rec.work_ms = work_ms;
                    rec.total_work_ms = total_work_ms;
                }
                let root_report = rec.turn != turn || now != was;
                let status = facts(rec) != heard;
                self.pty_status_changed(st, id, crate::status::Change { status, root_report, ..Default::default() });
            }
            if spawning {
                self.spawn_result(st, instance, id, Ok(h.pid));
            }
            match h.exit {
                Some(exit) => exited.push((id, exit)),
                None if !exact => nudge.push((id, h.cols, h.rows)),
                None => {}
            }
        }
        for (id, exit) in exited {
            self.pty_finish(st, id, Exit { code: exit.code, signal: exit.signal });
        }
        (adopted, nudge)
    }

    /// Resize down and back so full-screen programs repaint.
    fn nudge(&self, terminals: Vec<(u32, u16, u16)>) {
        let send = |id, cols, rows| {
            if let Some(link) = self.lock().link(id) {
                link.send(&proto::json_frame(proto::RESIZE, id, &proto::Size { cols, rows }));
            }
        };
        for &(id, cols, rows) in &terminals {
            send(id, cols, rows.saturating_sub(1).max(1));
        }
        std::thread::sleep(Duration::from_millis(50));
        for &(id, cols, rows) in &terminals {
            send(id, cols, rows);
        }
    }

    fn read_holder(self: Arc<Self>, mut stream: UnixStream, instance: String, socket: PathBuf) {
        let ended =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.holder_frames(&mut stream, &instance)));
        if matches!(ended, Ok(Ok(true))) {
            return;
        }
        let _ = stream.shutdown(std::net::Shutdown::Both);
        let stopping = || self.lock().stopping || self.restart.load(std::sync::atomic::Ordering::SeqCst);
        let mut relink = !matches!(ended, Ok(Ok(false))) && !stopping();
        let _starting = relink.then(|| self.holder_start.lock().unwrap_or_else(|p| p.into_inner()));
        relink &= !stopping();
        let mut st = self.lock();
        let Some(at) = st.holders.iter().position(|l| l.instance == instance) else { return };
        let link = st.holders.remove(at);
        let current = link.current;
        if relink {
            drop(st);
            drop(link);
            drop(stream);
            if let Ok(stream) = platform::connect(&socket) {
                return self.relink(stream, &instance, &socket);
            }
            st = self.lock();
        }
        let theirs: Vec<u32> = st
            .pty_reg
            .terminals
            .values()
            .filter(|t| t.is_live() && t.holder.as_deref() == Some(instance.as_str()))
            .map(|t| t.id())
            .collect();
        self.sweep_in_background(theirs.iter().filter_map(|id| st.pty_reg.terminals[id].scope.clone()).collect());
        for id in theirs {
            self.pty_record_finish(&mut st, id, Exit { code: None, signal: None }, "holder_lost");
        }
        if current {
            for (_, tx) in st.pending.drain() {
                let _ = tx.send(Err("the holder exited".into()));
            }
        }
        self.save_pty(&st);
        self.changed.notify_waiters();
    }

    fn relink(self: &Arc<Self>, stream: UnixStream, instance: &str, socket: &std::path::Path) {
        let last = self.lock().relinked.insert(instance.into(), Instant::now());
        let ours = stream.try_clone();
        let (answered, waiting) = mpsc::channel::<()>();
        if let Ok(watch) = stream.try_clone() {
            std::thread::spawn(move || {
                if waiting.recv_timeout(RELINK_WAIT) == Err(mpsc::RecvTimeoutError::Timeout) {
                    let _ = watch.shutdown(std::net::Shutdown::Both);
                }
            });
        }
        let taken = last.is_none_or(|at| at.elapsed() >= RELINK_GAP)
            && matches!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.take_over(
                    stream,
                    false,
                    socket,
                    Some(answered)
                ))),
                Ok(Ok(_))
            );
        if taken {
            eprintln!("core: lost the link to holder {instance}, reconnected");
            return;
        }
        let _ = writeln!(
            io::stderr(),
            "core: lost the link to holder {instance} for good; its terminals wait for the next core start"
        );
        if let Ok(ours) = ours {
            let _ = ours.shutdown(std::net::Shutdown::Both);
        }
        let mut st = self.lock();
        st.holders.retain(|l| l.instance != instance);
        st.disconnected.insert(instance.into(), socket.to_path_buf());
    }

    /// Handles the holder's frames until it says goodbye (true) or the link ends (false).
    fn holder_frames(self: &Arc<Self>, stream: &mut UnixStream, instance: &str) -> io::Result<bool> {
        while let Some((kind, id, payload)) = proto::read(stream)? {
            let mut st = self.lock();
            match kind {
                proto::OUTPUT => self.output(&mut st, id, &payload),
                proto::SPAWNED | proto::SPAWN_FAILED => {
                    let result = if kind == proto::SPAWNED {
                        proto::parse::<proto::Spawned>(&payload).map(|s| s.pid).map_err(|e| e.to_string())
                    } else {
                        Err(proto::parse::<proto::SpawnFailed>(&payload).map_or_else(|e| e.to_string(), |f| f.error))
                    };
                    self.spawn_result(&mut st, instance, id, result);
                }
                proto::EXITED => {
                    if let Ok(exit) = proto::parse::<proto::Exit>(&payload) {
                        self.pty_finish(&mut st, id, Exit { code: exit.code, signal: exit.signal });
                    }
                }
                proto::FOREGROUND_IS => {
                    let pgid = proto::parse::<proto::Foreground>(&payload).map_or(-1, |f| f.pgid);
                    for tx in st.foreground.remove(&id).unwrap_or_default() {
                        let _ = tx.send(pgid);
                    }
                }
                proto::BYE => {
                    if let Some(at) = st.holders.iter().position(|l| l.instance == instance) {
                        let _ = st.holders.remove(at).bye.send(());
                    }
                    return Ok(true);
                }
                _ => {}
            }
        }
        Ok(false)
    }

    fn spawn_result(&self, st: &mut State, instance: &str, id: u32, result: Result<u32, String>) {
        let waiting = st.pending.get(&id).is_some_and(|tx| !tx.is_closed());
        let result = match (result, st.pty_reg.terminals.get_mut(&id)) {
            (Ok(pid), Some(t)) if t.is_live() => {
                let restarted = t.reason.is_some();
                t.pid = Some(pid);
                t.program_started_ms = Some(now_ms());
                t.reason = None;
                t.exit_code = None;
                t.signal = None;
                let created = t.clone();
                let public = serde_json::to_value(crate::terminals::Public(created.clone())).unwrap_or_default();
                self.emit_pty(st, if restarted { "restarted" } else { "created" }, id, public);
                self.save_pty(st);
                Ok(created)
            }
            (Ok(_), _) => {
                if let Some(link) = st.holders.iter().find(|l| l.instance == instance) {
                    link.send(&proto::json_frame(proto::SIGNAL, id, &proto::Signal { signal: signal::KILL }));
                    link.send(&proto::frame(proto::RELEASE, id, &[]));
                }
                Err("removed while starting".into())
            }
            (Err(e), Some(t)) if t.is_live() && (t.reason.is_some() || !waiting) => {
                self.pty_spawn_failed(st, id);
                Err(e)
            }
            (Err(e), _) => Err(e),
        };
        if let Some(tx) = st.pending.remove(&id) {
            let _ = tx.send(result);
        }
    }

    /// A terminal program ended: record it, keep its final screen on disk, let the holder forget it.
    pub fn pty_finish(&self, st: &mut State, id: u32, exit: Exit) {
        let reason = finish_reason(st.pty_killing.contains(&id), st.stopping, false, platform::host_shutdown_in_progress());
        if let Some(link) = st.link(id) {
            link.send(&proto::frame(proto::RELEASE, id, &[]));
        }
        self.pty_record_finish(st, id, exit, reason);
        self.save_pty(st);
        self.changed.notify_waiters();
    }

    pub(crate) fn pty_record_finish(&self, st: &mut State, id: u32, exit: Exit, reason: &str) {
        if !st.pty_reg.terminals.get(&id).is_some_and(|t| t.is_live()) {
            return;
        }
        let reason = if st.pty_killing.remove(&id) {
            "killed"
        } else if reason == "exited" && st.pty_reg.terminals[&id].keep && exit.code.is_none() && exit.signal.is_some() {
            "signaled"
        } else {
            reason
        };
        if let Some(t) = st.pty_reg.terminals.get_mut(&id) {
            t.first_task_turn = None;
            let change = crate::status::reset_terminal(t);
            self.pty_status_changed(st, id, change);
        }
        st.pty_reg.finish(id, exit.code, exit.signal, reason, now_ms());
        let finished_ms = st.pty_reg.terminals.get(&id).and_then(|t| t.finished_ms);
        let end = serde_json::json!({"reason": reason, "exit_code": exit.code, "signal": exit.signal, "finished_ms": finished_ms});
        let pending = st.pty_reg.terminals[&id].is_live();
        self.emit_pty(st, if pending { "lost" } else { "finished" }, id, end.clone());
        if pending {
            let t = st.pty_reg.terminals.get_mut(&id).expect("lost");
            t.news += 1;
            t.told_parent = false;
        } else {
            self.pty_news(st, id);
            let target = TargetKey::Terminal(id);
            let _ = self.jobs.send(crate::notes::Job::Orphaned(target));
            crate::remote::orphaned_afar(self, st, target);
            let _ = self.drove.pool_tx.send(crate::drove::pool::PoolSignal::SessionClosed {
                address: crate::terminals::address(id),
            });
        }
        if let (Some(relay), Some(screen)) = (st.relays.get_mut(&id), st.screens.get_mut(&id)) {
            relay.flush(screen);
        }
        self.queue_resize_paints(st, id, None);
        for v in st.viewers.remove(&id).unwrap_or_default() {
            v.conn.with(|out| out.finish(v.channel, end.to_string().as_bytes()));
        }
        st.relays.remove(&id);
        if let Some(snapshot) = st.screens.remove(&id).and_then(|s| s.snapshot())
            && let Err(e) = write_atomic(&self.snapshot_path(id), &snapshot)
        {
            let _ = writeln!(io::stderr(), "core: cannot save screen of {}: {e}", crate::terminals::address(id));
        }
        let rm = st.pty_reg.terminals.get(&id).is_some_and(|t| {
            !t.is_live() && t.rm && !t.parent.as_ref().is_some_and(|p| st.waiting.contains_key(&(p.clone(), TargetKey::Terminal(id))))
        });
        if rm {
            self.pty_remove(st, id);
        }
        self.prune_pty(st);
    }

    pub fn prune_pty(&self, st: &mut State) {
        for gone in st.pty_reg.expired(now_ms()) {
            self.pty_remove(st, gone);
        }
    }

    /// The screen text of a live or finished terminal.
    pub fn screen_text(&self, st: &State, id: u32) -> Option<String> {
        match st.screens.get(&id) {
            Some(screen) => Some(screen.plain_text()),
            None => std::fs::read(self.snapshot_path(id))
                .ok()
                .and_then(|bytes| Screen::restore(&bytes, self.history_bytes()))
                .map(|s| s.plain_text()),
        }
    }

    pub fn pty_remove(&self, st: &mut State, id: u32) {
        let Some(was) = st.pty_reg.terminals.remove(&id) else { return };
        st.pty_prompts.remove(&id);
        self.emit_pty_from(st, "removed", id, serde_json::json!({}), Some(&was));
        let _ = std::fs::remove_file(self.snapshot_path(id));
        let _ = std::fs::remove_dir_all(crate::put::terminal_dir(&self.paths.state, id));
        self.save_pty(st);
    }

    pub fn pty_signal(&self, st: &State, id: u32, sig: i32) {
        if let Some(link) = st.link(id) {
            link.send(&proto::json_frame(proto::SIGNAL, id, &proto::Signal { signal: sig }));
        }
    }

    /// Hang up, then escalate. Returns once the program ended or KILL timed out.
    pub async fn pty_kill(&self, ids: &[u32]) {
        if ids.is_empty() {
            return;
        }
        let all_done = |st: &State| {
            ids.iter().all(|id| !st.pty_reg.terminals.get(id).is_some_and(|t| t.is_live() && t.holder.is_some()))
        };
        {
            let mut st = self.lock();
            for &id in ids {
                if st
                    .pty_reg
                    .terminals
                    .get(&id)
                    .is_some_and(|t| t.is_live() && (t.holder.is_none() || (t.reason.is_some() && t.pid.is_none())))
                {
                    let reason = if st.pty_killing.contains(&id) { "killed" } else { "server_stop" };
                    if let Some(link) = st.link(id) {
                        link.send(&proto::json_frame(proto::SIGNAL, id, &proto::Signal { signal: signal::KILL }));
                        link.send(&proto::frame(proto::RELEASE, id, &[]));
                    }
                    self.pty_record_finish(&mut st, id, Exit { code: None, signal: None }, reason);
                }
            }
            self.save_pty(&st);
        }
        for (signal, wait) in [(signal::HUP, 500), (signal::TERM, 500), (signal::KILL, 2000)] {
            {
                let st = self.lock();
                for &id in ids {
                    self.pty_signal(&st, id, signal);
                }
            }
            if self.wait_for(Duration::from_millis(wait), all_done).await {
                return;
            }
        }
    }

    /// Ends every holder link on purpose and waits until each holder confirms.
    pub(crate) fn goodbye(&self, mode: Goodbye) {
        let _starting = self.holder_start.lock().unwrap_or_else(|p| p.into_inner());
        let byes: Vec<mpsc::Receiver<()>> = {
            let mut st = self.lock();
            st.holders
                .iter_mut()
                .filter_map(|link| {
                    link.send(&proto::json_frame(proto::GOODBYE, 0, &mode));
                    link.bye_rx.take()
                })
                .collect()
        };
        if mode == Goodbye::Shutdown {
            let sockets: Vec<_> = self.lock().disconnected.drain().map(|(_, path)| path).collect();
            for path in sockets {
                if let Ok(mut stream) = platform::connect(&path) {
                    let Ok(watch) = stream.try_clone() else { continue };
                    let (done, waiting) = mpsc::channel::<()>();
                    std::thread::spawn(move || {
                        if waiting.recv_timeout(Duration::from_secs(6)) == Err(mpsc::RecvTimeoutError::Timeout) {
                            let _ = watch.shutdown(std::net::Shutdown::Both);
                        }
                    });
                    let hello = proto::json_frame(proto::HELLO, 0, &proto::Hello { version: proto::VERSION });
                    if io::Write::write_all(&mut stream, &hello).is_ok()
                        && io::Write::write_all(&mut stream, &proto::json_frame(proto::GOODBYE, 0, &mode)).is_ok()
                    {
                        let _ = io::copy(&mut stream, &mut io::sink());
                    }
                    drop(done);
                }
            }
        }
        for rx in byes {
            let _ = rx.recv_timeout(Duration::from_secs(6));
        }
    }

    pub(crate) fn save_pty_handover_state(&self, st: &mut State) {
        st.holders.clear();
        st.pty_reg.replies = st
            .relays
            .iter_mut()
            .filter_map(|(id, relay)| {
                let replies = relay.end_wait();
                (!replies.is_empty()).then_some((*id, replies))
            })
            .collect();
        st.pty_reg.drafts = st
            .gates
            .iter()
            .filter(|(id, _)| st.pty_reg.terminals.get(*id).is_some_and(|t| t.is_live()))
            .filter_map(|(id, gate)| Some((*id, gate.snapshot()?)))
            .collect();
        let State { screens, relays, .. } = &mut *st;
        for (id, screen) in screens.iter_mut() {
            if let Some(relay) = relays.get_mut(id) {
                relay.flush(screen);
            }
            if let Some(snapshot) = screen.snapshot() {
                let _ = write_atomic(&self.snapshot_path(*id), &snapshot);
            }
        }
        self.save_pty(st);
    }
}
