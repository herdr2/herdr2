//! Optional Magpie quota gateway client (disabled by default; never probes unless enabled).

use super::config::validate_http_url;
use super::types::{MagpieSettings, QuotaCard, QuotaWindow, parse_iso_ms};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CACHE_TTL: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MagpieTestResult {
    pub ok: bool,
    pub gateway_url: String,
    pub cards_count: usize,
    pub providers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Default)]
struct Cache {
    cards: Vec<QuotaCard>,
    fetched_at: Option<Instant>,
    last_error: Option<String>,
}

pub struct MagpieClient {
    cache: Mutex<Cache>,
}

impl Default for MagpieClient {
    fn default() -> Self {
        Self { cache: Mutex::new(Cache::default()) }
    }
}

impl MagpieClient {
    /// Resolves Magpie gateway URL: explicit override > env > local `gateway.json`/`settings.json` > `:3425`.
    pub fn resolve_gateway(override_url: Option<&str>) -> String {
        if let Some(u) = override_url.map(str::trim).filter(|s| !s.is_empty()) {
            return u.trim_end_matches('/').to_string();
        }
        for var in ["H2_DROVE_MAGPIE_GATEWAY", "CO_MAGPIE_GATEWAY", "MAGPIE_GATEWAY"] {
            if let Ok(u) = std::env::var(var)
                && !u.trim().is_empty()
            {
                return u.trim().trim_end_matches('/').to_string();
            }
        }
        if let Some(gw) = discover_local_magpie_gateway() {
            return gw;
        }
        "http://127.0.0.1:3425".into()
    }

    pub fn invalidate(&self) {
        let mut g = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        g.fetched_at = None;
        g.last_error = None;
        g.cards.clear();
    }

    /// Returns cached or freshly fetched quota cards, OR `None` immediately when disabled.
    pub async fn fetch_if_enabled(
        &self,
        settings: &MagpieSettings,
        force_refresh: bool,
    ) -> (Option<Vec<QuotaCard>>, Option<String>) {
        if !settings.enabled {
            return (None, None);
        }
        if !force_refresh {
            let g = self.cache.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(at) = g.fetched_at
                && at.elapsed() < CACHE_TTL
            {
                return (Some(g.cards.clone()), g.last_error.clone());
            }
        }
        let gateway = Self::resolve_gateway(settings.gateway_url.as_deref());
        match fetch_quotas_http(&gateway).await {
            Ok(cards) => {
                let mut g = self.cache.lock().unwrap_or_else(|p| p.into_inner());
                g.cards = cards.clone();
                g.fetched_at = Some(Instant::now());
                g.last_error = None;
                (Some(cards), None)
            }
            Err(http_err) => {
                if is_local_gateway_target(&gateway, settings.gateway_url.as_deref())
                    && let Some(cards) = read_quotas_file_fallback()
                {
                    let mut g = self.cache.lock().unwrap_or_else(|p| p.into_inner());
                    g.cards = cards.clone();
                    g.fetched_at = Some(Instant::now());
                    g.last_error = None;
                    return (Some(cards), None);
                }
                let mut g = self.cache.lock().unwrap_or_else(|p| p.into_inner());
                g.cards.clear();
                g.fetched_at = Some(Instant::now());
                g.last_error = Some(http_err.clone());
                (Some(Vec::new()), Some(http_err))
            }
        }
    }

