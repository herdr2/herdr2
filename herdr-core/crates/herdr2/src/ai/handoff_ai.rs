//! Semantic memory distillation, merge-back takeaway synthesis, and complementary provider
//! recommendation for cross-provider context handoffs (`agent.handoff` / `ai.handoff_recommend`).

use std::collections::BTreeMap;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ai::client::extract_json_object;
use crate::ai::compact::fingerprint_hex;
use crate::ai::prompts::format_op_user_message;
use crate::ai::{AiCompleteRequest, AiConversationMode, AiRuntime, AiTaskTier};
use crate::handoff::{LineageKind, format_turn_ranges};
use crate::transcript::{Entry, strip_h2_envelope};

const MAX_DISTILL_INPUT_BYTES: usize = 14 * 1024;
const MAX_RECOMMEND_INPUT_BYTES: usize = 6 * 1024;
const DISTILL_TIMEOUT: Duration = Duration::from_secs(15);
const RECOMMEND_TIMEOUT: Duration = Duration::from_secs(12);
const DISTILL_CACHE_TTL: Duration = Duration::from_secs(900);
const RECOMMEND_CACHE_TTL: Duration = Duration::from_secs(60);

const DEFAULT_PRESET_POOL: &[&str] = &["claude", "codex", "gemini", "agy", "cursor", "opencode", "qwen", "kimi"];

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HandoffRecommendParams {
    /// Source agent or terminal (`a1`, `t1`, name, or `machine/a1`).
    pub source: String,
    /// Installed target presets available on the source machine (optional).
    #[serde(default)]
    pub installed_presets: Vec<String>,
    /// Caller UI language (`zh` / `zh-CN` / `en`).
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HandoffRecommendResult {
    /// Recommended handoff mode: `"handoff"`, `"fork"`, or `"side-chat"`.
    pub recommended_kind: String,
    /// Recommended complementary target preset (e.g. `"codex"`, `"claude"`, `"gemini"`).
    pub recommended_preset: String,
    /// Short human-readable explanation for the recommendation.
    pub reason: String,
    /// Actionable pre-filled handoff instruction / note.
    pub suggested_note: String,
    pub used_ai: bool,
    pub from_cache: bool,
}

#[derive(Default)]
struct CompactTurn {
    users: Vec<String>,
    failed_tools: Vec<String>,
    ok_tools: Vec<String>,
    errors: Vec<String>,
    assistants: Vec<String>,
}

fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push('…');
    out
}

fn collect_compact_turns(entries: &[Entry], filter_turns: Option<&[u64]>) -> BTreeMap<u64, CompactTurn> {
    let mut map: BTreeMap<u64, CompactTurn> = BTreeMap::new();
    for entry in entries {
        let Some(turn) = entry.turn() else { continue };
        if turn == 0 {
            continue;
        }
        if let Some(allowed) = filter_turns
            && !allowed.contains(&turn)
        {
            continue;
        }
        let bucket = map.entry(turn).or_default();
        match entry {
            Entry::User { text, .. } => {
                let clean = strip_h2_envelope(text);
                let trimmed = clean.trim();
                if !trimmed.is_empty() {
                    bucket.users.push(truncate_utf8(trimmed, 420));
                }
            }
            Entry::ToolCall { title, status, detail, .. } => {
                let t = title.trim();
                if status == "failed" {
                    let mut line = truncate_utf8(t, 160);
                    if let Some(d) = detail.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                        let d_one = d.replace('\n', " ");
                        line.push_str(" — ");
                        line.push_str(&truncate_utf8(&d_one, 180));
                    }
                    bucket.failed_tools.push(line);
                } else if bucket.ok_tools.len() < 6 && !t.is_empty() {
                    bucket.ok_tools.push(truncate_utf8(t, 120));
                }
            }
            Entry::Error { message, .. } => {
                let clean = message.trim();
                if !clean.is_empty() {
                    bucket.errors.push(truncate_utf8(&clean.replace('\n', " "), 220));
                }
            }
            Entry::Assistant { text, .. } => {
                let clean = text.trim();
                if !clean.is_empty() {
                    bucket.assistants.push(truncate_utf8(clean, 520));
                }
            }
            Entry::Thought { .. }
            | Entry::Plan { .. }
            | Entry::Permission { .. }
            | Entry::Stderr { .. }
            | Entry::Checkpoint { .. }
            | Entry::Rewind { .. }
            | Entry::Handoff { .. } => {}
        }
    }
    map
}

