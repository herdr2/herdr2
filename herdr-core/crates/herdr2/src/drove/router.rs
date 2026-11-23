//! Role-first task router (`strengths` matching -> `primary` -> `fallbacks`, Magpie quota skip, + warm worker affinity reuse).

use super::config::{default_roles, resolve_role_config};
use super::magpie::quota_exhausted_reason;
use super::types::*;

#[derive(Clone, Debug)]
pub struct RouteDecision {
    pub role: String,
    pub role_config: RoleConfig,
    pub agent: String,
    pub model: Option<String>,
    pub context: Option<String>,
    pub effort: Effort,
    pub fast: bool,
    pub reason: String,
    pub warm_worker: Option<Worker>,
}

/// Resolves which role, agent/model/effort, and optional warm worker should execute `task`.
#[allow(dead_code)]
pub fn route_task(
    cfg: &DroveConfig,
    task: &Task,
    effective_cwd: &str,
    workers: &[Worker],
    probed: &ProbeSnapshot,
) -> RouteDecision {
    route_task_with_quotas(cfg, task, effective_cwd, workers, probed, None)
}

/// Resolves routing for `task` while skipping quota-exhausted candidates when Magpie data is available.
pub fn route_task_with_quotas(
    cfg: &DroveConfig,
    task: &Task,
    effective_cwd: &str,
    workers: &[Worker],
    probed: &ProbeSnapshot,
    quotas: Option<&[QuotaCard]>,
) -> RouteDecision {
    route_task_with_preference(cfg, task, effective_cwd, workers, probed, quotas, None)
}

/// Resolves routing for `task`, honoring an optional AI-selected candidate index `(chain_idx, reason)`
/// when deterministic strength matching had no unique winner.
pub fn route_task_with_preference(
    cfg: &DroveConfig,
    task: &Task,
    effective_cwd: &str,
    workers: &[Worker],
    probed: &ProbeSnapshot,
    quotas: Option<&[QuotaCard]>,
    preferred_candidate: Option<(usize, String)>,
) -> RouteDecision {
    let assign = task.assign.as_ref();
    let role_name =
        assign.and_then(|a| a.role.as_deref()).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("coder").to_string();

    let roles_fallback = default_roles();
    let role_cfg =
        resolve_role_config(cfg, &role_name).or_else(|| roles_fallback.get("coder").cloned()).unwrap_or(RoleConfig {
            label: Some(role_name.clone()),
            summary: None,
            access: Some(RoleAccessMode::Writable),
            allowed_paths: Vec::new(),
            primary: RoleTarget {
                agent: "claude".into(),
                model: None,
                effort: Effort::High,
                fast: false,
                context: None,
                strengths: Vec::new(),
                note: None,
            },
            fallbacks: Vec::new(),
            prompt: None,
        });

    let is_router_assigned = assign.and_then(|a| a.source.as_deref()) == Some("router");
    let explicit_agent = if is_router_assigned {
        None
    } else {
        assign.and_then(|a| a.agent.as_deref()).map(str::trim).filter(|s| !s.is_empty())
    };
    let explicit_model =
        if is_router_assigned { None } else { assign.and_then(|a| a.model.clone()).filter(|s| !s.trim().is_empty()) };
    let explicit_effort = if is_router_assigned { None } else { assign.and_then(|a| a.effort) };
    let explicit_fast = if is_router_assigned { None } else { assign.and_then(|a| a.fast) };

    let (chosen_agent, chosen_model, chosen_context, chosen_effort, chosen_fast, reason) =
        if let Some(agent) = explicit_agent {
            let same_as_primary = agent == role_cfg.primary.agent;
            let default_fast = if same_as_primary { role_cfg.primary.fast } else { false };
            let default_context = if same_as_primary {
                role_cfg.primary.context.clone()
            } else {
                role_cfg.fallbacks.iter().find(|f| f.agent == agent).and_then(|f| f.context.clone())
            };
            (
                agent.to_string(),
                explicit_model.or_else(|| role_cfg.primary.model.clone()),
                default_context,
                explicit_effort.unwrap_or(role_cfg.primary.effort),
                explicit_fast.unwrap_or(default_fast),
                format!("user_override role={role_name}"),
            )
        } else {
            pick_candidate(
                &role_name,
                &role_cfg,
                task,
                workers,
                probed,
                quotas,
                explicit_model,
                explicit_effort,
                explicit_fast,
                task.failures,
                preferred_candidate,
            )
        };

    // Prefer an explicit user-requested worker id if idle and matching cwd/agent;
    // ignore router-populated `assign.worker` so fallback retries can switch agents cleanly.
    let explicit_worker = if is_router_assigned { None } else { assign.and_then(|a| a.worker.as_deref()) };
    let warm_worker = if let Some(wid) = explicit_worker {
        workers
            .iter()
            .find(|w| {
                w.id == wid
                    && w.state == WorkerState::Idle
                    && w.read_only_dir.is_none()
                    && worker_matches_isolation(w, task, effective_cwd)
                    && w.agent == chosen_agent
                    && w.h2_agent.is_some()
            })
            .cloned()
            .or_else(|| {
                find_warm_worker(workers, &role_name, &chosen_agent, chosen_model.as_deref(), effective_cwd, task)
            })
    } else {
        find_warm_worker(workers, &role_name, &chosen_agent, chosen_model.as_deref(), effective_cwd, task)
    };

    RouteDecision {
        role: role_name,
        role_config: role_cfg,
        agent: chosen_agent,
        model: chosen_model,
        context: chosen_context,
        effort: chosen_effort,
        fast: chosen_fast,
        reason,
        warm_worker,
    }
}