    /// Performs an on-demand connectivity check for Settings ("Test Magpie").
    pub async fn test_gateway(&self, gateway_override: Option<&str>) -> MagpieTestResult {
        let raw = Self::resolve_gateway(gateway_override);
        let url = match validate_http_url(&raw, "magpie.gatewayUrl") {
            Ok(u) => u,
            Err(e) => {
                return MagpieTestResult {
                    ok: false,
                    gateway_url: raw,
                    cards_count: 0,
                    providers: Vec::new(),
                    error: Some(e.message),
                };
            }
        };
        match fetch_quotas_http(&url).await {
            Ok(cards) => {
                let providers = cards.iter().map(|c| c.provider.clone()).collect();
                MagpieTestResult { ok: true, gateway_url: url, cards_count: cards.len(), providers, error: None }
            }
            Err(err) => {
                if is_local_gateway_target(&url, gateway_override)
                    && let Some(cards) = read_quotas_file_fallback()
                {
                    let providers = cards.iter().map(|c| c.provider.clone()).collect();
                    return MagpieTestResult {
                        ok: true,
                        gateway_url: format!("{url} (quotas.json fallback)"),
                        cards_count: cards.len(),
                        providers,
                        error: None,
                    };
                }
                MagpieTestResult {
                    ok: false,
                    gateway_url: url,
                    cards_count: 0,
                    providers: Vec::new(),
                    error: Some(err),
                }
            }
        }
    }
}

fn magpie_config_dirs() -> Vec<PathBuf> {
    let (_, config_home) = crate::platform::data_dirs();
    let mut dirs = Vec::new();
    let mut push_dir = |p: PathBuf| {
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    };
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        push_dir(PathBuf::from(xdg).join("magpie"));
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        push_dir(PathBuf::from(home).join(".config").join("magpie"));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        push_dir(PathBuf::from(profile).join(".config").join("magpie"));
    }
    push_dir(config_home.join("magpie"));

    // Inside WSL2, Windows Magpie stores its config under /mnt/{c,d}/Users/<user>/.config/magpie.
    #[cfg(target_os = "linux")]
    {
        for drive in ["/mnt/c/Users", "/mnt/d/Users"] {
            if let Ok(entries) = std::fs::read_dir(drive) {
                for entry in entries.flatten() {
                    let candidate = entry.path().join(".config").join("magpie");
                    if candidate.is_dir() {
                        push_dir(candidate);
                    }
                }
            }
        }
    }
    dirs
}

fn discover_local_magpie_gateway() -> Option<String> {
    for dir in magpie_config_dirs() {
        if let Some(gw) = gateway_from_dir(&dir) {
            return Some(gw);
        }
    }
    None
}

pub(crate) fn gateway_from_dir(dir: &Path) -> Option<String> {
    if let Ok(text) = std::fs::read_to_string(dir.join("gateway.json"))
        && let Ok(val) = serde_json::from_str::<Value>(&text)
        && let Some(addr) = val["addr"].as_str().map(str::trim).filter(|s| !s.is_empty())
    {
        let normalized =
            addr.strip_prefix("0.0.0.0:").map(|p| format!("127.0.0.1:{p}")).unwrap_or_else(|| addr.to_string());
        let url = if normalized.starts_with("http://") || normalized.starts_with("https://") {
            normalized
        } else {
            format!("http://{normalized}")
        };
        return Some(url.trim_end_matches('/').to_string());
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("settings.json"))
        && let Ok(val) = serde_json::from_str::<Value>(&text)
        && let Some(port) = val["port"].as_u64()
        && (1..=65535).contains(&port)
    {
        return Some(format!("http://127.0.0.1:{port}"));
    }
    None
}

fn is_local_gateway_target(resolved: &str, explicit_override: Option<&str>) -> bool {
    if explicit_override.is_none_or(|s| s.trim().is_empty()) {
        return true;
    }
    let lower = resolved.to_ascii_lowercase();
    lower.starts_with("http://127.0.0.1") || lower.starts_with("http://localhost")
}

fn read_quotas_file_fallback() -> Option<Vec<QuotaCard>> {
    for dir in magpie_config_dirs() {
        let Ok(text) = std::fs::read_to_string(dir.join("quotas.json")) else {
            continue;
        };
        if let Ok(cards) = parse_quotas_file_json(&text)
            && !cards.is_empty()
        {
            return Some(cards);
        }
    }
    None
}

