//! `~/.config/herdr2/config.toml`. A missing file means defaults.

use crate::permissions::PermissionMode;
use crate::rpc::RpcError;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

#[derive(Deserialize, Default)]
pub struct Config {
    /// Executable and arguments for new shells (default: this machine's shell).
    pub shell: Option<Vec<String>>,
    /// The chord that detaches `h2 attach`: `ctrl+<key>`, default `ctrl+\`.
    pub detach_key: Option<String>,
    /// Seconds a `wait` runs before it checks in (default 180).
    pub wait_timeout: Option<f64>,
    /// Notes to parent agents and `<h2>` tags on messages between agents, on
    /// or off (default on). Off: h2 sends no automated notifications.
    pub notes: Option<bool>,
    /// Seconds of quiet before a note in a PTY terminal (default 3); after Enter or a clear it goes in at once.
    #[serde(default, deserialize_with = "seconds")]
    pub notes_quiet: Option<Duration>,
    /// Seconds an unfinished draft holds sends and notes after the last key (default 60); 0 disables it.
    #[serde(default, deserialize_with = "seconds")]
    pub draft_wait: Option<Duration>,
    /// Size of a terminal nobody is typing in, `COLSxROWS` (default 200x120).
    pub headless: Option<String>,
    /// Scrollback kept per live terminal, in MB (default 5).
    pub history_mb: Option<f64>,
    /// After a kept program is lost, show its last saved screen above the new shell (default off).
    pub restore_screen: Option<bool>,
    /// After a kept CLI agent terminal (`claude`, `codex`, `agy`) is lost on reboot/restart,
    /// automatically resume its CLI session in place instead of dropping to a blank shell (default on).
    pub restore_cli_sessions: Option<bool>,
    /// Where the core listens (default under $XDG_RUNTIME_DIR). `H2_SOCKET` wins.
    pub socket: Option<String>,
    /// Seconds another machine gets to answer a call (a wait gets its own
    /// timeout on top), and to send anything at all before its line is
    /// given up and dialed again (default 30).
    pub machine_timeout: Option<f64>,
    /// Default ACP permission policy (`approve-all`, `approve-reads`, `deny-all`, `ask`).
    pub default_permissions: Option<PermissionMode>,
    /// Auto-inject the built-in `herdr2` MCP server into ACP sessions (default true).
    pub inject_mcp: Option<bool>,
    /// Base URL for `h2 update` release manifests (`<url>/core/latest.json`). `HERDR2_RELEASES_URL` wins.
    pub releases_url: Option<String>,
    /// Automatically download and install missing managed ACP adapters into Herdr2's isolated
    /// adapters directory when a companion CLI (`claude`, `codex`, `pi`) is installed or launched (default true).
    pub auto_install_adapters: Option<bool>,
    /// Override the official ACP Registry index URL (`HERDR2_ACP_REGISTRY_URL` wins).
    pub acp_registry_url: Option<String>,
    /// Custom ACP agent preset commands overriding or extending built-in presets.
    #[serde(default)]
    pub agents: BTreeMap<String, Vec<String>>,
}

fn seconds<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
    Option::<f64>::deserialize(d)?
        .map(|s| {
            if !s.is_finite() {
                return Err(serde::de::Error::custom("seconds must be finite and fit in a duration"));
            }
            Duration::try_from_secs_f64(s.max(0.0))
                .map_err(|_| serde::de::Error::custom("seconds must be finite and fit in a duration"))
        })
        .transpose()
}

impl Config {
    pub fn shell(&self) -> Result<Vec<String>, RpcError> {
        match &self.shell {
            None => Ok(crate::platform::own_shell()),
            Some(argv) if argv.first().is_some_and(|exe| !exe.is_empty()) => Ok(argv.clone()),
            Some(_) => Err(invalid("shell: give an executable and optional arguments, like [\"nu\"]".into())),
        }
    }

    pub fn headless(&self) -> Result<(u16, u16), RpcError> {
        let Some(spec) = &self.headless else { return Ok((200, 120)) };
        spec.split_once('x')
            .and_then(|(c, r)| Some((c.trim().parse().ok()?, r.trim().parse().ok()?)))
            .filter(|(c, r): &(u16, u16)| *c > 0 && *r > 0)
            .ok_or_else(|| invalid(format!("headless {spec:?}: use COLSxROWS, like \"200x120\"")))
    }

    pub fn machine_timeout(&self) -> Duration {
        Duration::from_secs_f64(self.machine_timeout.filter(|s| s.is_finite() && *s > 0.0).unwrap_or(30.0))
    }

    pub fn history_bytes(&self) -> usize {
        (self.history_mb.unwrap_or(5.0).max(0.0) * 1024.0 * 1024.0) as usize
    }

    pub fn restore_cli_sessions(&self) -> bool {
        self.restore_cli_sessions.unwrap_or(true)
    }

    pub fn auto_install_adapters(&self) -> bool {
        self.auto_install_adapters.unwrap_or(true)
    }

    pub fn inject_mcp(&self) -> bool {
        self.inject_mcp.unwrap_or(true)
    }

