//! Persistent Drove configuration (`~/.config/herdr2/drove.json`), built-in role defaults,
//! and 0600 secret storage (`<state>/drove/secrets/`).

use super::crypto;
use super::types::*;
use crate::paths::{Paths, write_atomic};
use crate::rpc::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// Resolved filesystem layout for one Drove instance inside a `herdr2` core.
#[derive(Clone, Debug)]
pub struct DrovePaths {
    pub config_file: PathBuf,
    pub state_dir: PathBuf,
    pub secrets_dir: PathBuf,
    pub items_dir: PathBuf,
    pub workers_dir: PathBuf,
    pub artifacts_dir: PathBuf,
    pub state_file: PathBuf,
    pub models_cache_file: PathBuf,
    pub ai_key_file: PathBuf,
    pub coordinator_token_file: PathBuf,
    pub web_info_file: PathBuf,
}

impl DrovePaths {
    pub fn from_paths(paths: &Paths) -> Self {
        let config_dir = paths.config.parent().unwrap_or_else(|| Path::new("."));
        let config_file = std::env::var_os("H2_DROVE_CONFIG")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| config_dir.join("drove.json"));
        let state_dir = paths.state.join("drove");
        let secrets_dir = state_dir.join("secrets");
        Self {
            config_file,
            items_dir: state_dir.join("items"),
            workers_dir: state_dir.join("workers"),
            artifacts_dir: secrets_dir.join("artifacts"),
            state_file: state_dir.join("state.json"),
            models_cache_file: state_dir.join("models-cache.json"),
            ai_key_file: secrets_dir.join("ai.key"),
            coordinator_token_file: secrets_dir.join("coordinator.token"),
            web_info_file: state_dir.join("web.info.json"),
            secrets_dir,
            state_dir,
        }
    }

    pub fn ensure_dirs(&self) -> io::Result<()> {
        if let Some(parent) = self.config_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::platform::private_dir(&self.state_dir)?;
        crate::platform::private_dir(&self.secrets_dir)?;
        crate::platform::private_dir(&self.items_dir)?;
        crate::platform::private_dir(&self.workers_dir)?;
        crate::platform::private_dir(&self.artifacts_dir)?;
        Ok(())
    }
}

pub const BUILTIN_ROLE_IDS: [&str; 6] = ["coder", "tester", "writer", "illustrator", "reviewer", "researcher"];