pub(crate) fn parse_quotas_file_json(text: &str) -> Result<Vec<QuotaCard>, String> {
    let val: Value = serde_json::from_str(text).map_err(|e| format!("invalid quotas.json: {e}"))?;
    let mut cards = Vec::new();
    let entries: Vec<&Value> = if let Some(map) = val.as_object() {
        map.values().collect()
    } else if let Some(arr) = val.as_array() {
        arr.iter().collect()
    } else {
        return Ok(cards);
    };

    for entry in entries {
        let quota_val = entry.get("quota").unwrap_or(entry);
        let Ok(mut card) = serde_json::from_value::<QuotaCard>(quota_val.clone()) else {
            continue;
        };
        if card.as_of.is_none()
            && let Some(at) = entry["at"].as_str().filter(|s| !s.is_empty())
        {
            card.as_of = Some(at.to_string());
        }
        if let Some(raw_wins) = quota_val.get("windows").and_then(Value::as_array)
            && let Some(parsed_wins) = card.windows.as_mut()
        {
            for (raw_w, w) in raw_wins.iter().zip(parsed_wins.iter_mut()) {
                if raw_w.get("remaining").and_then(Value::as_f64).is_none() {
                    w.remaining = infer_remaining(raw_w, w);
                }
            }
        }
        cards.push(card);
    }
    Ok(cards)
}

fn infer_remaining(raw: &Value, win: &QuotaWindow) -> f64 {
    if win.unlimited == Some(true) {
        return 100.0;
    }
    if let (Some(limit), Some(used)) = (raw.get("limit").and_then(Value::as_f64), win.used)
        && limit > 0.0
    {
        return ((limit - used) / limit * 100.0).clamp(0.0, 100.0);
    }
    if let Some(used) = win.used {
        return (100.0 - used).clamp(0.0, 100.0);
    }
    100.0
}

fn provider_aliases_for_agent(agent: &str) -> &'static [&'static str] {
    match agent.trim().to_ascii_lowercase().as_str() {
        "claude" => &["claude"],
        "codex" => &["codex"],
        "agy" | "antigravity" => &["agy-cli", "antigravity"],
        "cursor" | "cursor-agent" => &["cursor"],
        "zcode" | "glm" => &["zcode"],
        "dsh" => &["deepseek"],
        "qoder" => &["qoder", "qoder-cn"],
        _ => &[],
    }
}

fn parse_balance_amount(balance: Option<&str>) -> Option<f64> {
    let raw = balance?.trim();
    if raw.is_empty() {
        return None;
    }
    let cleaned = raw.replace(',', "");
    let mut num = String::new();
    let mut started = false;
    let mut seen_dot = false;
    for ch in cleaned.chars() {
        if !started {
            if ch == '-' || ch.is_ascii_digit() {
                num.push(ch);
                started = true;
            }
        } else if ch.is_ascii_digit() {
            num.push(ch);
        } else if ch == '.' && !seen_dot {
            num.push(ch);
            seen_dot = true;
        } else {
            break;
        }
    }
    num.parse::<f64>().ok().filter(|v| v.is_finite())
}

enum CardStatus {
    Available,
    Exhausted(String),
    Unknown,
}

