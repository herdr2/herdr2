//! Adaptive semantic naming and zero-click hover micro-summary for ACP & PTY agents.
//!
//! Always produces an immediate 0ms deterministic baseline from structured transcripts or
//! terminal metadata, then optionally refines via Core AI (`op = "agent_digest"`, `AiTaskTier::Nano`)
//! sharing the byte-immutable `HERDR2_CONTROL_PLANE_SYSTEM` KV-cache prefix.

use super::client::extract_json_object;
use super::compact::{fingerprint_hex, strip_and_dedup_lines};
use super::prompts::format_op_user_message;
use super::{AiCompleteRequest, AiConversationMode, AiRuntime, AiTaskTier};
use crate::permissions::PublicPermission;
use crate::transcript::Entry;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

const DIGEST_CACHE_TTL: Duration = Duration::from_secs(300);
const DIGEST_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SummarizeAgentParams {
    /// Target agent or terminal address (`a1`, `t1`, name, or `machine/a1`).
    pub target: String,
    /// Preferred UI language (`zh-CN` / `en`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Optional client-inferred status (`working`, `blocked`, `done`, `idle`) for non-7501 PTY CLIs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Skip AI call and return only deterministic summary when false (default true).
    #[serde(default = "default_true")]
    pub include_ai: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentDigestResult {
    pub address: String,
    /// Concise semantic task title (<= 28 chars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_title: Option<String>,
    /// One-sentence real-time progress micro-summary (<= 96 chars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub micro_summary: Option<String>,
    pub used_ai: bool,
    #[serde(default)]
    pub from_cache: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeterministicDigest {
    pub semantic_title: Option<String>,
    pub micro_summary: Option<String>,
    /// Initial conversation task hint (first meaningful user prompt).
    pub initial_task_hint: String,
    /// Latest substantive user intent (skipping trivial follow-up commands when earlier recent turns exist).
    pub task_hint: String,
    /// Chronological list of the user's recent few turns (up to 5, oldest -> newest).
    pub recent_user_turns: Vec<String>,
    pub recent_steps: Vec<String>,
}

/// Returns true when text contains CJK Unified Ideographs (used for language auto-detection when unspecified).
pub fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c))
}

/// Returns true when recent transcript entries contain CJK text.
pub fn entries_prefer_zh(entries: &[Entry]) -> bool {
    entries.iter().rev().take(12).any(|e| match e {
        Entry::User { text, .. } | Entry::Assistant { text, .. } => has_cjk(text),
        Entry::ToolCall { title, .. } => has_cjk(title),
        Entry::Plan { items, .. } => items.iter().any(|i| has_cjk(i)),
        Entry::Error { message, .. } => has_cjk(message),
        _ => false,
    })
}

/// Strips leading `<h2 ...>...</h2` / `<h2 ...>` wrapper tags and normalizes whitespace.
pub fn strip_h2_envelope(raw: &str) -> String {
    let mut s = raw.trim();
    if s.starts_with("<h2 ")
        && let Some(end) = s.find('>')
    {
        s = &s[end + 1..];
        if let Some(close) = s.rfind("</h2>") {
            s = &s[..close];
        }
    }
    s.trim().to_string()
}

pub(crate) fn clean_bounded(raw: Option<&str>, max_chars: usize) -> Option<String> {
    let s = raw?.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("null") || s.eq_ignore_ascii_case("none") {
        return None;
    }
    let trimmed = s.trim_matches(|c| matches!(c, '"' | '\'' | '`' | '“' | '”'));
    let clean: String = trimmed
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if clean.is_empty() {
        return None;
    }
    let count = clean.chars().count();
    if count <= max_chars {
        Some(clean)
    } else {
        let mut out: String = clean.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        Some(out)
    }
}

fn first_meaningful_line(raw: &str) -> Option<String> {
    for line in raw.lines() {
        let trimmed = line.trim().trim_start_matches(['#', '-', '*', '>', ' ']).trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("---") || trimmed.starts_with("===") {
            continue;
        }
        return Some(trimmed.to_string());
    }
    None
}

const MAX_RECENT_USER_TURNS: usize = 5;

/// Returns true when a short user utterance is merely a generic confirmation, continuation,
/// or build/test/install follow-up that advances the preceding task rather than introducing a new intent.
pub(crate) fn is_followup_meta_prompt(raw: &str) -> bool {
    let trimmed = raw.trim().trim_end_matches(['.', '!', '?', '。', '！', '？', '~', '～', ' ']);
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "ok" | "okay"
            | "yes"
            | "y"
            | "go"
            | "go ahead"
            | "continue"
            | "proceed"
            | "retry"
            | "try again"
            | "lgtm"
            | "looks good"
            | "sure"
            | "thanks"
            | "thank you"
            | "thx"
            | "build"
            | "rebuild"
            | "install"
            | "build and install"
            | "compile"
            | "test"
            | "run tests"
            | "run test"
            | "commit"
            | "push"
            | "git push"
            | "好"
            | "好的"
            | "可以"
            | "可以的"
            | "行"
            | "行的"
            | "嗯"
            | "是"
            | "是的"
            | "对"
            | "继续"
            | "继续吧"
            | "开始"
            | "开始吧"
            | "执行"
            | "执行吧"
            | "确认"
            | "同意"
            | "没问题"
            | "重试"
            | "再试一次"
            | "再来"
            | "谢谢"
            | "编译"
            | "重新编译"
            | "编译一下"
            | "安装"
            | "编译安装"
            | "编译安装一下"
            | "编译安装最新版"
            | "编译安装最新版本"
            | "编译安装最新的版本"
            | "编译并安装"
            | "编译并安装最新版"
            | "编译并安装最新版本"
            | "测试"
            | "测试一下"
            | "跑测试"
            | "跑下测试"
            | "跑一下测试"
            | "运行测试"
            | "提交"
            | "提交代码"
            | "推送"
            | "推送代码"
    ) {
        return true;
    }
    false
}