fn render_compact_excerpt(turns: &BTreeMap<u64, CompactTurn>, max_bytes: usize) -> String {
    let mut out = String::new();
    for (&turn, b) in turns {
        let mut sec = format!("[Turn {turn}]\n");
        for u in b.users.iter().take(2) {
            sec.push_str("U: ");
            sec.push_str(u);
            sec.push('\n');
        }
        for e in b.errors.iter().take(4) {
            sec.push_str("ERR: ");
            sec.push_str(e);
            sec.push('\n');
        }
        for ft in b.failed_tools.iter().take(4) {
            sec.push_str("FAIL_TOOL: ");
            sec.push_str(ft);
            sec.push('\n');
        }
        if !b.ok_tools.is_empty() {
            sec.push_str("TOOLS: ");
            sec.push_str(&b.ok_tools.join(" | "));
            sec.push('\n');
        }
        if let Some(last_a) = b.assistants.last() {
            sec.push_str("A: ");
            sec.push_str(last_a);
            sec.push('\n');
        }
        if out.len() + sec.len() > max_bytes {
            let rem = max_bytes.saturating_sub(out.len());
            if rem > 64 {
                out.push_str(&truncate_utf8(&sec, rem));
            }
            break;
        }
        out.push_str(&sec);
    }
    out
}

fn json_string_list(val: &Value, key: &str, max_items: usize, max_chars: usize) -> Vec<String> {
    val.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::trim))
                .filter(|s| !s.is_empty())
                .take(max_items)
                .map(|s| s.chars().take(max_chars).collect::<String>())
                .collect()
        })
        .unwrap_or_default()
}

fn json_string_field(val: &Value, key: &str, max_chars: usize) -> Option<String> {
    val.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(max_chars).collect())
}

/// Distills omitted middle turns into a structured Markdown memory block for `<herdr2-handoff>`.
/// Returns `None` gracefully when AI is unconfigured, disabled, circuit-open, or fails.
pub async fn distill_omitted_turns(
    ai: &AiRuntime,
    source_address: &str,
    entries: &[Entry],
    omitted_turns: &[u64],
    is_zh: bool,
) -> Option<String> {
    if omitted_turns.is_empty() || !ai.handoff_distill_enabled() {
        return None;
    }
    let compact = collect_compact_turns(entries, Some(omitted_turns));
    let excerpt = render_compact_excerpt(&compact, MAX_DISTILL_INPUT_BYTES);
    if excerpt.trim().is_empty() {
        return None;
    }

    let lang = if is_zh { "zh" } else { "en" };
    let ranges = format_turn_ranges(omitted_turns);
    let cache_key = format!(
        "handoff_distill:{source_address}:{lang}:{ranges}:{}",
        fingerprint_hex(&[&excerpt])
    );
    let payload = json!({
        "source_address": source_address,
        "omitted_turns": ranges,
        "transcript": excerpt,
    });
    let user_prompt = format_op_user_message("handoff_distill", lang, &payload);
    let resp = ai
        .complete(AiCompleteRequest {
            op: "handoff_distill".into(),
            tier: AiTaskTier::Fast,
            mode: AiConversationMode::StatelessSharedPrefix,
            user_prompt,
            max_tokens: 420,
            timeout: DISTILL_TIMEOUT,
            cache_key: Some(cache_key),
            cache_ttl: Some(DISTILL_CACHE_TTL),
            prompt_cache_bucket: Some("cp".into()),
            ..Default::default()
        })
        .await
        .ok()?;

    let parsed = extract_json_object(&resp.content)?;
    format_distilled_omitted_markdown(&parsed, is_zh)
}