fn evaluate_card(card: &QuotaCard, agent: &str, model: Option<&str>, now_ms: u64) -> CardStatus {
    let has_windows = card.windows.as_ref().is_some_and(|w| !w.is_empty());
    let balance_amt = parse_balance_amount(card.balance.as_deref());
    if card.error.is_some() && !has_windows && balance_amt.is_none() {
        return CardStatus::Unknown;
    }
    if card.kind.as_deref() == Some("balance") || (!has_windows && balance_amt.is_some()) {
        return match balance_amt {
            Some(v) if v > 0.0 => CardStatus::Available,
            Some(_) => CardStatus::Exhausted(format!(
                "{} balance exhausted ({})",
                card.provider,
                card.balance.as_deref().unwrap_or("0")
            )),
            None => CardStatus::Unknown,
        };
    }

    let mut windows: Vec<&QuotaWindow> = card
        .windows
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|w| w.text != Some(true) && w.unlimited != Some(true))
        .filter(|w| {
            let lname = w.name.to_ascii_lowercase();
            !lname.contains("image") && !lname.contains("grok") && !lname.contains("free usage")
        })
        .filter(|w| w.resets_at.as_deref().and_then(parse_iso_ms).is_none_or(|ms| ms > now_ms))
        .collect();

    if windows.is_empty() {
        return if balance_amt.is_some_and(|v| v > 0.0) { CardStatus::Available } else { CardStatus::Unknown };
    }

    // Cursor differentiates native Cursor Models from "Other Models"; don't block Cursor when Cursor Models > 0.
    let agent_lower = agent.trim().to_ascii_lowercase();
    if (agent_lower == "cursor" || agent_lower == "cursor-agent")
        && windows.iter().any(|w| w.name.eq_ignore_ascii_case("Cursor Models") && w.remaining > 0.0)
    {
        windows.retain(|w| !w.name.eq_ignore_ascii_case("Other Models"));
    }

    // Antigravity splits pools between Gemini and Claude/GPT.
    if (agent_lower == "agy" || agent_lower == "antigravity")
        && let Some(m) = model.map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty())
    {
        let wants_claude = m.contains("claude") || m.contains("gpt") || m.contains("opus") || m.contains("sonnet");
        let wants_gemini = m.contains("gemini");
        if wants_claude {
            let claude_wins: Vec<&QuotaWindow> = windows
                .iter()
                .copied()
                .filter(|w| {
                    w.pool.as_deref().is_some_and(|p| p.to_ascii_lowercase().contains("claude"))
                        || w.name.to_ascii_lowercase().contains("claude")
                })
                .collect();
            if !claude_wins.is_empty() {
                windows = claude_wins;
            }
        } else if wants_gemini {
            let gem_wins: Vec<&QuotaWindow> = windows
                .iter()
                .copied()
                .filter(|w| {
                    w.pool.as_deref().is_some_and(|p| p.to_ascii_lowercase().contains("gemini"))
                        || w.name.to_ascii_lowercase().contains("gemini")
                })
                .collect();
            if !gem_wins.is_empty() {
                windows = gem_wins;
            }
        }
    }

    // Sub-bucket aggregate handling (WorkBuddy / Qoder-CN): if Total credits > 0, ignore depleted sub-buckets.
    if windows.iter().any(|w| w.name == "Total credits" && w.remaining > 0.0) {
        windows.retain(|w| !matches!(w.name.as_str(), "Credits" | "Extra credits" | "Add-on credits"));
    }

    if windows.is_empty() {
        return CardStatus::Available;
    }

    let exhausted: Vec<&QuotaWindow> = windows.into_iter().filter(|w| w.remaining <= 0.0).collect();
    if exhausted.is_empty() {
        return CardStatus::Available;
    }
    if balance_amt.is_some_and(|v| v > 0.0) {
        return CardStatus::Available;
    }
    let names = exhausted.iter().map(|w| w.name.as_str()).collect::<Vec<_>>().join("/");
    CardStatus::Exhausted(format!("{} {names} 0%", card.provider))
}

/// Returns a short exhaustion reason iff Magpie shows the agent/model has 0% quota across all active accounts
/// and no positive credit balance. Returns `None` when available or unknown.
pub fn quota_exhausted_reason(agent: &str, model: Option<&str>, cards: &[QuotaCard]) -> Option<String> {
    quota_exhausted_reason_at(agent, model, cards, super::now_ms_u64())
}

pub(crate) fn quota_exhausted_reason_at(
    agent: &str,
    model: Option<&str>,
    cards: &[QuotaCard],
    now_ms: u64,
) -> Option<String> {
    let aliases = provider_aliases_for_agent(agent);
    if aliases.is_empty() || cards.is_empty() {
        return None;
    }
    let matching: Vec<&QuotaCard> =
        cards.iter().filter(|c| aliases.iter().any(|a| c.provider.eq_ignore_ascii_case(a))).collect();
    if matching.is_empty() {
        return None;
    }

    let mut first_exhausted = None;
    for card in matching {
        match evaluate_card(card, agent, model, now_ms) {
            CardStatus::Available => return None,
            CardStatus::Exhausted(reason) => {
                if first_exhausted.is_none() {
                    first_exhausted = Some(reason);
                }
            }
            CardStatus::Unknown => {}
        }
    }
    first_exhausted
}

