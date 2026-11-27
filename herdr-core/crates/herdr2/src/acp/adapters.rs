//! Managed ACP adapter registry, on-demand downloader, and background auto-installer.
//!
//! Keeps managed adapters isolated under `~/.local/share/herdr2/adapters/` (Unix) or
//! `%LOCALAPPDATA%\herdr2\adapters\` (Windows), preferring official ACP Registry standalone
//! binaries (verified by SHA-256) and falling back to isolated `npm install --prefix`.

use crate::acp::presets::{candidate_files_in_dir, resolve_in_npx_cache, resolve_non_shim_binary_in_path};
use crate::rpc::RpcError;
use crate::runtime::{Core, now_ms};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DEFAULT_ACP_REGISTRY_URL: &str = "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";
const REGISTRY_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
const DOWNLOAD_TIMEOUT_SECS: &str = "900";
const DOWNLOAD_SPEED_LIMIT_BYTES: &str = "10240";
const DOWNLOAD_SPEED_TIME_SECS: &str = "45";
const REGISTRY_TIMEOUT_SECS: &str = "20";

/// Specification for one managed ACP adapter paired with a built-in Herdr2 preset.
#[derive(Clone, Copy, Debug)]
pub struct ManagedAdapterSpec {
    /// Canonical Herdr2 preset name (`claude`, `codex`, `pi`, `glm`).
    pub preset_id: &'static str,
    /// Official ACP Registry ID (`claude-acp`, `codex-acp`, `pi-acp`, `glm-acp-agent`).
    pub registry_id: &'static str,
    /// Executable command name expected by `BUILTIN_PRESETS`.
    pub binary_name: &'static str,
    /// Companion interactive CLI whose presence triggers background auto-install.
    pub companion_cli: Option<&'static str>,
    /// True when the ACP adapter shells out to `companion_cli` at runtime and cannot run without it.
    pub requires_companion_cli: bool,
    /// Fallback npm package when the ACP Registry is unreachable or omits the entry.
    pub fallback_npm_package: &'static str,
}

pub const MANAGED_ADAPTERS: &[ManagedAdapterSpec] = &[
    ManagedAdapterSpec {
        preset_id: "claude",
        registry_id: "claude-acp",
        binary_name: "claude-agent-acp",
        companion_cli: Some("claude"),
        requires_companion_cli: false,
        fallback_npm_package: "@agentclientprotocol/claude-agent-acp",
    },
    ManagedAdapterSpec {
        preset_id: "codex",
        registry_id: "codex-acp",
        binary_name: "codex-acp",
        companion_cli: Some("codex"),
        requires_companion_cli: false,
        fallback_npm_package: "@agentclientprotocol/codex-acp",
    },
    ManagedAdapterSpec {
        preset_id: "pi",
        registry_id: "pi-acp",
        binary_name: "pi-acp",
        companion_cli: Some("pi"),
        requires_companion_cli: true,
        fallback_npm_package: "pi-acp",
    },
    ManagedAdapterSpec {
        preset_id: "glm",
        registry_id: "glm-acp-agent",
        binary_name: "glm-acp-agent",
        companion_cli: None,
        requires_companion_cli: false,
        fallback_npm_package: "glm-acp-agent",
    },
];

pub fn lookup_spec(preset_or_bin: &str) -> Option<&'static ManagedAdapterSpec> {
    let norm = preset_or_bin.trim().to_ascii_lowercase();
    let base = norm.rsplit(['/', '\\']).next().unwrap_or(&norm);
    MANAGED_ADAPTERS.iter().find(|spec| {
        spec.preset_id == base
            || spec.registry_id == base
            || spec.binary_name == base
            || spec.companion_cli == Some(base)
    })
}