    pub fn validate(&self) -> Result<(), RpcError> {
        for (name, argv) in &self.agents {
            crate::acp::presets::check_preset_command(name, argv)?;
        }
        Ok(())
    }

    pub fn detach_key(&self) -> Result<DetachKey, RpcError> {
        let spec = self.detach_key.as_deref().unwrap_or("ctrl+\\");
        let key = spec.strip_prefix("ctrl+").and_then(|k| {
            let mut chars = k.chars();
            let c = chars.next()?.to_ascii_lowercase();
            (chars.next().is_none() && (c.is_ascii_lowercase() || "@[\\]^_".contains(c))).then_some(c)
        });
        let c = key.ok_or_else(|| {
            invalid(format!("detach_key {spec:?}: use a ctrl chord like \"ctrl+\\\\\" or \"ctrl+]\""))
        })?;
        Ok(DetachKey { byte: c as u8 & 0x1f, code: c as u32 })
    }
}

fn invalid(message: String) -> RpcError {
    let hint = if cfg!(windows) {
        r"fix %APPDATA%\herdr2\config.toml"
    } else {
        "fix ~/.config/herdr2/config.toml"
    };
    RpcError::new("config", message, hint)
}

pub fn load(path: &Path) -> Result<Config, RpcError> {
    let cfg: Config = match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| invalid(format!("{}: {e}", path.display())))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => return Err(invalid(format!("{}: {e}", path.display()))),
    };
    cfg.validate()?;
    Ok(cfg)
}

/// Delivery defaults if config cannot be parsed during runtime notifications.
pub fn delivery(path: &Path) -> Config {
    load(path).unwrap_or_else(|error| {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            let _ = writeln!(io::stderr(), "core: {}; using delivery defaults", error.message);
        });
        Config::default()
    })
}

/// Partial update to ACP-related keys in `~/.config/herdr2/config.toml`.
#[derive(Clone, Debug, Default)]
pub struct AcpConfigPatch {
    pub auto_install_adapters: Option<bool>,
    /// `Some(Some(url))` sets `acp_registry_url`; `Some(None)` clears it back to default.
    pub acp_registry_url: Option<Option<String>>,
    pub default_permissions: Option<PermissionMode>,
    pub inject_mcp: Option<bool>,
}

fn format_toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn upsert_top_level_toml_key(content: &str, key: &str, formatted_value: Option<&str>) -> String {
    let mut lines: Vec<String> = if content.is_empty() {
        Vec::new()
    } else {
        content.lines().map(str::to_string).collect()
    };

    // Find the first `[table]` header; top-level keys must live above it.
    let first_table_idx = lines
        .iter()
        .position(|line| {
            let t = line.trim_start();
            t.starts_with('[') && !t.starts_with("[[") || t.starts_with("[[")
        })
        .unwrap_or(lines.len());

    let mut matched_indices = Vec::new();
    for (idx, line) in lines.iter().enumerate().take(first_table_idx) {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix(key) {
            let rest = rest.trim_start();
            if rest.starts_with('=') {
                matched_indices.push(idx);
            }
        }
    }

    match formatted_value {
        Some(val) => {
            let assignment = format!("{key} = {val}");
            if let Some(&first) = matched_indices.first() {
                lines[first] = assignment;
                for &dup in matched_indices.iter().skip(1).rev() {
                    lines.remove(dup);
                }
            } else {
                // Insert right before any trailing blank lines preceding the first table.
                let mut insert_at = first_table_idx;
                while insert_at > 0 && lines[insert_at - 1].trim().is_empty() {
                    insert_at -= 1;
                }
                lines.insert(insert_at, assignment);
                if insert_at + 1 < lines.len() && lines[insert_at + 1].trim_start().starts_with('[') {
                    lines.insert(insert_at + 1, String::new());
                }
            }
        }
        None => {
            for &idx in matched_indices.iter().rev() {
                lines.remove(idx);
            }
        }
    }

    if lines.is_empty() {
        String::new()
    } else {
        let mut out = lines.join("\n");
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out
    }
}

/// Applies an ACP config patch to `config.toml` while preserving comments and other sections.
pub fn update_acp_config(path: &Path, patch: &AcpConfigPatch) -> Result<Config, RpcError> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(invalid(format!("{}: {e}", path.display()))),
    };
    if !existing.is_empty() {
        // Never overwrite a broken config.toml.
        let _: Config = toml::from_str(&existing).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
    }

    let mut next = existing;
    if let Some(auto) = patch.auto_install_adapters {
        let val = if auto { "true" } else { "false" };
        next = upsert_top_level_toml_key(&next, "auto_install_adapters", Some(val));
    }
    if let Some(reg) = &patch.acp_registry_url {
        match reg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(url) => {
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    return Err(RpcError::invalid_params("acp_registry_url must start with http:// or https://"));
                }
                let formatted = format_toml_string(url);
                next = upsert_top_level_toml_key(&next, "acp_registry_url", Some(&formatted));
            }
            None => {
                next = upsert_top_level_toml_key(&next, "acp_registry_url", None);
            }
        }
    }
    if let Some(perms) = patch.default_permissions {
        let formatted = format_toml_string(perms.as_str());
        next = upsert_top_level_toml_key(&next, "default_permissions", Some(&formatted));
    }
    if let Some(inject) = patch.inject_mcp {
        let val = if inject { "true" } else { "false" };
        next = upsert_top_level_toml_key(&next, "inject_mcp", Some(val));
    }

    let parsed: Config = toml::from_str(&next).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
    parsed.validate()?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| invalid(format!("{}: {e}", parent.display())))?;
    }
    let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
    std::fs::write(&tmp, next.as_bytes()).map_err(|e| invalid(format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        invalid(format!("{}: {e}", path.display()))
    })?;
    Ok(parsed)
}

