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
    ("zcode", &["zcode-acp"]),
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
    argv.first()
        .and_then(|p| Path::new(p).file_stem())
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.to_ascii_lowercase().starts_with("claude"))
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

/// Prevents `cursor-agent`'s bundled `global-agent@3.0.0` (`forceGlobalAgent: true` by default)
/// from hijacking `@connectrpc/connect-node` `https.request` calls during `session/new` when
/// `HTTP_PROXY` / `HTTPS_PROXY` is set, while preserving Cursor's native HTTP/2 proxy session manager.
pub fn apply_cursor_env_quirks(argv: &[String], env: &mut BTreeMap<String, String>) {
    if !is_cursor_agent(None, argv) {
        return;
    }
    let has_override = env
        .keys()
        .any(|k| k.eq_ignore_ascii_case("GLOBAL_AGENT_FORCE_GLOBAL_AGENT"));
    if !has_override {
        env.insert("GLOBAL_AGENT_FORCE_GLOBAL_AGENT".into(), "false".into());
    }
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
                return Err(RpcError::invalid_params("give initial prompt either with --prompt or positional arguments, not both"));
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
    let mut cands = vec![dir.join(program)];
    if cfg!(windows) && Path::new(program).extension().is_none() {
        let pathext = pathext_override.map(String::from).or_else(|| std::env::var("PATHEXT").ok());
        let exts = pathext.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or(".COM;.EXE;.BAT;.CMD");
        for ext in exts.split(';').map(str::trim).filter(|e| !e.is_empty()) {
            cands.push(dir.join(format!("{program}{ext}")));
            let lower = ext.to_ascii_lowercase();
            if lower != ext {
                cands.push(dir.join(format!("{program}{lower}")));
            }
        }
    }
    cands
}

fn check_not_ide_shim(program: &str) -> Result<(), RpcError> {
    if is_ide_shim_path(program) {
        return Err(shim_error(program));
    }
    if !program.contains('/') && !program.contains('\\')
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

        let mut wrapped = vec!["bwrap".into(), "--ro-bind".into(), "/".into(), "/".into(), "--".into(), "agy".into(), "acp".into()];
        assert!(is_agy_agent(None, &wrapped));
        ensure_agy_skip_permissions(&mut wrapped);
        assert_eq!(
            wrapped,
            ["bwrap", "--ro-bind", "/", "/", "--", "agy", "acp", "--dangerously-skip-permissions"]
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
}