pub fn default_roles() -> BTreeMap<String, RoleConfig> {
    let mut roles = BTreeMap::new();
    roles.insert(
        "coder".into(),
        RoleConfig {
            label: Some("Coder".into()),
            summary: Some("核心功能实现、模块重构与 Bug 修复".into()),
            access: Some(RoleAccessMode::Writable),
            allowed_paths: Vec::new(),
            primary: RoleTarget { agent: "agy".into(), model: None, effort: Effort::High, fast: false, context: None, strengths: Vec::new() },
            fallbacks: vec![
                RoleTarget { agent: "claude".into(), model: None, effort: Effort::High, fast: false, context: None, strengths: Vec::new() },
                RoleTarget { agent: "codex".into(), model: None, effort: Effort::High, fast: false, context: None, strengths: Vec::new() },
            ],
            prompt: None,
        },
    );
    roles.insert(
        "tester".into(),
        RoleConfig {
            label: Some("Tester".into()),
            summary: Some("单元/集成测试编写、故障复现与回归验证".into()),
            access: Some(RoleAccessMode::ScopedWrite),
            allowed_paths: vec![
                "test/**".into(),
                "tests/**".into(),
                "__tests__/**".into(),
                "**/*.test.*".into(),
                "**/*.spec.*".into(),
                "**/fixtures/**".into(),
                "**/testdata/**".into(),
            ],
            primary: RoleTarget { agent: "dsh".into(), model: None, effort: Effort::Medium, fast: false, context: None, strengths: Vec::new() },
            fallbacks: vec![RoleTarget { agent: "agy".into(), model: None, effort: Effort::Medium, fast: false, context: None, strengths: Vec::new() }],
            prompt: None,
        },
    );
    roles.insert(
        "writer".into(),
        RoleConfig {
            label: Some("Writer".into()),
            summary: Some("技术文档、架构说明与图表撰写".into()),
            access: Some(RoleAccessMode::ScopedWrite),
            allowed_paths: vec![
                "docs/**".into(),
                "skills/**".into(),
                "**/*.md".into(),
                "**/*.mdx".into(),
                "**/*.rst".into(),
                "**/*.txt".into(),
                "**/*.svg".into(),
                "**/*.mmd".into(),
                "**/*.puml".into(),
            ],
            primary: RoleTarget { agent: "agy".into(), model: None, effort: Effort::Medium, fast: false, context: None, strengths: Vec::new() },
            fallbacks: vec![RoleTarget { agent: "claude".into(), model: None, effort: Effort::Medium, fast: false, context: None, strengths: Vec::new() }],
            prompt: None,
        },
    );
    roles.insert(
        "illustrator".into(),
        RoleConfig {
            label: Some("Illustrator".into()),
            summary: Some("架构图、流程图、时序图、SVG 矢量图与可视化配图绘制".into()),
            access: Some(RoleAccessMode::ScopedWrite),
            allowed_paths: vec![
                "docs/**".into(),
                "assets/**".into(),
                "images/**".into(),
                "diagrams/**".into(),
                "**/*.svg".into(),
                "**/*.png".into(),
                "**/*.webp".into(),
                "**/*.mmd".into(),
                "**/*.mermaid".into(),
                "**/*.puml".into(),
                "**/*.dot".into(),
                "**/*.drawio".into(),
                "**/*.excalidraw".into(),
                "**/*.d2".into(),
                "**/*.html".into(),
                "**/*.md".into(),
            ],
            primary: RoleTarget { agent: "agy".into(), model: None, effort: Effort::High, fast: false, context: None, strengths: Vec::new() },
            fallbacks: vec![
                RoleTarget { agent: "claude".into(), model: None, effort: Effort::High, fast: false, context: None, strengths: Vec::new() },
                RoleTarget { agent: "gemini".into(), model: None, effort: Effort::High, fast: false, context: None, strengths: Vec::new() },
            ],
            prompt: None,
        },
    );
    roles.insert(
        "reviewer".into(),
        RoleConfig {
            label: Some("Reviewer".into()),
            summary: Some("只读异构代码审查、并发安全与架构审计".into()),
            access: Some(RoleAccessMode::Readonly),
            allowed_paths: Vec::new(),
            primary: RoleTarget { agent: "codex".into(), model: None, effort: Effort::High, fast: false, context: None, strengths: Vec::new() },
            fallbacks: vec![RoleTarget { agent: "agy".into(), model: None, effort: Effort::High, fast: false, context: None, strengths: Vec::new() }],
            prompt: None,
        },
    );
    roles.insert(
        "researcher".into(),
        RoleConfig {
            label: Some("Researcher".into()),
            summary: Some("跨仓源码检索、架构摸底与日志分析".into()),
            access: Some(RoleAccessMode::Readonly),
            allowed_paths: Vec::new(),
            primary: RoleTarget { agent: "dsh".into(), model: None, effort: Effort::High, fast: false, context: None, strengths: Vec::new() },
            fallbacks: vec![RoleTarget { agent: "agy".into(), model: None, effort: Effort::High, fast: false, context: None, strengths: Vec::new() }],
            prompt: None,
        },
    );
    roles
}

impl Default for DroveConfig {
    fn default() -> Self {
        Self {
            version: 1,
            updated_at: now_iso(),
            max_concurrency_default: 4,
            roles: default_roles(),
            ai: AiSettings::default(),
            magpie: MagpieSettings::default(),
        }
    }
}