pub fn lookup_spec_for_preset_or_program(
    preset_id: Option<&str>,
    program: &str,
) -> Option<&'static ManagedAdapterSpec> {
    if let Some(pid) = preset_id
        && let Some(spec) = lookup_spec(pid)
    {
        return Some(spec);
    }
    lookup_spec(program)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledMeta {
    pub preset_id: String,
    pub registry_id: String,
    pub version: Option<String>,
    pub method: String,
    pub executable: String,
    pub installed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct AdapterStatusItem {
    pub preset_id: String,
    pub registry_id: String,
    pub binary_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub companion_cli: Option<String>,
    pub companion_installed: bool,
    pub installed: bool,
    /// Where the resolved adapter comes from: `config`, `path`, `managed`, `npx_cache`, `npx_fallback`, or `missing`.
    pub source: String,
    /// Lifecycle state: `ready`, `installing`, `missing`, or `error`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AdapterStatusResult {
    pub adapters_dir: String,
    pub auto_install: bool,
    pub registry_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_registry_url: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_permissions: String,
    #[serde(default = "default_true")]
    pub inject_mcp: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub config_path: String,
    pub adapters: Vec<AdapterStatusItem>,
}

impl Default for AdapterStatusResult {
    fn default() -> Self {
        Self {
            adapters_dir: String::new(),
            auto_install: true,
            registry_url: String::new(),
            custom_registry_url: None,
            default_permissions: String::new(),
            inject_mcp: true,
            config_path: String::new(),
            adapters: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct AdapterInstallItemResult {
    pub preset_id: String,
    /// Outcome: `installed`, `updated`, `already_ready`, `skipped`, or `failed`.
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct AdapterInstallResult {
    pub adapters_dir: String,
    pub results: Vec<AdapterInstallItemResult>,
}

#[derive(Clone, Debug, Default)]
struct LiveInstallState {
    installing: bool,
    last_error: Option<String>,
}

/// Manages per-adapter single-flight locks and in-memory install progress inside a `Core`.
pub struct AdapterManager {
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    live: Mutex<HashMap<String, LiveInstallState>>,
}

impl Default for AdapterManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AdapterManager {
    pub fn new() -> Self {
        Self { locks: Mutex::new(HashMap::new()), live: Mutex::new(HashMap::new()) }
    }

    fn lock_for(&self, preset_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut guard = self.locks.lock().unwrap_or_else(|p| p.into_inner());
        guard.entry(preset_id.to_string()).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
    }

    fn set_installing(&self, preset_id: &str, installing: bool, err: Option<String>) {
        let mut guard = self.live.lock().unwrap_or_else(|p| p.into_inner());
        let entry = guard.entry(preset_id.to_string()).or_default();
        entry.installing = installing;
        if installing {
            entry.last_error = None;
        } else {
            entry.last_error = err;
        }
    }

    fn live_state(&self, preset_id: &str) -> LiveInstallState {
        self.live.lock().unwrap_or_else(|p| p.into_inner()).get(preset_id).cloned().unwrap_or_default()
    }

    /// Waits for an in-flight install (if any) and ensures a managed adapter exists when
    /// `auto_install_adapters` is enabled and no PATH/config/managed binary is ready yet.
    pub async fn ensure_for_launch(
        &self,
        core: &Core,
        preset_id: Option<&str>,
        argv: &[String],
        env: &BTreeMap<String, String>,
    ) {
        let Some(first) = argv.first() else {
            return;
        };
        let Some(spec) = lookup_spec_for_preset_or_program(preset_id, first) else {
            return;
        };
        let path_override = env_ignore_case(env, "PATH");
        let pathext_override = env_ignore_case(env, "PATHEXT");

        // User-installed PATH binary always wins immediately.
        if resolve_non_shim_binary_in_path(spec.binary_name, path_override, pathext_override).is_some() {
            return;
        }

        // Wait on the per-adapter lock so a launch racing with background startup pre-install
        // cleanly joins that install instead of spawning a duplicate download or premature npx.
        let lock = self.lock_for(spec.preset_id);
        let _guard = lock.lock().await;

        if resolve_managed_binary_in_env(Some(spec.preset_id), spec.binary_name, env).is_some() {
            return;
        }

        let cfg = crate::config::load(&core.paths.config).unwrap_or_default();
        if !cfg.auto_install_adapters() {
            return;
        }
        // Respect explicit user `[agents]` override in config.toml.
        if cfg.agents.contains_key(spec.preset_id) {
            return;
        }

        let adapters_dir = adapters_root_in_env(env);
        let registry_url = resolve_registry_url(env, &cfg);
        let _ = self.install_locked(spec, &adapters_dir, &registry_url, env, false).await;
    }

    /// Collects current availability, installation source, and in-flight state for all managed adapters.
    pub async fn status(&self, core: &Core) -> AdapterStatusResult {
        let cfg = crate::config::load(&core.paths.config).unwrap_or_default();
        let env = crate::platform::program_env().await;
        let mut res = self.status_in_env(&cfg, &env);
        res.config_path = core.paths.config.display().to_string();
        res
    }

    pub fn status_in_env(&self, cfg: &crate::config::Config, env: &BTreeMap<String, String>) -> AdapterStatusResult {
        let adapters_dir = adapters_root_in_env(env);
        let registry_url = resolve_registry_url(env, cfg);
        let path_override = env_ignore_case(env, "PATH");
        let pathext_override = env_ignore_case(env, "PATHEXT");
        let npx_available = resolve_non_shim_binary_in_path("npx", path_override, pathext_override).is_some();

        let mut items = Vec::with_capacity(MANAGED_ADAPTERS.len());
        for spec in MANAGED_ADAPTERS {
            let companion_installed = spec
                .companion_cli
                .is_some_and(|cli| resolve_non_shim_binary_in_path(cli, path_override, pathext_override).is_some());
            let live = self.live_state(spec.preset_id);

            let (installed, source, path, version, method) = if let Some(custom) = cfg.agents.get(spec.preset_id)
                && let Some(exe) = custom.first()
                && let Some(found) = resolve_non_shim_binary_in_path(exe, path_override, pathext_override)
            {
                (true, "config".to_string(), Some(found.display().to_string()), None, Some("config".into()))
            } else if let Some((found, meta)) = resolve_managed_with_meta(&adapters_dir, spec, pathext_override) {
                (
                    true,
                    "managed".to_string(),
                    Some(found.display().to_string()),
                    meta.as_ref().and_then(|m| m.version.clone()),
                    meta.as_ref().map(|m| m.method.clone()),
                )
            } else if let Some(found) =
                resolve_non_shim_binary_in_path(spec.binary_name, path_override, pathext_override)
            {
                (true, "path".to_string(), Some(found.display().to_string()), None, Some("path".into()))
            } else if let Some(cached) = resolve_in_npx_cache(spec.binary_name, env, pathext_override) {
                (true, "npx_cache".to_string(), Some(cached.display().to_string()), None, Some("npx_cache".into()))
            } else if companion_installed && npx_available {
                (false, "npx_fallback".to_string(), None, None, None)
            } else {
                (false, "missing".to_string(), None, None, None)
            };

            let state = if live.installing {
                "installing".to_string()
            } else if installed {
                "ready".to_string()
            } else if live.last_error.is_some() {
                "error".to_string()
            } else {
                "missing".to_string()
            };

            items.push(AdapterStatusItem {
                preset_id: spec.preset_id.into(),
                registry_id: spec.registry_id.into(),
                binary_name: spec.binary_name.into(),
                companion_cli: spec.companion_cli.map(String::from),
                companion_installed,
                installed,
                source,
                state,
                path,
                version,
                method,
                // Keep already-working adapters clean in status; failed manual updates still surface
                // their detailed error in the `adapter.update` / `adapter.install` RPC response.
                error: if installed { None } else { live.last_error },
            });
        }

        AdapterStatusResult {
            adapters_dir: adapters_dir.display().to_string(),
            auto_install: cfg.auto_install_adapters(),
            registry_url,
            custom_registry_url: cfg
                .acp_registry_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from),
            default_permissions: cfg.default_permissions.unwrap_or_default().as_str().into(),
            inject_mcp: cfg.inject_mcp(),
            config_path: String::new(),
            adapters: items,
        }
    }

    /// Installs or updates one or more managed adapters.
    pub async fn install_many(
        &self,
        core: &Arc<Core>,
        presets: &[String],
        all: bool,
        detected_only: bool,
        force: bool,
        refresh_registry: bool,
    ) -> Result<AdapterInstallResult, RpcError> {
        let cfg = crate::config::load(&core.paths.config)?;
        let env = crate::platform::program_env().await;
        let adapters_dir = adapters_root_in_env(&env);
        let registry_url = resolve_registry_url(&env, &cfg);
        let path_override = env_ignore_case(&env, "PATH");
        let pathext_override = env_ignore_case(&env, "PATHEXT");

        let selected: Vec<&'static ManagedAdapterSpec> = if !presets.is_empty() {
            let mut out = Vec::new();
            for name in presets {
                let spec = lookup_spec(name).ok_or_else(|| {
                    RpcError::invalid_params(format!(
                        "unknown managed adapter {name:?} (available: {})",
                        MANAGED_ADAPTERS.iter().map(|s| s.preset_id).collect::<Vec<_>>().join(", ")
                    ))
                })?;
                if !out.iter().any(|s: &&ManagedAdapterSpec| s.preset_id == spec.preset_id) {
                    out.push(spec);
                }
            }
            out
        } else if all {
            MANAGED_ADAPTERS.iter().collect()
        } else if detected_only {
            MANAGED_ADAPTERS
                .iter()
                .filter(|spec| {
                    spec.companion_cli.is_some_and(|cli| {
                        resolve_non_shim_binary_in_path(cli, path_override, pathext_override).is_some()
                    })
                })
                .collect()
        } else {
            // Default: any detected companion CLI OR already-managed adapter (useful for `update`).
            MANAGED_ADAPTERS
                .iter()
                .filter(|spec| {
                    spec.companion_cli.is_some_and(|cli| {
                        resolve_non_shim_binary_in_path(cli, path_override, pathext_override).is_some()
                    }) || resolve_managed_with_meta(&adapters_dir, spec, pathext_override).is_some()
                })
                .collect()
        };

        let mut results = Vec::with_capacity(selected.len());
        let mut any_changed = false;
        for spec in selected {
            let lock = self.lock_for(spec.preset_id);
            let _guard = lock.lock().await;

            if !force {
                if let Some(found) = resolve_non_shim_binary_in_path(spec.binary_name, path_override, pathext_override)
                {
                    results.push(AdapterInstallItemResult {
                        preset_id: spec.preset_id.into(),
                        action: "already_ready".into(),
                        method: Some("path".into()),
                        version: None,
                        path: Some(found.display().to_string()),
                        error: None,
                    });
                    continue;
                }
                if !refresh_registry
                    && let Some((found, meta)) = resolve_managed_with_meta(&adapters_dir, spec, pathext_override)
                {
                    results.push(AdapterInstallItemResult {
                        preset_id: spec.preset_id.into(),
                        action: "already_ready".into(),
                        method: meta.as_ref().map(|m| m.method.clone()),
                        version: meta.as_ref().and_then(|m| m.version.clone()),
                        path: Some(found.display().to_string()),
                        error: None,
                    });
                    continue;
                }
            }

            let had_managed = resolve_managed_with_meta(&adapters_dir, spec, pathext_override).is_some();
            match self.install_locked(spec, &adapters_dir, &registry_url, &env, refresh_registry).await {
                Ok((bin_path, meta)) => {
                    any_changed = true;
                    core.drove.clear_probe_error_for_agent(spec.preset_id);
                    results.push(AdapterInstallItemResult {
                        preset_id: spec.preset_id.into(),
                        action: if had_managed { "updated" } else { "installed" }.into(),
                        method: Some(meta.method),
                        version: meta.version,
                        path: Some(bin_path.display().to_string()),
                        error: None,
                    });
                }
                Err(err) => {
                    results.push(AdapterInstallItemResult {
                        preset_id: spec.preset_id.into(),
                        action: "failed".into(),
                        method: None,
                        version: None,
                        path: None,
                        error: Some(err),
                    });
                }
            }
        }

        if any_changed {
            let _ = core.drove.rpc_probe(true).await;
        }

        Ok(AdapterInstallResult { adapters_dir: adapters_dir.display().to_string(), results })
    }

    async fn install_locked(
        &self,
        spec: &'static ManagedAdapterSpec,
        adapters_dir: &Path,
        registry_url: &str,
        env: &BTreeMap<String, String>,
        refresh_registry: bool,
    ) -> Result<(PathBuf, InstalledMeta), String> {
        self.set_installing(spec.preset_id, true, None);
        let dir = adapters_dir.to_path_buf();
        let url = registry_url.to_string();
        let env_cloned = env.clone();
        let res = tokio::task::spawn_blocking(move || {
            install_adapter_sync(spec, &dir, &url, &env_cloned, refresh_registry)
        })
        .await
        .map_err(|e| format!("adapter installer task failed: {e}"))
        .and_then(|r| r);

        match &res {
            Ok(_) => self.set_installing(spec.preset_id, false, None),
            Err(e) => self.set_installing(spec.preset_id, false, Some(e.clone())),
        }
        res
    }
}

/// Starts the daemon background pre-installer: scans installed companion CLIs (`claude`, `codex`, `pi`)
/// and silently installs missing managed ACP adapters when `auto_install_adapters` is enabled.
pub fn start_background(core: &Arc<Core>) {
    if std::env::var("HERDR2_AUTO_INSTALL_ADAPTERS")
        .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no"))
    {
        return;
    }
    let core = core.clone();
    tokio::spawn(async move {
        // Brief startup yield so daemon socket serving and kept-session recovery take priority.
        tokio::time::sleep(Duration::from_secs(2)).await;
        if core.lock().stopping {
            return;
        }
        let cfg = crate::config::load(&core.paths.config).unwrap_or_default();
        if !cfg.auto_install_adapters() {
            return;
        }
        let env = crate::platform::program_env().await;
        let adapters_dir = adapters_root_in_env(&env);
        let registry_url = resolve_registry_url(&env, &cfg);
        let path_override = env_ignore_case(&env, "PATH");
        let pathext_override = env_ignore_case(&env, "PATHEXT");

        let mut any_installed = false;
        for spec in MANAGED_ADAPTERS {
            if core.lock().stopping {
                break;
            }
            let Some(companion) = spec.companion_cli else {
                continue;
            };
            if cfg.agents.contains_key(spec.preset_id) {
                continue;
            }
            if resolve_non_shim_binary_in_path(companion, path_override, pathext_override).is_none() {
                continue;
            }
            if resolve_non_shim_binary_in_path(spec.binary_name, path_override, pathext_override).is_some() {
                continue;
            }
            if resolve_managed_with_meta(&adapters_dir, spec, pathext_override).is_some() {
                continue;
            }

            let lock = core.adapters.lock_for(spec.preset_id);
            let _guard = lock.lock().await;
            if resolve_managed_with_meta(&adapters_dir, spec, pathext_override).is_some() {
                continue;
            }

            eprintln!(
                "core: detected `{companion}` without `{}`; installing managed ACP adapter into {}",
                spec.binary_name,
                adapters_dir.display()
            );
            match core.adapters.install_locked(spec, &adapters_dir, &registry_url, &env, false).await {
                Ok((bin, meta)) => {
                    any_installed = true;
                    core.drove.clear_probe_error_for_agent(spec.preset_id);
                    eprintln!(
                        "core: installed managed ACP adapter `{}` ({}, {}) at {}",
                        spec.preset_id,
                        meta.method,
                        meta.version.as_deref().unwrap_or("unknown"),
                        bin.display()
                    );
                }
                Err(err) => {
                    eprintln!("core: auto-install for `{}` adapter did not complete: {err}", spec.preset_id);
                }
            }
        }

        if any_installed && !core.lock().stopping {
            let _ = core.drove.rpc_probe(true).await;
        }
    });
}

fn env_ignore_case<'a>(env: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str()).filter(|s| !s.is_empty())
}

fn env_or_process(env: &BTreeMap<String, String>, key: &str) -> Option<String> {
    if let Some(v) = env_ignore_case(env, key).map(str::trim).filter(|s| !s.is_empty()) {
        return Some(v.to_string());
    }
    std::env::vars()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn resolve_registry_url(env: &BTreeMap<String, String>, cfg: &crate::config::Config) -> String {
    env_or_process(env, "HERDR2_ACP_REGISTRY_URL")
        .or_else(|| cfg.acp_registry_url.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from))
        .unwrap_or_else(|| DEFAULT_ACP_REGISTRY_URL.to_string())
}

/// Shared per-user managed adapters directory (`~/.local/share/herdr2/adapters` or `%LOCALAPPDATA%\herdr2\adapters`).
pub fn adapters_root_in_env(env: &BTreeMap<String, String>) -> PathBuf {
    if let Some(explicit) = env_or_process(env, "HERDR2_ADAPTERS_DIR") {
        return PathBuf::from(explicit);
    }
    if cfg!(windows) {
        if let Some(local) = env_or_process(env, "LOCALAPPDATA") {
            return PathBuf::from(local).join("herdr2").join("adapters");
        }
        if let Some(profile) = env_or_process(env, "USERPROFILE") {
            return PathBuf::from(profile).join("AppData").join("Local").join("herdr2").join("adapters");
        }
        return crate::platform::data_dirs().0.join("herdr2").join("adapters");
    }
    if let Some(xdg) = env_or_process(env, "XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("herdr2").join("adapters");
    }
    if let Some(home) = env_or_process(env, "HOME") {
        return PathBuf::from(home).join(".local").join("share").join("herdr2").join("adapters");
    }
    let (state_home, _) = crate::platform::data_dirs();
    state_home.parent().unwrap_or(&state_home).join("share").join("herdr2").join("adapters")
}

/// Resolves a managed adapter binary for `(preset_id, program)` inside the managed adapters root.
pub(crate) fn resolve_managed_binary_in_env(
    preset_id: Option<&str>,
    program: &str,
    env: &BTreeMap<String, String>,
) -> Option<PathBuf> {
    if program.contains('/') || program.contains('\\') {
        return None;
    }
    let spec = lookup_spec_for_preset_or_program(preset_id, program)?;
    let lower_prog = program.trim().to_ascii_lowercase();
    let stem = lower_prog
        .strip_suffix(".exe")
        .or_else(|| lower_prog.strip_suffix(".cmd"))
        .or_else(|| lower_prog.strip_suffix(".bat"))
        .unwrap_or(&lower_prog);
    if stem != spec.binary_name {
        return None;
    }
    let root = adapters_root_in_env(env);
    let pathext_override = env_ignore_case(env, "PATHEXT");
    resolve_managed_with_meta(&root, spec, pathext_override).map(|(p, _)| p)
}

fn resolve_managed_with_meta(
    adapters_root: &Path,
    spec: &ManagedAdapterSpec,
    pathext_override: Option<&str>,
) -> Option<(PathBuf, Option<InstalledMeta>)> {
    let dir = adapters_root.join(spec.preset_id);
    if !dir.is_dir() {
        return None;
    }
    let meta_path = dir.join("meta.json");
    let meta: Option<InstalledMeta> =
        std::fs::read(&meta_path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());

    if let Some(ref m) = meta {
        let rel = m.executable.trim();
        if !rel.is_empty() {
            let p = dir.join(rel);
            if p.is_file() {
                return Some((p, meta));
            }
            if let (Some(parent), Some(name)) = (p.parent(), p.file_name().and_then(|s| s.to_str()))
                && let Some(cand) = candidate_files_in_dir(parent, name, pathext_override).into_iter().find(|c| c.is_file())
            {
                return Some((cand, meta));
            }
        }
    }

    // Fallback scan for npm prefix layout or standalone binary layout without meta.json.
    for search_dir in [dir.join("node_modules/.bin"), dir.clone()] {
        if search_dir.is_dir()
            && let Some(cand) =
                candidate_files_in_dir(&search_dir, spec.binary_name, pathext_override).into_iter().find(|c| c.is_file())
        {
            return Some((cand, meta));
        }
    }
    None
}

// --- Official ACP Registry parsing & download ------------------------------

#[derive(Debug, Deserialize)]
struct RegistryIndex {
    #[serde(default)]
    agents: Vec<RegistryAgent>,
}

#[derive(Debug, Deserialize)]
struct RegistryAgent {
    id: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    distribution: RegistryDistribution,
}

#[derive(Debug, Default, Deserialize)]
struct RegistryDistribution {
    #[serde(default)]
    binary: BTreeMap<String, BinaryTarget>,
    #[serde(default)]
    npx: Option<NpxDistribution>,
}

#[derive(Debug, Clone, Deserialize)]
struct BinaryTarget {
    archive: String,
    cmd: String,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct NpxDistribution {
    package: String,
}

fn current_registry_platform_keys() -> Vec<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        vec!["darwin-aarch64"]
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        vec!["darwin-x86_64"]
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        vec!["linux-x86_64", "linux-x86_64-musl"]
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        vec!["linux-aarch64", "linux-aarch64-musl"]
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        vec!["windows-x86_64"]
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        vec!["windows-aarch64", "windows-x86_64"]
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
    )))]
    {
        vec![]
    }
}

