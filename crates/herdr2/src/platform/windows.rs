//! Windows: named pipes, job objects, stdio streams.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::{size_of, zeroed};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Console::{GetStdHandle, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
use windows_sys::Win32::System::IO::*;
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::core::BOOL;

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(Some(0)).collect()
}

fn check(ok: BOOL) -> io::Result<()> {
    if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

// --- Connections: named pipes in overlapped mode ---------------------------
//
// Synchronous I/O on one pipe is serialized by Windows: a read waiting in one
// thread would hold up every write from another. Overlapped handles let both
// run at once; each call still blocks its own thread until done.

struct Pipe {
    handle: OwnedHandle,
}

impl Pipe {
    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle() as HANDLE
    }

    /// One overlapped operation, waited for.
    fn io(&self, op: impl FnOnce(HANDLE, *mut OVERLAPPED) -> BOOL) -> io::Result<usize> {
        unsafe {
            let event = CreateEventW(null(), 1, 0, null());
            if event.is_null() {
                return Err(io::Error::last_os_error());
            }
            let event = OwnedHandle::from_raw_handle(event as RawHandle);
            let mut ov: OVERLAPPED = zeroed();
            ov.hEvent = event.as_raw_handle() as HANDLE;
            if op(self.raw(), &mut ov) == 0 {
                let err = GetLastError();
                if err != ERROR_IO_PENDING {
                    return match err {
                        ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED => Ok(0),
                        _ => Err(io::Error::from_raw_os_error(err as i32)),
                    };
                }
            }
            let mut n = 0u32;
            if GetOverlappedResult(self.raw(), &ov, &mut n, 1) == 0 {
                return match GetLastError() {
                    ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED | ERROR_OPERATION_ABORTED => Ok(0),
                    err => Err(io::Error::from_raw_os_error(err as i32)),
                };
            }
            Ok(n as usize)
        }
    }

    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.io(|h, ov| unsafe {
            ReadFile(h, buf.as_mut_ptr(), buf.len().min(u32::MAX as usize) as u32, null_mut(), ov)
        })
    }

    fn write(&self, buf: &[u8]) -> io::Result<usize> {
        let n = self.io(|h, ov| unsafe {
            WriteFile(h, buf.as_ptr(), buf.len().min(u32::MAX as usize) as u32, null_mut(), ov)
        })?;
        if n == 0 && !buf.is_empty() {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        Ok(n)
    }
}

/// A connection to a core (blocking, safe to read and write from
/// different threads at once).
#[derive(Clone)]
pub struct Stream(Arc<Pipe>);

impl Stream {
    pub fn try_clone(&self) -> io::Result<Stream> {
        Ok(self.clone())
    }

    /// Pipes can't close one direction: either way ends the connection, and
    /// wakes any read waiting on it.
    pub fn shutdown(&self, _: std::net::Shutdown) -> io::Result<()> {
        unsafe {
            CancelIoEx(self.0.raw(), null());
            DisconnectNamedPipe(self.0.raw());
        }
        Ok(())
    }

