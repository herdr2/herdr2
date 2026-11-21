//! What runs in a terminal right now: its foreground process and that
//! process's folder. Raw facts, read about twice a second; clients name and
//! draw programs themselves.

use crate::runtime::Core;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct Foreground {
    /// The foreground process group's leader.
    pub pid: i32,
    /// Its name as the OS has it (`node`, `vim`, `sh`).
    pub name: String,
    pub argv: Vec<String>,
    /// Its working directory now (resolved by the OS).
    pub cwd: String,
}

/// The facts about `pid`, if it is still there to read.
#[cfg(target_os = "linux")]
pub fn about(pid: i32) -> Option<Foreground> {
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?.trim_end().to_string();
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let argv =
        cmdline.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?.display().to_string();
    Some(Foreground { pid, name, argv, cwd })
}

/// A pseudo console has no foreground group: this is the program the
/// platform put in front, with the folder and command line Windows keeps in
/// its memory. None for a 32-bit process, whose own copy of them lives elsewhere.
#[cfg(windows)]
pub fn about(pid: i32) -> Option<Foreground> {
    use std::ffi::c_void;
    use std::mem::{MaybeUninit, size_of};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
    use windows_sys::Win32::Foundation::{HANDLE, UNICODE_STRING};
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Threading::*;

    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid as u32) };
    if raw.is_null() {
        return None;
    }
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    let h = process.as_raw_handle() as HANDLE;
    let mut wow64 = 0;
    if unsafe { IsWow64Process(h, &mut wow64) } == 0 || wow64 != 0 {
        return None;
    }
    let read = |at: *const c_void, into: *mut c_void, len: usize| {
        let mut n = 0;
        unsafe { ReadProcessMemory(h, at, into, len, &mut n) != 0 && n == len }
    };
    fn value<T>(read: impl Fn(*const c_void, *mut c_void, usize) -> bool, at: *const c_void) -> Option<T> {
        let mut v = MaybeUninit::<T>::uninit();
        read(at, v.as_mut_ptr().cast(), size_of::<T>()).then(|| unsafe { v.assume_init() })
    }
    let text = |s: UNICODE_STRING| -> Option<Vec<u16>> {
        let mut buf = vec![0u16; s.Length as usize / 2];
        read(s.Buffer.cast(), buf.as_mut_ptr().cast(), buf.len() * 2).then_some(buf)
    };

    let mut basic: PROCESS_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let size = size_of::<PROCESS_BASIC_INFORMATION>() as u32;
    let status =
        unsafe { NtQueryInformationProcess(h, ProcessBasicInformation, (&raw mut basic).cast(), size, &mut 0) };
    if status != 0 || basic.PebBaseAddress.is_null() {
        return None;
    }
    let peb: PEB = value(read, basic.PebBaseAddress.cast())?;
    let params: RTL_USER_PROCESS_PARAMETERS = value(read, peb.ProcessParameters.cast())?;
    // CurrentDirectory.DosPath: right after the standard handles, in what the SDK calls Reserved2.
    let dir: UNICODE_STRING = value(read, peb.ProcessParameters.cast::<u8>().wrapping_add(0x38).cast())?;
    let mut cwd = String::from_utf16(&text(dir)?).ok()?;
    if cwd.len() > 3 && cwd.ends_with('\\') {
        cwd.pop(); // Windows keeps C:\dir\; only a drive's root has one
    }
    let image = String::from_utf16(&text(params.ImagePathName)?).ok()?;
    let name = image.rsplit('\\').next().unwrap_or(&image).to_string();

    let argv = crate::platform::split_command_line(text(params.CommandLine)?)
        .ok()?
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    Some(Foreground { pid, name, argv, cwd })
}

#[cfg(target_os = "macos")]
pub fn about(pid: i32) -> Option<Foreground> {
    let mut name = [0u8; 256];
    let n = unsafe { libc::proc_name(pid, name.as_mut_ptr().cast(), name.len() as u32) };
    let name = String::from_utf8_lossy(&name[..n.max(0) as usize]).into_owned();
    let mut vnode: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    if unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut vnode).cast(), size) } != size {
        return None;
    }
    let path = unsafe { std::ffi::CStr::from_ptr(vnode.pvi_cdir.vip_path.as_ptr().cast()) };
    let (argv, _) = procargs(pid)?;
    Some(Foreground { pid, name, argv, cwd: path.to_string_lossy().into_owned() })
}