fn select_binary_target<'a>(
    targets: &'a BTreeMap<String, BinaryTarget>,
    keys: &[&str],
) -> Option<&'a BinaryTarget> {
    for key in keys {
        if let Some(t) = targets.get(*key) {
            return Some(t);
        }
    }
    None
}

fn load_registry_index(
    adapters_dir: &Path,
    registry_url: &str,
    env: &BTreeMap<String, String>,
    force_refresh: bool,
) -> Option<RegistryIndex> {
    let cache_path = adapters_dir.join("registry-cache.json");
    if !force_refresh
        && let Ok(meta) = std::fs::metadata(&cache_path)
        && let Ok(modified) = meta.modified()
        && SystemTime::now().duration_since(modified).is_ok_and(|age| age < REGISTRY_CACHE_TTL)
        && let Ok(bytes) = std::fs::read(&cache_path)
        && let Ok(parsed) = serde_json::from_slice::<RegistryIndex>(&bytes)
    {
        return Some(parsed);
    }

    let fetched_bytes = if let Some(local_path) = registry_url.strip_prefix("file://") {
        std::fs::read(local_path).ok()
    } else if Path::new(registry_url).is_file() {
        std::fs::read(registry_url).ok()
    } else {
        let _ = std::fs::create_dir_all(adapters_dir);
        let tmp = adapters_dir.join(format!(".registry-{}.tmp", std::process::id()));
        let ok = curl_download(registry_url, &tmp, env, REGISTRY_TIMEOUT_SECS, false).is_ok();
        let bytes = if ok { std::fs::read(&tmp).ok() } else { None };
        let _ = std::fs::remove_file(&tmp);
        bytes
    };

    if let Some(bytes) = fetched_bytes
        && let Ok(parsed) = serde_json::from_slice::<RegistryIndex>(&bytes)
    {
        let _ = std::fs::create_dir_all(adapters_dir);
        let _ = crate::paths::write_atomic(&cache_path, &bytes);
        return Some(parsed);
    }

    // Fallback to stale cache when offline.
    std::fs::read(&cache_path).ok().and_then(|bytes| serde_json::from_slice::<RegistryIndex>(&bytes).ok())
}