/// Returns true when either the global Drove routing `note` or any candidate's `note` is non-empty.
pub fn has_routing_notes(cfg: &DroveConfig, candidates: &[&RoleTarget]) -> bool {
    cfg.note.as_deref().is_some_and(|s| !s.trim().is_empty())
        || candidates.iter().any(|c| c.note.as_deref().is_some_and(|s| !s.trim().is_empty()))
}

/// Returns the eligible candidate pool when AI candidate selection should run on a first attempt (`failures == 0`):
/// - Always when a global `cfg.note` or any candidate `note` is configured and >= 2 candidates are eligible
///   (so AI evaluates time-window / quota / cost rules before launching a sub-agent), OR
/// - When candidates configure `strengths` and deterministic keyword matching had no unique winner.
pub fn candidate_pool_for_ai(
    cfg: &DroveConfig,
    task: &Task,
    workers: &[Worker],
    probed: &ProbeSnapshot,
    quotas: Option<&[QuotaCard]>,
) -> Option<(String, Vec<(usize, RoleTarget)>)> {
    if task.failures > 0 {
        return None;
    }
    let assign = task.assign.as_ref();
    let is_router_assigned = assign.and_then(|a| a.source.as_deref()) == Some("router");
    let explicit_agent = if is_router_assigned {
        None
    } else {
        assign.and_then(|a| a.agent.as_deref()).map(str::trim).filter(|s| !s.is_empty())
    };
    if explicit_agent.is_some() {
        return None;
    }
    let role_name =
        assign.and_then(|a| a.role.as_deref()).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("coder").to_string();
    let role_cfg = resolve_role_config(cfg, &role_name)?;
    let explicit_model =
        if is_router_assigned { None } else { assign.and_then(|a| a.model.clone()).filter(|s| !s.trim().is_empty()) };
    let (pool, _, _, _) = build_candidate_pool(&role_cfg, workers, probed, quotas, explicit_model.as_deref());
    if pool.len() < 2 {
        return None;
    }
    let cand_refs: Vec<&RoleTarget> = pool.iter().map(|(_, c)| *c).collect();
    if !has_routing_notes(cfg, &cand_refs) {
        if !pool.iter().any(|(_, c)| !c.strengths.is_empty()) {
            return None;
        }
        let scores: Vec<u32> = pool.iter().map(|(_, c)| score_candidate_strengths(c, task).0).collect();
        let max_score = scores.iter().copied().max().unwrap_or(0);
        let top_count = scores.iter().filter(|&&s| s == max_score).count();
        // Only invoke AI when deterministic matching had no hit or tied at the top.
        if max_score > 0 && top_count == 1 {
            return None;
        }
    }
    Some((role_name, pool.into_iter().map(|(idx, c)| (idx, c.clone())).collect()))
}