    pub fn set_read_timeout(&self, _: Option<std::time::Duration>) -> io::Result<()> {
        Ok(())
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Read for &Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Write for &Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn connect(path: &Path) -> io::Result<Stream> {
    let name = wide(path.as_os_str());
    for _ in 0..50 {
        let h = unsafe {
            // Identification only: whoever serves this name can't act as us
            // (a fake core can't borrow an elevated h2's rights).
            let flags = FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION;
            CreateFileW(name.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, null(), OPEN_EXISTING, flags, null_mut())
        };
        if h != INVALID_HANDLE_VALUE {
            return Ok(Stream(Arc::new(Pipe { handle: unsafe { OwnedHandle::from_raw_handle(h as RawHandle) } })));
        }
        // Every instance busy: one frees up as soon as the server accepts.
        if unsafe { GetLastError() } != ERROR_PIPE_BUSY {
            return Err(io::Error::last_os_error());
        }
        unsafe { WaitNamedPipeW(name.as_ptr(), 100) };
    }
    Err(io::Error::from(io::ErrorKind::TimedOut))
}

/// Who may open our pipes: this account and SYSTEM (the default also lets
/// everyone read, and names Administrators as owner when elevated). The
/// integrity label keeps lower processes out: an unelevated program can't
/// drive an elevated h2.
fn pipe_security() -> io::Result<*const SECURITY_ATTRIBUTES> {
    use windows_sys::Win32::Security::Authorization::*;
    use windows_sys::Win32::Security::*;
    static MADE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    if let Some(sa) = MADE.get() {
        return Ok(*sa as *const SECURITY_ATTRIBUTES);
    }
    unsafe {
        let mut token = null_mut();
        check(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token))?;
        let token = OwnedHandle::from_raw_handle(token as RawHandle);
        let mut n = 0u32;
        GetTokenInformation(token.as_raw_handle() as HANDLE, TokenUser, null_mut(), 0, &mut n);
        let mut user = vec![0usize; (n as usize).div_ceil(size_of::<usize>())];
        check(GetTokenInformation(token.as_raw_handle() as HANDLE, TokenUser, user.as_mut_ptr().cast(), n, &mut n))?;
        let mut sid = null_mut();
        check(ConvertSidToStringSidW((*user.as_ptr().cast::<TOKEN_USER>()).User.Sid, &mut sid))?;
        let sid_text =
            String::from_utf16_lossy(std::slice::from_raw_parts(sid, (0..).take_while(|&i| *sid.add(i) != 0).count()));
        LocalFree(sid.cast());
        let mut elevation: TOKEN_ELEVATION = zeroed();
        check(GetTokenInformation(
            token.as_raw_handle() as HANDLE,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut n,
        ))?;
        let level = if elevation.TokenIsElevated != 0 { "HI" } else { "ME" };
        let sddl = wide(format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid_text})S:(ML;;NRNW;;;{level})").as_ref());
        let mut sd = null_mut();
        check(ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            null_mut(),
        ))?;
        // Made once, kept for the life of the process.
        let sa = Box::leak(Box::new(SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        }));
        Ok(*MADE.get_or_init(|| sa as *const _ as usize) as *const SECURITY_ATTRIBUTES)
    }
}

fn new_instance(name: &[u16], first: bool) -> io::Result<Pipe> {
    let flags = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
    let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    let h = unsafe {
        CreateNamedPipeW(name.as_ptr(), flags, mode, PIPE_UNLIMITED_INSTANCES, 65536, 65536, 0, pipe_security()?)
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(Pipe { handle: unsafe { OwnedHandle::from_raw_handle(h as RawHandle) } })
}

/// A pipe name others connect to.
pub struct Listener {
    name: Vec<u16>,
    next: std::sync::Mutex<Option<Pipe>>,
}

pub fn may_autostart(explicit: bool) -> Result<(), crate::rpc::RpcError> {
    if explicit { Ok(()) } else { super::windows_session::check_autostart() }
}

pub fn status_may_start_core() -> bool {
    false
}

pub fn connect_error(error: &io::Error, existing_only: bool) -> Option<crate::rpc::RpcError> {
    if error.kind() == io::ErrorKind::PermissionDenied {
        Some(crate::rpc::RpcError::new(
            "unavailable",
            format!("the Windows core exists but this process cannot access its pipe: {error}"),
            "if it was started elevated or over SSH, stop it with 'h2 server stop' in an elevated terminal on that Windows machine, then open Herdr2 or run 'h2 server start' in a normal desktop terminal",
        ))
    } else if existing_only {
        Some(crate::rpc::RpcError::new(
            "unavailable",
            format!("the Windows core is not reachable: {error}"),
            "open Herdr2 on that Windows machine, or run 'h2 server start' there",
        ))
    } else {
        None
    }
}

pub fn merge_env(env: &mut BTreeMap<String, String>, extra: BTreeMap<String, String>) {
    env.retain(|key, _| !extra.keys().any(|other| key.eq_ignore_ascii_case(other)));
    env.extend(extra);
}

pub fn listen(path: &Path) -> io::Result<Listener> {
    let name = wide(path.as_os_str());
    let first = new_instance(&name, true)?;
    Ok(Listener { name, next: std::sync::Mutex::new(Some(first)) })
}

impl Listener {
    pub fn incoming(&self) -> impl Iterator<Item = io::Result<Stream>> + '_ {
        std::iter::from_fn(move || Some(self.accept()))
    }

    fn accept(&self) -> io::Result<Stream> {
        let pipe = match self.next.lock().unwrap_or_else(|p| p.into_inner()).take() {
            Some(pipe) => pipe,
            None => new_instance(&self.name, false)?,
        };
        let connected = pipe.io(|h, ov| unsafe { ConnectNamedPipe(h, ov) });
        match connected {
            Ok(_) => {}
            // A client that came before we waited is connected already.
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) => {}
            Err(e) => return Err(e),
        }
        *self.next.lock().unwrap_or_else(|p| p.into_inner()) = Some(new_instance(&self.name, false)?);
        Ok(Stream(Arc::new(pipe)))
    }
}

