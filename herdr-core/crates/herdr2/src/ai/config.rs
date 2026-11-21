//! Persistent Core AI configuration (`~/.config/herdr2/ai.json`), 0600 secret storage
//! (`<state>/secrets/ai.key`), and seamless backward-compatible fallback to legacy Drove paths.

use crate::paths::{Paths, write_atomic};
use crate::rpc::RpcError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Resolved filesystem layout for the shared Core AI runtime.
#[derive(Clone, Debug)]
pub struct AiPaths {
    pub config_file: PathBuf,
    pub secrets_dir: PathBuf,
    pub ai_key_file: PathBuf,
    pub models_cache_file: PathBuf,
    pub legacy_drove_config: PathBuf,
    pub legacy_drove_key_file: PathBuf,
    pub legacy_drove_models_cache: PathBuf,
}

impl AiPaths {
    pub fn from_paths(paths: &Paths) -> Self {
        let config_dir = paths.config.parent().unwrap_or_else(|| Path::new("."));
        let config_file = std::env::var_os("H2_AI_CONFIG")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| config_dir.join("ai.json"));
        let legacy_drove_config = std::env::var_os("H2_DROVE_CONFIG")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| config_dir.join("drove.json"));
        let secrets_dir = paths.state.join("secrets");
        let drove_state = paths.state.join("drove");
        Self {
            config_file,
            ai_key_file: secrets_dir.join("ai.key"),
            models_cache_file: paths.state.join("ai-models-cache.json"),
            legacy_drove_config,
            legacy_drove_key_file: drove_state.join("secrets").join("ai.key"),
            legacy_drove_models_cache: drove_state.join("models-cache.json"),
            secrets_dir,
        }
    }

    pub fn ensure_dirs(&self) -> io::Result<()> {
        if let Some(parent) = self.config_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::platform::private_dir(&self.secrets_dir)?;
        Ok(())
    }
}

fn default_true() -> bool {
    true
}

fn default_version() -> u32 {
    1
}

fn default_effort() -> String {
    "low".into()
}

/// Core-level AI Assistant configuration persisted in `~/.config/herdr2/ai.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiCoreConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_effort")]
    pub effort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Semantic fallback arbitration for non-OSC-7501 interactive PTY CLI agents.
    #[serde(default = "default_true")]
    pub pty_status_arbitration: bool,
    /// Out-of-lock async error/exit digest for parent-child `<h2>` notes envelopes.
    #[serde(default = "default_true")]
    pub notes_digest: bool,
    /// Shared static system prefix + append-only session reuse for high KV cache hit rate.
    #[serde(default = "default_true")]
    pub prompt_cache_enabled: bool,
    /// Distill omitted middle turns and merge-back discussions during `agent.handoff`.
    #[serde(default = "default_true")]
    pub handoff_distill: bool,
    /// Asynchronously generate semantic milestone labels for newly captured turn checkpoints.
    #[serde(default = "default_true")]
    pub checkpoint_labeling: bool,
    /// Generate semantic titles, hover micro-summaries, and catch-up briefing headlines.
    #[serde(default = "default_true")]
    pub semantic_summary: bool,
}

