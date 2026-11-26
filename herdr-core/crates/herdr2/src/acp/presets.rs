//! Built-in ACP agent presets, command word splitting, and IDE shim detection.

use crate::rpc::RpcError;
use std::collections::BTreeMap;
use std::path::Path;

pub const BUILTIN_PRESETS: &[(&str, &[&str])] = &[
    ("agy", &["agy", "acp"]),
    ("antigravity", &["agy", "acp"]),
    ("claude", &["claude-agent-acp"]),
    ("codex", &["codex-acp"]),
    ("gemini", &["gemini", "--acp"]),
    ("opencode", &["opencode", "acp"]),
    ("copilot", &["copilot", "--acp", "--stdio"]),
    ("cursor", &["cursor-agent", "acp"]),
    ("cursor-agent", &["cursor-agent", "acp"]),
    ("dsh", &["dsh", "--profile", "acp"]),
    ("kimi", &["kimi", "acp"]),
    ("qwen", &["qwen", "--acp"]),
    ("pi", &["pi-acp"]),
    ("hermes", &["hermes-acp"]),
    ("glm", &["glm-acp-agent"]),
    ("zcode", &["zcode-acp-server"]),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgent {
    pub preset: Option<String>,
    pub argv: Vec<String>,
    pub prompt: Option<String>,
}

pub fn is_claude_agent(preset: Option<&str>, argv: &[String]) -> bool {
    if preset == Some("claude") {
        return true;
    }
    if wrapped_cmd_index(argv)
        .and_then(|idx| argv.get(idx))
        .and_then(|p| Path::new(p).file_stem())
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.to_ascii_lowercase().starts_with("claude"))
    {
        return true;
    }
    argv.iter().any(|arg| arg.to_ascii_lowercase().contains("claude-agent-acp"))
}

fn is_agy_binary(program: &str) -> bool {
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program).to_ascii_lowercase();
    matches!(base.as_str(), "agy" | "agy.real" | "agy.exe" | "agy.cmd" | "agy.bat" | "agy.ps1")
}

fn is_cursor_binary(program: &str) -> bool {
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program).to_ascii_lowercase();
    matches!(
        base.as_str(),
        "cursor-agent" | "cursor-agent.exe" | "cursor-agent.cmd" | "cursor-agent.bat" | "cursor-agent.ps1"
    )
}

fn wrapped_cmd_index(argv: &[String]) -> Option<usize> {
    if let Some(sep) = argv.iter().position(|a| a == "--")
        && sep + 1 < argv.len()
    {
        return Some(sep + 1);
    }
    (!argv.is_empty()).then_some(0)
}

/// Returns `true` when the preset or command argv invokes Antigravity (`agy`).
pub fn is_agy_agent(preset: Option<&str>, argv: &[String]) -> bool {
    if matches!(preset, Some("agy" | "antigravity")) {
        return true;
    }
    wrapped_cmd_index(argv).is_some_and(|idx| argv.get(idx).is_some_and(|cmd| is_agy_binary(cmd)))
}

/// Returns `true` when the preset or command argv invokes Cursor (`cursor-agent`).
pub fn is_cursor_agent(preset: Option<&str>, argv: &[String]) -> bool {
    if matches!(preset, Some("cursor" | "cursor-agent")) {
        return true;
    }
    wrapped_cmd_index(argv).is_some_and(|idx| argv.get(idx).is_some_and(|cmd| is_cursor_binary(cmd)))
}

/// Ensures `agy acp` receives `--dangerously-skip-permissions` on CLI argv once.
pub fn ensure_agy_skip_permissions(argv: &mut Vec<String>) {
    let Some(idx) = wrapped_cmd_index(argv) else {
        return;
    };
    if argv.get(idx).is_some_and(|cmd| is_agy_binary(cmd))
        && argv.get(idx + 1).is_some_and(|sub| sub == "acp")
        && !argv.iter().any(|a| a == "--dangerously-skip-permissions")
    {
        argv.insert(idx + 2, "--dangerously-skip-permissions".into());
    }
}

/// Returns `true` when the preset or command argv invokes Codex ACP (`codex-acp`).
pub fn is_codex_agent(preset: Option<&str>, argv: &[String]) -> bool {
    if preset == Some("codex") {
        return true;
    }
    if wrapped_cmd_index(argv)
        .and_then(|idx| argv.get(idx))
        .and_then(|p| Path::new(p).file_stem())
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("codex-acp"))
    {
        return true;
    }
    argv.iter().any(|arg| arg.to_ascii_lowercase().contains("codex-acp"))
}

/// Prevents `cursor-agent`'s bundled `global-agent@3.0.0` (`forceGlobalAgent: true` by default)
/// from hijacking `@connectrpc/connect-node` `https.request` calls during `session/new` when
/// `HTTP_PROXY` / `HTTPS_PROXY` is set, while preserving Cursor's native HTTP/2 proxy session manager.
pub fn apply_cursor_env_quirks(argv: &[String], env: &mut BTreeMap<String, String>) {
    if !is_cursor_agent(None, argv) {
        return;
    }
    let has_override = env.keys().any(|k| k.eq_ignore_ascii_case("GLOBAL_AGENT_FORCE_GLOBAL_AGENT"));
    if !has_override {
        env.insert("GLOBAL_AGENT_FORCE_GLOBAL_AGENT".into(), "false".into());
    }
}

fn env_or_process_ignore_case(env: &BTreeMap<String, String>, key: &str) -> Option<String> {
    if let Some(val) = env_ignore_case(env, key).map(str::trim).filter(|s| !s.is_empty()) {
        return Some(val.to_string());
    }
    std::env::vars()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn has_claude_non_interactive_auth_in_map<'a>(mut entries: impl Iterator<Item = (&'a str, &'a str)>) -> bool {
    entries.any(|(k, v)| {
        let val = v.trim();
        if val.is_empty() {
            return false;
        }
        if k.eq_ignore_ascii_case("ANTHROPIC_API_KEY")
            || k.eq_ignore_ascii_case("ANTHROPIC_AUTH_TOKEN")
            || k.eq_ignore_ascii_case("ANTHROPIC_BASE_URL")
            || k.eq_ignore_ascii_case("CLAUDE_CODE_OAUTH_TOKEN")
            || k.eq_ignore_ascii_case("ANTHROPIC_FOUNDRY_API_KEY")
            || k.eq_ignore_ascii_case("ANTHROPIC_BEDROCK_BASE_URL")
            || k.eq_ignore_ascii_case("ANTHROPIC_VERTEX_BASE_URL")
            || k.eq_ignore_ascii_case("ANTHROPIC_CUSTOM_HEADERS")
            || k.eq_ignore_ascii_case("AWS_BEARER_TOKEN_BEDROCK")
        {
            return true;
        }
        if (k.eq_ignore_ascii_case("CLAUDE_CODE_USE_BEDROCK")
            || k.eq_ignore_ascii_case("CLAUDE_CODE_USE_VERTEX")
            || k.eq_ignore_ascii_case("CLAUDE_CODE_USE_FOUNDRY"))
            && matches!(val.to_ascii_lowercase().as_str(), "1" | "true" | "yes")
        {
            return true;
        }
        false
    })
}

