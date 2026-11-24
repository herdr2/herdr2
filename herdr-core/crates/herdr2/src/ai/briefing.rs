//! Catch-up Briefing / Attention Inbox aggregator (`ai.briefing`).
//!
//! Aggregates local & remote ACP agents, PTY terminal agents, and Drove DAG runs into
//! four actionable priority buckets:
//! 1. Critical blockers (sorted by transitive downstream tasks blocked)
//! 2. Permission clusters (batch-approvable `>= 2` identical/similar requests) & single decisions
//! 3. Ready to review (completed / verified items with diff hints)
//! 4. Working (compact background progress)
//!
//! Pure deterministic aggregation runs in <1ms with 0 tokens; optional Core AI executive
//! headline (`op = "catchup_briefing"`) layers cleanly on top using the shared KV-cache prefix.

use super::client::extract_json_object;
use super::compact::fingerprint_hex;
use super::digest::clean_bounded;
use super::prompts::format_op_user_message;
use super::{AiCompleteRequest, AiConversationMode, AiRuntime, AiTaskTier};
use crate::drove::types::{Message, Run, Task, TaskStatus};
use crate::permissions::PublicPermission;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::Duration;

const BRIEFING_CACHE_TTL: Duration = Duration::from_secs(60);
const BRIEFING_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BriefingTerminalInput {
    pub address: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub micro_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_status_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_status_kind: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BriefingParams {
    /// Target remote machine (`--on`). Omit to aggregate local + connected machines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    /// Preferred UI language (`zh-CN` / `en`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// How long the user was away (minutes), if triggered from an away-return event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub away_minutes: Option<u32>,
    /// Whether to request an AI executive headline (default true).
    #[serde(default = "default_true")]
    pub include_ai: bool,
    /// Optional desktop-known PTY terminal agent summaries (supplements Core's live records).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminals: Vec<BriefingTerminalInput>,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BriefingCounts {
    pub critical: usize,
    pub needs_you: usize,
    pub batch_groups: usize,
    pub ready_review: usize,
    pub working: usize,
    pub total_active: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BriefingItem {
    /// Stable identifier (`agent:<addr>`, `terminal:<addr>`, `drove_task:<id>`, `drove_msg:<id>`).
    pub id: String,
    /// Kind: `"acp_permission"`, `"acp_error"`, `"acp_done"`, `"acp_working"`,
    /// `"pty_blocked"`, `"pty_done"`, `"pty_working"`, `"drove_blocker"`, `"drove_question"`, `"drove_verified"`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_name: Option<String>,
    pub title: String,
    pub summary: String,
    pub status: String,
    #[serde(default)]
    pub blocked_downstream_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_downstream_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default)]
    pub has_diff: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchPermissionGroup {
    pub cluster_key: String,
    pub permission_kind: String,
    pub title: String,
    pub summary: String,
    pub addresses: Vec<String>,
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BriefingSnapshot {
    pub generated_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    pub headline_from_ai: bool,
    pub counts: BriefingCounts,
    #[serde(default)]
    pub critical_blockers: Vec<BriefingItem>,
    #[serde(default)]
    pub permission_clusters: Vec<BatchPermissionGroup>,
    #[serde(default)]
    pub pending_decisions: Vec<BriefingItem>,
    #[serde(default)]
    pub ready_to_review: Vec<BriefingItem>,
    #[serde(default)]
    pub working_items: Vec<BriefingItem>,
}

#[derive(Clone, Debug)]
pub struct BriefingAgentInput {
    pub address: String,
    pub machine: Option<String>,
    pub display_name: String,
    pub status: String,
    pub status_ms: Option<u64>,
    pub turn: u64,
    pub semantic_title: Option<String>,
    pub micro_summary: Option<String>,
    pub pending_permission: Option<PublicPermission>,
    pub has_diff: bool,
}

#[derive(Clone, Debug)]
pub struct BriefingDroveInput {
    pub run: Run,
    pub tasks: Vec<Task>,
    pub unread_messages: Vec<Message>,
}

/// Computes transitive downstream task keys blocked by each task key in a Drove Run DAG.
pub fn compute_downstream_impact(tasks: &[Task]) -> HashMap<String, Vec<String>> {
    let mut rev: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut active_keys: HashSet<&str> = HashSet::new();
    for t in tasks {
        active_keys.insert(t.key.as_str());
        for dep in &t.deps {
            rev.entry(dep.as_str()).or_default().push(t.key.as_str());
        }
    }
    let mut out = HashMap::new();
    for t in tasks {
        let mut visited: HashSet<&str> = HashSet::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        if let Some(children) = rev.get(t.key.as_str()) {
            for &child in children {
                if visited.insert(child) {
                    queue.push_back(child);
                }
            }
        }
        while let Some(cur) = queue.pop_front() {
            if let Some(children) = rev.get(cur) {
                for &child in children {
                    if visited.insert(child) {
                        queue.push_back(child);
                    }
                }
            }
        }
        let mut list: Vec<String> = visited.into_iter().filter(|k| active_keys.contains(k)).map(String::from).collect();
        list.sort();
        if !list.is_empty() {
            out.insert(t.key.clone(), list);
        }
    }
    out
}

/// Normalizes a permission request into a stable clustering key `(kind, action_stem)`.
pub fn permission_cluster_key(perm: &PublicPermission) -> (String, String) {
    let kind = perm.kind.as_deref().unwrap_or("").trim().to_ascii_lowercase();
    let norm_kind = if kind.is_empty() { "other".to_string() } else { kind };
    let cleaned = perm.title.trim().to_ascii_lowercase().replace(['"', '\'', '`'], "");
    let tokens: Vec<&str> = cleaned.split_whitespace().collect();
    let stem = if tokens.is_empty() {
        norm_kind.clone()
    } else if matches!(tokens[0], "run" | "execute" | "shell" | "bash" | "command") && tokens.len() >= 2 {
        // Group `Run cargo test foo` & `Run cargo test bar` under `run:cargo test`.
        let cmd = tokens.iter().skip(1).take(2).copied().collect::<Vec<_>>().join(" ");
        format!("run:{cmd}")
    } else if matches!(tokens[0], "edit" | "write" | "modify" | "delete" | "create") {
        tokens[0].to_string()
    } else {
        tokens.iter().take(3).copied().collect::<Vec<_>>().join(" ")
    };
    (norm_kind.clone(), format!("{norm_kind}:{stem}"))
}

/// Builds a pure deterministic Catch-up Briefing snapshot in <1ms (0 tokens).
pub fn build_deterministic_briefing(
    agents: &[BriefingAgentInput],
    terminals: &[BriefingTerminalInput],
    drove_runs: &[BriefingDroveInput],
    away_minutes: Option<u32>,
    is_zh: bool,
    now_ms: u64,
) -> BriefingSnapshot {
    let mut critical_blockers: Vec<BriefingItem> = Vec::new();
    let mut single_decisions: Vec<BriefingItem> = Vec::new();
    let mut ready_to_review: Vec<BriefingItem> = Vec::new();
    let mut working_items: Vec<BriefingItem> = Vec::new();

    // 1. Collect ACP permission requests for clustering & classify other ACP states.
    let mut perm_buckets: BTreeMap<String, Vec<&BriefingAgentInput>> = BTreeMap::new();
    for a in agents {
        let title = a.semantic_title.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or(&a.display_name).to_string();
        match a.status.as_str() {
            "error" => {
                critical_blockers.push(BriefingItem {
                    id: format!("agent:{}", a.address),
                    kind: "acp_error".into(),
                    address: Some(a.address.clone()),
                    machine: a.machine.clone(),
                    title,
                    summary: a.micro_summary.clone().unwrap_or_else(|| {
                        if is_zh { "ACP 会话发生错误".into() } else { "ACP session hit an error".into() }
                    }),
                    status: "error".into(),
                    status_ms: a.status_ms,
                    ..Default::default()
                });
            }
            "blocked" => {
                if let Some(perm) = &a.pending_permission {
                    let (_, key) = permission_cluster_key(perm);
                    perm_buckets.entry(key).or_default().push(a);
                } else {
                    single_decisions.push(BriefingItem {
                        id: format!("agent:{}", a.address),
                        kind: "acp_permission".into(),
                        address: Some(a.address.clone()),
                        machine: a.machine.clone(),
                        title,
                        summary: a.micro_summary.clone().unwrap_or_else(|| {
                            if is_zh { "等待人工确认".into() } else { "Waiting for confirmation".into() }
                        }),
                        status: "blocked".into(),
                        status_ms: a.status_ms,
                        ..Default::default()
                    });
                }
            }
            "done" => {
                ready_to_review.push(BriefingItem {
                    id: format!("agent:{}", a.address),
                    kind: "acp_done".into(),
                    address: Some(a.address.clone()),
                    machine: a.machine.clone(),
                    title,
                    summary: a.micro_summary.clone().unwrap_or_else(|| {
                        if is_zh {
                            format!("已完成第 {} 轮，可查看结果", a.turn.max(1))
                        } else {
                            format!("Finished turn {}; ready to review", a.turn.max(1))
                        }
                    }),
                    status: "done".into(),
                    has_diff: a.has_diff,
                    status_ms: a.status_ms,
                    ..Default::default()
                });
            }
            "working" => {
                working_items.push(BriefingItem {
                    id: format!("agent:{}", a.address),
                    kind: "acp_working".into(),
                    address: Some(a.address.clone()),
                    machine: a.machine.clone(),
                    title,
                    summary: a.micro_summary.clone().unwrap_or_else(|| {
                        if is_zh {
                            format!("后台执行中（第 {} 轮）", a.turn.max(1))
                        } else {
                            format!("Running turn {}", a.turn.max(1))
                        }
                    }),
                    status: "working".into(),
                    status_ms: a.status_ms,
                    ..Default::default()
                });
            }
            _ => {}
        }
    }

    let mut permission_clusters: Vec<BatchPermissionGroup> = Vec::new();
    for (cluster_key, group) in perm_buckets {
        if group.len() >= 2 {
            let first_perm = group[0].pending_permission.as_ref().expect("grouped perm");
            let perm_kind = first_perm.kind.clone().unwrap_or_else(|| "other".into());
            let title = clean_bounded(Some(&first_perm.title), 60).unwrap_or_else(|| perm_kind.clone());
            let summary = if is_zh {
                format!("{} 个智能体卡在同类权限审批，可一键合并处理", group.len())
            } else {
                format!("{} agents waiting on similar permission; batch approve available", group.len())
            };
            permission_clusters.push(BatchPermissionGroup {
                cluster_key,
                permission_kind: perm_kind,
                title,
                summary,
                addresses: group.iter().map(|a| a.address.clone()).collect(),
                labels: group
                    .iter()
                    .map(|a| a.semantic_title.clone().unwrap_or_else(|| a.display_name.clone()))
                    .collect(),
            });
        } else if let Some(a) = group.into_iter().next() {
            let perm = a.pending_permission.as_ref().expect("single perm");
            let perm_kind = perm.kind.clone().unwrap_or_else(|| "other".into());
            let title =
                a.semantic_title.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or(&a.display_name).to_string();
            single_decisions.push(BriefingItem {
                id: format!("agent:{}", a.address),
                kind: "acp_permission".into(),
                address: Some(a.address.clone()),
                machine: a.machine.clone(),
                title,
                summary: clean_bounded(Some(&perm.title), 88).unwrap_or_else(|| perm_kind.clone()),
                status: "blocked".into(),
                permission_kind: Some(perm_kind),
                permission_title: Some(perm.title.clone()),
                status_ms: a.status_ms,
                ..Default::default()
            });
        }
    }

    // 2. Merge PTY terminal agents supplied by desktop/core.
    let mut seen_addrs: HashSet<String> = agents.iter().map(|a| a.address.clone()).collect();
    for t in terminals {
        if !seen_addrs.insert(t.address.clone()) {
            continue;
        }
        let machine = t.address.split_once('/').map(|(m, _)| m.to_string());
        let title = t
            .semantic_title
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| (!t.title.trim().is_empty()).then_some(t.title.as_str()))
            .or(t.program_name.as_deref())
            .unwrap_or(&t.address)
            .to_string();
        let detail = t.ai_status_summary.clone().or_else(|| t.micro_summary.clone()).unwrap_or_default();
        match t.status.as_str() {
            "blocked" => {
                let is_err = t.ai_status_kind.as_deref() == Some("error");
                let item = BriefingItem {
                    id: format!("terminal:{}", t.address),
                    kind: if is_err { "acp_error".into() } else { "pty_blocked".into() },
                    address: Some(t.address.clone()),
                    machine,
                    space_name: t.space_name.clone(),
                    title,
                    summary: if !detail.is_empty() {
                        detail
                    } else if is_zh {
                        "终端智能体等待输入或确认".into()
                    } else {
                        "Terminal agent waiting for input".into()
                    },
                    status: "blocked".into(),
                    permission_kind: t.ai_status_kind.clone(),
                    ..Default::default()
                };
                if is_err {
                    critical_blockers.push(item);
                } else {
                    single_decisions.push(item);
                }
            }
            "done" => {
                ready_to_review.push(BriefingItem {
                    id: format!("terminal:{}", t.address),
                    kind: "pty_done".into(),
                    address: Some(t.address.clone()),
                    machine,
                    space_name: t.space_name.clone(),
                    title,
                    summary: if !detail.is_empty() {
                        detail
                    } else if is_zh {
                        "终端任务已完成，待查收".into()
                    } else {
                        "Terminal task completed".into()
                    },
                    status: "done".into(),
                    has_diff: true,
                    ..Default::default()
                });
            }
            "working" => {
                working_items.push(BriefingItem {
                    id: format!("terminal:{}", t.address),
                    kind: "pty_working".into(),
                    address: Some(t.address.clone()),
                    machine,
                    space_name: t.space_name.clone(),
                    title,
                    summary: if !detail.is_empty() {
                        detail
                    } else if is_zh {
                        "终端正在执行".into()
                    } else {
                        "Running in terminal".into()
                    },
                    status: "working".into(),
                    ..Default::default()
                });
            }
            _ => {}
        }
    }

    // 3. Merge Drove DAG runs: downstream impact blockers, unread questions, and verified tasks.
    for dr in drove_runs {
        if dr.run.trashed_at.is_some() {
            continue;
        }
        let impact = compute_downstream_impact(&dr.tasks);
        let run_label = dr.run.name.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or(&dr.run.objective);

        for task in &dr.tasks {
            let blocked_keys = impact.get(&task.key).cloned().unwrap_or_default();
            let downstream = blocked_keys.len();
            match task.status {
                TaskStatus::Failed | TaskStatus::Blocked => {
                    let reason = task
                        .result
                        .as_ref()
                        .and_then(|r| {
                            r.blockers.as_ref().and_then(|b| b.first()).cloned().or_else(|| Some(r.summary.clone()))
                        })
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| {
                            if is_zh { "任务失败或阻塞".into() } else { "Task failed or blocked".into() }
                        });
                    let summary = if downstream > 0 {
                        if is_zh {
                            format!("正阻塞 {downstream} 个下游任务 · {reason}")
                        } else {
                            format!("Blocking {downstream} downstream tasks · {reason}")
                        }
                    } else {
                        reason
                    };
                    critical_blockers.push(BriefingItem {
                        id: format!("drove_task:{}", task.id),
                        kind: "drove_blocker".into(),
                        machine: dr.run.machine.clone(),
                        space_name: clean_bounded(Some(run_label), 28),
                        title: format!("{}: {}", task.key, task.title),
                        summary: clean_bounded(Some(&summary), 100).unwrap_or(summary),
                        status: "error".into(),
                        blocked_downstream_count: downstream,
                        blocked_downstream_keys: blocked_keys,
                        run_id: Some(dr.run.id.clone()),
                        task_key: Some(task.key.clone()),
                        ..Default::default()
                    });
                }
                TaskStatus::Succeeded => {
                    let verified = task.spec.verify.is_some() || dr.run.verified_at.is_some();
                    let summary = task
                        .result
                        .as_ref()
                        .and_then(|r| {
                            r.compact_summary.clone().or_else(|| (!r.summary.is_empty()).then(|| r.summary.clone()))
                        })
                        .unwrap_or_else(|| {
                            if is_zh {
                                "任务已完成并通过校验".into()
                            } else {
                                "Task completed and verified".into()
                            }
                        });
                    ready_to_review.push(BriefingItem {
                        id: format!("drove_task:{}", task.id),
                        kind: "drove_verified".into(),
                        machine: dr.run.machine.clone(),
                        space_name: clean_bounded(Some(run_label), 28),
                        title: format!("{}: {}", task.key, task.title),
                        summary: clean_bounded(Some(&summary), 100).unwrap_or(summary),
                        status: "done".into(),
                        run_id: Some(dr.run.id.clone()),
                        task_key: Some(task.key.clone()),
                        has_diff: verified
                            || task
                                .result
                                .as_ref()
                                .and_then(|r| r.files_modified.as_ref())
                                .is_some_and(|f| !f.is_empty()),
                        ..Default::default()
                    });
                }
                _ => {}
            }
        }

        for msg in &dr.unread_messages {
            if !msg.needs_action || msg.handled_at.is_some() {
                continue;
            }
            let task_key =
                msg.task_id.as_deref().and_then(|tid| dr.tasks.iter().find(|t| t.id == tid).map(|t| t.key.clone()));
            let blocked_keys = task_key.as_deref().and_then(|k| impact.get(k).cloned()).unwrap_or_default();
            let title = (!msg.subject.trim().is_empty())
                .then(|| msg.subject.clone())
                .or_else(|| task_key.as_ref().map(|k| format!("Drove · {k}")))
                .unwrap_or_else(|| if is_zh { "Worker 提问".into() } else { "Worker question".into() });
            let body_text = msg.body.as_deref().unwrap_or(&msg.subject);
            single_decisions.push(BriefingItem {
                id: format!("drove_msg:{}", msg.id),
                kind: "drove_question".into(),
                machine: dr.run.machine.clone(),
                space_name: clean_bounded(Some(run_label), 28),
                title,
                summary: clean_bounded(Some(body_text), 100).unwrap_or_else(|| body_text.to_string()),
                status: "blocked".into(),
                blocked_downstream_count: blocked_keys.len(),
                blocked_downstream_keys: blocked_keys,
                run_id: Some(dr.run.id.clone()),
                task_key,
                message_id: Some(msg.id.clone()),
                ..Default::default()
            });
        }
    }

    // Sort critical blockers by downstream impact desc, then recency.
    critical_blockers.sort_by(|a, b| {
        b.blocked_downstream_count
            .cmp(&a.blocked_downstream_count)
            .then_with(|| b.status_ms.unwrap_or(0).cmp(&a.status_ms.unwrap_or(0)))
            .then_with(|| a.id.cmp(&b.id))
    });
    permission_clusters
        .sort_by(|a, b| b.addresses.len().cmp(&a.addresses.len()).then_with(|| a.cluster_key.cmp(&b.cluster_key)));
    single_decisions.sort_by(|a, b| {
        b.blocked_downstream_count
            .cmp(&a.blocked_downstream_count)
            .then_with(|| b.status_ms.unwrap_or(0).cmp(&a.status_ms.unwrap_or(0)))
            .then_with(|| a.id.cmp(&b.id))
    });
    ready_to_review
        .sort_by(|a, b| b.status_ms.unwrap_or(0).cmp(&a.status_ms.unwrap_or(0)).then_with(|| a.id.cmp(&b.id)));

    let clustered_count: usize = permission_clusters.iter().map(|g| g.addresses.len()).sum();
    let counts = BriefingCounts {
        critical: critical_blockers.len(),
        needs_you: clustered_count + single_decisions.len(),
        batch_groups: permission_clusters.len(),
        ready_review: ready_to_review.len(),
        working: working_items.len(),
        total_active: critical_blockers.len()
            + clustered_count
            + single_decisions.len()
            + ready_to_review.len()
            + working_items.len(),
    };

    let headline = Some(deterministic_headline(&counts, &critical_blockers, &permission_clusters, away_minutes, is_zh));

    BriefingSnapshot {
        generated_ms: now_ms,
        headline,
        headline_from_ai: false,
        counts,
        critical_blockers,
        permission_clusters,
        pending_decisions: single_decisions,
        ready_to_review,
        working_items,
    }
}