pub type AsyncRead = tokio::io::ReadHalf<tokio::net::windows::named_pipe::NamedPipeServer>;
pub type AsyncWrite = tokio::io::WriteHalf<tokio::net::windows::named_pipe::NamedPipeServer>;

// Duplex named pipes cannot half-close: read EOF means the client disconnected.
pub async fn reply_closed(_read: &AsyncRead) {}

/// The core's pipe for clients.
pub struct AsyncListener {
    name: PathBuf,
    next: tokio::net::windows::named_pipe::NamedPipeServer,
}

impl AsyncListener {
    /// Pipes are made inside the runtime; this only names it.
    pub fn bind(path: &Path) -> io::Result<PathBuf> {
        Ok(path.to_path_buf())
    }

    pub fn serve(name: PathBuf) -> io::Result<AsyncListener> {
        let next = Self::create(&name, true)?;
        Ok(AsyncListener { name, next })
    }

    fn create(name: &Path, first: bool) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        let mut options = tokio::net::windows::named_pipe::ServerOptions::new();
        options.first_pipe_instance(first).reject_remote_clients(true);
        unsafe { options.create_with_security_attributes_raw(name, pipe_security()? as *mut c_void) }
    }

    pub async fn accept(&mut self) -> io::Result<(AsyncRead, AsyncWrite)> {
        self.next.connect().await?;
        let connected = std::mem::replace(&mut self.next, Self::create(&self.name, false)?);
        Ok(tokio::io::split(connected))
    }
}

/// Pipe names vanish with their last handle.
pub fn remove_endpoint(_: &Path) {}

// --- Files -----------------------------------------------------------------

/// Default homes for state and config (XDG_STATE_HOME / XDG_CONFIG_HOME win).
pub fn data_dirs() -> (PathBuf, PathBuf) {
    let profile = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| r"C:\".into());
    let local = std::env::var_os("LOCALAPPDATA").map_or_else(|| profile.join(r"AppData\Local"), PathBuf::from);
    let roaming = std::env::var_os("APPDATA").map_or_else(|| profile.join(r"AppData\Roaming"), PathBuf::from);
    (local, roaming)
}

/// Pipes live in their own namespace; names carry the user so two users'
/// cores never meet.
/// Pipe names: up to 256 characters.
pub const MAX_SOCKET_PATH: usize = 256;

