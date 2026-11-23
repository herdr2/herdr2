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
    let trimmed = raw
        .trim()
        .trim_end_matches(['.', '!', '?', '。', '！', '？', '~', '～', ' ']);
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
    let mut task_hint = if !latest_substantive_user.is_empty() {
        latest_substantive_user.clone()
    } else {
        initial_task_hint.clone()
    };
    if task_hint.is_empty()
        && let Some(et) = existing_title.map(str::trim).filter(|s| !s.is_empty())
    {
        task_hint = et.to_string();
    }

    let semantic_title = clean_bounded((!latest_substantive_user.is_empty()).then_some(latest_substantive_user.as_str()), 22)
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
        let label = clean_bounded(Some(&perm.title), 64)
            .or_else(|| perm.kind.clone())
            .unwrap_or_else(|| "permission".into());
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
        clean_bounded(
            Some(&if is_zh { format!("执行中：{tool}") } else { format!("Running: {tool}") }),
            90,
        )
    } else if let Some((ai, ans)) = current_turn_assistant {
        let suffix = latest_checkpoint_files.filter(|(ci, _)| *ci >= *ai || *ci >= user_floor).map(|(_, n)| {
            if is_zh {
                format!("（改动 {n} 个文件）")
            } else {
                format!(" ({n} files changed)")
            }
        });
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

    DeterministicDigest {
        semantic_title,
        micro_summary,
        initial_task_hint,
        task_hint,
        recent_user_turns,
        recent_steps,
    }
}

/// Fallback deterministic digest from raw terminal screen text when a CLI agent has no structured
/// transcript file yet (e.g., opencode, cursor-agent, copilot, or pre-flush sessions).
pub fn build_deterministic_screen_digest(
    screen_text: &str,
    existing_title: Option<&str>,
) -> DeterministicDigest {
    let cleaned = strip_and_dedup_lines(screen_text, 16, 120, 1100);
    let recent_steps: Vec<String> = cleaned
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| format!("screen: {l}"))
        .collect();
    let semantic_title = existing_title.and_then(|s| clean_bounded(Some(s), 24));
    DeterministicDigest {
        semantic_title,
        micro_summary: None,
        initial_task_hint: String::new(),
        task_hint: existing_title.unwrap_or("").trim().to_string(),
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
    let numbered_user_turns: Vec<String> = det
        .recent_user_turns
        .iter()
        .enumerate()
        .map(|(i, u)| format!("{}. {u}", i + 1))
        .collect();
    let user_turns_joined = if numbered_user_turns.is_empty() {
        strip_and_dedup_lines(&det.task_hint, 2, 140, 240)
    } else {
        strip_and_dedup_lines(&numbered_user_turns.join("\n"), MAX_RECENT_USER_TURNS, 140, 720)
    };
    let steps_joined = strip_and_dedup_lines(&det.recent_steps.join("\n"), 10, 140, 1100);
    let existing = existing_title.unwrap_or("").trim();
    let cache_key = format!(
        "agent_digest:v2:{lang}:{program}:{status}:{}",
        fingerprint_hex(&[&user_turns_joined, &steps_joined])
    );

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
}

fn run_git_ro(cwd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("--no-optional-locks")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        None
    }
}

