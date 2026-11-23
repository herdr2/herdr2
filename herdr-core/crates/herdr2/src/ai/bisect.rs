//! Semantic milestone labeling for turn checkpoints and two-stage fault-introduction bisect
//! (`checkpoint.bisect`).

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ai::client::extract_json_object;
use crate::ai::compact::fingerprint_hex;
use crate::ai::prompts::format_op_user_message;
use crate::ai::{AiCompleteRequest, AiConversationMode, AiRuntime, AiTaskTier};
use crate::checkpoint::{CheckpointFileStat, CheckpointMeta, RepoInfo, diff_checkpoints};
use crate::rpc::RpcError;

const LABEL_PATCH_BUDGET: usize = 4096;
const BISECT_PATCH_BUDGET_MIN: usize = 4 * 1024;
const BISECT_PATCH_BUDGET_MED: usize = 8 * 1024;
const BISECT_PATCH_BUDGET_STD: usize = 12 * 1024;
const BISECT_PATCH_BUDGET_HIGH: usize = 16 * 1024;
const LABEL_TIMEOUT: Duration = Duration::from_secs(6);
const BISECT_RANK_TIMEOUT: Duration = Duration::from_secs(8);
const BISECT_VERIFY_TIMEOUT: Duration = Duration::from_secs(16);
const LABEL_CACHE_TTL: Duration = Duration::from_secs(3600);
const BISECT_CACHE_TTL: Duration = Duration::from_secs(600);

/// Computes an adaptive per-turn diff byte budget based on the model's configured context window
/// and the number of candidate turns inspected in Stage 2.
pub fn effective_bisect_patch_budget(ctx_tokens: Option<u32>, candidate_count: usize) -> usize {
    match ctx_tokens {
        Some(c) if c <= 8192 => BISECT_PATCH_BUDGET_MIN,
        Some(c) if c < 32_768 => BISECT_PATCH_BUDGET_MED,
        _ => {
            if candidate_count <= 2 {
                BISECT_PATCH_BUDGET_HIGH
            } else {
                BISECT_PATCH_BUDGET_STD
            }
        }
    }
}

fn is_low_signal_patch_file(path: &str) -> bool {
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

/// Splits a multi-file unified diff into per-file chunks, ranks them by relevance to `query`
/// (demoting lockfiles/generated assets), and packs the highest-priority chunks within `budget_bytes`.
pub fn select_relevant_patch(patch: &str, query: &str, budget_bytes: usize) -> String {
    let trimmed = patch.trim();
    if trimmed.is_empty() || trimmed.len() <= budget_bytes {
        return trimmed.to_string();
    }
    if !trimmed.contains("diff --git ") {
        return truncate_utf8(trimmed, budget_bytes);
    }

    struct FileChunk {
        idx: usize,
        path: String,
        body: String,
        score: i32,
    }

    let tokens = extract_query_tokens(query);
    let mut chunks: Vec<FileChunk> = Vec::new();
    let mut cur_body = String::new();
    let mut cur_path = String::new();
    let push_chunk = |chunks: &mut Vec<FileChunk>, body: &mut String, path: &mut String| {
        if body.trim().is_empty() {
            body.clear();
            path.clear();
            return;
        }
        let p = std::mem::take(path);
        let b = std::mem::take(body);
        let low_signal = is_low_signal_patch_file(&p);
        let mut score: i32 = if low_signal { -100 } else { 0 };
        let path_lower = p.to_ascii_lowercase();
        let body_lower = b.to_ascii_lowercase();
        for tok in &tokens {
            if path_lower.contains(tok) {
                score += 6;
            }
            if body_lower.contains(tok) {
                score += 2;
            }
        }
        let idx = chunks.len();
        chunks.push(FileChunk { idx, path: p, body: b, score });
    };

    for line in trimmed.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            push_chunk(&mut chunks, &mut cur_body, &mut cur_path);
            cur_path =
                rest.split_once(" b/").map(|(_, b)| b.trim().to_string()).unwrap_or_else(|| rest.trim().to_string());
        }
        cur_body.push_str(line);
        cur_body.push('\n');
    }
    push_chunk(&mut chunks, &mut cur_body, &mut cur_path);

    if chunks.is_empty() {
        return truncate_utf8(trimmed, budget_bytes);
    }

    chunks.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.idx.cmp(&b.idx)));

    let mut out = String::with_capacity(budget_bytes.min(trimmed.len()));
    let mut omitted: Vec<String> = Vec::new();
    for ch in chunks {
        let rem = budget_bytes.saturating_sub(out.len());
        if rem < 128 {
            if !ch.path.is_empty() {
                omitted.push(ch.path);
            }
            continue;
        }
        if ch.body.len() <= rem {
            out.push_str(&ch.body);
        } else {
            out.push_str(&truncate_utf8(&ch.body, rem));
            out.push('\n');
        }
    }
    if !omitted.is_empty() {
        let list = omitted.into_iter().take(8).collect::<Vec<_>>().join(", ");
        out.push_str(&format!("\n... (omitted lower-priority diffs: {list})"));
    }
    out.trim_end().to_string()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointBisectParams {
    /// Agent (`a1`), terminal (`t1`), remote (`machine/t1`), or unique name.
    pub target: String,
    /// Bug or regression description (e.g. "which turn broke auth tests?").
    pub query: String,
    /// Caller UI language (`zh` / `zh-CN` / `en`).
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BisectAlternative {
    pub turn: u64,
    pub confidence: f32,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointBisectResult {
    pub target: String,
    pub suspect_turn: u64,
    pub recommended_rollback_turn: u64,
    pub confidence: f32,
    pub reason: String,
    #[serde(default)]
    pub evidence_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<BisectAlternative>,
    #[serde(default)]
    pub analyzed_turns: Vec<u64>,
    pub from_cache: bool,
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
    out.push_str("\n… (truncated)");
    out
}

fn detect_prompt_is_zh(prompt: Option<&str>) -> bool {
    prompt.is_some_and(|p| p.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)))
}