fn settings_file_has_claude_auth(path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    if v.get("apiKeyHelper").and_then(serde_json::Value::as_str).is_some_and(|s| !s.trim().is_empty()) {
        return true;
    }
    if let Some(env_obj) = v.get("env").and_then(serde_json::Value::as_object) {
        let iter = env_obj.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.as_str(), s)));
        if has_claude_non_interactive_auth_in_map(iter) {
            return true;
        }
    }
    false
}

fn credentials_file_has_claude_auth(path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    v.as_object().is_some_and(|obj| {
        obj.values().any(|entry| match entry {
            serde_json::Value::Object(m) => !m.is_empty(),
            serde_json::Value::String(s) => !s.trim().is_empty(),
            _ => false,
        })
    })
}

fn claude_home_candidates(env: &BTreeMap<String, String>) -> Vec<std::path::PathBuf> {
    let mut homes = Vec::new();
    let mut push_unique = |raw: Option<String>| {
        let Some(s) = raw else { return };
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return;
        }
        let pb = std::path::PathBuf::from(trimmed);
        if !homes.contains(&pb) {
            homes.push(pb);
        }
    };
    // On Windows, Node.js `os.homedir()` and Claude Code prefer `USERPROFILE` over `HOME`
    // (which Git Bash / MSYS2 may set to a POSIX path like `/c/Users/...` or a separate folder).
    if cfg!(windows) {
        push_unique(env_or_process_ignore_case(env, "USERPROFILE"));
        if let (Some(drive), Some(path)) =
            (env_or_process_ignore_case(env, "HOMEDRIVE"), env_or_process_ignore_case(env, "HOMEPATH"))
        {
            push_unique(Some(format!("{drive}{path}")));
        }
        push_unique(env_or_process_ignore_case(env, "HOME"));
    } else {
        push_unique(env_or_process_ignore_case(env, "HOME"));
        push_unique(env_or_process_ignore_case(env, "USERPROFILE"));
    }
    homes
}

