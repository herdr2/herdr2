//! Drove: native Rust multi-agent orchestration inside `herdr2`.

pub mod ai;
pub mod cli;
pub mod config;
pub mod crypto;
pub mod diagnose;
pub mod http;
pub mod integrate;
pub mod magpie;
pub mod notify;
pub mod pool;
pub mod probe;
pub mod protocol;
pub mod review;
pub mod roles;
pub mod router;
pub mod routes;
pub mod sandbox;
pub mod scheduler;
pub mod store;
pub mod types;
pub mod verify;
pub mod yaml;

use self::config::{
    CachedAgentModels, DrovePaths, PersistedModelsCache, clear_ai_key, load_config,
    load_models_cache, mask_key, read_ai_key, save_config, save_models_cache, write_ai_key,
    write_secret_file,
};
use self::http::{HttpProxyRequest, HttpProxyResponse};
use self::magpie::{MagpieClient, MagpieTestResult};
use self::probe::AiTestResult;
use self::store::{Store, StoreEvent};
use self::types::*;
use crate::paths::{Paths, write_atomic};
use crate::rpc::RpcError;
use crate::runtime::Core;
use crate::wire::Conn;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, mpsc};

pub const MAX_AUTO_RETRIES: u32 = 2;

#[derive(Default)]
pub struct AiCircuitState {
    pub consecutive_failures: u32,
    pub circuit_open: bool,
    pub cooldown_until: Option<String>,
    pub last_error: Option<String>,
}

struct FramedSub {
    conn: Arc<Conn>,
    channel: u32,
    task: tokio::task::AbortHandle,
}

