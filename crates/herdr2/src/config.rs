//! `~/.config/herdr2/config.toml`. A missing file means defaults.

use crate::permissions::PermissionMode;
use crate::rpc::RpcError;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

#[derive(Deserialize, Default)]
pub struct Config {
    /// Seconds a `wait` runs before it checks in (default 180).
    pub wait_timeout: Option<f64>,
    /// Notes to parent agents and `<h2>` tags on messages between agents, on
    /// or off (default on). Off: h2 sends no automated parent notifications.
    pub notes: Option<bool>,
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

impl Config {
    pub fn machine_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(self.machine_timeout.filter(|s| s.is_finite() && *s > 0.0).unwrap_or(30.0))
    }

    pub fn validate(&self) -> Result<(), RpcError> {
        for (name, argv) in &self.agents {
            crate::acp::presets::check_preset_command(name, argv)?;
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