fn managed_claude_settings_paths() -> Vec<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        vec![std::path::PathBuf::from("/Library/Application Support/ClaudeCode/managed-settings.json")]
    }
    #[cfg(windows)]
    {
        let mut out = Vec::new();
        for key in ["ProgramFiles", "ProgramData"] {
            if let Ok(base) = std::env::var(key)
                && !base.trim().is_empty()
            {
                let p = std::path::PathBuf::from(base).join("ClaudeCode").join("managed-settings.json");
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
        for fallback in [
            r"C:\Program Files\ClaudeCode\managed-settings.json",
            r"C:\ProgramData\ClaudeCode\managed-settings.json",
        ] {
            let p = std::path::PathBuf::from(fallback);
            if !out.contains(&p) {
                out.push(p);
            }
        }
        out
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        vec![std::path::PathBuf::from("/etc/claude-code/managed-settings.json")]
    }
}

/// Checks whether Claude Code has completed first-time CLI onboarding & authentication before
/// spawning `claude-agent-acp`. On a fresh install where `claude` has never been run to choose
/// a theme and log in (and no API key / gateway / cloud provider env is configured), spawning
/// `claude-agent-acp` hangs during handshake or turn startup instead of guiding the user.
pub fn check_claude_acp_readiness(
    argv: &[String],
    env: &BTreeMap<String, String>,
    cwd: Option<&str>,
) -> Result<(), RpcError> {
    if !is_claude_agent(None, argv) {
        return Ok(());
    }
    let homes = claude_home_candidates(env);
    if homes.is_empty() {
        return Ok(());
    }
    let config_dir = env_or_process_ignore_case(env, "CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from);
    let process_env: Vec<(String, String)> = std::env::vars().collect();
    if has_claude_non_interactive_auth_in_map(process_env.iter().map(|(k, v)| (k.as_str(), v.as_str()))) {
        return Ok(());
    }
    for managed in managed_claude_settings_paths() {
        if settings_file_has_claude_auth(&managed) {
            return Ok(());
        }
    }
    let cwd_path = cwd.map(str::trim).filter(|s| !s.is_empty()).map(Path::new);
    check_claude_acp_readiness_in_homes(argv, env, &homes, config_dir.as_deref(), cwd_path)
}

#[cfg(test)]
fn check_claude_acp_readiness_in(
    argv: &[String],
    env: &BTreeMap<String, String>,
    home: &Path,
    config_dir_override: Option<&Path>,
) -> Result<(), RpcError> {
    check_claude_acp_readiness_in_homes(argv, env, &[home.to_path_buf()], config_dir_override, None)
}

fn check_claude_acp_readiness_in_homes(
    argv: &[String],
    env: &BTreeMap<String, String>,
    homes: &[std::path::PathBuf],
    config_dir_override: Option<&Path>,
    cwd: Option<&Path>,
) -> Result<(), RpcError> {
    if !is_claude_agent(None, argv) {
        return Ok(());
    }
    if has_claude_non_interactive_auth_in_map(env.iter().map(|(k, v)| (k.as_str(), v.as_str()))) {
        return Ok(());
    }

    let mut claude_dirs = Vec::new();
    if let Some(dir) = config_dir_override {
        claude_dirs.push(dir.to_path_buf());
    }
    for home in homes {
        let dir = home.join(".claude");
        if !claude_dirs.contains(&dir) {
            claude_dirs.push(dir);
        }
    }
    if let Some(project) = cwd {
        let proj_dir = project.join(".claude");
        for name in ["settings.json", "settings.local.json"] {
            if settings_file_has_claude_auth(&proj_dir.join(name)) {
                return Ok(());
            }
        }
    }
    for claude_dir in &claude_dirs {
        for name in ["settings.json", "settings.local.json"] {
            if settings_file_has_claude_auth(&claude_dir.join(name)) {
                return Ok(());
            }
        }
        if credentials_file_has_claude_auth(&claude_dir.join(".credentials.json")) {
            return Ok(());
        }
    }

    let mut state_candidates = Vec::new();
    if let Some(dir) = config_dir_override {
        state_candidates.push(dir.join(".claude.json"));
    }
    for home in homes {
        let p = home.join(".claude.json");
        if !state_candidates.contains(&p) {
            state_candidates.push(p);
        }
    }

    let mut has_completed_onboarding = false;
    let mut has_account = false;
    for path in state_candidates {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if v.get("hasCompletedOnboarding").and_then(serde_json::Value::as_bool) == Some(true) {
            has_completed_onboarding = true;
        }
        if v.get("oauthAccount").and_then(serde_json::Value::as_object).is_some_and(|m| !m.is_empty())
            || v.get("primaryApiKey").and_then(serde_json::Value::as_str).is_some_and(|s| !s.trim().is_empty())
            || v.pointer("/customApiKeyResponses/approved")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|arr| !arr.is_empty())
        {
            has_account = true;
        }
        if has_completed_onboarding && has_account {
            return Ok(());
        }
    }

    if !has_completed_onboarding {
        return Err(RpcError::new(
            "auth_required",
            "Authentication required: Claude CLI onboarding is incomplete. Please run 'claude' in a terminal to choose a theme and sign in first.",
            "Run `claude` in a terminal to complete first-time setup and login, then retry ACP mode.",
        ));
    }
    if !has_account {
        return Err(RpcError::new(
            "auth_required",
            "Authentication required: Claude CLI is not logged in. Please run 'claude' in a terminal to sign in first.",
            "Run `claude` in a terminal to sign in, then retry ACP mode.",
        ));
    }
    Ok(())
}

/// Ensures Windows Claude ACP sessions can locate Git Bash (`CLAUDE_CODE_GIT_BASH_PATH`) even when
/// Git for Windows only placed `Git\cmd` (not `Git\bin`) on `PATH`.
pub fn apply_claude_env_quirks(argv: &[String], env: &mut BTreeMap<String, String>) {
    if !is_claude_agent(None, argv) || !cfg!(windows) {
        return;
    }
    if env_ignore_case(env, "CLAUDE_CODE_GIT_BASH_PATH").is_some() {
        return;
    }
    if let Some(bash) = find_windows_git_bash_in(
        env_ignore_case(env, "PATH"),
        env_ignore_case(env, "ProgramFiles"),
        env_ignore_case(env, "ProgramFiles(x86)"),
        env_ignore_case(env, "LOCALAPPDATA"),
        env_ignore_case(env, "USERPROFILE"),
    ) {
        env.insert("CLAUDE_CODE_GIT_BASH_PATH".into(), bash.display().to_string());
    }
}

fn is_windows_wsl_bash_shim(path: &Path) -> bool {
    let lower = path.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
    lower.contains("\\windows\\system32\\")
        || lower.contains("\\windows\\syswow64\\")
        || lower.contains("\\windowsapps\\")
}

fn find_windows_git_bash_in(
    path_env: Option<&str>,
    program_files: Option<&str>,
    program_files_x86: Option<&str>,
    local_app_data: Option<&str>,
    user_profile: Option<&str>,
) -> Option<std::path::PathBuf> {
    let mut candidates = Vec::new();
    for base in [program_files, program_files_x86].into_iter().flatten() {
        let base = Path::new(base);
        candidates.push(base.join("Git").join("bin").join("bash.exe"));
        candidates.push(base.join("Git").join("usr").join("bin").join("bash.exe"));
    }
    if let Some(local) = local_app_data {
        let local = Path::new(local);
        candidates.push(local.join("Programs").join("Git").join("bin").join("bash.exe"));
        candidates.push(local.join("Programs").join("Git").join("usr").join("bin").join("bash.exe"));
    }
    if let Some(profile) = user_profile {
        let profile = Path::new(profile);
        candidates.push(profile.join("scoop").join("apps").join("git").join("current").join("bin").join("bash.exe"));
        candidates
            .push(profile.join("scoop").join("apps").join("git").join("current").join("usr").join("bin").join("bash.exe"));
    }
    for cand in candidates {
        if cand.is_file() {
            return Some(cand);
        }
    }
    if let Some(path) = path_env {
        for dir in std::env::split_paths(path) {
            let bash = dir.join("bash.exe");
            if bash.is_file() && !is_windows_wsl_bash_shim(&bash) {
                return Some(bash);
            }
            let git = dir.join("git.exe");
            if git.is_file()
                && let Some(git_root) = dir.parent()
            {
                for rel in [Path::new("bin").join("bash.exe"), Path::new("usr").join("bin").join("bash.exe")] {
                    let sib = git_root.join(rel);
                    if sib.is_file() {
                        return Some(sib);
                    }
                }
            }
        }
    }
    None
}

/// Points `@agentclientprotocol/codex-acp` at the user's installed `codex` CLI (`CODEX_PATH`) when
/// available on `PATH`, instead of letting an older cached `codex-acp` package fall back to its
/// stale bundled `@openai/codex` binary (which may miss newer models or `openai_base_url` routing).
/// Also merges missing entries from `<codex_home>/models_cache.json` into a cached copy of
/// `model_catalog_json` when a custom catalog (e.g. Magpie) omits newly released Codex models,
/// passing it both via a tiny `CODEX_PATH` wrapper (`-c model_catalog_json=...` so `codex app-server`'s
/// `model/list` exposes `reasoning_effort` and `fast-mode` for those models) and via `CODEX_CONFIG`.
pub fn apply_codex_env_quirks(argv: &[String], env: &mut BTreeMap<String, String>) {
    if !is_codex_agent(None, argv) {
        return;
    }
    let explicit_codex_path = env_ignore_case(env, "CODEX_PATH").is_some();
    let resolved_codex = if explicit_codex_path {
        None
    } else {
        let path_override = env_ignore_case(env, "PATH");
        let pathext_override = env_ignore_case(env, "PATHEXT");
        resolve_non_shim_binary_in_path("codex", path_override, pathext_override)
    };

    let merged_catalog = maybe_merge_codex_model_catalog(env);

    if let Some(codex_bin) = resolved_codex {
        let effective_codex = merged_catalog
            .as_deref()
            .and_then(|merged| write_codex_catalog_wrapper(&codex_bin, merged))
            .unwrap_or(codex_bin);
        env.insert("CODEX_PATH".into(), effective_codex.display().to_string());
    }
}

#[cfg(unix)]
fn sh_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn write_codex_catalog_wrapper(codex_bin: &Path, merged_catalog: &Path) -> Option<std::path::PathBuf> {
    let parent = merged_catalog.parent()?;
    std::fs::create_dir_all(parent).ok()?;

    #[cfg(unix)]
    let (wrapper_path, content) = {
        let catalog_toml_str = serde_json::to_string(&merged_catalog.display().to_string()).ok()?;
        let override_arg = format!("model_catalog_json={catalog_toml_str}");
        let script = format!(
            "#!/bin/sh\nexec {} -c {} \"$@\"\n",
            sh_single_quote(&codex_bin.display().to_string()),
            sh_single_quote(&override_arg),
        );
        (parent.join("codex-wrapper.sh"), script)
    };

    #[cfg(windows)]
    let (wrapper_path, content) = {
        let normalized_catalog = merged_catalog.display().to_string().replace('\\', "/");
        let catalog_toml_str = serde_json::to_string(&normalized_catalog).ok()?;
        let script =
            format!("@echo off\r\n\"{}\" -c model_catalog_json={} %*\r\n", codex_bin.display(), catalog_toml_str,);
        (parent.join("codex-wrapper.cmd"), script)
    };

    if std::fs::read_to_string(&wrapper_path).ok().as_deref() == Some(content.as_str()) {
        return Some(wrapper_path);
    }

    let tmp_path = wrapper_path.with_extension(format!("tmp.{}", std::process::id()));
    if std::fs::write(&tmp_path, content.as_bytes()).is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755)).is_err() {
            let _ = std::fs::remove_file(&tmp_path);
            return None;
        }
    }
    if std::fs::rename(&tmp_path, &wrapper_path).is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        return None;
    }
    Some(wrapper_path)
}