/// KERN_PROCARGS2: argc, the executable path, the arguments, then the
/// environment, each NUL-terminated. Returns (arguments, environment).
#[cfg(target_os = "macos")]
pub fn procargs(pid: i32) -> Option<(Vec<String>, Vec<u8>)> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    let call = |buf: *mut libc::c_void, size: &mut libc::size_t, mib: &mut [i32; 3]| unsafe {
        libc::sysctl(mib.as_mut_ptr(), 3, buf, size, std::ptr::null_mut(), 0)
    };
    if call(std::ptr::null_mut(), &mut size, &mut mib) != 0 || size < 4 {
        return None;
    }
    let mut buf = vec![0u8; size];
    if call(buf.as_mut_ptr().cast(), &mut size, &mut mib) != 0 {
        return None;
    }
    buf.truncate(size);
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?) as usize;
    let mut rest = &buf[4..];
    let one = |r: &[u8]| -> Option<usize> { r.iter().position(|b| *b == 0) };
    rest = &rest[one(rest)? + 1..]; // the executable path
    while rest.first() == Some(&0) {
        rest = &rest[1..]; // its padding
    }
    let mut argv = Vec::with_capacity(argc);
    for _ in 0..argc {
        let end = one(rest)?;
        argv.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        rest = &rest[end + 1..];
    }
    Some((argv, rest.to_vec()))
}

pub(crate) fn is_shell_root(root: &Foreground) -> bool {
    // Command and script wrappers can share their foreground group with a live agent.
    if root.argv.iter().skip(1).any(|arg| {
        (!arg.starts_with('-') && !arg.starts_with('+'))
            || matches!(arg.split('=').next(), Some("--command" | "--commands"))
            || (arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains('c'))
    }) {
        return false;
    }
    let program =
        if root.name.is_empty() { root.argv.first().map(String::as_str).unwrap_or_default() } else { &root.name };
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program).to_ascii_lowercase();
    let name = base.strip_suffix(".exe").unwrap_or(&base);
    matches!(
        name,
        "sh" | "bash" | "zsh" | "fish" | "nu" | "dash" | "ksh" | "tcsh" | "pwsh" | "powershell" | "cmd"
    )
}

fn is_cli_foreground(fg: &Foreground) -> bool {
    if crate::pty_transcript::hint_from_words(
        std::iter::once(fg.name.as_str()).chain(fg.argv.iter().map(String::as_str)),
    )
    .is_some()
    {
        return true;
    }
    if crate::pty_adapters::PtyCliKind::detect(&fg.argv) != crate::pty_adapters::PtyCliKind::Generic {
        return true;
    }
    std::iter::once(fg.name.as_str()).chain(fg.argv.iter().map(String::as_str)).any(|raw| {
        if raw.starts_with('-') {
            return false;
        }
        let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw).to_ascii_lowercase();
        let stem = base.strip_suffix(".exe").unwrap_or(&base);
        let clean = stem.strip_suffix(".real").unwrap_or(stem);
        matches!(clean, "cursor-agent" | "opencode" | "kimi" | "copilot" | "hermes" | "zcode" | "qoder" | "dsh")
    })
}