fn format_distilled_omitted_markdown(parsed: &Value, is_zh: bool) -> Option<String> {
    let goal = json_string_field(parsed, "goal_evolution", 280);
    let conventions = json_string_list(parsed, "architecture_conventions", 5, 160);
    let rejected = json_string_list(parsed, "rejected_approaches", 5, 160);
    let blockers = json_string_list(parsed, "current_blockers", 5, 160);
    let pending = json_string_list(parsed, "pending_items", 5, 160);

    if goal.is_none()
        && conventions.is_empty()
        && rejected.is_empty()
        && blockers.is_empty()
        && pending.is_empty()
    {
        return None;
    }

    let mut md = String::new();
    if let Some(g) = goal {
        if is_zh {
            md.push_str(&format!("- **目标演变：** {g}\n"));
        } else {
            md.push_str(&format!("- **Goal Evolution:** {g}\n"));
        }
    }
    let push_list = |dst: &mut String, title: &str, items: &[String]| {
        if items.is_empty() {
            return;
        }
        dst.push_str(&format!("- **{title}：**\n"));
        for item in items {
            dst.push_str(&format!("  - {item}\n"));
        }
    };
    let push_list_en = |dst: &mut String, title: &str, items: &[String]| {
        if items.is_empty() {
            return;
        }
        dst.push_str(&format!("- **{title}:**\n"));
        for item in items {
            dst.push_str(&format!("  - {item}\n"));
        }
    };
    if is_zh {
        push_list(&mut md, "关键架构约定", &conventions);
        push_list(&mut md, "已排除的错误方案（勿重蹈覆辙）", &rejected);
        push_list(&mut md, "当前卡点", &blockers);
        push_list(&mut md, "未完事项", &pending);
    } else {
        push_list_en(&mut md, "Key Architecture Conventions", &conventions);
        push_list_en(&mut md, "Ruled-Out Approaches (Do Not Repeat)", &rejected);
        push_list_en(&mut md, "Current Blockers", &blockers);
        push_list_en(&mut md, "Pending Items", &pending);
    }
    Some(md)
}

