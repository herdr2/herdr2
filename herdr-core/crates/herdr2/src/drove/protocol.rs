//! Worker prompt construction and structured context rendering for Drove tasks.

use super::types::*;
use serde_json::Value;

/// Optional handover snapshot extracted from a failed prior attempt on retry/fallback.
#[derive(Debug, Clone, Default)]
pub struct PriorAttemptHandover {
    pub attempt_id: String,
    pub agent: String,
    pub model: Option<String>,
    pub failure_reason: Option<String>,
    pub context_markdown: Option<String>,
    pub git_status: Option<String>,
}

/// Maximum bytes of a single upstream `result.data` JSON inlined directly into a downstream prompt
/// before collapsing to a `@task:<key>#data` pointer.
pub const INLINE_DEP_DATA_MAX_BYTES: usize = 512;
/// Maximum cumulative bytes of upstream `result.data` inlined across all dependencies in fan-in tasks.
pub const INLINE_TOTAL_DEP_MAX_BYTES: usize = 2048;

/// Builds the full task prompt delivered to an ACP worker for `attempt`.
pub fn build_task_prompt(
    run: &Run,
    task: &Task,
    attempt: &Attempt,
    role_name: &str,
    role_cfg: &RoleConfig,
    dep_tasks: &[Task],
    hint: Option<&str>,
) -> String {
    build_task_prompt_with_context(run, task, attempt, role_name, role_cfg, dep_tasks, &[], hint, None)
}

/// Builds the full task prompt with optional failed prior-attempt handover.
#[allow(clippy::too_many_arguments)]
pub fn build_task_prompt_with_handover(
    run: &Run,
    task: &Task,
    attempt: &Attempt,
    role_name: &str,
    role_cfg: &RoleConfig,
    dep_tasks: &[Task],
    hint: Option<&str>,
    prior_handover: Option<&PriorAttemptHandover>,
) -> String {
    build_task_prompt_with_context(run, task, attempt, role_name, role_cfg, dep_tasks, &[], hint, prior_handover)
}

