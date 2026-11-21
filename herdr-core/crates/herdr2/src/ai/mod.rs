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
pub use client::AiTestResult;
pub use config::{
    AiCoreConfig, AiPaths, format_ms_iso, is_loopback_http_url, mask_key, now_iso, now_ms_u64,
    parse_ai_token_size, parse_iso_ms, validate_http_url, write_secret_file,
};
pub use digest::{
    AgentDigestResult, CommitMessageParams, CommitMessageResult, DeterministicDigest, SummarizeAgentParams,
    build_deterministic_acp_digest, generate_commit_message, refine_agent_digest,
};
pub use handoff_ai::{
    HandoffRecommendParams, HandoffRecommendResult, distill_merge_back, distill_omitted_turns,
    recommend_handoff,
};
pub use terminal_state::{ClassifyTerminalStateParams, ClassifyTerminalStateResult, classify_terminal_state};

use crate::paths::Paths;
use crate::rpc::RpcError;
use cache::AiResultCache;
use client::{
    EndpointCapabilities, build_chat_payload_candidates, chat_endpoints, curl_json_timeout,
    effective_max_tokens, extract_json_object, fetch_ai_models, first_model_id, models_endpoints,
    parse_usage_tokens, record_winning_variant, refine_capabilities_from_error,
    should_retry_without_optional_fields, test_ai_connection, truncate_for_context,
};
use compact::{fingerprint_hex, fnv1a_64, strip_and_dedup_lines};
use config::{
    PersistedAiModelsCache, clear_ai_key, load_config, load_models_cache, read_ai_key, save_config,
    save_models_cache, write_ai_key,
};
use prompts::{HERDR2_CONTROL_PLANE_SYSTEM, compact_system_for_op, format_op_user_message};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use session::{AiSessionPool, ChatMessage, MAX_SESSION_TOKENS};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const COOLDOWN_SECS: u64 = 300;
const CIRCUIT_THRESHOLD: u32 = 3;
const BG_CONCURRENCY_LIMIT: usize = 3;