/// Distills a side-chat or forked exploration session into a clean takeaway brief for `Merge back`.
/// Returns `None` gracefully when distillation is skipped or unavailable.
pub async fn distill_merge_back(
    ai: &AiRuntime,
    source_address: &str,
    kind: LineageKind,
    entries: &[Entry],
    raw_markdown_bytes: usize,
    is_zh: bool,
) -> Option<String> {
    if !ai.handoff_distill_enabled() {
        return None;
    }
    let compact = collect_compact_turns(entries, None);
    if compact.len() < 2 && raw_markdown_bytes <= 2048 {
        return None;
    }
    let excerpt = render_compact_excerpt(&compact, MAX_DISTILL_INPUT_BYTES);
    if excerpt.trim().is_empty() {
        return None;
    }

    let lang = if is_zh { "zh" } else { "en" };
    let cache_key = format!(
        "handoff_merge:{source_address}:{}:{lang}:{}",
        kind.as_str(),
        fingerprint_hex(&[&excerpt])
    );
    let payload = json!({
        "source_address": source_address,
        "kind": kind.as_str(),
        "transcript": excerpt,
    });
    let user_prompt = format_op_user_message("handoff_merge_digest", lang, &payload);
    let resp = ai
        .complete(AiCompleteRequest {
            op: "handoff_merge_digest".into(),
            tier: AiTaskTier::Fast,
            mode: AiConversationMode::StatelessSharedPrefix,
            user_prompt,
            max_tokens: 420,
            timeout: DISTILL_TIMEOUT,
            cache_key: Some(cache_key),
            cache_ttl: Some(DISTILL_CACHE_TTL),
            prompt_cache_bucket: Some("cp".into()),
            ..Default::default()
        })
        .await
        .ok()?;

    let parsed = extract_json_object(&resp.content)?;
    let summary = json_string_field(&parsed, "summary", 320);
    let decisions = json_string_list(&parsed, "decisions", 5, 160);
    let action_items = json_string_list(&parsed, "action_items", 5, 160);
    let changed_files = json_string_list(&parsed, "changed_files", 8, 120);
    let open_risks = json_string_list(&parsed, "open_risks", 5, 160);

    if summary.is_none() && decisions.is_empty() && action_items.is_empty() {
        return None;
    }

    let mut md = if is_zh {
        format!("# 来自 `{source_address}` 的{}精炼结论\n\n", kind.display_label_zh())
    } else {
        format!("# Distilled Takeaways from `{source_address}` ({})\n\n", kind.display_label())
    };
    if let Some(s) = summary {
        md.push_str(if is_zh { "## 核心结论\n\n" } else { "## Summary\n\n" });
        md.push_str(&s);
        md.push_str("\n\n");
    }
    let write_sec = |dst: &mut String, heading: &str, items: &[String]| {
        if items.is_empty() {
            return;
        }
        dst.push_str(&format!("## {heading}\n\n"));
        for it in items {
            dst.push_str(&format!("- {it}\n"));
        }
        dst.push('\n');
    };
    if is_zh {
        write_sec(&mut md, "已确认决策", &decisions);
        write_sec(&mut md, "后续行动项", &action_items);
        write_sec(&mut md, "涉及文件", &changed_files);
        write_sec(&mut md, "遗留风险与注意事项", &open_risks);
        md.push_str(&format!(
            "---\n如需查看完整旁路讨论记录，可执行：`h2 read {source_address}`。\n"
        ));
    } else {
        write_sec(&mut md, "Decisions", &decisions);
        write_sec(&mut md, "Action Items", &action_items);
        write_sec(&mut md, "Relevant Files", &changed_files);
        write_sec(&mut md, "Open Risks", &open_risks);
        md.push_str(&format!(
            "---\nTo inspect the full raw side-session transcript, run: `h2 read {source_address}`.\n"
        ));
    }
    Some(md)
}

#[derive(Default)]
struct RecentSignals {
    recent_turn_count: usize,
    error_count: usize,
    failed_tool_count: usize,
    ui_hits: usize,
    systems_hits: usize,
    question_hits: usize,
    last_user_snippet: String,
    last_error_snippet: String,
}

fn analyze_recent_signals(turns: &BTreeMap<u64, CompactTurn>) -> RecentSignals {
    let mut sig = RecentSignals { recent_turn_count: turns.len(), ..Default::default() };
    for b in turns.values() {
        sig.error_count += b.errors.len();
        sig.failed_tool_count += b.failed_tools.len();
        if let Some(e) = b.errors.last().or_else(|| b.failed_tools.last()) {
            sig.last_error_snippet = truncate_utf8(e, 140);
        }
        for u in &b.users {
            sig.last_user_snippet = truncate_utf8(u, 120);
            let low = u.to_ascii_lowercase();
            if low.contains("css")
                || low.contains("layout")
                || low.contains("style")
                || low.contains("ui")
                || low.contains("frontend")
                || u.contains("样式")
                || u.contains("界面")
                || u.contains("布局")
                || u.contains("前端")
            {
                sig.ui_hits += 1;
            }
            if low.contains("rust")
                || low.contains("deadlock")
                || low.contains("race")
                || low.contains("panic")
                || low.contains("borrow")
                || low.contains("compile")
                || u.contains("死锁")
                || u.contains("并发")
                || u.contains("排查")
                || u.contains("报错")
            {
                sig.systems_hits += 1;
            }
            if u.contains('?') || u.contains('？') || low.contains("why ") || u.contains("为什么") || u.contains("分析") {
                sig.question_hits += 1;
            }
        }
        for t in b.ok_tools.iter().chain(b.failed_tools.iter()) {
            let low = t.to_ascii_lowercase();
            if low.contains(".tsx") || low.contains(".jsx") || low.contains(".css") || low.contains(".vue") || low.contains(".html") || low.contains("mods/") {
                sig.ui_hits += 1;
            }
            if low.contains(".rs") || low.contains(".go") || low.contains(".cpp") || low.contains("cargo test") {
                sig.systems_hits += 1;
            }
        }
    }
    sig
}

