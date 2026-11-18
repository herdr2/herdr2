//! Role-first task router (`primary` -> `fallbacks`, Magpie quota skip, + warm worker affinity reuse).

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
    let assign = task.assign.as_ref();
    let role_name = assign
        .and_then(|a| a.role.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("coder")
        .to_string();

    let roles_fallback = default_roles();
    let role_cfg = resolve_role_config(cfg, &role_name)
        .or_else(|| roles_fallback.get("coder").cloned())
        .unwrap_or(RoleConfig {
            label: Some(role_name.clone()),
            summary: None,
            access: Some(RoleAccessMode::Writable),
            allowed_paths: Vec::new(),
            primary: RoleTarget { agent: "claude".into(), model: None, effort: Effort::High, fast: false, context: None },
            fallbacks: Vec::new(),
            prompt: None,
        });

    let is_router_assigned = assign.and_then(|a| a.source.as_deref()) == Some("router");
    let explicit_agent = if is_router_assigned {
        None
    } else {
        assign.and_then(|a| a.agent.as_deref()).map(str::trim).filter(|s| !s.is_empty())
    };
    let explicit_model = if is_router_assigned {
        None
    } else {
        assign.and_then(|a| a.model.clone()).filter(|s| !s.trim().is_empty())
    };
    let explicit_effort = if is_router_assigned { None } else { assign.and_then(|a| a.effort) };
    let explicit_fast = if is_router_assigned { None } else { assign.and_then(|a| a.fast) };

    let (chosen_agent, chosen_model, chosen_context, chosen_effort, chosen_fast, reason) = if let Some(agent) = explicit_agent {
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
            workers,
            probed,
            quotas,
            explicit_model,
            explicit_effort,
            explicit_fast,
            task.failures,
        )
    };

    // Prefer an explicit user-requested worker id if idle and matching cwd/agent;
    // ignore router-populated `assign.worker` so fallback retries can switch agents cleanly.
    let explicit_worker = if is_router_assigned {
        None
    } else {
        assign.and_then(|a| a.worker.as_deref())
    };
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
            .or_else(|| find_warm_worker(workers, &role_name, &chosen_agent, effective_cwd, task))
    } else {
        find_warm_worker(workers, &role_name, &chosen_agent, effective_cwd, task)
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

fn pick_candidate(
    role_name: &str,
    role_cfg: &RoleConfig,
    workers: &[Worker],
    probed: &ProbeSnapshot,
    quotas: Option<&[QuotaCard]>,
    explicit_model: Option<String>,
    explicit_effort: Option<Effort>,
    explicit_fast: Option<bool>,
    failures: u32,
) -> (String, Option<String>, Option<String>, Effort, bool, String) {
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

    if !eligible.is_empty() {
        let mut first_skipped: Option<(String, String)> = None;
        let mut available: Vec<(usize, &RoleTarget)> = Vec::new();
        if let Some(cards) = quotas.filter(|c| !c.is_empty()) {
            for &(idx, cand) in &eligible {
                let eff_model = explicit_model.as_deref().or(cand.model.as_deref());
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
        let base_pool = if !available.is_empty() { &available } else { &eligible };
        let healthy: Vec<(usize, &RoleTarget)> = base_pool
            .iter()
            .copied()
            .filter(|(_, cand)| {
                let eff_model = explicit_model.as_deref().or(cand.model.as_deref());
                let has_dead = workers.iter().any(|w| {
                    w.state == WorkerState::Dead
                        && w.agent == cand.agent
                        && w.model.as_deref() == eff_model
                });
                let has_healthy = workers.iter().any(|w| {
                    matches!(w.state, WorkerState::Idle | WorkerState::Busy)
                        && w.agent == cand.agent
                        && w.model.as_deref() == eff_model
                });
                !has_dead || has_healthy
            })
            .collect();
        let pool = if !healthy.is_empty() { &healthy } else { base_pool };
        let chosen_pos = (failures as usize) % pool.len();
        let (idx, cand) = pool[chosen_pos];
        let kind = if idx == 0 { "primary" } else { "fallback" };
        let prefix = if any_installed || !available.is_empty() || all_exhausted {
            ""
        } else {
            "default_"
        };
        let retry_tag = if failures > 0 { format!(" retry={failures}") } else { String::new() };
        let reason = if all_exhausted {
            let why = first_skipped.map(|(_, r)| r).unwrap_or_else(|| "exhausted".into());
            format!("role={role_name} {kind}={}{retry_tag} (all quotas exhausted: {why})", cand.agent)
        } else if let Some((skipped_agent, skipped_why)) = &first_skipped {
            format!(
                "role={role_name} {kind}={}{retry_tag} (skipped {skipped_agent}: {skipped_why})",
                cand.agent
            )
        } else {
            format!("role={role_name} {prefix}{kind}={}{retry_tag}", cand.agent)
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
        w.worktree
            .as_deref()
            .is_some_and(|p| !p.trim().is_empty() && std::path::Path::new(p).is_dir())
    } else {
        w.worktree.is_none() && w.cwd == effective_cwd
    }
}

fn find_warm_worker(
    workers: &[Worker],
    role_name: &str,
    agent: &str,
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
    // Score affinity by overlapping files, deps, and tags.
    idle.sort_by_key(|w| {
        let mut score: i64 = 0;
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
                primary: RoleTarget { agent: "claude".into(), model: None, effort: Effort::High, fast: false, context: None },
                fallbacks: vec![RoleTarget { agent: "codex".into(), model: None, effort: Effort::High, fast: false, context: None }],
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

        let dec = route_task_with_quotas(
            &cfg,
            &sample_task("coder"),
            "/tmp",
            &[],
            &ProbeSnapshot::default(),
            Some(&cards),
        );
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
                primary: RoleTarget { agent: "claude".into(), model: None, effort: Effort::High, fast: false, context: None },
                fallbacks: vec![
                    RoleTarget { agent: "codex".into(), model: Some("gpt-5.4".into()), effort: Effort::High, fast: true, context: None },
                    RoleTarget { agent: "agy".into(), model: None, effort: Effort::Medium, fast: false, context: None },
                    RoleTarget { agent: "gemini".into(), model: None, effort: Effort::Low, fast: false, context: None },
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
                },
                fallbacks: vec![RoleTarget {
                    agent: "agy".into(),
                    model: Some("gemini-4-carbon-high".into()),
                    effort: Effort::High,
                    fast: false,
                    context: None,
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
}