fn build_candidate_pool<'a>(
    role_cfg: &'a RoleConfig,
    workers: &[Worker],
    probed: &ProbeSnapshot,
    quotas: Option<&[QuotaCard]>,
    explicit_model: Option<&str>,
) -> (Vec<(usize, &'a RoleTarget)>, Option<(String, String)>, bool, bool) {
    let mut chain = Vec::with_capacity(1 + role_cfg.fallbacks.len());
    chain.push(&role_cfg.primary);
    for fb in &role_cfg.fallbacks {
        chain.push(fb);
    }

    let any_installed = probed.agents.iter().any(|a| a.installed);
    let eligible: Vec<(usize, &RoleTarget)> = chain
        .into_iter()
        .enumerate()
        .filter(|(_, cand)| {
            if !any_installed {
                return true;
            }
            let installed = probed.agents.iter().any(|a| a.id == cand.agent && a.installed);
            let custom_spec = cand.agent.contains(' ') || cand.agent.contains('/');
            installed || custom_spec
        })
        .collect();

    if eligible.is_empty() {
        return (Vec::new(), None, false, any_installed);
    }

    let mut first_skipped: Option<(String, String)> = None;
    let mut available: Vec<(usize, &RoleTarget)> = Vec::new();
    if let Some(cards) = quotas.filter(|c| !c.is_empty()) {
        for &(idx, cand) in &eligible {
            let eff_model = explicit_model.or(cand.model.as_deref());
            if let Some(q_reason) = quota_exhausted_reason(&cand.agent, eff_model, cards) {
                if first_skipped.is_none() {
                    first_skipped = Some((cand.agent.clone(), q_reason));
                }
                continue;
            }
            available.push((idx, cand));
        }
    }

    let all_exhausted = quotas.is_some_and(|c| !c.is_empty()) && available.is_empty();
    let base_pool = if !available.is_empty() { available } else { eligible };
    let healthy: Vec<(usize, &RoleTarget)> = base_pool
        .iter()
        .copied()
        .filter(|(_, cand)| {
            let eff_model = explicit_model.or(cand.model.as_deref());
            let has_dead = workers
                .iter()
                .any(|w| w.state == WorkerState::Dead && w.agent == cand.agent && w.model.as_deref() == eff_model);
            let has_healthy = workers.iter().any(|w| {
                matches!(w.state, WorkerState::Idle | WorkerState::Busy)
                    && w.agent == cand.agent
                    && w.model.as_deref() == eff_model
            });
            !has_dead || has_healthy
        })
        .collect();
    let pool = if !healthy.is_empty() { healthy } else { base_pool };
    (pool, first_skipped, all_exhausted, any_installed)
}

/// Bilingual domain synonym groups used to match candidate `strengths` tags against task context.
const STRENGTH_GROUPS: &[(&[&str], &[&str])] = &[
    (
        &[
            "前端",
            "ui",
            "ux",
            "界面",
            "页面",
            "样式",
            "布局",
            "组件",
            "frontend",
            "web",
            "css",
            "react",
            "vue",
            "svelte",
            "html",
            "tailwind",
            "gpui",
            "设计稿",
        ],
        &[
            "前端", "ui", "ux", "界面", "页面", "样式", "布局", "组件", "按钮", "弹窗", "面板", "主题", "frontend",
            "css", "scss", "html", "tsx", "jsx", "vue", "svelte", "react", "tailwind", "gpui", "svg", "layout",
            "render", "style", "modal", "dialog", "button", "panel", "dropdown",
        ],
    ),
    (
        &[
            "后端",
            "服务端",
            "接口",
            "数据库",
            "微服务",
            "backend",
            "server",
            "api",
            "rpc",
            "sql",
            "database",
            "db",
            "storage",
            "daemon",
        ],
        &[
            "后端",
            "服务端",
            "接口",
            "数据库",
            "存储",
            "协议",
            "并发",
            "调度",
            "backend",
            "server",
            "api",
            "rpc",
            "http",
            "sql",
            "sqlite",
            "postgres",
            "redis",
            "schema",
            "migration",
            "storage",
            "daemon",
            "service",
            "endpoint",
            "auth",
            "middleware",
        ],
    ),
    (
        &[
            "架构",
            "重构",
            "复杂",
            "核心",
            "系统设计",
            "architecture",
            "refactor",
            "design",
            "complex",
            "core",
            "system",
            "large",
        ],
        &[
            "架构",
            "重构",
            "复杂",
            "核心",
            "解耦",
            "模块",
            "系统",
            "architecture",
            "refactor",
            "redesign",
            "complex",
            "decouple",
            "engine",
            "scheduler",
            "pipeline",
            "framework",
            "protocol",
        ],
    ),
    (
        &[
            "排障",
            "调试",
            "修bug",
            "修复",
            "定位",
            "故障",
            "bug",
            "bugfix",
            "fix",
            "debug",
            "crash",
            "troubleshoot",
            "hotfix",
            "error",
        ],
        &[
            "排障",
            "调试",
            "修复",
            "报错",
            "崩溃",
            "异常",
            "死锁",
            "卡死",
            "回归",
            "bug",
            "fix",
            "debug",
            "crash",
            "panic",
            "error",
            "broken",
            "hang",
            "deadlock",
            "leak",
            "regression",
            "hotfix",
            "diagnose",
            "trace",
        ],
    ),
    (
        &[
            "快速",
            "简单",
            "小改动",
            "脚本",
            "批处理",
            "日常",
            "quick",
            "simple",
            "fast",
            "small",
            "script",
            "chore",
            "routine",
            "trivial",
        ],
        &[
            "快速",
            "简单",
            "小改动",
            "微调",
            "脚本",
            "格式化",
            "清理",
            "quick",
            "simple",
            "minor",
            "typo",
            "script",
            "chore",
            "lint",
            "format",
            "cleanup",
            "rename",
            "bump",
        ],
    ),
    (
        &[
            "测试",
            "单测",
            "集成测试",
            "回归",
            "验证",
            "压测",
            "e2e",
            "qa",
            "test",
            "testing",
            "unit",
            "integration",
            "benchmark",
            "fuzz",
        ],
        &[
            "测试",
            "单测",
            "集成测试",
            "回归",
            "用例",
            "覆盖率",
            "断言",
            "压测",
            "e2e",
            "qa",
            "test",
            "tests",
            "spec",
            "unit",
            "integration",
            "benchmark",
            "fuzz",
            "assert",
            "mock",
            "fixture",
            "verify",
        ],
    ),
    (
        &["文档", "写作", "博客", "说明", "教程", "doc", "docs", "writing", "markdown", "readme", "changelog"],
        &[
            "文档",
            "写作",
            "博客",
            "说明",
            "教程",
            "注释",
            "手册",
            "doc",
            "docs",
            "readme",
            "markdown",
            ".md",
            "mdx",
            "changelog",
            "guide",
            "tutorial",
            "skill",
        ],
    ),
    (
        &[
            "绘图",
            "画图",
            "架构图",
            "流程图",
            "时序图",
            "矢量",
            "配图",
            "线框",
            "图表",
            "diagram",
            "flowchart",
            "sequence",
            "svg",
            "mermaid",
            "drawio",
            "excalidraw",
            "d2",
            "chart",
            "illustration",
            "visual",
            "wireframe",
        ],
        &[
            "绘图",
            "画图",
            "架构图",
            "流程图",
            "时序图",
            "状态图",
            "类图",
            "矢量",
            "配图",
            "线框",
            "原型",
            "图表",
            "可视化",
            "diagram",
            "flowchart",
            "sequence",
            "svg",
            ".svg",
            "mermaid",
            ".mmd",
            "puml",
            "drawio",
            "excalidraw",
            ".d2",
            "chart",
            "illustration",
            "wireframe",
            "canvas",
        ],
    ),
    (
        &["审查", "安全", "审计", "性能", "优化", "review", "security", "audit", "perf", "performance", "concurrency"],
        &[
            "审查",
            "安全",
            "审计",
            "漏洞",
            "权限",
            "性能",
            "耗时",
            "内存",
            "并发",
            "review",
            "security",
            "audit",
            "vuln",
            "auth",
            "permission",
            "perf",
            "performance",
            "latency",
            "memory",
            "race",
            "lock",
        ],
    ),
    (
        &[
            "调研",
            "源码",
            "分析",
            "长上下文",
            "全仓",
            "摸底",
            "总结",
            "research",
            "analysis",
            "explore",
            "codebase",
            "long-context",
            "context",
            "investigate",
        ],
        &[
            "调研",
            "源码",
            "分析",
            "排查",
            "摸底",
            "梳理",
            "对比",
            "方案",
            "全仓",
            "长上下文",
            "research",
            "analyze",
            "analysis",
            "explore",
            "investigate",
            "compare",
            "survey",
            "codebase",
            "grep",
            "log",
        ],
    ),
    (
        &["算法", "数学", "推理", "algorithm", "math", "reasoning", "optimization"],
        &[
            "算法",
            "数学",
            "推理",
            "排序",
            "图",
            "dag",
            "调度",
            "algorithm",
            "math",
            "graph",
            "dag",
            "heuristic",
            "solver",
            "dp",
        ],
    ),
    (
        &[
            "android",
            "aosp",
            "移动端",
            "内核",
            "驱动",
            "c++",
            "cpp",
            "native",
            "mobile",
            "ios",
            "kotlin",
            "swift",
            "kernel",
            "hal",
        ],
        &[
            "android",
            "aosp",
            "apk",
            "adb",
            "hal",
            "binder",
            "framework",
            "kernel",
            "driver",
            "jni",
            "cpp",
            "c++",
            "kotlin",
            "swift",
            "ios",
            "mobile",
            "ndk",
            "aidl",
            "sepolicy",
        ],
    ),
];

/// Scores how well a candidate's configured `strengths` match `task`.
/// Returns `(score, matched_strength_labels)`.
pub(crate) fn score_candidate_strengths(cand: &RoleTarget, task: &Task) -> (u32, Vec<String>) {
    if cand.strengths.is_empty() {
        return (0, Vec::new());
    }
    let mut parts: Vec<&str> =
        vec![task.title.as_str(), task.spec.target.as_str(), task.spec.change.as_str(), task.spec.acceptance.as_str()];
    if let Some(c) = task.spec.constraints.as_deref() {
        parts.push(c);
    }
    for t in &task.tags {
        parts.push(t.as_str());
    }
    for f in &task.files {
        parts.push(f.as_str());
    }
    let haystack = parts.join(" ").to_lowercase();
    let tag_haystack = task.tags.iter().map(|t| t.to_lowercase()).collect::<Vec<_>>();

    let mut total_score = 0u32;
    let mut matched_labels = Vec::new();

    for strength in &cand.strengths {
        let s_lower = strength.trim().to_lowercase();
        if s_lower.is_empty() {
            continue;
        }
        let mut s_score = 0u32;

        // 1. Direct tag match (highest confidence: coordinator explicitly tagged the task).
        if tag_haystack.iter().any(|tg| tg == &s_lower || tg.contains(&s_lower) || s_lower.contains(tg.as_str())) {
            s_score += 16;
        }

        // 2. Direct full phrase hit in task title/spec/files.
        if haystack.contains(&s_lower) {
            s_score += 10;
        } else {
            // 3. Split compound strength phrases (e.g. "前端/UI", "后端 API", "bugfix & debug") into tokens.
            let tokens: Vec<&str> = s_lower
                .split(|c: char| {
                    matches!(c, '/' | ',' | '、' | ';' | '；' | '|' | '&' | '+' | ' ' | '\t' | '(' | ')' | '（' | '）')
                })
                .map(str::trim)
                .filter(|tok| tok.chars().count() >= 2)
                .collect();
            for tok in &tokens {
                if haystack.contains(tok) {
                    s_score += 6;
                }
            }
        }

        // 4. Domain synonym / file-extension group match.
        for &(strength_kws, task_kws) in STRENGTH_GROUPS {
            let strength_in_group = strength_kws.iter().any(|kw| s_lower.contains(kw));
            if strength_in_group {
                let hits = task_kws.iter().filter(|kw| haystack.contains(*kw)).count();
                if hits > 0 {
                    s_score += (hits.min(3) as u32) * 3;
                }
            }
        }

        if s_score > 0 {
            total_score += s_score;
            matched_labels.push(strength.clone());
        }
    }

    (total_score, matched_labels)
}

#[allow(clippy::too_many_arguments)]
fn pick_candidate(
    role_name: &str,
    role_cfg: &RoleConfig,
    task: &Task,
    workers: &[Worker],
    probed: &ProbeSnapshot,
    quotas: Option<&[QuotaCard]>,
    explicit_model: Option<String>,
    explicit_effort: Option<Effort>,
    explicit_fast: Option<bool>,
    failures: u32,
    preferred_candidate: Option<(usize, String)>,
) -> (String, Option<String>, Option<String>, Effort, bool, String) {
    let (pool, first_skipped, all_exhausted, any_installed) =
        build_candidate_pool(role_cfg, workers, probed, quotas, explicit_model.as_deref());

    if !pool.is_empty() {
        let mut ranked: Vec<(usize, &RoleTarget, u32, Vec<String>, Option<String>)> = pool
            .into_iter()
            .map(|(idx, cand)| {
                let (score, matched) = score_candidate_strengths(cand, task);
                let ai_why =
                    preferred_candidate.as_ref().and_then(|(pref_idx, why)| (*pref_idx == idx).then(|| why.clone()));
                (idx, cand, score, matched, ai_why)
            })
            .collect();

        // Rank by: AI tie-break preference (if any) -> deterministic strength score -> original #1/#2/#3 order.
        let has_specialty_signal = ranked.iter().any(|(_, _, sc, _, ai)| *sc > 0 || ai.is_some());
        if has_specialty_signal {
            ranked.sort_by(|a, b| b.4.is_some().cmp(&a.4.is_some()).then_with(|| b.2.cmp(&a.2)));
        }

        let chosen_pos = (failures as usize) % ranked.len();
        let (idx, cand, score, matched, ai_why) = &ranked[chosen_pos];
        let idx = *idx;
        let kind = if idx == 0 { "primary" } else { "fallback" };
        let prefix = if any_installed || first_skipped.is_some() || all_exhausted { "" } else { "default_" };
        let retry_tag = if failures > 0 { format!(" retry={failures}") } else { String::new() };
        let match_tag = if let Some(why) = ai_why {
            format!(" ai_match={why}")
        } else if *score > 0 && !matched.is_empty() {
            format!(" matched=[{}]", matched.join(","))
        } else {
            String::new()
        };
        let reason = if all_exhausted {
            let why = first_skipped.map(|(_, r)| r).unwrap_or_else(|| "exhausted".into());
            format!("role={role_name} {kind}={}{match_tag}{retry_tag} (all quotas exhausted: {why})", cand.agent)
        } else if let Some((skipped_agent, skipped_why)) = &first_skipped {
            format!(
                "role={role_name} {kind}={}{match_tag}{retry_tag} (skipped {skipped_agent}: {skipped_why})",
                cand.agent
            )
        } else {
            format!("role={role_name} {prefix}{kind}={}{match_tag}{retry_tag}", cand.agent)
        };
        return (
            cand.agent.clone(),
            explicit_model.or_else(|| cand.model.clone()),
            cand.context.clone(),
            explicit_effort.unwrap_or(cand.effort),
            explicit_fast.unwrap_or(cand.fast),
            reason,
        );
    }

    (
        role_cfg.primary.agent.clone(),
        explicit_model.or_else(|| role_cfg.primary.model.clone()),
        role_cfg.primary.context.clone(),
        explicit_effort.unwrap_or(role_cfg.primary.effort),
        explicit_fast.unwrap_or(role_cfg.primary.fast),
        format!("role={role_name} default_primary={}", role_cfg.primary.agent),
    )
}

fn worker_matches_isolation(w: &Worker, task: &Task, effective_cwd: &str) -> bool {
    if task.isolation == Isolation::Worktree {
        w.worktree.as_deref().is_some_and(|p| !p.trim().is_empty() && std::path::Path::new(p).is_dir())
    } else {
        w.worktree.is_none() && w.cwd == effective_cwd
    }
}

fn find_warm_worker(
    workers: &[Worker],
    role_name: &str,
    agent: &str,
    model: Option<&str>,
    effective_cwd: &str,
    task: &Task,
) -> Option<Worker> {
    let mut idle: Vec<&Worker> = workers
        .iter()
        .filter(|w| {
            w.state == WorkerState::Idle
                && w.read_only_dir.is_none()
                && w.agent == agent
                && w.role.as_deref().unwrap_or("coder") == role_name
                && worker_matches_isolation(w, task, effective_cwd)
                && w.h2_agent.is_some()
        })
        .collect();
    if idle.is_empty() {
        return None;
    }
    // Score affinity by overlapping model, deps, files, and tags.
    idle.sort_by_key(|w| {
        let mut score: i64 = 0;
        if let Some(m) = model
            && w.model.as_deref() == Some(m)
        {
            score += 8;
        }
        for d in &task.deps {
            if w.fingerprint.task_ids.contains(d) {
                score += 10;
            }
        }
        for f in &task.files {
            if w.fingerprint.files.contains(f) {
                score += 4;
            }
        }
        for tag in &task.tags {
            if w.fingerprint.tags.contains(tag) {
                score += 2;
            }
        }
        std::cmp::Reverse(score)
    });
    idle.first().copied().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_task(role: &str) -> Task {
        Task {
            id: "t1".into(),
            run_id: "r1".into(),
            key: "t1".into(),
            title: "Task".into(),
            phase: None,
            spec: Spec {
                target: "x".into(),
                change: "y".into(),
                constraints: None,
                ownership: None,
                acceptance: "z".into(),
                verify: None,
            },
            tags: Vec::new(),
            files: Vec::new(),
            deps: Vec::new(),
            output_schema: None,
            isolation: Isolation::None,
            timeout_sec: None,
            stall_warn_sec: None,
            stall_kill_sec: None,
            when: None,
            assign: Some(Assign { role: Some(role.into()), ..Assign::default() }),
            status: TaskStatus::Ready,
            result: None,
            failures: 0,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
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
        }
    }

    #[test]
    fn route_skips_exhausted_primary_and_keeps_codex_with_credits() {
        let mut cfg = DroveConfig::default();
        cfg.roles.insert(
            "coder".into(),
            RoleConfig {
                label: Some("Coder".into()),
                summary: None,
                access: Some(RoleAccessMode::Writable),
                allowed_paths: Vec::new(),
                primary: RoleTarget {
                    agent: "claude".into(),
                    model: None,
                    effort: Effort::High,
                    fast: false,
                    context: None,
                    strengths: Vec::new(),
                    note: None,
                },
                fallbacks: vec![RoleTarget {
                    agent: "codex".into(),
                    model: None,
                    effort: Effort::High,
                    fast: false,
                    context: None,
                    strengths: Vec::new(),
                    note: None,
                }],
                prompt: None,
            },
        );
        let cards = vec![
            QuotaCard {
                provider: "claude".into(),
                name: Some("Claude".into()),
                kind: None,
                plan: None,
                user: None,
                windows: Some(vec![QuotaWindow {
                    unlimited: None,
                    name: "5 hours".into(),
                    used: Some(100.0),
                    remaining: 0.0,
                    resets_at: Some("2099-01-01T00:00:00Z".into()),
                    display: None,
                    text: None,
                    pool: None,
                }]),
                balance: None,
                error: None,
                as_of: None,
            },
            QuotaCard {
                provider: "codex".into(),
                name: Some("Codex".into()),
                kind: None,
                plan: None,
                user: None,
                windows: Some(vec![QuotaWindow {
                    unlimited: None,
                    name: "5 hours".into(),
                    used: Some(100.0),
                    remaining: 0.0,
                    resets_at: Some("2099-01-01T00:00:00Z".into()),
                    display: None,
                    text: None,
                    pool: None,
                }]),
                balance: Some("51025.34 credits".into()),
                error: None,
                as_of: None,
            },
        ];

        let dec =
            route_task_with_quotas(&cfg, &sample_task("coder"), "/tmp", &[], &ProbeSnapshot::default(), Some(&cards));
        assert_eq!(dec.agent, "codex");
        assert!(dec.reason.contains("skipped claude"));
    }

    #[test]
    fn route_advances_across_multi_agent_chain_on_retries() {
        let mut cfg = DroveConfig::default();
        cfg.roles.insert(
            "coder".into(),
            RoleConfig {
                label: Some("Coder".into()),
                summary: None,
                access: Some(RoleAccessMode::Writable),
                allowed_paths: Vec::new(),
                primary: RoleTarget {
                    agent: "claude".into(),
                    model: None,
                    effort: Effort::High,
                    fast: false,
                    context: None,
                    strengths: Vec::new(),
                    note: None,
                },
                fallbacks: vec![
                    RoleTarget {
                        agent: "codex".into(),
                        model: Some("gpt-5.4".into()),
                        effort: Effort::High,
                        fast: true,
                        context: None,
                        strengths: Vec::new(),
                        note: None,
                    },
                    RoleTarget {
                        agent: "agy".into(),
                        model: None,
                        effort: Effort::Medium,
                        fast: false,
                        context: None,
                        strengths: Vec::new(),
                        note: None,
                    },
                    RoleTarget {
                        agent: "gemini".into(),
                        model: None,
                        effort: Effort::Low,
                        fast: false,
                        context: None,
                        strengths: Vec::new(),
                        note: None,
                    },
                ],
                prompt: None,
            },
        );

        let mut task = sample_task("coder");
        task.assign.as_mut().unwrap().source = Some("router".into());
        task.assign.as_mut().unwrap().agent = Some("claude".into());

        let expected = ["claude", "codex", "agy", "gemini"];
        for (fail_count, expected_agent) in expected.iter().enumerate() {
            task.failures = fail_count as u32;
            let dec = route_task_with_quotas(&cfg, &task, "/tmp", &[], &ProbeSnapshot::default(), None);
            assert_eq!(&dec.agent, expected_agent, "failures={fail_count}");
            assert_eq!(dec.fast, *expected_agent == "codex", "fast flag mismatch at failures={fail_count}");
        }

        // Cursor target propagates configured context window size.
        cfg.roles.get_mut("coder").unwrap().primary = RoleTarget {
            agent: "cursor".into(),
            model: Some("grok-4.7".into()),
            effort: Effort::High,
            fast: false,
            context: Some("500k".into()),
            strengths: Vec::new(),
            note: None,
        };
        let mut cursor_task = sample_task("coder");
        cursor_task.assign = None;
        let dec = route_task_with_quotas(&cfg, &cursor_task, "/tmp", &[], &ProbeSnapshot::default(), None);
        assert_eq!(dec.agent, "cursor");
        assert_eq!(dec.context.as_deref(), Some("500k"));

        // User-pinned override stays pinned even when retries increment.
        task.assign.as_mut().unwrap().source = Some("user".into());
        task.assign.as_mut().unwrap().agent = Some("kimi".into());
        task.failures = 2;
        let pinned = route_task_with_quotas(&cfg, &task, "/tmp", &[], &ProbeSnapshot::default(), None);
        assert_eq!(pinned.agent, "kimi");
        assert!(pinned.reason.contains("user_override"));
    }

    #[test]
    fn fallback_retry_does_not_reuse_router_assigned_primary_worker() {
        let mut cfg = DroveConfig::default();
        cfg.roles.insert(
            "reviewer".into(),
            RoleConfig {
                label: Some("Reviewer".into()),
                summary: None,
                access: Some(RoleAccessMode::Readonly),
                allowed_paths: Vec::new(),
                primary: RoleTarget {
                    agent: "codex".into(),
                    model: Some("gpt-6.2-sol".into()),
                    effort: Effort::High,
                    fast: false,
                    context: None,
                    strengths: Vec::new(),
                    note: None,
                },
                fallbacks: vec![RoleTarget {
                    agent: "agy".into(),
                    model: Some("gemini-4-carbon-high".into()),
                    effort: Effort::High,
                    fast: false,
                    context: None,
                    strengths: Vec::new(),
                    note: None,
                }],
                prompt: None,
            },
        );

        let workers = vec![Worker {
            id: "w_primary".into(),
            run_id: "r1".into(),
            agent: "codex".into(),
            model: Some("gpt-6.2-sol".into()),
            context_window: None,
            effort: Some(Effort::High),
            fast: Some(false),
            session_key: "r1:w_primary".into(),
            h2_agent: Some("a1".into()),
            cwd: "/tmp".into(),
            worktree: None,
            state: WorkerState::Idle,
            current_attempt_id: None,
            pid: None,
            fingerprint: Fingerprint::default(),
            context: WorkerContext::default(),
            totals: WorkerTotals { tokens: 0, cost: 0.0, attempts: 1, estimated: None },
            created_at: "2026-01-01T00:00:00Z".into(),
            last_active_at: "2026-01-01T00:00:00Z".into(),
            role: Some("reviewer".into()),
            forked_from: None,
            protocol_gen: Some(1),
            read_only_dir: None,
        }];

        let mut task = sample_task("reviewer");
        task.failures = 1;
        task.assign = Some(Assign {
            role: Some("reviewer".into()),
            source: Some("router".into()),
            agent: Some("codex".into()),
            model: Some("gpt-6.2-sol".into()),
            worker: Some("w_primary".into()),
            ..Assign::default()
        });

        let dec = route_task_with_quotas(&cfg, &task, "/tmp", &workers, &ProbeSnapshot::default(), None);
        assert_eq!(dec.agent, "agy");
        assert_eq!(dec.model.as_deref(), Some("gemini-4-carbon-high"));
        assert!(dec.warm_worker.is_none(), "must not reuse primary codex worker for agy fallback");
    }

    #[test]
    fn route_picks_best_matching_candidate_by_strengths_and_rotates_on_retry() {
        let mut cfg = DroveConfig::default();
        cfg.roles.insert(
            "coder".into(),
            RoleConfig {
                label: Some("Coder".into()),
                summary: None,
                access: Some(RoleAccessMode::Writable),
                allowed_paths: Vec::new(),
                primary: RoleTarget {
                    agent: "claude".into(),
                    model: Some("claude-opus-4-6".into()),
                    effort: Effort::High,
                    fast: false,
                    context: None,
                    strengths: vec!["后端架构".into(), "复杂重构".into()],
                    note: None,
                },
                fallbacks: vec![
                    RoleTarget {
                        agent: "gemini".into(),
                        model: Some("gemini-2.5-pro".into()),
                        effort: Effort::Medium,
                        fast: false,
                        context: None,
                        strengths: vec!["前端UI".into(), "样式布局".into()],
                        note: None,
                    },
                    RoleTarget {
                        agent: "codex".into(),
                        model: Some("gpt-5.4".into()),
                        effort: Effort::High,
                        fast: true,
                        context: None,
                        strengths: vec!["快速修复".into(), "算法性能".into()],
                        note: None,
                    },
                ],
                prompt: None,
            },
        );

        let mut ui_task = sample_task("coder");
        ui_task.title = "优化设置页面的前端组件布局与 CSS 样式".into();
        ui_task.files = vec!["src/components/SettingsPanel.tsx".into()];
        let dec = route_task_with_quotas(&cfg, &ui_task, "/tmp", &[], &ProbeSnapshot::default(), None);
        assert_eq!(dec.agent, "gemini");
        assert_eq!(dec.model.as_deref(), Some("gemini-2.5-pro"));
        assert!(dec.reason.contains("matched=["), "reason={}", dec.reason);
        // Clear deterministic match means AI tie-breaker is skipped.
        assert!(candidate_pool_for_ai(&cfg, &ui_task, &[], &ProbeSnapshot::default(), None).is_none());

        // On retry, rotate away from gemini to the next ranked candidate (claude).
        ui_task.failures = 1;
        let retry_dec = route_task_with_quotas(&cfg, &ui_task, "/tmp", &[], &ProbeSnapshot::default(), None);
        assert_eq!(retry_dec.agent, "claude");
        assert!(retry_dec.reason.contains("retry=1"), "reason={}", retry_dec.reason);

        // Generic task with no keyword match falls back to #1 primary AND qualifies for AI tie-breaker.
        let generic_task = sample_task("coder");
        let default_dec = route_task_with_quotas(&cfg, &generic_task, "/tmp", &[], &ProbeSnapshot::default(), None);
        assert_eq!(default_dec.agent, "claude");
        assert!(candidate_pool_for_ai(&cfg, &generic_task, &[], &ProbeSnapshot::default(), None).is_some());

        // AI preference overrides default #1 cleanly.
        let ai_dec = route_task_with_preference(
            &cfg,
            &generic_task,
            "/tmp",
            &[],
            &ProbeSnapshot::default(),
            None,
            Some((2, "fast codex".into())),
        );
        assert_eq!(ai_dec.agent, "codex");
        assert!(ai_dec.reason.contains("ai_match=fast codex"), "reason={}", ai_dec.reason);

        // When a global routing note is set (e.g. GLM 14:00-18:00 double-quota rule),
        // AI routing MUST be invoked even when deterministic strength matching had a unique winner.
        cfg.note = Some("GLM 相关模型在下午 2:00–6:00 额度按两倍计算，应避免使用，除非没有其他 agent 可用。".into());
        ui_task.failures = 0;
        assert!(
            candidate_pool_for_ai(&cfg, &ui_task, &[], &ProbeSnapshot::default(), None).is_some(),
            "global routing note must trigger AI candidate evaluation even on unique strength match"
        );

        // Per-candidate note alone (with no strengths at all) also triggers AI evaluation when >= 2 candidates exist.
        let mut note_only_cfg = DroveConfig::default();
        note_only_cfg.roles.get_mut("coder").unwrap().fallbacks[0].note = Some("下午 2-6 点双倍消耗，尽量避开".into());
        assert!(
            candidate_pool_for_ai(&note_only_cfg, &generic_task, &[], &ProbeSnapshot::default(), None).is_some(),
            "per-candidate note must trigger AI candidate evaluation"
        );
    }
}