pub fn endpoints(_state: &Path) -> (PathBuf, String) {
    let user: String =
        std::env::var("USERNAME").unwrap_or_default().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    (PathBuf::from(r"\\.\pipe\"), format!("herdr2-{user}-"))
}

pub fn private_dir(path: &Path) -> io::Result<()> {
    if path.as_os_str().to_string_lossy().starts_with(r"\\.\pipe") {
        return Ok(());
    }
    std::fs::create_dir_all(path)
}

/// A new file with the user state directory's inherited permissions.
pub fn private_file(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new().write(true).create_new(true).open(path)
}

pub fn try_lock(file: &File) -> bool {
    unsafe {
        let mut ov: OVERLAPPED = zeroed();
        LockFileEx(
            file.as_raw_handle() as HANDLE,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut ov,
        ) != 0
    }
}

pub fn can_replace(_: &Path) -> bool {
    true
}

pub fn make_executable(_: &Path) -> io::Result<()> {
    Ok(())
}

/// The files of a release, as they sit beside herdr2.exe.
const RELEASE: [&str; 1] = ["herdr2.exe"];

/// Puts the release whose herdr2.exe is `from` where `dest`'s folder
/// is. Windows won't overwrite a running file but will rename one: every
/// file is staged and checked first, then each old one is moved aside and
/// the new one moved in; any failure moves everything back. Moved-aside
/// files go on the next update. `h2.exe`, if it is a link to herdr2.exe,
/// is linked to the new one.
pub fn swap_in(from: &Path, dest: &Path, works: impl Fn(&Path) -> bool) -> io::Result<()> {
    let dir = dest.parent().ok_or_else(|| io::Error::other("no folder"))?;
    let src = from.parent().ok_or_else(|| io::Error::other("no folder"))?;
    let _lock = retry(|| {
        std::os::windows::fs::OpenOptionsExt::share_mode(
            std::fs::OpenOptions::new().create(true).truncate(false).write(true),
            0,
        )
        .open(dir.join(".update.lock"))
    })?;
    for leftover in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        // Only what an update named: <release file>.<old|new|failed>-<pid>-<32 hex>.
        let name = leftover.file_name().to_string_lossy().to_ascii_lowercase();
        let tagged = |rest: &str| {
            rest.split_once('-').is_some_and(|(pid, hex)| {
                !pid.is_empty()
                    && pid.bytes().all(|b| b.is_ascii_digit())
                    && hex.len() == 32
                    && hex.bytes().all(|b| b.is_ascii_hexdigit())
            })
        };
        let ours = ["herdr2.exe", "h2.exe"]
            .iter()
            .flat_map(|f| ["old", "new", "failed"].map(|w| format!("{f}.{w}-")))
            .any(|prefix| name.strip_prefix(&prefix).is_some_and(tagged));
        if ours {
            let _ = std::fs::remove_file(leftover.path()); // still in use: next time
        }
    }
    let tag = format!("{}-{}", std::process::id(), crate::platform::random_hex());
    let aside = |p: &Path, what: &str| PathBuf::from(format!("{}.{what}-{tag}", p.display()));
    let exe = dir.join(RELEASE[0]);
    let h2 = dir.join("h2.exe");
    let linked = |a: &Path, b: &Path| matches!((std::fs::metadata(a), std::fs::metadata(b)), (Ok(a), Ok(b)) if file_identity(&a) == file_identity(&b));
    let h2_ours = linked(&h2, &exe);

    // (staged, final) pairs, all staged before anything moves.
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    let stage = || -> io::Result<Vec<(PathBuf, PathBuf)>> {
        let mut done = Vec::new();
        for rel in RELEASE {
            let (from, to) = (src.join(rel), dir.join(rel));
            if !from.is_file() {
                continue;
            }
            std::fs::create_dir_all(to.parent().unwrap())?;
            std::fs::copy(&from, aside(&to, "new"))?;
            done.push((aside(&to, "new"), to));
        }
        Ok(done)
    };
    let result = (|| {
        staged = stage()?;
        if !staged.first().is_some_and(|(s, _)| works(s)) {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        if h2_ours {
            std::fs::hard_link(&staged[0].0, aside(&h2, "new"))?;
            staged.push((aside(&h2, "new"), h2.clone()));
        }
        Ok(())
    })();
    let cleanup = |staged: &[(PathBuf, PathBuf)]| staged.iter().for_each(|(s, _)| drop(std::fs::remove_file(s)));
    if let Err(e) = result {
        cleanup(&staged);
        return Err(e);
    }

    // (final, moved-aside old) for each file swapped so far.
    let mut swapped: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for (new, to) in &staged {
        let old = to.exists().then(|| aside(to, "old"));
        let step = old.as_ref().map_or(Ok(()), |old| retry(|| std::fs::rename(to, old))).and_then(|_| {
            retry(|| std::fs::rename(new, to)).inspect_err(|_| {
                if let Some(old) = &old {
                    let _ = retry(|| std::fs::rename(old, to));
                }
            })
        });
        if let Err(e) = step {
            // Back as it was, with the same patience as going forward (a
            // scanner may be reading the file we just moved in). What can't
            // go back is said, and its old copy kept out of cleanup's reach.
            let mut stuck = Vec::new();
            for (to, old) in swapped.iter().rev() {
                let moved = retry(|| std::fs::rename(to, aside(to, "failed")));
                if let Some(old) = old
                    && (moved.is_err() || retry(|| std::fs::rename(old, to)).is_err())
                {
                    let keep = aside(to, "keep");
                    let _ = std::fs::rename(old, &keep);
                    stuck.push(format!("{} (the previous one is {})", to.display(), keep.display()));
                }
            }
            cleanup(&staged);
            if !stuck.is_empty() {
                return Err(io::Error::other(format!("{e}; and could not put back {}", stuck.join(", "))));
            }
            return Err(e);
        }
        swapped.push((to.clone(), old));
    }
    Ok(())
}

/// A file someone briefly holds (a scanner, an indexer): tried for 5 s.
fn retry<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let start = std::time::Instant::now();
    loop {
        match op() {
            // Access denied, sharing violation.
            Err(e)
                if matches!(e.raw_os_error(), Some(5 | 32)) && start.elapsed() < std::time::Duration::from_secs(5) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            other => return other,
        }
    }
}

pub fn file_identity(m: &std::fs::Metadata) -> String {
    use std::os::windows::fs::MetadataExt;
    format!("{}:{}:{}", m.creation_time(), m.last_write_time(), m.file_size())
}

// --- Processes -------------------------------------------------------------

/// Starts `cmd` with no console, out of the job of
/// the shell it was started from (ending that shell won't end it),
/// stdin/stdout to NUL and stderr to `log`. It inherits only those two
/// handles: std's spawn hands down every inheritable one, and a pipe of an
/// ssh session living on in the daemon keeps that session from ending.
pub fn spawn_detached(cmd: Command, log: File) -> io::Result<()> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    for (k, v) in cmd.get_envs() {
        let k = k.to_string_lossy().into_owned();
        match v {
            Some(v) => env.insert(k, v.to_string_lossy().into_owned()),
            None => env.remove(&k),
        };
    }
    let parts: Vec<String> =
        std::iter::once(cmd.get_program()).chain(cmd.get_args()).map(|a| quote(&a.to_string_lossy())).collect();
    let mut line = wide(parts.join(" ").as_ref());
    let mut block = env_block(&env);
    let nul = std::fs::OpenOptions::new().read(true).write(true).open("NUL")?;
    unsafe {
        let handles = [nul.as_raw_handle() as HANDLE, log.as_raw_handle() as HANDLE];
        for h in handles {
            check(SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT))?;
        }
        let mut n = 0usize;
        InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut n);
        let mut attrs = vec![0u8; n];
        let list = attrs.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        check(InitializeProcThreadAttributeList(list, 1, 0, &mut n))?;
        let set = UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            handles.as_ptr().cast(),
            size_of_val(&handles),
            null_mut(),
            null(),
        );
        let mut si: STARTUPINFOEXW = zeroed();
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = list;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = handles[0];
        si.StartupInfo.hStdOutput = handles[0];
        si.StartupInfo.hStdError = handles[1];
        let mut pi: PROCESS_INFORMATION = zeroed();
        let flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | DETACHED_PROCESS;
        let mut start = |flags| {
            CreateProcessW(
                null(),
                line.as_mut_ptr(),
                null(),
                null(),
                1,
                flags,
                block.as_mut_ptr().cast(),
                null(),
                &si.StartupInfo,
                &mut pi,
            ) != 0
        };
        let mut started = set != 0 && start(flags | CREATE_BREAKAWAY_FROM_JOB);
        // A job that won't let us leave. Under ssh, one that kills on close
        // ends every agent at logout: refuse. Elsewhere
        // (cargo, some launchers) stay in it and live as long as it does.
        if !started && set != 0 && GetLastError() == ERROR_ACCESS_DENIED {
            if std::env::var_os("SSH_CONNECTION").is_some() && job_kills_on_close() {
                DeleteProcThreadAttributeList(list);
                return Err(io::Error::other(
                    "this ssh session ends everything it started when you log out, and won't let h2 leave: start h2 once outside ssh (h2 server start in a local terminal), then ssh in",
                ));
            }
            started = start(flags);
        }
        let err = io::Error::last_os_error();
        DeleteProcThreadAttributeList(list);
        if !started {
            return Err(err);
        }
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
    }
    Ok(())
}