fn install_adapter_sync(
    spec: &ManagedAdapterSpec,
    adapters_dir: &Path,
    registry_url: &str,
    env: &BTreeMap<String, String>,
    refresh_registry: bool,
) -> Result<(PathBuf, InstalledMeta), String> {
    std::fs::create_dir_all(adapters_dir)
        .map_err(|e| format!("cannot create adapters directory {}: {e}", adapters_dir.display()))?;

    let registry = load_registry_index(adapters_dir, registry_url, env, refresh_registry);
    let reg_agent = registry.as_ref().and_then(|idx| idx.agents.iter().find(|a| a.id == spec.registry_id));

    let platform_keys = current_registry_platform_keys();
    let mut binary_error: Option<String> = None;

    // 1. Prefer standalone binary archive from official ACP Registry (zero Node.js dependency).
    if let Some(agent) = reg_agent
        && let Some(target) = select_binary_target(&agent.distribution.binary, &platform_keys)
    {
        match install_binary_archive(spec, adapters_dir, target, agent.version.as_deref(), env) {
            Ok(ok) => return Ok(ok),
            Err(err) => {
                binary_error = Some(err);
            }
        }
    }

    // 2. Fallback to isolated `npm install --prefix` into the managed adapter directory.
    let npm_pkg = reg_agent
        .and_then(|a| a.distribution.npx.as_ref())
        .map(|n| n.package.clone())
        .unwrap_or_else(|| format!("{}@latest", spec.fallback_npm_package));
    let reg_version = reg_agent.and_then(|a| a.version.clone());

    match install_via_npm_prefix(spec, adapters_dir, &npm_pkg, reg_version.as_deref(), env) {
        Ok(ok) => Ok(ok),
        Err(npm_err) => match binary_error {
            Some(bin_err) => Err(format!("binary install failed ({bin_err}); npm fallback failed ({npm_err})")),
            None => Err(npm_err),
        },
    }
}

