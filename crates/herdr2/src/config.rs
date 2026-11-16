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
    RpcError::new("config", message, "fix ~/.config/herdr2/config.toml")
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

[agents]
claude = ["my-claude-acp", "--flag"]
"#,
        )
        .unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.default_permissions, Some(PermissionMode::ApproveAll));
        assert_eq!(cfg.inject_mcp, Some(false));
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
}
