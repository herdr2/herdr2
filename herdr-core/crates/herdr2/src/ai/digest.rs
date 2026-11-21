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
    pub task_hint: String,
    pub recent_steps: Vec<String>,
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

/// Pure, 0ms deterministic digest derived from structured ACP transcript entries.
pub fn build_deterministic_acp_digest(
    status: &str,
    turn: u64,
    pending_permission: Option<&PublicPermission>,
    entries: &[Entry],
    existing_title: Option<&str>,
    is_zh: bool,
) -> DeterministicDigest {
    let mut task_hint = String::new();
    for entry in entries {
        if let Entry::User { text, .. } = entry {
            let clean = strip_h2_envelope(text);
            if let Some(line) = first_meaningful_line(&clean) {
                task_hint = line;
                break;
            }
        }
    }
    if task_hint.is_empty()
        && let Some(et) = existing_title.map(str::trim).filter(|s| !s.is_empty())
    {
        task_hint = et.to_string();
    }

    let semantic_title = existing_title
        .and_then(|s| clean_bounded(Some(s), 24))
        .or_else(|| clean_bounded((!task_hint.is_empty()).then_some(task_hint.as_str()), 22));

    let mut recent_steps: Vec<String> = Vec::new();
    let mut latest_tool: Option<String> = None;
    let mut latest_plan: Option<String> = None;
    let mut latest_assistant: Option<String> = None;
    let mut latest_error: Option<String> = None;
    let mut latest_checkpoint_files: Option<usize> = None;

    for entry in entries.iter().rev().take(24).collect::<Vec<_>>().into_iter().rev() {
        match entry {
            Entry::User { text, .. } => {
                let c = strip_h2_envelope(text);
                if let Some(line) = first_meaningful_line(&c)
                    && let Some(b) = clean_bounded(Some(&line), 90)
                {
                    recent_steps.push(format!("user: {b}"));
                }
            }
            Entry::ToolCall { title, status: ts, .. } => {
                if let Some(b) = clean_bounded(Some(title), 72) {
                    latest_tool = Some(b.clone());
                    recent_steps.push(format!("tool({ts}): {b}"));
                }
            }
            Entry::Plan { items, .. } => {
                if let Some(first) = items.iter().find(|i| !i.trim().is_empty())
                    && let Some(b) = clean_bounded(Some(first), 72)
                {
                    latest_plan = Some(b.clone());
                    recent_steps.push(format!("plan: {b}"));
                }
            }
            Entry::Assistant { text, .. } => {
                if let Some(line) = first_meaningful_line(text)
                    && let Some(b) = clean_bounded(Some(&line), 90)
                {
                    latest_assistant = Some(b.clone());
                    recent_steps.push(format!("assistant: {b}"));
                }
            }
            Entry::Error { message, .. } => {
                if let Some(line) = first_meaningful_line(message)
                    && let Some(b) = clean_bounded(Some(&line), 90)
                {
                    latest_error = Some(b.clone());
                    recent_steps.push(format!("error: {b}"));
                }
            }
            Entry::Permission { title, outcome, .. } => {
                if let Some(b) = clean_bounded(Some(title), 72) {
                    recent_steps.push(format!("perm({outcome}): {b}"));
                }
            }
            Entry::Checkpoint { files, .. } if !files.is_empty() => {
                latest_checkpoint_files = Some(files.len());
            }
            _ => {}
        }
    }
    if recent_steps.len() > 10 {
        recent_steps = recent_steps.split_off(recent_steps.len() - 10);
    }

    let micro_summary = if let Some(perm) = pending_permission {
        let label = clean_bounded(Some(&perm.title), 64)
            .or_else(|| perm.kind.clone())
            .unwrap_or_else(|| "permission".into());
        Some(if is_zh { format!("等待审批：{label}") } else { format!("Waiting approval: {label}") })
    } else if status == "error" {
        let err = latest_error.unwrap_or_else(|| if is_zh { "执行异常".into() } else { "Execution error".into() });
        clean_bounded(Some(&if is_zh { format!("异常：{err}") } else { format!("Error: {err}") }), 90)
    } else if status == "working" {
        if let Some(tool) = latest_tool {
            clean_bounded(
                Some(&if is_zh {
                    format!("执行中：{tool}（第 {} 轮）", turn.max(1))
                } else {
                    format!("Running: {tool} (turn {})", turn.max(1))
                }),
                90,
            )
        } else if let Some(plan) = latest_plan {
            clean_bounded(Some(&if is_zh { format!("计划推进：{plan}") } else { format!("Plan: {plan}") }), 90)
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
    } else if let Some(ans) = latest_assistant {
        let suffix = latest_checkpoint_files.map(|n| {
            if is_zh {
                format!("（改动 {n} 个文件）")
            } else {
                format!(" ({n} files changed)")
            }
        });
        let combined = match suffix {
            Some(s) => format!("{ans}{s}"),
            None => ans,
        };
        clean_bounded(Some(&combined), 90)
    } else if !task_hint.is_empty() {
        clean_bounded(Some(&task_hint), 90)
    } else {
        None
    };

    DeterministicDigest { semantic_title, micro_summary, task_hint, recent_steps }
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
        || (det.task_hint.is_empty() && det.recent_steps.is_empty())
    {
        return fallback;
    }

    let lang = if language.is_some_and(|l| l.to_ascii_lowercase().starts_with("zh")) { "zh" } else { "en" };
    let steps_joined = strip_and_dedup_lines(&det.recent_steps.join("\n"), 10, 140, 1100);
    let hint_clean = strip_and_dedup_lines(&det.task_hint, 2, 140, 240);
    let existing = existing_title.unwrap_or("").trim();
    let cache_key = format!(
        "agent_digest:{lang}:{program}:{status}:{}",
        fingerprint_hex(&[&hint_clean, &steps_joined, existing])
    );

    let payload = json!({
        "program": program,
        "status": status,
        "task_hint": hint_clean,
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
                ts_ms: 100,
            },
            Entry::ToolCall {
                turn: 1,
                id: "tc1".into(),
                title: "cargo test auth_retry".into(),
                tool_kind: Some("execute".into()),
                status: "running".into(),
                detail: None,
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
}
