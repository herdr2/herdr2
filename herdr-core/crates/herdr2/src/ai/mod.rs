//! Shared Core AI runtime (`Core.ai`):
//! - Unified configuration (`~/.config/herdr2/ai.json`) & 0600 secret (`<state>/secrets/ai.key`)
//!   with seamless fallback to legacy Drove config/secrets.
//! - High KV-cache hit rate via byte-immutable `HERDR2_CONTROL_PLANE_SYSTEM` prefix (>1024 tokens)
//!   and append-only multi-turn `AiSessionPool`.
//! - Local zero-token LRU deduplication, endpoint capability memory, reasoning-token tiering,
//!   and full cache-hit / token-saving observability.

pub mod bisect;
pub mod briefing;
pub mod cache;
pub mod client;
pub mod compact;
pub mod config;
pub mod digest;
pub mod handoff_ai;
pub mod prompts;
pub mod session;
pub mod terminal_state;

pub use bisect::{BisectAlternative, CheckpointBisectParams, CheckpointBisectResult, generate_checkpoint_label};
pub use briefing::{
    BatchPermissionGroup, BriefingAgentInput, BriefingCounts, BriefingDroveInput, BriefingItem, BriefingParams,
    BriefingSnapshot, BriefingTerminalInput, build_deterministic_briefing, enhance_briefing_headline,
};
pub use client::{AiModelsResult, AiProtocol, AiTestResult, allows_keyless_url};
pub use config::{
    AiCoreConfig, AiPaths, format_ms_iso, is_loopback_http_url, mask_key, normalize_ai_language,
    normalize_prompt_mode, now_iso, now_ms_u64, parse_ai_token_size, parse_iso_ms, prompt_lang_code,
    read_app_settings_language, validate_http_url, write_secret_file,
};
pub use digest::{
    AgentDigestResult, CommitMessageParams, CommitMessageResult, DeterministicDigest, PolishContextInput,
    PolishPromptParams, PolishPromptResult, PolishSessionContext, SummarizeAgentParams,
    build_deterministic_acp_digest, build_polish_session_context, generate_commit_message, polish_prompt,
    refine_agent_digest,
};
pub use handoff_ai::{
    HandoffRecommendParams, HandoffRecommendResult, distill_merge_back, distill_omitted_turns, recommend_handoff,
};
pub use terminal_state::{
    ClassifyTerminalStateParams, ClassifyTerminalStateResult, classify_acp_turn_state, classify_terminal_state,
};

use crate::paths::Paths;
use crate::rpc::RpcError;
use cache::AiResultCache;
use client::{
    EndpointCapabilities, HttpAuthStyle, KV_CACHE_DEGRADE_THRESHOLD, anthropic_messages_endpoints,
    build_anthropic_payload_candidates, build_chat_payload_candidates, chat_endpoints, effective_max_tokens,
    extract_json_object, extract_response_content, fetch_ai_models, fetch_ai_models_detailed,
    http_json_request_with_auth, is_valid_protocol_response, parse_usage_tokens, protocol_probe_order,
    record_winning_variant_for_protocol, refine_capabilities_from_error_for_protocol,
    should_retry_without_optional_fields, test_ai_connection_detailed, truncate_for_context,
};
use compact::{fingerprint_hex, fnv1a_64, redact_secrets, strip_and_dedup_lines};
use config::{
    PersistedAiModelsCache, clear_ai_key, load_config, load_endpoint_caps, load_models_cache, read_ai_key, save_config,
    save_endpoint_caps, save_models_cache, write_ai_key,
};
use prompts::{HERDR2_CONTROL_PLANE_SYSTEM, compact_system_for_op, format_op_user_message, op_expects_json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use session::{AiSessionPool, ChatMessage, MAX_SESSION_TOKENS, build_seeded_stateless_messages};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const COOLDOWN_SECS: u64 = 300;
const CIRCUIT_THRESHOLD: u32 = 3;
const FG_CONCURRENCY_LIMIT: usize = 4;
const BG_CONCURRENCY_LIMIT: usize = 3;

#[derive(Default, Clone, Debug)]
pub struct AiCircuitState {
    pub consecutive_failures: u32,
    pub rate_limit_streak: u32,
    pub circuit_open: bool,
    pub half_open_probe_in_flight: bool,
    pub cooldown_until: Option<String>,
    pub last_error: Option<String>,
}

struct CircuitProbeGuard<'a> {
    rt: &'a AiRuntime,
    is_probe: bool,
    settled: bool,
}

impl<'a> CircuitProbeGuard<'a> {
    fn settle(&mut self) {
        self.settled = true;
    }
}

impl Drop for CircuitProbeGuard<'_> {
    fn drop(&mut self) {
        if self.is_probe && !self.settled {
            let mut cir = self.rt.circuit.lock().unwrap_or_else(|p| p.into_inner());
            cir.half_open_probe_in_flight = false;
        }
    }
}