/// Builds the full task prompt delivered to an ACP worker for `attempt`, applying Pointer-First
/// `#ref` compaction for large upstream payloads, shared channel contract index headers, and
/// strong-schema `drove_finish` MCP tool instructions.
#[allow(clippy::too_many_arguments)]
pub fn build_task_prompt_with_context(
    run: &Run,
    task: &Task,
    attempt: &Attempt,
    role_name: &str,
    role_cfg: &RoleConfig,
    dep_tasks: &[Task],
    contracts: &[ContractPost],
    hint: Option<&str>,
    prior_handover: Option<&PriorAttemptHandover>,
) -> String {
    let mut sections = Vec::new();
    sections.push(format!(
        "You are a Drove worker (`{}` / role `{role_name}`) executing attempt `{}` for task `{}` (`{}`).",
        attempt.agent, attempt.id, task.key, task.title
    ));
    sections.push(format!("Run objective: {}", run.objective));
    if let Some(prompt) = &role_cfg.prompt
        && !prompt.trim().is_empty()
    {
        sections.push(format!("Role guidance:\n{prompt}"));
    }

    let access_note = match role_cfg.access.unwrap_or(RoleAccessMode::Writable) {
        RoleAccessMode::Readonly => {
            "Access mode: READONLY. Write/edit tools are blocked at runtime; do NOT create, modify, or delete any workspace files."
                .to_string()
        }
        RoleAccessMode::ScopedWrite => {
            if role_cfg.allowed_paths.is_empty() {
                "Access mode: SCOPED_WRITE. Only edit files matching the configured allowedPaths.".to_string()
            } else {
                format!(
                    "Access mode: SCOPED_WRITE (enforced pre-tool). Allowed paths: [{}]. Edits outside these globs are blocked immediately.",
                    role_cfg.allowed_paths.join(", ")
                )
            }
        }
        RoleAccessMode::Writable => "Access mode: WRITABLE. Workspace edits are permitted within task ownership.".into(),
    };
    sections.push(access_note);

    let mut spec_lines = vec![
        format!("- Target: {}", task.spec.target),
        format!("- Change:\n{}", task.spec.change),
        format!("- Acceptance: {}", task.spec.acceptance),
    ];
    if let Some(c) = &task.spec.constraints {
        spec_lines.push(format!("- Constraints: {c}"));
    }
    if let Some(o) = &task.spec.ownership {
        spec_lines.push(format!("- Ownership: {o}"));
    }
    if let Some(v) = &task.spec.verify {
        let cmd = match v {
            VerifySpec::Cmd(s) => s.clone(),
            VerifySpec::Obj(o) => o.cmd.clone(),
        };
        spec_lines.push(format!("- Verify gate: `{cmd}`"));
    }
    sections.push(format!("Task spec:\n{}", spec_lines.join("\n")));

    if !dep_tasks.is_empty() {
        let mut dep_lines = Vec::new();
        let mut inlined_bytes = 0usize;
        for d in dep_tasks {
            if let Some(res) = &d.result {
                let summary_txt =
                    res.compact_summary.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or(&res.summary);
                dep_lines.push(format!("- `{}` ({:?}): {}", d.key, d.status, summary_txt));
                if let Some(files) = &res.files_modified
                    && !files.is_empty()
                {
                    dep_lines.push(format!("  filesModified: {}", files.join(", ")));
                }
                if let Some(data) = &res.data {
                    let serialized = serde_json::to_string(data).unwrap_or_default();
                    if serialized.len() <= INLINE_DEP_DATA_MAX_BYTES
                        && inlined_bytes + serialized.len() <= INLINE_TOTAL_DEP_MAX_BYTES
                    {
                        inlined_bytes += serialized.len();
                        dep_lines.push(format!("  data: {serialized}"));
                    } else {
                        let shape = describe_json_shape(data);
                        dep_lines.push(format!(
                            "  dataRef: `@task:{}#data` ({} B, {shape}) -> read on demand via `drove_context` (`action: \"read\", target: \"task:{}\"` or `json_path`)",
                            d.key,
                            serialized.len(),
                            d.key
                        ));
                    }
                }
            }
        }
        if !dep_lines.is_empty() {
            sections.push(format!("Upstream handovers:\n{}", dep_lines.join("\n")));
        }
    }

    if !contracts.is_empty() || !task.channels.is_empty() {
        let mut c_lines = Vec::new();
        if !task.channels.is_empty() {
            c_lines.push(format!(
                "Subscribed channels: [main, {}] (publish shared interface contracts via `drove_context` `action: \"publish\"`)",
                task.channels.join(", ")
            ));
        }
        for c in contracts.iter().rev().take(16).rev() {
            c_lines.push(format!(
                "- `#contract:{}` [{}] `{}` (by `{}`): {} -> read payload via `drove_context(action=\"read\", target=\"#contract:{}\")`",
                c.seq, c.channel, c.key, c.author_task, c.summary, c.seq
            ));
        }
        if !c_lines.is_empty() {
            sections.push(format!("Shared contract board (pointer index):\n{}", c_lines.join("\n")));
        }
    }

    if let Some(schema) = &task.output_schema
        && !schema.is_null()
    {
        let schema_pretty = serde_json::to_string_pretty(schema).unwrap_or_else(|_| schema.to_string());
        sections.push(format!(
            "Required outputSchema (`--data` JSON MUST satisfy this schema when calling `drove worker done`):\n{schema_pretty}"
        ));
    }

    if let Some(ctx) = &task.context
        && !ctx.is_null()
    {
        sections.push(format!("Foreach / context item:\n{}", serde_json::to_string_pretty(ctx).unwrap_or_default()));
    }

    if let Some(h) = hint.filter(|s| !s.trim().is_empty()) {
        sections.push(format!("Retry / coordinator hint:\n{h}"));
    }

    if let Some(rc) = &task.review_circuit
        && rc.stage == ReviewCircuitStage::PostHandoffReview
    {
        let mut lines = vec![format!(
            "POST-HANDOFF FINAL REVIEW (1/1): This task previously failed {} review round(s) and was taken over directly by the Coordinator.",
            rc.review_rounds
        )];
        lines.push(
            "Focus strictly on verifying whether the concrete blocking findings from prior rounds are resolved; do NOT expand scope with speculative second-order fault injection or new design preferences.".into(),
        );
        for rec in rc.history.iter().rev().take(3).rev() {
            lines.push(format!("- Prior Round {} ({}): {}", rec.round, rec.verdict, rec.summary));
        }
        sections.push(lines.join("\n"));
    }

    if let Some(prior) = prior_handover {
        let model_label = prior.model.as_deref().unwrap_or("default");
        let mut lines =
            vec![format!("- Previous attempt: `{}` (`{}` / `{model_label}`)", prior.attempt_id, prior.agent)];
        if let Some(reason) = prior.failure_reason.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            lines.push(format!("- Failure reason: {reason}"));
        }
        if let Some(st) = prior.git_status.as_deref().filter(|s| !s.trim().is_empty()) {
            lines.push(format!("- Current worktree `git status --short`:\n```text\n{}\n```", st.trim_end()));
        }
        if let Some(md) = prior.context_markdown.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            lines.push(format!("- Prior attempt transcript tail:\n{md}"));
        }
        sections.push(format!(
            "Prior failed attempt handover (avoid repeating failed approaches and inspect existing edits before rewriting):\n{}",
            lines.join("\n")
        ));
    }

    let data_flag = if task.output_schema.as_ref().is_some_and(|s| !s.is_null()) {
        "--data '<JSON matching outputSchema>' (REQUIRED)"
    } else {
        "[--data '{\"key\": \"val\"}']"
    };
    let mcp_data_note = if task.output_schema.as_ref().is_some_and(|s| !s.is_null()) {
        " and `data` matching `outputSchema` (REQUIRED)"
    } else {
        ""
    };
    let protocol_instr = format!(
        "Completion protocol (REQUIRED):\n\
         Prefer the injected `drove` MCP tools (strong JSON schema, no shell escaping needed):\n\
         - Finish task: call `drove_finish` with `outcome: \"done\"`, `summary: \"...\"`{mcp_data_note} (or `outcome: \"failed\"`, `reason: \"...\"`)\n\
         - Ask coordinator (blocks until reply): call `drove_ask` with `question` (and optional `options`)\n\
         - Read `#ref` pointers / publish shared contracts / post notes: call `drove_context` (`action: \"read\" | \"publish\" | \"list\" | \"note\" | \"inbox\"`)\n\
         CLI fallback (if MCP tools are unavailable in your adapter):\n\
         - Report completion: `h2 drove worker done --outcome done --summary \"...\" {data_flag}`\n\
         - Report failure: `h2 drove worker done --outcome failed --reason \"...\"`\n\
         - Ask coordinator: `h2 drove worker ask \"Question?\" [--options \"A,B\"]`\n\
         - Read pointer/contract: `h2 drove worker read <target> [--path <json.path>]`\n\
         Current `attemptId`: `{}` (`h2 drove worker` and `drove_*` MCP tools resolve your active attempt automatically).\n\
         NEVER call `drove_finish` / `drove worker done` until all changes and local checks for this attempt are complete.",
        attempt.id
    );
    sections.push(protocol_instr);
    sections.join("\n\n")
}