impl Core {
    /// Applies one foreground sample for terminal `id`, resetting agent status and stale CLI checkpoints
    /// when a CLI exits back to its shell or a fresh non-resumed CLI starts in the same terminal.
    pub(crate) fn apply_foreground_update(&self, st: &mut crate::runtime::State, id: u32, now: Option<Foreground>) {
        let Some((t_pid, t_addr, t_cwd, prev_fg, has_status_or_records)) = st
            .pty_reg
            .terminals
            .get(&id)
            .filter(|t| t.is_live())
            .map(|t| {
                let cwd = t.last_cwd.clone().unwrap_or_else(|| t.cwd.clone());
                (t.pid, t.address.clone(), cwd, t.foreground.clone(), t.status.is_some() || !t.records.is_empty())
            })
        else {
            return;
        };

        let now_is_shell_root = now.as_ref().is_some_and(|n| Some(n.pid as u32) == t_pid && is_shell_root(n));
        let prev_was_shell_root = prev_fg.as_ref().is_some_and(|p| Some(p.pid as u32) == t_pid && is_shell_root(p));
        let at_shell = crate::platform::foreground_tracks_jobs() && now_is_shell_root;
        let respawning = st.pty_respawning.contains(&id);

        let mut cleared_checkpoints = false;
        let mut should_reattach_resumed = false;
        if !respawning {
            let tc = self.target_checkpoints(st, &t_addr);
            let has_ckpt_state = !tc.checkpoints.is_empty()
                || tc.cli_kind.is_some()
                || tc.cli_session_id.is_some()
                || tc.cli_transcript_path.is_some()
                || tc.cli_fg_pid.is_some()
                || st.pty_prompts.contains_key(&id);
            if has_ckpt_state && tc.pending_rollback_turn.is_none() && tc.pending_handoff.is_none() {
                let has_attached_cli_meta = tc
                    .cli_kind
                    .as_deref()
                    .is_some_and(|k| !k.is_empty() && k != "generic")
                    || tc.cli_session_id.is_some();
                let had_cli_session = has_attached_cli_meta
                    || tc.cli_fg_pid.is_some()
                    || has_status_or_records
                    || prev_fg.as_ref().is_some_and(is_cli_foreground);
                let exited_cli_to_shell = now_is_shell_root
                    && (has_attached_cli_meta
                        || (had_cli_session && (!prev_was_shell_root || has_status_or_records)));
                let started_clean_cli = !now_is_shell_root
                    && now.as_ref().is_some_and(|fg| {
                        let fg_pid = u32::try_from(fg.pid).ok();
                        let pid_changed = prev_fg.as_ref().is_none_or(|p| p.pid != fg.pid);
                        pid_changed
                            && is_cli_foreground(fg)
                            && !crate::pty_transcript::argv_has_resume(&fg.argv)
                            && tc.cli_fg_pid != fg_pid
                            && had_cli_session
                    });
                if exited_cli_to_shell || started_clean_cli {
                    self.detach_target_checkpoints(st, &t_addr, &t_cwd);
                    st.pty_prompts.remove(&id);
                    cleared_checkpoints = true;
                }
            }
            if !now_is_shell_root
                && let Some(ref fg) = now
                && is_cli_foreground(fg)
                && crate::pty_transcript::argv_has_resume(&fg.argv)
            {
                let fg_pid = u32::try_from(fg.pid).ok();
                let pid_changed = prev_fg.as_ref().is_none_or(|p| p.pid != fg.pid);
                if pid_changed || (fg_pid.is_some() && tc.cli_fg_pid != fg_pid && crate::pty_adapters::needs_resumed_reattach(&tc, fg_pid)) {
                    should_reattach_resumed = true;
                }
            }
        }

        let Some(t) = st.pty_reg.terminals.get_mut(&id).filter(|t| t.is_live()) else {
            return;
        };
        let mut change = (at_shell && (t.status.is_some() || !t.records.is_empty())).then(|| {
            t.first_task_turn = None;
            crate::status::reset_terminal(t)
        });
        let mut turn_reset = false;
        if cleared_checkpoints {
            if let Some(ref mut ch) = change {
                // Exiting a working CLI back to shell must not trigger a post-exit shell checkpoint.
                ch.turn_ended = false;
            }
            if t.turn != 0 {
                t.turn = 0;
                turn_reset = true;
            }
        }
        let foreground_changed = t.foreground != now;
        let mut argv_updated = false;
        if let Some(f) = &now {
            t.last_cwd = Some(f.cwd.clone());
            // Once a rollback/launch shell wrapper (`zsh -lic '"$@"; exec "$0" -li' ... claude --resume ...`)
            // exits back to the interactive shell (`zsh -li`), replace stale wrapper `t.argv` with the live shell
            // argv so old `--resume` flags cannot linger after exit.
            if now_is_shell_root
                && !f.argv.is_empty()
                && crate::pty_adapters::PtyCliKind::detect(&t.argv) != crate::pty_adapters::PtyCliKind::Generic
            {
                t.argv = f.argv.clone();
                argv_updated = true;
            }
        }
        t.foreground = now.clone();
        if change.is_some() || foreground_changed || turn_reset || argv_updated {
            self.save_pty_soon();
        }
        if let Some(change) = change {
            self.pty_status_changed(st, id, change);
        }
        if turn_reset {
            self.emit_pty(st, "turn", id, serde_json::json!({ "turn": 0 }));
        }
        if foreground_changed {
            self.emit_pty(st, "foreground", id, serde_json::to_value(&now).unwrap_or_default());
        }
        if cleared_checkpoints {
            self.emit_pty(st, "checkpoint", id, serde_json::json!({ "cleared": true }));
        }
        if should_reattach_resumed {
            let _ = self.jobs.send(crate::notes::Job::PtyResumeCheckpoints { id });
        }
    }