/// Whether the job we run in (if any) ends its processes when it closes.
fn job_kills_on_close() -> bool {
    unsafe {
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        let ok = QueryInformationJobObject(
            null_mut(),
            JobObjectExtendedLimitInformation,
            (&raw mut limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            null_mut(),
        );
        ok != 0 && limits.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE != 0
    }
}

pub fn parent_pid() -> u32 {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    unsafe {
        let me = GetCurrentProcessId();
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return 0;
        }
        let snap = OwnedHandle::from_raw_handle(snap as RawHandle);
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap.as_raw_handle() as HANDLE, &mut entry);
        while ok != 0 {
            if entry.th32ProcessID == me {
                return entry.th32ParentProcessID;
            }
            ok = Process32NextW(snap.as_raw_handle() as HANDLE, &mut entry);
        }
        0
    }
}

pub fn exit_signal(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

pub fn random_bytes(bytes: &mut [u8]) {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
    let status =
        unsafe { BCryptGenRandom(null_mut(), bytes.as_mut_ptr(), bytes.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    assert_eq!(status, 0, "BCryptGenRandom failed");
}

/// SHA-256 as lowercase hex, by Windows (no sha256sum there).
pub fn sha256(bytes: &[u8]) -> Option<String> {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut out = [0u8; 32];
    let status = unsafe {
        BCryptHash(BCRYPT_SHA256_ALG_HANDLE, null(), 0, bytes.as_ptr(), bytes.len() as u32, out.as_mut_ptr(), 32)
    };
    (status == 0).then(|| out.iter().map(|b| format!("{b:02x}")).collect())
}

/// No SIGTERM: planned restarts come through the API.
pub async fn termination() {
    std::future::pending::<()>().await
}

pub fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "windows".into()).to_lowercase()
}

/// This system's own id (MachineGuid, new on a sysprepped clone). None when unreadable.
pub fn machine_id() -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("reg")
        .args(["query", r"HKLM\SOFTWARE\Microsoft\Cryptography", "/v", "MachineGuid"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = text.lines().find(|l| l.contains("MachineGuid"))?;
    line.split_whitespace().last().map(String::from)
}

/// Windows has no SIGTERM handoff; a requested restart is bounded by its expiry.
pub fn boot_id() -> io::Result<Option<String>> {
    Ok(None)
}

// --- Agent subprocesses ----------------------------------------------------

/// Prepares a headless ACP agent command with `.cmd`/`.bat` shim support and piped stdio.
pub fn agent_command(
    argv: &[String],
    env: &BTreeMap<String, String>,
    cwd: &str,
) -> io::Result<tokio::process::Command> {
    let program = argv.first().ok_or_else(|| io::Error::other("empty argv"))?;
    let path = env.iter().find(|(k, _)| k.eq_ignore_ascii_case("PATH")).map(|(_, v)| v.clone()).unwrap_or_default();
    let exts = env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("PATHEXT"))
        .map_or(".COM;.EXE;.BAT;.CMD".into(), |(_, v)| v.clone());
    let found = resolve(program, &path, &exts)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{program}: not found on PATH")))?;
    let ext = found.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let mut cmd = if ext == "cmd" || ext == "bat" {
        let mut c = tokio::process::Command::new("cmd.exe");
        c.args(["/e:ON", "/v:OFF", "/d", "/c"]).raw_arg(batch_tail(&found, &argv[1..])?);
        c
    } else if super::is_shell_line(argv) {
        let mut c = tokio::process::Command::new(&found);
        c.args(["/d", "/s", "/c"]).raw_arg(format!("\"{}\"", argv[4]));
        c
    } else {
        let mut c = tokio::process::Command::new(&found);
        c.args(&argv[1..]);
        c
    };
    cmd.current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .kill_on_drop(true);
    Ok(cmd)
}

/// Lifetime job guard holding everything an ACP agent spawned.
pub struct AgentGuard(OwnedHandle);

pub fn attach_agent_guard(child: &tokio::process::Child) -> io::Result<AgentGuard> {
    let Some(process) = child.raw_handle() else {
        return Err(io::Error::other("agent exited before guard attachment"));
    };
    unsafe {
        let job = CreateJobObjectW(null(), null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle::from_raw_handle(job as RawHandle);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        check(SetInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ))?;
        check(AssignProcessToJobObject(job.as_raw_handle() as HANDLE, process as HANDLE))?;
        Ok(AgentGuard(job))
    }
}

impl AgentGuard {
    pub fn terminate(&self) {
        unsafe { TerminateJobObject(self.0.as_raw_handle() as HANDLE, 1) };
    }
}

pub fn signal_agent(_pid: u32, _signal: i32) {}

/// A .cmd/.bat (how npm installs agents) runs through cmd, which reads
/// `& | < > ^ %` itself: each argument is quoted, `"` doubled and `%`
/// defused, so a prompt arrives as one literal argument (the scheme Rust's
/// std uses since BatBadBut). cmd can't carry a line break in an argument.
fn batch_tail(script: &Path, args: &[String]) -> io::Result<String> {
    let mut line = format!("\"\"{}\"", script.display());
    for arg in args {
        if arg.contains(['\r', '\n', '\0']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is a batch file: it can't take an argument with a line break", script.display()),
            ));
        }
        let plain =
            |c: char| c.is_ascii_alphanumeric() || r"#$*+-./:?@\_".contains(c) || !c.is_ascii() && !c.is_control();
        let quoted = arg.is_empty() || arg.ends_with('\\') || !arg.chars().all(plain);
        line.push(' ');
        if quoted {
            line.push('"');
        }
        let mut slashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => slashes += 1,
                '"' => {
                    line.push_str(&"\\".repeat(slashes));
                    line.push('"'); // doubled
                    slashes = 0;
                }
                // `%cd:~,%` expands to nothing, so `%VAR%` can't pair up.
                '%' => {
                    line.push_str("%%cd:~,");
                    slashes = 0;
                }
                _ => slashes = 0,
            }
            line.push(c);
        }
        if quoted {
            line.push_str(&"\\".repeat(slashes));
            line.push('"');
        }
    }
    line.push('"');
    Ok(line)
}