/// Evaluates `task.when` against upstream tasks in the run.
pub fn when_matches(when: Option<&TaskWhen>, run_tasks: &[Task]) -> bool {
    let Some(w) = when else { return true };
    let Some(target) = run_tasks.iter().find(|t| t.key == w.task || t.id == w.task) else {
        return false;
    };
    if target.status != TaskStatus::Succeeded {
        return false;
    }
    let val = extract_when_value(target, w.path.as_deref());
    if let Some(must_exist) = w.exists {
        let present = val.is_some_and(|v| !v.is_null());
        if present != must_exist {
            return false;
        }
    }
    if let Some(expected) = &w.equals {
        if val != Some(expected) {
            return false;
        }
    }
    if let Some(allowed) = &w.in_values {
        if !val.is_some_and(|v| allowed.contains(v)) {
            return false;
        }
    }
    true
}

fn describe_json_shape(val: &Value) -> String {
    match val {
        Value::Object(map) => {
            let keys: Vec<String> = map
                .iter()
                .take(6)
                .map(|(k, v)| match v {
                    Value::Array(a) => format!("{k}[{}]", a.len()),
                    Value::Object(m) => format!("{k}{{{}}}", m.len()),
                    _ => k.clone(),
                })
                .collect();
            let suffix = if map.len() > 6 { ", ..." } else { "" };
            format!("object keys=[{}{suffix}]", keys.join(", "))
        }
        Value::Array(arr) => format!("array len={}", arr.len()),
        Value::String(s) => format!("string len={}", s.len()),
        other => format!("json {}", other),
    }
}