/// Parses a token window size such as `"8k"`, `"128k"`, `"1m"`, or `"32768"` (1K..=4M).
pub fn parse_ai_token_size(raw: &str) -> Option<u32> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() || s == "auto" || s == "default" {
        return None;
    }
    let val = if let Some(num) = s.strip_suffix('k') {
        let n: f64 = num.trim().parse().ok()?;
        if !n.is_finite() || n <= 0.0 {
            return None;
        }
        (n * 1024.0).round()
    } else if let Some(num) = s.strip_suffix('m') {
        let n: f64 = num.trim().parse().ok()?;
        if !n.is_finite() || n <= 0.0 {
            return None;
        }
        (n * 1_048_576.0).round()
    } else {
        let n: u32 = s.parse().ok()?;
        n as f64
    };
    if !(1024.0..=4_194_304.0).contains(&val) {
        return None;
    }
    Some(val as u32)
}

/// Validates and normalizes a `DroveConfig` before persisting or applying.
pub fn validate_config(cfg: &mut DroveConfig) -> Result<(), RpcError> {
    if cfg.max_concurrency_default < 1 || cfg.max_concurrency_default > 16 {
        return Err(RpcError::invalid_params("maxConcurrencyDefault must be between 1 and 16"));
    }
    if !cfg.ai.base_url.trim().is_empty() {
        cfg.ai.base_url = validate_http_url(&cfg.ai.base_url, "ai.baseUrl")?;
    } else {
        cfg.ai.base_url = String::new();
    }
    cfg.ai.model = cfg.ai.model.trim().to_string();
    cfg.ai.effort = cfg.ai.effort.trim().to_ascii_lowercase();
    if cfg.ai.effort.is_empty() {
        cfg.ai.effort = "low".into();
    }
    if !matches!(cfg.ai.effort.as_str(), "auto" | "none" | "low" | "medium" | "high") {
        return Err(RpcError::invalid_params("ai.effort must be auto, none, low, medium, or high"));
    }
    if let Some(ctx) = cfg.ai.context_window.take() {
        let clean = ctx.trim().to_ascii_lowercase();
        if !clean.is_empty() && clean != "auto" && clean != "default" {
            if parse_ai_token_size(&clean).is_none() {
                return Err(RpcError::invalid_params(
                    "ai.contextWindow must be auto or a token size between 1k and 4m (e.g. 8k, 32k, 128k, 1m)",
                ));
            }
            cfg.ai.context_window = Some(clean);
        }
    }
    if let Some(mt) = cfg.ai.max_tokens {
        if mt == 0 {
            cfg.ai.max_tokens = None;
        } else if !(64..=131_072).contains(&mt) {
            return Err(RpcError::invalid_params("ai.maxTokens must be between 64 and 131072"));
        }
    }
    cfg.ai.conflict_policy = cfg.ai.conflict_policy.trim().to_string();
    cfg.ai.question_mode = cfg.ai.question_mode.trim().to_string();
    cfg.ai.loop_detect = cfg.ai.loop_detect.trim().to_string();
    if !matches!(cfg.ai.conflict_policy.as_str(), "off" | "warn" | "auto_worktree") {
        return Err(RpcError::invalid_params("ai.conflictPolicy must be off, warn, or auto_worktree"));
    }
    if !matches!(cfg.ai.question_mode.as_str(), "off" | "draft" | "auto" | "auto_when_offline") {
        return Err(RpcError::invalid_params("ai.questionMode must be off, draft, auto, or auto_when_offline"));
    }
    if !matches!(cfg.ai.loop_detect.as_str(), "off" | "warn" | "steer_once") {
        return Err(RpcError::invalid_params("ai.loopDetect must be off, warn, or steer_once"));
    }
    if let Some(url) = &cfg.magpie.gateway_url {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            cfg.magpie.gateway_url = None;
        } else {
            cfg.magpie.gateway_url = Some(validate_http_url(trimmed, "magpie.gatewayUrl")?);
        }
    }
    if cfg.roles.is_empty() {
        return Err(RpcError::invalid_params("roles cannot be empty"));
    }
    for (id, role) in &mut cfg.roles {
        validate_role_id(id)?;
        role.primary.agent = role.primary.agent.trim().to_string();
        if role.primary.agent.is_empty() {
            return Err(RpcError::invalid_params(format!("role {id}: primary.agent cannot be empty")));
        }
        role.primary.strengths = normalize_strengths(&role.primary.strengths);
        for fb in &mut role.fallbacks {
            fb.agent = fb.agent.trim().to_string();
            if fb.agent.is_empty() {
                return Err(RpcError::invalid_params(format!("role {id}: fallback agent cannot be empty")));
            }
            fb.strengths = normalize_strengths(&fb.strengths);
        }
    }
    cfg.version = 1;
    cfg.updated_at = now_iso();
    Ok(())
}