/// Pure, 0ms deterministic digest derived from structured ACP/PTY transcript entries.
/// Captures the user's recent few turns (`recent_user_turns`) and latest execution steps so
/// `semantic_title` reflects the true intent of recent conversation turns rather than Turn 1
/// or trivial follow-ups like "continue" / "build & install".
pub fn build_deterministic_acp_digest(
    status: &str,
    turn: u64,
    pending_permission: Option<&PublicPermission>,
    entries: &[Entry],
    existing_title: Option<&str>,
    is_zh: bool,
) -> DeterministicDigest {
    let mut initial_task_hint = String::new();
    let mut all_user_turns: Vec<String> = Vec::new();
    for entry in entries {
        if let Entry::User { text, .. } = entry {
            let clean = strip_h2_envelope(text);
            if let Some(line) = first_meaningful_line(&clean) {
                if initial_task_hint.is_empty() {
                    initial_task_hint = line.clone();
                }
                if let Some(bounded) = clean_bounded(Some(&line), 120) {
                    all_user_turns.push(bounded);
                }
            }
        }
    }
    let recent_user_turns: Vec<String> = if all_user_turns.len() > MAX_RECENT_USER_TURNS {
        all_user_turns.split_off(all_user_turns.len() - MAX_RECENT_USER_TURNS)
    } else {
        all_user_turns
    };
    let latest_raw_user = recent_user_turns.last().cloned().unwrap_or_default();
    let latest_substantive_user = recent_user_turns
        .iter()
        .rev()
        .find(|u| !is_followup_meta_prompt(u))
        .cloned()
        .unwrap_or_else(|| latest_raw_user.clone());
    let mut task_hint =
        if !latest_substantive_user.is_empty() { latest_substantive_user.clone() } else { initial_task_hint.clone() };
    if task_hint.is_empty()
        && let Some(et) = existing_title.map(str::trim).filter(|s| !s.is_empty())
    {
        task_hint = et.to_string();
    }

    let semantic_title =
        clean_bounded((!latest_substantive_user.is_empty()).then_some(latest_substantive_user.as_str()), 22)
            .or_else(|| existing_title.and_then(|s| clean_bounded(Some(s), 24)))
            .or_else(|| clean_bounded((!task_hint.is_empty()).then_some(task_hint.as_str()), 22));

    let mut recent_steps: Vec<String> = Vec::new();
    let mut latest_user_idx: Option<usize> = None;
    let mut latest_tool: Option<(usize, String)> = None;
    let mut latest_plan: Option<(usize, String)> = None;
    let mut latest_assistant: Option<(usize, String)> = None;
    let mut latest_error: Option<(usize, String)> = None;
    let mut latest_checkpoint_files: Option<(usize, usize)> = None;

    let tail: Vec<&Entry> = entries.iter().rev().take(24).collect::<Vec<_>>().into_iter().rev().collect();
    for (idx, entry) in tail.into_iter().enumerate() {
        match entry {
            Entry::User { text, .. } => {
                let c = strip_h2_envelope(text);
                if let Some(line) = first_meaningful_line(&c)
                    && let Some(b) = clean_bounded(Some(&line), 90)
                {
                    latest_user_idx = Some(idx);
                    recent_steps.push(format!("user: {b}"));
                }
            }
            Entry::ToolCall { title, status: ts, .. } => {
                if let Some(b) = clean_bounded(Some(title), 72) {
                    latest_tool = Some((idx, b.clone()));
                    recent_steps.push(format!("tool({ts}): {b}"));
                }
            }
            Entry::Plan { items, .. } => {
                if let Some(first) = items.iter().find(|i| !i.trim().is_empty())
                    && let Some(b) = clean_bounded(Some(first), 72)
                {
                    latest_plan = Some((idx, b.clone()));
                    recent_steps.push(format!("plan: {b}"));
                }
            }
            Entry::Assistant { text, .. } => {
                if let Some(line) = first_meaningful_line(text)
                    && let Some(b) = clean_bounded(Some(&line), 90)
                {
                    latest_assistant = Some((idx, b.clone()));
                    recent_steps.push(format!("assistant: {b}"));
                }
            }
            Entry::Error { message, .. } => {
                if let Some(line) = first_meaningful_line(message)
                    && let Some(b) = clean_bounded(Some(&line), 90)
                {
                    latest_error = Some((idx, b.clone()));
                    recent_steps.push(format!("error: {b}"));
                }
            }
            Entry::Permission { title, outcome, .. } => {
                if let Some(b) = clean_bounded(Some(title), 72) {
                    recent_steps.push(format!("perm({outcome}): {b}"));
                }
            }
            Entry::Checkpoint { files, .. } if !files.is_empty() => {
                latest_checkpoint_files = Some((idx, files.len()));
            }
            _ => {}
        }
    }
    if recent_steps.len() > 10 {
        recent_steps = recent_steps.split_off(recent_steps.len() - 10);
    }

    let user_floor = latest_user_idx.unwrap_or(0);
    let current_turn_tool = latest_tool.as_ref().filter(|(i, _)| *i >= user_floor).map(|(_, s)| s.as_str());
    let current_turn_plan = latest_plan.as_ref().filter(|(i, _)| *i >= user_floor).map(|(_, s)| s.as_str());
    let current_turn_assistant = latest_assistant.as_ref().filter(|(i, _)| *i >= user_floor);
    let tool_after_assistant = match (&latest_tool, &latest_assistant) {
        (Some((ti, ts)), Some((ai, _))) if ti > ai && *ti >= user_floor => Some(ts.as_str()),
        (Some((ti, ts)), None) if *ti >= user_floor => Some(ts.as_str()),
        _ => None,
    };

    let micro_summary = if let Some(perm) = pending_permission {
        let label =
            clean_bounded(Some(&perm.title), 64).or_else(|| perm.kind.clone()).unwrap_or_else(|| "permission".into());
        Some(if is_zh { format!("等待审批：{label}") } else { format!("Waiting approval: {label}") })
    } else if status == "error" {
        let err = latest_error
            .map(|(_, e)| e)
            .unwrap_or_else(|| if is_zh { "执行异常".into() } else { "Execution error".into() });
        clean_bounded(Some(&if is_zh { format!("异常：{err}") } else { format!("Error: {err}") }), 90)
    } else if status == "working" {
        if let Some(tool) = current_turn_tool {
            clean_bounded(
                Some(&if is_zh {
                    format!("执行中：{tool}（第 {} 轮）", turn.max(1))
                } else {
                    format!("Running: {tool} (turn {})", turn.max(1))
                }),
                90,
            )
        } else if let Some(plan) = current_turn_plan {
            clean_bounded(Some(&if is_zh { format!("计划推进：{plan}") } else { format!("Plan: {plan}") }), 90)
        } else if !latest_raw_user.is_empty() {
            clean_bounded(
                Some(&if is_zh {
                    format!("处理中：{latest_raw_user}")
                } else {
                    format!("Working on: {latest_raw_user}")
                }),
                90,
            )
        } else {
            clean_bounded(
                Some(&if is_zh {
                    format!("思考与处理中（第 {} 轮）", turn.max(1))
                } else {
                    format!("Working on turn {}", turn.max(1))
                }),
                90,
            )
        }
    } else if let Some(tool) = tool_after_assistant {
        clean_bounded(Some(&if is_zh { format!("执行中：{tool}") } else { format!("Running: {tool}") }), 90)
    } else if let Some((ai, ans)) = current_turn_assistant {
        let suffix = latest_checkpoint_files
            .filter(|(ci, _)| *ci >= *ai || *ci >= user_floor)
            .map(|(_, n)| if is_zh { format!("（改动 {n} 个文件）") } else { format!(" ({n} files changed)") });
        let combined = match suffix {
            Some(s) => format!("{ans}{s}"),
            None => ans.clone(),
        };
        clean_bounded(Some(&combined), 90)
    } else if !latest_raw_user.is_empty() {
        clean_bounded(Some(&latest_raw_user), 90)
    } else if let Some((_, ans)) = latest_assistant {
        clean_bounded(Some(&ans), 90)
    } else if !task_hint.is_empty() {
        clean_bounded(Some(&task_hint), 90)
    } else {
        None
    };

    DeterministicDigest { semantic_title, micro_summary, initial_task_hint, task_hint, recent_user_turns, recent_steps }
}

fn is_auto_program_label(lower: &str) -> bool {
    matches!(
        lower,
        "terminal" | "shell" | "zsh" | "bash" | "sh" | "fish" | "nu" | "nushell" | "dash"
            | "pwsh" | "powershell" | "powershell 7" | "windows powershell" | "cmd"
            | "claude" | "claude code" | "claude-code" | "codex" | "gemini" | "gemini-cli"
            | "antigravity" | "agy" | "agy.real" | "opencode" | "cursor" | "cursor-agent"
            | "pi" | "omp" | "oh-my-pi" | "kimi" | "kimi-code" | "qwen" | "qwen-code"
            | "kiro" | "kiro-cli" | "qoder" | "qodercli" | "copilot" | "github-copilot"
            | "amp" | "grok" | "hermes" | "goose" | "crush" | "dsh" | "dsh-tui"
            | "zcode" | "zcode-cli" | "glm" | "agent"
    )
}

fn is_launch_command_title(lower: &str, program: Option<&str>) -> bool {
    let mut parts = lower.split_whitespace();
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(second) = parts.next() else {
        return false;
    };
    if !second.starts_with('-') {
        return false;
    }
    let base = first.rsplit(['/', '\\']).next().unwrap_or(first);
    let stem = base.strip_suffix(".exe").unwrap_or(base);
    let clean = stem.strip_suffix(".real").unwrap_or(stem);
    if is_auto_program_label(clean) {
        return true;
    }
    program
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .is_some_and(|p| clean == p || clean == p.strip_suffix(".real").unwrap_or(&p))
}