fn codex_home_dir(env: &BTreeMap<String, String>) -> Option<std::path::PathBuf> {
    if let Some(explicit) = env_ignore_case(env, "CODEX_HOME") {
        return Some(std::path::PathBuf::from(explicit));
    }
    for key in ["HOME", "USERPROFILE"] {
        if let Some(home) = env_ignore_case(env, key) {
            return Some(std::path::PathBuf::from(home).join(".codex"));
        }
    }
    None
}

fn codex_merged_catalog_path(env: &BTreeMap<String, String>) -> std::path::PathBuf {
    if let Some(cache) = env_ignore_case(env, "XDG_CACHE_HOME") {
        return std::path::PathBuf::from(cache).join("herdr2/codex-merged-models.json");
    }
    #[cfg(windows)]
    if let Some(local) = env_ignore_case(env, "LOCALAPPDATA") {
        return std::path::PathBuf::from(local).join("herdr2/codex-merged-models.json");
    }
    for key in ["HOME", "USERPROFILE"] {
        if let Some(home) = env_ignore_case(env, key) {
            return std::path::PathBuf::from(home).join(".cache/herdr2/codex-merged-models.json");
        }
    }
    std::env::temp_dir().join("herdr2-codex-merged-models.json")
}

fn maybe_merge_codex_model_catalog(env: &mut BTreeMap<String, String>) -> Option<std::path::PathBuf> {
    let existing_cfg_key = env.keys().find(|k| k.eq_ignore_ascii_case("CODEX_CONFIG")).cloned();
    let mut cfg_obj = match existing_cfg_key.as_deref().and_then(|k| env.get(k)) {
        Some(raw) if !raw.trim().is_empty() => match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(serde_json::Value::Object(map)) => {
                if map.contains_key("model_catalog_json") {
                    return None;
                }
                map
            }
            _ => return None,
        },
        _ => serde_json::Map::new(),
    };

    let codex_home = codex_home_dir(env)?;
    let config_toml_path = codex_home.join("config.toml");
    let toml_text = std::fs::read_to_string(&config_toml_path).ok()?;
    let parsed_toml = toml::from_str::<toml::Value>(&toml_text).ok()?;
    let raw_catalog =
        parsed_toml.get("model_catalog_json").and_then(toml::Value::as_str).map(str::trim).filter(|s| !s.is_empty())?;

    let catalog_path = {
        let p = std::path::PathBuf::from(raw_catalog);
        if p.is_absolute() { p } else { codex_home.join(p) }
    };
    let cache_path = codex_home.join("models_cache.json");
    if !catalog_path.is_file() || !cache_path.is_file() {
        return None;
    }

    let merged_path = codex_merged_catalog_path(env);
    let mtime_of = |p: &Path| std::fs::metadata(p).ok().and_then(|m| m.modified().ok());
    let up_to_date = match (mtime_of(&merged_path), mtime_of(&catalog_path), mtime_of(&cache_path)) {
        (Some(mt), Some(ct), Some(kt)) => mt >= ct && mt >= kt,
        _ => false,
    };

    if !up_to_date {
        let catalog_text = std::fs::read_to_string(&catalog_path).ok()?;
        let cache_text = std::fs::read_to_string(&cache_path).ok()?;
        let mut catalog_val = serde_json::from_str::<serde_json::Value>(&catalog_text).ok()?;
        let cache_val = serde_json::from_str::<serde_json::Value>(&cache_text).ok()?;
        let catalog_models = catalog_val.get("models").and_then(serde_json::Value::as_array)?;
        let cache_models = cache_val.get("models").and_then(serde_json::Value::as_array)?;

        let mut existing_slugs = std::collections::HashSet::new();
        for m in catalog_models {
            if let Some(slug) = m.get("slug").and_then(serde_json::Value::as_str) {
                existing_slugs.insert(slug.to_string());
            }
        }
        let missing: Vec<serde_json::Value> = cache_models
            .iter()
            .filter(|m| m.get("slug").and_then(serde_json::Value::as_str).is_some_and(|s| !existing_slugs.contains(s)))
            .cloned()
            .collect();
        if missing.is_empty() {
            return None;
        }

        let mut merged_models = Vec::with_capacity(missing.len() + catalog_models.len());
        merged_models.extend(missing);
        merged_models.extend(catalog_models.iter().cloned());
        if let Some(obj) = catalog_val.as_object_mut() {
            obj.insert("models".into(), serde_json::Value::Array(merged_models));
        } else {
            return None;
        }

        let bytes = serde_json::to_vec(&catalog_val).ok()?;
        if let Some(parent) = merged_path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return None;
        }
        let tmp_path = merged_path.with_extension(format!("json.tmp.{}", std::process::id()));
        if std::fs::write(&tmp_path, bytes).is_err() {
            let _ = std::fs::remove_file(&tmp_path);
            return None;
        }
        if std::fs::rename(&tmp_path, &merged_path).is_err() {
            let _ = std::fs::remove_file(&tmp_path);
            return None;
        }
    }

    cfg_obj.insert("model_catalog_json".into(), serde_json::Value::String(merged_path.display().to_string()));
    if let Ok(serialized) = serde_json::to_string(&serde_json::Value::Object(cfg_obj)) {
        let key = existing_cfg_key.unwrap_or_else(|| "CODEX_CONFIG".into());
        env.insert(key, serialized);
    }
    Some(merged_path)
}

pub fn preset_names(config_presets: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut names: Vec<String> = BUILTIN_PRESETS.iter().map(|(n, _)| (*n).into()).collect();
    for k in config_presets.keys() {
        if !names.contains(k) {
            names.push(k.clone());
        }
    }
    names.sort();
    names
}

fn lookup_preset(name: &str, config_presets: &BTreeMap<String, Vec<String>>) -> Option<Vec<String>> {
    if let Some(cmd) = config_presets.get(name)
        && !cmd.is_empty()
    {
        return Some(cmd.clone());
    }
    BUILTIN_PRESETS.iter().find(|(k, _)| *k == name).map(|(_, v)| v.iter().map(|s| (*s).into()).collect())
}