/// Cleans model output into a single concise milestone label without quotes or `T4` prefix.
pub fn sanitize_checkpoint_label(raw: &str) -> Option<String> {
    let first_line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut s = first_line
        .trim_matches(|c| matches!(c, '"' | '\'' | '`' | '“' | '”' | '「' | '」' | '《' | '》'))
        .trim()
        .to_string();
    // Strip leading turn tag like "T4 · ", "T4: ", "Turn 4 - " if emitted despite instructions.
    if let Some(rest) = strip_turn_prefix(&s) {
        s = rest;
    }
    s = s.trim_end_matches(['.', '。', ';', '；']).trim().to_string();
    if s.is_empty() {
        return None;
    }
    let clipped: String = s.chars().take(36).collect();
    Some(clipped)
}

fn strip_turn_prefix(s: &str) -> Option<String> {
    let trimmed = s.trim();
    let rest = trimmed
        .strip_prefix("Turn ")
        .or_else(|| trimmed.strip_prefix("turn "))
        .or_else(|| trimmed.strip_prefix('T'))
        .or_else(|| trimmed.strip_prefix('t'))?;
    let digits_len = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits_len == 0 {
        return None;
    }
    let after = rest[digits_len..].trim_start();
    let after = after.trim_start_matches(['·', ':', '：', '-', '—', '|', ' ']).trim();
    (!after.is_empty()).then(|| after.to_string())
}

/// Generates a concise 10–15 char semantic milestone label for a newly captured checkpoint turn.
pub async fn generate_checkpoint_label(
    ai: &AiRuntime,
    commit_oid: &str,
    turn: u64,
    prompt: Option<&str>,
    files: &[CheckpointFileStat],
    patch: &str,
) -> Option<String> {
    if turn == 0 || files.is_empty() || !ai.checkpoint_labeling_enabled() {
        return None;
    }
    let lang = if detect_prompt_is_zh(prompt) { "zh" } else { "en" };
    let file_items: Vec<Value> = files
        .iter()
        .take(15)
        .map(|f| {
            json!({
                "path": f.path,
                "status": f.status.as_deref().unwrap_or("M"),
                "added": f.added,
                "deleted": f.deleted,
            })
        })
        .collect();
    let bounded_patch = truncate_utf8(patch.trim(), LABEL_PATCH_BUDGET);
    let cache_key = format!("ckpt_label:{commit_oid}:{lang}");
    let payload = json!({
        "turn": turn,
        "prompt": prompt.map(|p| truncate_utf8(p.trim(), 240)).unwrap_or_default(),
        "files": file_items,
        "patch": bounded_patch,
    });
    let user_prompt = format_op_user_message("checkpoint_label", lang, &payload);
    let resp = ai
        .complete_bg(AiCompleteRequest {
            op: "checkpoint_label".into(),
            tier: AiTaskTier::Fast,
            mode: AiConversationMode::StatelessSharedPrefix,
            user_prompt,
            max_tokens: 48,
            timeout: LABEL_TIMEOUT,
            cache_key: Some(cache_key),
            cache_ttl: Some(LABEL_CACHE_TTL),
            prompt_cache_bucket: Some("cp".into()),
            ..Default::default()
        })
        .await?;
    sanitize_checkpoint_label(&resp.content)
}

