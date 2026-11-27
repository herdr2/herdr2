//! `h2 adapter`: inspect, install, and update managed ACP adapters.

use super::{Out, call};
use crate::paths::Paths;
use crate::rpc::RpcError;
use clap::{Args, Subcommand};
use serde_json::{Value, json};

#[derive(Args)]
pub(crate) struct AdapterCli {
    #[command(subcommand)]
    pub action: AdapterAction,
}

#[derive(Subcommand)]
pub(crate) enum AdapterAction {
    /// Inspect managed ACP adapters, companion CLIs, installation source, and versions.
    Status {
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Download and install missing managed ACP adapters from the official ACP Registry.
    Install {
        /// Specific preset(s) to install (`claude`, `codex`, `pi`, `glm`).
        presets: Vec<String>,
        /// Install all managed ACP adapters.
        #[arg(long)]
        all: bool,
        /// Only install adapters for companion CLIs detected on PATH.
        #[arg(long)]
        detected: bool,
        /// Force re-install even if already installed.
        #[arg(long)]
        force: bool,
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Refresh the official ACP Registry and update managed ACP adapters to the latest version.
    Update {
        /// Specific preset(s) to update (`claude`, `codex`, `pi`, `glm`).
        presets: Vec<String>,
        /// Update all managed ACP adapters.
        #[arg(long)]
        all: bool,
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Update ACP detection & runtime configuration in config.toml.
    Config {
        /// Automatically install missing managed ACP adapters (`true` or `false`).
        #[arg(long, value_name = "BOOL")]
        auto_install: Option<bool>,
        /// Custom ACP Registry index URL.
        #[arg(long, value_name = "URL")]
        registry_url: Option<String>,
        /// Reset `acp_registry_url` back to the official ACP Registry default.
        #[arg(long)]
        clear_registry_url: bool,
        /// Default ACP permission policy (`approve-reads`, `approve-all`, `ask`, `deny-all`).
        #[arg(long, value_name = "MODE")]
        default_permissions: Option<crate::permissions::PermissionMode>,
        /// Auto-inject the built-in `herdr2` MCP server into ACP sessions (`true` or `false`).
        #[arg(long, value_name = "BOOL")]
        inject_mcp: Option<bool>,
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
}

pub(crate) fn run(paths: &Paths, cli: AdapterCli) -> Result<Out, RpcError> {
    match cli.action {
        AdapterAction::Status { on } => {
            let mut params = json!({});
            if let Some(m) = on {
                params["on"] = json!(m);
            }
            let r = call(paths, "adapter.status", params)?;
            let human = format_status(&r);
            Ok(Out { json: r, human })
        }
        AdapterAction::Install { presets, all, detected, force, on } => {
            let mut params = json!({
                "presets": presets,
                "all": all,
                "detected": detected,
                "force": force,
            });
            if let Some(m) = on {
                params["on"] = json!(m);
            }
            let r = call(paths, "adapter.install", params)?;
            let human = format_install(&r);
            Ok(Out { json: r, human })
        }
        AdapterAction::Update { presets, all, on } => {
            let mut params = json!({
                "presets": presets,
                "all": all,
            });
            if let Some(m) = on {
                params["on"] = json!(m);
            }
            let r = call(paths, "adapter.update", params)?;
            let human = format_install(&r);
            Ok(Out { json: r, human })
        }
        AdapterAction::Config { auto_install, registry_url, clear_registry_url, default_permissions, inject_mcp, on } => {
            let mut params = json!({});
            if let Some(v) = auto_install {
                params["auto_install_adapters"] = json!(v);
            }
            if clear_registry_url {
                params["acp_registry_url"] = json!("");
            } else if let Some(u) = registry_url {
                params["acp_registry_url"] = json!(u);
            }
            if let Some(mode) = default_permissions {
                params["default_permissions"] = json!(mode);
            }
            if let Some(v) = inject_mcp {
                params["inject_mcp"] = json!(v);
            }
            if let Some(m) = on {
                params["on"] = json!(m);
            }
            let r = call(paths, "adapter.config.set", params)?;
            let human = format_status(&r);
            Ok(Out { json: r, human })
        }
    }
}

fn format_status(r: &Value) -> String {
    let dir = r["adapters_dir"].as_str().unwrap_or("");
    let auto = r["auto_install"].as_bool().unwrap_or(true);
    let mut lines = vec![format!("Managed ACP adapters ({dir})  auto_install={auto}")];
    if let Some(arr) = r["adapters"].as_array() {
        for item in arr {
            let preset = item["preset_id"].as_str().unwrap_or("?");
            let state = item["state"].as_str().unwrap_or("?");
            let source = item["source"].as_str().unwrap_or("?");
            let ver = item["version"].as_str().map(|v| format!(" v{v}")).unwrap_or_default();
            let cli = match (item["companion_cli"].as_str(), item["companion_installed"].as_bool()) {
                (Some(c), Some(true)) => format!("cli:{c}=ok"),
                (Some(c), _) => format!("cli:{c}=missing"),
                _ => "standalone".into(),
            };
            let extra = if let Some(err) = item["error"].as_str() {
                format!("  error: {err}")
            } else if let Some(path) = item["path"].as_str() {
                format!("  {path}")
            } else {
                String::new()
            };
            lines.push(format!("  {preset:<8} {state:<11} {source:<12}{ver:<10} ({cli}){extra}"));
        }
    }
    lines.join("\n")
}

fn format_install(r: &Value) -> String {
    let dir = r["adapters_dir"].as_str().unwrap_or("");
    let mut lines = vec![format!("Adapters dir: {dir}")];
    if let Some(arr) = r["results"].as_array() {
        if arr.is_empty() {
            lines.push("  no matching adapters selected (pass a preset name, --detected, or --all)".into());
        }
        for item in arr {
            let preset = item["preset_id"].as_str().unwrap_or("?");
            let action = item["action"].as_str().unwrap_or("?");
            let method = item["method"].as_str().map(|m| format!(" via {m}")).unwrap_or_default();
            let ver = item["version"].as_str().map(|v| format!(" v{v}")).unwrap_or_default();
            let tail = if let Some(err) = item["error"].as_str() {
                format!(": {err}")
            } else if let Some(path) = item["path"].as_str() {
                format!(" ({path})")
            } else {
                String::new()
            };
            lines.push(format!("  {preset:<8} {action}{method}{ver}{tail}"));
        }
    }
    lines.join("\n")
}