fn normalize_preset(raw: &str) -> Option<String> {
    let low = raw.trim().to_ascii_lowercase();
    if low.is_empty() {
        return None;
    }
    if low.contains("antigravity") || low == "agy" {
        return Some("agy".into());
    }
    for key in ["claude", "codex", "gemini", "cursor", "opencode", "qwen", "kimi", "copilot", "hermes", "glm", "pi"] {
        if low.contains(key) {
            return Some(key.into());
        }
    }
    None
}

fn pick_from_pool(pool: &[String], preferred: &[&str], avoid: Option<&str>) -> String {
    for p in preferred {
        if avoid.is_some_and(|a| a == *p) {
            continue;
        }
        if let Some(found) = pool.iter().find(|x| x.as_str() == *p) {
            return found.clone();
        }
    }
    if let Some(alt) = pool.iter().find(|x| avoid.is_none_or(|a| x.as_str() != a)) {
        return alt.clone();
    }
    pool.first().cloned().unwrap_or_else(|| "claude".into())
}

fn heuristic_recommend(
    src_preset: Option<&str>,
    pool: &[String],
    sig: &RecentSignals,
    is_zh: bool,
) -> HandoffRecommendResult {
    let stuck = sig.error_count + sig.failed_tool_count >= 2;
    if stuck {
        let preset = pick_from_pool(pool, &["codex", "claude", "gemini", "agy"], src_preset);
        let note = if sig.last_error_snippet.is_empty() {
            if is_zh {
                "上游连续遇到报错，请换一种排查思路定位根因并避免重复失败方案。".into()
            } else {
                "Upstream hit repeated failures; diagnose root cause with a fresh approach and avoid dead ends.".into()
            }
        } else if is_zh {
            format!("上游持续报错（{}），请重新审视根因并给出替代修复方案。", truncate_utf8(&sig.last_error_snippet, 72))
        } else {
            format!("Upstream repeatedly failed ({}); re-diagnose root cause with an alternative approach.", truncate_utf8(&sig.last_error_snippet, 72))
        };
        return HandoffRecommendResult {
            recommended_kind: "fork".into(),
            recommended_preset: preset,
            reason: if is_zh {
                "近期连续报错/工具失败，建议换互补推理模型分叉破局".into()
            } else {
                "Repeated errors detected; fork to a complementary reasoning model".into()
            },
            suggested_note: note,
            used_ai: false,
            from_cache: false,
        };
    }

    if sig.ui_hits > sig.systems_hits && sig.ui_hits > 0 {
        let preset = pick_from_pool(pool, &["gemini", "claude", "agy", "cursor"], src_preset);
        return HandoffRecommendResult {
            recommended_kind: "handoff".into(),
            recommended_preset: preset,
            reason: if is_zh {
                "近期聚焦 UI/前端细节，建议交接给界面表现更强的互补模型".into()
            } else {
                "Recent work focuses on UI/frontend polish; hand off to a UI-strong model".into()
            },
            suggested_note: if is_zh {
                "承接当前界面实现，重点打磨交互细节、视觉对齐与边界态体验。".into()
            } else {
                "Continue the UI work with focus on visual polish, alignment, and interaction edge cases.".into()
            },
            used_ai: false,
            from_cache: false,
        };
    }

    if sig.question_hits >= 2 && sig.error_count == 0 && sig.failed_tool_count == 0 {
        let preset = pick_from_pool(pool, &["claude", "gemini", "codex"], src_preset);
        return HandoffRecommendResult {
            recommended_kind: "side-chat".into(),
            recommended_preset: preset,
            reason: if is_zh {
                "近期以方案分析与求证为主，建议开只读旁路讨论避免污染主线".into()
            } else {
                "Recent turns are exploratory questions; open a read-only side chat".into()
            },
            suggested_note: if is_zh {
                "基于当前上下文做只读方案评审与风险核对，暂不修改工作区文件。".into()
            } else {
                "Review the current approach and edge cases read-only without modifying workspace files.".into()
            },
            used_ai: false,
            from_cache: false,
        };
    }

    let preset = pick_from_pool(pool, &["claude", "codex", "gemini", "agy"], src_preset);
    HandoffRecommendResult {
        recommended_kind: "handoff".into(),
        recommended_preset: preset,
        reason: if is_zh {
            "推荐切换到互补模型无缝接力当前进度".into()
        } else {
            "Switch to a complementary provider to continue smoothly".into()
        },
        suggested_note: if sig.last_user_snippet.is_empty() {
            String::new()
        } else if is_zh {
            format!("承接上一轮目标继续推进：{}", truncate_utf8(&sig.last_user_snippet, 80))
        } else {
            format!("Continue from the latest goal: {}", truncate_utf8(&sig.last_user_snippet, 80))
        },
        used_ai: false,
        from_cache: false,
    }
}