/// Resolves `--agent`, positional `argv`, `--prompt`, and `config.agents` presets into an ACP command + initial prompt.
pub fn resolve(
    agent: Option<&str>,
    argv: &[String],
    prompt: Option<String>,
    config_presets: &BTreeMap<String, Vec<String>>,
) -> Result<ResolvedAgent, RpcError> {
    let prompt = prompt.filter(|s| !s.is_empty());
    let resolved = if let Some(spec) = agent {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err(RpcError::invalid_params("--agent cannot be empty"));
        }
        let (preset, cmd) = if let Some(preset_argv) = lookup_preset(spec, config_presets) {
            (Some(spec.to_string()), preset_argv)
        } else {
            (None, split_words(spec)?)
        };
        let prompt = match (prompt, argv.is_empty()) {
            (Some(p), true) => Some(p),
            (None, false) => Some(argv.join(" ")),
            (Some(_), false) => {
                return Err(RpcError::invalid_params(
                    "give initial prompt either with --prompt or positional arguments, not both",
                ));
            }
            (None, true) => None,
        };
        ResolvedAgent { preset, argv: cmd, prompt }
    } else if let Some((first, rest)) = argv.split_first() {
        if let Some(mut cmd) = lookup_preset(first, config_presets) {
            let resolved_prompt = match prompt {
                Some(p) => {
                    cmd.extend(rest.iter().cloned());
                    Some(p)
                }
                None => (!rest.is_empty()).then(|| rest.join(" ")),
            };
            ResolvedAgent { preset: Some(first.clone()), argv: cmd, prompt: resolved_prompt }
        } else {
            ResolvedAgent { preset: None, argv: argv.to_vec(), prompt }
        }
    } else {
        return Err(RpcError::invalid_params(format!(
            "give an agent preset ({}) or command",
            preset_names(config_presets).join(", ")
        )));
    };

    check_not_ide_shim(&resolved.argv[0])?;
    Ok(resolved)
}

/// Splits `--agent "python3 agent.py --flag"` using shell-like quotes and backslash escapes without invoking a shell.
pub fn split_words(raw: &str) -> Result<Vec<String>, RpcError> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => cur.push(ch),
                        None => return Err(RpcError::invalid_params("unterminated single quote in --agent")),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(esc) if matches!(esc, '"' | '\\' | '$') => cur.push(esc),
                            Some(esc) => {
                                cur.push('\\');
                                cur.push(esc);
                            }
                            None => return Err(RpcError::invalid_params("unterminated double quote in --agent")),
                        },
                        Some(ch) => cur.push(ch),
                        None => return Err(RpcError::invalid_params("unterminated double quote in --agent")),
                    }
                }
            }
            '\\' => {
                in_word = true;
                let Some(next) = chars.next() else {
                    return Err(RpcError::invalid_params("trailing backslash in --agent"));
                };
                cur.push(next);
            }
            ch => {
                in_word = true;
                cur.push(ch);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    if words.is_empty() {
        return Err(RpcError::invalid_params("--agent command cannot be empty"));
    }
    Ok(words)
}

fn candidate_files_in_dir(dir: &Path, program: &str, pathext_override: Option<&str>) -> Vec<std::path::PathBuf> {
    candidate_files_in_dir_for_platform(dir, program, pathext_override, cfg!(windows))
}

fn candidate_files_in_dir_for_platform(
    dir: &Path,
    program: &str,
    pathext_override: Option<&str>,
    is_windows: bool,
) -> Vec<std::path::PathBuf> {
    if is_windows && Path::new(program).extension().is_none() {
        // On Windows, npm/npx and Git Bash place extensionless POSIX shell scripts alongside `.cmd`/`.exe`
        // shims. Only consider `PATHEXT` executables so we never resolve an unrunnable POSIX shim first.
        let pathext = pathext_override.map(String::from).or_else(|| std::env::var("PATHEXT").ok());
        let exts = pathext.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or(".COM;.EXE;.BAT;.CMD");
        let mut cands = Vec::new();
        for ext in exts.split(';').map(str::trim).filter(|e| !e.is_empty()) {
            cands.push(dir.join(format!("{program}{ext}")));
            let lower = ext.to_ascii_lowercase();
            if lower != ext {
                cands.push(dir.join(format!("{program}{lower}")));
            }
        }
        return cands;
    }
    vec![dir.join(program)]
}

fn check_not_ide_shim(program: &str) -> Result<(), RpcError> {
    if is_ide_shim_path(program) {
        return Err(shim_error(program));
    }
    if !program.contains('/')
        && !program.contains('\\')
        && let Some(path) = std::env::var_os("PATH")
    {
        for dir in std::env::split_paths(&path) {
            if let Some(cand) = candidate_files_in_dir(&dir, program, None).into_iter().find(|c| c.is_file()) {
                if is_ide_shim_path(&cand.to_string_lossy()) {
                    return Err(shim_error(&cand.display().to_string()));
                }
                break;
            }
        }
    }
    Ok(())
}

pub(crate) fn is_ide_shim_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains(".windsurf") || lower.contains(".codeium")
}

/// Resolves an executable in the supplied `PATH`/`PATHEXT` (or process environment) if it exists and is not an IDE shim.
pub(crate) fn resolve_non_shim_binary_in_path(
    program: &str,
    path_override: Option<&str>,
    pathext_override: Option<&str>,
) -> Option<std::path::PathBuf> {
    if is_ide_shim_path(program) {
        return None;
    }
    let p = Path::new(program);
    if program.contains('/') || program.contains('\\') {
        let dir = p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            return candidate_files_in_dir(dir, name, pathext_override)
                .into_iter()
                .find(|c| c.is_file() && !is_ide_shim_path(&c.to_string_lossy()));
        }
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = path_override.map(std::ffi::OsString::from).or_else(|| std::env::var_os("PATH"))?;
    for dir in std::env::split_paths(&path) {
        if let Some(cand) = candidate_files_in_dir(&dir, program, pathext_override).into_iter().find(|c| c.is_file()) {
            return (!is_ide_shim_path(&cand.to_string_lossy())).then_some(cand);
        }
    }
    None
}

/// Resolves an executable in `PATH` (or explicit path) if it exists and is not an IDE shim.
#[allow(dead_code)]
pub(crate) fn resolve_non_shim_binary(program: &str) -> Option<std::path::PathBuf> {
    resolve_non_shim_binary_in_path(program, None, None)
}

fn env_ignore_case<'a>(env: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str()).filter(|s| !s.is_empty())
}

fn npx_cache_roots(env: &BTreeMap<String, String>) -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    let mut push_unique = |p: std::path::PathBuf| {
        if !roots.contains(&p) {
            roots.push(p);
        }
    };
    if let Some(cache) = env_ignore_case(env, "NPM_CONFIG_CACHE") {
        push_unique(std::path::PathBuf::from(cache).join("_npx"));
    }
    let mut homes: Vec<std::path::PathBuf> = Vec::new();
    for key in ["HOME", "USERPROFILE"] {
        if let Some(h) = env_ignore_case(env, key) {
            let pb = std::path::PathBuf::from(h);
            if !homes.contains(&pb) {
                homes.push(pb);
            }
        }
    }
    if homes.is_empty() && env.is_empty() {
        for key in ["HOME", "USERPROFILE"] {
            if let Ok(h) = std::env::var(key)
                && !h.trim().is_empty()
            {
                let pb = std::path::PathBuf::from(h);
                if !homes.contains(&pb) {
                    homes.push(pb);
                }
            }
        }
    }
    for home in homes {
        push_unique(home.join(".npm/_npx"));
    }
    if let Some(local) = env_ignore_case(env, "LOCALAPPDATA") {
        push_unique(std::path::PathBuf::from(local).join("npm-cache/_npx"));
    }
    roots
}