fn clean_telling_osc_title(raw: Option<&str>, program: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let stripped_glyphs = raw.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '~' && c != '/' && c != '[' && c != '(');
    if stripped_glyphs.starts_with("~/")
        || stripped_glyphs.starts_with('/')
        || (stripped_glyphs.len() >= 3
            && stripped_glyphs.as_bytes()[0].is_ascii_alphabetic()
            && stripped_glyphs.as_bytes()[1] == b':'
            && stripped_glyphs.as_bytes()[2] == b'\\')
    {
        return None;
    }
    let cleaned = stripped_glyphs
        .strip_prefix("[ ! ] ")
        .or_else(|| stripped_glyphs.strip_prefix("[ . ] "))
        .unwrap_or(stripped_glyphs)
        .trim();
    if cleaned.is_empty() || cleaned.contains('@') {
        return None;
    }
    let lower = cleaned.to_ascii_lowercase();
    if let Some(p) = program.map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty())
        && (lower == p || lower == p.strip_suffix(".real").unwrap_or(&p))
    {
        return None;
    }
    if is_auto_program_label(&lower) || is_launch_command_title(&lower, program) {
        return None;
    }
    clean_bounded(Some(cleaned), 24)
}

/// Fallback deterministic digest from raw terminal screen text when a CLI agent has no structured
/// transcript file yet (e.g., opencode, cursor-agent, copilot, or pre-flush sessions).
pub fn build_deterministic_screen_digest(screen_text: &str, existing_title: Option<&str>) -> DeterministicDigest {
    build_deterministic_screen_digest_with_title(screen_text, existing_title, None, None)
}

/// Like [`build_deterministic_screen_digest`], also considering the terminal's live OSC title
/// so CLIs without structured transcript files still refresh `semantic_title` when their live
/// task/command title updates.
pub fn build_deterministic_screen_digest_with_title(
    screen_text: &str,
    existing_title: Option<&str>,
    live_title: Option<&str>,
    program: Option<&str>,
) -> DeterministicDigest {
    let cleaned = strip_and_dedup_lines(screen_text, 16, 120, 1100);
    let recent_steps: Vec<String> =
        cleaned.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| format!("screen: {l}")).collect();
    let telling_live = clean_telling_osc_title(live_title, program);
    let semantic_title = telling_live.clone().or_else(|| existing_title.and_then(|s| clean_bounded(Some(s), 24)));
    let task_hint = telling_live
        .unwrap_or_else(|| existing_title.unwrap_or("").trim().to_string());
    DeterministicDigest {
        semantic_title,
        micro_summary: None,
        initial_task_hint: String::new(),
        task_hint,
        recent_user_turns: Vec::new(),
        recent_steps,
    }
}

/// Refines a deterministic agent digest using the shared Core AI runtime (`op = "agent_digest"`).
pub async fn refine_agent_digest(
    ai: &AiRuntime,
    address: &str,
    program: &str,
    status: &str,
    det: &DeterministicDigest,
    existing_title: Option<&str>,
    language: Option<&str>,
) -> AgentDigestResult {
    let cfg = ai.config_snapshot();
    let fallback = AgentDigestResult {
        address: address.to_string(),
        semantic_title: det.semantic_title.clone(),
        micro_summary: det.micro_summary.clone(),
        used_ai: false,
        from_cache: false,
    };
    if !cfg.enabled
        || !cfg.semantic_summary
        || cfg.base_url.trim().is_empty()
        || ai.is_circuit_open()
        || (det.task_hint.is_empty() && det.recent_user_turns.is_empty() && det.recent_steps.is_empty())
    {
        return fallback;
    }

    let lang = match language {
        Some(l) if l.to_ascii_lowercase().starts_with("zh") => "zh",
        Some(_) => "en",
        None => {
            if has_cjk(&det.task_hint)
                || det.recent_user_turns.iter().any(|s| has_cjk(s))
                || det.recent_steps.iter().any(|s| has_cjk(s))
            {
                "zh"
            } else {
                "en"
            }
        }
    };
    let numbered_user_turns: Vec<String> =
        det.recent_user_turns.iter().enumerate().map(|(i, u)| format!("{}. {u}", i + 1)).collect();
    let user_turns_joined = if numbered_user_turns.is_empty() {
        strip_and_dedup_lines(&det.task_hint, 2, 140, 240)
    } else {
        strip_and_dedup_lines(&numbered_user_turns.join("\n"), MAX_RECENT_USER_TURNS, 140, 720)
    };
    let steps_joined = strip_and_dedup_lines(&det.recent_steps.join("\n"), 10, 140, 1100);
    let existing = existing_title.unwrap_or("").trim();
    let cache_key =
        format!("agent_digest:v2:{lang}:{program}:{status}:{}", fingerprint_hex(&[&user_turns_joined, &steps_joined]));

    let payload = json!({
        "program": program,
        "status": status,
        "recent_user_turns": numbered_user_turns,
        "existing_title": if existing.is_empty() { serde_json::Value::Null } else { json!(existing) },
        "recent_steps": steps_joined,
    });
    let user_prompt = format_op_user_message("agent_digest", lang, &payload);

    let Some(resp) = ai
        .complete_bg(AiCompleteRequest {
            op: "agent_digest".into(),
            tier: AiTaskTier::Nano,
            mode: AiConversationMode::StatelessSharedPrefix,
            user_prompt,
            max_tokens: 512,
            timeout: DIGEST_TIMEOUT,
            cache_key: Some(cache_key),
            cache_ttl: Some(DIGEST_CACHE_TTL),
            prompt_cache_bucket: Some("cp".into()),
            ..Default::default()
        })
        .await
    else {
        return fallback;
    };

    let Some(parsed) = extract_json_object(&resp.content) else {
        if !resp.from_local_cache {
            ai.record_parse_error("agent_digest");
        }
        return fallback;
    };
    let semantic_title = clean_bounded(parsed["semantic_title"].as_str(), 24).or(fallback.semantic_title);
    let micro_summary = clean_bounded(parsed["micro_summary"].as_str(), 92).or(fallback.micro_summary);
    AgentDigestResult {
        address: address.to_string(),
        semantic_title,
        micro_summary,
        used_ai: true,
        from_cache: resp.from_local_cache,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommitMessageParams {
    /// Target remote machine (`--on`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    /// Git working directory to inspect (defaults to core cwd).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Preferred UI language (`zh-CN` / `en`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Skip AI call and return deterministic fallback when false (default true).
    #[serde(default = "default_true")]
    pub include_ai: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommitMessageResult {
    /// Suggested single-line Git commit message.
    pub message: String,
    pub used_ai: bool,
    #[serde(default)]
    pub from_cache: bool,
}

struct GitDiffSnapshot {
    branch: String,
    files: Vec<String>,
    diff_stat: String,
    diff_excerpt: String,
    recent_commits: Vec<String>,
}

fn is_low_signal_diff_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(&lower);
    matches!(
        base,
        "cargo.lock"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "bun.lock"
            | "bun.lockb"
            | "poetry.lock"
            | "go.sum"
            | "composer.lock"
    ) || lower.ends_with(".min.js")
        || lower.ends_with(".min.css")
        || lower.ends_with(".map")
        || lower.ends_with(".svg")
}

/// Computes an adaptive diff excerpt character budget for `commit_message` based on configured context window.
pub fn effective_commit_diff_budget(ctx_tokens: Option<u32>) -> usize {
    match ctx_tokens {
        None => 10_000,
        Some(c) if c >= 32_768 => 10_000,
        Some(c) if c >= 16_384 => 6_000,
        Some(_) => 3_200,
    }
}

fn prioritize_non_lockfile_diff(raw_diff: &str) -> String {
    if !raw_diff.contains("diff --git ") {
        return raw_diff.to_string();
    }
    let mut high: Vec<String> = Vec::new();
    let mut low: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_path = String::new();
    let flush = |chunk: &mut String, path: &mut String, high: &mut Vec<String>, low: &mut Vec<String>| {
        if chunk.trim().is_empty() {
            chunk.clear();
            path.clear();
            return;
        }
        if is_low_signal_diff_path(path) {
            low.push(std::mem::take(chunk));
        } else {
            high.push(std::mem::take(chunk));
        }
        path.clear();
    };
    for line in raw_diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush(&mut cur, &mut cur_path, &mut high, &mut low);
            cur_path =
                rest.split_once(" b/").map(|(_, b)| b.trim().to_string()).unwrap_or_else(|| rest.trim().to_string());
        }
        cur.push_str(line);
        cur.push('\n');
    }
    flush(&mut cur, &mut cur_path, &mut high, &mut low);
    high.extend(low);
    high.join("")
}

