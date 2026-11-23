//! Process launch sandboxing for Drove workers across Linux, macOS, and Windows:
//!
//! - **Linux (`bwrap`)**: bind-mounts `<state>/drove` read-only, shadows `<state>/drove/secrets`
//!   and `<state>/secrets` with `--tmpfs`, re-binds writable worktrees inside `<state>/drove/worktrees`,
//!   and mounts `<cwd>` read-only for `RoleAccessMode::Readonly`.
//! - **macOS (`sandbox-exec`)**: runs workers under an inline Seatbelt (SBPL) profile that denies
//!   writes to `<state>/drove`, `drove.json`, and `ai.json` (re-allowing writable worktrees under
//!   `<state>/drove/worktrees`), denies writes to `<cwd>` for `RoleAccessMode::Readonly`, and denies
//!   all reads and writes to `<state>/drove/secrets` and `<state>/secrets`.
//! - **Windows (`restricted-token`)**: wraps worker execution through `herdr2 __drove-sandbox -- <argv>`,
//!   spawning the child under a Win32 Restricted Token (`CreateRestrictedToken` with
//!   `DISABLE_MAX_PRIVILEGE` + `NT AUTHORITY\RESTRICTED` `S-1-5-12` in `SidsToRestrict`) via
//!   `CreateProcessAsUserW` and a `KILL_ON_JOB_CLOSE` Job Object, paired with idempotent NTFS Deny
//!   ACEs for `S-1-5-12` blocking read/write on `<state>/drove/secrets` and `<state>/secrets` and
//!   blocking write on `<state>/drove`, `drove.json`, and `ai.json` (while protecting
//!   `<state>/drove/worktrees` from inheriting the write-deny ACE). User repository ACLs are never
//!   mutated; `Readonly` workspace edits on Windows continue to be guarded by `roles.rs` diff checks.

use super::config::DrovePaths;
use super::types::RoleAccessMode;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// Internal hidden subcommand used on Windows to launch a worker under a Restricted Token.
pub const WINDOWS_SANDBOX_SUBCOMMAND: &str = "__drove-sandbox";

const MACOS_SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

static BWRAP_USABLE: OnceLock<bool> = OnceLock::new();
static MACOS_SANDBOX_USABLE: OnceLock<bool> = OnceLock::new();
static WINDOWS_SANDBOX_USABLE: OnceLock<bool> = OnceLock::new();

