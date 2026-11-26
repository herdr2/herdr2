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

/// Shells on Windows (Git Bash / MSYS2 / Cygwin) whose `cd` updates their internal
/// POSIX cwd without calling Win32 `SetCurrentDirectoryW`, leaving PEB `CurrentDirectory`
/// frozen at startup. For these shells, OSC 7 `reported_cwd` is authoritative.
pub(crate) fn is_windows_posix_shell(name: &str) -> bool {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase();
    let stem = base.strip_suffix(".exe").unwrap_or(&base);
    matches!(stem, "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "tcsh" | "csh" | "ash" | "mintty")
}

/// Any interactive shell on Windows (POSIX/MSYS, CMD, PowerShell, nushell) whose
/// OSC 7 / OSC 9;9 working-directory reports should update `foreground.cwd` immediately.
pub(crate) fn is_windows_shell(name: &str) -> bool {
    if is_windows_posix_shell(name) {
        return true;
    }
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase();
    let stem = base.strip_suffix(".exe").unwrap_or(&base);
    matches!(stem, "cmd" | "powershell" | "pwsh" | "nu")
}

/// Parses an OSC 7 `file://[host]/path` URI or an OSC 9;9 plain Windows path
/// (e.g. `D:\Work\proj` from `cmd.exe` `PROMPT=$E]9;9;$P$E\$P$G`) into a local OS directory path.
/// Ignores OSC 7 URIs targeting a different host (e.g. an SSH session inside a local PTY).
pub(crate) fn parse_osc7_cwd(raw: &str, local_host: &str, windows: bool) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\0') {
        return None;
    }
    // OSC 9;9 on Windows sends a plain drive path (`D:\Work\proj` or `D:/Work/proj`) without `file://`.
    if windows && (trimmed.len() < 7 || !trimmed[..7].eq_ignore_ascii_case("file://")) {
        let bytes = trimmed.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            let drive = (bytes[0] as char).to_ascii_uppercase();
            let tail = trimmed[2..].replace('/', "\\");
            let mut out = if tail.is_empty() { format!("{drive}:\\") } else { format!("{drive}:{tail}") };
            while out.len() > 3 && out.ends_with('\\') {
                out.pop();
            }
            return Some(out);
        }
        return None;
    }
    if trimmed.len() < 7 || !trimmed[..7].eq_ignore_ascii_case("file://") {
        return None;
    }
    let rest = &trimmed[7..];
    let slash = rest.find('/')?;
    let host = &rest[..slash];
    if !host.is_empty()
        && !host.eq_ignore_ascii_case("localhost")
        && !local_host.is_empty()
        && !host.eq_ignore_ascii_case(local_host)
    {
        return None;
    }
    let raw_path = &rest[slash..];
    let decoded = percent_decode_utf8(raw_path)?;
    if decoded.is_empty() || decoded.contains('\0') {
        return None;
    }
    if windows {
        let bytes = decoded.as_bytes();
        // `file:///D:/Work/repo` -> `/D:/Work/repo` -> `D:\Work\repo`
        if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
            let drive = (bytes[1] as char).to_ascii_uppercase();
            let tail = decoded[3..].replace('/', "\\");
            let mut out = if tail.is_empty() { format!("{drive}:\\") } else { format!("{drive}:{tail}") };
            while out.len() > 3 && out.ends_with('\\') {
                out.pop();
            }
            return Some(out);
        }
        // MSYS2/Git Bash POSIX drive path: `/d/Work/repo` or `/d` -> `D:\Work\repo` or `D:\`
        if bytes.len() >= 2
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && (bytes.len() == 2 || bytes[2] == b'/')
        {
            let drive = (bytes[1] as char).to_ascii_uppercase();
            let tail = if bytes.len() == 2 { "\\".to_string() } else { decoded[2..].replace('/', "\\") };
            let mut out = format!("{drive}:{tail}");
            while out.len() > 3 && out.ends_with('\\') {
                out.pop();
            }
            return Some(out);
        }
        return None;
    }
    if !decoded.starts_with('/') {
        return None;
    }
    let mut out = decoded;
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    Some(out)
}