/// Recommends a complementary target preset, handoff kind, and pre-filled note from the source
/// session's recent turns. Uses `Core.ai` when available, gracefully falling back to deterministic
/// heuristics when AI is unconfigured, disabled, or circuit-open.
pub async fn recommend_handoff(
    ai: &AiRuntime,
    source_address: &str,
    source_provider: Option<&str>,
    source_model: Option<&str>,
    entries: &[Entry],
    installed_presets: &[String],
    language: Option<&str>,
) -> HandoffRecommendResult {
    let is_zh = language
        .map(str::trim)
        .is_some_and(|l| l.to_ascii_lowercase().starts_with("zh"));
    let src_preset = source_provider.and_then(normalize_preset);

    let mut pool: Vec<String> = Vec::new();
    for raw in installed_presets {
        if let Some(norm) = normalize_preset(raw)
            && !pool.contains(&norm)
        {
            pool.push(norm);
        }
    }
    if pool.is_empty() {
        pool = DEFAULT_PRESET_POOL.iter().map(|s| (*s).to_string()).collect();
    }

    let all_turns = collect_compact_turns(entries, None);
    let recent_keys: Vec<u64> = all_turns.keys().rev().take(4).copied().collect();
    let mut recent_turns: BTreeMap<u64, CompactTurn> = BTreeMap::new();
    for k in recent_keys.into_iter().rev() {
        if let Some(v) = all_turns.get(&k) {
            recent_turns.insert(
                k,
                CompactTurn {
                    users: v.users.clone(),
                    failed_tools: v.failed_tools.clone(),
                    ok_tools: v.ok_tools.clone(),
                    errors: v.errors.clone(),
                    assistants: v.assistants.clone(),
                },
            );
        }
    }

    let sig = analyze_recent_signals(&recent_turns);
    let fallback = heuristic_recommend(src_preset.as_deref(), &pool, &sig, is_zh);

    if !ai.is_configured() || ai.is_circuit_open() || recent_turns.is_empty() {
        return fallback;
    }

    let excerpt = render_compact_excerpt(&recent_turns, MAX_RECOMMEND_INPUT_BYTES);
    if excerpt.trim().is_empty() {
        return fallback;
    }

    let lang = if is_zh { "zh" } else { "en" };
    let pool_key = pool.join(",");
    let cache_key = format!(
        "handoff_rec:{source_address}:{lang}:{pool_key}:{}",
        fingerprint_hex(&[source_provider.unwrap_or(""), &excerpt])
    );
    let payload = json!({
        "source_provider": src_preset.as_deref().or(source_provider).unwrap_or("unknown"),
        "source_model": source_model.unwrap_or(""),
        "installed_presets": pool,
        "recent_signals": {
            "turns": sig.recent_turn_count,
            "errors": sig.error_count,
            "failed_tools": sig.failed_tool_count,
            "ui_hits": sig.ui_hits,
            "systems_hits": sig.systems_hits,
        },
        "recent_turns": excerpt,
    });
    let user_prompt = format_op_user_message("handoff_recommend", lang, &payload);
    let Ok(resp) = ai
        .complete(AiCompleteRequest {
            op: "handoff_recommend".into(),
            tier: AiTaskTier::Nano,
            mode: AiConversationMode::StatelessSharedPrefix,
            user_prompt,
            max_tokens: 512,
            timeout: RECOMMEND_TIMEOUT,
            cache_key: Some(cache_key),
            cache_ttl: Some(RECOMMEND_CACHE_TTL),
            prompt_cache_bucket: Some("cp".into()),
            ..Default::default()
        })
        .await
    else {
        return fallback;
    };

    let Some(parsed) = extract_json_object(&resp.content) else {
        return fallback;
    };
    let kind = match parsed["recommended_kind"].as_str().map(str::trim) {
        Some("handoff") => "handoff".to_string(),
        Some("fork") => "fork".to_string(),
        Some("side-chat") | Some("side_chat") => "side-chat".to_string(),
        _ => fallback.recommended_kind.clone(),
    };
    let preset = parsed["recommended_preset"]
        .as_str()
        .and_then(normalize_preset)
        .filter(|p| pool.contains(p))
        .unwrap_or_else(|| fallback.recommended_preset.clone());
    let reason = json_string_field(&parsed, "reason", 100).unwrap_or(fallback.reason);
    let suggested_note = json_string_field(&parsed, "suggested_note", 180).unwrap_or(fallback.suggested_note);

    HandoffRecommendResult {
        recommended_kind: kind,
        recommended_preset: preset,
        reason,
        suggested_note,
        used_ai: true,
        from_cache: resp.from_local_cache,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::AiSettingsSetParams;
    use crate::paths::Paths;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn mock_openai(responses: Vec<String>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(responses)));
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let q = queue.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 8192];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]);
                    if head.starts_with("GET ") && head.contains("/models") {
                        let body = r#"{"data":[{"id":"fast-model"}]}"#;
                        let http = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = sock.write_all(http.as_bytes()).await;
                        return;
                    }
                    let content = q.lock().unwrap().pop_front().unwrap_or_else(|| "{}".into());
                    let body = json!({"choices": [{"message": {"content": content}}]}).to_string();
                    let http = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(http.as_bytes()).await;
                });
            }
        });
        format!("http://127.0.0.1:{port}/v1")
    }

    fn temp_runtime() -> (Arc<AiRuntime>, std::path::PathBuf) {
        let tmp = crate::test_tmp().join(format!("h2-ho-ai-{}", crate::platform::random_hex()));
        let paths = Paths {
            socket: tmp.join("core.sock"),
            holder_socket: tmp.join("holder.sock"),
            state: tmp.join("state"),
            config: tmp.join("config/config.toml"),
        };
        paths.create_dirs().unwrap();
        (AiRuntime::open(&paths).unwrap(), tmp)
    }

    #[tokio::test]
    async fn distills_omitted_turns_merge_back_and_recommends_with_fallback() {
        let (rt, tmp) = temp_runtime();
        let entries = vec![
            Entry::User { turn: 1, from: None, text: "Implement lock-free cache".into(), attachments: vec![], ts_ms: 1 },
            Entry::Error { turn: 2, message: "deadlock in storage mutex".into(), ts_ms: 2 },
            Entry::ToolCall {
                turn: 2,
                id: "c1".into(),
                title: "cargo test".into(),
                tool_kind: None,
                status: "failed".into(),
                detail: Some("test auth_lock timed out".into()),
                attachments: vec![],
                ts_ms: 3,
            },
            Entry::Assistant { turn: 2, text: "Mutex ordering caused deadlock; switched to RwLock.".into(), attachments: vec![], ts_ms: 4 },
            Entry::User { turn: 3, from: None, text: "Continue fixing timeout".into(), attachments: vec![], ts_ms: 5 },
            Entry::Error { turn: 3, message: "still failing test auth_lock".into(), ts_ms: 6 },
        ];

        // Unconfigured AI -> heuristic fallback recommends fork to complementary provider (codex)
        let fb = recommend_handoff(
            &rt,
            "a1",
            Some("claude"),
            None,
            &entries,
            &["claude".into(), "codex".into(), "gemini".into()],
            Some("zh-CN"),
        )
        .await;
        assert!(!fb.used_ai);
        assert_eq!(fb.recommended_kind, "fork");
        assert_eq!(fb.recommended_preset, "codex");
        assert!(!fb.suggested_note.is_empty());

        // Configured AI -> structured distillation, merge digest, and AI recommendation
        let url = mock_openai(vec![
            r#"{"goal_evolution":"从单锁重构为读写锁","architecture_conventions":["禁止持锁 await"],"rejected_approaches":["全局 Mutex 导致死锁"],"current_blockers":["auth_lock 超时"],"pending_items":["补充并发单测"]}"#.into(),
            r#"{"summary":"已确认死锁根因并确定读写锁方案","decisions":["拆分锁粒度"],"action_items":["修复 auth.rs"],"changed_files":["src/auth.rs"],"open_risks":["需跑回归测试"]}"#.into(),
            r#"{"recommended_kind":"fork","recommended_preset":"codex","reason":"逻辑卡死推荐强推理模型分叉排查","suggested_note":"重点排查 auth_lock 死锁路径，避免回退到全局 Mutex"}"#.into(),
        ])
        .await;
        rt.update_settings(AiSettingsSetParams {
            enabled: Some(true),
            base_url: Some(url),
            model: Some("fast-model".into()),
            ..Default::default()
        })
        .await
        .unwrap();

        let distilled = distill_omitted_turns(&rt, "a1", &entries, &[2], true).await.unwrap();
        assert!(distilled.contains("从单锁重构为读写锁"));
        assert!(distilled.contains("全局 Mutex 导致死锁"));
        assert!(distilled.contains("禁止持锁 await"));

        let merged = distill_merge_back(&rt, "a2", LineageKind::SideChat, &entries, 4096, true)
            .await
            .unwrap();
        assert!(merged.contains("已确认死锁根因"));
        assert!(merged.contains("h2 read a2"));

        let rec = recommend_handoff(
            &rt,
            "a1",
            Some("claude"),
            Some("claude-sonnet-4"),
            &entries,
            &["claude".into(), "codex".into(), "gemini".into()],
            Some("zh-CN"),
        )
        .await;
        assert!(rec.used_ai);
        assert_eq!(rec.recommended_kind, "fork");
        assert_eq!(rec.recommended_preset, "codex");
        assert!(rec.reason.contains("推理"));
        assert!(rec.suggested_note.contains("auth_lock"));

        // Second identical recommendation hits 0-token local cache
        let rec2 = recommend_handoff(
            &rt,
            "a1",
            Some("claude"),
            Some("claude-sonnet-4"),
            &entries,
            &["claude".into(), "codex".into(), "gemini".into()],
            Some("zh-CN"),
        )
        .await;
        assert!(rec2.from_cache);

        let _ = std::fs::remove_dir_all(tmp);
    }
}