/// Probes once whether Linux `bwrap` exists in `PATH` and user namespaces permit mounting.
pub fn bwrap_available() -> bool {
    *BWRAP_USABLE.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return false;
        }
        Command::new("bwrap")
            .args(["--dev-bind", "/", "/", "--", "true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Probes once whether macOS `sandbox-exec` (Seatbelt) is available and permits sandboxing.
pub fn macos_sandbox_available() -> bool {
    *MACOS_SANDBOX_USABLE.get_or_init(|| {
        if !cfg!(target_os = "macos") {
            return false;
        }
        let bin = macos_sandbox_exec_bin();
        Command::new(bin)
            .args(["-p", "(version 1)(allow default)", "--", "/usr/bin/true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Probes once whether Windows Restricted Token sandboxing (`CreateRestrictedToken` +
/// `CreateProcessAsUserW`) is available for this process.
pub fn windows_sandbox_available() -> bool {
    *WINDOWS_SANDBOX_USABLE.get_or_init(|| {
        #[cfg(windows)]
        {
            win_sandbox::probe_available()
        }
        #[cfg(not(windows))]
        {
            false
        }
    })
}

/// Returns `true` when the current host OS has a working Drove process sandbox backend.
pub fn sandbox_available() -> bool {
    sandbox_backend() != "none"
}

/// Returns the active process sandbox backend name for this host (`"bwrap"`, `"sandbox-exec"`,
/// `"restricted-token"`, or `"none"`).
pub fn sandbox_backend() -> &'static str {
    if cfg!(target_os = "linux") && bwrap_available() {
        "bwrap"
    } else if cfg!(target_os = "macos") && macos_sandbox_available() {
        "sandbox-exec"
    } else if cfg!(windows) && windows_sandbox_available() {
        "restricted-token"
    } else {
        "none"
    }
}

/// Wraps worker ACP argv with the host OS sandbox to isolate Drove/Core secrets (`ai.key`,
/// `coordinator.token`, `<state>/secrets`), protect Drove state/config from worker writes, and
/// enforce read-only workspace mounts when `access == RoleAccessMode::Readonly`.
pub fn wrap_worker_argv(
    argv: &[String],
    paths: &DrovePaths,
    cwd: &str,
    access: RoleAccessMode,
    isolate_secrets: bool,
) -> (Vec<String>, &'static str) {
    if argv.is_empty() || !isolate_secrets {
        return (argv.to_vec(), "none");
    }

    #[cfg(target_os = "linux")]
    {
        if !bwrap_available() {
            return (argv.to_vec(), "none");
        }
        let _ = paths.ensure_dirs();
        if let Some(wrapped) = build_bwrap_argv(argv, paths, cwd, access) {
            return (wrapped, "bwrap");
        }
        (argv.to_vec(), "none")
    }

    #[cfg(target_os = "macos")]
    {
        if !macos_sandbox_available() {
            return (argv.to_vec(), "none");
        }
        let _ = paths.ensure_dirs();
        if let Some(wrapped) = build_macos_sandbox_argv(macos_sandbox_exec_bin(), argv, paths, cwd, access) {
            return (wrapped, "sandbox-exec");
        }
        (argv.to_vec(), "none")
    }

    #[cfg(windows)]
    {
        let _ = (cwd, access);
        if !windows_sandbox_available() {
            return (argv.to_vec(), "none");
        }
        let _ = paths.ensure_dirs();
        if win_sandbox::ensure_sandbox_acls(paths).is_err() {
            return (argv.to_vec(), "none");
        }
        let Some(exe) = crate::client::exe_path().or_else(|| std::env::current_exe().ok()) else {
            return (argv.to_vec(), "none");
        };
        if let Some(wrapped) = build_windows_sandbox_argv(&exe, argv) {
            return (wrapped, "restricted-token");
        }
        (argv.to_vec(), "none")
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (paths, cwd, access);
        (argv.to_vec(), "none")
    }
}

/// Entrypoint for the internal `herdr2 __drove-sandbox` helper subcommand (Windows only).
pub fn run_windows_sandbox_entry(args: &[OsString]) -> i32 {
    #[cfg(windows)]
    {
        win_sandbox::run_entry(args)
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        eprintln!("error: {WINDOWS_SANDBOX_SUBCOMMAND} is only supported on Windows");
        1
    }
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn macos_sandbox_exec_bin() -> &'static str {
    if Path::new(MACOS_SANDBOX_EXEC_PATH).is_file() {
        MACOS_SANDBOX_EXEC_PATH
    } else {
        "sandbox-exec"
    }
}

/// Canonicalizes `p` if it exists; otherwise canonicalizes its nearest existing ancestor and
/// re-appends the remaining suffix components. This ensures symlinked prefixes (such as macOS
/// `/var` -> `/private/var` and `/tmp` -> `/private/tmp`) are always resolved even for paths that
/// have not been created yet.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos", test)), allow(dead_code))]
fn canonicalize_best_effort(p: &Path) -> PathBuf {
    if let Ok(canon) = p.canonicalize() {
        return canon;
    }
    let mut missing = Vec::new();
    let mut cur = p;
    while let Some(name) = cur.file_name() {
        missing.push(name.to_os_string());
        let Some(parent) = cur.parent() else {
            break;
        };
        cur = parent;
        if let Ok(mut canon) = cur.canonicalize() {
            for comp in missing.into_iter().rev() {
                canon.push(comp);
            }
            return canon;
        }
    }
    p.to_path_buf()
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos", test)), allow(dead_code))]
fn is_root_path(p: &Path) -> bool {
    p == Path::new("/") || (p.parent().is_none() && p.has_root())
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
fn build_bwrap_argv(
    argv: &[String],
    paths: &DrovePaths,
    cwd: &str,
    access: RoleAccessMode,
) -> Option<Vec<String>> {
    if argv.is_empty() {
        return None;
    }

    let state_dir = canonicalize_best_effort(&paths.state_dir);
    let secrets_dir = canonicalize_best_effort(&paths.secrets_dir);
    let core_secrets_dir = paths
        .state_dir
        .parent()
        .map(|p| canonicalize_best_effort(&p.join("secrets")));
    let cfg_file = canonicalize_best_effort(&paths.config_file);
    let ai_cfg_file = paths
        .config_file
        .parent()
        .map(|p| canonicalize_best_effort(&p.join("ai.json")));
    let cwd_path = canonicalize_best_effort(Path::new(cwd));

    // Avoid binding root `/` as tmpfs or read-only if something went wrong with paths.
    if is_root_path(&secrets_dir) || is_root_path(&state_dir) {
        return None;
    }

    let state_str = state_dir.display().to_string();
    let secrets_str = secrets_dir.display().to_string();
    let cfg_str = cfg_file.display().to_string();
    let cwd_str = cwd_path.display().to_string();

    let mut out = vec![
        "bwrap".to_string(),
        "--dev-bind".into(),
        "/".into(),
        "/".into(),
        "--ro-bind".into(),
        state_str.clone(),
        state_str,
    ];

    if cfg_file.exists() {
        out.extend(["--ro-bind".into(), cfg_str.clone(), cfg_str]);
    }
    if let Some(ai_cfg) = ai_cfg_file
        && ai_cfg.exists()
    {
        let s = ai_cfg.display().to_string();
        out.extend(["--ro-bind".into(), s.clone(), s]);
    }

    if !is_root_path(&cwd_path) && cwd_path.is_dir() {
        if access == RoleAccessMode::Readonly {
            out.extend(["--ro-bind".into(), cwd_str.clone(), cwd_str]);
        } else if cwd_path.starts_with(&state_dir) {
            // Worktrees live inside state_dir/worktrees; re-bind writable after state_dir --ro-bind.
            out.extend(["--bind".into(), cwd_str.clone(), cwd_str]);
        }
    }

    // Mask secrets last so neither --dev-bind / nor writable cwd can reveal coordinator/AI secrets.
    if let Some(core_sec) = core_secrets_dir
        && !is_root_path(&core_sec)
        && core_sec.is_dir()
    {
        out.extend(["--tmpfs".into(), core_sec.display().to_string()]);
    }
    out.extend([
        "--tmpfs".into(),
        secrets_str,
        "--unshare-pid".into(),
        "--proc".into(),
        "/proc".into(),
        "--die-with-parent".into(),
        "--".into(),
    ]);
    out.extend(argv.iter().cloned());
    Some(out)
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn sbpl_escape(path: &Path) -> Option<String> {
    let raw = path.to_str()?;
    let trimmed = if raw == "/" { raw } else { raw.trim_end_matches('/') };
    if trimmed.is_empty() || trimmed.bytes().any(|b| b == 0 || b == b'\n' || b == b'\r') {
        return None;
    }
    let mut out = String::with_capacity(trimmed.len() + 2);
    out.push('"');
    for ch in trimmed.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out.push('"');
    Some(out)
}

/// Builds a macOS Seatbelt (SBPL) profile matching the Linux `bwrap` isolation policy.
/// In Seatbelt, later rules override earlier rules ("last match wins").
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn build_macos_sbpl_profile(paths: &DrovePaths, cwd: &str, access: RoleAccessMode) -> Option<String> {
    let state_dir = canonicalize_best_effort(&paths.state_dir);
    let secrets_dir = canonicalize_best_effort(&paths.secrets_dir);
    let core_secrets_dir = paths
        .state_dir
        .parent()
        .map(|p| canonicalize_best_effort(&p.join("secrets")));
    let cfg_file = canonicalize_best_effort(&paths.config_file);
    let ai_cfg_file = paths
        .config_file
        .parent()
        .map(|p| canonicalize_best_effort(&p.join("ai.json")));
    let cwd_path = canonicalize_best_effort(Path::new(cwd));

    if is_root_path(&secrets_dir) || is_root_path(&state_dir) {
        return None;
    }

    let state_q = sbpl_escape(&state_dir)?;
    let secrets_q = sbpl_escape(&secrets_dir)?;
    let cfg_q = sbpl_escape(&cfg_file)?;

    let mut rules = vec![
        "(version 1)".to_string(),
        "(allow default)".to_string(),
        format!("(deny file-write* (subpath {state_q}))"),
        format!("(deny file-write* (literal {cfg_q}))"),
    ];

    if let Some(ai_cfg) = ai_cfg_file
        && let Some(ai_q) = sbpl_escape(&ai_cfg)
    {
        rules.push(format!("(deny file-write* (literal {ai_q}))"));
    }

    if !is_root_path(&cwd_path) && cwd_path.is_dir() {
        let cwd_q = sbpl_escape(&cwd_path)?;
        if access == RoleAccessMode::Readonly {
            rules.push(format!("(deny file-write* (subpath {cwd_q}))"));
        } else if cwd_path.starts_with(&state_dir) {
            // Worktrees live inside state_dir/worktrees; re-allow writes after denying state_dir.
            rules.push(format!("(allow file-write* (subpath {cwd_q}))"));
        }
    }

    // Secrets are denied last so neither `(allow default)` nor a writable worktree can reveal them.
    if let Some(core_sec) = core_secrets_dir
        && !is_root_path(&core_sec)
        && let Some(core_q) = sbpl_escape(&core_sec)
    {
        rules.push(format!("(deny file-read* file-write* (subpath {core_q}))"));
    }
    rules.push(format!("(deny file-read* file-write* (subpath {secrets_q}))"));

    Some(rules.join("\n"))
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn build_macos_sandbox_argv(
    bin: &str,
    argv: &[String],
    paths: &DrovePaths,
    cwd: &str,
    access: RoleAccessMode,
) -> Option<Vec<String>> {
    if argv.is_empty() {
        return None;
    }
    let profile = build_macos_sbpl_profile(paths, cwd, access)?;
    let mut out = Vec::with_capacity(argv.len() + 4);
    out.push(bin.to_string());
    out.push("-p".into());
    out.push(profile);
    out.push("--".into());
    out.extend(argv.iter().cloned());
    Some(out)
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn build_windows_sandbox_argv(exe: &Path, argv: &[String]) -> Option<Vec<String>> {
    if argv.is_empty() {
        return None;
    }
    let exe_str = exe.to_str()?;
    if exe_str.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(argv.len() + 3);
    out.push(exe_str.to_string());
    out.push(WINDOWS_SANDBOX_SUBCOMMAND.to_string());
    out.push("--".to_string());
    out.extend(argv.iter().cloned());
    Some(out)
}

#[cfg(windows)]
mod win_sandbox {
    use super::DrovePaths;
    use std::collections::BTreeMap;
    use std::ffi::{OsString, c_void};
    use std::io;
    use std::mem::{size_of, zeroed};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::path::Path;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Security::Authorization::*;
    use windows_sys::Win32::Security::*;
    use windows_sys::Win32::Storage::FileSystem::*;
    use windows_sys::Win32::System::Console::*;
    use windows_sys::Win32::System::JobObjects::*;
    use windows_sys::Win32::System::Threading::*;

    const PROBE_CHILD_ARG: &str = "--probe-child";

    // Standard Win32 constants from winnt.h (kept inline to avoid pulling Win32_System_SystemServices).
    const ACCESS_DENIED_ACE_TYPE: u8 = 1;
    const SE_GROUP_ENABLED: u32 = 0x0000_0004;
    const SE_GROUP_USE_FOR_DENY_ONLY: u32 = 0x0000_0010;
    const SE_GROUP_INTEGRITY: u32 = 0x0000_0020;
    const SE_GROUP_INTEGRITY_ENABLED: u32 = 0x0000_0040;

    /// Strictly write/mutation bits — intentionally excludes `SYNCHRONIZE` and `READ_CONTROL`
    /// (which are included in `FILE_GENERIC_WRITE` and also required by `FILE_GENERIC_READ` /
    /// `FILE_GENERIC_EXECUTE`).
    const WRITE_DENY_MASK: u32 = FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_WRITE_EA
        | FILE_DELETE_CHILD
        | FILE_WRITE_ATTRIBUTES
        | DELETE
        | WRITE_DAC
        | WRITE_OWNER;

    const SECRET_DENY_MASK: u32 = FILE_ALL_ACCESS;

    fn restricted_code_sid() -> io::Result<Vec<u8>> {
        let mut size = SECURITY_MAX_SID_SIZE;
        let mut buf = vec![0u8; size as usize];
        let ok = unsafe {
            CreateWellKnownSid(WinRestrictedCodeSid, null_mut(), buf.as_mut_ptr().cast(), &mut size)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        buf.truncate(size as usize);
        Ok(buf)
    }

    fn query_token_info(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<usize>> {
        let mut needed = 0u32;
        unsafe {
            GetTokenInformation(token, class, null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
        let ok = unsafe {
            GetTokenInformation(token, class, buf.as_mut_ptr().cast(), needed, &mut needed)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(buf)
    }

    /// Creates a restricted primary token derived from the current process token:
    /// - Strips privileges via `DISABLE_MAX_PRIVILEGE` (keeps `SeChangeNotifyPrivilege`).
    /// - Sets `SidsToRestrict` to `[TokenUser, ...enabled non-deny/non-integrity groups..., S-1-5-12]`.
    ///   Normal paths remain accessible via `TokenUser` + enabled groups, while any NTFS object
    ///   carrying an explicit `DENY(S-1-5-12)` ACE is rejected by the kernel in Pass 2 of
    ///   `SeAccessCheck` for this worker and all of its descendant processes.
    fn create_worker_restricted_token() -> io::Result<OwnedHandle> {
        unsafe {
            let mut raw_token: HANDLE = null_mut();
            if OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY,
                &mut raw_token,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let token = OwnedHandle::from_raw_handle(raw_token as RawHandle);
            let h_token = token.as_raw_handle() as HANDLE;

            let user_buf = query_token_info(h_token, TokenUser)?;
            let user_sid = (*user_buf.as_ptr().cast::<TOKEN_USER>()).User.Sid;

            let groups_buf = query_token_info(h_token, TokenGroups)?;
            let groups_ptr = groups_buf.as_ptr().cast::<TOKEN_GROUPS>();
            let group_count = (*groups_ptr).GroupCount as usize;
            let groups_slice = std::slice::from_raw_parts((*groups_ptr).Groups.as_ptr(), group_count);

            let mut restricted_sid_buf = restricted_code_sid()?;
            let restricted_sid: PSID = restricted_sid_buf.as_mut_ptr().cast();

            let mut sids_to_restrict: Vec<SID_AND_ATTRIBUTES> = Vec::with_capacity(group_count + 2);
            let push_unique = |list: &mut Vec<SID_AND_ATTRIBUTES>, sid: PSID| {
                if sid.is_null() {
                    return;
                }
                if list.iter().any(|existing| EqualSid(existing.Sid, sid) != 0) {
                    return;
                }
                // CreateRestrictedToken requires Attributes == 0 on all SidsToRestrict entries.
                list.push(SID_AND_ATTRIBUTES { Sid: sid, Attributes: 0 });
            };

            push_unique(&mut sids_to_restrict, user_sid);
            for g in groups_slice {
                let attrs = g.Attributes;
                let enabled = (attrs & SE_GROUP_ENABLED) != 0;
                let deny_only = (attrs & SE_GROUP_USE_FOR_DENY_ONLY) != 0;
                let integrity = (attrs & (SE_GROUP_INTEGRITY | SE_GROUP_INTEGRITY_ENABLED)) != 0;
                if enabled && !deny_only && !integrity {
                    push_unique(&mut sids_to_restrict, g.Sid);
                }
            }
            push_unique(&mut sids_to_restrict, restricted_sid);

            let mut new_token: HANDLE = null_mut();
            let ok = CreateRestrictedToken(
                h_token,
                DISABLE_MAX_PRIVILEGE,
                0,
                null(),
                0,
                null(),
                sids_to_restrict.len() as u32,
                sids_to_restrict.as_ptr(),
                &mut new_token,
            );
            // Keep buffers alive until after CreateRestrictedToken returns.
            drop(user_buf);
            drop(groups_buf);
            drop(restricted_sid_buf);
            if ok == 0 || new_token.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok(OwnedHandle::from_raw_handle(new_token as RawHandle))
        }
    }

    unsafe fn dacl_has_deny_for_sid(
        dacl: *const ACL,
        sid: PSID,
        required_mask: u32,
        required_inherit: u32,
        explicit_only: bool,
    ) -> bool {
        if dacl.is_null() {
            return false;
        }
        unsafe {
            let mut info: ACL_SIZE_INFORMATION = zeroed();
            if GetAclInformation(
                dacl,
                (&raw mut info).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            ) == 0
            {
                return false;
            }
            for idx in 0..info.AceCount {
                let mut ace_ptr: *mut c_void = null_mut();
                if GetAce(dacl, idx, &mut ace_ptr) == 0 || ace_ptr.is_null() {
                    continue;
                }
                let header = *(ace_ptr as *const ACE_HEADER);
                if header.AceType != ACCESS_DENIED_ACE_TYPE {
                    continue;
                }
                if explicit_only && (header.AceFlags as u32 & INHERITED_ACE) != 0 {
                    continue;
                }
                let ace = &*(ace_ptr as *const ACCESS_DENIED_ACE);
                let ace_sid = (&raw const ace.SidStart) as PSID;
                if EqualSid(ace_sid, sid) != 0
                    && (ace.Mask & required_mask) == required_mask
                    && (header.AceFlags as u32 & required_inherit) == required_inherit
                {
                    return true;
                }
            }
            false
        }
    }

    /// Marks `<state>/drove/worktrees` with a protected DACL copying all non-`S-1-5-12`-deny ACEs
    /// as explicit entries so worktrees stay writable and never inherit the parent `<state>/drove`
    /// write-deny ACE.
    fn protect_worktrees_dacl(path: &Path, restricted_sid: PSID) -> io::Result<()> {
        let wide = crate::platform::wide(path.as_os_str());
        unsafe {
            let mut old_dacl: *mut ACL = null_mut();
            let mut sd: PSECURITY_DESCRIPTOR = null_mut();
            let err = GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut old_dacl,
                null_mut(),
                &mut sd,
            );
            if err != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(err as i32));
            }
            let _sd_guard = LocalFreeGuard(sd);
            if old_dacl.is_null() {
                return Err(io::Error::other("filesystem does not support NTFS DACLs"));
            }

            let mut control: u16 = 0;
            let mut rev: u32 = 0;
            if GetSecurityDescriptorControl(sd, &mut control, &mut rev) != 0
                && (control & SE_DACL_PROTECTED) != 0
                && !dacl_has_deny_for_sid(old_dacl, restricted_sid, 0, 0, false)
            {
                return Ok(());
            }

            let mut info: ACL_SIZE_INFORMATION = zeroed();
            if GetAclInformation(
                old_dacl,
                (&raw mut info).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }

            let acl_bytes = (info.AclBytesInUse as usize).max(size_of::<ACL>()) + 64;
            let mut new_acl_words = vec![0u64; acl_bytes.div_ceil(size_of::<u64>())];
            let new_acl_ptr = new_acl_words.as_mut_ptr().cast::<ACL>();
            if InitializeAcl(new_acl_ptr, (new_acl_words.len() * size_of::<u64>()) as u32, ACL_REVISION) == 0 {
                return Err(io::Error::last_os_error());
            }

            for idx in 0..info.AceCount {
                let mut ace_ptr: *mut c_void = null_mut();
                if GetAce(old_dacl, idx, &mut ace_ptr) == 0 || ace_ptr.is_null() {
                    continue;
                }
                let header = *(ace_ptr as *const ACE_HEADER);
                let ace_size = header.AceSize as usize;
                if ace_size < size_of::<ACE_HEADER>() {
                    continue;
                }
                if header.AceType == ACCESS_DENIED_ACE_TYPE {
                    let ace = &*(ace_ptr as *const ACCESS_DENIED_ACE);
                    let ace_sid = (&raw const ace.SidStart) as PSID;
                    if EqualSid(ace_sid, restricted_sid) != 0 {
                        continue;
                    }
                }
                let mut ace_copy = vec![0u8; ace_size];
                std::ptr::copy_nonoverlapping(ace_ptr.cast::<u8>(), ace_copy.as_mut_ptr(), ace_size);
                // Convert inherited ACEs to explicit ACEs since we protect the DACL from parent inheritance.
                let copy_hdr = &mut *(ace_copy.as_mut_ptr().cast::<ACE_HEADER>());
                copy_hdr.AceFlags &= !(INHERITED_ACE as u8);
                if AddAce(new_acl_ptr, ACL_REVISION, u32::MAX, ace_copy.as_ptr().cast(), ace_size as u32) == 0 {
                    return Err(io::Error::last_os_error());
                }
            }

            let set_err = SetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                new_acl_ptr,
                null(),
            );
            if set_err != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(set_err as i32));
            }
            Ok(())
        }
    }

    fn ensure_deny_ace(
        path: &Path,
        restricted_sid: PSID,
        deny_mask: u32,
        inheritance: u32,
    ) -> io::Result<()> {
        let wide = crate::platform::wide(path.as_os_str());
        unsafe {
            let mut old_dacl: *mut ACL = null_mut();
            let mut sd: PSECURITY_DESCRIPTOR = null_mut();
            let err = GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut old_dacl,
                null_mut(),
                &mut sd,
            );
            if err != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(err as i32));
            }
            let _sd_guard = LocalFreeGuard(sd);
            if old_dacl.is_null() {
                return Err(io::Error::other("filesystem does not support NTFS DACLs"));
            }

            if dacl_has_deny_for_sid(old_dacl, restricted_sid, deny_mask, inheritance, true) {
                return Ok(());
            }

            let ea = EXPLICIT_ACCESS_W {
                grfAccessPermissions: deny_mask,
                grfAccessMode: DENY_ACCESS,
                grfInheritance: inheritance,
                Trustee: TRUSTEE_W {
                    pMultipleTrustee: null_mut(),
                    MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                    TrusteeForm: TRUSTEE_IS_SID,
                    TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                    ptstrName: restricted_sid.cast(),
                },
            };
            let mut new_dacl: *mut ACL = null_mut();
            let acl_err = SetEntriesInAclW(1, &ea, old_dacl, &mut new_dacl);
            if acl_err != ERROR_SUCCESS || new_dacl.is_null() {
                return Err(io::Error::from_raw_os_error(acl_err as i32));
            }
            let _acl_guard = LocalFreeGuard(new_dacl.cast());

            let set_err = SetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                new_dacl,
                null(),
            );
            if set_err != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(set_err as i32));
            }
            Ok(())
        }
    }

    struct LocalFreeGuard(*mut c_void);

    impl Drop for LocalFreeGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    LocalFree(self.0);
                }
            }
        }
    }

    pub(super) fn ensure_sandbox_acls(paths: &DrovePaths) -> io::Result<()> {
        let mut sid_buf = restricted_code_sid()?;
        let restricted_sid: PSID = sid_buf.as_mut_ptr().cast();
        let inherit_all = OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE;

        // Protect worktrees BEFORE applying write-deny to state_dir so inheritance stops here.
        let worktrees_dir = paths.state_dir.join("worktrees");
        std::fs::create_dir_all(&worktrees_dir)?;
        protect_worktrees_dacl(&worktrees_dir, restricted_sid)?;

        // State dir: read/execute ok (for bin/h2.exe & state inspection), writes denied.
        ensure_deny_ace(&paths.state_dir, restricted_sid, WRITE_DENY_MASK, inherit_all)?;

        // Secrets dir: all read/write access denied.
        ensure_deny_ace(&paths.secrets_dir, restricted_sid, SECRET_DENY_MASK, inherit_all)?;

        // Core AI secrets dir (<state>/secrets): all read/write access denied if present.
        if let Some(core_secrets) = paths.state_dir.parent().map(|p| p.join("secrets"))
            && core_secrets.is_dir()
        {
            let _ = ensure_deny_ace(&core_secrets, restricted_sid, SECRET_DENY_MASK, inherit_all);
        }

        // Config files (drove.json, ai.json): read ok, writes denied if present.
        if paths.config_file.is_file() {
            let _ = ensure_deny_ace(&paths.config_file, restricted_sid, WRITE_DENY_MASK, NO_INHERITANCE);
        }
        if let Some(ai_cfg) = paths.config_file.parent().map(|p| p.join("ai.json"))
            && ai_cfg.is_file()
        {
            let _ = ensure_deny_ace(&ai_cfg, restricted_sid, WRITE_DENY_MASK, NO_INHERITANCE);
        }

        Ok(())
    }

    pub(super) fn probe_available() -> bool {
        let Some(exe) = crate::client::exe_path().or_else(|| std::env::current_exe().ok()) else {
            return false;
        };
        let Ok(token) = create_worker_restricted_token() else {
            return false;
        };
        let line_str = format!(
            "{} {} {}",
            crate::platform::quote(&exe.display().to_string()),
            super::WINDOWS_SANDBOX_SUBCOMMAND,
            PROBE_CHILD_ARG
        );
        let mut cmd_line = crate::platform::wide(line_str.as_ref());
        unsafe {
            let mut si: STARTUPINFOW = zeroed();
            si.cb = size_of::<STARTUPINFOW>() as u32;
            si.dwFlags = STARTF_USESTDHANDLES;
            si.hStdInput = INVALID_HANDLE_VALUE;
            si.hStdOutput = INVALID_HANDLE_VALUE;
            si.hStdError = INVALID_HANDLE_VALUE;
            let mut pi: PROCESS_INFORMATION = zeroed();
            let ok = CreateProcessAsUserW(
                token.as_raw_handle() as HANDLE,
                null(),
                cmd_line.as_mut_ptr(),
                null(),
                null(),
                0,
                CREATE_NO_WINDOW,
                null(),
                null(),
                &si,
                &mut pi,
            );
            if ok == 0 {
                return false;
            }
            let proc_handle = OwnedHandle::from_raw_handle(pi.hProcess as RawHandle);
            CloseHandle(pi.hThread);
            let waited = WaitForSingleObject(proc_handle.as_raw_handle() as HANDLE, 5_000);
            if waited != WAIT_OBJECT_0 {
                let _ = TerminateProcess(proc_handle.as_raw_handle() as HANDLE, 1);
                return false;
            }
            let mut code = 1u32;
            GetExitCodeProcess(proc_handle.as_raw_handle() as HANDLE, &mut code) != 0 && code == 0
        }
    }

    pub(super) fn run_entry(args: &[OsString]) -> i32 {
        if args.first().and_then(|a| a.to_str()) == Some(PROBE_CHILD_ARG) {
            return 0;
        }
        let inner_os = match args.iter().position(|a| a == "--") {
            Some(sep) => &args[sep + 1..],
            None => args,
        };
        if inner_os.is_empty() {
            eprintln!("error: missing command after --");
            return 1;
        }
        let inner_argv: Vec<String> = inner_os
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        match spawn_restricted_and_wait(&inner_argv) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("error: windows drove sandbox launch failed: {err}");
                1
            }
        }
    }

    fn spawn_restricted_and_wait(inner_argv: &[String]) -> io::Result<i32> {
        let env: BTreeMap<String, String> = crate::platform::utf8_vars().collect();
        let mut cmd_line = crate::platform::command_line(inner_argv, &env)?;
        let token = create_worker_restricted_token()?;

        unsafe {
            let h_in = GetStdHandle(STD_INPUT_HANDLE);
            let h_out = GetStdHandle(STD_OUTPUT_HANDLE);
            let h_err = GetStdHandle(STD_ERROR_HANDLE);

            let mut handles: Vec<HANDLE> = Vec::with_capacity(3);
            for h in [h_in, h_out, h_err] {
                if !h.is_null() && h != INVALID_HANDLE_VALUE && !handles.contains(&h) {
                    let _ = SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
                    handles.push(h);
                }
            }

            let mut pi: PROCESS_INFORMATION = zeroed();
            let started = if !handles.is_empty() {
                let mut attr_size = 0usize;
                InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut attr_size);
                let mut attr_buf = vec![0u8; attr_size];
                let attr_list = attr_buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
                if InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_size) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let updated = UpdateProcThreadAttribute(
                    attr_list,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    handles.as_ptr().cast(),
                    handles.len() * size_of::<HANDLE>(),
                    null_mut(),
                    null(),
                );
                let mut si: STARTUPINFOEXW = zeroed();
                si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
                si.lpAttributeList = attr_list;
                si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
                si.StartupInfo.hStdInput = h_in;
                si.StartupInfo.hStdOutput = h_out;
                si.StartupInfo.hStdError = h_err;
                let ok = updated != 0
                    && CreateProcessAsUserW(
                        token.as_raw_handle() as HANDLE,
                        null(),
                        cmd_line.as_mut_ptr(),
                        null(),
                        null(),
                        1,
                        EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW | CREATE_SUSPENDED,
                        null(),
                        null(),
                        &si.StartupInfo,
                        &mut pi,
                    ) != 0;
                let err = io::Error::last_os_error();
                DeleteProcThreadAttributeList(attr_list);
                if !ok {
                    return Err(err);
                }
                true
            } else {
                let mut si: STARTUPINFOW = zeroed();
                si.cb = size_of::<STARTUPINFOW>() as u32;
                CreateProcessAsUserW(
                    token.as_raw_handle() as HANDLE,
                    null(),
                    cmd_line.as_mut_ptr(),
                    null(),
                    null(),
                    0,
                    CREATE_NO_WINDOW | CREATE_SUSPENDED,
                    null(),
                    null(),
                    &si,
                    &mut pi,
                ) != 0
            };

            if !started {
                return Err(io::Error::last_os_error());
            }

            let proc_handle = OwnedHandle::from_raw_handle(pi.hProcess as RawHandle);
            let thread_handle = OwnedHandle::from_raw_handle(pi.hThread as RawHandle);

            // Best-effort local KILL_ON_JOB_CLOSE job so the child dies if this wrapper dies.
            let _job = attach_kill_on_close_job(proc_handle.as_raw_handle() as HANDLE);

            ResumeThread(thread_handle.as_raw_handle() as HANDLE);
            drop(thread_handle);

            WaitForSingleObject(proc_handle.as_raw_handle() as HANDLE, INFINITE);
            let mut exit_code = 0u32;
            if GetExitCodeProcess(proc_handle.as_raw_handle() as HANDLE, &mut exit_code) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(exit_code as i32)
        }
    }

    unsafe fn attach_kill_on_close_job(process: HANDLE) -> Option<OwnedHandle> {
        unsafe {
            let job = CreateJobObjectW(null(), null());
            if job.is_null() {
                return None;
            }
            let job = OwnedHandle::from_raw_handle(job as RawHandle);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            limits.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
            let set = SetInformationJobObject(
                job.as_raw_handle() as HANDLE,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            (set != 0 && AssignProcessToJobObject(job.as_raw_handle() as HANDLE, process) != 0).then_some(job)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;

    fn test_paths(base: &Path) -> DrovePaths {
        let p = Paths {
            socket: base.join("s.sock"),
            holder_socket: base.join("h.sock"),
            state: base.join("state"),
            config: base.join("config/config.toml"),
        };
        DrovePaths::from_paths(&p)
    }

    #[test]
    fn wrap_falls_back_cleanly_when_isolation_disabled() {
        let base = std::env::temp_dir().join("drove-test-sb-disabled");
        let paths = test_paths(&base);
        let argv = vec!["python3".into(), "agent.py".into()];
        let (out, mode) = wrap_worker_argv(&argv, &paths, "/tmp", RoleAccessMode::Writable, false);
        assert_eq!(out, argv);
        assert_eq!(mode, "none");
    }

    #[test]
    fn sbpl_escape_handles_spaces_quotes_backslashes_and_trailing_slashes() {
        assert_eq!(sbpl_escape(Path::new("/Users/dev/my project/")), Some("\"/Users/dev/my project\"".into()));
        assert_eq!(sbpl_escape(Path::new("/Users/a\"b\\c")), Some("\"/Users/a\\\"b\\\\c\"".into()));
        assert_eq!(sbpl_escape(Path::new("/")), Some("\"/\"".into()));
        assert_eq!(sbpl_escape(Path::new("/bad\npath")), None);
        assert_eq!(sbpl_escape(Path::new("")), None);
    }

    #[test]
    fn macos_sbpl_profile_isolates_secrets_state_readonly_cwd_and_allows_worktree() {
        let root = std::env::temp_dir().join(format!(
            "drove-test-sb-mac-{}-{}",
            std::process::id(),
            crate::platform::random_hex()
        ));
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        let core_secrets = root.join("state/secrets");
        std::fs::create_dir_all(&core_secrets).unwrap();
        let project_dir = root.join("project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let worktree_dir = paths.state_dir.join("worktrees/run-1/wt-1");
        std::fs::create_dir_all(&worktree_dir).unwrap();

        let state_canon = canonicalize_best_effort(&paths.state_dir);
        let secrets_canon = canonicalize_best_effort(&paths.secrets_dir);
        let core_secrets_canon = canonicalize_best_effort(&core_secrets);
        let project_canon = canonicalize_best_effort(&project_dir);
        let worktree_canon = canonicalize_best_effort(&worktree_dir);

        // 1. Readonly role on normal project dir denies writes to state, config, project, and denies r/w to secrets.
        let ro = build_macos_sbpl_profile(&paths, project_dir.to_str().unwrap(), RoleAccessMode::Readonly).unwrap();
        assert!(ro.starts_with("(version 1)\n(allow default)\n"));
        assert!(ro.contains(&format!("(deny file-write* (subpath \"{}\"))", state_canon.display())));
        assert!(ro.contains(&format!("(deny file-write* (subpath \"{}\"))", project_canon.display())));
        let sec_rule = format!("(deny file-read* file-write* (subpath \"{}\"))", secrets_canon.display());
        let core_sec_rule = format!("(deny file-read* file-write* (subpath \"{}\"))", core_secrets_canon.display());
        assert!(ro.contains(&core_sec_rule));
        assert!(ro.ends_with(&sec_rule));

        // 2. Writable role inside state_dir/worktrees re-allows worktree writes AFTER denying state_dir,
        //    and STILL denies secrets AFTER re-allowing the worktree.
        let wt = build_macos_sbpl_profile(&paths, worktree_dir.to_str().unwrap(), RoleAccessMode::Writable).unwrap();
        let state_deny_pos = wt.find(&format!("(deny file-write* (subpath \"{}\"))", state_canon.display())).unwrap();
        let wt_allow_pos = wt.find(&format!("(allow file-write* (subpath \"{}\"))", worktree_canon.display())).unwrap();
        let sec_deny_pos = wt.find(&sec_rule).unwrap();
        assert!(state_deny_pos < wt_allow_pos, "worktree allow must follow state_dir deny");
        assert!(wt_allow_pos < sec_deny_pos, "secrets deny must follow worktree allow");

        // 3. Wrapped argv places `--` right before inner command so preset detectors work.
        let argv = vec!["agy".into(), "acp".into()];
        let wrapped = build_macos_sandbox_argv(
            MACOS_SANDBOX_EXEC_PATH,
            &argv,
            &paths,
            project_dir.to_str().unwrap(),
            RoleAccessMode::Readonly,
        )
        .unwrap();
        assert_eq!(wrapped[0], MACOS_SANDBOX_EXEC_PATH);
        assert_eq!(wrapped[1], "-p");
        assert_eq!(wrapped[3], "--");
        assert_eq!(&wrapped[4..], &["agy", "acp"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn windows_sandbox_argv_uses_internal_subcommand_and_separator() {
        let exe = Path::new(r"C:\Users\dev\bin\herdr2.exe");
        let argv = vec!["codex-acp".into(), "--stdio".into()];
        let wrapped = build_windows_sandbox_argv(exe, &argv).unwrap();
        assert_eq!(
            wrapped,
            vec![
                r"C:\Users\dev\bin\herdr2.exe",
                WINDOWS_SANDBOX_SUBCOMMAND,
                "--",
                "codex-acp",
                "--stdio",
            ]
        );
    }

    #[test]
    fn bwrap_argv_does_not_false_match_sibling_dir_prefix() {
        let root = std::env::temp_dir().join(format!(
            "drove-test-sb-bwrap-{}-{}",
            std::process::id(),
            crate::platform::random_hex()
        ));
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        // Sibling directory whose string prefix matches `<state>/drove` (".../drove2")
        let sibling = root.join("state/drove2");
        std::fs::create_dir_all(&sibling).unwrap();

        let argv = vec!["claude-agent-acp".into()];
        let wrapped = build_bwrap_argv(&argv, &paths, sibling.to_str().unwrap(), RoleAccessMode::Writable).unwrap();
        // Should NOT emit `--bind <sibling> <sibling>` because it is not a path child of state_dir.
        assert!(!wrapped.iter().any(|a| a == "--bind"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