/// Trims, deduplicates (case-insensitively), and caps a candidate's work-type strength tags.
pub fn normalize_strengths(raw: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in raw {
        let trimmed: String = item.trim().chars().take(64).collect();
        let clean = trimmed.trim();
        if clean.is_empty() {
            continue;
        }
        let key = clean.to_lowercase();
        if seen.insert(key) {
            out.push(clean.to_string());
            if out.len() >= 12 {
                break;
            }
        }
    }
    out
}

pub fn validate_role_id(id: &str) -> Result<(), RpcError> {
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(RpcError::invalid_params(format!("invalid role id {id:?}: use 1-32 [a-zA-Z0-9_-]")));
    }
    Ok(())
}

pub fn resolve_role_config(cfg: &DroveConfig, role: &str) -> Option<RoleConfig> {
    cfg.roles.get(role).cloned().or_else(|| default_roles().remove(role))
}

pub fn validate_http_url(raw: &str, field: &str) -> Result<String, RpcError> {
    let trimmed = raw.trim().trim_end_matches('/').to_string();
    if trimmed.chars().any(char::is_control) || trimmed.contains(char::is_whitespace) {
        return Err(RpcError::invalid_params(format!("{field}: URL contains whitespace or control chars")));
    }
    let Some(rest) = trimmed.strip_prefix("http://").or_else(|| trimmed.strip_prefix("https://")) else {
        return Err(RpcError::invalid_params(format!("{field}: URL must start with http:// or https://")));
    };
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        return Err(RpcError::invalid_params(format!("{field}: missing host in URL")));
    }
    Ok(trimmed)
}

/// Returns true when an HTTP(S) endpoint points to a local loopback host (`127.0.0.1`, `localhost`, or `::1`).
pub fn is_loopback_http_url(raw: &str) -> bool {
    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix("http://").or_else(|| trimmed.strip_prefix("https://")) else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

pub fn load_config(paths: &DrovePaths) -> Result<DroveConfig, RpcError> {
    match std::fs::read_to_string(&paths.config_file) {
        Ok(text) => {
            let mut cfg: DroveConfig = serde_json::from_str(&text)
                .map_err(|e| RpcError::new("config", format!("{}: {e}", paths.config_file.display()), "fix drove.json"))?;
            // Ensure all built-in roles exist if missing from older config, preserving user edits.
            for (k, v) in default_roles() {
                cfg.roles.entry(k).or_insert(v);
            }
            Ok(cfg)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(DroveConfig::default()),
        Err(e) => Err(RpcError::new("config", format!("{}: {e}", paths.config_file.display()), "check file permissions")),
    }
}

pub fn save_config(paths: &DrovePaths, cfg: &mut DroveConfig) -> Result<(), RpcError> {
    validate_config(cfg)?;
    if let Some(parent) = paths.config_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| RpcError::internal(format!("mkdir config dir: {e}")))?;
    }
    let bytes = serde_json::to_vec_pretty(cfg).map_err(|e| RpcError::internal(e.to_string()))?;
    write_atomic(&paths.config_file, &bytes)
        .map_err(|e| RpcError::internal(format!("write {}: {e}", paths.config_file.display())))
}