fn resolve(program: &str, path: &str, exts: &str) -> Option<PathBuf> {
    let with_exts = |base: PathBuf| -> Option<PathBuf> {
        if base.extension().is_some() && base.is_file() {
            return Some(base);
        }
        exts.split(';')
            .filter(|e| !e.is_empty())
            .map(|e| PathBuf::from(format!("{}{e}", base.display())))
            .find(|p| p.is_file())
    };
    let p = Path::new(program);
    if p.components().count() > 1 || p.is_absolute() {
        return with_exts(p.to_path_buf());
    }
    std::env::split_paths(path).find_map(|dir| with_exts(dir.join(program)))
}

/// One argument as Windows' command line parsing reads it back.
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.into();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            c => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

fn env_block(env: &BTreeMap<String, String>) -> Vec<u16> {
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in env {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

// --- The console (connect) ---------------------------------------------------

pub mod console {
    use super::*;

    fn std(which: STD_HANDLE) -> HANDLE {
        unsafe { GetStdHandle(which) }
    }

    pub fn stdin() -> std::mem::ManuallyDrop<File> {
        std::mem::ManuallyDrop::new(unsafe { File::from_raw_handle(std(STD_INPUT_HANDLE) as RawHandle) })
    }

    pub fn stdout() -> std::mem::ManuallyDrop<File> {
        std::mem::ManuallyDrop::new(unsafe { File::from_raw_handle(std(STD_OUTPUT_HANDLE) as RawHandle) })
    }
}