async fn fetch_quotas_http(gateway: &str) -> Result<Vec<QuotaCard>, String> {
    let url = format!("{gateway}/v1/magpie/quotas?kept=1");
    let cfg = format!("url = \"{}\"\nheader = \"Authorization: Bearer magpie\"\n", escape_curl_str(&url));
    let out = super::bg_tokio_command("curl")
        .args(["-sS", "--max-time", "2", "-w", "\n%{http_code}", "--config", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to run curl: {e}"))?;

    let mut child = out;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(cfg.as_bytes()).await;
    }
    let output = child.wait_with_output().await.map_err(|e| format!("magpie probe failed: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if err.is_empty() { format!("curl exited with {}", output.status) } else { err });
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (body, code_str) = text.rsplit_once('\n').unwrap_or(("", text.trim()));
    let status: u16 = code_str.trim().parse().unwrap_or(0);
    if !(200..300).contains(&status) {
        return Err(format!("magpie HTTP {status}"));
    }
    let val: Value = serde_json::from_str(body).map_err(|e| format!("invalid magpie JSON: {e}"))?;
    let data = val.get("data").cloned().ok_or_else(|| "magpie response missing data array".to_string())?;
    serde_json::from_value(data).map_err(|e| format!("invalid magpie quota card schema: {e}"))
}

fn escape_curl_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disabled_magpie_never_hits_network_and_returns_none() {
        let client = MagpieClient::default();
        let settings = MagpieSettings { enabled: false, gateway_url: Some("http://127.0.0.1:1".into()) };
        let (cards, err) = client.fetch_if_enabled(&settings, true).await;
        assert!(cards.is_none());
        assert!(err.is_none());
    }

    #[test]
    fn discovers_gateway_json_addr_over_settings() {
        let tmp = crate::test_tmp().join(format!("drove-magpie-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("gateway.json"), r#"{"addr":"127.0.0.1:3425","pid":42}"#).unwrap();
        std::fs::write(tmp.join("settings.json"), r#"{"port":7878}"#).unwrap();
        assert_eq!(gateway_from_dir(&tmp).as_deref(), Some("http://127.0.0.1:3425"));
    }

    #[test]
    fn parses_real_magpie_http_and_quotas_file_schemas() {
        let http_json = r#"{
            "provider":"codex",
            "name":"Codex",
            "kind":"subscription",
            "plan":"plus",
            "windows":[{"name":"5 hours","used":100,"remaining":0,"resetsAt":"2026-11-17T20:41:03+08:00"}],
            "balance":"51025.34 credits",
            "readAt":"2026-11-17T20:37:41.7946626+08:00"
        }"#;
        let codex_card: QuotaCard = serde_json::from_str(http_json).unwrap();
        assert_eq!(codex_card.as_of.as_deref(), Some("2026-11-17T20:37:41.7946626+08:00"));
        // Codex has 0% 5-hour window, but positive credits balance -> stays available!
        assert_eq!(quota_exhausted_reason_at("codex", None, &[codex_card], 1_700_000_000_000), None);

        let quotas_file = r#"{
            "claude/a@b.com": {
                "at": "2026-11-17T12:00:00Z",
                "quota": {
                    "provider": "claude",
                    "windows": [{"name": "5 hours", "used": 100, "resetsAt": "2099-01-01T00:00:00Z"}]
                }
            }
        }"#;
        let cards = parse_quotas_file_json(quotas_file).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].windows.as_ref().unwrap()[0].remaining, 0.0);
        assert!(quota_exhausted_reason_at("claude", None, &cards, 1_700_000_000_000).is_some());
    }
}