/// Writes a private 0600 secret file atomically inside `secrets_dir`.
pub fn write_secret_file(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        crate::platform::private_dir(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", &crypto::rand_id("sec")));
    {
        use std::io::Write;
        let mut f = crate::platform::private_file(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(tmp, path)
}

pub fn read_ai_key(paths: &DrovePaths) -> (Option<String>, &'static str) {
    if let Ok(v) = std::env::var("H2_DROVE_AI_API_KEY")
        && !v.trim().is_empty()
    {
        return (Some(v.trim().to_string()), "env:H2_DROVE_AI_API_KEY");
    }
    match std::fs::read_to_string(&paths.ai_key_file) {
        Ok(s) if !s.trim().is_empty() => (Some(s.trim().to_string()), "file"),
        _ => (None, "none"),
    }
}

pub fn write_ai_key(paths: &DrovePaths, key: &str) -> Result<(), RpcError> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(RpcError::invalid_params("API Key cannot be empty"));
    }
    if trimmed.contains('\n') || trimmed.contains('\r') {
        return Err(RpcError::invalid_params("API Key cannot contain newlines"));
    }
    write_secret_file(&paths.ai_key_file, &format!("{trimmed}\n"))
        .map_err(|e| RpcError::internal(format!("save AI key: {e}")))
}

pub fn clear_ai_key(paths: &DrovePaths) -> io::Result<()> {
    match std::fs::remove_file(&paths.ai_key_file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

pub fn mask_key(key: &str) -> String {
    let trimmed = key.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 8 {
        "***".into()
    } else {
        let head: String = chars[..4].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}***{tail}")
    }
}

/// Resolves role access & allowed path globs (merging built-in defaults with overrides).
pub fn role_spec_for<'a>(cfg: &'a DroveConfig, role_id: Option<&str>) -> (String, RoleConfig, RoleAccessMode, Vec<String>) {
    let id = role_id.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("coder").to_string();
    let builtins = default_roles();
    let role = cfg
        .roles
        .get(&id)
        .cloned()
        .or_else(|| builtins.get(&id).cloned())
        .unwrap_or_else(|| builtins.get("coder").cloned().expect("coder role exists"));
    let access = role.access.unwrap_or_else(|| {
        builtins
            .get(&id)
            .and_then(|b| b.access)
            .unwrap_or(RoleAccessMode::Writable)
    });
    let allowed = if !role.allowed_paths.is_empty() {
        role.allowed_paths.clone()
    } else {
        builtins.get(&id).map(|b| b.allowed_paths.clone()).unwrap_or_default()
    };
    (id, role, access, allowed)
}

/// Builds the read-only payload served to `drove-web` on `GET /api/agents`.
pub fn web_agents_payload(
    cfg: &DroveConfig,
    stats: BTreeMap<String, StatsEntry>,
    quotas: Option<Vec<QuotaCard>>,
    quota_error: Option<String>,
) -> Value {
    let mut roles_json = serde_json::Map::new();
    let mut seen_agents = BTreeMap::<String, Vec<String>>::new();
    for (id, role) in &cfg.roles {
        let mut agents = vec![format_target(&role.primary)];
        for fb in &role.fallbacks {
            let formatted = format_target(fb);
            if !agents.contains(&formatted) {
                agents.push(formatted);
            }
        }
        seen_agents.entry(role.primary.agent.clone()).or_default();
        if let Some(m) = &role.primary.model {
            let list = seen_agents.entry(role.primary.agent.clone()).or_default();
            if !list.contains(m) {
                list.push(m.clone());
            }
        }
        for fb in &role.fallbacks {
            let list = seen_agents.entry(fb.agent.clone()).or_default();
            if let Some(m) = &fb.model
                && !list.contains(m)
            {
                list.push(m.clone());
            }
        }
        let (_, _, access, allowed_paths) = role_spec_for(cfg, Some(id));
        let candidates: Vec<Value> = std::iter::once(&role.primary)
            .chain(role.fallbacks.iter())
            .map(|c| {
                json!({
                    "agent": c.agent,
                    "model": c.model,
                    "effort": c.effort,
                    "fast": c.fast,
                    "context": c.context,
                    "strengths": c.strengths,
                })
            })
            .collect();
        roles_json.insert(
            id.clone(),
            json!({
                "label": role.label.as_deref().unwrap_or(id),
                "summary": role.summary,
                "agents": agents,
                "candidates": candidates,
                "model": role.primary.model,
                "effort": role.primary.effort,
                "access": access,
                "allowedPaths": allowed_paths,
                "prompt": role.prompt,
            }),
        );
    }
    // Lightweight agent projection for TaskAssignEditor/RunOverview selectors in drove-web (no capability pool!).
    let agents_list: Vec<Value> = seen_agents
        .into_iter()
        .map(|(agent_id, models)| {
            json!({
                "id": agent_id,
                "launch": { "command": agent_id },
                "modelSelection": "session",
                "models": models.into_iter().map(|m| json!({ "id": m })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let mut out = json!({
        "profile": {
            "version": cfg.version,
            "updatedAt": cfg.updated_at,
            "agents": agents_list,
            "roles": Value::Object(roles_json),
        },
        "stats": stats,
        "magpieEnabled": cfg.magpie.enabled,
    });
    if let Some(q) = quotas {
        out["quotas"] = json!(q);
    }
    if let Some(err) = quota_error {
        out["quotaError"] = json!(err);
    }
    out
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedAgentModels {
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default)]
    pub efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_info: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_contexts: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_efforts: BTreeMap<String, Vec<String>>,
}

/// Normalizes raw effort choice strings into the canonical Drove `Effort` levels (`low`, `medium`, `high`, `xhigh`).
pub fn normalize_effort_choices(raw: &[String]) -> Vec<Effort> {
    let mut has_low = false;
    let mut has_med = false;
    let mut has_high = false;
    let mut has_xhigh = false;
    for item in raw {
        match item.trim().to_ascii_lowercase().as_str() {
            "low" | "minimal" => has_low = true,
            "medium" | "med" => has_med = true,
            "high" => has_high = true,
            "xhigh" | "extra-high" | "extra_high" | "max" | "ultra" => has_xhigh = true,
            _ => {}
        }
    }
    let mut out = Vec::new();
    if has_low {
        out.push(Effort::Low);
    }
    if has_med {
        out.push(Effort::Medium);
    }
    if has_high {
        out.push(Effort::High);
    }
    if has_xhigh {
        out.push(Effort::Xhigh);
    }
    out
}

fn strip_model_effort_and_bracket(raw: &str) -> String {
    let mut s = raw.trim().to_ascii_lowercase();
    if let Some(i) = s.find('[') {
        s.truncate(i);
    }
    s = s.trim().to_string();
    if let Some(rest) = s.strip_prefix("cursor-") {
        s = rest.to_string();
    }
    if let Some(rest) = s.strip_suffix("-fast") {
        s = rest.to_string();
    }
    for suf in ["-extra-high", "-xhigh", "-minimal", "-medium", "-none", "-high", "-low", "-med", "-max", "-thinking"] {
        if let Some(rest) = s.strip_suffix(suf) {
            s = rest.to_string();
            break;
        }
    }
    if let Some(rest) = s.strip_suffix("-thinking") {
        s = rest.to_string();
    }
    s
}

/// Returns the supported reasoning effort levels for an `(agent, model)` pair:
/// either 3 levels (`low`/`medium`/`high`) or 4 levels (`low`/`medium`/`high`/`xhigh`),
/// preferring live probed per-model metadata when present.
pub fn supported_efforts_for_model(
    agent: &str,
    model: Option<&str>,
    probed_agent: Option<&ProbedAgent>,
) -> Vec<Effort> {
    let three = vec![Effort::Low, Effort::Medium, Effort::High];
    let four = vec![Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh];

    let raw_model = model.map(str::trim).filter(|s| !s.is_empty());
    if let (Some(pa), Some(m)) = (probed_agent, raw_model) {
        let base = strip_model_effort_and_bracket(m);
        let probed_list = pa
            .model_efforts
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(m) || k.eq_ignore_ascii_case(&base))
            .map(|(_, v)| v);
        if let Some(vals) = probed_list {
            let norm = normalize_effort_choices(vals);
            if !norm.is_empty() {
                return norm;
            }
        }
    }

    if let Some(m) = raw_model {
        let lower = strip_model_effort_and_bracket(m);
        let slug = lower.rsplit('/').next().unwrap_or(&lower);
        let slug = slug.rsplit(':').next().unwrap_or(slug);
        if slug != "default" && slug != "auto" {
            // Known 4-level (low/medium/high/xhigh) flagship reasoning families.
            let supports_xhigh = slug == "opus"
                || slug == "fable"
                || slug.contains("opus")
                || slug.contains("fable")
                || slug.starts_with("claude-sonnet-5")
                || slug.starts_with("gpt-6")
                || slug.starts_with("gpt-5.2")
                || slug.starts_with("gpt-5.3")
                || slug.starts_with("gpt-5.4")
                || slug.starts_with("gpt-5.5")
                || slug.starts_with("gpt-5.6")
                || slug.starts_with("gpt-5.7")
                || slug.starts_with("gpt-5.8")
                || slug.starts_with("gpt-5.9")
                || slug.starts_with("o3")
                || slug.starts_with("o4")
                || slug.starts_with("grok-4.7")
                || slug.starts_with("grok-4.8")
                || slug.starts_with("grok-5")
                || slug.contains("gemini-4-neon");
            if supports_xhigh {
                return four;
            }
            // Known 3-level (low/medium/high) families.
            let three_only = slug == "sonnet"
                || slug == "haiku"
                || slug.contains("haiku")
                || slug.contains("sonnet")
                || slug.contains("flash")
                || slug.contains("lite")
                || slug.contains("mini")
                || slug.contains("nano")
                || slug.contains("turbo")
                || slug.contains("composer")
                || slug.contains("gemini-2.5")
                || slug.contains("gemini-3")
                || slug.contains("gemini-4-carbon")
                || slug.contains("gemini-4-argon")
                || slug.contains("grok-4.6")
                || slug.starts_with("gpt-4")
                || slug.starts_with("gpt-5.0")
                || slug.starts_with("gpt-5.1")
                || slug.contains("glm-")
                || slug.contains("kimi")
                || slug.contains("qwen")
                || slug.contains("deepseek")
                || slug.contains("doubao");
            if three_only {
                return three;
            }
        }
    }

    if let Some(pa) = probed_agent
        && !pa.efforts.is_empty()
    {
        let norm = normalize_effort_choices(&pa.efforts);
        if !norm.is_empty() {
            return norm;
        }
    }

    match agent.trim().to_ascii_lowercase().as_str() {
        "claude" | "codex" | "cursor" | "cursor-agent" | "hermes" | "agy" | "antigravity" => four,
        _ => three,
    }
}

/// Clamps `effort` to the closest valid effort supported by `(agent, model)`.
pub fn clamp_effort_for_model(
    agent: &str,
    model: Option<&str>,
    effort: Effort,
    probed_agent: Option<&ProbedAgent>,
) -> Effort {
    let allowed = supported_efforts_for_model(agent, model, probed_agent);
    if allowed.contains(&effort) {
        return effort;
    }
    if effort == Effort::Xhigh && allowed.contains(&Effort::High) {
        return Effort::High;
    }
    allowed.last().copied().unwrap_or(Effort::High)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedModelsCache {
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub probe: Option<ProbeSnapshot>,
    #[serde(default)]
    pub agent_models: BTreeMap<String, CachedAgentModels>,
    #[serde(default)]
    pub ai_base_url: String,
    #[serde(default)]
    pub ai_models: Vec<String>,
}

pub fn load_models_cache(paths: &DrovePaths) -> PersistedModelsCache {
    match std::fs::read_to_string(&paths.models_cache_file) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => PersistedModelsCache::default(),
    }
}

pub fn save_models_cache(paths: &DrovePaths, cache: &PersistedModelsCache) {
    if let Ok(bytes) = serde_json::to_vec_pretty(cache) {
        let _ = write_atomic(&paths.models_cache_file, &bytes);
    }
}

fn format_target(t: &RoleTarget) -> String {
    match &t.model {
        Some(m) if !m.trim().is_empty() => format!("{}/{m}", t.agent),
        _ => t.agent.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_magpie_disabled_and_five_builtin_roles() {
        let mut cfg = DroveConfig::default();
        assert!(!cfg.magpie.enabled);
        for id in BUILTIN_ROLE_IDS {
            assert!(cfg.roles.contains_key(id), "missing role {id}");
        }
        validate_config(&mut cfg).unwrap();
    }

    #[test]
    fn validates_ai_and_magpie_urls_strictly() {
        let mut cfg = DroveConfig::default();
        cfg.ai.base_url = "https://api.example.com/v1/".into();
        validate_config(&mut cfg).unwrap();
        assert_eq!(cfg.ai.base_url, "https://api.example.com/v1");

        cfg.ai.base_url = "ftp://bad.example.com".into();
        assert!(validate_config(&mut cfg).is_err());

        cfg.ai.base_url = "https://good.example.com".into();
        cfg.magpie.gateway_url = Some("http://127.0.0.1:3425/".into());
        validate_config(&mut cfg).unwrap();
        assert_eq!(cfg.magpie.gateway_url.as_deref(), Some("http://127.0.0.1:3425"));
    }

    #[test]
    fn secret_ai_key_is_written_0600_and_masked() {
        let tmp = crate::test_tmp().join(format!("drove-sec-{}", crate::platform::random_hex()));
        let paths = DrovePaths {
            config_file: tmp.join("drove.json"),
            state_dir: tmp.join("state"),
            secrets_dir: tmp.join("state/secrets"),
            items_dir: tmp.join("state/items"),
            workers_dir: tmp.join("state/workers"),
            artifacts_dir: tmp.join("state/secrets/artifacts"),
            state_file: tmp.join("state/state.json"),
            models_cache_file: tmp.join("state/models-cache.json"),
            ai_key_file: tmp.join("state/secrets/ai.key"),
            coordinator_token_file: tmp.join("state/secrets/coordinator.token"),
            web_info_file: tmp.join("state/web.info.json"),
        };
        paths.ensure_dirs().unwrap();
        write_ai_key(&paths, "sk-test-1234567890abcd").unwrap();
        let (key, src) = read_ai_key(&paths);
        assert_eq!(src, "file");
        assert_eq!(key.as_deref(), Some("sk-test-1234567890abcd"));
        assert_eq!(mask_key("sk-test-1234567890abcd"), "sk-t***abcd");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&paths.ai_key_file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        clear_ai_key(&paths).unwrap();
        assert_eq!(read_ai_key(&paths).0, None);
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn parses_and_validates_ai_model_parameters() {
        assert_eq!(parse_ai_token_size(""), None);
        assert_eq!(parse_ai_token_size("auto"), None);
        assert_eq!(parse_ai_token_size("default"), None);
        assert_eq!(parse_ai_token_size("8k"), Some(8192));
        assert_eq!(parse_ai_token_size("128K"), Some(131_072));
        assert_eq!(parse_ai_token_size("1m"), Some(1_048_576));
        assert_eq!(parse_ai_token_size("32768"), Some(32_768));
        assert_eq!(parse_ai_token_size("512"), None);
        assert_eq!(parse_ai_token_size("8m"), None);
        assert_eq!(parse_ai_token_size("abc"), None);

        let mut cfg = DroveConfig::default();
        cfg.ai.effort = " HIGH ".into();
        cfg.ai.context_window = Some(" 64K ".into());
        cfg.ai.max_tokens = Some(2048);
        validate_config(&mut cfg).unwrap();
        assert_eq!(cfg.ai.effort, "high");
        assert_eq!(cfg.ai.context_window.as_deref(), Some("64k"));
        assert_eq!(cfg.ai.max_tokens, Some(2048));

        cfg.ai.context_window = Some("auto".into());
        cfg.ai.max_tokens = Some(0);
        validate_config(&mut cfg).unwrap();
        assert_eq!(cfg.ai.context_window, None);
        assert_eq!(cfg.ai.max_tokens, None);

        cfg.ai.effort = "ultra".into();
        assert!(validate_config(&mut cfg).is_err());

        cfg.ai.effort = "low".into();
        cfg.ai.context_window = Some("512".into());
        assert!(validate_config(&mut cfg).is_err());

        cfg.ai.context_window = None;
        cfg.ai.max_tokens = Some(16);
        assert!(validate_config(&mut cfg).is_err());
    }
}