fn resolve_in_npx_cache(
    program: &str,
    env: &BTreeMap<String, String>,
    pathext_override: Option<&str>,
) -> Option<std::path::PathBuf> {
    if program.contains('/') || program.contains('\\') || is_ide_shim_path(program) {
        return None;
    }
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for root in npx_cache_roots(env) {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let bin_dir = entry.path().join("node_modules/.bin");
            if !bin_dir.is_dir() {
                continue;
            }
            for cand in candidate_files_in_dir(&bin_dir, program, pathext_override) {
                if !cand.is_file() || is_ide_shim_path(&cand.to_string_lossy()) {
                    continue;
                }
                let mtime = cand.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().is_none_or(|(prev, _)| mtime > *prev) {
                    best = Some((mtime, cand));
                }
            }
        }
    }
    best.map(|(_, path)| path)
}

fn companion_npx_adapter(preset_id: Option<&str>, program: &str) -> Option<(&'static str, &'static str)> {
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program).to_ascii_lowercase();
    match (preset_id, base.as_str()) {
        (Some("claude"), "claude-agent-acp") | (_, "claude-agent-acp") => {
            Some(("claude", "@agentclientprotocol/claude-agent-acp"))
        }
        (Some("codex"), "codex-acp") | (_, "codex-acp") => Some(("codex", "@agentclientprotocol/codex-acp")),
        (Some("pi"), "pi-acp") | (_, "pi-acp") => Some(("pi", "pi-acp")),
        _ => None,
    }
}

/// Resolves an ACP agent command against `env` (`PATH`, cached `_npx` adapters, and companion CLI + `npx` fallback).
/// Returns `(resolved_executable_path, effective_argv)` when the agent is available to run.
pub(crate) fn resolve_acp_command_in_env(
    preset_id: Option<&str>,
    argv: &[String],
    env: &BTreeMap<String, String>,
) -> Option<(std::path::PathBuf, Vec<String>)> {
    let first = argv.first()?;
    let path_override = env_ignore_case(env, "PATH");
    let pathext_override = env_ignore_case(env, "PATHEXT");

    if let Some(found) = resolve_non_shim_binary_in_path(first, path_override, pathext_override) {
        return Some((found, argv.to_vec()));
    }

    if let Some(cached) = resolve_in_npx_cache(first, env, pathext_override) {
        let mut effective = Vec::with_capacity(argv.len());
        effective.push(cached.display().to_string());
        effective.extend(argv[1..].iter().cloned());
        return Some((cached, effective));
    }

    if let Some((companion_cli, npx_pkg)) = companion_npx_adapter(preset_id, first)
        && resolve_non_shim_binary_in_path(companion_cli, path_override, pathext_override).is_some()
        && let Some(npx_bin) = resolve_non_shim_binary_in_path("npx", path_override, pathext_override)
    {
        let mut effective = Vec::with_capacity(argv.len() + 2);
        effective.push(npx_bin.display().to_string());
        effective.push("-y".into());
        effective.push(npx_pkg.into());
        effective.extend(argv[1..].iter().cloned());
        return Some((npx_bin, effective));
    }

    None
}

fn shim_error(program: &str) -> RpcError {
    RpcError::new(
        "config",
        format!("{program} appears to be an IDE shim, not a standalone ACP adapter"),
        "install the real ACP adapter (e.g. claude-agent-acp) or override it in config.toml [agents]",
    )
}