    /// Keeps every live terminal's `foreground` current, with an event on each change.
    pub async fn watch_foreground(self: Arc<Self>) {
        let ms = std::env::var("H2_FOREGROUND_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
        loop {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            let live: Vec<u32> = self.lock().pty_reg.terminals.values().filter(|t| t.is_live()).map(|t| t.id()).collect();
            for id in live {
                let pgid = self.foreground(id).await;
                let now = (pgid > 0).then(|| about(pgid)).flatten();
                let mut st = self.lock();
                self.apply_foreground_update(&mut st, id, now);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_flags_keep_shell_roots_from_clearing_live_status() {
        for shell in ["sh", "bash", "zsh", "fish", "nu", "dash", "ksh", "tcsh"] {
            let mut root = Foreground { pid: 1, name: shell.into(), argv: vec![shell.into()], cwd: "/".into() };
            assert!(is_shell_root(&root), "{shell}");
            for arg in ["-i", "-li", "--login", "--no-config"] {
                root.argv = vec![shell.into(), arg.into()];
                assert!(is_shell_root(&root), "{shell} {arg}");
            }
            for arg in ["-c", "-cecho", "-ic echo", "--command", "--command=echo", "--commands", "--commands=echo"] {
                root.argv = vec![shell.into(), arg.into()];
                assert!(!is_shell_root(&root), "{shell} {arg}");
            }
        }
    }

    #[test]
    fn exiting_or_switching_cli_agent_clears_stale_checkpoints_but_preserves_plain_shell_snaps() {
        use crate::checkpoint::{CheckpointMeta, TargetCheckpoints};
        use crate::paths::Paths;
        use crate::terminals::NewTerminal;

        let root = crate::test_tmp().join(format!("h2-fg-ckpt-{}-{}", std::process::id(), crate::platform::random_hex()));
        let paths = Paths {
            socket: root.join("core.sock"),
            holder_socket: root.join("holder.sock"),
            state: root.clone(),
            config: root.join("config.toml"),
        };
        paths.create_dirs().unwrap();
        let core = Core::start(paths, crate::api::CoreInfo { id: "here".into(), name: "here".into() }).unwrap();

        let id = {
            let mut st = core.lock();
            let id = st
                .pty_reg
                .create(
                    NewTerminal {
                        argv: vec!["zsh".into()],
                        cwd: root.to_string_lossy().to_string(),
                        cols: 80,
                        rows: 24,
                        ..Default::default()
                    },
                    1,
                )
                .unwrap();
            st.pty_reg.terminals.get_mut(&id).unwrap().pid = Some(100);
            id
        };
        let addr = crate::terminals::address(id);
        let cwd = root.to_string_lossy().to_string();
        let zsh = Foreground { pid: 100, name: "zsh".into(), argv: vec!["zsh".into()], cwd: cwd.clone() };
        let claude = Foreground { pid: 200, name: "claude".into(), argv: vec!["claude".into()], cwd: cwd.clone() };
        let codex = Foreground { pid: 300, name: "codex".into(), argv: vec!["codex".into()], cwd: cwd.clone() };
        let ls = Foreground { pid: 400, name: "ls".into(), argv: vec!["ls".into()], cwd: cwd.clone() };

        // 1. Agent A (claude) runs and leaves checkpoints + sent prompts; exiting back to zsh clears them and resets turn.
        {
            let mut st = core.lock();
            {
                let t = st.pty_reg.terminals.get_mut(&id).unwrap();
                t.foreground = Some(claude.clone());
                t.turn = 4;
            }
            st.pty_prompts.insert(id, vec![(1, 10, "prompt A".into())]);
            core.save_target_checkpoints(
                &mut st,
                TargetCheckpoints {
                    addr: addr.clone(),
                    cli_kind: Some("claude".into()),
                    cli_session_id: Some("sess-a".into()),
                    cli_fg_pid: Some(200),
                    checkpoints: vec![CheckpointMeta {
                        turn: 1,
                        session_turn: Some(1),
                        ref_name: "refs/herdr2/checkpoints/test/t1/turn-1".into(),
                        commit_oid: "deadbeef".into(),
                        head_oid: None,
                        prompt: Some("prompt A".into()),
                        label: None,
                        label_source: None,
                        files: vec![],
                        ts_ms: 10,
                    }],
                    ..Default::default()
                },
            );
            core.apply_foreground_update(&mut st, id, Some(zsh.clone()));
            let after = core.target_checkpoints(&mut st, &addr);
            assert!(after.checkpoints.is_empty(), "exiting CLI back to shell must clear Agent A checkpoints");
            assert!(after.cli_kind.is_none());
            assert!(after.cli_session_id.is_none());
            assert!(!st.pty_prompts.contains_key(&id));
            assert_eq!(st.pty_reg.terminals[&id].turn, 0, "exiting CLI to shell must reset turn to 0");
        }

        // 2. Rapid switch directly from Agent A (claude) to clean Agent B (codex) also clears stale checkpoints.
        {
            let mut st = core.lock();
            st.pty_reg.terminals.get_mut(&id).unwrap().foreground = Some(claude.clone());
            core.save_target_checkpoints(
                &mut st,
                TargetCheckpoints {
                    addr: addr.clone(),
                    cli_kind: Some("claude".into()),
                    cli_session_id: Some("sess-a".into()),
                    cli_fg_pid: Some(200),
                    checkpoints: vec![CheckpointMeta {
                        turn: 1,
                        session_turn: Some(1),
                        ref_name: "refs/herdr2/checkpoints/test/t1/turn-1".into(),
                        commit_oid: "deadbeef".into(),
                        head_oid: None,
                        prompt: Some("prompt A".into()),
                        label: None,
                        label_source: None,
                        files: vec![],
                        ts_ms: 10,
                    }],
                    ..Default::default()
                },
            );
            core.apply_foreground_update(&mut st, id, Some(codex.clone()));
            let after = core.target_checkpoints(&mut st, &addr);
            assert!(after.checkpoints.is_empty(), "starting fresh clean CLI agent must clear previous CLI checkpoints");
        }

        // 3. Manual checkpoint taken in a plain shell survives running and exiting `ls`.
        {
            let mut st = core.lock();
            st.pty_reg.terminals.get_mut(&id).unwrap().foreground = Some(zsh.clone());
            core.save_target_checkpoints(
                &mut st,
                TargetCheckpoints {
                    addr: addr.clone(),
                    checkpoints: vec![CheckpointMeta {
                        turn: 0,
                        session_turn: None,
                        ref_name: "refs/herdr2/checkpoints/test/t1/turn-0".into(),
                        commit_oid: "cafebabe".into(),
                        head_oid: None,
                        prompt: None,
                        label: Some("manual".into()),
                        label_source: Some("manual".into()),
                        files: vec![],
                        ts_ms: 20,
                    }],
                    ..Default::default()
                },
            );
            core.apply_foreground_update(&mut st, id, Some(ls));
            core.apply_foreground_update(&mut st, id, Some(zsh.clone()));
            let after = core.target_checkpoints(&mut st, &addr);
            assert_eq!(after.checkpoints.len(), 1, "plain shell manual snapshot must be preserved");
        }

        // 4. Exiting Claude archives its session checkpoints so later `claude -c` re-attaches them
        // even if a placeholder T0 baseline was already captured on the same fg_pid.
        {
            let mut st = core.lock();
            st.pty_reg.terminals.get_mut(&id).unwrap().foreground = Some(claude.clone());
            core.save_target_checkpoints(
                &mut st,
                TargetCheckpoints {
                    addr: addr.clone(),
                    repo_root: Some(root.to_string_lossy().to_string()),
                    cli_kind: Some("claude".into()),
                    cli_session_id: Some("sess-resume-1".into()),
                    cli_fg_pid: Some(200),
                    checkpoints: vec![
                        CheckpointMeta {
                            turn: 0,
                            session_turn: None,
                            ref_name: "refs/herdr2/checkpoints/test/t1/0".into(),
                            commit_oid: "11111111".into(),
                            head_oid: None,
                            prompt: None,
                            label: None,
                            label_source: None,
                            files: vec![],
                            ts_ms: 100,
                        },
                        CheckpointMeta {
                            turn: 1,
                            session_turn: Some(1),
                            ref_name: "refs/herdr2/checkpoints/test/t1/1".into(),
                            commit_oid: "22222222".into(),
                            head_oid: None,
                            prompt: Some("add calc".into()),
                            label: None,
                            label_source: None,
                            files: vec![],
                            ts_ms: 200,
                        },
                    ],
                    ..Default::default()
                },
            );
            // Exit to zsh: active view becomes clean...
            core.apply_foreground_update(&mut st, id, Some(zsh));
            assert!(core.target_checkpoints(&mut st, &addr).checkpoints.is_empty());
            // ...but archived session checkpoints are preserved and loadable on `-c` / `--resume`!
            let archived = core
                .load_archived_cli_session("claude", Some("sess-resume-1"), Some(&root.to_string_lossy()))
                .expect("archived claude session must survive exit to shell");
            assert_eq!(archived.checkpoints.len(), 2);

            // Simulate user running `claude --dangerously-skip-permissions -c` where a blank T0
            // raced ahead with the same cli_fg_pid before reattach ran.
            let claude_continue = Foreground {
                pid: 400,
                name: "claude".into(),
                argv: vec!["claude".into(), "--dangerously-skip-permissions".into(), "-c".into()],
                cwd: root.to_string_lossy().to_string(),
            };
            core.apply_foreground_update(&mut st, id, Some(claude_continue));
            core.save_target_checkpoints(
                &mut st,
                TargetCheckpoints {
                    addr: addr.clone(),
                    repo_root: Some(root.to_string_lossy().to_string()),
                    cli_fg_pid: Some(400),
                    checkpoints: vec![CheckpointMeta {
                        turn: 0,
                        session_turn: None,
                        ref_name: "refs/herdr2/checkpoints/test/t1/0".into(),
                        commit_oid: "99999999".into(),
                        head_oid: None,
                        prompt: None,
                        label: None,
                        label_source: None,
                        files: vec![],
                        ts_ms: 300,
                    }],
                    ..Default::default()
                },
            );
        }
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let core_arc = Arc::new(core);
        let reattached = rt.block_on(crate::pty_adapters::reattach_resumed_cli_checkpoints(&core_arc, id));
        assert!(reattached, "claude -c must automatically re-attach archived session checkpoints even after placeholder T0");
        {
            let mut st = core_arc.lock();
            let restored = core_arc.target_checkpoints(&mut st, &addr);
            assert_eq!(restored.checkpoints.len(), 2);
            assert_eq!(restored.cli_session_id.as_deref(), Some("sess-resume-1"));
            assert_eq!(restored.cli_fg_pid, Some(400));
            assert_eq!(st.pty_reg.terminals[&id].turn, 1);
        }

        // 5. Post-rollback wrapper `t.argv` containing `--resume <sid>` must NOT re-attach checkpoints
        // after exiting back to `zsh -li` OR when starting a fresh clean `claude --dangerously-skip-permissions`.
        {
            let mut st = core_arc.lock();
            {
                let t = st.pty_reg.terminals.get_mut(&id).unwrap();
                t.argv = vec![
                    "/usr/bin/zsh".into(),
                    "-lic".into(),
                    "\"$@\"; exec \"$0\" -li".into(),
                    "/usr/bin/zsh".into(),
                    "claude".into(),
                    "--dangerously-skip-permissions".into(),
                    "--resume".into(),
                    "sess-resume-1".into(),
                ];
            }
            let zsh_li = Foreground {
                pid: 100,
                name: "zsh".into(),
                argv: vec!["/usr/bin/zsh".into(), "-li".into()],
                cwd: root.to_string_lossy().to_string(),
            };
            core_arc.apply_foreground_update(&mut st, id, Some(zsh_li));
            assert!(core_arc.target_checkpoints(&mut st, &addr).checkpoints.is_empty());
            assert_eq!(st.pty_reg.terminals[&id].argv, vec!["/usr/bin/zsh", "-li"]);
        }
        assert!(
            !rt.block_on(crate::pty_adapters::reattach_resumed_cli_checkpoints(&core_arc, id)),
            "exited shell root must not re-attach checkpoints from stale wrapper argv"
        );
        {
            let mut st = core_arc.lock();
            let clean_claude = Foreground {
                pid: 500,
                name: "claude".into(),
                argv: vec!["claude".into(), "--dangerously-skip-permissions".into()],
                cwd: root.to_string_lossy().to_string(),
            };
            core_arc.apply_foreground_update(&mut st, id, Some(clean_claude));
        }
        assert!(
            !rt.block_on(crate::pty_adapters::reattach_resumed_cli_checkpoints(&core_arc, id)),
            "fresh clean `claude --dangerously-skip-permissions` must not re-attach old checkpoints"
        );
        {
            let mut st = core_arc.lock();
            assert!(core_arc.target_checkpoints(&mut st, &addr).checkpoints.is_empty());
        }

        drop(core_arc);
        let _ = std::fs::remove_dir_all(root);
    }
}