fn percent_decode_utf8(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn is_shell_root(root: &Foreground) -> bool {
    // Command and script wrappers can share their foreground group with a live agent.
    let mut skip_next = false;
    for arg in root.argv.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if matches!(arg.as_str(), "--rcfile" | "--init-file") {
            skip_next = true;
            continue;
        }
        if (!arg.starts_with('-') && !arg.starts_with('+'))
            || matches!(arg.split('=').next(), Some("--command" | "--commands"))
            || (arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains('c'))
        {
            return false;
        }
    }
    let program =
        if root.name.is_empty() { root.argv.first().map(String::as_str).unwrap_or_default() } else { &root.name };
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program).to_ascii_lowercase();
    let name = base.strip_suffix(".exe").unwrap_or(&base);
    matches!(name, "sh" | "bash" | "zsh" | "fish" | "nu" | "dash" | "ksh" | "tcsh" | "pwsh" | "powershell" | "cmd")
}

pub(crate) fn is_at_shell_root(t: &crate::terminals::Terminal) -> bool {
    t.foreground.as_ref().is_some_and(|n| Some(n.pid as u32) == t.pid && is_shell_root(n))
}

pub(crate) fn is_cli_foreground(fg: &Foreground) -> bool {
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
        matches!(
            clean,
            "cursor-agent" | "opencode" | "kimi" | "copilot" | "hermes" | "zcode" | "zcode-cli" | "qoder" | "dsh"
        )
    })
}