pub fn check_preset_command(name: &str, argv: &[String]) -> Result<(), RpcError> {
    if name.trim().is_empty() || argv.is_empty() || argv.iter().any(|s| s.is_empty()) {
        return Err(RpcError::invalid_params(format!("invalid agent preset {name:?}")));
    }
    if Path::new(&argv[0]).as_os_str().is_empty() {
        return Err(RpcError::invalid_params(format!("empty executable in preset {name:?}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_builtin_and_configured_presets() {
        let mut cfg = BTreeMap::new();
        cfg.insert("claude".into(), vec!["my-claude-acp".into(), "--fast".into()]);
        let r = resolve(None, &["claude".into(), "fix tests".into()], None, &cfg).unwrap();
        assert_eq!(r.preset.as_deref(), Some("claude"));
        assert_eq!(r.argv, ["my-claude-acp", "--fast"]);
        assert_eq!(r.prompt.as_deref(), Some("fix tests"));

        let g = resolve(Some("gemini"), &[], Some("hi".into()), &BTreeMap::new()).unwrap();
        assert_eq!(g.argv, ["gemini", "--acp"]);
        assert_eq!(g.prompt.as_deref(), Some("hi"));
    }

    #[test]
    fn splits_custom_agent_command_words_and_rejects_windsurf_shims() {
        let custom = resolve(
            Some(r#"python3 "/tmp/my agent.py" --tag 'hello world'"#),
            &["prompt".into()],
            None,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(custom.preset, None);
        assert_eq!(custom.argv, ["python3", "/tmp/my agent.py", "--tag", "hello world"]);
        assert_eq!(custom.prompt.as_deref(), Some("prompt"));

        let err = resolve(Some("/home/u/.windsurf-server/bin/claude"), &[], None, &BTreeMap::new()).unwrap_err();
        assert_eq!(err.kind, "config");
    }

    #[test]
    fn detects_and_hardens_agy_acp_skip_permissions_flag() {
        assert!(is_agy_agent(Some("agy"), &[]));
        assert!(is_agy_agent(Some("antigravity"), &[]));

        let mut direct = vec!["agy".into(), "acp".into()];
        assert!(is_agy_agent(None, &direct));
        ensure_agy_skip_permissions(&mut direct);
        assert_eq!(direct, ["agy", "acp", "--dangerously-skip-permissions"]);
        ensure_agy_skip_permissions(&mut direct);
        assert_eq!(direct, ["agy", "acp", "--dangerously-skip-permissions"]);

        let mut win = vec![r"C:\Users\dev\AppData\Local\Programs\Antigravity\agy.exe".into(), "acp".into()];
        assert!(is_agy_agent(None, &win));
        ensure_agy_skip_permissions(&mut win);
        assert_eq!(
            win,
            [r"C:\Users\dev\AppData\Local\Programs\Antigravity\agy.exe", "acp", "--dangerously-skip-permissions"]
        );

        let mut wrapped =
            vec!["bwrap".into(), "--ro-bind".into(), "/".into(), "/".into(), "--".into(), "agy".into(), "acp".into()];
        assert!(is_agy_agent(None, &wrapped));
        ensure_agy_skip_permissions(&mut wrapped);
        assert_eq!(wrapped, ["bwrap", "--ro-bind", "/", "/", "--", "agy", "acp", "--dangerously-skip-permissions"]);

        let mut mac_wrapped = vec![
            "/usr/bin/sandbox-exec".into(),
            "-p".into(),
            "(version 1)(allow default)".into(),
            "--".into(),
            "agy".into(),
            "acp".into(),
        ];
        assert!(is_agy_agent(None, &mac_wrapped));
        ensure_agy_skip_permissions(&mut mac_wrapped);
        assert_eq!(
            mac_wrapped,
            [
                "/usr/bin/sandbox-exec",
                "-p",
                "(version 1)(allow default)",
                "--",
                "agy",
                "acp",
                "--dangerously-skip-permissions",
            ]
        );

        let mut win_wrapped = vec![
            r"C:\Users\dev\bin\herdr2.exe".into(),
            "__drove-sandbox".into(),
            "--".into(),
            r"C:\Users\dev\AppData\Local\Programs\Antigravity\agy.exe".into(),
            "acp".into(),
        ];
        assert!(is_agy_agent(None, &win_wrapped));
        ensure_agy_skip_permissions(&mut win_wrapped);
        assert_eq!(
            win_wrapped,
            [
                r"C:\Users\dev\bin\herdr2.exe",
                "__drove-sandbox",
                "--",
                r"C:\Users\dev\AppData\Local\Programs\Antigravity\agy.exe",
                "acp",
                "--dangerously-skip-permissions",
            ]
        );

        let mut non_agy = vec!["claude-agent-acp".into()];
        ensure_agy_skip_permissions(&mut non_agy);
        assert_eq!(non_agy, ["claude-agent-acp"]);
    }

    #[test]
    fn detects_cursor_agent_and_disables_force_global_agent_hijack() {
        assert!(is_cursor_agent(Some("cursor"), &[]));
        assert!(is_cursor_agent(Some("cursor-agent"), &[]));

        let wrapped = vec![
            "bwrap".into(),
            "--dev-bind".into(),
            "/".into(),
            "/".into(),
            "--".into(),
            "cursor-agent".into(),
            "acp".into(),
        ];
        assert!(is_cursor_agent(None, &wrapped));

        let mut env = BTreeMap::new();
        apply_cursor_env_quirks(&wrapped, &mut env);
        assert_eq!(env.get("GLOBAL_AGENT_FORCE_GLOBAL_AGENT").map(String::as_str), Some("false"));

        // Explicit caller setting wins.
        let mut custom = BTreeMap::from([("GLOBAL_AGENT_FORCE_GLOBAL_AGENT".into(), "true".into())]);
        apply_cursor_env_quirks(&wrapped, &mut custom);
        assert_eq!(custom.get("GLOBAL_AGENT_FORCE_GLOBAL_AGENT").map(String::as_str), Some("true"));

        // Non-cursor agents are untouched.
        let mut other = BTreeMap::new();
        apply_cursor_env_quirks(&["claude-agent-acp".into()], &mut other);
        assert!(other.is_empty());
    }

    #[test]
    fn resolves_acp_adapters_from_npx_cache_and_companion_cli_fallback() {
        let root = std::env::temp_dir().join(format!(
            "herdr2-presets-acp-resolve-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
        ));
        let bin_dir = root.join("bin");
        let npx_bin_dir = root.join(".npm/_npx/abc123/node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(&npx_bin_dir).unwrap();

        let claude_acp = npx_bin_dir.join("claude-agent-acp");
        std::fs::write(&claude_acp, "#!/bin/sh\n").unwrap();
        let codex_cli = bin_dir.join("codex");
        let npx_cli = bin_dir.join("npx");
        std::fs::write(&codex_cli, "#!/bin/sh\n").unwrap();
        std::fs::write(&npx_cli, "#!/bin/sh\n").unwrap();

        let env = BTreeMap::from([
            ("HOME".into(), root.display().to_string()),
            ("PATH".into(), bin_dir.display().to_string()),
        ]);

        let (resolved_path, resolved_argv) =
            resolve_acp_command_in_env(Some("claude"), &["claude-agent-acp".into()], &env).unwrap();
        assert_eq!(resolved_path, claude_acp);
        assert_eq!(resolved_argv, vec![claude_acp.display().to_string()]);
        assert!(is_claude_agent(None, &resolved_argv));

        let (codex_path, codex_argv) = resolve_acp_command_in_env(Some("codex"), &["codex-acp".into()], &env).unwrap();
        assert_eq!(codex_path, npx_cli);
        assert_eq!(
            codex_argv,
            vec![npx_cli.display().to_string(), "-y".into(), "@agentclientprotocol/codex-acp".into()]
        );

        let npx_claude =
            vec![npx_cli.display().to_string(), "-y".into(), "@agentclientprotocol/claude-agent-acp".into()];
        assert!(is_claude_agent(None, &npx_claude));

        assert!(resolve_acp_command_in_env(Some("opencode"), &["opencode".into(), "acp".into()], &env).is_none());

        let mut codex_env = env.clone();
        apply_codex_env_quirks(&codex_argv, &mut codex_env);
        assert_eq!(codex_env.get("CODEX_PATH").map(String::as_str), Some(codex_cli.display().to_string().as_str()));

        let mut explicit_env = env.clone();
        explicit_env.insert("CODEX_PATH".into(), "/custom/codex".into());
        apply_codex_env_quirks(&["codex-acp".into()], &mut explicit_env);
        assert_eq!(explicit_env.get("CODEX_PATH").map(String::as_str), Some("/custom/codex"));

        let mut claude_env = env.clone();
        apply_codex_env_quirks(&["claude-agent-acp".into()], &mut claude_env);
        assert!(!claude_env.contains_key("CODEX_PATH"));

        // Custom model_catalog_json missing models from models_cache.json gets merged automatically.
        let codex_home = root.join(".codex");
        std::fs::create_dir_all(&codex_home).unwrap();
        let custom_catalog = codex_home.join("magpie-models.json");
        std::fs::write(&custom_catalog, r#"{"models":[{"slug":"gpt-6.1-sol"}]}"#).unwrap();
        std::fs::write(
            codex_home.join("models_cache.json"),
            r#"{"models":[{"slug":"gpt-6.2-sol","context_window":500000},{"slug":"gpt-6.1-sol"}]}"#,
        )
        .unwrap();
        std::fs::write(
            codex_home.join("config.toml"),
            format!("model = \"gpt-6.2-sol\"\nmodel_catalog_json = {:?}\n", custom_catalog.display().to_string()),
        )
        .unwrap();

        let mut merged_env = env.clone();
        merged_env.insert("CODEX_CONFIG".into(), r#"{"existing":true}"#.into());
        apply_codex_env_quirks(&codex_argv, &mut merged_env);
        let cfg_val: serde_json::Value =
            serde_json::from_str(merged_env.get("CODEX_CONFIG").expect("CODEX_CONFIG set")).unwrap();
        assert_eq!(cfg_val["existing"], serde_json::json!(true));
        let merged_cat_path = cfg_val["model_catalog_json"].as_str().expect("merged path");
        let merged_cat: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(merged_cat_path).unwrap()).unwrap();
        let slugs: Vec<&str> =
            merged_cat["models"].as_array().unwrap().iter().filter_map(|m| m["slug"].as_str()).collect();
        assert_eq!(slugs, vec!["gpt-6.2-sol", "gpt-6.1-sol"]);
        let wrapper_path = merged_env.get("CODEX_PATH").expect("wrapper CODEX_PATH set");
        let wrapper_text = std::fs::read_to_string(wrapper_path).unwrap();
        assert!(wrapper_text.contains("model_catalog_json="), "unexpected wrapper: {wrapper_text}");
        assert!(wrapper_text.contains(&codex_cli.display().to_string()), "unexpected wrapper: {wrapper_text}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn claude_acp_readiness_detects_missing_onboarding_and_allows_authenticated_setups() {
        let root = std::env::temp_dir().join(format!(
            "herdr2-claude-ready-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let argv = vec!["claude-agent-acp".into()];
        let empty_env = BTreeMap::new();

        // 1. Brand-new home with no .claude.json or env credentials -> onboarding error.
        let err = check_claude_acp_readiness_in(&argv, &empty_env, &root, None).unwrap_err();
        assert!(err.message.contains("onboarding is incomplete"), "unexpected error: {}", err.message);

        // 2. Non-claude agent is never blocked.
        assert!(check_claude_acp_readiness_in(&["codex-acp".into()], &empty_env, &root, None).is_ok());

        // 3. Env API key / auth token / Bedrock bypasses CLI onboarding check.
        let mut api_env = BTreeMap::new();
        api_env.insert("ANTHROPIC_API_KEY".into(), "sk-ant-test".into());
        assert!(check_claude_acp_readiness_in(&argv, &api_env, &root, None).is_ok());

        let mut bedrock_env = BTreeMap::new();
        bedrock_env.insert("CLAUDE_CODE_USE_BEDROCK".into(), "1".into());
        assert!(check_claude_acp_readiness_in(&argv, &bedrock_env, &root, None).is_ok());

        // 4. settings.json with ANTHROPIC_AUTH_TOKEN (custom gateway / Magpie) passes.
        let claude_dir = root.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.json"),
            r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"tok-123"}}"#,
        )
        .unwrap();
        assert!(check_claude_acp_readiness_in(&argv, &empty_env, &root, None).is_ok());
        std::fs::remove_file(claude_dir.join("settings.json")).unwrap();

        // 5. Completed onboarding without oauthAccount or credentials -> login required error.
        std::fs::write(root.join(".claude.json"), r#"{"hasCompletedOnboarding":true}"#).unwrap();
        let err2 = check_claude_acp_readiness_in(&argv, &empty_env, &root, None).unwrap_err();
        assert!(err2.message.contains("not logged in"), "unexpected error: {}", err2.message);

        // 6. Completed onboarding + oauthAccount -> passes.
        std::fs::write(
            root.join(".claude.json"),
            r#"{"hasCompletedOnboarding":true,"oauthAccount":{"emailAddress":"u@example.com"}}"#,
        )
        .unwrap();
        assert!(check_claude_acp_readiness_in(&argv, &empty_env, &root, None).is_ok());

        // 7. .credentials.json with oauth payload -> passes even without .claude.json.
        std::fs::remove_file(root.join(".claude.json")).unwrap();
        std::fs::write(
            claude_dir.join(".credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"tok"}}"#,
        )
        .unwrap();
        assert!(check_claude_acp_readiness_in(&argv, &empty_env, &root, None).is_ok());

        // 8. Multi-home fallback (e.g. Windows where HOME is a POSIX path like /c/Users/...
        //    while USERPROFILE holds .claude/.credentials.json) still passes.
        let fake_posix_home = root.join("nonexistent-posix-home");
        assert!(
            check_claude_acp_readiness_in_homes(&argv, &empty_env, &[fake_posix_home.clone(), root.clone()], None, None)
                .is_ok()
        );

        // 9. Project-local .claude/settings.local.json with ANTHROPIC_BASE_URL passes.
        let empty_home = root.join("empty-home");
        let project_dir = root.join("project");
        std::fs::create_dir_all(project_dir.join(".claude")).unwrap();
        std::fs::write(
            project_dir.join(".claude/settings.local.json"),
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:4000"}}"#,
        )
        .unwrap();
        assert!(
            check_claude_acp_readiness_in_homes(&argv, &empty_env, &[empty_home], None, Some(&project_dir)).is_ok()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn windows_candidate_resolution_skips_extensionless_posix_shims_and_finds_git_bash() {
        let root = std::env::temp_dir().join(format!(
            "herdr2-win-claude-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
        ));
        let npm_dir = root.join("npm");
        std::fs::create_dir_all(&npm_dir).unwrap();
        std::fs::write(npm_dir.join("claude-agent-acp"), "#!/bin/sh\n").unwrap();
        std::fs::write(npm_dir.join("claude-agent-acp.cmd"), "@echo off\r\n").unwrap();

        let win_cands =
            candidate_files_in_dir_for_platform(&npm_dir, "claude-agent-acp", Some(".EXE;.CMD"), true);
        assert!(
            !win_cands.contains(&npm_dir.join("claude-agent-acp")),
            "Windows resolution must not pick extensionless POSIX shim: {win_cands:?}"
        );
        let picked = win_cands.into_iter().find(|p| p.is_file()).expect("should resolve .cmd");
        assert_eq!(picked, npm_dir.join("claude-agent-acp.cmd"));

        // Git Bash discovery: skips System32 WSL shim and finds Git\bin\bash.exe relative to git.exe on PATH.
        let fake_sys32 = root.join("Windows/System32");
        let git_cmd = root.join("CustomGit/cmd");
        let git_bin = root.join("CustomGit/bin");
        std::fs::create_dir_all(&fake_sys32).unwrap();
        std::fs::create_dir_all(&git_cmd).unwrap();
        std::fs::create_dir_all(&git_bin).unwrap();
        std::fs::write(fake_sys32.join("bash.exe"), b"").unwrap();
        std::fs::write(git_cmd.join("git.exe"), b"").unwrap();
        let expected_bash = git_bin.join("bash.exe");
        std::fs::write(&expected_bash, b"").unwrap();

        let joined_path = std::env::join_paths([&fake_sys32, &git_cmd]).unwrap();
        let found = find_windows_git_bash_in(
            Some(&joined_path.to_string_lossy()),
            None,
            None,
            None,
            None,
        );
        assert_eq!(found.as_deref(), Some(expected_bash.as_path()));

        let _ = std::fs::remove_dir_all(&root);
    }
}