fn deterministic_headline(
    counts: &BriefingCounts,
    blockers: &[BriefingItem],
    clusters: &[BatchPermissionGroup],
    away_minutes: Option<u32>,
    is_zh: bool,
) -> String {
    if counts.critical == 0 && counts.needs_you == 0 && counts.ready_review == 0 {
        return if counts.working > 0 {
            if is_zh {
                format!("当前有 {} 个智能体正在平稳推进，暂无待处理阻塞。", counts.working)
            } else {
                format!("{} agents running smoothly with no blockers.", counts.working)
            }
        } else if is_zh {
            "所有智能体均空闲，暂无待处理事项。".into()
        } else {
            "All agents are idle; nothing needs attention.".into()
        };
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(mins) = away_minutes.filter(|&m| m > 0) {
        parts.push(if is_zh { format!("离开约 {mins} 分钟期间") } else { format!("While away ({mins}m)") });
    }
    if counts.critical > 0 {
        let top_ds = blockers.first().map(|b| b.blocked_downstream_count).unwrap_or(0);
        if top_ds > 0 {
            parts.push(if is_zh {
                format!("{} 项异常阻塞（最高卡住 {} 个下游任务）", counts.critical, top_ds)
            } else {
                format!("{} critical blockers (top blocking {} downstream)", counts.critical, top_ds)
            });
        } else {
            parts.push(if is_zh {
                format!("{} 项异常待优先排查", counts.critical)
            } else {
                format!("{} critical errors need attention", counts.critical)
            });
        }
    }
    if counts.needs_you > 0 {
        if let Some(g) = clusters.first() {
            parts.push(if is_zh {
                format!(
                    "{} 项待确认（含 {} 组可合并批复，最多 {} 个同类审批）",
                    counts.needs_you,
                    clusters.len(),
                    g.addresses.len()
                )
            } else {
                format!(
                    "{} waiting on you ({} batchable group(s), up to {} similar)",
                    counts.needs_you,
                    clusters.len(),
                    g.addresses.len()
                )
            });
        } else {
            parts.push(if is_zh {
                format!("{} 项等待你拍板", counts.needs_you)
            } else {
                format!("{} items need your decision", counts.needs_you)
            });
        }
    }
    if counts.ready_review > 0 {
        parts.push(if is_zh {
            format!("{} 项已完成可检视 Diff", counts.ready_review)
        } else {
            format!("{} completed and ready to review", counts.ready_review)
        });
    }
    if is_zh { format!("{}。", parts.join("；")) } else { format!("{}.", parts.join("; ")) }
}

/// Optionally upgrades `snapshot.headline` via shared Core AI (`op = "catchup_briefing"`).
pub async fn enhance_briefing_headline(
    ai: &AiRuntime,
    snapshot: &mut BriefingSnapshot,
    away_minutes: Option<u32>,
    language: Option<&str>,
) {
    let cfg = ai.config_snapshot();
    if !cfg.enabled
        || !cfg.semantic_summary
        || cfg.base_url.trim().is_empty()
        || ai.is_circuit_open()
        || snapshot.counts.total_active == 0
    {
        return;
    }

    let lang = if language.is_some_and(|l| l.to_ascii_lowercase().starts_with("zh")) { "zh" } else { "en" };
    let top_blockers: Vec<Value> = snapshot
        .critical_blockers
        .iter()
        .take(4)
        .map(|b| json!({"title": b.title, "summary": b.summary, "downstream": b.blocked_downstream_count}))
        .collect();
    let clusters: Vec<Value> = snapshot
        .permission_clusters
        .iter()
        .take(3)
        .map(|g| json!({"kind": g.permission_kind, "title": g.title, "count": g.addresses.len()}))
        .collect();
    let ready: Vec<Value> =
        snapshot.ready_to_review.iter().take(4).map(|r| json!({"title": r.title, "summary": r.summary})).collect();

    let payload = json!({
        "away_minutes": away_minutes,
        "counts": snapshot.counts,
        "top_blockers": top_blockers,
        "permission_clusters": clusters,
        "ready_to_review": ready,
    });
    let payload_str = payload.to_string();
    let cache_key = format!("catchup_briefing:{lang}:{}", fingerprint_hex(&[&payload_str]));
    let user_prompt = format_op_user_message("catchup_briefing", lang, &payload);

    if let Ok(resp) = ai
        .complete_fg(AiCompleteRequest {
            op: "catchup_briefing".into(),
            tier: AiTaskTier::Nano,
            mode: AiConversationMode::StatelessSharedPrefix,
            user_prompt,
            max_tokens: 512,
            timeout: BRIEFING_TIMEOUT,
            cache_key: Some(cache_key),
            cache_ttl: Some(BRIEFING_CACHE_TTL),
            prompt_cache_bucket: Some("cp".into()),
            ..Default::default()
        })
        .await
    {
        if let Some(parsed) = extract_json_object(&resp.content)
            && let Some(headline) = clean_bounded(parsed["headline"].as_str(), 220)
        {
            snapshot.headline = Some(headline);
            snapshot.headline_from_ai = true;
        } else if !resp.from_local_cache {
            ai.record_parse_error("catchup_briefing");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drove::types::{CoordinatorInfo, Isolation, RunStatus, Spec};

    fn make_task(key: &str, deps: &[&str], status: TaskStatus) -> Task {
        Task {
            id: format!("task_{key}"),
            run_id: "run_1".into(),
            key: key.into(),
            title: format!("Task {key}"),
            phase: None,
            spec: Spec {
                target: String::new(),
                change: String::new(),
                constraints: None,
                ownership: None,
                acceptance: String::new(),
                verify: None,
            },
            tags: vec![],
            files: vec![],
            deps: deps.iter().map(|s| s.to_string()).collect(),
            output_schema: None,
            isolation: Isolation::None,
            timeout_sec: None,
            stall_warn_sec: None,
            stall_kill_sec: None,
            when: None,
            assign: None,
            status,
            result: None,
            failures: 0,
            created_at: String::new(),
            updated_at: String::new(),
            kind: None,
            exec_phase: None,
            execution_version: None,
            trigger_rule: None,
            context: None,
            exec: None,
            foreach_from: None,
            template: None,
            expansion_state: None,
            expanded_from: None,
            review: None,
            review_state: None,
            candidate_result: None,
            cache: None,
            cached_from: None,
            retry_on_worker_reported: None,
        }
    }

    #[test]
    fn computes_transitive_downstream_impact() {
        let tasks = vec![
            make_task("schema", &[], TaskStatus::Failed),
            make_task("api", &["schema"], TaskStatus::Pending),
            make_task("ui", &["schema"], TaskStatus::Pending),
            make_task("e2e", &["api", "ui"], TaskStatus::Pending),
            make_task("docs", &[], TaskStatus::Failed),
        ];
        let impact = compute_downstream_impact(&tasks);
        assert_eq!(impact.get("schema").map(Vec::len), Some(3));
        assert_eq!(impact.get("api").map(Vec::len), Some(1));
        assert_eq!(impact.get("docs"), None);
    }

    #[test]
    fn clusters_similar_permissions_and_sorts_blockers_by_impact() {
        let agents = vec![
            BriefingAgentInput {
                address: "a1".into(),
                machine: None,
                display_name: "worker-1".into(),
                status: "blocked".into(),
                status_ms: Some(100),
                turn: 2,
                semantic_title: Some("Auth tests".into()),
                micro_summary: None,
                pending_permission: Some(PublicPermission {
                    request_id: "r1".into(),
                    session_id: "s1".into(),
                    title: "Run cargo test auth".into(),
                    kind: Some("execute".into()),
                    options: vec![],
                }),
                has_diff: false,
            },
            BriefingAgentInput {
                address: "wsl/a2".into(),
                machine: Some("wsl".into()),
                display_name: "worker-2".into(),
                status: "blocked".into(),
                status_ms: Some(200),
                turn: 1,
                semantic_title: Some("Storage tests".into()),
                micro_summary: None,
                pending_permission: Some(PublicPermission {
                    request_id: "r2".into(),
                    session_id: "s2".into(),
                    title: "Run cargo test storage".into(),
                    kind: Some("execute".into()),
                    options: vec![],
                }),
                has_diff: false,
            },
            BriefingAgentInput {
                address: "a3".into(),
                machine: None,
                display_name: "reviewer".into(),
                status: "done".into(),
                status_ms: Some(300),
                turn: 3,
                semantic_title: Some("Code review".into()),
                micro_summary: Some("Verified auth diff cleanly".into()),
                pending_permission: None,
                has_diff: true,
            },
        ];

        let run = Run {
            id: "run_1".into(),
            objective: "Refactor API".into(),
            cwd: "/repo".into(),
            coordinator: CoordinatorInfo {
                host: "local".into(),
                host_session: "t1".into(),
                generation: 1,
                agent: None,
                model: None,
                host_pid: None,
                session_bound: None,
                notification: None,
            },
            max_concurrency: 4,
            budget: None,
            status: RunStatus::Active,
            pause_reason: None,
            verify: None,
            goal: None,
            finalization: None,
            verified_at: None,
            name: Some("API Refactor".into()),
            order: None,
            trashed_at: None,
            status_before_trash: None,
            ai_usage: None,
            machine: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let drove = vec![BriefingDroveInput {
            run,
            tasks: vec![
                make_task("docs", &[], TaskStatus::Failed),
                make_task("schema", &[], TaskStatus::Failed),
                make_task("api", &["schema"], TaskStatus::Pending),
                make_task("e2e", &["api"], TaskStatus::Pending),
            ],
            unread_messages: vec![],
        }];

        let snap = build_deterministic_briefing(&agents, &[], &drove, Some(15), true, 1000);
        assert_eq!(snap.permission_clusters.len(), 1);
        assert_eq!(snap.permission_clusters[0].addresses, vec!["a1", "wsl/a2"]);
        assert_eq!(snap.critical_blockers.len(), 2);
        assert_eq!(snap.critical_blockers[0].task_key.as_deref(), Some("schema"));
        assert_eq!(snap.critical_blockers[0].blocked_downstream_count, 2);
        assert_eq!(snap.ready_to_review.len(), 1);
        assert!(snap.headline.as_deref().unwrap().contains("离开约 15 分钟"));
    }
}