fn sample_untracked_files(cwd: &str, untracked_paths: &[String]) -> String {
    const MAX_FILES: usize = 5;
    const MAX_FILE_BYTES: u64 = 64 * 1024;
    const MAX_LINES_PER_FILE: usize = 40;
    let mut out = String::new();
    let mut sampled = 0usize;
    for rel in untracked_paths {
        if sampled >= MAX_FILES {
            break;
        }
        if is_low_signal_diff_path(rel) {
            continue;
        }
        let full = std::path::Path::new(cwd).join(rel);
        let Ok(meta) = std::fs::metadata(&full) else {
            continue;
        };
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&full) else {
            continue;
        };
        // Skip binary files.
        if bytes.iter().take(1024).any(|&b| b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        out.push_str(&format!("+++ untracked/{rel}\n"));
        for line in text.lines().take(MAX_LINES_PER_FILE) {
            out.push('+');
            out.push_str(line);
            out.push('\n');
        }
        sampled += 1;
    }
    out
}

fn run_git_ro(cwd: &str, args: &[&str]) -> Option<String> {
    let out = crate::platform::bg_command("git")
        .arg("--no-optional-locks")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if out.status.success() { Some(String::from_utf8_lossy(&out.stdout).to_string()) } else { None }
}

fn collect_git_diff_snapshot(cwd: &str, diff_budget_chars: usize) -> Result<GitDiffSnapshot, String> {
    let status =
        run_git_ro(cwd, &["status", "--porcelain=v1", "-b"]).ok_or_else(|| format!("not a git repository: {cwd}"))?;
    let mut branch = String::new();
    let mut all_files = Vec::new();
    let mut untracked_paths = Vec::new();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            branch = rest.split("...").next().unwrap_or(rest).trim().to_string();
        } else if !line.trim().is_empty() {
            if let Some(path) = line.strip_prefix("?? ") {
                let clean = path.trim().trim_matches('"').to_string();
                if !clean.is_empty() && !clean.ends_with('/') {
                    untracked_paths.push(clean);
                }
            }
            if all_files.len() < 40 {
                all_files.push(line.to_string());
            }
        }
    }
    if all_files.is_empty() {
        return Err("no uncommitted changes".into());
    }

    let staged_names: Vec<String> = run_git_ro(cwd, &["diff", "--cached", "--name-only"])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(40)
        .map(String::from)
        .collect();
    let use_staged_only = !staged_names.is_empty();
    let has_head = run_git_ro(cwd, &["rev-parse", "--verify", "HEAD"]).is_some();

    let (files, diff_stat, mut raw_diff) = if use_staged_only {
        let stat = run_git_ro(cwd, &["diff", "--cached", "--stat", "--no-color"]).unwrap_or_default();
        let diff = run_git_ro(cwd, &["diff", "--cached", "--no-color", "--unified=1"]).unwrap_or_default();
        let staged_lines: Vec<String> = all_files
            .iter()
            .filter(|l| {
                let first = l.as_bytes().first().copied().unwrap_or(b' ');
                first != b' ' && first != b'?'
            })
            .cloned()
            .collect();
        let effective_files = if staged_lines.is_empty() {
            staged_names.into_iter().map(|p| format!("M  {p}")).collect()
        } else {
            staged_lines
        };
        (effective_files, stat, diff)
    } else {
        let stat_args: &[&str] = if has_head {
            &["diff", "HEAD", "--stat", "--no-color"]
        } else {
            &["diff", "--cached", "--stat", "--no-color"]
        };
        let stat = run_git_ro(cwd, stat_args).unwrap_or_default();
        let diff_args: &[&str] = if has_head {
            &["diff", "HEAD", "--no-color", "--unified=1"]
        } else {
            &["diff", "--cached", "--no-color", "--unified=1"]
        };
        let diff = run_git_ro(cwd, diff_args).unwrap_or_default();
        (all_files, stat, diff)
    };

    raw_diff = prioritize_non_lockfile_diff(&raw_diff);
    if !use_staged_only && !untracked_paths.is_empty() {
        let untracked_sample = sample_untracked_files(cwd, &untracked_paths);
        if !untracked_sample.is_empty() {
            if !raw_diff.is_empty() && !raw_diff.ends_with('\n') {
                raw_diff.push('\n');
            }
            raw_diff.push_str(&untracked_sample);
        }
    }

    let recent_commits: Vec<String> = if has_head {
        run_git_ro(cwd, &["log", "--oneline", "-n", "5"])
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(100).collect())
            .collect()
    } else {
        Vec::new()
    };

    let max_lines = (diff_budget_chars / 40).clamp(80, 260);
    let diff_excerpt = strip_and_dedup_lines(&raw_diff, max_lines, 160, diff_budget_chars);
    Ok(GitDiffSnapshot {
        branch,
        files,
        diff_stat: strip_and_dedup_lines(&diff_stat, 30, 120, 1200),
        diff_excerpt,
        recent_commits,
    })
}

pub fn build_deterministic_commit_message(files: &[String], is_zh: bool) -> String {
    let names: Vec<String> = files
        .iter()
        .filter_map(|line| line.get(3..).map(str::trim))
        .filter(|p| !p.is_empty())
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
        .take(3)
        .collect();
    let count = files.len();
    let all_new = files.iter().all(|l| l.starts_with("??") || l.starts_with("A "));
    let all_docs = files.iter().all(|l| l.to_ascii_lowercase().ends_with(".md"));
    let kind = if all_docs {
        "docs"
    } else if all_new {
        "feat"
    } else {
        "chore"
    };
    let list = names.join(", ");
    if is_zh {
        if count <= 3 && !list.is_empty() {
            format!("{kind}: 更新 {list}")
        } else {
            format!("{kind}: 更新 {list} 等 {count} 个文件")
        }
    } else if count <= 3 && !list.is_empty() {
        format!("{kind}: update {list}")
    } else {
        format!("{kind}: update {list} and {} more files", count.saturating_sub(names.len()))
    }
}