fn staging_dir_for(adapters_dir: &Path, preset_id: &str) -> PathBuf {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    adapters_dir.join(format!(".staging-{preset_id}-{}-{nonce}", std::process::id()))
}

fn swap_directory_atomic(staging: &Path, dest: &Path) -> Result<(), String> {
    let backup = dest.with_file_name(format!(
        ".backup-{}-{}",
        dest.file_name().and_then(|s| s.to_str()).unwrap_or("adapter"),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&backup);
    let had_dest = dest.exists();
    if had_dest {
        std::fs::rename(dest, &backup)
            .map_err(|e| format!("cannot move previous adapter out of {}: {e}", dest.display()))?;
    }
    if let Err(e) = std::fs::rename(staging, dest) {
        if had_dest {
            let _ = std::fs::rename(&backup, dest);
        }
        return Err(format!("cannot activate managed adapter at {}: {e}", dest.display()));
    }
    if had_dest {
        let _ = std::fs::remove_dir_all(&backup);
    }
    Ok(())
}

fn install_binary_archive(
    spec: &ManagedAdapterSpec,
    adapters_dir: &Path,
    target: &BinaryTarget,
    version: Option<&str>,
    env: &BTreeMap<String, String>,
) -> Result<(PathBuf, InstalledMeta), String> {
    let staging = staging_dir_for(adapters_dir, spec.preset_id);
    std::fs::create_dir_all(&staging).map_err(|e| format!("cannot create staging dir: {e}"))?;

    let result = (|| -> Result<(PathBuf, InstalledMeta), String> {
        let archive_name = target.archive.rsplit('/').next().unwrap_or("adapter.archive");
        let archive_path = staging.join(archive_name);

        if let Some(local_path) = target.archive.strip_prefix("file://") {
            std::fs::copy(local_path, &archive_path)
                .map_err(|e| format!("cannot copy local archive {local_path}: {e}"))?;
        } else {
            curl_download(&target.archive, &archive_path, env, DOWNLOAD_TIMEOUT_SECS, true)?;
        }

        if let Some(expected_sha) = target.sha256.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let actual = sha256_file(&archive_path)
                .ok_or_else(|| format!("cannot compute SHA-256 for {}", archive_path.display()))?;
            if !actual.eq_ignore_ascii_case(expected_sha) {
                return Err(format!(
                    "SHA-256 mismatch for {}: expected {expected_sha}, got {actual}",
                    target.archive
                ));
            }
        }

        let extract_dir = staging.join("pkg");
        std::fs::create_dir_all(&extract_dir).map_err(|e| format!("cannot create extract dir: {e}"))?;
        extract_archive(&archive_path, &extract_dir, env)?;
        let _ = std::fs::remove_file(&archive_path);

        let pathext_override = env_ignore_case(env, "PATHEXT");
        let exe_rel = locate_extracted_cmd(&extract_dir, &target.cmd, spec.binary_name, pathext_override)?;
        let staged_exe = extract_dir.join(&exe_rel);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&staged_exe, std::fs::Permissions::from_mode(0o755));
        }

        let rel_from_pkg = PathBuf::from("pkg").join(&exe_rel);
        let meta = InstalledMeta {
            preset_id: spec.preset_id.into(),
            registry_id: spec.registry_id.into(),
            version: version.map(String::from),
            method: "binary".into(),
            executable: rel_from_pkg.to_string_lossy().replace('\\', "/"),
            installed_at_ms: now_ms(),
            source: Some(target.archive.clone()),
        };
        let meta_bytes = serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?;
        std::fs::write(staging.join("meta.json"), meta_bytes).map_err(|e| format!("cannot write meta.json: {e}"))?;

        let dest = adapters_dir.join(spec.preset_id);
        swap_directory_atomic(&staging, &dest)?;
        let final_exe = dest.join(&rel_from_pkg);
        Ok((final_exe, meta))
    })();

    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn install_via_npm_prefix(
    spec: &ManagedAdapterSpec,
    adapters_dir: &Path,
    package_spec: &str,
    registry_version: Option<&str>,
    env: &BTreeMap<String, String>,
) -> Result<(PathBuf, InstalledMeta), String> {
    let path_override = env_ignore_case(env, "PATH");
    let pathext_override = env_ignore_case(env, "PATHEXT");
    let npm_bin = resolve_non_shim_binary_in_path("npm", path_override, pathext_override).ok_or_else(|| {
        format!(
            "cannot install `{}` adapter automatically: `npm` is not on PATH (install Node.js or install `{}` manually)",
            spec.preset_id, spec.binary_name
        )
    })?;

    let staging = staging_dir_for(adapters_dir, spec.preset_id);
    std::fs::create_dir_all(&staging).map_err(|e| format!("cannot create staging dir: {e}"))?;

    let result = (|| -> Result<(PathBuf, InstalledMeta), String> {
        let mut cmd = crate::platform::bg_command(&npm_bin);
        cmd.args(["install", "--no-audit", "--no-fund", "--loglevel=error", "--prefix"])
            .arg(&staging)
            .arg(package_spec)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        apply_env_to_std_cmd(&mut cmd, env);

        let output = run_cmd_with_timeout(cmd, INSTALL_TIMEOUT, &format!("npm install {package_spec}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let detail = if !stderr.is_empty() { stderr } else { stdout };
            return Err(format!(
                "`npm install {package_spec}` exited with {}{}",
                output.status.code().map_or_else(|| "signal".into(), |c| c.to_string()),
                if detail.is_empty() { String::new() } else { format!(": {detail}") }
            ));
        }

        let bin_dir = staging.join("node_modules/.bin");
        let found_bin = candidate_files_in_dir(&bin_dir, spec.binary_name, pathext_override)
            .into_iter()
            .find(|c| c.is_file())
            .ok_or_else(|| {
                format!(
                    "`npm install {package_spec}` succeeded but `{}` was not found in {}",
                    spec.binary_name,
                    bin_dir.display()
                )
            })?;

        let rel_exe = found_bin
            .strip_prefix(&staging)
            .map_err(|_| "failed computing relative adapter path".to_string())?
            .to_path_buf();

        let installed_ver = read_npm_installed_version(&staging, spec.fallback_npm_package)
            .or_else(|| registry_version.map(String::from));

        let meta = InstalledMeta {
            preset_id: spec.preset_id.into(),
            registry_id: spec.registry_id.into(),
            version: installed_ver,
            method: "npm".into(),
            executable: rel_exe.to_string_lossy().replace('\\', "/"),
            installed_at_ms: now_ms(),
            source: Some(package_spec.to_string()),
        };
        let meta_bytes = serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?;
        std::fs::write(staging.join("meta.json"), meta_bytes).map_err(|e| format!("cannot write meta.json: {e}"))?;

        let dest = adapters_dir.join(spec.preset_id);
        swap_directory_atomic(&staging, &dest)?;
        let final_exe = dest.join(&rel_exe);
        Ok((final_exe, meta))
    })();

    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn read_npm_installed_version(prefix: &Path, pkg_name: &str) -> Option<String> {
    let mut pkg_json = prefix.join("node_modules");
    for seg in pkg_name.split('/') {
        pkg_json.push(seg);
    }
    pkg_json.push("package.json");
    let raw = std::fs::read_to_string(pkg_json).ok()?;
    let val: serde_json::Value = serde_json::from_str(&raw).ok()?;
    val.get("version").and_then(serde_json::Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

fn locate_extracted_cmd(
    extract_dir: &Path,
    cmd_hint: &str,
    binary_name: &str,
    pathext_override: Option<&str>,
) -> Result<PathBuf, String> {
    let clean_hint = cmd_hint.trim().trim_start_matches("./").trim_start_matches(".\\");
    if !clean_hint.is_empty() {
        let direct = extract_dir.join(clean_hint);
        if direct.is_file() {
            return Ok(PathBuf::from(clean_hint));
        }
        if let (Some(parent), Some(name)) = (direct.parent(), direct.file_name().and_then(|s| s.to_str()))
            && let Some(cand) = candidate_files_in_dir(parent, name, pathext_override).into_iter().find(|c| c.is_file())
            && let Ok(rel) = cand.strip_prefix(extract_dir)
        {
            return Ok(rel.to_path_buf());
        }
    }
    if let Some(cand) =
        candidate_files_in_dir(extract_dir, binary_name, pathext_override).into_iter().find(|c| c.is_file())
        && let Ok(rel) = cand.strip_prefix(extract_dir)
    {
        return Ok(rel.to_path_buf());
    }
    // Check one directory level down (some release archives wrap files in a single top-level folder).
    if let Ok(entries) = std::fs::read_dir(extract_dir) {
        for entry in entries.flatten() {
            let sub = entry.path();
            if !sub.is_dir() {
                continue;
            }
            if !clean_hint.is_empty() {
                let nested = sub.join(clean_hint);
                if nested.is_file()
                    && let Ok(rel) = nested.strip_prefix(extract_dir)
                {
                    return Ok(rel.to_path_buf());
                }
            }
            if let Some(cand) =
                candidate_files_in_dir(&sub, binary_name, pathext_override).into_iter().find(|c| c.is_file())
                && let Ok(rel) = cand.strip_prefix(extract_dir)
            {
                return Ok(rel.to_path_buf());
            }
        }
    }
    Err(format!("extracted archive does not contain `{cmd_hint}` or `{binary_name}`"))
}

fn apply_env_to_std_cmd(cmd: &mut std::process::Command, env: &BTreeMap<String, String>) {
    if !env.is_empty() {
        cmd.env_clear();
        for (k, v) in env {
            cmd.env(k, v);
        }
    }
}

fn curl_program() -> PathBuf {
    match std::env::var_os("SystemRoot") {
        Some(root) if cfg!(windows) => Path::new(&root).join(r"System32\curl.exe"),
        _ => PathBuf::from("curl"),
    }
}

fn curl_download(
    url: &str,
    dest: &Path,
    env: &BTreeMap<String, String>,
    max_time_secs: &str,
    stall_detect: bool,
) -> Result<(), String> {
    let mut cmd = crate::platform::bg_command(curl_program());
    if cfg!(windows) {
        cmd.arg("--ssl-no-revoke");
    }
    cmd.args(["-fsSL", "--retry", "2", "--connect-timeout", "10", "--max-time", max_time_secs]);
    if stall_detect {
        cmd.args(["--speed-limit", DOWNLOAD_SPEED_LIMIT_BYTES, "--speed-time", DOWNLOAD_SPEED_TIME_SECS]);
    }
    cmd.arg("-o")
        .arg(dest)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    apply_env_to_std_cmd(&mut cmd, env);
    let out = cmd.output().map_err(|e| format!("failed to run curl for {url}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { format!("cannot download {url}") } else { format!("cannot download {url}: {err}") });
    }
    Ok(())
}

fn tar_program() -> PathBuf {
    match std::env::var_os("SystemRoot") {
        Some(root) if cfg!(windows) => Path::new(&root).join(r"System32\tar.exe"),
        _ => PathBuf::from("tar"),
    }
}

fn extract_archive(archive: &Path, dest: &Path, env: &BTreeMap<String, String>) -> Result<(), String> {
    let lower = archive.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".zip") && !cfg!(windows) {
        let path_override = env_ignore_case(env, "PATH");
        if let Some(unzip) = resolve_non_shim_binary_in_path("unzip", path_override, None) {
            let mut cmd = crate::platform::bg_command(unzip);
            cmd.args(["-qo"]).arg(archive).arg("-d").arg(dest);
            apply_env_to_std_cmd(&mut cmd, env);
            let out = cmd.output().map_err(|e| format!("unzip failed: {e}"))?;
            if out.status.success() {
                return Ok(());
            }
        }
        if let Some(py) = resolve_non_shim_binary_in_path("python3", path_override, None) {
            let mut cmd = crate::platform::bg_command(py);
            cmd.args(["-m", "zipfile", "-e"]).arg(archive).arg(dest);
            apply_env_to_std_cmd(&mut cmd, env);
            let out = cmd.output().map_err(|e| format!("python3 zipfile extract failed: {e}"))?;
            if out.status.success() {
                return Ok(());
            }
        }
    }
    let mut cmd = crate::platform::bg_command(tar_program());
    cmd.arg("-xf").arg(archive).arg("-C").arg(dest);
    apply_env_to_std_cmd(&mut cmd, env);
    let out = cmd.output().map_err(|e| format!("tar extract failed: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("failed to extract {}: {}", archive.display(), err));
    }
    Ok(())
}

#[cfg(windows)]
fn sha256_file(path: &Path) -> Option<String> {
    crate::platform::sha256(&std::fs::read(path).ok()?)
}

#[cfg(unix)]
fn sha256_file(path: &Path) -> Option<String> {
    let run = |cmd: &str, args: &[&str]| {
        crate::platform::bg_command(cmd).args(args).arg(path).output().ok().filter(|o| o.status.success())
    };
    let out = run("sha256sum", &[]).or_else(|| run("shasum", &["-a", "256"]))?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().next().map(String::from)
}

fn run_cmd_with_timeout(
    mut cmd: std::process::Command,
    timeout: Duration,
    label: &str,
) -> Result<std::process::Output, String> {
    let mut child = cmd.spawn().map_err(|e| format!("failed to spawn `{label}`: {e}"))?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child.wait_with_output().map_err(|e| format!("failed collecting output from `{label}`: {e}"));
            }
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("`{label}` timed out after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("error waiting for `{label}`: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolves_managed_binary_from_meta_and_npm_layout() {
        let root = temp_dir("h2-managed-resolve");
        let adapters_dir = root.join("adapters");
        let claude_bin_dir = adapters_dir.join("claude/pkg");
        std::fs::create_dir_all(&claude_bin_dir).unwrap();
        let claude_exe = claude_bin_dir.join(if cfg!(windows) { "claude-agent-acp.exe" } else { "claude-agent-acp" });
        std::fs::write(&claude_exe, "#!/bin/sh\n").unwrap();
        let meta = InstalledMeta {
            preset_id: "claude".into(),
            registry_id: "claude-acp".into(),
            version: Some("0.112.0".into()),
            method: "binary".into(),
            executable: format!("pkg/{}", claude_exe.file_name().unwrap().to_string_lossy()),
            installed_at_ms: 1,
            source: None,
        };
        std::fs::write(adapters_dir.join("claude/meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();

        let codex_bin_dir = adapters_dir.join("codex/node_modules/.bin");
        std::fs::create_dir_all(&codex_bin_dir).unwrap();
        let codex_exe = codex_bin_dir.join(if cfg!(windows) { "codex-acp.cmd" } else { "codex-acp" });
        std::fs::write(&codex_exe, "#!/bin/sh\n").unwrap();

        let env = BTreeMap::from([("HERDR2_ADAPTERS_DIR".into(), adapters_dir.display().to_string())]);
        assert_eq!(resolve_managed_binary_in_env(Some("claude"), "claude-agent-acp", &env), Some(claude_exe));
        assert_eq!(resolve_managed_binary_in_env(Some("codex"), "codex-acp", &env), Some(codex_exe));
        assert_eq!(resolve_managed_binary_in_env(Some("pi"), "pi-acp", &env), None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn installs_standalone_binary_archive_with_sha256_verification() {
        let root = temp_dir("h2-managed-binary-install");
        let src_dir = root.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let fake_bin = src_dir.join("claude-agent-acp");
        std::fs::write(&fake_bin, "#!/bin/sh\necho ok\n").unwrap();

        let archive = root.join("claude-agent-acp.tar.gz");
        let status = std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .arg("-C")
            .arg(&src_dir)
            .arg("claude-agent-acp")
            .status()
            .unwrap();
        assert!(status.success());
        let sha = sha256_file(&archive).expect("sha256");

        let keys = current_registry_platform_keys();
        assert!(!keys.is_empty());
        let reg_json = serde_json::json!({
            "version": "1.0.0",
            "agents": [{
                "id": "claude-acp",
                "version": "0.112.0",
                "distribution": {
                    "binary": {
                        keys[0]: {
                            "archive": format!("file://{}", archive.display()),
                            "cmd": "./claude-agent-acp",
                            "sha256": sha
                        }
                    }
                }
            }]
        });
        let reg_file = root.join("registry.json");
        std::fs::write(&reg_file, serde_json::to_vec(&reg_json).unwrap()).unwrap();

        let adapters_dir = root.join("adapters");
        let env = BTreeMap::from([("HERDR2_ADAPTERS_DIR".into(), adapters_dir.display().to_string())]);
        let spec = lookup_spec("claude").unwrap();

        let (installed_exe, meta) =
            install_adapter_sync(spec, &adapters_dir, &format!("file://{}", reg_file.display()), &env, true).unwrap();
        assert!(installed_exe.is_file());
        assert_eq!(meta.method, "binary");
        assert_eq!(meta.version.as_deref(), Some("0.112.0"));
        assert_eq!(resolve_managed_binary_in_env(Some("claude"), "claude-agent-acp", &env), Some(installed_exe));

        // Corrupt sha256 must fail safely without breaking existing install.
        let bad_reg = serde_json::json!({
            "version": "1.0.0",
            "agents": [{
                "id": "claude-acp",
                "version": "0.113.0",
                "distribution": {
                    "binary": {
                        keys[0]: {
                            "archive": format!("file://{}", archive.display()),
                            "cmd": "./claude-agent-acp",
                            "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
                        }
                    }
                }
            }]
        });
        std::fs::write(&reg_file, serde_json::to_vec(&bad_reg).unwrap()).unwrap();
        let err = install_adapter_sync(spec, &adapters_dir, &format!("file://{}", reg_file.display()), &env, true)
            .unwrap_err();
        assert!(err.contains("SHA-256 mismatch"), "unexpected error: {err}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn installs_via_fake_npm_prefix_fallback() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir("h2-managed-npm-install");
        let fake_bin_dir = root.join("bin");
        std::fs::create_dir_all(&fake_bin_dir).unwrap();
        let fake_npm = fake_bin_dir.join("npm");
        let script = r#"#!/bin/sh
prefix=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--prefix" ]; then
    shift
    prefix="$1"
  fi
  shift
done
mkdir -p "$prefix/node_modules/.bin" "$prefix/node_modules/@agentclientprotocol/codex-acp"
printf '#!/bin/sh\necho codex-acp\n' > "$prefix/node_modules/.bin/codex-acp"
chmod +x "$prefix/node_modules/.bin/codex-acp"
printf '{"name":"@agentclientprotocol/codex-acp","version":"2.12.1"}' > "$prefix/node_modules/@agentclientprotocol/codex-acp/package.json"
"#;
        std::fs::write(&fake_npm, script).unwrap();
        std::fs::set_permissions(&fake_npm, std::fs::Permissions::from_mode(0o755)).unwrap();

        let reg_file = root.join("registry.json");
        let reg_json = serde_json::json!({
            "version": "1.0.0",
            "agents": [{
                "id": "codex-acp",
                "version": "2.12.1",
                "distribution": {
                    "npx": { "package": "@agentclientprotocol/codex-acp@2.12.1" }
                }
            }]
        });
        std::fs::write(&reg_file, serde_json::to_vec(&reg_json).unwrap()).unwrap();

        let adapters_dir = root.join("adapters");
        let env = BTreeMap::from([
            ("PATH".into(), format!("{}:/usr/bin:/bin", fake_bin_dir.display())),
            ("HERDR2_ADAPTERS_DIR".into(), adapters_dir.display().to_string()),
        ]);
        let spec = lookup_spec("codex").unwrap();
        let (installed_exe, meta) =
            install_adapter_sync(spec, &adapters_dir, &format!("file://{}", reg_file.display()), &env, true).unwrap();
        assert!(installed_exe.is_file());
        assert_eq!(meta.method, "npm");
        assert_eq!(meta.version.as_deref(), Some("2.12.1"));
        assert_eq!(resolve_managed_binary_in_env(Some("codex"), "codex-acp", &env), Some(installed_exe));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn status_reports_companion_cli_and_managed_source_accurately() {
        let root = temp_dir("h2-managed-status");
        let bin_dir = root.join("bin");
        let adapters_dir = root.join("adapters");
        let claude_pkg = adapters_dir.join("claude/pkg");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(&claude_pkg).unwrap();

        let claude_cli = bin_dir.join(if cfg!(windows) { "claude.exe" } else { "claude" });
        let codex_cli = bin_dir.join(if cfg!(windows) { "codex.exe" } else { "codex" });
        let npx_cli = bin_dir.join(if cfg!(windows) { "npx.cmd" } else { "npx" });
        std::fs::write(&claude_cli, "#!/bin/sh\n").unwrap();
        std::fs::write(&codex_cli, "#!/bin/sh\n").unwrap();
        std::fs::write(&npx_cli, "#!/bin/sh\n").unwrap();

        let claude_bin = claude_pkg.join(if cfg!(windows) { "claude-agent-acp.exe" } else { "claude-agent-acp" });
        std::fs::write(&claude_bin, "#!/bin/sh\n").unwrap();
        std::fs::write(
            adapters_dir.join("claude/meta.json"),
            serde_json::to_vec(&InstalledMeta {
                preset_id: "claude".into(),
                registry_id: "claude-acp".into(),
                version: Some("0.112.0".into()),
                method: "binary".into(),
                executable: format!("pkg/{}", claude_bin.file_name().unwrap().to_string_lossy()),
                installed_at_ms: 1,
                source: None,
            })
            .unwrap(),
        )
        .unwrap();

        let env = BTreeMap::from([
            ("HOME".into(), root.display().to_string()),
            ("PATH".into(), bin_dir.display().to_string()),
            ("HERDR2_ADAPTERS_DIR".into(), adapters_dir.display().to_string()),
        ]);
        let mgr = AdapterManager::new();
        let st = mgr.status_in_env(&crate::config::Config::default(), &env);
        assert!(st.auto_install);
        assert!(st.inject_mcp);
        assert!(AdapterStatusResult::default().inject_mcp);
        let parsed: AdapterStatusResult = serde_json::from_value(serde_json::json!({
            "adapters_dir": "/tmp/adapters",
            "auto_install": true,
            "registry_url": "https://example.test/registry.json",
            "adapters": []
        }))
        .unwrap();
        assert!(parsed.inject_mcp);

        let claude = st.adapters.iter().find(|a| a.preset_id == "claude").unwrap();
        assert!(claude.installed);
        assert!(claude.companion_installed);
        assert_eq!(claude.source, "managed");
        assert_eq!(claude.state, "ready");
        assert_eq!(claude.version.as_deref(), Some("0.112.0"));

        let codex = st.adapters.iter().find(|a| a.preset_id == "codex").unwrap();
        assert!(!codex.installed);
        assert!(codex.companion_installed);
        assert_eq!(codex.source, "npx_fallback");
        assert_eq!(codex.state, "missing");

        let pi = st.adapters.iter().find(|a| a.preset_id == "pi").unwrap();
        assert!(!pi.installed);
        assert!(!pi.companion_installed);
        assert_eq!(pi.source, "missing");

        // Managed adapter beats PATH binary (e.g. after explicit `adapter update`), while config wins over both.
        let path_claude = bin_dir.join(if cfg!(windows) { "claude-agent-acp.cmd" } else { "claude-agent-acp" });
        std::fs::write(&path_claude, "#!/bin/sh\n").unwrap();
        let st2 = mgr.status_in_env(&crate::config::Config::default(), &env);
        let claude2 = st2.adapters.iter().find(|a| a.preset_id == "claude").unwrap();
        assert_eq!(claude2.source, "managed");
        assert_eq!(claude2.path.as_deref(), Some(claude_bin.to_string_lossy().as_ref()));

        // A failed manual update on an already-ready adapter must not pollute status error,
        // while a failed install on a missing adapter must surface state="error" and error message.
        mgr.set_installing("claude", false, Some("download timed out".into()));
        mgr.set_installing("codex", false, Some("npm timed out".into()));
        let st3 = mgr.status_in_env(&crate::config::Config::default(), &env);
        let claude3 = st3.adapters.iter().find(|a| a.preset_id == "claude").unwrap();
        assert_eq!(claude3.state, "ready");
        assert!(claude3.error.is_none());
        let codex3 = st3.adapters.iter().find(|a| a.preset_id == "codex").unwrap();
        assert_eq!(codex3.state, "error");
        assert_eq!(codex3.error.as_deref(), Some("npm timed out"));

        // Mismatched custom binary name must not resolve to the managed adapter.
        assert_eq!(resolve_managed_binary_in_env(Some("claude"), "my-custom-claude-acp", &env), None);

        let _ = std::fs::remove_dir_all(&root);
    }
}