fn extract_when_value<'a>(task: &'a Task, path: Option<&str>) -> Option<&'a Value> {
    let data = task.result.as_ref().and_then(|r| r.data.as_ref())?;
    let Some(p) = path.map(str::trim).filter(|s| !s.is_empty()) else {
        return Some(data);
    };
    let clean = p.strip_prefix("data.").unwrap_or(p);
    let mut cur = data;
    for seg in clean.split('.') {
        if seg.is_empty() {
            continue;
        }
        cur = cur.get(seg)?;
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_task_prompt_renders_output_schema_and_compact_summary() {
        let run = Run {
            id: "run_1".into(),
            objective: "Demo".into(),
            cwd: "/tmp".into(),
            coordinator: CoordinatorInfo {
                host: "herdr2".into(),
                host_session: "t1".into(),
                generation: 1,
                agent: None,
                model: None,
                host_pid: None,
                session_bound: None,
                notification: None,
            },
            max_concurrency: 2,
            budget: None,
            status: RunStatus::Active,
            pause_reason: None,
            verify: None,
            goal: None,
            finalization: None,
            verified_at: None,
            name: None,
            order: None,
            trashed_at: None,
            status_before_trash: None,
            ai_usage: None,
            briefing: None,
            machine: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        };
        let dep = Task {
            id: "tsk_dep".into(),
            run_id: "run_1".into(),
            key: "discover".into(),
            title: "Discover".into(),
            phase: None,
            spec: Spec {
                target: "SPEC.md".into(),
                change: "Analyze".into(),
                constraints: None,
                ownership: None,
                acceptance: "Done".into(),
                verify: None,
            },
            tags: Vec::new(),
            files: Vec::new(),
            deps: Vec::new(),
            output_schema: None,
            isolation: Isolation::None,
            effective_isolation: None,
            isolation_reason: None,
            timeout_sec: None,
            stall_warn_sec: None,
            stall_kill_sec: None,
            when: None,
            assign: None,
            status: TaskStatus::Succeeded,
            result: Some(TaskResult {
                summary: "Very long raw summary...".into(),
                compact_summary: Some("Compact handover sentence".into()),
                data: None,
                files_modified: None,
                blockers: None,
                output_file_hashes: None,
            }),
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
            review_circuit: None,
            candidate_result: None,
            cache: None,
            cached_from: None,
            retry_on_worker_reported: None,
            fork_from: None,
            fork_mode: None,
            channels: Vec::new(),
        };
        let mut task = dep.clone();
        task.id = "tsk_2".into();
        task.key = "impl".into();
        task.result = None;
        task.output_schema = Some(json!({ "type": "object", "required": ["ok"] }));
        let attempt = Attempt {
            id: "att_1".into(),
            task_id: "tsk_2".into(),
            run_id: "run_1".into(),
            worker_id: "wrk_1".into(),
            agent: "codex".into(),
            model: None,
            effort: None,
            fast: None,
            route_reason: "primary".into(),
            reused: false,
            retry_of: None,
            hint: None,
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            awaiting_late_done: None,
            tokens: TokenCounts::default(),
            cost: None,
            estimated: None,
            outcome: None,
            failure: None,
            result: None,
            kind: Some("author".into()),
            review_round: None,
            verifying: None,
            verify_rejections: None,
            stagnant_rejections: None,
            last_verify_hash: None,
            last_tree_sha: None,
            verify_log_artifact_id: None,
            verify_ai_digest: None,
            ai_diagnosis: None,
            spinning_detected_at: None,
        };
        let role_cfg = RoleConfig {
            label: None,
            summary: None,
            access: Some(RoleAccessMode::Writable),
            allowed_paths: Vec::new(),
            primary: RoleTarget {
                agent: "codex".into(),
                model: None,
                effort: Effort::Medium,
                fast: false,
                context: None,
                strengths: Vec::new(),
                note: None,
            },
            fallbacks: Vec::new(),
            prompt: None,
        };
        let prompt = build_task_prompt(&run, &task, &attempt, "coder", &role_cfg, &[dep.clone()], None);
        assert!(prompt.contains("Compact handover sentence"), "{prompt}");
        assert!(!prompt.contains("Very long raw summary"), "{prompt}");
        assert!(prompt.contains("Required outputSchema"), "{prompt}");
        assert!(prompt.contains("\"ok\""), "{prompt}");
        assert!(prompt.contains("--data '<JSON matching outputSchema>' (REQUIRED)"), "{prompt}");

        let prior = PriorAttemptHandover {
            attempt_id: "att_0".into(),
            agent: "claude".into(),
            model: Some("sonnet".into()),
            failure_reason: Some("AgentError: timed out".into()),
            context_markdown: Some("## Recent Turns\nTried patching auth.rs".into()),
            git_status: Some(" M src/lib.rs\n".into()),
        };
        let retry_prompt = build_task_prompt_with_handover(
            &run,
            &task,
            &attempt,
            "coder",
            &role_cfg,
            &[dep],
            Some("Switch to fallback"),
            Some(&prior),
        );
        assert!(retry_prompt.contains("Prior failed attempt handover"), "{retry_prompt}");
        assert!(retry_prompt.contains("Previous attempt: `att_0` (`claude` / `sonnet`)"), "{retry_prompt}");
        assert!(retry_prompt.contains("Failure reason: AgentError: timed out"), "{retry_prompt}");
        assert!(retry_prompt.contains("```text\n M src/lib.rs\n```"), "{retry_prompt}");
        assert!(retry_prompt.contains("Tried patching auth.rs"), "{retry_prompt}");
        assert!(retry_prompt.contains("drove_finish"), "{retry_prompt}");

        // Large upstream `data` (> 512B) collapses to `@task:<key>#data` pointer; contracts render as `#contract:<seq>` headers.
        let mut big_dep = task.clone();
        big_dep.key = "scan".into();
        big_dep.status = TaskStatus::Succeeded;
        let big_items: Vec<String> = (0..40).map(|i| format!("packages/module_{i}/src/index.ts")).collect();
        big_dep.result = Some(TaskResult {
            summary: "Scanned modules".into(),
            compact_summary: Some("Found 40 target modules".into()),
            data: Some(json!({ "modules": big_items, "meta": { "version": 1 } })),
            files_modified: None,
            blockers: None,
            output_file_hashes: None,
        });
        let mut consumer = task.clone();
        consumer.channels = vec!["api-v2".into()];
        let contracts = vec![ContractPost {
            seq: 3,
            run_id: "run_1".into(),
            channel: "api-v2".into(),
            key: "jwt-claims".into(),
            author_task: "auth-backend".into(),
            worker_id: Some("wrk_1".into()),
            summary: "JWT claims {sub, exp, role}".into(),
            payload: Some(json!({ "very_large_secret_schema": "not_inlined_into_prompt" })),
            created_at: "2026-01-01T00:00:00Z".into(),
        }];
        let ptr_prompt = build_task_prompt_with_context(
            &run,
            &consumer,
            &attempt,
            "coder",
            &role_cfg,
            &[big_dep],
            &contracts,
            None,
            None,
        );
        assert!(ptr_prompt.contains("@task:scan#data"), "{ptr_prompt}");
        assert!(ptr_prompt.contains("modules[40]"), "{ptr_prompt}");
        assert!(!ptr_prompt.contains("packages/module_39/src/index.ts"), "{ptr_prompt}");
        assert!(ptr_prompt.contains("`#contract:3` [api-v2] `jwt-claims`"), "{ptr_prompt}");
        assert!(!ptr_prompt.contains("not_inlined_into_prompt"), "{ptr_prompt}");
    }
}