/// Generates a concise Git commit message from uncommitted changes in `cwd` using Core AI,
/// with a deterministic fallback when AI is disabled or unavailable.
pub async fn generate_commit_message(
    ai: &AiRuntime,
    cwd: &str,
    language: Option<&str>,
    include_ai: bool,
) -> Result<CommitMessageResult, String> {
    let cfg = ai.config_snapshot();
    let ctx_tokens = cfg.context_window.as_deref().and_then(super::parse_ai_token_size);
    let diff_budget = effective_commit_diff_budget(ctx_tokens);
    let cwd_owned = cwd.to_string();
    let snap = tokio::task::spawn_blocking(move || collect_git_diff_snapshot(&cwd_owned, diff_budget))
        .await
        .map_err(|e| e.to_string())??;
    let is_zh = language.is_some_and(|l| l.to_ascii_lowercase().starts_with("zh"));
    let fallback_msg = build_deterministic_commit_message(&snap.files, is_zh);
    let fallback = CommitMessageResult { message: fallback_msg, used_ai: false, from_cache: false };

    if !include_ai || !cfg.enabled || cfg.base_url.trim().is_empty() || ai.is_circuit_open() {
        return Ok(fallback);
    }

    let lang = if is_zh { "zh" } else { "en" };
    let files_joined = snap.files.join("\n");
    let commits_joined = snap.recent_commits.join("\n");
    let cache_key = format!(
        "commit_msg:v2:{lang}:{}",
        fingerprint_hex(&[&snap.branch, &files_joined, &snap.diff_stat, &snap.diff_excerpt, &commits_joined])
    );
    let payload = json!({
        "branch": snap.branch,
        "files": snap.files,
        "diff_stat": snap.diff_stat,
        "diff_excerpt": snap.diff_excerpt,
        "recent_commits": snap.recent_commits,
    });
    let user_prompt = format_op_user_message("commit_message", lang, &payload);
    let Ok(resp) = ai
        .complete_fg(AiCompleteRequest {
            op: "commit_message".into(),
            tier: AiTaskTier::Nano,
            mode: AiConversationMode::StatelessSharedPrefix,
            user_prompt,
            max_tokens: 256,
            timeout: DIGEST_TIMEOUT,
            cache_key: Some(cache_key),
            cache_ttl: Some(Duration::from_secs(120)),
            prompt_cache_bucket: Some("cp".into()),
            ..Default::default()
        })
        .await
    else {
        return Ok(fallback);
    };

    let parsed_msg =
        extract_json_object(&resp.content).and_then(|v| clean_bounded(v["message"].as_str(), 84)).or_else(|| {
            if !resp.from_local_cache {
                ai.record_parse_error("commit_message");
            }
            clean_bounded(resp.content.lines().next(), 84)
        });
    match parsed_msg {
        Some(message) => Ok(CommitMessageResult { message, used_ai: true, from_cache: resp.from_local_cache }),
        None => Ok(fallback),
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PolishPromptParams {
    /// Raw draft prompt to clarify and structure before sending to an agent.
    pub prompt: String,
    /// Optional target agent or terminal address (`a1`, `t1`, or `machine/a1`) for brief task/cwd context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Target agent kind (`acp` or `cli`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<String>,
    /// Optional extra workspace or conversation context hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Preferred UI language (`zh-CN` / `en`); defaults to preserving the prompt's own language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Target remote machine (`h2 machine ls`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PolishPromptResult {
    pub polished: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub used_ai: bool,
    #[serde(default)]
    pub from_cache: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

/// Cleans a multi-line polished prompt while preserving intentional newlines and list formatting.
pub fn clean_multiline_prompt(raw: &str, max_chars: usize) -> Option<String> {
    let mut s = raw.trim().replace("\r\n", "\n");
    if s.starts_with("```") {
        let mut lines = s.lines();
        let _ = lines.next();
        let mut body: Vec<&str> = lines.collect();
        if body.last().is_some_and(|l| l.trim() == "```") {
            body.pop();
        }
        s = body.join("\n").trim().to_string();
    }
    if s.is_empty() {
        return None;
    }
    let out = if s.chars().count() > max_chars {
        s.chars().take(max_chars).collect::<String>().trim_end().to_string()
    } else {
        s
    };
    if out.is_empty() { None } else { Some(out) }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolishSessionContext {
    /// Compact metadata string (program/CLI, semantic title, live progress, cwd, caller hints).
    pub meta: Option<String>,
    /// Chronological recent conversation excerpts (`User: ...`, `Assistant: ...`, `Error: ...`).
    pub recent_turns: Vec<String>,
    /// Recent concrete tool actions, errors, or pending approvals from the latest turn(s).
    pub recent_steps: Vec<String>,
    /// Cleaned trailing terminal screen lines (ANSI-stripped, deduplicated, excluding unsent draft echo).
    pub screen_tail: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PolishContextInput<'a> {
    pub caller_context: Option<&'a str>,
    pub program_tag: Option<String>,
    pub semantic_title: Option<&'a str>,
    pub progress_summary: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub pending_permission: Option<&'a PublicPermission>,
    pub entries: &'a [Entry],
    pub screen_text: Option<&'a str>,
    pub draft_prompt: &'a str,
}

fn clean_polish_screen_tail(raw_screen: &str, draft_prompt: &str, has_transcript: bool) -> Option<String> {
    let (max_lines, max_chars) = if has_transcript { (8, 420) } else { (16, 900) };
    let cleaned = strip_and_dedup_lines(raw_screen, max_lines + 4, 140, max_chars + 240);
    if cleaned.is_empty() {
        return None;
    }
    let draft_first_line = draft_prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let draft_probe: String = draft_first_line.chars().take(32).collect();
    let draft_lines: Vec<&str> = draft_prompt
        .lines()
        .map(str::trim)
        .filter(|l| l.chars().count() >= 4)
        .collect();

    let mut lines: Vec<&str> = cleaned.lines().collect();
    // Drop trailing lines that merely echo the user's current unsent prompt inside the CLI input box.
    while let Some(&last) = lines.last() {
        let stripped = last.trim_start_matches(['>', '❯', '›', '$', '%', '?', '•', '·', ' ']).trim();
        let is_draft_echo = !stripped.is_empty()
            && ((!draft_probe.is_empty()
                && draft_probe.chars().count() >= 4
                && (stripped == draft_first_line || stripped.contains(&draft_probe)))
                || draft_lines.iter().any(|dl| stripped == *dl));
        if is_draft_echo {
            lines.pop();
        } else {
            break;
        }
    }
    if lines.is_empty() {
        return None;
    }
    let start = lines.len().saturating_sub(max_lines);
    let mut joined = lines[start..].join("\n");
    if joined.chars().count() > max_chars {
        let keep_from = joined.chars().count().saturating_sub(max_chars.saturating_sub(1));
        let suffix: String = joined.chars().skip(keep_from).collect();
        joined = format!("…{}", suffix.trim_start());
    }
    let trimmed = joined.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}

/// Builds bounded, high-signal session context from agent/terminal metadata, recent transcript
/// turns, recent tool/error steps, and cleaned terminal screen tail.
pub fn build_polish_session_context(input: PolishContextInput<'_>) -> PolishSessionContext {
    let mut meta_parts: Vec<String> = Vec::new();
    let caller = input.caller_context.and_then(|c| clean_bounded(Some(c), 180));
    if let Some(ref c) = caller {
        meta_parts.push(c.clone());
    }
    if let Some(ref prog) = input.program_tag
        && !prog.trim().is_empty()
        && !caller.as_deref().is_some_and(|c| c.contains(prog.as_str()))
    {
        meta_parts.push(prog.trim().to_string());
    }
    let title_clean = input.semantic_title.and_then(|t| clean_bounded(Some(t), 64));
    if let Some(ref title) = title_clean {
        meta_parts.push(format!("task={title}"));
    }
    if let Some(progress) = input.progress_summary.and_then(|p| clean_bounded(Some(p), 96))
        && title_clean.as_deref() != Some(progress.as_str())
    {
        meta_parts.push(format!("progress={progress}"));
    }
    if let Some(perm) = input.pending_permission
        && let Some(ptitle) = clean_bounded(Some(&perm.title), 88)
    {
        meta_parts.push(format!("waiting_approval={ptitle}"));
    }
    if let Some(cwd) = input.cwd.map(str::trim).filter(|s| !s.is_empty()) {
        let cwd_tag = format!("cwd={cwd}");
        if !meta_parts.iter().any(|p| p.contains(&cwd_tag)) {
            meta_parts.push(cwd_tag);
        }
    }
    let meta = if meta_parts.is_empty() {
        None
    } else {
        clean_bounded(Some(&meta_parts.join(" · ")), 320)
    };

    // Extract recent dialogue turns (up to 6 items / ~1200 chars, chronological).
    const MAX_TURN_ITEMS: usize = 6;
    const MAX_TURNS_CHARS: usize = 1200;
    let mut rev_turns: Vec<String> = Vec::new();
    let mut turn_chars = 0usize;
    for entry in input.entries.iter().rev() {
        if rev_turns.len() >= MAX_TURN_ITEMS || turn_chars >= MAX_TURNS_CHARS {
            break;
        }
        let item = match entry {
            Entry::User { text, .. } => {
                let unwrapped = strip_h2_envelope(text);
                clean_bounded(Some(&unwrapped), 240).map(|s| format!("User: {s}"))
            }
            Entry::Assistant { text, .. } => clean_bounded(Some(text), 320).map(|s| format!("Assistant: {s}")),
            Entry::Error { message, .. } => clean_bounded(Some(message), 200).map(|s| format!("Error: {s}")),
            _ => None,
        };
        if let Some(s) = item {
            let len = s.chars().count();
            if !rev_turns.is_empty() && turn_chars + len > MAX_TURNS_CHARS {
                break;
            }
            turn_chars += len;
            rev_turns.push(s);
        }
    }
    rev_turns.reverse();

    // Extract recent concrete tool steps / plan items / pending approval (up to 5 items / ~360 chars).
    const MAX_STEP_ITEMS: usize = 5;
    const MAX_STEPS_CHARS: usize = 360;
    let mut rev_steps: Vec<String> = Vec::new();
    let mut step_chars = 0usize;
    let tail_start = input.entries.len().saturating_sub(24);
    for entry in input.entries[tail_start..].iter().rev() {
        if rev_steps.len() >= MAX_STEP_ITEMS || step_chars >= MAX_STEPS_CHARS {
            break;
        }
        let candidate = match entry {
            Entry::ToolCall { title, status, .. } => clean_bounded(Some(title), 84).map(|t| {
                let st = status.trim().to_ascii_lowercase();
                if matches!(st.as_str(), "failed" | "error" | "running" | "in_progress") {
                    format!("[{st}] {t}")
                } else {
                    t
                }
            }),
            Entry::Plan { items, .. } => items.first().and_then(|i| clean_bounded(Some(i), 84)).map(|p| format!("Plan: {p}")),
            _ => None,
        };
        if let Some(step) = candidate
            && !rev_steps.iter().any(|existing| existing == &step)
        {
            let len = step.chars().count();
            if !rev_steps.is_empty() && step_chars + len > MAX_STEPS_CHARS {
                break;
            }
            step_chars += len;
            rev_steps.push(step);
        }
    }
    rev_steps.reverse();
    if let Some(perm) = input.pending_permission
        && let Some(ptitle) = clean_bounded(Some(&perm.title), 84)
    {
        let label = format!("Pending approval: {ptitle}");
        if !rev_steps.iter().any(|s| s == &label) {
            if rev_steps.len() >= MAX_STEP_ITEMS {
                rev_steps.remove(0);
            }
            rev_steps.push(label);
        }
    }

    let screen_tail = input
        .screen_text
        .and_then(|s| clean_polish_screen_tail(s, input.draft_prompt, !rev_turns.is_empty()));

    PolishSessionContext {
        meta,
        recent_turns: rev_turns,
        recent_steps: rev_steps,
        screen_tail,
    }
}

/// Rewrites a rough user prompt into a clearer, more structured coding-agent prompt using Core AI.
pub async fn polish_prompt(
    ai: &AiRuntime,
    prompt: &str,
    agent_kind: Option<&str>,
    session_ctx: &PolishSessionContext,
    language: Option<&str>,
) -> Result<PolishPromptResult, String> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return Err("prompt is empty".to_string());
    }
    let is_zh = if has_cjk(trimmed) {
        true
    } else if trimmed.chars().any(|c| c.is_ascii_alphabetic()) {
        false
    } else {
        language.is_some_and(|l| l.to_ascii_lowercase().starts_with("zh"))
    };
    let lang = if is_zh { "zh" } else { "en" };

    let cfg = ai.config_snapshot();
    if !cfg.enabled || cfg.base_url.trim().is_empty() {
        return Ok(PolishPromptResult {
            polished: trimmed.to_string(),
            summary: None,
            used_ai: false,
            from_cache: false,
            fallback_reason: Some("ai_not_configured".to_string()),
        });
    }
    if ai.is_circuit_open() {
        return Ok(PolishPromptResult {
            polished: trimmed.to_string(),
            summary: None,
            used_ai: false,
            from_cache: false,
            fallback_reason: Some("ai_circuit_open".to_string()),
        });
    }

    let kind = match agent_kind.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("cli") | Some("pty") => "cli",
        _ => "acp",
    };
    let bounded_prompt: String = trimmed.chars().take(4000).collect();
    let ctx_clean = session_ctx.meta.as_deref().and_then(|c| clean_bounded(Some(c), 320));
    let turns_fp = session_ctx.recent_turns.join("\n");
    let steps_fp = session_ctx.recent_steps.join("\n");
    let screen_fp = session_ctx.screen_tail.as_deref().unwrap_or("");
    let cache_key = format!(
        "polish_prompt:{lang}:{kind}:{}",
        fingerprint_hex(&[
            &bounded_prompt,
            ctx_clean.as_deref().unwrap_or(""),
            &turns_fp,
            &steps_fp,
            screen_fp,
        ])
    );
    let mut payload = json!({
        "prompt": bounded_prompt,
        "agent_kind": kind,
        "context": ctx_clean,
    });
    if !session_ctx.recent_turns.is_empty() {
        payload["recent_turns"] = json!(session_ctx.recent_turns);
    }
    if !session_ctx.recent_steps.is_empty() {
        payload["recent_steps"] = json!(session_ctx.recent_steps);
    }
    if let Some(ref tail) = session_ctx.screen_tail {
        payload["screen_tail"] = json!(tail);
    }
    let user_prompt = format_op_user_message("polish_prompt", lang, &payload);
    let resp = match ai
        .complete_fg(AiCompleteRequest {
            op: "polish_prompt".into(),
            tier: AiTaskTier::Fast,
            mode: AiConversationMode::StatelessSharedPrefix,
            user_prompt,
            max_tokens: 1024,
            timeout: Duration::from_secs(20),
            cache_key: Some(cache_key),
            cache_ttl: Some(Duration::from_secs(300)),
            prompt_cache_bucket: Some("cp".into()),
            ..Default::default()
        })
        .await
    {
        Ok(r) => r,
        Err(err) => {
            let lower = err.to_ascii_lowercase();
            let reason = if lower.contains("timed out") || lower.contains("timeout") {
                "ai_timeout"
            } else if lower.contains("429") || lower.contains("rate limit") {
                "ai_rate_limited"
            } else if lower.contains("401") || lower.contains("403") {
                "ai_auth_error"
            } else if lower.contains("circuit breaker") {
                "ai_circuit_open"
            } else {
                "ai_request_failed"
            };
            return Ok(PolishPromptResult {
                polished: trimmed.to_string(),
                summary: None,
                used_ai: false,
                from_cache: false,
                fallback_reason: Some(reason.to_string()),
            });
        }
    };

    if let Some(v) = extract_json_object(&resp.content)
        && let Some(polished) = v["polished"].as_str().and_then(|s| clean_multiline_prompt(s, 4500))
    {
        let summary = clean_bounded(v["summary"].as_str(), 60);
        return Ok(PolishPromptResult {
            polished,
            summary,
            used_ai: true,
            from_cache: resp.from_local_cache,
            fallback_reason: None,
        });
    }

    if !resp.from_local_cache {
        ai.record_parse_error("polish_prompt");
    }
    if let Some(polished) = clean_multiline_prompt(&resp.content, 4500) {
        return Ok(PolishPromptResult {
            polished,
            summary: None,
            used_ai: true,
            from_cache: resp.from_local_cache,
            fallback_reason: None,
        });
    }

    Ok(PolishPromptResult {
        polished: trimmed.to_string(),
        summary: None,
        used_ai: false,
        from_cache: false,
        fallback_reason: Some("ai_empty_response".to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_h2_envelope_and_builds_deterministic_digest() {
        let entries = vec![
            Entry::User {
                turn: 1,
                from: Some("t1".into()),
                text: "<h2 from=\"t1\">修复登录接口超时重试逻辑</h2>".into(),
                attachments: vec![],
                ts_ms: 100,
            },
            Entry::ToolCall {
                turn: 1,
                id: "tc1".into(),
                title: "cargo test auth_retry".into(),
                tool_kind: Some("execute".into()),
                status: "running".into(),
                detail: None,
                attachments: vec![],
                ts_ms: 200,
            },
        ];
        let d = build_deterministic_acp_digest("working", 1, None, &entries, None, true);
        assert_eq!(d.semantic_title.as_deref(), Some("修复登录接口超时重试逻辑"));
        assert!(d.micro_summary.as_deref().unwrap().contains("cargo test auth_retry"));
    }

    #[test]
    fn prioritizes_pending_permission_in_micro_summary() {
        let perm = PublicPermission {
            request_id: "r1".into(),
            session_id: "s1".into(),
            title: "Run cargo test --workspace".into(),
            kind: Some("execute".into()),
            options: vec![],
        };
        let d = build_deterministic_acp_digest("blocked", 2, Some(&perm), &[], Some("Auth refactor"), false);
        assert_eq!(d.semantic_title.as_deref(), Some("Auth refactor"));
        assert_eq!(d.micro_summary.as_deref(), Some("Waiting approval: Run cargo test --workspace"));
    }

    #[test]
    fn multi_turn_conversation_updates_semantic_title_and_micro_summary_to_latest_turn() {
        let entries = vec![
            Entry::User {
                turn: 1,
                from: None,
                text: "修复登录接口超时重试逻辑".into(),
                attachments: vec![],
                ts_ms: 100,
            },
            Entry::Assistant {
                turn: 1, text: "已完成登录超时重试并验证通过".into(), attachments: vec![], ts_ms: 200
            },
            Entry::User {
                turn: 2,
                from: None,
                text: "把左下角闪念清单的回收站图标改成和智能体一致".into(),
                attachments: vec![],
                ts_ms: 300,
            },
            Entry::ToolCall {
                turn: 2,
                id: "tc2".into(),
                title: "edit_file flash-note/main.js".into(),
                tool_kind: Some("edit".into()),
                status: "running".into(),
                detail: None,
                attachments: vec![],
                ts_ms: 350,
            },
        ];

        // Even when status is "idle" (non-7501 CLI whose core status is None) and Turn 1 had an existing_title,
        // the digest must reflect Turn 2's topic and active tool call instead of staying frozen on Turn 1.
        let d_mid = build_deterministic_acp_digest("idle", 2, None, &entries, Some("修复登录超时"), true);
        assert_eq!(d_mid.initial_task_hint, "修复登录接口超时重试逻辑");
        assert_eq!(d_mid.task_hint, "把左下角闪念清单的回收站图标改成和智能体一致");
        assert!(
            d_mid.semantic_title.as_deref().unwrap().contains("闪念清单"),
            "semantic_title must advance to Turn 2 topic: {:?}",
            d_mid.semantic_title
        );
        assert!(
            d_mid.micro_summary.as_deref().unwrap().contains("flash-note/main.js"),
            "micro_summary must not stay stuck on Turn 1 assistant answer: {:?}",
            d_mid.micro_summary
        );

        // Once Turn 2 assistant answers, micro_summary updates to Turn 2's reply.
        let mut finished = entries;
        finished.push(Entry::Assistant {
            turn: 2,
            text: "已统一为 archive.svg 图标并通过测试".into(),
            attachments: vec![],
            ts_ms: 400,
        });
        let d_done = build_deterministic_acp_digest("idle", 2, None, &finished, d_mid.semantic_title.as_deref(), true);
        assert!(
            d_done.micro_summary.as_deref().unwrap().contains("archive.svg"),
            "micro_summary must update to Turn 2 assistant answer: {:?}",
            d_done.micro_summary
        );
    }

    #[test]
    fn recent_user_turns_survive_many_tool_calls_and_skip_meta_followups_for_title() {
        let mut entries = vec![
            Entry::User {
                turn: 1, from: None, text: "清理退出后的残留状态".into(), attachments: vec![], ts_ms: 10
            },
            Entry::Assistant { turn: 1, text: "已修复退出清理".into(), attachments: vec![], ts_ms: 20 },
            Entry::User {
                turn: 2, from: None, text: "统一闪念清单回收站图标".into(), attachments: vec![], ts_ms: 30
            },
            Entry::Assistant { turn: 2, text: "已改为 archive.svg".into(), attachments: vec![], ts_ms: 40 },
            Entry::User {
                turn: 3,
                from: None,
                text: "修复 Tab 顶部任务信息不随对话更新".into(),
                attachments: vec![],
                ts_ms: 50,
            },
        ];
        // Flood Turn 3 with 30 tool calls to ensure `recent_user_turns` is never crowded out.
        for i in 0..30 {
            entries.push(Entry::ToolCall {
                turn: 3,
                id: format!("tc-{i}"),
                title: format!("read_file step_{i}.rs"),
                tool_kind: Some("read".into()),
                status: "completed".into(),
                detail: None,
                attachments: vec![],
                ts_ms: 60 + i as u64,
            });
        }
        entries
            .push(Entry::Assistant { turn: 3, text: "已修复实时更新".into(), attachments: vec![], ts_ms: 200 });
        entries.push(Entry::User {
            turn: 4,
            from: None,
            text: "编译安装最新的版本".into(),
            attachments: vec![],
            ts_ms: 300,
        });

        let d = build_deterministic_acp_digest("working", 4, None, &entries, None, true);
        assert_eq!(d.recent_user_turns.len(), 4);
        assert_eq!(d.recent_user_turns[0], "清理退出后的残留状态");
        assert_eq!(d.recent_user_turns[3], "编译安装最新的版本");
        // Follow-up meta command ("编译安装最新的版本") must NOT overwrite the substantive intent from Turn 3.
        assert!(
            d.semantic_title.as_deref().unwrap().contains("Tab 顶部任务信息"),
            "semantic_title should preserve Turn 3 substantive intent over meta follow-up: {:?}",
            d.semantic_title
        );
        // Meanwhile micro_summary reflects the live follow-up action.
        assert!(
            d.micro_summary.as_deref().unwrap().contains("编译安装最新的版本"),
            "micro_summary should reflect current working follow-up: {:?}",
            d.micro_summary
        );
    }

    #[test]
    fn clean_multiline_prompt_strips_code_fence_and_keeps_structure() {
        let raw = "```markdown\n1. 修复登录超时\n2. 补充单元测试\n```";
        assert_eq!(clean_multiline_prompt(raw, 500).as_deref(), Some("1. 修复登录超时\n2. 补充单元测试"));
        assert_eq!(clean_multiline_prompt("   \n  ", 500), None);
    }

    #[test]
    fn commit_snapshot_samples_untracked_files_recent_commits_and_prefers_staged() {
        let root = crate::test_tmp().join(format!("commit-snap-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            let st = std::process::Command::new("git").args(args).current_dir(&root).status().unwrap();
            assert!(st.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(root.join("lib.rs"), "pub fn a() {}\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "feat(core): initial setup"]);

        // Untracked file only -> sampled into diff_excerpt, plus recent_commits populated.
        std::fs::write(root.join("new_feature.rs"), "pub fn hello_world() -> &'static str { \"hi\" }\n").unwrap();
        let snap1 = collect_git_diff_snapshot(root.to_str().unwrap(), 10_000).unwrap();
        assert!(snap1.diff_excerpt.contains("+++ untracked/new_feature.rs"));
        assert!(snap1.diff_excerpt.contains("hello_world"));
        assert!(snap1.recent_commits.iter().any(|c| c.contains("feat(core): initial setup")));

        // Stage lib.rs edit while leaving new_feature.rs untracked -> staged-only focus.
        std::fs::write(root.join("lib.rs"), "pub fn a() { println!(\"ok\"); }\n").unwrap();
        git(&["add", "lib.rs"]);
        let snap2 = collect_git_diff_snapshot(root.to_str().unwrap(), 10_000).unwrap();
        assert!(snap2.diff_excerpt.contains("println!"));
        assert!(!snap2.diff_excerpt.contains("untracked/new_feature.rs"));

        assert_eq!(effective_commit_diff_budget(None), 10_000);
        assert_eq!(effective_commit_diff_budget(Some(16_384)), 6_000);
        assert_eq!(effective_commit_diff_budget(Some(8192)), 3_200);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn screen_digest_with_live_osc_title_updates_semantic_title_and_skips_paths() {
        let d1 = build_deterministic_screen_digest_with_title(
            "running tests...",
            Some("旧任务"),
            Some("✳ 修复左侧名称同步"),
            Some("claude"),
        );
        assert_eq!(d1.semantic_title.as_deref(), Some("修复左侧名称同步"));
        assert_eq!(d1.task_hint, "修复左侧名称同步");

        let d_path = build_deterministic_screen_digest_with_title(
            "running...",
            Some("旧任务"),
            Some("✳ ~/Work/herdr2"),
            Some("claude"),
        );
        assert_eq!(d_path.semantic_title.as_deref(), Some("旧任务"));

        let d_prog = build_deterministic_screen_digest_with_title(
            "running...",
            Some("旧任务"),
            Some("Claude Code"),
            Some("claude"),
        );
        assert_eq!(d_prog.semantic_title.as_deref(), Some("旧任务"));

        // Shell preexec launch command lines must not be mistaken for a telling task title.
        let d_launch = build_deterministic_screen_digest_with_title(
            "Welcome to Antigravity",
            None,
            Some("agy --dangerously-skip-permissions"),
            Some("agy.real"),
        );
        assert_eq!(d_launch.semantic_title, None);
        assert!(d_launch.task_hint.is_empty());

        let d_claude_launch = build_deterministic_screen_digest_with_title(
            "Welcome back!",
            None,
            Some("claude --dangerously-skip-permissions"),
            Some("claude"),
        );
        assert_eq!(d_claude_launch.semantic_title, None);
    }

    #[test]
    fn polish_session_context_extracts_turns_steps_permission_and_dedups_cwd() {
        let perm = PublicPermission {
            request_id: "r1".into(),
            session_id: "s1".into(),
            title: "Run cargo test -p herdr2".into(),
            kind: Some("execute".into()),
            options: vec![],
        };
        let entries = vec![
            Entry::User {
                turn: 1,
                from: Some("t1".into()),
                text: "<h2 from=\"t1\">给左侧智能体清单加上下拖动排序动画</h2>".into(),
                attachments: vec![],
                ts_ms: 10,
            },
            Entry::ToolCall {
                turn: 1,
                id: "tc1".into(),
                title: "edit_file defaults/mods/agents/main.js".into(),
                tool_kind: Some("edit".into()),
                status: "completed".into(),
                detail: None,
                attachments: vec![],
                ts_ms: 20,
            },
            Entry::ToolCall {
                turn: 1,
                id: "tc2".into(),
                title: "bun test scripts/agents.test.ts".into(),
                tool_kind: Some("execute".into()),
                status: "failed".into(),
                detail: None,
                attachments: vec![],
                ts_ms: 30,
            },
            Entry::Assistant {
                turn: 1,
                text: "已完成弹簧动画实现，但 agents.test.ts 中有 1 个 ID 匹配断言需要调整。".into(),
                attachments: vec![],
                ts_ms: 40,
            },
            Entry::Error {
                turn: 1,
                message: "Expected ag-a1 but received agent-unit-a1".into(),
                ts_ms: 45,
            },
        ];

        let ctx = build_polish_session_context(PolishContextInput {
            caller_context: Some("cwd=/repo/herdr2"),
            program_tag: Some("agent=claude".into()),
            semantic_title: Some("侧边栏拖拽排序".into()),
            progress_summary: Some("正在修复 agents.test.ts 断言".into()),
            cwd: Some("/repo/herdr2"),
            pending_permission: Some(&perm),
            entries: &entries,
            screen_text: None,
            draft_prompt: "把那个报错修一下",
        });

        let meta = ctx.meta.as_deref().unwrap();
        assert!(meta.contains("agent=claude"), "meta={meta}");
        assert!(meta.contains("task=侧边栏拖拽排序"), "meta={meta}");
        assert!(meta.contains("progress=正在修复 agents.test.ts 断言"), "meta={meta}");
        assert!(meta.contains("waiting_approval=Run cargo test -p herdr2"), "meta={meta}");
        assert_eq!(meta.matches("cwd=/repo/herdr2").count(), 1, "cwd must not duplicate: {meta}");

        assert_eq!(ctx.recent_turns.len(), 3);
        assert!(ctx.recent_turns[0].starts_with("User: 给左侧智能体清单加上下拖动排序动画"));
        assert!(ctx.recent_turns[1].starts_with("Assistant: 已完成弹簧动画实现"));
        assert!(ctx.recent_turns[2].starts_with("Error: Expected ag-a1"));

        assert!(ctx.recent_steps.iter().any(|s| s.contains("edit_file defaults/mods/agents/main.js")));
        assert!(ctx.recent_steps.iter().any(|s| s == "[failed] bun test scripts/agents.test.ts"));
        assert!(ctx.recent_steps.iter().any(|s| s == "Pending approval: Run cargo test -p herdr2"));
        assert_eq!(ctx.screen_tail, None);
    }

    #[test]
    fn polish_session_context_cleans_cli_screen_tail_and_drops_draft_echo() {
        let raw_screen = "\
\x1b[32m✓\x1b[0m compiled herdr-core\n\
error[E0308]: mismatched types in src/api.rs:1835\n\
expected `&PolishSessionContext`, found `Option<&str>`\n\
────────────────────────────\n\
❯ 帮我把上面这个类型错误修一下并跑单测\n";

        let ctx = build_polish_session_context(PolishContextInput {
            caller_context: None,
            program_tag: Some("cli=claude".into()),
            semantic_title: None,
            progress_summary: None,
            cwd: Some("/repo/herdr2"),
            pending_permission: None,
            entries: &[],
            screen_text: Some(raw_screen),
            draft_prompt: "帮我把上面这个类型错误修一下并跑单测",
        });

        let tail = ctx.screen_tail.as_deref().expect("screen_tail should be populated");
        assert!(tail.contains("error[E0308]: mismatched types in src/api.rs:1835"), "tail={tail}");
        assert!(tail.contains("expected `&PolishSessionContext`"), "tail={tail}");
        assert!(!tail.contains("帮我把上面这个类型错误修一下并跑单测"), "draft echo must be stripped: {tail}");
        assert!(!tail.contains('\x1b'), "ANSI escapes must be stripped: {tail}");
    }

    #[test]
    fn polish_session_context_respects_turn_and_step_budgets() {
        let mut entries = Vec::new();
        for i in 0..15 {
            entries.push(Entry::User {
                turn: i + 1,
                from: None,
                text: format!("Turn {i} user message {}", "x".repeat(400)),
                attachments: vec![],
                ts_ms: i * 10,
            });
            entries.push(Entry::Assistant {
                turn: i + 1,
                text: format!("Turn {i} assistant reply {}", "y".repeat(500)),
                attachments: vec![],
                ts_ms: i * 10 + 5,
            });
        }
        let ctx = build_polish_session_context(PolishContextInput {
            entries: &entries,
            draft_prompt: "继续",
            ..Default::default()
        });
        assert!(!ctx.recent_turns.is_empty() && ctx.recent_turns.len() <= 6);
        let total_chars: usize = ctx.recent_turns.iter().map(|s| s.chars().count()).sum();
        assert!(total_chars <= 1200, "recent_turns exceeded budget: {total_chars}");
        // Must keep the latest turn at the end in chronological order.
        assert!(ctx.recent_turns.last().unwrap().contains("Turn 14 assistant reply"));
    }
}