/// A detach chord: the control byte it sends, and its key code for the
/// kitty and modifyOtherKeys encodings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetachKey {
    pub byte: u8,
    pub code: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(spec: &str) -> Result<DetachKey, RpcError> {
        Config { detach_key: Some(spec.into()), ..Default::default() }.detach_key()
    }

    #[test]
    fn parses_permissions_mcp_and_agent_presets() {
        let cfg: Config = toml::from_str(
            r#"
default_permissions = "approve-all"
inject_mcp = false
wait_timeout = 42.5
auto_install_adapters = false
acp_registry_url = "https://mirror.example/registry.json"

[agents]
claude = ["my-claude-acp", "--flag"]
"#,
        )
        .unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.default_permissions, Some(PermissionMode::ApproveAll));
        assert_eq!(cfg.inject_mcp, Some(false));
        assert!(!cfg.inject_mcp());
        assert!(!cfg.auto_install_adapters());
        assert_eq!(cfg.acp_registry_url.as_deref(), Some("https://mirror.example/registry.json"));
        assert!(Config::default().auto_install_adapters());
        assert!(Config::default().inject_mcp());
        assert_eq!(cfg.agents["claude"], ["my-claude-acp", "--flag"]);
    }

    #[test]
    fn rejects_empty_agent_preset() {
        let cfg: Config = toml::from_str("[agents]\nempty = []\n").unwrap();
        assert_eq!(cfg.validate().unwrap_err().kind, "invalid_params");
    }

    #[test]
    fn delivery_seconds_keep_negative_quiet_compatible_and_reject_nonfinite_or_overflow() {
        let c: Config = toml::from_str("notes_quiet = -1\nshell = ['sh']").unwrap();
        assert_eq!(c.notes_quiet, Some(Duration::ZERO));
        assert_eq!(c.shell().unwrap(), ["sh"]);
        for setting in ["notes_quiet", "draft_wait"] {
            for value in ["nan", "inf", "-inf", "1e100"] {
                assert!(toml::from_str::<Config>(&format!("{setting} = {value}")).is_err());
            }
        }
    }

    #[test]
    fn detach_keys_are_ctrl_chords_only() {
        assert_eq!(Config::default().detach_key().unwrap(), DetachKey { byte: 0x1c, code: 92 });
        assert_eq!(key("ctrl+]").unwrap(), DetachKey { byte: 0x1d, code: 93 });
        assert_eq!(key("ctrl+A").unwrap(), DetachKey { byte: 0x01, code: 97 });
        for bad in ["alt+x", "ctrl+", "ctrl+ab", "ctrl+1", "x"] {
            assert_eq!(key(bad).unwrap_err().kind, "config", "{bad}");
        }
    }

    #[test]
    fn update_acp_config_inserts_before_tables_preserves_comments_and_clears_registry() {
        let dir = std::env::temp_dir().join(format!("herdr2-cfg-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "# user comment\nshell = [\"zsh\"]\n\n[agents]\nclaude = [\"my-claude-acp\"]\n",
        )
        .unwrap();

        let cfg = update_acp_config(
            &path,
            &AcpConfigPatch {
                auto_install_adapters: Some(false),
                acp_registry_url: Some(Some("https://mirror.example/registry.json".into())),
                default_permissions: Some(PermissionMode::ApproveAll),
                inject_mcp: Some(false),
            },
        )
        .unwrap();
        assert!(!cfg.auto_install_adapters());
        assert_eq!(cfg.acp_registry_url.as_deref(), Some("https://mirror.example/registry.json"));
        assert_eq!(cfg.default_permissions, Some(PermissionMode::ApproveAll));
        assert_eq!(cfg.inject_mcp, Some(false));
        assert_eq!(cfg.agents["claude"], ["my-claude-acp"]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# user comment"));

        let cfg2 = update_acp_config(
            &path,
            &AcpConfigPatch {
                auto_install_adapters: Some(true),
                acp_registry_url: Some(None),
                default_permissions: Some(PermissionMode::Ask),
                inject_mcp: Some(true),
            },
        )
        .unwrap();
        assert!(cfg2.auto_install_adapters());
        assert_eq!(cfg2.acp_registry_url, None);
        assert_eq!(cfg2.default_permissions, Some(PermissionMode::Ask));
        assert_eq!(cfg2.inject_mcp, Some(true));
        assert_eq!(cfg2.agents["claude"], ["my-claude-acp"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