fn keyword_score(meta: &CheckpointMeta, query_tokens: &[String]) -> usize {
    let mut score = 0usize;
    let hay_label = meta.label.as_deref().unwrap_or("").to_ascii_lowercase();
    let hay_prompt = meta.prompt.as_deref().unwrap_or("").to_ascii_lowercase();
    for tok in query_tokens {
        if hay_label.contains(tok) {
            score += 3;
        }
        if hay_prompt.contains(tok) {
            score += 2;
        }
        for f in &meta.files {
            if f.path.to_ascii_lowercase().contains(tok) {
                score += 4;
            }
        }
    }
    score
}

fn extract_query_tokens(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| s.chars().count() >= 2)
        .take(16)
        .collect()
}

/// Computes the safest rollback anchor strictly preceding `suspect_turn` (or `0` for baseline).
pub fn safe_rollback_turn_before(checkpoints: &[CheckpointMeta], suspect_turn: u64) -> u64 {
    checkpoints.iter().filter(|c| c.turn < suspect_turn).map(|c| c.turn).max().unwrap_or(0)
}

/// Runs two-stage fault-introduction bisect over `checkpoints` inside `repo`:
/// - Stage 1 (when > 3 candidate turns): rank top-3 turns from metadata (`label`, `prompt`, `files`).
/// - Stage 2: inspect incremental unified diffs of the top candidates to identify `suspect_turn`.
pub async fn bisect_checkpoints(
    ai: &AiRuntime,
    repo: RepoInfo,
    target_addr: &str,
    checkpoints: &[CheckpointMeta],
    query: &str,
    language: Option<&str>,
) -> Result<CheckpointBisectResult, RpcError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(RpcError::invalid_params("query must not be empty"));
    }
    if !ai.is_configured() {
        return Err(RpcError::new(
            "unavailable",
            "Core AI assistant is not configured",
            "configure an AI endpoint in Settings -> AI Assistant to run checkpoint bisect",
        ));
    }

    let mut ordered: Vec<CheckpointMeta> = checkpoints.to_vec();
    ordered.sort_by_key(|c| c.turn);
    let mut active_turns: Vec<&CheckpointMeta> = ordered.iter().filter(|c| c.turn > 0 && !c.files.is_empty()).collect();
    if active_turns.is_empty() {
        active_turns = ordered.iter().filter(|c| c.turn > 0).collect();
    }
    if active_turns.is_empty() {
        return Err(RpcError::invalid_params(format!("no turn checkpoints recorded for {target_addr} yet")));
    }

    let is_zh = language.map(str::trim).is_some_and(|l| l.to_ascii_lowercase().starts_with("zh"))
        || (language.is_none() && detect_prompt_is_zh(Some(query)));
    let lang = if is_zh { "zh" } else { "en" };

    let valid_turn_nums: Vec<u64> = active_turns.iter().map(|c| c.turn).collect();
    let oids_fp: String =
        active_turns.iter().map(|c| format!("{}:{}", c.turn, c.commit_oid)).collect::<Vec<_>>().join(",");
    let verify_cache_key = format!("ckpt_bisect:{target_addr}:{lang}:{}", fingerprint_hex(&[query, &oids_fp]));

    // Stage 1: narrow to top 3 candidate turns if > 3 turns exist.
    let candidate_turns: Vec<u64> = if active_turns.len() <= 3 {
        valid_turn_nums.clone()
    } else {
        let turns_meta: Vec<Value> = active_turns
            .iter()
            .map(|c| {
                json!({
                    "turn": c.turn,
                    "label": c.label.as_deref().unwrap_or(""),
                    "prompt": c.prompt.as_deref().map(|p| truncate_utf8(p.trim(), 160)).unwrap_or_default(),
                    "files": c.files.iter().take(12).map(|f| format!("{} (+{} -{})", f.path, f.added, f.deleted)).collect::<Vec<_>>(),
                })
            })
            .collect();
        let rank_payload = json!({
            "query": query,
            "turns": turns_meta,
        });
        let rank_prompt = format_op_user_message("checkpoint_bisect_rank", lang, &rank_payload);
        let rank_cache = format!("ckpt_bisect_rank:{target_addr}:{lang}:{}", fingerprint_hex(&[query, &oids_fp]));
        let mut picked: Vec<u64> = Vec::new();
        if let Ok(resp) = ai
            .complete_fg(AiCompleteRequest {
                op: "checkpoint_bisect_rank".into(),
                tier: AiTaskTier::Fast,
                mode: AiConversationMode::StatelessSharedPrefix,
                user_prompt: rank_prompt,
                max_tokens: 140,
                timeout: BISECT_RANK_TIMEOUT,
                cache_key: Some(rank_cache),
                cache_ttl: Some(BISECT_CACHE_TTL),
                prompt_cache_bucket: Some("cp".into()),
                ..Default::default()
            })
            .await
            && let Some(parsed) = extract_json_object(&resp.content)
            && let Some(arr) = parsed["candidates"].as_array()
        {
            for v in arr {
                if let Some(t) = v.as_u64()
                    && valid_turn_nums.contains(&t)
                    && !picked.contains(&t)
                {
                    picked.push(t);
                    if picked.len() >= 3 {
                        break;
                    }
                }
            }
        }
        if picked.is_empty() {
            let tokens = extract_query_tokens(query);
            let mut scored: Vec<(usize, u64)> =
                active_turns.iter().map(|c| (keyword_score(c, &tokens), c.turn)).collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
            picked = scored.into_iter().take(3).map(|(_, t)| t).collect();
        }
        picked
    };

    // Stage 2: load incremental diffs for candidate turns in one blocking task.
    let diff_jobs: Vec<(u64, u64, String, String)> = candidate_turns
        .iter()
        .filter_map(|&to_turn| {
            let to_meta = ordered.iter().find(|c| c.turn == to_turn)?;
            let from_meta = ordered.iter().filter(|c| c.turn < to_turn).max_by_key(|c| c.turn)?;
            Some((from_meta.turn, to_turn, from_meta.ref_name.clone(), to_meta.ref_name.clone()))
        })
        .collect();

    let ctx_tokens = ai.config_snapshot().context_window.as_deref().and_then(crate::ai::parse_ai_token_size);
    let patch_budget = effective_bisect_patch_budget(ctx_tokens, candidate_turns.len());
    let query_owned = query.to_string();
    let repo_bg = repo.clone();
    let patches: Vec<(u64, String)> = tokio::task::spawn_blocking(move || {
        let mut out = Vec::with_capacity(diff_jobs.len());
        for (from_t, to_t, from_ref, to_ref) in diff_jobs {
            let patch = diff_checkpoints(&repo_bg, from_t, to_t, &from_ref, &to_ref)
                .map(|d| select_relevant_patch(&d.patch, &query_owned, patch_budget))
                .unwrap_or_default();
            out.push((to_t, patch));
        }
        out
    })
    .await
    .map_err(|e| RpcError::internal(format!("bisect diff task panicked: {e}")))?;

    let candidates_json: Vec<Value> = candidate_turns
        .iter()
        .filter_map(|&t| {
            let meta = ordered.iter().find(|c| c.turn == t)?;
            let patch = patches.iter().find(|(pt, _)| *pt == t).map(|(_, p)| p.as_str()).unwrap_or("");
            Some(json!({
                "turn": t,
                "label": meta.label.as_deref().unwrap_or(""),
                "prompt": meta.prompt.as_deref().map(|p| truncate_utf8(p.trim(), 200)).unwrap_or_default(),
                "files": meta.files.iter().take(15).map(|f| format!("{} (+{} -{})", f.path, f.added, f.deleted)).collect::<Vec<_>>(),
                "patch": patch,
            }))
        })
        .collect();

    let verify_payload = json!({
        "query": query,
        "candidates": candidates_json,
    });
    let verify_prompt = format_op_user_message("checkpoint_bisect_verify", lang, &verify_payload);
    let resp = ai
        .complete_fg(AiCompleteRequest {
            op: "checkpoint_bisect_verify".into(),
            tier: AiTaskTier::Standard,
            mode: AiConversationMode::StatelessSharedPrefix,
            user_prompt: verify_prompt,
            max_tokens: 320,
            timeout: BISECT_VERIFY_TIMEOUT,
            cache_key: Some(verify_cache_key),
            cache_ttl: Some(BISECT_CACHE_TTL),
            prompt_cache_bucket: Some("cp".into()),
            ..Default::default()
        })
        .await
        .map_err(|e| RpcError::new("unavailable", e, "check Core AI Assistant settings"))?;

    let parsed = extract_json_object(&resp.content).ok_or_else(|| {
        if !resp.from_local_cache {
            ai.record_parse_error("checkpoint_bisect_verify");
        }
        RpcError::new("unavailable", "AI returned unparseable bisect JSON", "retry checkpoint bisect")
    })?;

    let suspect_turn =
        parsed["suspect_turn"].as_u64().filter(|t| valid_turn_nums.contains(t)).unwrap_or(candidate_turns[0]);
    let recommended_rollback_turn = safe_rollback_turn_before(&ordered, suspect_turn);
    let confidence = parsed["confidence"].as_f64().map(|v| (v as f32).clamp(0.0, 1.0)).unwrap_or(0.5);
    let reason = parsed["reason"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(300).collect())
        .unwrap_or_else(|| {
            if is_zh {
                format!("第 {suspect_turn} 轮的改动与描述的问题最相关。")
            } else {
                format!("Turn {suspect_turn} contains the changes most closely matching the reported issue.")
            }
        });
    let evidence_files: Vec<String> = parsed["evidence_files"]
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
    let alternatives: Vec<BisectAlternative> = parsed["alternatives"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let t = v["turn"].as_u64()?;
                    if t == suspect_turn || !valid_turn_nums.contains(&t) {
                        return None;
                    }
                    let c = v["confidence"].as_f64().map(|x| (x as f32).clamp(0.0, 1.0)).unwrap_or(0.2);
                    let r = v["reason"].as_str().map(str::trim).unwrap_or("").chars().take(160).collect();
                    Some(BisectAlternative { turn: t, confidence: c, reason: r })
                })
                .take(2)
                .collect()
        })
        .unwrap_or_default();

    Ok(CheckpointBisectResult {
        target: target_addr.to_string(),
        suspect_turn,
        recommended_rollback_turn,
        confidence,
        reason,
        evidence_files,
        alternatives,
        analyzed_turns: candidate_turns,
        from_cache: resp.from_local_cache,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::AiSettingsSetParams;
    use crate::checkpoint::{capture_checkpoint, checkpoint_ref, probe_repo};
    use crate::paths::Paths;
    use std::path::Path;
    use std::process::Command;
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
                    let mut buf = [0u8; 16384];
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

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@local")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@local")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn sanitize_label_strips_quotes_and_turn_prefix() {
        assert_eq!(
            sanitize_checkpoint_label("「T4 · 拆分存储锁并修复死锁。」").as_deref(),
            Some("拆分存储锁并修复死锁")
        );
        assert_eq!(
            sanitize_checkpoint_label("\"Turn 2: Add token refresh guard\"").as_deref(),
            Some("Add token refresh guard")
        );
        assert_eq!(sanitize_checkpoint_label("   "), None);
    }

    #[tokio::test]
    async fn generates_semantic_label_and_runs_two_stage_bisect() {
        let root = crate::test_tmp().join(format!("h2-bisect-{}", crate::platform::random_hex()));
        let repo_dir = root.join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        git(&repo_dir, &["init", "-b", "main"]);
        std::fs::write(repo_dir.join("auth.rs"), "pub fn check() -> bool { true }\n").unwrap();
        std::fs::write(repo_dir.join("ui.rs"), "pub fn title() -> &'static str { \"hi\" }\n").unwrap();
        git(&repo_dir, &["add", "."]);
        git(&repo_dir, &["commit", "-m", "init"]);

        let repo = probe_repo(&repo_dir).unwrap();
        let mut ckpts = Vec::new();
        let r0 = checkpoint_ref("s", "a1", 0);
        let (c0, h0, t0) = capture_checkpoint(&repo, &r0).unwrap();
        ckpts.push(CheckpointMeta {
            turn: 0,
            session_turn: None,
            ref_name: r0,
            commit_oid: c0,
            head_oid: h0,
            prompt: None,
            label: None,
            label_source: None,
            files: vec![],
            ts_ms: t0,
        });

        // Create 4 turns so two-stage bisect (Stage 1 rank + Stage 2 verify) executes
        let edits = [
            (1u64, "ui.rs", "pub fn title() -> &'static str { \"v1\" }\n", "改标题"),
            (2u64, "auth.rs", "pub fn check() -> bool { false }\n", "继续"),
            (3u64, "ui.rs", "pub fn title() -> &'static str { \"v2\" }\n", "再改下"),
            (4u64, "ui.rs", "pub fn title() -> &'static str { \"v3\" }\n", "调整样式"),
        ];
        for (turn, file, content, prompt) in edits {
            std::fs::write(repo_dir.join(file), content).unwrap();
            let r = checkpoint_ref("s", "a1", turn);
            let (oid, head, ts) = capture_checkpoint(&repo, &r).unwrap();
            ckpts.push(CheckpointMeta {
                turn,
                session_turn: Some(turn),
                ref_name: r,
                commit_oid: oid,
                head_oid: head,
                prompt: Some(prompt.into()),
                label: None,
                label_source: None,
                files: vec![CheckpointFileStat {
                    path: file.into(),
                    status: Some("M".into()),
                    added: 1,
                    deleted: 1,
                    binary: false,
                }],
                ts_ms: ts,
            });
        }

        let paths = Paths {
            socket: root.join("core.sock"),
            holder_socket: root.join("holder.sock"),
            state: root.join("state"),
            config: root.join("config/config.toml"),
        };
        paths.create_dirs().unwrap();
        let rt = AiRuntime::open(&paths).unwrap();
        let url = mock_openai(vec![
            "T2 · 误将鉴权检查改为恒假".into(),
            r#"{"candidates":[2,4],"rationale":"Turn 2 touched auth.rs"}"#.into(),
            r#"{"suspect_turn":2,"confidence":0.93,"reason":"T2 将 auth.rs 中的 check() 返回值从 true 改为 false","evidence_files":["auth.rs"],"alternatives":[]}"#.into(),
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

        let label = generate_checkpoint_label(
            &rt,
            &ckpts[2].commit_oid,
            2,
            Some("继续"),
            &ckpts[2].files,
            "-pub fn check() -> bool { true }\n+pub fn check() -> bool { false }",
        )
        .await
        .unwrap();
        assert_eq!(label, "误将鉴权检查改为恒假");

        let res = bisect_checkpoints(&rt, repo, "a1", &ckpts, "哪一轮导致鉴权测试失败？", Some("zh-CN")).await.unwrap();
        assert_eq!(res.suspect_turn, 2);
        assert_eq!(res.recommended_rollback_turn, 1);
        assert!(res.confidence >= 0.9);
        assert_eq!(res.evidence_files, vec!["auth.rs"]);
        assert_eq!(res.analyzed_turns, vec![2, 4]);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn adaptive_bisect_budget_scales_with_context_window() {
        assert_eq!(effective_bisect_patch_budget(Some(8192), 3), 4 * 1024);
        assert_eq!(effective_bisect_patch_budget(Some(16_384), 3), 8 * 1024);
        assert_eq!(effective_bisect_patch_budget(Some(65_536), 3), 12 * 1024);
        assert_eq!(effective_bisect_patch_budget(None, 2), 16 * 1024);
        assert_eq!(effective_bisect_patch_budget(None, 3), 12 * 1024);
    }

    #[test]
    fn select_relevant_patch_prioritizes_query_matches_over_large_lockfiles() {
        let lock_noise = "+version = \"1.0.0\"\n".repeat(400);
        let patch = format!(
            "diff --git a/Cargo.lock b/Cargo.lock\n--- a/Cargo.lock\n+++ b/Cargo.lock\n@@ -1 +1 @@\n{lock_noise}\
             diff --git a/src/auth.rs b/src/auth.rs\n--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -1 +1 @@\n-pub fn check_auth() -> bool {{ true }}\n+pub fn check_auth() -> bool {{ false }}\n"
        );
        let selected = select_relevant_patch(&patch, "which turn broke auth check_auth?", 1024);
        assert!(selected.contains("src/auth.rs"), "target file must survive budget selection: {selected}");
        assert!(selected.contains("check_auth"), "target hunk must survive budget selection: {selected}");
        assert!(selected.find("src/auth.rs").unwrap() < selected.find("Cargo.lock").unwrap_or(usize::MAX));
    }
}
