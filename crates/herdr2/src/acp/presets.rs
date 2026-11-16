//! Built-in ACP agent presets, command word splitting, and IDE shim detection.

use crate::rpc::RpcError;
use std::collections::BTreeMap;
use std::path::Path;

pub const BUILTIN_PRESETS: &[(&str, &[&str])] = &[
    ("claude", &["claude-agent-acp"]),
    ("codex", &["codex-acp"]),
    ("gemini", &["gemini", "--acp"]),
    ("opencode", &["opencode", "acp"]),
    ("copilot", &["copilot", "--acp", "--stdio"]),
    ("cursor", &["cursor-agent", "acp"]),
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

fn check_not_ide_shim(program: &str) -> Result<(), RpcError> {
    if is_ide_shim_path(program) {
        return Err(shim_error(program));
    }
    if !program.contains('/') && !program.contains('\\')
        && let Some(path) = std::env::var_os("PATH")
    {
        for dir in std::env::split_paths(&path) {
            let cand = dir.join(program);
            if cand.is_file() {
                if is_ide_shim_path(&cand.to_string_lossy()) {
                    return Err(shim_error(&cand.display().to_string()));
                }
                break;
            }
        }
    }
    Ok(())
}

fn is_ide_shim_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains(".windsurf") || lower.contains(".codeium")
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
}