/// Live runtime state for Drove inside a single `herdr2` daemon.
pub struct DroveRuntime {
    pub paths: DrovePaths,
    pub store: Arc<Store>,
    pub config: Mutex<DroveConfig>,
    pub magpie: Arc<MagpieClient>,
    pub web_token: String,
    pub read_token: String,
    pub coordinator_secret: String,
    pub web_port: AtomicU16,
    pub started_at: String,
    pub http_stop: Arc<Notify>,
    pub scheduler_wake: Arc<Notify>,
    pub message_reply_wake: Arc<Notify>,
    pub pool_tx: mpsc::UnboundedSender<pool::PoolSignal>,
    pool_rx: Mutex<Option<mpsc::UnboundedReceiver<pool::PoolSignal>>>,
    core: Mutex<Weak<Core>>,
    probe_cache: Mutex<Option<ProbeSnapshot>>,
    probe_is_full: AtomicBool,
    agent_models_cache: Mutex<BTreeMap<String, CachedAgentModels>>,
    ai_models_cache: Mutex<Vec<String>>,
    ai_models_url: Mutex<String>,
    ai_circuit: Mutex<AiCircuitState>,
    subs: Mutex<Vec<FramedSub>>,
    worker_tokens: Mutex<HashMap<String, String>>,
    reserved_tasks: Mutex<HashSet<String>>,
    task_hints: Mutex<HashMap<String, String>>,
    attempt_baselines: Mutex<HashMap<String, roles::DirtyBaseline>>,
    active_asks: Mutex<HashSet<String>>,
    reminded_attempts: Mutex<HashSet<String>>,
    stall_warned: Mutex<HashSet<String>>,
    run_merge_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl DroveRuntime {
    /// Initializes Drove runtime directories, config, store, and daemon secrets.
    pub fn open(paths: &Paths) -> io::Result<Arc<Self>> {
        let dpaths = DrovePaths::from_paths(paths);
        dpaths.ensure_dirs()?;
        let store = Store::open(&dpaths)?;
        let cfg = match load_config(&dpaths) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("drove: using defaults because {}: {}", dpaths.config_file.display(), e.message);
                DroveConfig::default()
            }
        };
        let coordinator_secret = crypto::rand_secret();
        write_secret_file(&dpaths.coordinator_token_file, &format!("{coordinator_secret}\n"))?;
        let disk_cache = load_models_cache(&dpaths);
        let mut agent_models_cache = disk_cache.agent_models;
        if let Some(snap) = &disk_cache.probe {
            for a in &snap.agents {
                if !a.models.is_empty() && !agent_models_cache.contains_key(&a.id) {
                    agent_models_cache.insert(
                        a.id.clone(),
                        CachedAgentModels {
                            models: a.models.clone(),
                            default_model: a.default_model.clone(),
                            efforts: a.efforts.clone(),
                            default_effort: a.default_effort.clone(),
                            agent_info: a.agent_info.clone(),
                        },
                    );
                }
            }
        }
        let probe_is_full = disk_cache.probe.as_ref().is_some_and(|p| p.full_probe);
        let cfg_url = cfg.ai.base_url.trim().to_string();
        let use_saved_ai = !disk_cache.ai_models.is_empty()
            && (cfg_url.is_empty() || disk_cache.ai_base_url.is_empty() || disk_cache.ai_base_url == cfg_url);
        let ai_models_cache = if use_saved_ai { disk_cache.ai_models } else { Vec::new() };
        let ai_models_url = if use_saved_ai {
            if disk_cache.ai_base_url.is_empty() { cfg_url } else { disk_cache.ai_base_url }
        } else {
            String::new()
        };
        let (pool_tx, pool_rx) = mpsc::unbounded_channel();
        Ok(Arc::new(Self {
            paths: dpaths,
            store,
            config: Mutex::new(cfg),
            magpie: Arc::new(MagpieClient::default()),
            web_token: crypto::rand_secret(),
            read_token: crypto::rand_secret(),
            coordinator_secret,
            web_port: AtomicU16::new(0),
            started_at: now_iso(),
            http_stop: Arc::new(Notify::new()),
            scheduler_wake: Arc::new(Notify::new()),
            message_reply_wake: Arc::new(Notify::new()),
            pool_tx,
            pool_rx: Mutex::new(Some(pool_rx)),
            core: Mutex::new(Weak::new()),
            probe_cache: Mutex::new(disk_cache.probe),
            probe_is_full: AtomicBool::new(probe_is_full),
            agent_models_cache: Mutex::new(agent_models_cache),
            ai_models_cache: Mutex::new(ai_models_cache),
            ai_models_url: Mutex::new(ai_models_url),
            ai_circuit: Mutex::new(AiCircuitState::default()),
            subs: Mutex::new(Vec::new()),
            worker_tokens: Mutex::new(HashMap::new()),
            reserved_tasks: Mutex::new(HashSet::new()),
            task_hints: Mutex::new(HashMap::new()),
            attempt_baselines: Mutex::new(HashMap::new()),
            active_asks: Mutex::new(HashSet::new()),
            reminded_attempts: Mutex::new(HashSet::new()),
            stall_warned: Mutex::new(HashSet::new()),
            run_merge_locks: Mutex::new(HashMap::new()),
        }))
    }

    pub fn run_merge_lock(&self, run_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.run_merge_locks.lock().unwrap_or_else(|p| p.into_inner());
        locks
            .entry(run_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub fn take_pool_rx(&self) -> Option<mpsc::UnboundedReceiver<pool::PoolSignal>> {
        self.pool_rx.lock().unwrap_or_else(|p| p.into_inner()).take()
    }

    pub fn wake_scheduler(&self) {
        self.scheduler_wake.notify_one();
    }

    pub fn cached_probe(&self) -> ProbeSnapshot {
        self.probe_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .unwrap_or_default()
    }

    fn persist_models_cache(&self) {
        let probe = self.probe_cache.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let agent_models = self.agent_models_cache.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let ai_base_url = self.ai_models_url.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let ai_models = self.ai_models_cache.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let cache = PersistedModelsCache {
            updated_at: now_iso(),
            probe,
            agent_models,
            ai_base_url,
            ai_models,
        };
        save_models_cache(&self.paths, &cache);
    }

    fn apply_and_persist_probe(&self, mut snap: ProbeSnapshot, is_full: bool) -> ProbeSnapshot {
        {
            let mut map = self.agent_models_cache.lock().unwrap_or_else(|p| p.into_inner());
            for a in &mut snap.agents {
                if is_full && a.installed && a.error.is_none() && !a.models.is_empty() {
                    map.insert(
                        a.id.clone(),
                        CachedAgentModels {
                            models: a.models.clone(),
                            default_model: a.default_model.clone(),
                            efforts: a.efforts.clone(),
                            default_effort: a.default_effort.clone(),
                            agent_info: a.agent_info.clone(),
                        },
                    );
                } else if let Some(saved) = map.get(&a.id) {
                    a.models = merge_unique_models(&a.models, &saved.models);
                    if a.default_model.as_deref().is_none_or(|s| s.trim().is_empty()) {
                        a.default_model = saved.default_model.clone();
                    }
                    if a.efforts.is_empty() && !saved.efforts.is_empty() {
                        a.efforts = saved.efforts.clone();
                    }
                    if a.default_effort.is_none() {
                        a.default_effort = saved.default_effort.clone();
                    }
                    if a.agent_info.is_none() {
                        a.agent_info = saved.agent_info.clone();
                    }
                } else if a.installed && !a.models.is_empty() {
                    map.insert(
                        a.id.clone(),
                        CachedAgentModels {
                            models: a.models.clone(),
                            default_model: a.default_model.clone(),
                            efforts: a.efforts.clone(),
                            default_effort: a.default_effort.clone(),
                            agent_info: a.agent_info.clone(),
                        },
                    );
                }
            }
        }
        if is_full {
            snap.full_probe = true;
            *self.probe_cache.lock().unwrap_or_else(|p| p.into_inner()) = Some(snap.clone());
            self.probe_is_full.store(true, Ordering::SeqCst);
            self.persist_models_cache();
        } else if !self.probe_is_full.load(Ordering::SeqCst) {
            *self.probe_cache.lock().unwrap_or_else(|p| p.into_inner()) = Some(snap.clone());
            self.persist_models_cache();
        }
        snap
    }

    fn update_ai_models(&self, base_url: &str, models: Vec<String>) {
        let deduped = merge_unique_models(&models, &[]);
        if deduped.is_empty() {
            return;
        }
        *self.ai_models_cache.lock().unwrap_or_else(|p| p.into_inner()) = deduped;
        *self.ai_models_url.lock().unwrap_or_else(|p| p.into_inner()) = base_url.trim().to_string();
        self.persist_models_cache();
    }

    pub fn register_worker_token(&self, worker_id: &str) -> String {
        let tok = crypto::rand_secret();
        self.worker_tokens
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(worker_id.to_string(), tok.clone());
        tok
    }

    pub fn verify_worker_token(&self, worker_id: &str, token: &str) -> bool {
        let map = self.worker_tokens.lock().unwrap_or_else(|p| p.into_inner());
        map.get(worker_id)
            .is_some_and(|expected| crypto::secure_eq(expected, token))
    }

    pub fn try_reserve_task(&self, task_id: &str) -> bool {
        self.reserved_tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(task_id.to_string())
    }

    pub fn release_task_reservation(&self, task_id: &str) {
        self.reserved_tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(task_id);
    }

    pub fn set_task_hint(&self, task_id: &str, hint: String) {
        self.task_hints
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(task_id.to_string(), hint);
    }

    pub fn take_task_hint(&self, task_id: &str) -> Option<String> {
        self.task_hints
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(task_id)
    }

    pub fn record_attempt_baseline(&self, attempt_id: &str, baseline: roles::DirtyBaseline) {
        self.attempt_baselines
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(attempt_id.to_string(), baseline);
    }

    pub fn get_attempt_baseline(&self, attempt_id: &str) -> Option<roles::DirtyBaseline> {
        self.attempt_baselines
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(attempt_id)
            .cloned()
    }

    pub fn enter_ask(&self, attempt_id: &str) {
        self.active_asks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(attempt_id.to_string());
    }

    pub fn leave_ask(&self, attempt_id: &str) {
        self.active_asks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(attempt_id);
    }

    pub fn has_active_ask(&self, attempt_id: &str) -> bool {
        self.active_asks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(attempt_id)
    }

    /// Returns `true` if already reminded before, else records reminder and returns `false`.
    pub fn mark_attempt_reminded(&self, attempt_id: &str) -> bool {
        let mut set = self.reminded_attempts.lock().unwrap_or_else(|p| p.into_inner());
        if set.contains(attempt_id) {
            true
        } else {
            set.insert(attempt_id.to_string());
            false
        }
    }

    /// Returns `true` the first time this attempt crosses its silence warning threshold.
    pub fn mark_stall_warned(&self, attempt_id: &str) -> bool {
        self.stall_warned
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(attempt_id.to_string())
    }

    pub fn is_ai_circuit_open(&self) -> bool {
        let mut cir = self.ai_circuit.lock().unwrap_or_else(|p| p.into_inner());
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

    pub fn record_ai_success(&self) {
        let mut cir = self.ai_circuit.lock().unwrap_or_else(|p| p.into_inner());
        cir.consecutive_failures = 0;
        cir.circuit_open = false;
        cir.cooldown_until = None;
        cir.last_error = None;
    }

    pub fn record_ai_failure(&self, err: &str) {
        let mut cir = self.ai_circuit.lock().unwrap_or_else(|p| p.into_inner());
        cir.consecutive_failures += 1;
        cir.last_error = Some(err.to_string());
        if cir.consecutive_failures >= ai::circuit_threshold() {
            cir.circuit_open = true;
            cir.cooldown_until = Some(format_ms_iso(now_ms_u64() + ai::circuit_cooldown_secs() * 1000));
        }
    }

    pub fn attach_core(&self, core: &Arc<Core>) {
        *self.core.lock().unwrap_or_else(|p| p.into_inner()) = Arc::downgrade(core);
    }

    pub fn core(&self) -> Option<Arc<Core>> {
        self.core.lock().unwrap_or_else(|p| p.into_inner()).upgrade()
    }

    pub fn config_snapshot(&self) -> DroveConfig {
        self.config.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn write_web_info(&self, port: u16) {
        let info = json!({
            "pid": std::process::id(),
            "port": port,
            "token": self.read_token,
            "startedAt": self.started_at,
        });
        if let Ok(bytes) = serde_json::to_vec_pretty(&info) {
            let _ = write_atomic(&self.paths.web_info_file, &bytes);
        }
    }

    pub fn shutdown(&self) {
        self.http_stop.notify_waiters();
        let subs: Vec<FramedSub> = self.subs.lock().unwrap_or_else(|p| p.into_inner()).drain(..).collect();
        for s in subs {
            s.task.abort();
        }
        if let Ok(text) = std::fs::read_to_string(&self.paths.web_info_file)
            && let Ok(v) = serde_json::from_str::<Value>(&text)
            && v["pid"].as_u64() == Some(std::process::id() as u64)
        {
            let _ = std::fs::remove_file(&self.paths.web_info_file);
        }
        if let Ok(text) = std::fs::read_to_string(&self.paths.coordinator_token_file)
            && text.trim() == self.coordinator_secret
        {
            let _ = std::fs::remove_file(&self.paths.coordinator_token_file);
        }
    }

    pub fn unsubscribe_conn(&self, conn: &Arc<Conn>, channel: Option<u32>) {
        let mut subs = self.subs.lock().unwrap_or_else(|p| p.into_inner());
        let mut i = 0;
        while i < subs.len() {
            if Arc::ptr_eq(&subs[i].conn, conn) && channel.is_none_or(|c| subs[i].channel == c) {
                let s = subs.swap_remove(i);
                s.task.abort();
            } else {
                i += 1;
            }
        }
    }

    pub fn web_url(&self, run_id: Option<&str>) -> String {
        let port = self.web_port.load(Ordering::SeqCst);
        let base = format!("http://127.0.0.1:{port}/");
        match run_id.map(str::trim).filter(|s| !s.is_empty()) {
            Some(r) => format!("{base}?run={}", percent_encode_param(r)),
            None => base,
        }
    }

    /// Computes AI status without exposing the raw API key.
    pub fn ai_status(&self) -> DaemonAiStatus {
        let cfg = self.config_snapshot();
        let (key, src) = read_ai_key(&self.paths);
        let cir = self.ai_circuit.lock().unwrap_or_else(|p| p.into_inner());
        let has_key = key.is_some();
        let is_local = config::is_loopback_http_url(&cfg.ai.base_url);
        let configured = cfg.ai.enabled && !cfg.ai.base_url.trim().is_empty() && (has_key || is_local);
        let key_source = if has_key {
            src.to_string()
        } else if is_local && !cfg.ai.base_url.trim().is_empty() {
            "local".into()
        } else {
            src.to_string()
        };
        DaemonAiStatus {
            enabled: cfg.ai.enabled,
            configured,
            key_source,
            key_file_path: self.paths.ai_key_file.display().to_string(),
            key_masked: key.as_deref().map(mask_key),
            base_url: cfg.ai.base_url.clone(),
            model: cfg.ai.model.clone(),
            available_models: self.ai_models_cache.lock().unwrap_or_else(|p| p.into_inner()).clone(),
            circuit_open: cir.circuit_open,
            cooldown_until: cir.cooldown_until.clone(),
            consecutive_failures: cir.consecutive_failures,
            last_error: cir.last_error.clone(),
            switches: AiSwitches {
                verify_digest: cfg.ai.verify_digest,
                handover_compact: true,
                auto_title: cfg.ai.auto_title,
                task_enrich: cfg.ai.task_enrich,
                conflict_policy: cfg.ai.conflict_policy.clone(),
                question_mode: cfg.ai.question_mode.clone(),
                loop_detect: "off".into(),
            },
        }
    }

    // --- RPC handlers -------------------------------------------------------

    pub async fn rpc_settings_get(&self) -> DroveSettingsResult {
        let cfg = self.config_snapshot();
        let cfg_url = cfg.ai.base_url.trim();
        if !cfg_url.is_empty() {
            let need_fetch = {
                let cached = self.ai_models_cache.lock().unwrap_or_else(|p| p.into_inner());
                let cached_url = self.ai_models_url.lock().unwrap_or_else(|p| p.into_inner());
                cached.is_empty() || *cached_url != cfg_url
            };
            if need_fetch {
                let (stored_key, _) = read_ai_key(&self.paths);
                let discovered = probe::fetch_ai_models(cfg_url, stored_key.as_deref()).await;
                if !discovered.is_empty() {
                    self.update_ai_models(cfg_url, discovered);
                }
            }
        }
        let ai_status = self.ai_status();
        let cached = self.probe_cache.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let agents = match cached {
            Some(snap) => snap,
            None => match self.core() {
                Some(core) => {
                    let quick = probe::probe_agents(&core, true).await;
                    self.apply_and_persist_probe(quick, false)
                }
                None => ProbeSnapshot::default(),
            },
        };
        let magpie_gateway = MagpieClient::resolve_gateway(cfg.magpie.gateway_url.as_deref());
        let (magpie_quotas, magpie_error) = if cfg.magpie.enabled {
            self.magpie.fetch_if_enabled(&cfg.magpie, false).await
        } else {
            (None, None)
        };
        DroveSettingsResult {
            machine: self.core().map(|c| c.info.name.clone()).unwrap_or_else(|| "local".into()),
            web_port: self.web_port.load(Ordering::SeqCst),
            config_path: self.paths.config_file.display().to_string(),
            config: cfg,
            ai_status,
            magpie_gateway_resolved: magpie_gateway,
            magpie_quotas,
            magpie_error,
            agents,
        }
    }

    pub async fn rpc_settings_set(&self, params: DroveSettingsSetParams) -> Result<DroveSettingsResult, RpcError> {
        let prev_base_url = self.config_snapshot().ai.base_url;
        let mut next = params.config.unwrap_or_else(|| self.config_snapshot());

        if let Some(mc) = params.max_concurrency_default {
            next.max_concurrency_default = mc;
        }
        if let Some(roles) = params.roles {
            next.roles = roles;
        }
        let mut patch_api_key: Option<String> = None;
        let mut patch_clear = false;
        if let Some(ai) = params.ai {
            if let Some(b) = ai.enabled {
                next.ai.enabled = b;
            }
            if let Some(s) = ai.base_url {
                next.ai.base_url = s;
            }
            if let Some(s) = ai.model {
                next.ai.model = s;
            }
            if let Some(b) = ai.auto_title {
                next.ai.auto_title = b;
            }
            if let Some(b) = ai.verify_digest {
                next.ai.verify_digest = b;
            }
            if let Some(b) = ai.task_enrich {
                next.ai.task_enrich = b;
            }
            if let Some(s) = ai.question_mode {
                next.ai.question_mode = s;
            }
            if let Some(s) = ai.conflict_policy {
                next.ai.conflict_policy = s;
            }
            patch_api_key = ai.api_key;
            patch_clear = ai.clear_key.unwrap_or(false);
        }
        if let Some(mag) = params.magpie {
            if let Some(b) = mag.enabled {
                next.magpie.enabled = b;
            }
            if let Some(url) = mag.gateway_url {
                next.magpie.gateway_url = Some(url);
            }
        }

        let mut key_changed = false;
        if params.clear_ai_key == Some(true) || patch_clear {
            clear_ai_key(&self.paths).map_err(|e| RpcError::internal(format!("clear key: {e}")))?;
            key_changed = true;
        } else if let Some(key) = params
            .api_key
            .or(patch_api_key)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            write_ai_key(&self.paths, &key)?;
            key_changed = true;
        }

        save_config(&self.paths, &mut next)?;
        let url_changed = next.ai.base_url.trim() != prev_base_url.trim();
        let cached_matches = *self.ai_models_url.lock().unwrap_or_else(|p| p.into_inner()) == next.ai.base_url.trim();
        *self.config.lock().unwrap_or_else(|p| p.into_inner()) = next;
        self.magpie.invalidate();
        if (url_changed && !cached_matches) || key_changed {
            self.ai_models_cache.lock().unwrap_or_else(|p| p.into_inner()).clear();
            self.ai_models_url.lock().unwrap_or_else(|p| p.into_inner()).clear();
            self.persist_models_cache();
        }
        Ok(self.rpc_settings_get().await)
    }

    pub async fn rpc_probe(&self, refresh: bool) -> Result<ProbeSnapshot, RpcError> {
        if !refresh
            && self.probe_is_full.load(Ordering::SeqCst)
            && let Some(cached) = self.probe_cache.lock().unwrap_or_else(|p| p.into_inner()).clone()
        {
            return Ok(cached);
        }
        let core = self.core().ok_or_else(|| RpcError::internal("core unavailable"))?;
        let snap = probe::probe_agents(&core, !refresh).await;
        Ok(self.apply_and_persist_probe(snap, refresh))
    }

    pub async fn rpc_ai_test(
        &self,
        url_override: Option<String>,
        key_override: Option<String>,
        model_override: Option<String>,
    ) -> Result<AiTestResult, RpcError> {
        let cfg = self.config_snapshot();
        let base_url = url_override.filter(|s| !s.trim().is_empty()).unwrap_or(cfg.ai.base_url);
        if base_url.trim().is_empty() {
            return Err(RpcError::invalid_params("AI Service URL is empty"));
        }
        let (stored_key, _) = read_ai_key(&self.paths);
        let key = key_override.filter(|s| !s.trim().is_empty()).or(stored_key);
        let model = model_override
            .filter(|s| !s.trim().is_empty())
            .or_else(|| (!cfg.ai.model.trim().is_empty()).then_some(cfg.ai.model));
        let res = probe::test_ai_connection(&base_url, key.as_deref(), model.as_deref()).await;
        let discovered: Vec<String> = res.discovered_model.iter().cloned().collect();
        let merged = merge_unique_models(&discovered, &res.available_models);
        if !merged.is_empty() {
            self.update_ai_models(&base_url, merged);
        }
        {
            let mut cir = self.ai_circuit.lock().unwrap_or_else(|p| p.into_inner());
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

    pub async fn rpc_magpie_test(&self, gateway_override: Option<String>) -> Result<MagpieTestResult, RpcError> {
        let cfg = self.config_snapshot();
        let target = gateway_override.or(cfg.magpie.gateway_url);
        Ok(self.magpie.test_gateway(target.as_deref()).await)
    }

    pub async fn rpc_http_proxy(&self, req: HttpProxyRequest) -> Result<HttpProxyResponse, RpcError> {
        let core = self.core().ok_or_else(|| RpcError::internal("core unavailable"))?;
        Ok(http::dispatch_http(&core, req, true).await)
    }

    pub fn rpc_subscribe(&self, conn: &Arc<Conn>) -> Result<Value, RpcError> {
        let channel = conn.new_channel();
        conn.with(|out| out.open(channel));
        let mut rx = self.store.subscribe();
        let c = conn.clone();
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            tick.tick().await;
            loop {
                tokio::select! {
                    ev = rx.recv() => match ev {
                        Ok(StoreEvent::Change(chg)) if chg.table != "items" => {
                            let line = format!("{}\n", json!({ "event": "change", "data": chg }));
                            c.with(|out| {
                                out.data(channel, line.as_bytes());
                            });
                        }
                        Ok(StoreEvent::Item { item, .. }) => {
                            let line = format!("{}\n", json!({ "event": "item", "data": item }));
                            c.with(|out| {
                                out.data(channel, line.as_bytes());
                            });
                        }
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                    _ = tick.tick() => {
                        let line = format!("{}\n", json!({ "heartbeat": true }));
                        c.with(|out| {
                            if out.queued(channel) == 0 {
                                out.data(channel, line.as_bytes());
                            }
                        });
                    }
                }
            }
        });
        self.subs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(FramedSub { conn: conn.clone(), channel, task: task.abort_handle() });
        Ok(json!({ "channel": channel }))
    }

    pub fn open_web(&self, run_id: Option<&str>, no_open: bool) -> Result<DroveWebOpenResult, RpcError> {
        let clean_run = run_id.map(str::trim).filter(|s| !s.is_empty());
        if let Some(rid) = clean_run {
            validate_run_id_chars(rid)?;
        }
        // When invoked inside an SSH-connected remote core, ask connected peers to open their local browser.
        if !no_open
            && let Some(core) = self.core()
            && crate::remote::notify_peers_drove_open(&core, clean_run)
        {
            let url = self.web_url(clean_run);
            return Ok(DroveWebOpenResult { url, opened: true, via_peer: true });
        }
        let url = self.web_url(clean_run);
        if !no_open && std::env::var_os("H2_DROVE_NO_OPEN").is_none() {
            let _ = launch_browser(&url);
        }
        Ok(DroveWebOpenResult { url, opened: !no_open, via_peer: false })
    }

    pub fn record_peer_open_url(&self, machine: &str, run_id: Option<&str>) {
        let port = self.web_port.load(Ordering::SeqCst);
        let enc_machine = percent_encode_param(machine);
        let url = if let Some(rid) = run_id.filter(|s| !s.is_empty()) {
            if validate_run_id_chars(rid).is_err() {
                return;
            }
            let combined = format!("{machine}/{rid}");
            format!("http://127.0.0.1:{port}/?on={enc_machine}&run={}", percent_encode_param(&combined))
        } else {
            format!("http://127.0.0.1:{port}/?on={enc_machine}")
        };
        if std::env::var_os("H2_DROVE_NO_OPEN").is_none() {
            let _ = launch_browser(&url);
        }
    }
}

/// Starts the background Drove HTTP/SSE service, pool signal loop, and DAG scheduler.
pub fn start_background(core: &Arc<Core>) {
    core.drove.attach_core(core);
    tokio::spawn(http::serve_http(core.clone()));
    if let Some(rx) = core.drove.take_pool_rx() {
        tokio::spawn(pool::run_signal_loop(core.clone(), rx));
    }
    tokio::spawn(scheduler::run_scheduler_loop(core.clone()));
}

pub async fn worktree_for_task(core: &Arc<Core>, run: &Run, task: &Task) -> Result<(String, Option<String>), String> {
    integrate::prepare_task_worktree(core, run, task).await
}

pub async fn fail_or_retry_task(
    core: &Arc<Core>,
    run_id: &str,
    task_id: &str,
    attempt_id: &str,
    kind: FailureKind,
    message: &str,
    max_retries: u32,
) {
    pool::handle_task_failure(core, run_id, task_id, attempt_id, kind, message, max_retries).await;
}

pub fn now_ms_u64() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn merge_unique_models(primary: &[String], extra: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in primary.iter().chain(extra.iter()) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() && !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

pub(crate) fn validate_run_id_chars(id: &str) -> Result<(), RpcError> {
    if id.is_empty()
        || id.len() > 96
        || !id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(RpcError::invalid_params(format!("invalid run id {id:?}")));
    }
    Ok(())
}

fn percent_encode_param(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub(crate) fn bg_std_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    #[allow(unused_mut)]
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

pub(crate) fn bg_tokio_command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

fn launch_browser(url: &str) -> io::Result<()> {
    if !url.starts_with("http://127.0.0.1:") {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "refusing non-localhost browser URL"));
    }
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = bg_std_command("cmd");
        c.args(["/C", "start", "", url]);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(dir: &std::path::Path) -> Paths {
        Paths {
            socket: dir.join("core.sock"),
            holder_socket: dir.join("holder.sock"),
            state: dir.join("state"),
            config: dir.join("config/config.toml"),
        }
    }

    #[test]
    fn models_cache_persists_and_preserves_models_on_timeout_across_reopen() {
        let tmp = crate::test_tmp().join(format!("drove-mcache-{}", crate::platform::random_hex()));
        let paths = test_paths(&tmp);
        paths.create_dirs().unwrap();

        let rt = DroveRuntime::open(&paths).unwrap();
        let initial = ProbeSnapshot {
            probed_at: now_iso(),
            full_probe: true,
            agents: vec![ProbedAgent {
                id: "claude".into(),
                command: vec!["claude-agent-acp".into()],
                installed: true,
                path: Some("/usr/bin/claude-agent-acp".into()),
                agent_info: Some("claude-code-acp 0.1".into()),
                models: vec!["claude-opus-4-6".into(), "claude-sonnet-4-6".into()],
                default_model: Some("claude-sonnet-4-6".into()),
                efforts: vec!["low".into(), "medium".into(), "high".into()],
                default_effort: Some("high".into()),
                error: None,
            }],
        };
        rt.apply_and_persist_probe(initial, true);
        rt.update_ai_models("http://127.0.0.1:4000/v1", vec!["deepseek-chat".into(), "gpt-4o-mini".into()]);
        drop(rt);

        // Reopen runtime -> loads persisted probe + full_probe + agent models + AI models.
        let rt2 = DroveRuntime::open(&paths).unwrap();
        let cached = rt2.cached_probe();
        assert!(cached.full_probe);
        assert_eq!(cached.agents[0].models, vec!["claude-opus-4-6", "claude-sonnet-4-6"]);
        assert_eq!(rt2.ai_models_cache.lock().unwrap().as_slice(), &["deepseek-chat", "gpt-4o-mini"]);

        // Subsequent full probe where claude times out preserves previously saved models.
        let timed_out = ProbeSnapshot {
            probed_at: now_iso(),
            full_probe: true,
            agents: vec![ProbedAgent {
                id: "claude".into(),
                command: vec!["claude-agent-acp".into()],
                installed: true,
                path: Some("/usr/bin/claude-agent-acp".into()),
                agent_info: None,
                models: vec![],
                default_model: None,
                efforts: vec![],
                default_effort: None,
                error: Some("ACP probe timed out after 10s".into()),
            }],
        };
        let merged = rt2.apply_and_persist_probe(timed_out, true);
        assert_eq!(merged.agents[0].models, vec!["claude-opus-4-6", "claude-sonnet-4-6"]);
        assert_eq!(merged.agents[0].default_model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(merged.agents[0].agent_info.as_deref(), Some("claude-code-acp 0.1"));

        let _ = std::fs::remove_dir_all(tmp);
    }
}