struct InflightGuard<'a> {
    rt: &'a AiRuntime,
    key: Option<String>,
    notify: Option<Arc<tokio::sync::Notify>>,
}

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.rt.inflight.lock().unwrap_or_else(|p| p.into_inner()).remove(&key);
        }
        if let Some(notify) = self.notify.take() {
            notify.notify_waiters();
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AiTaskTier {
    /// Trivial classification/routing: suppresses thinking (`none` -> `low`), temperature 0, strict max_tokens.
    Nano,
    /// Concise extraction/summarization: `low` effort, bounded output tokens.
    #[default]
    Fast,
    /// Deeper analysis (loop audit, diagnosis, role architect): honors user's configured effort/max_tokens.
    Standard,
}

#[derive(Clone, Debug, Default)]
pub enum AiConversationMode {
    /// Reuses canonical static system prompt with no accumulated prior history (best for independent snapshots).
    #[default]
    StatelessSharedPrefix,
    /// Shares static system prompt + read-only pinned seed context with zero lock contention across concurrent calls.
    SeededStateless { seed_context: String },
    /// Appends onto an existing per-scope KV-cached conversation (`drove:loop:<attempt_id>`).
    Session { key: String, seed_context: Option<String> },
    /// Custom standalone system prompt (e.g. role architect or generic `ai.complete` RPC caller).
    CustomSystem(String),
}

#[derive(Clone, Debug)]
pub struct AiCompleteRequest {
    pub op: String,
    pub tier: AiTaskTier,
    pub mode: AiConversationMode,
    pub user_prompt: String,
    pub max_tokens: u32,
    pub timeout: Duration,
    pub cache_key: Option<String>,
    pub cache_ttl: Option<Duration>,
    pub prompt_cache_bucket: Option<String>,
    pub url_override: Option<String>,
    pub key_override: Option<String>,
    pub model_override: Option<String>,
}

impl Default for AiCompleteRequest {
    fn default() -> Self {
        Self {
            op: "custom".into(),
            tier: AiTaskTier::Fast,
            mode: AiConversationMode::StatelessSharedPrefix,
            user_prompt: String::new(),
            max_tokens: 256,
            timeout: client::AI_CHAT_TIMEOUT,
            cache_key: None,
            cache_ttl: None,
            prompt_cache_bucket: None,
            url_override: None,
            key_override: None,
            model_override: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
}

#[derive(Clone, Debug)]
pub struct AiCompleteResponse {
    pub content: String,
    pub model: String,
    pub latency_ms: u64,
    pub usage: AiUsage,
    pub from_local_cache: bool,
    pub session_reused: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiOpMetrics {
    pub calls: u64,
    pub local_cache_hits: u64,
    pub failures: u64,
    pub parse_errors: u64,
    pub low_confidence_rejects: u64,
    pub total_latency_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiUsageStats {
    pub total_calls: u64,
    pub local_cache_hits: u64,
    pub sessions_reused: u64,
    pub active_sessions: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    /// `cached_input_tokens / input_tokens` in `0.0..=1.0`.
    pub cache_hit_ratio: f64,
    /// True when the active endpoint repeatedly reported 0 cached input tokens and automatically fell back to compact prompts.
    #[serde(default)]
    pub prompt_cache_auto_degraded: bool,
    /// Remembered auto-detected prompt mode for the active `(base_url, model)`: `"shared_prefix"`, `"compact"`, or `None` (pending/reset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_detected_mode: Option<String>,
    /// Effective prompt mode currently applied for stateless control-plane ops (`"shared_prefix"` or `"compact"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_prompt_mode: Option<String>,
    /// Number of consecutive >=1024-token shared-prefix probes without KV-cache hits so far.
    #[serde(default)]
    pub prompt_cache_probe_misses: u8,
    /// Probe threshold before auto mode degrades to compact prompts.
    #[serde(default)]
    pub prompt_cache_probe_threshold: u8,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_op: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub op_metrics: BTreeMap<String, AiOpMetrics>,
}

fn default_true() -> bool {
    true
}

fn default_prompt_mode() -> String {
    "auto".into()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiCoreSwitches {
    pub pty_status_arbitration: bool,
    pub notes_digest: bool,
    pub prompt_cache_enabled: bool,
    #[serde(default = "default_prompt_mode")]
    pub prompt_mode: String,
    pub handoff_distill: bool,
    pub checkpoint_labeling: bool,
    pub semantic_summary: bool,
    #[serde(default = "default_true")]
    pub redact_secrets: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiStatus {
    pub enabled: bool,
    pub configured: bool,
    pub key_source: String,
    pub key_file_path: String,
    pub config_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_masked: Option<String>,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<AiProtocol>,
    pub model: String,
    pub effort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default)]
    pub available_models: Vec<String>,
    pub circuit_open: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until: Option<String>,
    pub consecutive_failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub switches: AiCoreSwitches,
    pub usage: AiUsageStats,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiOnParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiSettingsSetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear_key: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pty_status_arbitration: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_digest: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_prompt_cache_detection: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_distill: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_labeling: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_summary: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redact_secrets: Option<bool>,
}

impl AiSettingsSetParams {
    /// True when this patch only updates `language` and touches no endpoint, key, model, or switch.
    pub fn is_language_only(&self) -> bool {
        self.language.is_some()
            && self.enabled.is_none()
            && self.base_url.is_none()
            && self.model.is_none()
            && self.effort.is_none()
            && self.context_window.is_none()
            && self.max_tokens.is_none()
            && self.api_key.is_none()
            && self.clear_key.is_none()
            && self.pty_status_arbitration.is_none()
            && self.notes_digest.is_none()
            && self.prompt_cache_enabled.is_none()
            && self.prompt_mode.is_none()
            && self.reset_prompt_cache_detection.is_none()
            && self.handoff_distill.is_none()
            && self.checkpoint_labeling.is_none()
            && self.semantic_summary.is_none()
            && self.redact_secrets.is_none()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiProbePromptModeResult {
    pub ok: bool,
    /// Detected prompt mode: `"shared_prefix"`, `"compact"`, or `"pending"`.
    pub detected_mode: String,
    pub probes_run: u8,
    pub cached_input_tokens: u64,
    pub input_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub status: AiStatus,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiModelsParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiTestParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiCompleteParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub user_prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<AiTaskTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiCompleteResult {
    pub content: String,
    pub model: String,
    pub latency_ms: u64,
    pub usage: AiUsage,
    pub from_local_cache: bool,
    pub session_reused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildNoteDigest {
    pub root_cause: String,
    pub files: Vec<String>,
    pub next_step: String,
}

/// Core-wide shared AI runtime mounted at `Core.ai`.
pub struct AiRuntime {
    pub paths: AiPaths,
    config: Mutex<AiCoreConfig>,
    models_cache: Mutex<Vec<String>>,
    models_url: Mutex<String>,
    circuit: Mutex<AiCircuitState>,
    endpoint_caps: Mutex<BTreeMap<String, EndpointCapabilities>>,
    sessions: Arc<AiSessionPool>,
    result_cache: Arc<AiResultCache>,
    inflight: Mutex<HashMap<String, Arc<tokio::sync::Notify>>>,
    usage: Mutex<AiUsageStats>,
    fg_sem: Arc<tokio::sync::Semaphore>,
    bg_sem: Arc<tokio::sync::Semaphore>,
}

impl AiRuntime {
    pub fn open(paths: &Paths) -> io::Result<Arc<Self>> {
        let ai_paths = AiPaths::from_paths(paths);
        ai_paths.ensure_dirs()?;
        let cfg = match load_config(&ai_paths) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("ai: using defaults because {}: {}", ai_paths.config_file.display(), e.message);
                AiCoreConfig::default()
            }
        };
        let disk_cache = load_models_cache(&ai_paths);
        let cfg_url = cfg.base_url.trim().to_string();
        let use_saved = !disk_cache.models.is_empty()
            && (cfg_url.is_empty() || disk_cache.base_url.is_empty() || disk_cache.base_url == cfg_url);
        let models_cache = if use_saved { disk_cache.models } else { Vec::new() };
        let models_url = if use_saved {
            if disk_cache.base_url.is_empty() { cfg_url } else { disk_cache.base_url }
        } else {
            String::new()
        };
        let saved_caps: BTreeMap<String, EndpointCapabilities> = load_endpoint_caps(&ai_paths);

        Ok(Arc::new(Self {
            paths: ai_paths,
            config: Mutex::new(cfg),
            models_cache: Mutex::new(models_cache),
            models_url: Mutex::new(models_url),
            circuit: Mutex::new(AiCircuitState::default()),
            endpoint_caps: Mutex::new(saved_caps),
            sessions: Arc::new(AiSessionPool::default()),
            result_cache: Arc::new(AiResultCache::default()),
            inflight: Mutex::new(HashMap::new()),
            usage: Mutex::new(AiUsageStats::default()),
            fg_sem: Arc::new(tokio::sync::Semaphore::new(FG_CONCURRENCY_LIMIT)),
            bg_sem: Arc::new(tokio::sync::Semaphore::new(BG_CONCURRENCY_LIMIT)),
        }))
    }

    pub fn config_snapshot(&self) -> AiCoreConfig {
        self.config.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn read_key(&self) -> (Option<String>, &'static str) {
        read_ai_key(&self.paths)
    }

    pub fn is_configured(&self) -> bool {
        let cfg = self.config_snapshot();
        if !cfg.enabled || cfg.base_url.trim().is_empty() {
            return false;
        }
        let (key, _) = read_ai_key(&self.paths);
        key.is_some() || allows_keyless_url(&cfg.base_url)
    }

    pub fn status_arbitration_enabled(&self) -> bool {
        let cfg = self.config_snapshot();
        cfg.enabled && cfg.pty_status_arbitration && self.is_configured() && !self.is_circuit_open()
    }

    pub fn notes_digest_enabled(&self) -> bool {
        let cfg = self.config_snapshot();
        cfg.enabled && cfg.notes_digest && self.is_configured() && !self.is_circuit_open()
    }

    pub fn handoff_distill_enabled(&self) -> bool {
        let cfg = self.config_snapshot();
        cfg.enabled && cfg.handoff_distill && self.is_configured() && !self.is_circuit_open()
    }

    pub fn checkpoint_labeling_enabled(&self) -> bool {
        let cfg = self.config_snapshot();
        cfg.enabled && cfg.checkpoint_labeling && self.is_configured() && !self.is_circuit_open()
    }

    pub fn is_circuit_open(&self) -> bool {
        let cir = self.circuit.lock().unwrap_or_else(|p| p.into_inner());
        if !cir.circuit_open {
            return false;
        }
        if let Some(until) = &cir.cooldown_until
            && let Some(until_ms) = parse_iso_ms(until)
            && now_ms_u64() >= until_ms
        {
            // Cooldown elapsed: allow a single half-open probe unless one is already running.
            if cir.half_open_probe_in_flight {
                return true;
            }
            return false;
        }
        true
    }

    fn try_enter_circuit(&self) -> Result<CircuitProbeGuard<'_>, String> {
        let mut cir = self.circuit.lock().unwrap_or_else(|p| p.into_inner());
        if !cir.circuit_open {
            return Ok(CircuitProbeGuard { rt: self, is_probe: false, settled: false });
        }
        if let Some(until) = &cir.cooldown_until
            && let Some(until_ms) = parse_iso_ms(until)
            && now_ms_u64() >= until_ms
        {
            if cir.half_open_probe_in_flight {
                return Err("AI circuit breaker half-open probe in flight".into());
            }
            cir.half_open_probe_in_flight = true;
            return Ok(CircuitProbeGuard { rt: self, is_probe: true, settled: false });
        }
        if cir.rate_limit_streak > 0 {
            Err("AI rate limit backoff active".into())
        } else {
            Err("AI circuit breaker is open".into())
        }
    }

    pub fn record_success(&self) {
        let mut cir = self.circuit.lock().unwrap_or_else(|p| p.into_inner());
        cir.consecutive_failures = 0;
        cir.rate_limit_streak = 0;
        cir.circuit_open = false;
        cir.half_open_probe_in_flight = false;
        cir.cooldown_until = None;
        cir.last_error = None;
    }

    pub fn record_failure(&self, err: &str) {
        let mut cir = self.circuit.lock().unwrap_or_else(|p| p.into_inner());
        let was_probe = cir.half_open_probe_in_flight;
        cir.half_open_probe_in_flight = false;
        cir.consecutive_failures += 1;
        cir.last_error = Some(err.to_string());
        if was_probe || cir.consecutive_failures >= CIRCUIT_THRESHOLD {
            cir.circuit_open = true;
            cir.cooldown_until = Some(format_ms_iso(now_ms_u64() + COOLDOWN_SECS * 1000));
        }
    }

    pub fn record_rate_limit(&self, retry_after_secs: Option<u64>, err: &str) {
        let mut cir = self.circuit.lock().unwrap_or_else(|p| p.into_inner());
        cir.half_open_probe_in_flight = false;
        cir.rate_limit_streak = cir.rate_limit_streak.saturating_add(1);
        cir.last_error = Some(err.to_string());
        let backoff_secs = retry_after_secs.unwrap_or_else(|| match cir.rate_limit_streak {
            1 => 10,
            2 => 30,
            _ => 60,
        });
        cir.circuit_open = true;
        cir.cooldown_until = Some(format_ms_iso(now_ms_u64() + backoff_secs * 1000));
    }

    pub fn record_auth_failure(&self, err: &str) {
        let mut cir = self.circuit.lock().unwrap_or_else(|p| p.into_inner());
        cir.half_open_probe_in_flight = false;
        cir.consecutive_failures = CIRCUIT_THRESHOLD;
        cir.last_error = Some(err.to_string());
        cir.circuit_open = true;
        cir.cooldown_until = Some(format_ms_iso(now_ms_u64() + COOLDOWN_SECS * 1000));
    }

    pub fn record_op_failure(&self, op: &str) {
        let mut u = self.usage.lock().unwrap_or_else(|p| p.into_inner());
        u.op_metrics.entry(op.to_string()).or_default().failures += 1;
    }

    pub fn record_parse_error(&self, op: &str) {
        let mut u = self.usage.lock().unwrap_or_else(|p| p.into_inner());
        u.op_metrics.entry(op.to_string()).or_default().parse_errors += 1;
    }

    pub fn record_low_conf_reject(&self, op: &str) {
        let mut u = self.usage.lock().unwrap_or_else(|p| p.into_inner());
        u.op_metrics.entry(op.to_string()).or_default().low_confidence_rejects += 1;
    }

    pub fn update_models(&self, base_url: &str, models: Vec<String>) {
        let mut deduped = Vec::new();
        for raw in models {
            let t = raw.trim();
            if !t.is_empty() && !deduped.iter().any(|x| x == t) {
                deduped.push(t.to_string());
            }
        }
        if deduped.is_empty() {
            return;
        }
        *self.models_cache.lock().unwrap_or_else(|p| p.into_inner()) = deduped.clone();
        *self.models_url.lock().unwrap_or_else(|p| p.into_inner()) = base_url.trim().to_string();
        save_models_cache(
            &self.paths,
            &PersistedAiModelsCache { updated_at: now_iso(), base_url: base_url.trim().to_string(), models: deduped },
        );
    }

    pub fn cached_models(&self) -> Vec<String> {
        self.models_cache.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn evict_session(&self, scope_key: &str) {
        self.sessions.evict(scope_key);
    }

    pub fn evict_session_prefix(&self, prefix: &str) {
        self.sessions.evict_prefix(prefix);
    }

    /// Resolves the preferred human-facing language tag (`"zh-CN"` or `"en"`):
    /// explicit `ai.json` `language` > local `herdr2-app/settings.json` `language` > `"en"`.
    pub fn preferred_language_tag(&self) -> &'static str {
        if let Some(lang) = self.config_snapshot().language.as_deref() {
            return if prompt_lang_code(lang) == "zh" { "zh-CN" } else { "en" };
        }
        if let Some(lang) = read_app_settings_language(&self.paths.app_settings_file) {
            return if prompt_lang_code(&lang) == "zh" { "zh-CN" } else { "en" };
        }
        "en"
    }

    /// Resolves the canonical prompt `lang` code (`"zh"` or `"en"`).
    pub fn preferred_prompt_lang(&self) -> &'static str {
        prompt_lang_code(self.preferred_language_tag())
    }

    /// Resolves prompt `lang` code with an optional per-request explicit override.
    pub fn resolve_prompt_lang(&self, explicit: Option<&str>) -> &'static str {
        if let Some(raw) = explicit
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto") && !s.eq_ignore_ascii_case("default"))
        {
            return prompt_lang_code(raw);
        }
        self.preferred_prompt_lang()
    }

    fn resolve_remembered_protocol(&self, base: &str, model: &str) -> Option<(AiProtocol, bool)> {
        let norm_base = base.trim().trim_end_matches('/');
        if norm_base.is_empty() {
            return None;
        }
        let map = self.endpoint_caps.lock().unwrap_or_else(|p| p.into_inner());
        let clean_model = model.trim();
        if !clean_model.is_empty()
            && let Some(caps) = map.get(&format!("{norm_base}|{clean_model}"))
            && let Some(proto) = caps.protocol
        {
            return Some((proto, caps.anthropic_bearer_auth));
        }
        if let Some(caps) = map.get(norm_base)
            && let Some(proto) = caps.protocol
        {
            return Some((proto, caps.anthropic_bearer_auth));
        }
        let prefix = format!("{norm_base}|");
        map.iter()
            .find(|(k, v)| k.starts_with(&prefix) && v.protocol.is_some())
            .and_then(|(_, v)| v.protocol.map(|p| (p, v.anthropic_bearer_auth)))
    }

    fn remember_endpoint_protocol(
        &self,
        base: &str,
        model: Option<&str>,
        protocol: AiProtocol,
        anthropic_bearer_auth: bool,
    ) {
        let norm_base = base.trim().trim_end_matches('/');
        if norm_base.is_empty() {
            return;
        }
        let mut map = self.endpoint_caps.lock().unwrap_or_else(|p| p.into_inner());
        let base_entry =
            map.entry(norm_base.to_string()).or_insert_with(|| EndpointCapabilities::default_for_url(norm_base));
        base_entry.protocol = Some(protocol);
        base_entry.anthropic_bearer_auth = anthropic_bearer_auth;
        if let Some(m) = model.map(str::trim).filter(|s| !s.is_empty()) {
            let entry = map
                .entry(format!("{norm_base}|{m}"))
                .or_insert_with(|| EndpointCapabilities::default_for_url(norm_base));
            entry.protocol = Some(protocol);
            entry.anthropic_bearer_auth = anthropic_bearer_auth;
        }
        save_endpoint_caps(&self.paths, &map);
    }

    pub fn status(&self) -> AiStatus {
        let cfg = self.config_snapshot();
        let (key, src) = read_ai_key(&self.paths);
        let cir = self.circuit.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let has_key = key.is_some();
        let is_local = is_loopback_http_url(&cfg.base_url);
        let configured = cfg.enabled && !cfg.base_url.trim().is_empty() && (has_key || allows_keyless_url(&cfg.base_url));
        let key_source = if has_key {
            src.to_string()
        } else if is_local && !cfg.base_url.trim().is_empty() {
            "local".into()
        } else {
            src.to_string()
        };
        let mut usage = self.usage.lock().unwrap_or_else(|p| p.into_inner()).clone();
        usage.active_sessions = self.sessions.active_count() as u64;
        usage.cache_hit_ratio = if usage.input_tokens > 0 {
            (usage.cached_input_tokens as f64 / usage.input_tokens as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let active_base = cfg.base_url.trim().trim_end_matches('/');
        let active_model = if !cfg.model.trim().is_empty() {
            cfg.model.trim().to_string()
        } else {
            self.cached_models().into_iter().next().unwrap_or_default()
        };
        let resolved_prompt_mode = cfg.resolved_prompt_mode();
        let active_caps = if !active_base.is_empty() && !active_model.is_empty() {
            let cap_key = format!("{active_base}|{active_model}");
            self.endpoint_caps
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&cap_key)
                .copied()
        } else {
            None
        };
        let protocol = active_caps
            .and_then(|c| c.protocol)
            .or_else(|| self.resolve_remembered_protocol(active_base, &active_model).map(|(p, _)| p));
        let small_ctx = cfg.context_window.as_deref().and_then(parse_ai_token_size).is_some_and(|c| c <= 4096);
        let caps_degraded = active_caps.is_some_and(|c| !c.prompt_cache_effective);
        usage.prompt_cache_auto_degraded = resolved_prompt_mode == "auto" && caps_degraded;
        usage.prompt_cache_detected_mode = active_caps.and_then(|c| c.detected_prompt_mode()).map(str::to_owned);
        usage.prompt_cache_probe_misses = active_caps.map_or(0, |c| c.prefix_probes_without_cache);
        usage.prompt_cache_probe_threshold = KV_CACHE_DEGRADE_THRESHOLD;
        usage.effective_prompt_mode = Some(
            match resolved_prompt_mode {
                "shared_prefix" if !small_ctx => "shared_prefix",
                "compact" => "compact",
                _ if !small_ctx && !caps_degraded => "shared_prefix",
                _ => "compact",
            }
            .into(),
        );
        AiStatus {
            enabled: cfg.enabled,
            configured,
            key_source,
            key_file_path: self.paths.ai_key_file.display().to_string(),
            config_path: self.paths.config_file.display().to_string(),
            key_masked: key.as_deref().map(mask_key),
            base_url: cfg.base_url,
            protocol,
            model: cfg.model,
            effort: cfg.effort,
            context_window: cfg.context_window,
            max_tokens: cfg.max_tokens,
            language: cfg.language,
            available_models: self.cached_models(),
            circuit_open: cir.circuit_open,
            cooldown_until: cir.cooldown_until,
            consecutive_failures: cir.consecutive_failures,
            last_error: cir.last_error,
            switches: AiCoreSwitches {
                pty_status_arbitration: cfg.pty_status_arbitration,
                notes_digest: cfg.notes_digest,
                prompt_cache_enabled: resolved_prompt_mode != "compact",
                prompt_mode: resolved_prompt_mode.into(),
                handoff_distill: cfg.handoff_distill,
                checkpoint_labeling: cfg.checkpoint_labeling,
                semantic_summary: cfg.semantic_summary,
                redact_secrets: cfg.redact_secrets,
            },
            usage,
        }
    }

    /// Fetches `/models` lazily if configured URL changed or model list is empty, then returns `AiStatus`.
    pub async fn status_refresh_models(&self) -> AiStatus {
        let cfg = self.config_snapshot();
        let cfg_url = cfg.base_url.trim();
        if !cfg_url.is_empty() {
            let need_fetch = {
                let cached = self.models_cache.lock().unwrap_or_else(|p| p.into_inner());
                let cached_url = self.models_url.lock().unwrap_or_else(|p| p.into_inner());
                cached.is_empty() || *cached_url != cfg_url
            };
            if need_fetch {
                let (key, _) = read_ai_key(&self.paths);
                let res = fetch_ai_models_detailed(cfg_url, key.as_deref()).await;
                if res.ok && !res.models.is_empty() {
                    self.update_models(&res.base_url, res.models);
                    if res.protocol == Some(AiProtocol::Anthropic)
                        && self.resolve_remembered_protocol(&res.base_url, &cfg.model).is_none()
                    {
                        self.remember_endpoint_protocol(&res.base_url, None, AiProtocol::Anthropic, false);
                    }
                }
            }
        }
        self.status()
    }

    pub async fn update_settings(&self, params: AiSettingsSetParams) -> Result<AiStatus, RpcError> {
        let prev = self.config_snapshot();
        let mut next = prev.clone();
        if let Some(b) = params.enabled {
            next.enabled = b;
        }
        if let Some(s) = params.base_url {
            next.base_url = s;
        }
        if let Some(s) = params.model {
            next.model = s;
        }
        if let Some(s) = params.effort {
            next.effort = s;
        }
        if let Some(s) = params.context_window {
            let clean = s.trim().to_ascii_lowercase();
            next.context_window =
                if clean.is_empty() || clean == "auto" || clean == "default" { None } else { Some(clean) };
        }
        if let Some(n) = params.max_tokens {
            next.max_tokens = if n == 0 { None } else { Some(n) };
        }
        if let Some(s) = params.language {
            next.language = normalize_ai_language(&s)?;
        }
        if let Some(b) = params.pty_status_arbitration {
            next.pty_status_arbitration = b;
        }
        if let Some(b) = params.notes_digest {
            next.notes_digest = b;
        }
        let explicit_prompt_mode = params.prompt_mode.is_some();
        if let Some(ref raw_pm) = params.prompt_mode {
            let norm = normalize_prompt_mode(raw_pm)?;
            next.prompt_cache_enabled = norm != "compact";
            next.prompt_mode = Some(norm);
        } else if let Some(b) = params.prompt_cache_enabled {
            next.prompt_cache_enabled = b;
            next.prompt_mode = Some(if b { "auto" } else { "compact" }.into());
        }
        if let Some(b) = params.handoff_distill {
            next.handoff_distill = b;
        }
        if let Some(b) = params.checkpoint_labeling {
            next.checkpoint_labeling = b;
        }
        if let Some(b) = params.semantic_summary {
            next.semantic_summary = b;
        }
        if let Some(b) = params.redact_secrets {
            next.redact_secrets = b;
        }

        let (prev_key, _) = read_ai_key(&self.paths);
        let mut key_changed = false;
        if params.clear_key == Some(true) {
            clear_ai_key(&self.paths).map_err(|e| RpcError::internal(format!("clear key: {e}")))?;
            key_changed = prev_key.is_some();
        } else if let Some(key) = params.api_key.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
            && prev_key.as_deref() != Some(key.as_str())
        {
            write_ai_key(&self.paths, &key)?;
            key_changed = true;
        }

        let url_changed =
            next.base_url.trim().trim_end_matches('/') != prev.base_url.trim().trim_end_matches('/');
        let model_changed = next.model.trim() != prev.model.trim();
        // When the user switches model (without explicitly pinning a prompt mode in the same call),
        // reset prompt_mode to "auto" so the runtime re-detects KV-cache support for the new model.
        if model_changed && !explicit_prompt_mode {
            next.prompt_mode = Some("auto".into());
            next.prompt_cache_enabled = true;
        }
        let reset_detection = params.reset_prompt_cache_detection == Some(true);
        if reset_detection && !explicit_prompt_mode {
            next.prompt_mode = Some("auto".into());
            next.prompt_cache_enabled = true;
        }

        let prompt_mode_changed = next.resolved_prompt_mode() != prev.resolved_prompt_mode();
        let cfg_changed = next.enabled != prev.enabled
            || url_changed
            || model_changed
            || next.effort.trim().to_ascii_lowercase() != prev.effort.trim().to_ascii_lowercase()
            || next.context_window != prev.context_window
            || next.max_tokens != prev.max_tokens
            || next.language != prev.language
            || next.pty_status_arbitration != prev.pty_status_arbitration
            || next.notes_digest != prev.notes_digest
            || next.prompt_cache_enabled != prev.prompt_cache_enabled
            || prompt_mode_changed
            || next.handoff_distill != prev.handoff_distill
            || next.checkpoint_labeling != prev.checkpoint_labeling
            || next.semantic_summary != prev.semantic_summary
            || next.redact_secrets != prev.redact_secrets;
        if !cfg_changed && !key_changed && !reset_detection {
            return Ok(self.status());
        }

        if cfg_changed {
            save_config(&self.paths, &mut next)?;
            *self.config.lock().unwrap_or_else(|p| p.into_inner()) = next.clone();
        }
        let lang_changed = next.language != prev.language;

        if url_changed || key_changed || model_changed || prompt_mode_changed || reset_detection {
            let mut caps_map = self.endpoint_caps.lock().unwrap_or_else(|p| p.into_inner());
            if url_changed || key_changed {
                caps_map.clear();
            } else {
                for c in caps_map.values_mut() {
                    c.reset_prompt_cache_detection();
                }
            }
            save_endpoint_caps(&self.paths, &caps_map);
        }
        if url_changed || key_changed {
            self.models_cache.lock().unwrap_or_else(|p| p.into_inner()).clear();
            self.models_url.lock().unwrap_or_else(|p| p.into_inner()).clear();
            save_models_cache(&self.paths, &PersistedAiModelsCache::default());
            self.record_success();
        }
        if url_changed
            || model_changed
            || key_changed
            || lang_changed
            || prompt_mode_changed
            || reset_detection
            || next.redact_secrets != prev.redact_secrets
        {
            self.sessions.clear_all();
            self.result_cache.clear();
        }
        Ok(self.status_refresh_models().await)
    }

    fn full_replication_params(&self, reset_detection: Option<bool>) -> AiSettingsSetParams {
        let cfg = self.config_snapshot();
        let (key, _) = read_ai_key(&self.paths);
        let eff_lang = cfg
            .language
            .clone()
            .or_else(|| read_app_settings_language(&self.paths.app_settings_file))
            .unwrap_or_default();
        AiSettingsSetParams {
            on: None,
            enabled: Some(cfg.enabled),
            base_url: Some(cfg.base_url.clone()),
            model: Some(cfg.model.clone()),
            effort: Some(cfg.effort.clone()),
            context_window: Some(cfg.context_window.clone().unwrap_or_default()),
            max_tokens: Some(cfg.max_tokens.unwrap_or(0)),
            language: Some(eff_lang),
            api_key: key.clone(),
            clear_key: if key.is_none() { Some(true) } else { None },
            pty_status_arbitration: Some(cfg.pty_status_arbitration),
            notes_digest: Some(cfg.notes_digest),
            prompt_cache_enabled: Some(cfg.prompt_cache_enabled),
            prompt_mode: Some(cfg.resolved_prompt_mode().to_string()),
            reset_prompt_cache_detection: reset_detection,
            handoff_distill: Some(cfg.handoff_distill),
            checkpoint_labeling: Some(cfg.checkpoint_labeling),
            semantic_summary: Some(cfg.semantic_summary),
            redact_secrets: Some(cfg.redact_secrets),
        }
    }

    /// Builds the replication payload to push when a remote machine connects so all machines
    /// share this core's single AI configuration. Never overwrites a legacy remote config when
    /// local has neither a base URL nor an API key yet.
    pub fn sync_params_for_connect(&self) -> Option<AiSettingsSetParams> {
        let cfg = self.config_snapshot();
        let (key, _) = read_ai_key(&self.paths);
        if !cfg.base_url.trim().is_empty() || key.is_some() {
            return Some(self.full_replication_params(None));
        }
        let lang = cfg.language.or_else(|| read_app_settings_language(&self.paths.app_settings_file));
        lang.map(|language| AiSettingsSetParams { language: Some(language), ..Default::default() })
    }

    /// Builds the replication payload to broadcast to connected remote machines after a local edit.
    pub fn sync_params_after_update(&self, patch: &AiSettingsSetParams) -> AiSettingsSetParams {
        let cfg = self.config_snapshot();
        let (key, _) = read_ai_key(&self.paths);
        if !cfg.base_url.trim().is_empty() || key.is_some() {
            return self.full_replication_params(patch.reset_prompt_cache_detection);
        }
        if patch.base_url.is_some() || patch.clear_key == Some(true) {
            return self.full_replication_params(patch.reset_prompt_cache_detection);
        }
        let mut forwarded = patch.clone();
        forwarded.on = None;
        forwarded
    }

    pub async fn list_models(
        &self,
        url_override: Option<String>,
        key_override: Option<String>,
    ) -> Result<AiModelsResult, RpcError> {
        let cfg = self.config_snapshot();
        let base_url = url_override.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| cfg.base_url.clone());
        if base_url.trim().is_empty() {
            return Err(RpcError::invalid_params("AI Service URL is empty"));
        }
        let saved_norm = cfg.base_url.trim().trim_end_matches('/');
        let target_norm = base_url.trim().trim_end_matches('/');
        let same_as_saved = !saved_norm.is_empty() && target_norm == saved_norm;
        let (stored_key, _) = read_ai_key(&self.paths);
        let key = key_override
            .filter(|s| !s.trim().is_empty())
            .or_else(|| if same_as_saved { stored_key } else { None });
        let res = fetch_ai_models_detailed(&base_url, key.as_deref()).await;
        if res.ok && !res.models.is_empty() && same_as_saved {
            self.update_models(&res.base_url, res.models.clone());
        }
        Ok(res)
    }

    pub async fn test_connection(
        &self,
        url_override: Option<String>,
        key_override: Option<String>,
        model_override: Option<String>,
    ) -> Result<AiTestResult, RpcError> {
        let cfg = self.config_snapshot();
        let base_url = url_override.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| cfg.base_url.clone());
        if base_url.trim().is_empty() {
            return Err(RpcError::invalid_params("AI Service URL is empty"));
        }
        let saved_norm = cfg.base_url.trim().trim_end_matches('/');
        let target_norm = base_url.trim().trim_end_matches('/');
        let same_as_saved = !saved_norm.is_empty() && target_norm == saved_norm;
        let (stored_key, _) = read_ai_key(&self.paths);
        let key = key_override
            .filter(|s| !s.trim().is_empty())
            .or_else(|| if same_as_saved { stored_key } else { None });
        let model = model_override
            .filter(|s| !s.trim().is_empty())
            .or_else(|| (same_as_saved && !cfg.model.trim().is_empty()).then_some(cfg.model));
        let (res, used_bearer) = test_ai_connection_detailed(&base_url, key.as_deref(), model.as_deref()).await;
        let mut merged = Vec::new();
        if let Some(m) = &res.discovered_model {
            merged.push(m.clone());
        }
        merged.extend(res.available_models.clone());
        if !merged.is_empty() && (same_as_saved || saved_norm.is_empty()) {
            self.update_models(&base_url, merged);
        }
        {
            let mut cir = self.circuit.lock().unwrap_or_else(|p| p.into_inner());
            if res.ok {
                cir.consecutive_failures = 0;
                cir.rate_limit_streak = 0;
                cir.circuit_open = false;
                cir.half_open_probe_in_flight = false;
                cir.cooldown_until = None;
                cir.last_error = None;
            } else {
                cir.last_error = res.error.clone();
            }
        }
        if res.ok {
            if let Some(proto) = res.protocol
                && (same_as_saved || saved_norm.is_empty())
            {
                self.remember_endpoint_protocol(
                    &res.base_url,
                    res.discovered_model.as_deref().or(model.as_deref()),
                    proto,
                    used_bearer,
                );
            }
            let mut caps_map = self.endpoint_caps.lock().unwrap_or_else(|p| p.into_inner());
            for c in caps_map.values_mut() {
                c.reset_prompt_cache_detection();
            }
            save_endpoint_caps(&self.paths, &caps_map);
        }
        Ok(res)
    }

    /// Clears previous prompt-prefix detection records, ensures `prompt_mode = "auto"`, and
    /// immediately executes up to 3 lightweight back-to-back probes against the active endpoint
    /// to detect whether KV prefix caching is effective (`shared_prefix`) or ineffective (`compact`).
    pub async fn probe_prompt_mode(&self) -> Result<AiProbePromptModeResult, RpcError> {
        // 1. Ensure prompt_mode is "auto" and persist if changed.
        let mut cfg = self.config_snapshot();
        if cfg.resolved_prompt_mode() != "auto" || !cfg.prompt_cache_enabled {
            cfg.prompt_mode = Some("auto".into());
            cfg.prompt_cache_enabled = true;
            save_config(&self.paths, &mut cfg)?;
            *self.config.lock().unwrap_or_else(|p| p.into_inner()) = cfg.clone();
        }

        // 2. Clear prior detection records, sessions, and cached results.
        {
            let mut caps_map = self.endpoint_caps.lock().unwrap_or_else(|p| p.into_inner());
            for c in caps_map.values_mut() {
                c.reset_prompt_cache_detection();
            }
            save_endpoint_caps(&self.paths, &caps_map);
        }
        self.sessions.clear_all();
        self.result_cache.clear();

        let base = cfg.base_url.trim().trim_end_matches('/').to_string();
        let (key, _) = read_ai_key(&self.paths);
        if !cfg.enabled || base.is_empty() || (key.is_none() && !allows_keyless_url(&base)) {
            return Ok(AiProbePromptModeResult {
                ok: true,
                detected_mode: "pending".into(),
                probes_run: 0,
                cached_input_tokens: 0,
                input_tokens: 0,
                model: (!cfg.model.trim().is_empty()).then(|| cfg.model.trim().to_string()),
                reason: Some("AI endpoint is not configured; prior detection record cleared.".into()),
                status: self.status(),
            });
        }

        // 3. Context window <= 4096 cannot accommodate the >1024-token shared prefix safely.
        let ctx_tokens = cfg.context_window.as_deref().and_then(parse_ai_token_size);
        if ctx_tokens.is_some_and(|c| c <= 4096) {
            let active_model = if !cfg.model.trim().is_empty() {
                cfg.model.trim().to_string()
            } else {
                self.cached_models().into_iter().next().unwrap_or_default()
            };
            if !active_model.is_empty() {
                let cap_key = format!("{base}|{active_model}");
                let mut map = self.endpoint_caps.lock().unwrap_or_else(|p| p.into_inner());
                let entry = map.entry(cap_key).or_insert_with(|| EndpointCapabilities::default_for_url(&base));
                entry.mark_prompt_cache_ineffective();
                save_endpoint_caps(&self.paths, &map);
            }
            return Ok(AiProbePromptModeResult {
                ok: true,
                detected_mode: "compact".into(),
                probes_run: 0,
                cached_input_tokens: 0,
                input_tokens: 0,
                model: (!active_model.is_empty()).then_some(active_model),
                reason: Some("Context window <= 4096 tokens; compact prompts selected.".into()),
                status: self.status(),
            });
        }

        // 4. User explicitly requested re-detection; clear any transient cooldown so probe can run.
        {
            let mut cir = self.circuit.lock().unwrap_or_else(|p| p.into_inner());
            cir.consecutive_failures = 0;
            cir.rate_limit_streak = 0;
            cir.circuit_open = false;
            cir.half_open_probe_in_flight = false;
            cir.cooldown_until = None;
        }

        // 5. Run up to 3 lightweight back-to-back shared-prefix probes (16 output tokens each).
        let mut probes_run: u8 = 0;
        let mut max_cached: u64 = 0;
        let mut last_input: u64 = 0;
        let mut last_model: Option<String> = None;
        for attempt in 1..=3_u8 {
            probes_run = attempt;
            let user_prompt = format!(
                r#"{{"op":"session_seed","lang":"en","data":{{"probe":"kv_cache_redetect","attempt":{attempt}}}}}"#
            );
            let res = self
                .complete_fg(AiCompleteRequest {
                    op: "session_seed".into(),
                    tier: AiTaskTier::Nano,
                    mode: AiConversationMode::StatelessSharedPrefix,
                    user_prompt,
                    max_tokens: 16,
                    timeout: Duration::from_secs(12),
                    cache_key: None,
                    cache_ttl: None,
                    prompt_cache_bucket: Some("cp".into()),
                    ..Default::default()
                })
                .await;
            match res {
                Ok(resp) => {
                    last_input = last_input.max(resp.usage.input_tokens);
                    max_cached = max_cached.max(resp.usage.cached_input_tokens);
                    if !resp.model.trim().is_empty() {
                        last_model = Some(resp.model.clone());
                    }
                    if resp.usage.cached_input_tokens > 0 {
                        return Ok(AiProbePromptModeResult {
                            ok: true,
                            detected_mode: "shared_prefix".into(),
                            probes_run,
                            cached_input_tokens: resp.usage.cached_input_tokens,
                            input_tokens: resp.usage.input_tokens,
                            model: last_model,
                            reason: None,
                            status: self.status(),
                        });
                    }
                }
                Err(err) => {
                    return Ok(AiProbePromptModeResult {
                        ok: false,
                        detected_mode: "pending".into(),
                        probes_run,
                        cached_input_tokens: max_cached,
                        input_tokens: last_input,
                        model: last_model,
                        reason: Some(err),
                        status: self.status(),
                    });
                }
            }
        }

        // All 3 consecutive identical-prefix probes returned 0 cached tokens.
        if last_input >= 1024 {
            let model_name = last_model.clone().unwrap_or_else(|| cfg.model.trim().to_string());
            if !model_name.is_empty() {
                let cap_key = format!("{base}|{model_name}");
                let mut map = self.endpoint_caps.lock().unwrap_or_else(|p| p.into_inner());
                let entry = map.entry(cap_key).or_insert_with(|| EndpointCapabilities::default_for_url(&base));
                entry.mark_prompt_cache_ineffective();
                save_endpoint_caps(&self.paths, &map);
            }
            return Ok(AiProbePromptModeResult {
                ok: true,
                detected_mode: "compact".into(),
                probes_run,
                cached_input_tokens: 0,
                input_tokens: last_input,
                model: last_model,
                reason: Some("Endpoint returned 0 cached tokens across 3 consecutive shared-prefix probes.".into()),
                status: self.status(),
            });
        }

        Ok(AiProbePromptModeResult {
            ok: true,
            detected_mode: "pending".into(),
            probes_run,
            cached_input_tokens: 0,
            input_tokens: last_input,
            model: last_model,
            reason: Some("Endpoint did not report prompt token usage; keeping auto mode.".into()),
            status: self.status(),
        })
    }

    fn try_cached(&self, op: &str, cache_key: Option<&str>) -> Option<AiCompleteResponse> {
        let ck = cache_key?;
        let cached = self.result_cache.get(ck)?;
        self.record_local_cache_hit(op);
        Some(AiCompleteResponse {
            content: cached,
            model: self.config_snapshot().model,
            latency_ms: 0,
            usage: AiUsage::default(),
            from_local_cache: true,
            session_reused: false,
        })
    }

    /// Removes one entry from the local response cache (e.g. when a caller rejects a malformed or
    /// drifted model reply so subsequent retries do not replay the bad response).
    pub fn invalidate_cached(&self, cache_key: &str) {
        self.result_cache.remove(cache_key);
    }

    /// Coalesces concurrent requests sharing the same `cache_key` (singleflight).
    /// Returns `Ok(guard)` when this caller owns execution, or `Err(cached_resp)` if an
    /// in-flight leader populated the cache while we waited.
    async fn acquire_singleflight(
        &self,
        op: &str,
        cache_key: Option<&str>,
        timeout: Duration,
    ) -> Result<InflightGuard<'_>, AiCompleteResponse> {
        let Some(ck) = cache_key.filter(|s| !s.is_empty()) else {
            return Ok(InflightGuard { rt: self, key: None, notify: None });
        };
        let deadline = Instant::now() + timeout.max(Duration::from_secs(1));
        loop {
            if let Some(hit) = self.try_cached(op, Some(ck)) {
                return Err(hit);
            }
            let waiter = {
                let mut map = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(existing) = map.get(ck) {
                    Some(existing.clone())
                } else {
                    let notify = Arc::new(tokio::sync::Notify::new());
                    map.insert(ck.to_string(), notify.clone());
                    return Ok(InflightGuard { rt: self, key: Some(ck.to_string()), notify: Some(notify) });
                }
            };
            if let Some(notify) = waiter {
                let rem = deadline.saturating_duration_since(Instant::now());
                if rem.is_zero() {
                    return Ok(InflightGuard { rt: self, key: None, notify: None });
                }
                let _ = tokio::time::timeout(rem, notify.notified()).await;
            }
        }
    }

    /// Foreground interactive completion guarded by a dedicated queuing semaphore (`fg_sem`).
    /// Never starved or silently dropped by background tasks; returns exact error strings.
    pub async fn complete_fg(&self, req: AiCompleteRequest) -> Result<AiCompleteResponse, String> {
        if let Some(hit) = self.try_cached(&req.op, req.cache_key.as_deref()) {
            return Ok(hit);
        }
        let permit = tokio::time::timeout(req.timeout.max(Duration::from_secs(1)), self.fg_sem.clone().acquire_owned())
            .await
            .map_err(|_| "foreground AI queue timed out".to_string())?
            .map_err(|_| "foreground AI semaphore closed".to_string())?;
        let res = self.complete(req).await;
        drop(permit);
        res
    }

    /// Non-blocking background completion guarded by a bounded concurrency semaphore (`bg_sem`).
    /// Drops immediately (returns `None`) if background slots are saturated or circuit is open.
    pub async fn complete_bg(&self, req: AiCompleteRequest) -> Option<AiCompleteResponse> {
        // Check local cache BEFORE acquiring semaphore so 0-token hits never contend.
        if let Some(hit) = self.try_cached(&req.op, req.cache_key.as_deref()) {
            return Some(hit);
        }
        // If an identical request is already in-flight, wait for its cached result without burning a bg slot.
        if let Some(ck) = req.cache_key.as_deref().filter(|s| !s.is_empty()) {
            let existing = self.inflight.lock().unwrap_or_else(|p| p.into_inner()).get(ck).cloned();
            if let Some(notify) = existing {
                let _ = tokio::time::timeout(req.timeout.max(Duration::from_secs(1)), notify.notified()).await;
                if let Some(hit) = self.try_cached(&req.op, Some(ck)) {
                    return Some(hit);
                }
            }
        }
        let Ok(_permit) = self.bg_sem.clone().try_acquire_owned() else {
            return None;
        };
        self.complete(req).await.ok()
    }

    /// Executes an OpenAI-compatible chat completion with:
    /// - Local zero-token LRU cache & singleflight deduplication
    /// - Automatic secret redaction for outbound untrusted text
    /// - Shared byte-immutable system prefix (>1024 tokens) with automatic KV-cache degradation
    /// - Lock-free `SeededStateless` or append-only `AiSessionPool`
    /// - Persistent endpoint capability memory (`response_format`, `enable_thinking`, `cache_control`, `prompt_cache_key`)
    /// - Graded circuit breaker with Half-Open single-probe recovery & per-op observability
    pub async fn complete(&self, req: AiCompleteRequest) -> Result<AiCompleteResponse, String> {
        let started = Instant::now();
        let _inflight_guard = match self.acquire_singleflight(&req.op, req.cache_key.as_deref(), req.timeout).await {
            Ok(g) => g,
            Err(cached) => return Ok(cached),
        };

        let cfg = self.config_snapshot();
        let has_override = req.url_override.as_deref().is_some_and(|s| !s.trim().is_empty());
        if !has_override && !cfg.enabled {
            return Err("AI assistant is disabled".into());
        }
        let mut probe_guard = if !has_override { Some(self.try_enter_circuit()?) } else { None };

        let raw_base =
            req.url_override.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(cfg.base_url.trim());
        if raw_base.is_empty() {
            return Err("AI Service URL is not configured".into());
        }
        let base = validate_http_url(raw_base, "ai.baseUrl").map_err(|e| e.message)?;

        let (stored_key, _) = read_ai_key(&self.paths);
        let key = req.key_override.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from).or(stored_key);
        if key.is_none() && !allows_keyless_url(&base) {
            return Err("AI API key is not configured".into());
        }

        let mut model = req
            .model_override
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(cfg.model.trim())
            .to_string();
        if model.is_empty()
            && let Some(first) = self.cached_models().into_iter().next()
        {
            model = first;
        }
        if model.is_empty()
            && let Some(m) = fetch_ai_models(&base, key.as_deref()).await.into_iter().next()
        {
            model = m;
        }
        if model.is_empty() {
            if let Some(g) = probe_guard.as_mut() {
                g.settle();
            }
            self.record_failure("no model configured or discovered");
            self.record_op_failure(&req.op);
            return Err("no model configured or discovered".into());
        }

        let cap_key = format!("{base}|{model}");
        let initial_caps = self
            .endpoint_caps
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&cap_key)
            .copied()
            .unwrap_or_else(|| EndpointCapabilities::default_for_url(&base));
        let mut caps = initial_caps;
        let remembered_proto = if let Some(p) = caps.protocol {
            Some(p)
        } else if let Some((p, bearer)) = self.resolve_remembered_protocol(&base, &model) {
            caps.protocol = Some(p);
            caps.anthropic_bearer_auth = bearer;
            Some(p)
        } else {
            None
        };

        let prompt_mode = cfg.resolved_prompt_mode();
        let ctx_tokens = cfg.context_window.as_deref().and_then(parse_ai_token_size);
        let small_ctx = ctx_tokens.is_some_and(|c| c <= 4096);
        let use_shared_prefix = match prompt_mode {
            "shared_prefix" => !small_ctx,
            "compact" => false,
            _ => !small_ctx && caps.prompt_cache_effective,
        };
        let enable_session_cache = prompt_mode != "compact";

        let cfg_effort = cfg.effort.trim();
        let effective_effort = match req.tier {
            // Nano tasks (pty_status, agent_digest, route_candidate) have tight latency/token
            // budgets; never let global high/medium reasoning effort burn tokens in thinking.
            AiTaskTier::Nano => "none",
            AiTaskTier::Fast => match cfg_effort {
                "none" => "none",
                _ => "low",
            },
            AiTaskTier::Standard => match cfg_effort {
                "high" | "medium" | "low" | "none" => cfg_effort,
                _ => "auto",
            },
        };
        let cap_strict = cfg.max_tokens.is_none() && matches!(req.tier, AiTaskTier::Nano | AiTaskTier::Fast);
        let eff_max_tokens =
            effective_max_tokens(req.max_tokens.max(32), cfg.max_tokens, effective_effort, ctx_tokens, cap_strict);
        let temperature = match req.tier {
            AiTaskTier::Nano => 0.0,
            AiTaskTier::Fast => 0.1,
            AiTaskTier::Standard => 0.2,
        };

        let raw_user = if cfg.redact_secrets { redact_secrets(&req.user_prompt) } else { req.user_prompt.clone() };
        let bounded_user = match ctx_tokens {
            Some(ctx) => {
                let reserve = if use_shared_prefix { eff_max_tokens + 1800 } else { eff_max_tokens + 512 };
                let input_budget_tokens = ctx.saturating_sub(reserve).max(512);
                truncate_for_context(&raw_user, (input_budget_tokens as usize).saturating_mul(3))
            }
            None => raw_user,
        };

        // Choose system prompt + conversation messages (holding per-scope session mutex ONLY for Session mode).
        let (system_prompt, session_slot_opt, seed_ctx_opt, cache_bucket, used_canonical_prefix) = match &req.mode {
            AiConversationMode::StatelessSharedPrefix => {
                let sys = if use_shared_prefix {
                    HERDR2_CONTROL_PLANE_SYSTEM.to_string()
                } else {
                    compact_system_for_op(&req.op).to_string()
                };
                let bucket = req.prompt_cache_bucket.clone().unwrap_or_else(|| "cp".into());
                (sys, None, None, Some(format!("h2:{bucket}:v1")), use_shared_prefix)
            }
            AiConversationMode::SeededStateless { seed_context } => {
                let sys = if use_shared_prefix {
                    HERDR2_CONTROL_PLANE_SYSTEM.to_string()
                } else {
                    compact_system_for_op(&req.op).to_string()
                };
                let clean_seed = if cfg.redact_secrets { redact_secrets(seed_context) } else { seed_context.clone() };
                let bucket = req.prompt_cache_bucket.clone().unwrap_or_else(|| "cp".into());
                (sys, None, Some(clean_seed), Some(format!("h2:{bucket}:v1")), use_shared_prefix)
            }
            AiConversationMode::Session { key: scope_key, seed_context } => {
                let sys = if use_shared_prefix {
                    HERDR2_CONTROL_PLANE_SYSTEM.to_string()
                } else {
                    compact_system_for_op(&req.op).to_string()
                };
                let clean_seed =
                    seed_context.as_ref().map(|s| if cfg.redact_secrets { redact_secrets(s) } else { s.clone() });
                let slot = enable_session_cache.then(|| self.sessions.acquire_slot(scope_key));
                let cache_id = format!("h2:s:{}", fingerprint_hex(&[scope_key]));
                (sys, slot, clean_seed, Some(cache_id), use_shared_prefix)
            }
            AiConversationMode::CustomSystem(custom) => {
                let framed = format!(
                    "You are a pure text assistant with NO tools or shell access. \
                     Never emit <tool_use>, DSML, XML tool tags, or shell commands. {custom}"
                );
                (framed, None, None, None, false)
            }
        };

        let system_hash = fnv1a_64(system_prompt.as_bytes());
        let session_budget = ctx_tokens.map(|c| (c / 3).clamp(2048, MAX_SESSION_TOKENS)).unwrap_or(MAX_SESSION_TOKENS);

        // Hold per-scope session lock across prepare -> HTTP -> commit ONLY for mutable Session mode.
        let mut session_guard = match &session_slot_opt {
            Some(slot) => Some(slot.lock().await),
            None => None,
        };

        let (messages, session_reused, fresh_base) = if let Some(st) = session_guard.as_deref_mut() {
            st.prepare_turn(&system_prompt, system_hash, &model, session_budget, seed_ctx_opt.as_deref(), &bounded_user)
        } else if seed_ctx_opt.is_some() {
            (build_seeded_stateless_messages(&system_prompt, seed_ctx_opt.as_deref(), &bounded_user), false, Vec::new())
        } else {
            (vec![ChatMessage::system(&system_prompt), ChatMessage::user(&bounded_user)], false, Vec::new())
        };

        let prompt_cache_key = enable_session_cache.then_some(cache_bucket.as_deref()).flatten();
        let expect_json = op_expects_json(&req.op);
        let protocols = protocol_probe_order(&base, key.as_deref(), remembered_proto);
        let has_key = key.as_deref().map(str::trim).is_some_and(|s| !s.is_empty());

        #[derive(Clone)]
        struct AttemptError {
            status: Option<u16>,
            retry_after_secs: Option<u64>,
            message: String,
            is_parse_error: bool,
        }

        let mut protocol_errors: Vec<AttemptError> = Vec::new();

        for (proto_idx, proto) in protocols.into_iter().enumerate() {
            let endpoints = match proto {
                AiProtocol::Openai => chat_endpoints(&base),
                AiProtocol::Anthropic => anthropic_messages_endpoints(&base),
            };
            let candidates = match proto {
                AiProtocol::Openai => build_chat_payload_candidates(
                    &model,
                    &messages,
                    eff_max_tokens,
                    effective_effort,
                    ctx_tokens,
                    temperature,
                    prompt_cache_key,
                    expect_json,
                    caps,
                ),
                AiProtocol::Anthropic => build_anthropic_payload_candidates(
                    &model,
                    &messages,
                    eff_max_tokens,
                    temperature,
                    prompt_cache_key,
                    caps,
                ),
            };
            let auth_styles: Vec<HttpAuthStyle> = match proto {
                AiProtocol::Openai => vec![HttpAuthStyle::Bearer],
                AiProtocol::Anthropic => {
                    if !has_key {
                        vec![HttpAuthStyle::AnthropicApiKey]
                    } else if caps.anthropic_bearer_auth {
                        vec![HttpAuthStyle::AnthropicBearer, HttpAuthStyle::AnthropicApiKey]
                    } else {
                        vec![HttpAuthStyle::AnthropicApiKey, HttpAuthStyle::AnthropicBearer]
                    }
                }
            };

            let mut proto_err: Option<AttemptError> = None;
            let mut should_try_other_protocol = false;

            for url in endpoints {
                let mut resp: Result<client::HttpJsonResponse, String> = Err("no payload candidates".into());
                let mut winning_variant = None;
                let mut winning_style = auth_styles[0];
                let mut fell_back = false;

                for (idx, variant) in candidates.iter().enumerate() {
                    if idx > 0 {
                        let skip = match proto {
                            AiProtocol::Openai => {
                                (variant.used_num_ctx && !caps.supports_num_ctx)
                                    || (variant.used_cache_key && !caps.supports_prompt_cache_key)
                                    || (variant.used_cache_control && !caps.supports_cache_control)
                                    || (variant.used_json_object && !caps.supports_json_object)
                                    || (variant.used_enable_thinking && !caps.supports_enable_thinking)
                                    || (variant.used_effort.is_some() && !caps.supports_reasoning_effort)
                                    || (variant.used_effort.as_deref() == Some("none") && !caps.supports_effort_none)
                                    || (variant.used_temperature && !caps.supports_temperature)
                            }
                            AiProtocol::Anthropic => {
                                (variant.used_cache_control && !caps.supports_anthropic_cache_control)
                                    || (variant.used_temperature && !caps.supports_temperature)
                            }
                        };
                        if skip {
                            continue;
                        }
                    }

                    let mut used_style = auth_styles[0];
                    for (s_idx, style) in auth_styles.iter().enumerate() {
                        used_style = *style;
                        resp = http_json_request_with_auth(
                            "POST",
                            &url,
                            key.as_deref(),
                            *style,
                            Some(&variant.body),
                            req.timeout,
                        )
                        .await;
                        if let Ok(ref http_resp) = resp
                            && (http_resp.status == 401 || http_resp.status == 403)
                            && s_idx + 1 < auth_styles.len()
                        {
                            continue;
                        }
                        break;
                    }

                    let has_next = idx + 1 < candidates.len();
                    if let Ok(ref http_resp) = resp {
                        if (200..300).contains(&http_resp.status) {
                            winning_variant = Some(variant);
                            winning_style = used_style;
                            break;
                        }
                        if has_next && should_retry_without_optional_fields(http_resp.status, &http_resp.body) {
                            refine_capabilities_from_error_for_protocol(&mut caps, proto, variant, &http_resp.body);
                            fell_back = true;
                            continue;
                        }
                    }
                    break;
                }

                if let Some(win) = winning_variant
                    && fell_back
                {
                    record_winning_variant_for_protocol(&mut caps, proto, win);
                }

                match resp {
                    Ok(http_resp) if (200..300).contains(&http_resp.status) => {
                        let latency_ms = started.elapsed().as_millis() as u64;
                        let parsed: Value = match serde_json::from_str(&http_resp.body) {
                            Ok(v) => v,
                            Err(e) => {
                                proto_err = Some(AttemptError {
                                    status: Some(http_resp.status),
                                    retry_after_secs: None,
                                    message: format!("failed to parse AI JSON response: {e}"),
                                    is_parse_error: true,
                                });
                                should_try_other_protocol = proto_idx == 0;
                                break;
                            }
                        };
                        if !is_valid_protocol_response(&parsed, proto) {
                            proto_err = Some(AttemptError {
                                status: Some(http_resp.status),
                                retry_after_secs: None,
                                message: format!("unexpected AI response shape for {proto} protocol"),
                                is_parse_error: true,
                            });
                            should_try_other_protocol = proto_idx == 0;
                            break;
                        }

                        caps.protocol = Some(proto);
                        if proto == AiProtocol::Anthropic {
                            caps.anthropic_bearer_auth = winning_style == HttpAuthStyle::AnthropicBearer;
                        }
                        let (mut in_tok, mut out_tok, mut cache_tok) = parse_usage_tokens(&parsed);
                        if prompt_mode == "auto"
                            && used_canonical_prefix
                            && matches!(req.mode, AiConversationMode::StatelessSharedPrefix)
                        {
                            caps.record_shared_prefix_usage(in_tok, cache_tok);
                        }
                        if caps != initial_caps {
                            let mut map = self.endpoint_caps.lock().unwrap_or_else(|p| p.into_inner());
                            map.insert(cap_key.clone(), caps);
                            let base_entry = map
                                .entry(base.clone())
                                .or_insert_with(|| EndpointCapabilities::default_for_url(&base));
                            base_entry.protocol = Some(proto);
                            base_entry.anthropic_bearer_auth = caps.anthropic_bearer_auth;
                            save_endpoint_caps(&self.paths, &map);
                        }

                        let (mut raw_content, mut finish_reason) = extract_response_content(&parsed, proto);
                        // Always-thinking models (e.g. DeepSeek reasoning proxies ignoring `reasoning_effort`)
                        // can exhaust tight `Nano`/`Fast` `max_tokens` inside `reasoning_content` before emitting
                        // JSON `content`. Retry once with an expanded token ceiling when that happens.
                        if raw_content.is_empty()
                            && finish_reason == "length"
                            && out_tok > 0
                            && eff_max_tokens < 2048
                            && let Some(win) = winning_variant
                            && let Ok(mut retry_json) = serde_json::from_str::<Value>(&win.body)
                        {
                            let bumped = (eff_max_tokens.saturating_mul(3)).clamp(1024, 2560);
                            retry_json["max_tokens"] = json!(bumped);
                            if let Ok(retry_resp) = http_json_request_with_auth(
                                "POST",
                                &url,
                                key.as_deref(),
                                winning_style,
                                Some(&retry_json.to_string()),
                                req.timeout,
                            )
                            .await
                                && (200..300).contains(&retry_resp.status)
                                && let Ok(retry_parsed) = serde_json::from_str::<Value>(&retry_resp.body)
                                && is_valid_protocol_response(&retry_parsed, proto)
                            {
                                let (r_in, r_out, r_cache) = parse_usage_tokens(&retry_parsed);
                                in_tok = in_tok.saturating_add(r_in);
                                out_tok = out_tok.saturating_add(r_out);
                                cache_tok = cache_tok.saturating_add(r_cache);
                                let (rc, rf) = extract_response_content(&retry_parsed, proto);
                                raw_content = rc;
                                finish_reason = rf;
                            }
                        }
                        let usage =
                            AiUsage { input_tokens: in_tok, output_tokens: out_tok, cached_input_tokens: cache_tok };
                        if raw_content.is_empty() || raw_content.contains("<tool_use") || raw_content.contains("DSML") {
                            let reason = if raw_content.is_empty() {
                                if finish_reason == "length" {
                                    "empty assistant content (finish_reason=length)"
                                } else {
                                    "empty assistant content"
                                }
                            } else {
                                "unexpected tool-call markup in content"
                            };
                            if let Some(g) = probe_guard.as_mut() {
                                g.settle();
                            }
                            // Content-only errors must NOT trip the global circuit breaker.
                            self.record_success();
                            self.record_op_failure(&req.op);
                            return Err(reason.into());
                        }

                        if let Some(g) = probe_guard.as_mut() {
                            g.settle();
                        }
                        self.record_success();
                        self.record_network_usage(&req.op, &usage, session_reused, latency_ms);
                        if let Some(st) = session_guard.as_deref_mut() {
                            st.commit_turn(system_hash, &model, fresh_base, bounded_user, raw_content.clone());
                        }
                        if let (Some(ck), Some(ttl)) = (req.cache_key, req.cache_ttl) {
                            self.result_cache.put(ck, raw_content.clone(), ttl);
                        }
                        return Ok(AiCompleteResponse {
                            content: raw_content,
                            model,
                            latency_ms,
                            usage,
                            from_local_cache: false,
                            session_reused,
                        });
                    }
                    Ok(http_resp) => {
                        let status = http_resp.status;
                        let snippet: String = http_resp.body.chars().take(160).collect();
                        let msg = format!("HTTP {status}: {snippet}");
                        proto_err = Some(AttemptError {
                            status: Some(status),
                            retry_after_secs: http_resp.retry_after_secs,
                            message: msg,
                            is_parse_error: false,
                        });
                        if status == 401 || status == 403 || status == 429 {
                            should_try_other_protocol = proto_idx == 0 && remembered_proto.is_none() && status != 429;
                            break;
                        }
                        if matches!(status, 404 | 405 | 501) || (remembered_proto.is_none() && proto_idx == 0) {
                            should_try_other_protocol = proto_idx == 0;
                        }
                    }
                    Err(e) => {
                        proto_err = Some(AttemptError {
                            status: None,
                            retry_after_secs: None,
                            message: e,
                            is_parse_error: false,
                        });
                    }
                }
            }

            if let Some(err) = proto_err {
                protocol_errors.push(err);
            }
            if !should_try_other_protocol {
                break;
            }
        }

        let chosen_err = match (protocol_errors.first(), protocol_errors.get(1)) {
            (Some(p), Some(s)) => {
                let p_missing = matches!(p.status, Some(404 | 405 | 501));
                let s_missing = matches!(s.status, Some(404 | 405 | 501));
                if p_missing && !s_missing { s.clone() } else { p.clone() }
            }
            (Some(p), None) => p.clone(),
            _ => AttemptError {
                status: None,
                retry_after_secs: None,
                message: "AI endpoint did not respond".into(),
                is_parse_error: false,
            },
        };

        if let Some(g) = probe_guard.as_mut() {
            g.settle();
        }
        if chosen_err.is_parse_error {
            self.record_success();
            self.record_parse_error(&req.op);
            self.record_op_failure(&req.op);
        } else {
            self.record_op_failure(&req.op);
            match chosen_err.status {
                Some(401 | 403) => self.record_auth_failure(&chosen_err.message),
                Some(429) => self.record_rate_limit(chosen_err.retry_after_secs, &chosen_err.message),
                _ => self.record_failure(&chosen_err.message),
            }
        }
        Err(chosen_err.message)
    }

    /// Out-of-lock async summary for failed/non-zero-exited child agents delivered via `<h2>` notes.
    pub async fn summarize_child_note(
        &self,
        address: &str,
        news_seq: u64,
        status_label: &str,
        exit_code: Option<i32>,
        cwd: Option<&str>,
        raw_output: &str,
    ) -> Option<ChildNoteDigest> {
        if !self.notes_digest_enabled() {
            return None;
        }
        let compact_tail = strip_and_dedup_lines(raw_output, 80, 220, 3200);
        let line_count = compact_tail.lines().count();
        if line_count < 6 && compact_tail.chars().count() < 360 {
            return None;
        }
        let lang = self.preferred_prompt_lang();
        let cache_key =
            format!("notes_digest:{lang}:{address}:{news_seq}:{}", fingerprint_hex(&[status_label, &compact_tail]));
        let payload = json!({
            "address": address,
            "status": status_label,
            "exit_code": exit_code,
            "cwd": cwd.unwrap_or(""),
            "tail": compact_tail,
        });
        let user_prompt = format_op_user_message("notes_digest", lang, &payload);
        let resp = self
            .complete_bg(AiCompleteRequest {
                op: "notes_digest".into(),
                tier: AiTaskTier::Fast,
                mode: AiConversationMode::StatelessSharedPrefix,
                user_prompt,
                max_tokens: 160,
                timeout: Duration::from_secs(4),
                cache_key: Some(cache_key),
                cache_ttl: Some(Duration::from_secs(600)),
                prompt_cache_bucket: Some("cp".into()),
                ..Default::default()
            })
            .await?;

        let parsed = extract_json_object(&resp.content)?;
        let root_cause = parsed["root_cause"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())?
            .chars()
            .take(180)
            .collect::<String>();
        let files: Vec<String> = parsed["files"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::trim))
                    .filter(|s| !s.is_empty())
                    .take(5)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let next_step = parsed["next_step"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("")
            .chars()
            .take(100)
            .collect::<String>();
        Some(ChildNoteDigest { root_cause, files, next_step })
    }

    pub async fn rpc_complete(&self, p: AiCompleteParams) -> Result<AiCompleteResult, RpcError> {
        if p.user_prompt.trim().is_empty() {
            return Err(RpcError::invalid_params("user_prompt cannot be empty"));
        }
        let op = p.op.unwrap_or_else(|| "custom".into());
        let mode = if let Some(sys) = p.system_prompt.filter(|s| !s.trim().is_empty()) {
            AiConversationMode::CustomSystem(sys)
        } else if let Some(sk) = p.session_key.filter(|s| !s.trim().is_empty()) {
            AiConversationMode::Session { key: sk, seed_context: None }
        } else {
            AiConversationMode::StatelessSharedPrefix
        };
        let timeout_secs = p.timeout_secs.unwrap_or(30).clamp(5, 120);
        let res = self
            .complete(AiCompleteRequest {
                op,
                tier: p.tier.unwrap_or(AiTaskTier::Fast),
                mode,
                user_prompt: p.user_prompt,
                max_tokens: p.max_tokens.unwrap_or(512).clamp(32, 4096),
                timeout: Duration::from_secs(timeout_secs),
                ..Default::default()
            })
            .await
            .map_err(|e| RpcError::new("unavailable", e, "check AI Assistant settings"))?;
        Ok(AiCompleteResult {
            content: res.content,
            model: res.model,
            latency_ms: res.latency_ms,
            usage: res.usage,
            from_local_cache: res.from_local_cache,
            session_reused: res.session_reused,
        })
    }

    fn record_local_cache_hit(&self, op: &str) {
        let mut u = self.usage.lock().unwrap_or_else(|p| p.into_inner());
        u.local_cache_hits += 1;
        *u.by_op.entry(op.to_string()).or_default() += 1;
        u.op_metrics.entry(op.to_string()).or_default().local_cache_hits += 1;
    }

    fn record_network_usage(&self, op: &str, usage: &AiUsage, session_reused: bool, latency_ms: u64) {
        let mut u = self.usage.lock().unwrap_or_else(|p| p.into_inner());
        u.total_calls += 1;
        if session_reused {
            u.sessions_reused += 1;
        }
        u.input_tokens += usage.input_tokens;
        u.cached_input_tokens += usage.cached_input_tokens;
        u.output_tokens += usage.output_tokens;
        *u.by_op.entry(op.to_string()).or_default() += 1;
        let m = u.op_metrics.entry(op.to_string()).or_default();
        m.calls += 1;
        m.total_latency_ms = m.total_latency_ms.saturating_add(latency_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_mock_openai(responses: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let cap_clone = captured.clone();
        let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(responses)));

        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let cap = cap_clone.clone();
                let q = queue.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    let mut header_end = None;
                    let mut content_len = 0usize;
                    while let Ok(n) = sock.read(&mut tmp).await {
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if header_end.is_none()
                            && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                        {
                            header_end = Some(pos + 4);
                            let head = String::from_utf8_lossy(&buf[..pos]);
                            for line in head.lines() {
                                if let Some((k, v)) = line.split_once(':')
                                    && k.trim().eq_ignore_ascii_case("content-length")
                                {
                                    content_len = v.trim().parse().unwrap_or(0);
                                }
                            }
                        }
                        if let Some(he) = header_end
                            && buf.len() >= he + content_len
                        {
                            break;
                        }
                    }
                    let req_line =
                        String::from_utf8_lossy(&buf[..buf.iter().position(|b| *b == b'\r').unwrap_or(buf.len())])
                            .into_owned();
                    if req_line.starts_with("GET ") && req_line.contains("/models") {
                        let models_body = r#"{"data":[{"id":"test-fast-model"}]}"#;
                        let http = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{models_body}",
                            models_body.len()
                        );
                        let _ = sock.write_all(http.as_bytes()).await;
                        return;
                    }
                    if let Some(he) = header_end {
                        let body = &buf[he..buf.len().min(he + content_len)];
                        if let Ok(val) = serde_json::from_slice::<serde_json::Value>(body) {
                            cap.lock().unwrap().push(val);
                        }
                    }
                    let (status, resp_body) =
                        q.lock().unwrap().pop_front().unwrap_or_else(|| {
                            (200, r#"{"choices":[{"message":{"content":"{\"ok\":true}"}}]}"#.into())
                        });
                    let status_text = if status == 200 { "OK" } else { "Bad Request" };
                    let http = format!(
                        "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                        resp_body.len()
                    );
                    let _ = sock.write_all(http.as_bytes()).await;
                });
            }
        });
        (format!("http://127.0.0.1:{}/v1", addr.port()), captured)
    }

    #[tokio::test]
    async fn shared_prefix_session_reuse_local_cache_and_cap_memory() {
        let tmp = crate::test_tmp().join(format!("ai-rt-{}", crate::platform::random_hex()));
        let paths = Paths {
            socket: tmp.join("core.sock"),
            holder_socket: tmp.join("holder.sock"),
            state: tmp.join("state"),
            config: tmp.join("config/config.toml"),
        };
        paths.create_dirs().unwrap();

        let (url, captured) = spawn_mock_openai(vec![
            // Call 1: reject prompt_cache_key once -> client adapts and remembers!
            (400, r#"{"error":{"message":"unknown field prompt_cache_key"}}"#.into()),
            // Retry of Call 1 succeeds with cached_tokens metric.
            (
                200,
                r#"{"choices":[{"message":{"content":"{\"state\":\"blocked\",\"kind\":\"question\",\"confidence\":0.91,\"summary\":\"选方案\"}"}}],"usage":{"prompt_tokens":1300,"completion_tokens":28,"prompt_tokens_details":{"cached_tokens":0}}}"#.into(),
            ),
            // Call 2 (Session Turn 1): should NOT send prompt_cache_key anymore!
            (
                200,
                r#"{"choices":[{"message":{"content":"{\"looping\":false,\"confidence\":0.2}"}}],"usage":{"prompt_tokens":1450,"completion_tokens":18,"prompt_cache_hit_tokens":1200}}"#.into(),
            ),
            // Call 3 (Session Turn 2): appends delta onto Turn 1!
            (
                200,
                r#"{"choices":[{"message":{"content":"{\"looping\":true,\"confidence\":0.9,\"pattern\":\"osc\",\"root_cause\":\"dup\",\"steer_hint\":\"stop\"}"}}],"usage":{"prompt_tokens":1600,"completion_tokens":30,"cache_read_input_tokens":1420}}"#.into(),
            ),
        ])
        .await;

        let rt = AiRuntime::open(&paths).unwrap();
        rt.update_settings(AiSettingsSetParams {
            enabled: Some(true),
            base_url: Some(url),
            model: Some("test-fast-model".into()),
            ..Default::default()
        })
        .await
        .unwrap();

        // 1. Terminal state classification -> network call + local cache populate
        let r1 = classify_terminal_state(
            &rt,
            ClassifyTerminalStateParams {
                program: Some("claude".into()),
                raw_title: "⠋ Claude (12s)".into(),
                tail_lines: vec!["请选择方案 1 或 2 (14:01:02)".into()],
                language: Some("zh-CN".into()),
                ..Default::default()
            },
        )
        .await;
        assert!(r1.used_ai && r1.accepted && !r1.from_cache);
        assert_eq!(r1.state.as_deref(), Some("blocked"));

        // 2. Identical prompt with different spinner/clock -> 0-token local cache hit!
        let r2 = classify_terminal_state(
            &rt,
            ClassifyTerminalStateParams {
                program: Some("claude".into()),
                raw_title: "⠙ Claude (45s)".into(),
                tail_lines: vec!["请选择方案 1 或 2 (14:01:35)".into()],
                language: Some("zh-CN".into()),
                ..Default::default()
            },
        )
        .await;
        assert!(r2.used_ai && r2.accepted && r2.from_cache);

        // 3. Multi-turn session Turn 1 & Turn 2 share exact same byte-identical system prompt as Call 1!
        let s1 = rt
            .complete(AiCompleteRequest {
                op: "loop_detect".into(),
                tier: AiTaskTier::Standard,
                mode: AiConversationMode::Session {
                    key: "drove:loop:att_1".into(),
                    seed_context: Some("Task: fix auth".into()),
                },
                user_prompt: "delta 1..8".into(),
                max_tokens: 160,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(!s1.session_reused);

        let s2 = rt
            .complete(AiCompleteRequest {
                op: "loop_detect".into(),
                tier: AiTaskTier::Standard,
                mode: AiConversationMode::Session {
                    key: "drove:loop:att_1".into(),
                    seed_context: Some("Task: fix auth".into()),
                },
                user_prompt: "delta 9..12".into(),
                max_tokens: 160,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(s2.session_reused);

        let reqs = captured.lock().unwrap();
        // 4 HTTP calls total (call1 400 + call1 retry + s1 + s2; r2 hit local cache!)
        assert_eq!(reqs.len(), 4);
        // Byte-identical system prefix across pty_status and loop_detect!
        assert_eq!(reqs[1]["messages"][0]["content"], reqs[2]["messages"][0]["content"]);
        assert_eq!(reqs[2]["messages"][0]["content"], reqs[3]["messages"][0]["content"]);
        // s1 remembered prompt_cache_key rejection from call 1!
        assert!(reqs[2].get("prompt_cache_key").is_none());
        // s2 appended incrementally onto s1's exact messages!
        assert_eq!(reqs[2]["messages"].as_array().unwrap().len(), 4);
        assert_eq!(reqs[3]["messages"].as_array().unwrap().len(), 6);

        let st = rt.status();
        assert_eq!(st.usage.total_calls, 3);
        assert_eq!(st.usage.local_cache_hits, 1);
        assert_eq!(st.usage.sessions_reused, 1);
        assert_eq!(st.usage.cached_input_tokens, 2620);
        assert!(st.usage.cache_hit_ratio > 0.55);

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[tokio::test]
    async fn nano_and_fast_tiers_cap_reasoning_effort_even_when_global_effort_is_high() {
        let tmp = crate::test_tmp().join(format!("ai-effort-{}", crate::platform::random_hex()));
        let paths = Paths {
            socket: tmp.join("core.sock"),
            holder_socket: tmp.join("holder.sock"),
            state: tmp.join("state"),
            config: tmp.join("config/config.toml"),
        };
        paths.create_dirs().unwrap();

        let (url, captured) = spawn_mock_openai(vec![
            (200, r#"{"choices":[{"message":{"content":"{\"semantic_title\":\"修复重命名\",\"micro_summary\":\"已完成测试\"}"}}]}"#.into()),
            (200, r#"{"choices":[{"message":{"content":"{\"headline\":\"all good\"}"}}]}"#.into()),
            (200, r#"{"choices":[{"message":{"content":"{\"ok\":true}"}}]}"#.into()),
        ])
        .await;

        let rt = AiRuntime::open(&paths).unwrap();
        rt.update_settings(AiSettingsSetParams {
            enabled: Some(true),
            base_url: Some(url),
            model: Some("reasoning-model".into()),
            effort: Some("high".into()),
            ..Default::default()
        })
        .await
        .unwrap();

        rt.complete(AiCompleteRequest {
            op: "agent_digest".into(),
            tier: AiTaskTier::Nano,
            user_prompt: "digest".into(),
            max_tokens: 180,
            ..Default::default()
        })
        .await
        .unwrap();

        rt.complete(AiCompleteRequest {
            op: "catchup_briefing".into(),
            tier: AiTaskTier::Fast,
            user_prompt: "brief".into(),
            max_tokens: 200,
            ..Default::default()
        })
        .await
        .unwrap();

        rt.complete(AiCompleteRequest {
            op: "task_diagnose".into(),
            tier: AiTaskTier::Standard,
            user_prompt: "diagnose".into(),
            max_tokens: 400,
            ..Default::default()
        })
        .await
        .unwrap();

        let reqs = captured.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0]["reasoning_effort"], "none");
        assert_eq!(reqs[1]["reasoning_effort"], "low");
        assert_eq!(reqs[2]["reasoning_effort"], "high");

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[tokio::test]
    async fn preferred_language_follows_explicit_config_then_app_settings_then_en() {
        let tmp = crate::test_tmp().join(format!("ai-lang-rt-{}", crate::platform::random_hex()));
        let paths = Paths {
            socket: tmp.join("core.sock"),
            holder_socket: tmp.join("holder.sock"),
            state: tmp.join("state"),
            config: tmp.join("cfg/herdr2/config.toml"),
        };
        paths.create_dirs().unwrap();
        let app_settings = tmp.join("cfg/herdr2-app/settings.json");

        let rt = AiRuntime::open(&paths).unwrap();
        // Default without app settings or ai.json language is "en".
        assert_eq!(rt.preferred_prompt_lang(), "en");
        assert_eq!(rt.resolve_prompt_lang(Some("zh-CN")), "zh");

        // Writing desktop app settings.json with zh-CN is picked up when ai.json language is unset.
        std::fs::create_dir_all(app_settings.parent().unwrap()).unwrap();
        std::fs::write(&app_settings, r#"{"language":"zh-CN"}"#).unwrap();
        assert_eq!(rt.preferred_language_tag(), "zh-CN");
        assert_eq!(rt.preferred_prompt_lang(), "zh");

        // Explicit ai.settings.set overrides app settings.json.
        rt.update_settings(AiSettingsSetParams { language: Some("en".into()), ..Default::default() }).await.unwrap();
        assert_eq!(rt.preferred_language_tag(), "en");
        assert_eq!(rt.preferred_prompt_lang(), "en");
        assert_eq!(rt.status().language.as_deref(), Some("en"));

        // Switching to zh-CN updates status and prompt lang.
        rt.update_settings(AiSettingsSetParams { language: Some("zh-CN".into()), ..Default::default() }).await.unwrap();
        assert_eq!(rt.preferred_language_tag(), "zh-CN");
        assert_eq!(rt.preferred_prompt_lang(), "zh");
        assert_eq!(rt.status().language.as_deref(), Some("zh-CN"));

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[tokio::test]
    async fn singleflight_redaction_graded_circuit_and_kv_cache_auto_degrade() {
        let tmp = crate::test_tmp().join(format!("ai-hardening-{}", crate::platform::random_hex()));
        let paths = Paths {
            socket: tmp.join("core.sock"),
            holder_socket: tmp.join("holder.sock"),
            state: tmp.join("state"),
            config: tmp.join("config/config.toml"),
        };
        paths.create_dirs().unwrap();

        let mut responses: Vec<(u16, String)> = Vec::new();
        // 1. Singleflight leader response (should only be hit ONCE despite 3 concurrent callers).
        responses.push((
            200,
            r#"{"choices":[{"message":{"content":"{\"headline\":\"coalesced\"}"}}],"usage":{"prompt_tokens":1600,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":0}}}"#.into(),
        ));
        // 2..=4. Three content-only errors (empty assistant content) -> MUST NOT trip global circuit!
        for _ in 0..3 {
            responses.push((
                200,
                r#"{"choices":[{"message":{"content":""},"finish_reason":"length"}],"usage":{"prompt_tokens":1600,"completion_tokens":0,"prompt_tokens_details":{"cached_tokens":0}}}"#.into(),
            ));
        }
        // 5. Next call still succeeds immediately (proving circuit stayed closed) and triggers 5th uncached large-prefix probe!
        responses.push((
            200,
            r#"{"choices":[{"message":{"content":"{\"headline\":\"still-open\"}"}}],"usage":{"prompt_tokens":1600,"completion_tokens":12,"prompt_tokens_details":{"cached_tokens":0}}}"#.into(),
        ));
        // 6. After 5 consecutive uncached large-prefix calls, stateless op auto-degrades to compact prompt!
        responses.push((
            200,
            r#"{"choices":[{"message":{"content":"{\"headline\":\"compact-now\"}"}}],"usage":{"prompt_tokens":120,"completion_tokens":12}}"#.into(),
        ));
        // 7. 429 rate limit sets short backoff (not 300s).
        responses.push((429, r#"{"error":{"message":"rate limit exceeded"}}"#.into()));

        let (url, captured) = spawn_mock_openai(responses).await;
        let rt = AiRuntime::open(&paths).unwrap();
        rt.update_settings(AiSettingsSetParams {
            enabled: Some(true),
            base_url: Some(url),
            model: Some("test-model".into()),
            ..Default::default()
        })
        .await
        .unwrap();

        // A. Singleflight + outbound secret redaction: 3 concurrent calls with identical cache_key -> 1 HTTP request.
        let req_template = || {
            AiCompleteRequest {
            op: "catchup_briefing".into(),
            tier: AiTaskTier::Nano,
            mode: AiConversationMode::StatelessSharedPrefix,
            user_prompt: "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.supersecret OPENAI_API_KEY=sk-proj-1234567890ABCDEFGHIJKLMNOP".into(),
            max_tokens: 120,
            cache_key: Some("sf:test:1".into()),
            cache_ttl: Some(Duration::from_secs(60)),
            ..Default::default()
        }
        };
        let (a, b, c) = tokio::join!(
            rt.complete_fg(req_template()),
            rt.complete_fg(req_template()),
            rt.complete_fg(req_template()),
        );
        assert_eq!(a.unwrap().content, r#"{"headline":"coalesced"}"#);
        assert_eq!(b.unwrap().content, r#"{"headline":"coalesced"}"#);
        assert_eq!(c.unwrap().content, r#"{"headline":"coalesced"}"#);
        assert_eq!(captured.lock().unwrap().len(), 1);
        let sent_user = captured.lock().unwrap()[0]["messages"][1]["content"].as_str().unwrap().to_string();
        assert!(!sent_user.contains("eyJhbGci"));
        assert!(!sent_user.contains("sk-proj-1234"));
        assert!(sent_user.contains("<REDACTED>"));

        // B. Three content-only errors do NOT trip the global circuit breaker.
        for i in 0..3 {
            let err = rt
                .complete_fg(AiCompleteRequest {
                    op: "catchup_briefing".into(),
                    tier: AiTaskTier::Nano,
                    mode: AiConversationMode::StatelessSharedPrefix,
                    user_prompt: format!("empty-{i}"),
                    max_tokens: 64,
                    ..Default::default()
                })
                .await
                .unwrap_err();
            assert!(err.contains("empty assistant content"));
            assert!(!rt.is_circuit_open(), "content-only error must not open circuit");
        }

        // C. 5th uncached large-prefix call succeeds and marks endpoint cache degraded.
        let ok5 = rt
            .complete_fg(AiCompleteRequest {
                op: "catchup_briefing".into(),
                tier: AiTaskTier::Nano,
                mode: AiConversationMode::StatelessSharedPrefix,
                user_prompt: "probe-5".into(),
                max_tokens: 64,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(ok5.content, r#"{"headline":"still-open"}"#);
        assert!(rt.status().usage.prompt_cache_auto_degraded);

        // D. 6th call automatically switches to short compact prompt (< 300 chars vs > 4000 chars).
        let ok6 = rt
            .complete_fg(AiCompleteRequest {
                op: "catchup_briefing".into(),
                tier: AiTaskTier::Nano,
                mode: AiConversationMode::StatelessSharedPrefix,
                user_prompt: "probe-6".into(),
                max_tokens: 64,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(ok6.content, r#"{"headline":"compact-now"}"#);
        {
            let reqs = captured.lock().unwrap();
            let sys_first = reqs[0]["messages"][0]["content"].as_str().unwrap();
            let sys_sixth = reqs[5]["messages"][0]["content"].as_str().unwrap();
            assert!(sys_first.len() > 4000);
            assert!(sys_sixth.len() < 400, "expected auto-degraded compact system prompt, got len {}", sys_sixth.len());
        }

        // E. 429 triggers short rate-limit backoff (< 30s), NOT 300s circuit lock.
        let err429 = rt
            .complete_fg(AiCompleteRequest {
                op: "catchup_briefing".into(),
                tier: AiTaskTier::Nano,
                mode: AiConversationMode::StatelessSharedPrefix,
                user_prompt: "probe-429".into(),
                max_tokens: 64,
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(err429.contains("429"));
        let st = rt.status();
        assert!(st.circuit_open);
        let until_ms = parse_iso_ms(st.cooldown_until.as_deref().unwrap()).unwrap();
        assert!(until_ms.saturating_sub(now_ms_u64()) <= 20_000, "429 should use short backoff, not 300s");
        assert!(st.usage.op_metrics.get("catchup_briefing").is_some_and(|m| m.calls >= 3 && m.failures >= 4));

        // F. Endpoint capability persistence survives restart.
        let rt2 = AiRuntime::open(&paths).unwrap();
        assert!(rt2.status().usage.prompt_cache_auto_degraded);

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[tokio::test]
    async fn seeded_stateless_calls_run_concurrently_without_blocking() {
        let tmp = crate::test_tmp().join(format!("ai-seeded-{}", crate::platform::random_hex()));
        let paths = Paths {
            socket: tmp.join("core.sock"),
            holder_socket: tmp.join("holder.sock"),
            state: tmp.join("state"),
            config: tmp.join("config/config.toml"),
        };
        paths.create_dirs().unwrap();

        let (url, captured) = spawn_mock_openai(vec![
            (200, r#"{"choices":[{"message":{"content":"{\"conflict\":false,\"confidence\":0.9,\"reason\":\"disjoint\"}"}}]}"#.into()),
            (200, r#"{"choices":[{"message":{"content":"{\"index\":0,\"reason\":\"best\"}"}}]}"#.into()),
        ])
        .await;
        let rt = AiRuntime::open(&paths).unwrap();
        rt.update_settings(AiSettingsSetParams {
            enabled: Some(true),
            base_url: Some(url),
            model: Some("test-model".into()),
            ..Default::default()
        })
        .await
        .unwrap();

        let (r1, r2) = tokio::join!(
            rt.complete(AiCompleteRequest {
                op: "write_conflict".into(),
                tier: AiTaskTier::Nano,
                mode: AiConversationMode::SeededStateless { seed_context: "run objective".into() },
                user_prompt: "check conflict".into(),
                max_tokens: 96,
                ..Default::default()
            }),
            rt.complete(AiCompleteRequest {
                op: "route_candidate".into(),
                tier: AiTaskTier::Nano,
                mode: AiConversationMode::SeededStateless { seed_context: "run objective".into() },
                user_prompt: "pick role".into(),
                max_tokens: 96,
                ..Default::default()
            }),
        );
        assert!(r1.is_ok() && r2.is_ok());
        let reqs = captured.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        // Both share identical 3-message prefix (system + session_seed + ack) and stay 4 messages total.
        let m0 = reqs[0]["messages"].as_array().unwrap();
        let m1 = reqs[1]["messages"].as_array().unwrap();
        assert_eq!(m0.len(), 4);
        assert_eq!(m1.len(), 4);
        assert_eq!(&m0[0..3], &m1[0..3]);

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[tokio::test]
    async fn prompt_mode_options_model_switch_reset_and_probe_redetect() {
        let tmp = crate::test_tmp().join(format!("ai-prompt-mode-{}", crate::platform::random_hex()));
        let paths = Paths {
            socket: tmp.join("core.sock"),
            holder_socket: tmp.join("holder.sock"),
            state: tmp.join("state"),
            config: tmp.join("config/config.toml"),
        };
        paths.create_dirs().unwrap();

        let (url, captured) = spawn_mock_openai(vec![
            // 1. Forced compact mode -> short system prompt (< 400 chars)
            (200, r#"{"choices":[{"message":{"content":"{\"headline\":\"c1\"}"}}],"usage":{"prompt_tokens":120,"completion_tokens":10}}"#.into()),
            // 2-7. Forced shared_prefix mode: 6 consecutive 0-cache calls must NOT auto-degrade
            (200, r#"{"choices":[{"message":{"content":"{\"headline\":\"s1\"}"}}],"usage":{"prompt_tokens":1800,"completion_tokens":10,"prompt_tokens_details":{"cached_tokens":0}}}"#.into()),
            (200, r#"{"choices":[{"message":{"content":"{\"headline\":\"s2\"}"}}],"usage":{"prompt_tokens":1800,"completion_tokens":10,"prompt_tokens_details":{"cached_tokens":0}}}"#.into()),
            (200, r#"{"choices":[{"message":{"content":"{\"headline\":\"s3\"}"}}],"usage":{"prompt_tokens":1800,"completion_tokens":10,"prompt_tokens_details":{"cached_tokens":0}}}"#.into()),
            (200, r#"{"choices":[{"message":{"content":"{\"headline\":\"s4\"}"}}],"usage":{"prompt_tokens":1800,"completion_tokens":10,"prompt_tokens_details":{"cached_tokens":0}}}"#.into()),
            (200, r#"{"choices":[{"message":{"content":"{\"headline\":\"s5\"}"}}],"usage":{"prompt_tokens":1800,"completion_tokens":10,"prompt_tokens_details":{"cached_tokens":0}}}"#.into()),
            (200, r#"{"choices":[{"message":{"content":"{\"headline\":\"s6\"}"}}],"usage":{"prompt_tokens":1800,"completion_tokens":10,"prompt_tokens_details":{"cached_tokens":0}}}"#.into()),
            // 8-10. probe_prompt_mode #1: 3 consecutive 0-cache probes -> detects "compact"
            (200, r#"{"choices":[{"message":{"content":"{\"ack\":true}"}}],"usage":{"prompt_tokens":1700,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":0}}}"#.into()),
            (200, r#"{"choices":[{"message":{"content":"{\"ack\":true}"}}],"usage":{"prompt_tokens":1700,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":0}}}"#.into()),
            (200, r#"{"choices":[{"message":{"content":"{\"ack\":true}"}}],"usage":{"prompt_tokens":1700,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":0}}}"#.into()),
            // 11-12. probe_prompt_mode #2: probe 1 warms, probe 2 hits KV cache -> detects "shared_prefix"
            (200, r#"{"choices":[{"message":{"content":"{\"ack\":true}"}}],"usage":{"prompt_tokens":1700,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":0}}}"#.into()),
            (200, r#"{"choices":[{"message":{"content":"{\"ack\":true}"}}],"usage":{"prompt_tokens":1700,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":1400}}}"#.into()),
        ])
        .await;

        let rt = AiRuntime::open(&paths).unwrap();
        // A. Set explicit "compact" mode
        let st = rt
            .update_settings(AiSettingsSetParams {
                enabled: Some(true),
                base_url: Some(url),
                model: Some("model-a".into()),
                prompt_mode: Some("compact".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(st.switches.prompt_mode, "compact");
        assert!(!st.switches.prompt_cache_enabled);
        assert_eq!(st.usage.effective_prompt_mode.as_deref(), Some("compact"));

        rt.complete_fg(AiCompleteRequest {
            op: "catchup_briefing".into(),
            tier: AiTaskTier::Nano,
            mode: AiConversationMode::StatelessSharedPrefix,
            user_prompt: "compact-check".into(),
            max_tokens: 64,
            ..Default::default()
        })
        .await
        .unwrap();
        {
            let reqs = captured.lock().unwrap();
            assert!(reqs[0]["messages"][0]["content"].as_str().unwrap().len() < 400);
            assert!(reqs[0].get("prompt_cache_key").is_none());
        }

        // B. Switch to explicit "shared_prefix" -> stays long prefix even after 6 zero-cache calls
        let st = rt
            .update_settings(AiSettingsSetParams {
                prompt_mode: Some("shared_prefix".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(st.switches.prompt_mode, "shared_prefix");
        assert!(st.switches.prompt_cache_enabled);
        for i in 0..6 {
            rt.complete_fg(AiCompleteRequest {
                op: "catchup_briefing".into(),
                tier: AiTaskTier::Nano,
                mode: AiConversationMode::StatelessSharedPrefix,
                user_prompt: format!("shared-{i}"),
                max_tokens: 64,
                ..Default::default()
            })
            .await
            .unwrap();
        }
        {
            let reqs = captured.lock().unwrap();
            assert!(reqs[6]["messages"][0]["content"].as_str().unwrap().len() > 4000);
        }
        assert!(!rt.status().usage.prompt_cache_auto_degraded);

        // C. Switching model automatically resets prompt_mode to "auto" and clears detection records
        let st_switched = rt
            .update_settings(AiSettingsSetParams {
                model: Some("model-b".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(st_switched.model, "model-b");
        assert_eq!(st_switched.switches.prompt_mode, "auto");
        assert!(st_switched.switches.prompt_cache_enabled);
        assert_eq!(st_switched.usage.prompt_cache_detected_mode, None);
        assert!(!st_switched.usage.prompt_cache_auto_degraded);

        // D. Active probe detects "compact" when 3 consecutive probes get 0 cached tokens
        let probe1 = rt.probe_prompt_mode().await.unwrap();
        assert!(probe1.ok);
        assert_eq!(probe1.detected_mode, "compact");
        assert_eq!(probe1.probes_run, 3);
        assert!(probe1.status.usage.prompt_cache_auto_degraded);
        assert_eq!(probe1.status.usage.prompt_cache_detected_mode.as_deref(), Some("compact"));
        assert_eq!(probe1.status.usage.effective_prompt_mode.as_deref(), Some("compact"));

        // E. Re-detect clears prior "compact" detection and detects "shared_prefix" when cache hits
        let probe2 = rt.probe_prompt_mode().await.unwrap();
        assert!(probe2.ok);
        assert_eq!(probe2.detected_mode, "shared_prefix");
        assert_eq!(probe2.probes_run, 2);
        assert_eq!(probe2.cached_input_tokens, 1400);
        assert!(!probe2.status.usage.prompt_cache_auto_degraded);
        assert_eq!(probe2.status.usage.prompt_cache_detected_mode.as_deref(), Some("shared_prefix"));
        assert_eq!(probe2.status.usage.effective_prompt_mode.as_deref(), Some("shared_prefix"));

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[tokio::test]
    async fn list_models_works_keyless_and_does_not_leak_other_provider_key() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let saw_auth = Arc::new(Mutex::new(Vec::<bool>::new()));
        let saw_auth_srv = saw_auth.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let Ok(mut conn) = stream else { break };
                let mut buf = [0u8; 4096];
                let n = conn.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                saw_auth_srv.lock().unwrap().push(req.contains("authorization: bearer"));
                let body = r#"{"data":[{"id":"magpie-a"},{"id":"magpie-b"}]}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = conn.write_all(resp.as_bytes());
            }
        });

        let tmp = crate::test_tmp().join(format!("ai-models-keyless-{}", crate::platform::random_hex()));
        let paths = Paths {
            socket: tmp.join("core.sock"),
            holder_socket: tmp.join("holder.sock"),
            state: tmp.join("state"),
            config: tmp.join("config/config.toml"),
        };
        let rt = AiRuntime::open(&paths).unwrap();

        // Save DeepSeek with a key first.
        rt.update_settings(AiSettingsSetParams {
            enabled: Some(true),
            base_url: Some("https://api.deepseek.com/v1".into()),
            api_key: Some("sk-deepseek-secret".into()),
            ..Default::default()
        })
        .await
        .unwrap();

        // Querying a different local Magpie URL without apiKey must succeed and NOT send DeepSeek's key.
        let local_url = format!("http://127.0.0.1:{port}/v1");
        let res = rt.list_models(Some(local_url.clone()), None).await.unwrap();
        assert!(res.ok, "expected ok: {res:?}");
        assert_eq!(res.models, vec!["magpie-a", "magpie-b"]);
        assert_eq!(*saw_auth.lock().unwrap(), vec![false]);

        // Strict cloud hosts still require a key, whereas custom/local gateways allow keyless operation.
        assert!(!allows_keyless_url("https://api.deepseek.com/v1"));
        assert!(!allows_keyless_url("https://api.openai.com/v1"));
        assert!(allows_keyless_url(&local_url));
        assert!(allows_keyless_url("http://192.168.1.50:8080/v1"));

        // Saving custom keyless gateway clears old key and marks configured=true.
        let st = rt
            .update_settings(AiSettingsSetParams {
                enabled: Some(true),
                base_url: Some(local_url),
                model: Some("magpie-b".into()),
                clear_key: Some(true),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(st.configured);
        assert_eq!(st.model, "magpie-b");
        assert_eq!(st.key_masked, None);

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[tokio::test]
    async fn replication_params_and_idempotent_update_preserve_caches() {
        let tmp = crate::test_tmp().join(format!("ai-repl-{}", crate::platform::random_hex()));
        let paths = Paths {
            socket: tmp.join("core.sock"),
            holder_socket: tmp.join("holder.sock"),
            state: tmp.join("state"),
            config: tmp.join("config/config.toml"),
        };
        let rt = AiRuntime::open(&paths).unwrap();

        // 1. Fresh unconfigured runtime emits no connect sync payload (does not wipe legacy remote).
        assert!(rt.sync_params_for_connect().is_none());

        // 2. Language-only update on unconfigured local only replicates language, not empty endpoint/clearKey.
        let lang_patch = AiSettingsSetParams { language: Some("zh-CN".into()), ..Default::default() };
        rt.update_settings(lang_patch.clone()).await.unwrap();
        let after_lang = rt.sync_params_after_update(&lang_patch);
        assert_eq!(after_lang.language.as_deref(), Some("zh-CN"));
        assert!(after_lang.base_url.is_none());
        assert!(after_lang.clear_key.is_none());
        let connect_lang = rt.sync_params_for_connect().expect("language-only connect sync");
        assert_eq!(connect_lang.language.as_deref(), Some("zh-CN"));
        assert!(connect_lang.base_url.is_none());
        assert!(connect_lang.clear_key.is_none());

        // 3. Full configuration replicates every field including API key.
        let full_patch = AiSettingsSetParams {
            enabled: Some(true),
            base_url: Some("https://api.example.com/v1".into()),
            model: Some("deepseek-chat".into()),
            effort: Some("medium".into()),
            context_window: Some("64k".into()),
            max_tokens: Some(2048),
            api_key: Some("sk-shared-secret-1234".into()),
            pty_status_arbitration: Some(false),
            prompt_mode: Some("compact".into()),
            ..Default::default()
        };
        rt.update_settings(full_patch.clone()).await.unwrap();
        rt.update_models("https://api.example.com/v1", vec!["deepseek-chat".into(), "deepseek-reasoner".into()]);

        let repl = rt.sync_params_for_connect().expect("full connect sync");
        assert_eq!(repl.base_url.as_deref(), Some("https://api.example.com/v1"));
        assert_eq!(repl.model.as_deref(), Some("deepseek-chat"));
        assert_eq!(repl.effort.as_deref(), Some("medium"));
        assert_eq!(repl.context_window.as_deref(), Some("64k"));
        assert_eq!(repl.max_tokens, Some(2048));
        assert_eq!(repl.language.as_deref(), Some("zh-CN"));
        assert_eq!(repl.api_key.as_deref(), Some("sk-shared-secret-1234"));
        assert_eq!(repl.clear_key, None);
        assert_eq!(repl.pty_status_arbitration, Some(false));
        assert_eq!(repl.prompt_mode.as_deref(), Some("compact"));

        // 4. Applying the exact same replication payload again is idempotent (preserves cached models).
        rt.update_settings(repl).await.unwrap();
        assert_eq!(rt.cached_models(), vec!["deepseek-chat".to_string(), "deepseek-reasoner".to_string()]);

        // 5. Clearing key propagates clearKey=true.
        let clear_patch = AiSettingsSetParams { clear_key: Some(true), ..Default::default() };
        rt.update_settings(clear_patch.clone()).await.unwrap();
        let repl_after_clear = rt.sync_params_after_update(&clear_patch);
        assert_eq!(repl_after_clear.api_key, None);
        assert_eq!(repl_after_clear.clear_key, Some(true));

        let _ = std::fs::remove_dir_all(tmp);
    }

    async fn spawn_mock_anthropic(
        require_bearer: bool,
        responses: Vec<(u16, String)>,
    ) -> (String, Arc<Mutex<Vec<(String, String, serde_json::Value)>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let cap_clone = captured.clone();
        let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(responses)));

        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let cap = cap_clone.clone();
                let q = queue.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    let mut header_end = None;
                    let mut content_len = 0usize;
                    while let Ok(n) = sock.read(&mut tmp).await {
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if header_end.is_none()
                            && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                        {
                            header_end = Some(pos + 4);
                            let head = String::from_utf8_lossy(&buf[..pos]);
                            for line in head.lines() {
                                if let Some((k, v)) = line.split_once(':')
                                    && k.trim().eq_ignore_ascii_case("content-length")
                                {
                                    content_len = v.trim().parse().unwrap_or(0);
                                }
                            }
                        }
                        if let Some(he) = header_end
                            && buf.len() >= he + content_len
                        {
                            break;
                        }
                    }
                    let head = String::from_utf8_lossy(&buf[..header_end.unwrap_or(buf.len())]).into_owned();
                    let head_lower = head.to_ascii_lowercase();
                    let req_line = head.lines().next().unwrap_or("").to_string();
                    let has_version = head_lower.contains("anthropic-version: 2023-06-01");
                    let has_x_api_key = head_lower.contains("x-api-key: sk-claude-secret");
                    let has_bearer = head_lower.contains("authorization: bearer sk-claude-secret");
                    let auth_ok = if require_bearer { has_bearer } else { has_x_api_key };

                    let write_resp = |status: u16, body: &str| {
                        let status_text = match status {
                            200 => "OK",
                            401 => "Unauthorized",
                            404 => "Not Found",
                            _ => "Bad Request",
                        };
                        format!(
                            "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    };

                    if req_line.contains("/chat/completions") {
                        let http = write_resp(404, r#"{"error":{"message":"not an openai endpoint"}}"#);
                        let _ = sock.write_all(http.as_bytes()).await;
                        return;
                    }
                    if !has_version || !auth_ok {
                        let http = write_resp(401, r#"{"type":"error","error":{"message":"missing anthropic auth"}}"#);
                        let _ = sock.write_all(http.as_bytes()).await;
                        return;
                    }
                    if req_line.starts_with("GET ") && req_line.contains("/models") {
                        let body = r#"{"data":[{"id":"claude-sonnet-4-6","type":"model"},{"id":"claude-haiku-4-5","type":"model"}]}"#;
                        let http = write_resp(200, body);
                        let _ = sock.write_all(http.as_bytes()).await;
                        return;
                    }
                    if req_line.starts_with("POST ") && req_line.contains("/messages") {
                        let body_val = header_end
                            .and_then(|he| {
                                serde_json::from_slice::<serde_json::Value>(&buf[he..buf.len().min(he + content_len)])
                                    .ok()
                            })
                            .unwrap_or(Value::Null);
                        cap.lock().unwrap().push((req_line, head, body_val));
                        let (status, resp_body) = q.lock().unwrap().pop_front().unwrap_or_else(|| {
                            (
                                200,
                                r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-6","content":[{"type":"text","text":"{\"ok\":true}"}],"stop_reason":"end_turn","usage":{"input_tokens":20,"cache_creation_input_tokens":1400,"cache_read_input_tokens":0,"output_tokens":12}}"#
                                    .into(),
                            )
                        });
                        let http = write_resp(status, &resp_body);
                        let _ = sock.write_all(http.as_bytes()).await;
                        return;
                    }
                    let http = write_resp(404, r#"{"error":{"message":"unknown path"}}"#);
                    let _ = sock.write_all(http.as_bytes()).await;
                });
            }
        });
        // Intentionally neutral URL with no "anthropic" or "claude" hint to prove full auto-detection.
        (format!("http://127.0.0.1:{}/v1", addr.port()), captured)
    }

    #[tokio::test]
    async fn auto_detects_claude_protocol_persists_memory_and_tracks_cache_usage() {
        let tmp = crate::test_tmp().join(format!("ai-claude-auto-{}", crate::platform::random_hex()));
        let paths = Paths {
            socket: tmp.join("core.sock"),
            holder_socket: tmp.join("holder.sock"),
            state: tmp.join("state"),
            config: tmp.join("config/config.toml"),
        };
        paths.create_dirs().unwrap();

        let (url, captured) = spawn_mock_anthropic(
            false,
            vec![
                // 1. Test connection probe
                (
                    200,
                    r#"{"id":"msg_test","type":"message","role":"assistant","model":"claude-sonnet-4-6","content":[{"type":"text","text":"OK"}],"stop_reason":"end_turn","usage":{"input_tokens":8,"output_tokens":2}}"#
                        .into(),
                ),
                // 2. First real completion (writes cache)
                (
                    200,
                    r#"{"id":"msg_2","type":"message","role":"assistant","model":"claude-sonnet-4-6","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"{\"headline\":\"all healthy\"}"}],"stop_reason":"end_turn","usage":{"input_tokens":40,"cache_creation_input_tokens":1500,"cache_read_input_tokens":0,"output_tokens":16}}"#
                        .into(),
                ),
                // 3. Second real completion (hits cache)
                (
                    200,
                    r#"{"id":"msg_3","type":"message","role":"assistant","model":"claude-sonnet-4-6","content":[{"type":"text","text":"{\"headline\":\"cached hit\"}"}],"stop_reason":"end_turn","usage":{"input_tokens":40,"cache_creation_input_tokens":0,"cache_read_input_tokens":1500,"output_tokens":14}}"#
                        .into(),
                ),
            ],
        )
        .await;

        let rt = AiRuntime::open(&paths).unwrap();
        let saved = rt
            .update_settings(AiSettingsSetParams {
                enabled: Some(true),
                base_url: Some(url.clone()),
                api_key: Some("sk-claude-secret".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(saved.available_models, vec!["claude-sonnet-4-6", "claude-haiku-4-5"]);
        assert_eq!(saved.protocol, Some(AiProtocol::Anthropic));

        // Explicit test_connection verifies Claude protocol and reports it.
        let test_res = rt.test_connection(None, None, None).await.unwrap();
        assert!(test_res.ok, "test failed: {test_res:?}");
        assert_eq!(test_res.protocol, Some(AiProtocol::Anthropic));
        assert_eq!(test_res.discovered_model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(rt.status().protocol, Some(AiProtocol::Anthropic));

        // Completions go straight to POST /v1/messages with top-level system + cache_control.
        let c1 = rt
            .complete_fg(AiCompleteRequest {
                op: "catchup_briefing".into(),
                tier: AiTaskTier::Nano,
                mode: AiConversationMode::StatelessSharedPrefix,
                user_prompt: "brief 1".into(),
                max_tokens: 128,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(c1.content, r#"{"headline":"all healthy"}"#);
        assert_eq!(c1.usage.input_tokens, 1540);
        assert_eq!(c1.usage.cached_input_tokens, 0);

        let c2 = rt
            .complete_fg(AiCompleteRequest {
                op: "catchup_briefing".into(),
                tier: AiTaskTier::Nano,
                mode: AiConversationMode::StatelessSharedPrefix,
                user_prompt: "brief 2".into(),
                max_tokens: 128,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(c2.content, r#"{"headline":"cached hit"}"#);
        assert_eq!(c2.usage.input_tokens, 1540);
        assert_eq!(c2.usage.cached_input_tokens, 1500);

        let st = rt.status();
        assert_eq!(st.protocol, Some(AiProtocol::Anthropic));
        assert_eq!(st.usage.prompt_cache_detected_mode.as_deref(), Some("shared_prefix"));
        assert!(!st.circuit_open);
        assert_eq!(st.usage.op_metrics.get("catchup_briefing").map(|m| m.failures), Some(0));

        let reqs = captured.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        assert!(reqs[1].0.contains("/v1/messages"));
        assert_eq!(reqs[1].2["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(reqs[1].2.get("prompt_cache_key").is_none());

        // Persisted protocol survives restart.
        let rt2 = AiRuntime::open(&paths).unwrap();
        assert_eq!(rt2.status().protocol, Some(AiProtocol::Anthropic));

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[tokio::test]
    async fn claude_complete_auto_falls_back_from_openai_and_supports_bearer_proxies_and_cache_control_fallback() {
        let tmp = crate::test_tmp().join(format!("ai-claude-bearer-{}", crate::platform::random_hex()));
        let paths = Paths {
            socket: tmp.join("core.sock"),
            holder_socket: tmp.join("holder.sock"),
            state: tmp.join("state"),
            config: tmp.join("config/config.toml"),
        };
        paths.create_dirs().unwrap();

        let (url, captured) = spawn_mock_anthropic(
            true, // Proxy requires Authorization: Bearer + anthropic-version
            vec![
                // Reject cache_control on first attempt -> runtime falls back to plain string system and remembers.
                (400, r#"{"type":"error","error":{"message":"unsupported field cache_control"}}"#.into()),
                (
                    200,
                    r#"{"id":"msg_b1","type":"message","role":"assistant","model":"claude-sonnet-4-6","content":[{"type":"text","text":"{\"ok\":true}"}],"stop_reason":"end_turn","usage":{"input_tokens":80,"output_tokens":10}}"#
                        .into(),
                ),
                (
                    200,
                    r#"{"id":"msg_b2","type":"message","role":"assistant","model":"claude-sonnet-4-6","content":[{"type":"text","text":"{\"ok\":true}"}],"stop_reason":"end_turn","usage":{"input_tokens":80,"output_tokens":10}}"#
                        .into(),
                ),
            ],
        )
        .await;

        let rt = AiRuntime::open(&paths).unwrap();
        rt.update_settings(AiSettingsSetParams {
            enabled: Some(true),
            base_url: Some(url),
            model: Some("claude-sonnet-4-6".into()),
            api_key: Some("sk-claude-secret".into()),
            ..Default::default()
        })
        .await
        .unwrap();

        let r1 = rt
            .complete_fg(AiCompleteRequest {
                op: "custom".into(),
                tier: AiTaskTier::Fast,
                mode: AiConversationMode::StatelessSharedPrefix,
                user_prompt: "ping 1".into(),
                max_tokens: 64,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(r1.content, r#"{"ok":true}"#);
        assert_eq!(rt.status().protocol, Some(AiProtocol::Anthropic));

        // Second call remembers both Anthropic protocol, Bearer auth, and cache_control=false!
        let r2 = rt
            .complete_fg(AiCompleteRequest {
                op: "custom".into(),
                tier: AiTaskTier::Fast,
                mode: AiConversationMode::StatelessSharedPrefix,
                user_prompt: "ping 2".into(),
                max_tokens: 64,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(r2.content, r#"{"ok":true}"#);

        let reqs = captured.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        assert!(reqs[0].2["system"].is_array());
        assert!(reqs[1].2["system"].is_string());
        assert!(reqs[2].2["system"].is_string());
        assert!(reqs[2].1.to_ascii_lowercase().contains("authorization: bearer sk-claude-secret"));

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[tokio::test]
    async fn classify_acp_turn_state_arbitrates_subtle_questions_and_caches() {
        let tmp = std::env::temp_dir().join(format!("herdr2-ai-acp-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let paths = Paths {
            socket: tmp.join("core.sock"),
            holder_socket: tmp.join("holder.sock"),
            state: tmp.join("state"),
            config: tmp.join("config/config.toml"),
        };
        paths.create_dirs().unwrap();

        let (url, _captured) = spawn_mock_openai(vec![
            (
                200,
                r#"{"choices":[{"message":{"content":"{\"state\":\"blocked\",\"kind\":\"question\",\"confidence\":0.92,\"summary\":\"等待用户拍板方案\"}"}}]}"#.into(),
            ),
            (
                200,
                r#"{"choices":[{"message":{"content":"{\"state\":\"done\",\"kind\":null,\"confidence\":0.95,\"summary\":null}"}}]}"#.into(),
            ),
        ])
        .await;

        let rt = AiRuntime::open(&paths).unwrap();
        rt.update_settings(AiSettingsSetParams {
            enabled: Some(true),
            base_url: Some(url),
            model: Some("test-nano".into()),
            ..Default::default()
        })
        .await
        .unwrap();

        // Subtle Chinese prompt without '?' -> AI identifies blocked(question).
        let r1 = classify_acp_turn_state(
            &rt,
            "claude",
            "目前有方案一和方案二，各自利弊如上，先等你拍板后再开始改代码。",
            Some("zh"),
        )
        .await;
        assert!(r1.used_ai && r1.accepted && !r1.from_cache);
        assert_eq!(r1.state.as_deref(), Some("blocked"));
        assert_eq!(r1.kind.as_deref(), Some("question"));

        // Repeated identical message hits local 0-token cache.
        let r1_cached = classify_acp_turn_state(
            &rt,
            "claude",
            "目前有方案一和方案二，各自利弊如上，先等你拍板后再开始改代码。",
            Some("zh"),
        )
        .await;
        assert!(r1_cached.used_ai && r1_cached.accepted && r1_cached.from_cache);

        // Rhetorical question in a completed summary -> AI corrects to done.
        let r2 = classify_acp_turn_state(&rt, "claude", "为什么之前会超时？因为锁未释放，现已彻底修复并通过测试。", Some("zh")).await;
        assert!(r2.used_ai && r2.accepted && !r2.from_cache);
        assert_eq!(r2.state.as_deref(), Some("done"));

        let _ = std::fs::remove_dir_all(tmp);
    }
}