#[derive(Default, Clone, Debug)]
pub struct AiCircuitState {
    pub consecutive_failures: u32,
    pub circuit_open: bool,
    pub cooldown_until: Option<String>,
    pub last_error: Option<String>,
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
    /// Appends onto an existing per-scope KV-cached conversation (`drove:run:<id>`, `drove:loop:<id>`).
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_op: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiCoreSwitches {
    pub pty_status_arbitration: bool,
    pub notes_digest: bool,
    pub prompt_cache_enabled: bool,
    pub handoff_distill: bool,
    pub checkpoint_labeling: bool,
    pub semantic_summary: bool,
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
    pub model: String,
    pub effort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
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
    pub handoff_distill: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_labeling: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_summary: Option<bool>,
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
    endpoint_caps: Mutex<HashMap<String, EndpointCapabilities>>,
    sessions: Arc<AiSessionPool>,
    result_cache: Arc<AiResultCache>,
    usage: Mutex<AiUsageStats>,
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

        Ok(Arc::new(Self {
            paths: ai_paths,
            config: Mutex::new(cfg),
            models_cache: Mutex::new(models_cache),
            models_url: Mutex::new(models_url),
            circuit: Mutex::new(AiCircuitState::default()),
            endpoint_caps: Mutex::new(HashMap::new()),
            sessions: Arc::new(AiSessionPool::default()),
            result_cache: Arc::new(AiResultCache::default()),
            usage: Mutex::new(AiUsageStats::default()),
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
        key.is_some() || is_loopback_http_url(&cfg.base_url)
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
        let mut cir = self.circuit.lock().unwrap_or_else(|p| p.into_inner());
        if !cir.circuit_open {
            return false;
        }
        if let Some(until) = &cir.cooldown_until
            && let Some(until_ms) = parse_iso_ms(until)
            && now_ms_u64() >= until_ms
        {
            cir.circuit_open = false;
            cir.consecutive_failures = 0;
            cir.cooldown_until = None;
            return false;
        }
        true
    }

    pub fn record_success(&self) {
        let mut cir = self.circuit.lock().unwrap_or_else(|p| p.into_inner());
        cir.consecutive_failures = 0;
        cir.circuit_open = false;
        cir.cooldown_until = None;
        cir.last_error = None;
    }

    pub fn record_failure(&self, err: &str) {
        let mut cir = self.circuit.lock().unwrap_or_else(|p| p.into_inner());
        cir.consecutive_failures += 1;
        cir.last_error = Some(err.to_string());
        if cir.consecutive_failures >= CIRCUIT_THRESHOLD {
            cir.circuit_open = true;
            cir.cooldown_until = Some(format_ms_iso(now_ms_u64() + COOLDOWN_SECS * 1000));
        }
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
            &PersistedAiModelsCache {
                updated_at: now_iso(),
                base_url: base_url.trim().to_string(),
                models: deduped,
            },
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

    pub fn status(&self) -> AiStatus {
        let cfg = self.config_snapshot();
        let (key, src) = read_ai_key(&self.paths);
        let cir = self.circuit.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let has_key = key.is_some();
        let is_local = is_loopback_http_url(&cfg.base_url);
        let configured = cfg.enabled && !cfg.base_url.trim().is_empty() && (has_key || is_local);
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
        AiStatus {
            enabled: cfg.enabled,
            configured,
            key_source,
            key_file_path: self.paths.ai_key_file.display().to_string(),
            config_path: self.paths.config_file.display().to_string(),
            key_masked: key.as_deref().map(mask_key),
            base_url: cfg.base_url,
            model: cfg.model,
            effort: cfg.effort,
            context_window: cfg.context_window,
            max_tokens: cfg.max_tokens,
            available_models: self.cached_models(),
            circuit_open: cir.circuit_open,
            cooldown_until: cir.cooldown_until,
            consecutive_failures: cir.consecutive_failures,
            last_error: cir.last_error,
            switches: AiCoreSwitches {
                pty_status_arbitration: cfg.pty_status_arbitration,
                notes_digest: cfg.notes_digest,
                prompt_cache_enabled: cfg.prompt_cache_enabled,
                handoff_distill: cfg.handoff_distill,
                checkpoint_labeling: cfg.checkpoint_labeling,
                semantic_summary: cfg.semantic_summary,
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
                let discovered = fetch_ai_models(cfg_url, key.as_deref()).await;
                if !discovered.is_empty() {
                    self.update_models(cfg_url, discovered);
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
            next.context_window = if clean.is_empty() || clean == "auto" || clean == "default" {
                None
            } else {
                Some(clean)
            };
        }
        if let Some(n) = params.max_tokens {
            next.max_tokens = if n == 0 { None } else { Some(n) };
        }
        if let Some(b) = params.pty_status_arbitration {
            next.pty_status_arbitration = b;
        }
        if let Some(b) = params.notes_digest {
            next.notes_digest = b;
        }
        if let Some(b) = params.prompt_cache_enabled {
            next.prompt_cache_enabled = b;
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

        let mut key_changed = false;
        if params.clear_key == Some(true) {
            clear_ai_key(&self.paths).map_err(|e| RpcError::internal(format!("clear key: {e}")))?;
            key_changed = true;
        } else if let Some(key) = params.api_key.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
            write_ai_key(&self.paths, &key)?;
            key_changed = true;
        }

        save_config(&self.paths, &mut next)?;
        let url_changed = next.base_url.trim() != prev.base_url.trim();
        let model_changed = next.model.trim() != prev.model.trim();
        *self.config.lock().unwrap_or_else(|p| p.into_inner()) = next.clone();

        if url_changed || key_changed {
            self.models_cache.lock().unwrap_or_else(|p| p.into_inner()).clear();
            self.models_url.lock().unwrap_or_else(|p| p.into_inner()).clear();
            self.endpoint_caps.lock().unwrap_or_else(|p| p.into_inner()).clear();
            save_models_cache(&self.paths, &PersistedAiModelsCache::default());
            self.record_success();
        }
        if url_changed || model_changed || key_changed || next.prompt_cache_enabled != prev.prompt_cache_enabled {
            self.sessions.clear_all();
            self.result_cache.clear();
        }
        Ok(self.status_refresh_models().await)
    }

    pub async fn test_connection(
        &self,
        url_override: Option<String>,
        key_override: Option<String>,
        model_override: Option<String>,
    ) -> Result<AiTestResult, RpcError> {
        let cfg = self.config_snapshot();
        let base_url = url_override
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(cfg.base_url);
        if base_url.trim().is_empty() {
            return Err(RpcError::invalid_params("AI Service URL is empty"));
        }
        let (stored_key, _) = read_ai_key(&self.paths);
        let key = key_override.filter(|s| !s.trim().is_empty()).or(stored_key);
        let model = model_override
            .filter(|s| !s.trim().is_empty())
            .or_else(|| (!cfg.model.trim().is_empty()).then_some(cfg.model));
        let res = test_ai_connection(&base_url, key.as_deref(), model.as_deref()).await;
        let mut merged = Vec::new();
        if let Some(m) = &res.discovered_model {
            merged.push(m.clone());
        }
        merged.extend(res.available_models.clone());
        if !merged.is_empty() {
            self.update_models(&base_url, merged);
        }
        {
            let mut cir = self.circuit.lock().unwrap_or_else(|p| p.into_inner());
            if res.ok {
                cir.consecutive_failures = 0;
                cir.circuit_open = false;
                cir.cooldown_until = None;
                cir.last_error = None;
            } else {
                cir.last_error = res.error.clone();
            }
        }
        Ok(res)
    }

    /// Non-blocking background completion guarded by a bounded concurrency semaphore.
    /// Drops immediately (returns `None`) if background slots are saturated or circuit is open.
    pub async fn complete_bg(&self, req: AiCompleteRequest) -> Option<AiCompleteResponse> {
        // Check local cache BEFORE acquiring semaphore so 0-token hits never contend.
        if let Some(ck) = req.cache_key.as_deref()
            && let Some(cached) = self.result_cache.get(ck)
        {
            self.record_local_cache_hit(&req.op);
            return Some(AiCompleteResponse {
                content: cached,
                model: self.config_snapshot().model,
                latency_ms: 0,
                usage: AiUsage::default(),
                from_local_cache: true,
                session_reused: false,
            });
        }
        let Ok(_permit) = self.bg_sem.clone().try_acquire_owned() else {
            return None;
        };
        self.complete(req).await.ok()
    }

    /// Executes an OpenAI-compatible chat completion with:
    /// - Local zero-token LRU cache
    /// - Shared byte-immutable system prefix (>1024 tokens) or append-only `AiSessionPool`
    /// - Endpoint capability memory & `prompt_cache_key`
    /// - Circuit breaker and cache-hit accounting
    pub async fn complete(&self, req: AiCompleteRequest) -> Result<AiCompleteResponse, String> {
        let started = Instant::now();
        if let Some(ck) = req.cache_key.as_deref()
            && let Some(cached) = self.result_cache.get(ck)
        {
            self.record_local_cache_hit(&req.op);
            return Ok(AiCompleteResponse {
                content: cached,
                model: self.config_snapshot().model,
                latency_ms: 0,
                usage: AiUsage::default(),
                from_local_cache: true,
                session_reused: false,
            });
        }

        let cfg = self.config_snapshot();
        let has_override = req.url_override.as_deref().is_some_and(|s| !s.trim().is_empty());
        if !has_override && !cfg.enabled {
            return Err("AI assistant is disabled".into());
        }
        if !has_override && self.is_circuit_open() {
            return Err("AI circuit breaker is open".into());
        }

        let raw_base = req
            .url_override
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(cfg.base_url.trim());
        if raw_base.is_empty() {
            return Err("AI Service URL is not configured".into());
        }
        let base = validate_http_url(raw_base, "ai.baseUrl").map_err(|e| e.message)?;

        let (stored_key, _) = read_ai_key(&self.paths);
        let key = req
            .key_override
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or(stored_key);
        if key.is_none() && !is_loopback_http_url(&base) {
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
        if model.is_empty() {
            for url in models_endpoints(&base) {
                if let Ok((st, body)) = curl_json_timeout("GET", &url, key.as_deref(), None, client::AI_MODELS_TIMEOUT).await
                    && (200..300).contains(&st)
                    && let Some(m) = first_model_id(&body)
                {
                    model = m;
                    break;
                }
            }
        }
        if model.is_empty() {
            self.record_failure("no model configured or discovered");
            return Err("no model configured or discovered".into());
        }

        let ctx_tokens = cfg.context_window.as_deref().and_then(parse_ai_token_size);
        let small_ctx = ctx_tokens.is_some_and(|c| c <= 4096);
        let use_shared_prefix = cfg.prompt_cache_enabled && !small_ctx;

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
        let eff_max_tokens = effective_max_tokens(
            req.max_tokens.max(32),
            cfg.max_tokens,
            effective_effort,
            ctx_tokens,
            cap_strict,
        );
        let temperature = match req.tier {
            AiTaskTier::Nano => 0.0,
            AiTaskTier::Fast => 0.1,
            AiTaskTier::Standard => 0.2,
        };

        let bounded_user = match ctx_tokens {
            Some(ctx) => {
                let reserve = if use_shared_prefix { eff_max_tokens + 1800 } else { eff_max_tokens + 512 };
                let input_budget_tokens = ctx.saturating_sub(reserve).max(512);
                truncate_for_context(&req.user_prompt, (input_budget_tokens as usize).saturating_mul(3))
            }
            None => req.user_prompt.clone(),
        };

        // Choose system prompt + conversation messages (holding per-scope session mutex if Session mode).
        let (system_prompt, session_slot_opt, seed_ctx_opt, cache_bucket) = match &req.mode {
            AiConversationMode::StatelessSharedPrefix => {
                let sys = if use_shared_prefix {
                    HERDR2_CONTROL_PLANE_SYSTEM.to_string()
                } else {
                    compact_system_for_op(&req.op).to_string()
                };
                let bucket = req.prompt_cache_bucket.clone().unwrap_or_else(|| "cp".into());
                (sys, None, None, Some(format!("h2:{bucket}:v1")))
            }
            AiConversationMode::Session { key: scope_key, seed_context } => {
                let sys = if use_shared_prefix {
                    HERDR2_CONTROL_PLANE_SYSTEM.to_string()
                } else {
                    compact_system_for_op(&req.op).to_string()
                };
                let slot = cfg.prompt_cache_enabled.then(|| self.sessions.acquire_slot(scope_key));
                let cache_id = format!("h2:s:{}", fingerprint_hex(&[scope_key]));
                (sys, slot, seed_context.clone(), Some(cache_id))
            }
            AiConversationMode::CustomSystem(custom) => {
                let framed = format!(
                    "You are a pure text assistant with NO tools or shell access. \
                     Never emit <tool_use>, DSML, XML tool tags, or shell commands. {custom}"
                );
                (framed, None, None, None)
            }
        };

        let system_hash = fnv1a_64(system_prompt.as_bytes());
        let session_budget = ctx_tokens
            .map(|c| (c / 3).clamp(2048, MAX_SESSION_TOKENS))
            .unwrap_or(MAX_SESSION_TOKENS);

        // Hold per-scope session lock across prepare -> HTTP -> commit so concurrent calls on the
        // SAME scope append cleanly without racing and breaking KV cache prefix order.
        let mut session_guard = match &session_slot_opt {
            Some(slot) => Some(slot.lock().await),
            None => None,
        };

        let (messages, session_reused, fresh_base) = if let Some(st) = session_guard.as_deref_mut() {
            st.prepare_turn(
                &system_prompt,
                system_hash,
                &model,
                session_budget,
                seed_ctx_opt.as_deref(),
                &bounded_user,
            )
        } else {
            (
                vec![ChatMessage::system(&system_prompt), ChatMessage::user(&bounded_user)],
                false,
                Vec::new(),
            )
        };

        let cap_key = format!("{base}|{model}");
        let mut caps = self
            .endpoint_caps
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&cap_key)
            .copied()
            .unwrap_or_else(|| EndpointCapabilities::default_for_url(&base));

        let prompt_cache_key = cfg
            .prompt_cache_enabled
            .then_some(cache_bucket.as_deref())
            .flatten();
        let candidates = build_chat_payload_candidates(
            &model,
            &messages,
            eff_max_tokens,
            effective_effort,
            ctx_tokens,
            temperature,
            prompt_cache_key,
            caps,
        );

        let mut last_err = String::from("AI endpoint did not respond");
        for url in chat_endpoints(&base) {
            let mut resp = Err("no payload candidates".into());
            let mut winning_variant = None;
            for (idx, variant) in candidates.iter().enumerate() {
                if idx > 0
                    && ((variant.used_num_ctx && !caps.supports_num_ctx)
                        || (variant.used_cache_key && !caps.supports_prompt_cache_key)
                        || (variant.used_effort.is_some() && !caps.supports_reasoning_effort)
                        || (variant.used_effort.as_deref() == Some("none") && !caps.supports_effort_none)
                        || (variant.used_temperature && !caps.supports_temperature))
                {
                    continue;
                }
                resp = curl_json_timeout("POST", &url, key.as_deref(), Some(&variant.body), req.timeout).await;
                let has_next = idx + 1 < candidates.len();
                if let Ok((st, ref body)) = resp {
                    if (200..300).contains(&st) {
                        winning_variant = Some(variant);
                        break;
                    }
                    if has_next && should_retry_without_optional_fields(st, body) {
                        refine_capabilities_from_error(&mut caps, variant, body);
                        continue;
                    }
                }
                break;
            }
            if let Some(win) = winning_variant {
                record_winning_variant(&mut caps, win);
                self.endpoint_caps
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(cap_key.clone(), caps);
            }

            match resp {
                Ok((status, body)) if (200..300).contains(&status) => {
                    let parsed: Value = serde_json::from_str(&body)
                        .map_err(|e| format!("failed to parse AI JSON response: {e}"))?;
                    let (in_tok, out_tok, cache_tok) = parse_usage_tokens(&parsed);
                    let usage = AiUsage {
                        input_tokens: in_tok,
                        output_tokens: out_tok,
                        cached_input_tokens: cache_tok,
                    };
                    let raw_content = parsed["choices"][0]["message"]["content"]
                        .as_str()
                        .map(str::trim)
                        .unwrap_or("");
                    if raw_content.is_empty()
                        || raw_content.contains("<tool_use")
                        || raw_content.contains("DSML")
                    {
                        let finish_reason = parsed["choices"][0]["finish_reason"]
                            .as_str()
                            .unwrap_or("");
                        let reason = if raw_content.is_empty() {
                            if finish_reason == "length" {
                                "empty assistant content (finish_reason=length)"
                            } else {
                                "empty assistant content"
                            }
                        } else {
                            "unexpected tool-call markup in content"
                        };
                        self.record_failure(reason);
                        return Err(reason.into());
                    }

                    self.record_success();
                    self.record_network_usage(&req.op, &usage, session_reused);
                    if let Some(st) = session_guard.as_deref_mut() {
                        st.commit_turn(
                            system_hash,
                            &model,
                            fresh_base,
                            bounded_user,
                            raw_content.to_string(),
                        );
                    }
                    if let (Some(ck), Some(ttl)) = (req.cache_key, req.cache_ttl) {
                        self.result_cache.put(ck, raw_content.to_string(), ttl);
                    }
                    return Ok(AiCompleteResponse {
                        content: raw_content.to_string(),
                        model,
                        latency_ms: started.elapsed().as_millis() as u64,
                        usage,
                        from_local_cache: false,
                        session_reused,
                    });
                }
                Ok((status, body)) => {
                    let snippet: String = body.chars().take(160).collect();
                    last_err = format!("HTTP {status}: {snippet}");
                    self.record_failure(&last_err);
                    if status == 401 || status == 403 {
                        return Err(last_err);
                    }
                }
                Err(e) => {
                    last_err = e;
                    self.record_failure(&last_err);
                }
            }
        }
        Err(last_err)
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
        let cache_key = format!(
            "notes_digest:{address}:{news_seq}:{}",
            fingerprint_hex(&[status_label, &compact_tail])
        );
        let payload = json!({
            "address": address,
            "status": status_label,
            "exit_code": exit_code,
            "cwd": cwd.unwrap_or(""),
            "tail": compact_tail,
        });
        let user_prompt = format_op_user_message("notes_digest", "en", &payload);
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
        let res = self
            .complete(AiCompleteRequest {
                op,
                tier: p.tier.unwrap_or(AiTaskTier::Fast),
                mode,
                user_prompt: p.user_prompt,
                max_tokens: p.max_tokens.unwrap_or(512).clamp(32, 4096),
                timeout: Duration::from_secs(20),
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
    }

    fn record_network_usage(&self, op: &str, usage: &AiUsage, session_reused: bool) {
        let mut u = self.usage.lock().unwrap_or_else(|p| p.into_inner());
        u.total_calls += 1;
        if session_reused {
            u.sessions_reused += 1;
        }
        u.input_tokens += usage.input_tokens;
        u.cached_input_tokens += usage.cached_input_tokens;
        u.output_tokens += usage.output_tokens;
        *u.by_op.entry(op.to_string()).or_default() += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_mock_openai(
        responses: Vec<(u16, String)>,
    ) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
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
                    let req_line = String::from_utf8_lossy(&buf[..buf.iter().position(|b| *b == b'\r').unwrap_or(buf.len())]).into_owned();
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
                    let (status, resp_body) = q
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or_else(|| (200, r#"{"choices":[{"message":{"content":"{\"ok\":true}"}}]}"#.into()));
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
}