fn collect_git_diff_snapshot(cwd: &str) -> Result<GitDiffSnapshot, String> {
    let status = run_git_ro(cwd, &["status", "--porcelain=v1", "-b"])
        .ok_or_else(|| format!("not a git repository: {cwd}"))?;
    let mut branch = String::new();
    let mut files = Vec::new();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            branch = rest.split("...").next().unwrap_or(rest).trim().to_string();
        } else if !line.trim().is_empty() && files.len() < 40 {
            files.push(line.to_string());
        }
    }
    if files.is_empty() {
        return Err("no uncommitted changes".into());
    }
    let has_head = run_git_ro(cwd, &["rev-parse", "--verify", "HEAD"]).is_some();
    let stat_args: &[&str] = if has_head {
        &["diff", "HEAD", "--stat", "--no-color"]
    } else {
        &["diff", "--cached", "--stat", "--no-color"]
    };
    let diff_stat = run_git_ro(cwd, stat_args).unwrap_or_default();
    let diff_args: &[&str] = if has_head {
        &["diff", "HEAD", "--no-color", "--unified=1"]
    } else {
        &["diff", "--cached", "--no-color", "--unified=1"]
    };
    let raw_diff = run_git_ro(cwd, diff_args).unwrap_or_default();
    let diff_excerpt = strip_and_dedup_lines(&raw_diff, 80, 160, 3200);
    Ok(GitDiffSnapshot { branch, files, diff_stat: strip_and_dedup_lines(&diff_stat, 30, 120, 1200), diff_excerpt })
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
    let cwd_owned = cwd.to_string();
    let snap = tokio::task::spawn_blocking(move || collect_git_diff_snapshot(&cwd_owned))
        .await
        .map_err(|e| e.to_string())??;
    let is_zh = language.is_some_and(|l| l.to_ascii_lowercase().starts_with("zh"));
    let fallback_msg = build_deterministic_commit_message(&snap.files, is_zh);
    let fallback = CommitMessageResult { message: fallback_msg, used_ai: false, from_cache: false };

    let cfg = ai.config_snapshot();
    if !include_ai || !cfg.enabled || cfg.base_url.trim().is_empty() || ai.is_circuit_open() {
        return Ok(fallback);
    }

    let lang = if is_zh { "zh" } else { "en" };
    let files_joined = snap.files.join("\n");
    let cache_key = format!(
        "commit_msg:{lang}:{}",
        fingerprint_hex(&[&snap.branch, &files_joined, &snap.diff_stat, &snap.diff_excerpt])
    );
    let payload = json!({
        "branch": snap.branch,
        "files": snap.files,
        "diff_stat": snap.diff_stat,
        "diff_excerpt": snap.diff_excerpt,
    });
    let user_prompt = format_op_user_message("commit_message", lang, &payload);
    let Some(resp) = ai
        .complete_bg(AiCompleteRequest {
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

    let parsed_msg = extract_json_object(&resp.content)
        .and_then(|v| clean_bounded(v["message"].as_str(), 84))
        .or_else(|| clean_bounded(resp.content.lines().next(), 84));
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

/// Rewrites a rough user prompt into a clearer, more structured coding-agent prompt using Core AI.
pub async fn polish_prompt(
    ai: &AiRuntime,
    prompt: &str,
    agent_kind: Option<&str>,
    context: Option<&str>,
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
    let ctx_clean = context.and_then(|c| clean_bounded(Some(c), 240));
    let cache_key = format!(
        "polish_prompt:{lang}:{kind}:{}",
        fingerprint_hex(&[&bounded_prompt, ctx_clean.as_deref().unwrap_or("")])
    );
    let payload = json!({
        "prompt": bounded_prompt,
        "agent_kind": kind,
        "context": ctx_clean,
    });
    let user_prompt = format_op_user_message("polish_prompt", lang, &payload);
    let Some(resp) = ai
        .complete_bg(AiCompleteRequest {
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
    else {
        return Ok(PolishPromptResult {
            polished: trimmed.to_string(),
            summary: None,
            used_ai: false,
            from_cache: false,
            fallback_reason: Some("ai_request_failed".to_string()),
        });
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
                turn: 1,
                text: "已完成登录超时重试并验证通过".into(),
                attachments: vec![],
                ts_ms: 200,
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
            Entry::User { turn: 1, from: None, text: "清理退出后的残留状态".into(), attachments: vec![], ts_ms: 10 },
            Entry::Assistant { turn: 1, text: "已修复退出清理".into(), attachments: vec![], ts_ms: 20 },
            Entry::User { turn: 2, from: None, text: "统一闪念清单回收站图标".into(), attachments: vec![], ts_ms: 30 },
            Entry::Assistant { turn: 2, text: "已改为 archive.svg".into(), attachments: vec![], ts_ms: 40 },
            Entry::User { turn: 3, from: None, text: "修复 Tab 顶部任务信息不随对话更新".into(), attachments: vec![], ts_ms: 50 },
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
        entries.push(Entry::Assistant { turn: 3, text: "已修复实时更新".into(), attachments: vec![], ts_ms: 200 });
        entries.push(Entry::User { turn: 4, from: None, text: "编译安装最新的版本".into(), attachments: vec![], ts_ms: 300 });

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
        assert_eq!(
            clean_multiline_prompt(raw, 500).as_deref(),
            Some("1. 修复登录超时\n2. 补充单元测试")
        );
        assert_eq!(clean_multiline_prompt("   \n  ", 500), None);
    }
}