impl Default for AiCoreConfig {
    fn default() -> Self {
        Self {
            version: 1,
            updated_at: now_iso(),
            enabled: true,
            base_url: String::new(),
            model: String::new(),
            effort: default_effort(),
            context_window: None,
            max_tokens: None,
            pty_status_arbitration: true,
            notes_digest: true,
            prompt_cache_enabled: true,
            handoff_distill: true,
            checkpoint_labeling: true,
            semantic_summary: true,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedAiModelsCache {
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<String>,
}

pub fn now_ms_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub fn now_iso() -> String {
    format_ms_iso(now_ms_u64())
}

pub fn format_ms_iso(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let millis = ms % 1000;
    let days = secs.div_euclid(86_400);
    let sec_of_day = secs.rem_euclid(86_400);
    let hour = sec_of_day / 3600;
    let min = (sec_of_day % 3600) / 60;
    let sec = sec_of_day % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}.{millis:03}Z")
}

pub fn parse_iso_ms(iso: &str) -> Option<u64> {
    let trimmed = iso.trim();
    let (date, time_and_tz) = trimmed.split_once('T').or_else(|| trimmed.split_once('t'))?;
    let (time, tz_offset_secs) =
        if let Some(t) = time_and_tz.strip_suffix('Z').or_else(|| time_and_tz.strip_suffix('z')) {
            (t, 0_i64)
        } else if let Some(pos) = time_and_tz.rfind(['+', '-']) {
            let (t, off_str) = time_and_tz.split_at(pos);
            let sign = if off_str.starts_with('-') { -1_i64 } else { 1_i64 };
            let digits: String = off_str[1..].chars().filter(|c| c.is_ascii_digit()).collect();
            let (hh, mm) = match digits.len() {
                2 => (digits.parse::<i64>().ok()?, 0_i64),
                4 => (digits[..2].parse::<i64>().ok()?, digits[2..4].parse::<i64>().ok()?),
                _ => return None,
            };
            (t, sign * (hh * 3600 + mm * 60))
        } else {
            return None;
        };
    let mut dp = date.split('-');
    let year: i64 = dp.next()?.parse().ok()?;
    let month: u64 = dp.next()?.parse().ok()?;
    let day: u64 = dp.next()?.parse().ok()?;
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut tp = hms.split(':');
    let hour: u64 = tp.next()?.parse().ok()?;
    let min: u64 = tp.next()?.parse().ok()?;
    let sec: u64 = tp.next()?.parse().ok()?;
    let millis_str = format!("{frac:0<3}");
    let millis: u64 = millis_str.get(..3)?.parse().ok()?;

    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m_adj + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + (doe as i64) - 719_468;
    let total_secs = days * 86_400 + (hour * 3600 + min * 60 + sec) as i64 - tz_offset_secs;
    (total_secs >= 0).then_some(total_secs as u64 * 1000 + millis)
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

pub fn validate_config(cfg: &mut AiCoreConfig) -> Result<(), RpcError> {
    if !cfg.base_url.trim().is_empty() {
        cfg.base_url = validate_http_url(&cfg.base_url, "ai.baseUrl")?;
    } else {
        cfg.base_url = String::new();
    }
    cfg.model = cfg.model.trim().to_string();
    cfg.effort = cfg.effort.trim().to_ascii_lowercase();
    if cfg.effort.is_empty() {
        cfg.effort = "low".into();
    }
    if !matches!(cfg.effort.as_str(), "auto" | "none" | "low" | "medium" | "high") {
        return Err(RpcError::invalid_params("ai.effort must be auto, none, low, medium, or high"));
    }
    if let Some(ctx) = cfg.context_window.take() {
        let clean = ctx.trim().to_ascii_lowercase();
        if !clean.is_empty() && clean != "auto" && clean != "default" {
            if parse_ai_token_size(&clean).is_none() {
                return Err(RpcError::invalid_params(
                    "ai.contextWindow must be auto or a token size between 1k and 4m (e.g. 8k, 32k, 128k, 1m)",
                ));
            }
            cfg.context_window = Some(clean);
        }
    }
    if let Some(mt) = cfg.max_tokens {
        if mt == 0 {
            cfg.max_tokens = None;
        } else if !(64..=131_072).contains(&mt) {
            return Err(RpcError::invalid_params("ai.maxTokens must be between 64 and 131072"));
        }
    }
    cfg.version = 1;
    cfg.updated_at = now_iso();
    Ok(())
}

/// Loads `ai.json`, transparently falling back to legacy `drove.json`'s `ai` block on first upgrade.
pub fn load_config(paths: &AiPaths) -> Result<AiCoreConfig, RpcError> {
    match std::fs::read_to_string(&paths.config_file) {
        Ok(text) => {
            let mut cfg: AiCoreConfig = serde_json::from_str(&text)
                .map_err(|e| RpcError::new("config", format!("{}: {e}", paths.config_file.display()), "fix ai.json"))?;
            let _ = validate_config(&mut cfg);
            Ok(cfg)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Ok(legacy_text) = std::fs::read_to_string(&paths.legacy_drove_config)
                && let Ok(val) = serde_json::from_str::<Value>(&legacy_text)
                && let Some(ai_obj) = val.get("ai")
                && ai_obj.is_object()
            {
                let mut cfg = AiCoreConfig::default();
                if let Some(b) = ai_obj.get("enabled").and_then(Value::as_bool) {
                    cfg.enabled = b;
                }
                if let Some(s) = ai_obj.get("baseUrl").and_then(Value::as_str) {
                    cfg.base_url = s.trim().to_string();
                }
                if let Some(s) = ai_obj.get("model").and_then(Value::as_str) {
                    cfg.model = s.trim().to_string();
                }
                if let Some(s) = ai_obj.get("effort").and_then(Value::as_str) {
                    cfg.effort = s.trim().to_ascii_lowercase();
                }
                if let Some(s) = ai_obj.get("contextWindow").and_then(Value::as_str) {
                    cfg.context_window = Some(s.trim().to_ascii_lowercase());
                }
                if let Some(n) = ai_obj.get("maxTokens").and_then(Value::as_u64) {
                    cfg.max_tokens = u32::try_from(n).ok();
                }
                let _ = validate_config(&mut cfg);
                return Ok(cfg);
            }
            Ok(AiCoreConfig::default())
        }
        Err(e) => Err(RpcError::new(
            "config",
            format!("{}: {e}", paths.config_file.display()),
            "check file permissions",
        )),
    }
}

pub fn save_config(paths: &AiPaths, cfg: &mut AiCoreConfig) -> Result<(), RpcError> {
    validate_config(cfg)?;
    if let Some(parent) = paths.config_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| RpcError::internal(format!("mkdir config dir: {e}")))?;
    }
    let bytes = serde_json::to_vec_pretty(cfg).map_err(|e| RpcError::internal(e.to_string()))?;
    write_atomic(&paths.config_file, &bytes)
        .map_err(|e| RpcError::internal(format!("write {}: {e}", paths.config_file.display())))
}

/// Writes a private 0600 secret file atomically inside a private directory.
pub fn write_secret_file(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        crate::platform::private_dir(parent)?;
    }
    let suffix: String = crate::platform::random_hex().chars().take(12).collect();
    let tmp = path.with_extension(format!("tmp.{suffix}"));
    {
        use std::io::Write;
        let mut f = crate::platform::private_file(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(tmp, path)
}

/// Resolves the AI API key with precedence:
/// `H2_AI_API_KEY` -> `H2_DROVE_AI_API_KEY` -> `<state>/secrets/ai.key` -> `<state>/drove/secrets/ai.key`.
pub fn read_ai_key(paths: &AiPaths) -> (Option<String>, &'static str) {
    if let Ok(v) = std::env::var("H2_AI_API_KEY")
        && !v.trim().is_empty()
    {
        return (Some(v.trim().to_string()), "env:H2_AI_API_KEY");
    }
    if let Ok(v) = std::env::var("H2_DROVE_AI_API_KEY")
        && !v.trim().is_empty()
    {
        return (Some(v.trim().to_string()), "env:H2_DROVE_AI_API_KEY");
    }
    if let Ok(s) = std::fs::read_to_string(&paths.ai_key_file)
        && !s.trim().is_empty()
    {
        return (Some(s.trim().to_string()), "file");
    }
    if let Ok(s) = std::fs::read_to_string(&paths.legacy_drove_key_file)
        && !s.trim().is_empty()
    {
        return (Some(s.trim().to_string()), "file:legacy");
    }
    (None, "none")
}

pub fn write_ai_key(paths: &AiPaths, key: &str) -> Result<(), RpcError> {
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

pub fn clear_ai_key(paths: &AiPaths) -> io::Result<()> {
    for p in [&paths.ai_key_file, &paths.legacy_drove_key_file] {
        match std::fs::remove_file(p) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
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

pub fn load_models_cache(paths: &AiPaths) -> PersistedAiModelsCache {
    if let Ok(text) = std::fs::read_to_string(&paths.models_cache_file)
        && let Ok(parsed) = serde_json::from_str::<PersistedAiModelsCache>(&text)
    {
        return parsed;
    }
    if let Ok(text) = std::fs::read_to_string(&paths.legacy_drove_models_cache)
        && let Ok(val) = serde_json::from_str::<Value>(&text)
    {
        let base_url = val["aiBaseUrl"].as_str().unwrap_or("").to_string();
        let models = val["aiModels"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        return PersistedAiModelsCache { updated_at: now_iso(), base_url, models };
    }
    PersistedAiModelsCache::default()
}

pub fn save_models_cache(paths: &AiPaths, cache: &PersistedAiModelsCache) {
    if let Some(parent) = paths.models_cache_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(cache) {
        let _ = write_atomic(&paths.models_cache_file, &bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_legacy_drove_config_and_key_seamlessly() {
        let tmp = crate::test_tmp().join(format!("ai-cfg-legacy-{}", crate::platform::random_hex()));
        let paths = Paths {
            socket: tmp.join("core.sock"),
            holder_socket: tmp.join("holder.sock"),
            state: tmp.join("state"),
            config: tmp.join("config/config.toml"),
        };
        std::fs::create_dir_all(tmp.join("config")).unwrap();
        std::fs::create_dir_all(tmp.join("state/drove/secrets")).unwrap();
        std::fs::write(
            tmp.join("config/drove.json"),
            r#"{"ai":{"enabled":true,"baseUrl":"https://api.example.com/v1","model":"deepseek-chat","effort":"low","contextWindow":"64k","maxTokens":1024}}"#,
        )
        .unwrap();
        std::fs::write(tmp.join("state/drove/secrets/ai.key"), "sk-legacy-1234567890\n").unwrap();

        let ai_paths = AiPaths::from_paths(&paths);
        let cfg = load_config(&ai_paths).unwrap();
        assert_eq!(cfg.base_url, "https://api.example.com/v1");
        assert_eq!(cfg.model, "deepseek-chat");
        assert_eq!(cfg.context_window.as_deref(), Some("64k"));
        assert_eq!(cfg.max_tokens, Some(1024));
        assert!(cfg.pty_status_arbitration);
        assert!(cfg.notes_digest);
        assert!(cfg.prompt_cache_enabled);

        if std::env::var_os("H2_AI_API_KEY").is_none() && std::env::var_os("H2_DROVE_AI_API_KEY").is_none() {
            let (k, src) = read_ai_key(&ai_paths);
            assert_eq!(k.as_deref(), Some("sk-legacy-1234567890"));
            assert_eq!(src, "file:legacy");

            write_ai_key(&ai_paths, "sk-new-9876543210").unwrap();
            let (k2, src2) = read_ai_key(&ai_paths);
            assert_eq!(k2.as_deref(), Some("sk-new-9876543210"));
            assert_eq!(src2, "file");

            clear_ai_key(&ai_paths).unwrap();
            let (k3, _) = read_ai_key(&ai_paths);
            assert!(k3.is_none());
        }

        let _ = std::fs::remove_dir_all(tmp);
    }
}