impl Core {
    /// Records an OSC 7 / OSC 9;9 working directory update and immediately propagates it to
    /// `foreground.cwd` for Windows shells (Git Bash / MSYS2 / Cygwin / CMD / PowerShell),
    /// synthesizing a shell foreground record if `foreground::about` has not populated one yet.
    pub(crate) fn apply_osc7_cwd(&self, st: &mut crate::runtime::State, id: u32, dir: String, windows: bool) {
        let mut emit_fg = None;
        let mut changed = false;
        if let Some(t) = st.pty_reg.terminals.get_mut(&id).filter(|t| t.is_live()) {
            if t.reported_cwd.as_deref() != Some(dir.as_str()) {
                t.reported_cwd = Some(dir.clone());
                changed = true;
            }
            if t.last_cwd.as_deref() != Some(dir.as_str()) {
                t.last_cwd = Some(dir.clone());
                changed = true;
            }
            if windows {
                if let Some(fg) = t.foreground.as_mut() {
                    if is_windows_shell(&fg.name) && fg.cwd != dir {
                        fg.cwd = dir;
                        emit_fg = Some(fg.clone());
                        changed = true;
                    }
                } else if let Some(pid) = t.pid {
                    let name = t
                        .argv
                        .first()
                        .map(|p| p.rsplit(['/', '\\']).next().unwrap_or(p).to_string())
                        .unwrap_or_default();
                    if is_windows_shell(&name) {
                        let synth = Foreground { pid: pid as i32, name, argv: t.argv.clone(), cwd: dir };
                        t.foreground = Some(synth.clone());
                        emit_fg = Some(synth);
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.save_pty_soon();
        }
        if let Some(fg) = emit_fg {
            self.emit_pty(st, "foreground", id, serde_json::to_value(&fg).unwrap_or_default());
        }
    }

    /// Applies one foreground sample for terminal `id`, resetting agent status, task digest, timing,
    /// and stale CLI checkpoints when a CLI exits back to its shell or a fresh non-resumed CLI starts
    /// in the same terminal.
    pub(crate) fn apply_foreground_update(&self, st: &mut crate::runtime::State, id: u32, now: Option<Foreground>) {
        self.apply_foreground_update_for_platform(st, id, now, cfg!(windows));
    }

    pub(crate) fn apply_foreground_update_for_platform(
        &self,
        st: &mut crate::runtime::State,
        id: u32,
        mut now: Option<Foreground>,
        windows: bool,
    ) {
        let Some((
            t_pid,
            t_addr,
            t_cwd,
            t_argv,
            prev_fg,
            has_status_or_records,
            has_agent_task_state,
            t_title,
            reported_cwd,
            prev_os_cwd,
        )) = st.pty_reg.terminals.get(&id).filter(|t| t.is_live()).map(|t| {
            let cwd = t.last_cwd.clone().unwrap_or_else(|| t.cwd.clone());
            let has_status = t.status.is_some() || !t.records.is_empty();
            let has_task_state = has_status
                || t.semantic_title.is_some()
                || t.micro_summary.is_some()
                || t.reported
                || t.turn > 0
                || t.working_since.is_some()
                || t.work_ms.is_some()
                || t.total_work_ms > 0
                || t.status_ms.is_some()
                || t.labels.contains_key("herdr.status")
                || st.pty_prompts.contains_key(&id);
            (
                t.pid,
                t.address.clone(),
                cwd,
                t.argv.clone(),
                t.foreground.clone(),
                has_status,
                has_task_state,
                t.title.clone(),
                t.reported_cwd.clone(),
                t.last_os_cwd.clone(),
            )
        })
        else {
            return;
        };

        let mut next_os_cwd = prev_os_cwd.clone();
        let mut next_reported_cwd = reported_cwd.clone();
        if windows {
            match now.as_mut() {
                Some(fg) => {
                    let raw_os_cwd = fg.cwd.clone();
                    if let Some(rcwd) = reported_cwd.as_deref() {
                        if is_windows_posix_shell(&fg.name) {
                            fg.cwd = rcwd.to_owned();
                        } else if is_windows_shell(&fg.name) {
                            let os_moved = prev_os_cwd.as_deref().is_some_and(|p| p != raw_os_cwd);
                            if os_moved {
                                next_reported_cwd = Some(raw_os_cwd.clone());
                            } else {
                                fg.cwd = rcwd.to_owned();
                            }
                        }
                    }
                    next_os_cwd = Some(raw_os_cwd);
                }
                None => {
                    if let (Some(pid), Some(rcwd)) = (t_pid, reported_cwd.as_deref()) {
                        let name = t_argv
                            .first()
                            .map(|p| p.rsplit(['/', '\\']).next().unwrap_or(p).to_string())
                            .unwrap_or_default();
                        if is_windows_shell(&name) {
                            now = Some(Foreground { pid: pid as i32, name, argv: t_argv, cwd: rcwd.to_owned() });
                        }
                    }
                }
            }
            if let Some(t) = st.pty_reg.terminals.get_mut(&id) {
                t.last_os_cwd = next_os_cwd;
                t.reported_cwd = next_reported_cwd;
            }
        }

        let now_is_shell_root = now.as_ref().is_some_and(|n| Some(n.pid as u32) == t_pid && is_shell_root(n));
        let prev_was_shell_root = prev_fg.as_ref().is_some_and(|p| Some(p.pid as u32) == t_pid && is_shell_root(p));
        let at_shell = crate::platform::foreground_tracks_jobs() && now_is_shell_root;
        let respawning = st.pty_respawning.contains(&id);

        if !now_is_shell_root && now.as_ref().is_some_and(is_cli_foreground) && !t_title.is_empty() {
            st.pty_cli_titles.insert(id, t_title);
        }

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
                let has_attached_cli_meta = tc.cli_kind.as_deref().is_some_and(|k| !k.is_empty() && k != "generic")
                    || tc.cli_session_id.is_some();
                let had_cli_session = has_attached_cli_meta
                    || tc.cli_fg_pid.is_some()
                    || has_status_or_records
                    || prev_fg.as_ref().is_some_and(is_cli_foreground);
                let exited_cli_to_shell = now_is_shell_root
                    && (has_attached_cli_meta || (had_cli_session && (!prev_was_shell_root || has_status_or_records)));
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
                if pid_changed
                    || (fg_pid.is_some()
                        && tc.cli_fg_pid != fg_pid
                        && crate::pty_adapters::needs_resumed_reattach(&tc, fg_pid))
                {
                    should_reattach_resumed = true;
                }
            }
        }

        let prev_was_cli = prev_fg.as_ref().is_some_and(is_cli_foreground);
        let exited_to_shell_from_cli =
            now_is_shell_root && (cleared_checkpoints || prev_was_cli || (at_shell && has_agent_task_state));
        let entered_fresh_cli_from_shell_or_other = !now_is_shell_root
            && now.as_ref().is_some_and(|fg| {
                let pid_changed = prev_fg.as_ref().is_some_and(|p| p.pid != fg.pid);
                is_cli_foreground(fg)
                    && !crate::pty_transcript::argv_has_resume(&fg.argv)
                    && pid_changed
            });
        let started_clean_cli_any = !now_is_shell_root
            && now.as_ref().is_some_and(|fg| {
                is_cli_foreground(fg)
                    && !crate::pty_transcript::argv_has_resume(&fg.argv)
                    && (cleared_checkpoints
                        || prev_fg.as_ref().is_some_and(|p| p.pid != fg.pid && is_cli_foreground(p)))
            });
        let clear_agent_task_state = !respawning && (exited_to_shell_from_cli || started_clean_cli_any);

        let mut cleared_cli_title = None;
        if clear_agent_task_state {
            st.pty_prompts.remove(&id);
            st.pty_digest_last_ai.remove(&id);
            cleared_cli_title = st.pty_cli_titles.remove(&id);
            self.clear_manual_status_pty_locked(st, id);
            if exited_to_shell_from_cli
                && st
                    .pty_reg
                    .terminals
                    .get(&id)
                    .is_some_and(|t| t.labels.get("herdr.view").map(String::as_str) == Some("peek"))
            {
                let was = st.pty_reg.terminals.get(&id).cloned();
                st.pty_reg.terminals.get_mut(&id).expect("checked").labels.remove("herdr.view");
                let labels = serde_json::to_value(&st.pty_reg.terminals[&id].labels).unwrap_or_default();
                self.save_pty(st);
                self.emit_pty_from(st, "labels", id, serde_json::json!({ "labels": labels }), was.as_ref());
            }
        }

        let Some(t) = st.pty_reg.terminals.get_mut(&id).filter(|t| t.is_live()) else {
            return;
        };
        let mut change =
            ((at_shell || clear_agent_task_state) && (t.status.is_some() || !t.records.is_empty())).then(|| {
                t.first_task_turn = None;
                crate::status::reset_terminal(t)
            });
        let mut turn_reset = false;
        let mut digest_cleared = false;
        let mut title_cleared = false;
        let mut status_meta_cleared = false;
        if clear_agent_task_state {
            if let Some(ref mut ch) = change {
                // Exiting a working CLI back to shell or starting a fresh clean CLI
                // must not trigger a post-exit shell checkpoint or AI digest.
                ch.turn_ended = false;
            }
            if t.semantic_title.take().is_some() | t.micro_summary.take().is_some() {
                digest_cleared = true;
            }
            if t.first_task_turn.take().is_some()
                | t.working_since.take().is_some()
                | t.work_ms.take().is_some()
                | (t.total_work_ms != 0)
                | t.status_ms.take().is_some()
                | t.reported
            {
                t.total_work_ms = 0;
                t.reported = false;
                status_meta_cleared = true;
            }
            if t.turn != 0 {
                t.turn = 0;
                turn_reset = true;
            }
            if cleared_cli_title.as_deref().is_some_and(|ct| !ct.is_empty() && ct == t.title) {
                t.title.clear();
                title_cleared = true;
            }
        }
        if !respawning && (started_clean_cli_any || entered_fresh_cli_from_shell_or_other) {
            t.program_started_ms = Some(crate::runtime::now_ms());
        }
        if status_meta_cleared && change.is_none() {
            change = Some(crate::status::Change { status: true, ..Default::default() });
        }
        let foreground_changed = t.foreground != now;
        let mut argv_updated = false;
        let mut saved_cli_changed = false;
        if let Some(f) = &now {
            t.last_cwd = Some(f.cwd.clone());
            // Once a rollback/launch shell wrapper (`zsh -lic '"$@"; exec "$0" -li' ... claude --resume ...`)
            // exits back to the interactive shell (`zsh -li`), replace stale wrapper `t.argv` with the live shell
            // argv so old `--resume` flags cannot linger after exit.
            if now_is_shell_root {
                if !f.argv.is_empty()
                    && crate::pty_adapters::PtyCliKind::detect(&t.argv) != crate::pty_adapters::PtyCliKind::Generic
                {
                    t.argv = f.argv.clone();
                    argv_updated = true;
                }
                if t.saved_cli.take().is_some() {
                    saved_cli_changed = true;
                }
            } else if !respawning {
                let root_shell = crate::pty_adapters::terminal_shell_executable(&t.argv);
                if let Some(mut detected) =
                    crate::pty_adapters::extract_saved_cli_from_argv(&f.argv, root_shell.as_deref(), None, None)
                {
                    if let Some(ref existing) = t.saved_cli
                        && existing.kind == detected.kind
                        && !started_clean_cli_any
                    {
                        if detected.session_id.is_none() {
                            detected.session_id = existing.session_id.clone();
                        }
                        if detected.transcript_path.is_none() {
                            detected.transcript_path = existing.transcript_path.clone();
                        }
                        if detected.wrapper_shell.is_none() {
                            detected.wrapper_shell = existing.wrapper_shell.clone();
                        }
                    }
                    if t.saved_cli.as_ref() != Some(&detected) {
                        t.saved_cli = Some(detected);
                        saved_cli_changed = true;
                    }
                }
            }
        }
        t.foreground = now.clone();
        if change.is_some()
            || foreground_changed
            || turn_reset
            || argv_updated
            || saved_cli_changed
            || digest_cleared
            || title_cleared
        {
            self.save_pty_soon();
        }
        if let Some(change) = change {
            self.pty_status_changed(st, id, change);
        }
        if turn_reset {
            self.emit_pty(st, "turn", id, serde_json::json!({ "turn": 0 }));
        }
        if digest_cleared {
            self.emit_pty(
                st,
                "digest",
                id,
                serde_json::json!({ "semantic_title": serde_json::Value::Null, "micro_summary": serde_json::Value::Null }),
            );
        }
        if title_cleared {
            self.emit_pty(st, "title", id, serde_json::json!({ "title": "" }));
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
        let mut last_cli_probe: std::collections::HashMap<u32, std::time::Instant> = std::collections::HashMap::new();
        loop {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            let live: Vec<u32> =
                self.lock().pty_reg.terminals.values().filter(|t| t.is_live()).map(|t| t.id()).collect();
            last_cli_probe.retain(|id, _| live.contains(id));
            for id in live {
                let pgid = self.foreground(id).await;
                let now = (pgid > 0).then(|| about(pgid)).flatten();
                let needs_cli_sid = {
                    let mut st = self.lock();
                    self.apply_foreground_update(&mut st, id, now);
                    st.pty_reg
                        .terminals
                        .get(&id)
                        .and_then(|t| t.saved_cli.as_ref())
                        .is_some_and(|s| s.session_id.is_none() || s.transcript_path.is_none())
                };
                if needs_cli_sid && last_cli_probe.get(&id).is_none_or(|at| at.elapsed() >= Duration::from_secs(3)) {
                    last_cli_probe.insert(id, std::time::Instant::now());
                    let core_bg = Arc::clone(&self);
                    tokio::spawn(async move {
                        crate::pty_adapters::discover_and_bind_cli_session(&core_bg, id).await;
                    });
                }
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

        let root =
            crate::test_tmp().join(format!("h2-fg-ckpt-{}-{}", std::process::id(), crate::platform::random_hex()));
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
        assert!(
            reattached,
            "claude -c must automatically re-attach archived session checkpoints even after placeholder T0"
        );
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

    #[test]
    fn exiting_cli_agent_to_shell_clears_digest_timing_status_and_stale_title() {
        use crate::paths::Paths;
        use crate::terminals::NewTerminal;

        let root =
            crate::test_tmp().join(format!("h2-fg-exit-task-{}-{}", std::process::id(), crate::platform::random_hex()));
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
        let cwd = root.to_string_lossy().to_string();
        let zsh = Foreground { pid: 100, name: "zsh".into(), argv: vec!["zsh".into()], cwd: cwd.clone() };
        let claude = Foreground { pid: 200, name: "claude".into(), argv: vec!["claude".into()], cwd: cwd.clone() };

        // 1. First foreground poll of a freshly started CLI that already reported status must NOT wipe its state.
        {
            let mut st = core.lock();
            {
                let t = st.pty_reg.terminals.get_mut(&id).unwrap();
                t.reported = true;
                t.status = Some(crate::status::Status {
                    state: "working".into(),
                    kind: None,
                    progress: None,
                    app: "claude-code".into(),
                    title: String::new(),
                    msg: String::new(),
                });
                t.semantic_title = Some("Fix login flow".into());
                t.micro_summary = Some("Updating auth handler".into());
                t.turn = 3;
                t.working_since = Some(1000);
                t.work_ms = Some(5000);
                t.total_work_ms = 12_000;
                t.status_ms = Some(2000);
                t.title = "✳ Fix login flow".into();
                t.labels.insert("herdr.status".into(), "blocked@3".into());
                t.labels.insert("herdr.view".into(), "peek".into());
            }
            st.pty_digest_last_ai.insert(id, std::time::Instant::now());

            core.apply_foreground_update(&mut st, id, Some(claude.clone()));
            let t = &st.pty_reg.terminals[&id];
            assert_eq!(t.semantic_title.as_deref(), Some("Fix login flow"));
            assert_eq!(t.micro_summary.as_deref(), Some("Updating auth handler"));
            assert!(t.reported);
            assert_eq!(t.turn, 3);
            assert_eq!(t.status.as_ref().map(|s| s.state.as_str()), Some("working"));
        }

        // 2. Exiting the CLI back to shell clears all task info even without checkpoints.
        {
            let mut st = core.lock();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            core.subscribe(&mut st, crate::events::Filter::default(), crate::events::Sink::Waiter(tx));
            core.apply_foreground_update(&mut st, id, Some(zsh.clone()));

            let t = &st.pty_reg.terminals[&id];
            assert_eq!(t.semantic_title, None, "semantic_title must be cleared on CLI exit");
            assert_eq!(t.micro_summary, None, "micro_summary must be cleared on CLI exit");
            assert_eq!(t.status, None, "status must be cleared on CLI exit");
            assert!(!t.reported, "reported must be reset to false on CLI exit");
            assert_eq!(t.turn, 0, "turn must be reset to 0 on CLI exit");
            assert_eq!(t.working_since, None);
            assert_eq!(t.work_ms, None);
            assert_eq!(t.total_work_ms, 0);
            assert_eq!(t.status_ms, None);
            assert!(!t.labels.contains_key("herdr.status"), "manual status label must be cleared");
            assert!(
                !t.labels.contains_key("herdr.view"),
                "structured peek view must return to raw terminal on CLI exit"
            );
            assert!(t.title.is_empty(), "unchanged CLI title must be cleared when shell did not overwrite it");
            assert!(!st.pty_digest_last_ai.contains_key(&id));

            let mut kinds = Vec::new();
            while let Ok(ev) = rx.try_recv() {
                if let Some(kind) = ev["type"].as_str() {
                    kinds.push(kind.to_owned());
                }
            }
            assert!(kinds.iter().any(|k| k == "digest"), "must emit digest clear event: {kinds:?}");
            assert!(kinds.iter().any(|k| k == "status"), "must emit status clear event: {kinds:?}");
            assert!(kinds.iter().any(|k| k == "turn"), "must emit turn reset event: {kinds:?}");
            assert!(kinds.iter().any(|k| k == "title"), "must emit title clear event: {kinds:?}");
            assert!(kinds.iter().any(|k| k == "labels"), "must emit labels update event: {kinds:?}");
        }

        // 3. If the shell already updated OSC title before the foreground poll, preserve the shell's title.
        {
            let mut st = core.lock();
            {
                let t = st.pty_reg.terminals.get_mut(&id).unwrap();
                t.title = "✳ Second CLI task".into();
                t.semantic_title = Some("Second CLI task".into());
            }
            core.apply_foreground_update(&mut st, id, Some(claude));
            st.pty_reg.terminals.get_mut(&id).unwrap().title = "~/Project/herdr2".into();
            core.apply_foreground_update(&mut st, id, Some(zsh));
            let t = &st.pty_reg.terminals[&id];
            assert_eq!(t.semantic_title, None);
            assert_eq!(t.title, "~/Project/herdr2", "shell's fresh title must be preserved");
        }

        // 4. Starting a fresh non-resumed CLI inside an older shell advances `program_started_ms`
        // so transcript discovery does not inherit sessions created while the shell was idle earlier.
        {
            let mut st = core.lock();
            st.pty_reg.terminals.get_mut(&id).unwrap().program_started_ms = Some(1_000);
            let fresh_agy = Foreground {
                pid: 456,
                name: "agy.real".into(),
                argv: vec!["/home/u/.local/bin/agy.real".into(), "--dangerously-skip-permissions".into()],
                cwd: root.to_string_lossy().to_string(),
            };
            core.apply_foreground_update(&mut st, id, Some(fresh_agy));
            let started = st.pty_reg.terminals[&id].program_started_ms.unwrap_or(0);
            assert!(started > 1_000, "fresh CLI start must advance program_started_ms, got {started}");
        }

        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recognizes_windows_bundled_cursor_agent_as_cli_foreground() {
        let fg = Foreground {
            pid: 64740,
            name: "node.exe".into(),
            argv: vec![
                r"D:\Users\zhihao.zou\AppData\Local\cursor-agent\versions\2026.10.22-27dcf5f\node.exe".into(),
                r"D:\Users\zhihao.zou\AppData\Local\cursor-agent\versions\2026.10.22-27dcf5f\index.js".into(),
                "--yolo".into(),
                "--trust".into(),
            ],
            cwd: r"D:\Users\zhihao.zou".into(),
        };
        assert!(is_cli_foreground(&fg));
    }

    #[test]
    fn osc7_cwd_parsing_and_windows_git_bash_sync() {
        use crate::paths::Paths;
        use crate::terminals::NewTerminal;

        // Windows drive paths (`pwd -W`, MSYS `/d/...`, and CMD OSC 9;9), percent-encoding, UTF-8, and host filtering.
        assert_eq!(parse_osc7_cwd("file:///D:/Work/herdr2", "my-pc", true).as_deref(), Some(r"D:\Work\herdr2"));
        assert_eq!(parse_osc7_cwd(r"D:\Work\herdr2\", "my-pc", true).as_deref(), Some(r"D:\Work\herdr2"));
        assert_eq!(parse_osc7_cwd(r"C:\", "my-pc", true).as_deref(), Some(r"C:\"));
        assert_eq!(
            parse_osc7_cwd("file://MY-PC/d:/Work/my%20repo/%E9%A1%B9%E7%9B%AE/", "my-pc", true).as_deref(),
            Some(r"D:\Work\my repo\项目")
        );
        assert_eq!(parse_osc7_cwd("file:///d/Work/herdr2/", "my-pc", true).as_deref(), Some(r"D:\Work\herdr2"));
        assert_eq!(parse_osc7_cwd("file:///c:/", "my-pc", true).as_deref(), Some(r"C:\"));
        assert_eq!(parse_osc7_cwd("file://remote-box/D:/Work/herdr2", "my-pc", true), None);
        assert_eq!(parse_osc7_cwd("file://localhost/home/me/my%20app/", "my-pc", false).as_deref(), Some("/home/me/my app"));

        // `bash --rcfile <path> -i` stays recognized as an interactive shell root.
        let rc_bash = Foreground {
            pid: 10,
            name: "bash.exe".into(),
            argv: vec![
                r"C:\Program Files\Git\bin\bash.exe".into(),
                "--rcfile".into(),
                "C:/Users/me/AppData/Roaming/herdr2/shell/git-bash-init.sh".into(),
                "-i".into(),
            ],
            cwd: r"C:\Users\me".into(),
        };
        assert!(is_shell_root(&rc_bash));
        assert!(is_windows_posix_shell(&rc_bash.name));

        // On Windows, OSC 7 overrides Git Bash's frozen PEB cwd immediately and across subsequent polls,
        // without overriding native Win32 child processes (e.g. PowerShell or Claude).
        let root =
            crate::test_tmp().join(format!("h2-fg-osc7-{}-{}", std::process::id(), crate::platform::random_hex()));
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
                        argv: rc_bash.argv.clone(),
                        cwd: r"C:\Users\me".into(),
                        cols: 80,
                        rows: 24,
                        ..Default::default()
                    },
                    1,
                )
                .unwrap();
            st.pty_reg.terminals.get_mut(&id).unwrap().pid = Some(10);
            id
        };
        {
            let mut st = core.lock();
            core.apply_foreground_update_for_platform(&mut st, id, Some(rc_bash.clone()), true);
            assert_eq!(st.pty_reg.terminals[&id].foreground.as_ref().unwrap().cwd, r"C:\Users\me");

            // User runs `cd /d/Work/herdr2` in Git Bash -> OSC 7 updates live foreground cwd immediately.
            core.apply_osc7_cwd(&mut st, id, r"D:\Work\herdr2".into(), true);
            assert_eq!(st.pty_reg.terminals[&id].foreground.as_ref().unwrap().cwd, r"D:\Work\herdr2");
            assert_eq!(st.pty_reg.terminals[&id].last_cwd.as_deref(), Some(r"D:\Work\herdr2"));

            // Next 500ms PEB poll still reads frozen `C:\Users\me` from bash.exe, but OSC 7 keeps `D:\Work\herdr2`.
            core.apply_foreground_update_for_platform(&mut st, id, Some(rc_bash.clone()), true);
            assert_eq!(st.pty_reg.terminals[&id].foreground.as_ref().unwrap().cwd, r"D:\Work\herdr2");

            // Native Win32 child (e.g. PowerShell) keeps its own live PEB cwd.
            let ps = Foreground {
                pid: 20,
                name: "powershell.exe".into(),
                argv: vec!["powershell.exe".into()],
                cwd: r"E:\Other".into(),
            };
            core.apply_foreground_update_for_platform(&mut st, id, Some(ps), true);
            assert_eq!(st.pty_reg.terminals[&id].foreground.as_ref().unwrap().cwd, r"E:\Other");

            // Windows CMD terminal synthesizes and retains OSC 9;9 cwd even if PEB read returns None.
            let cmd_id = st
                .pty_reg
                .create(
                    NewTerminal {
                        argv: vec![r"C:\Windows\System32\cmd.exe".into()],
                        cwd: r"C:\Users\me".into(),
                        cols: 80,
                        rows: 24,
                        ..Default::default()
                    },
                    2,
                )
                .unwrap();
            st.pty_reg.terminals.get_mut(&cmd_id).unwrap().pid = Some(30);
            core.apply_osc7_cwd(&mut st, cmd_id, r"D:\Projects\demo".into(), true);
            assert_eq!(st.pty_reg.terminals[&cmd_id].foreground.as_ref().unwrap().cwd, r"D:\Projects\demo");
            core.apply_foreground_update_for_platform(&mut st, cmd_id, None, true);
            assert_eq!(st.pty_reg.terminals[&cmd_id].foreground.as_ref().unwrap().cwd, r"D:\Projects\demo");
        }
        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }
}
