//! Worker prompt construction and structured context rendering for Drove tasks.

use super::types::*;
use serde_json::Value;

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
        RoleAccessMode::Readonly => "Access mode: READONLY. Do NOT create, modify, or delete any tracked or untracked workspace files.",
        RoleAccessMode::ScopedWrite => "Access mode: SCOPED_WRITE. Only edit files matching the configured allowedPaths.",
        RoleAccessMode::Writable => "Access mode: WRITABLE. Workspace edits are permitted within task ownership.",
    };
    sections.push(access_note.to_string());

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
        for d in dep_tasks {
            if let Some(res) = &d.result {
                dep_lines.push(format!("- `{}` ({:?}): {}", d.key, d.status, res.summary));
                if let Some(files) = &res.files_modified
                    && !files.is_empty()
                {
                    dep_lines.push(format!("  filesModified: {}", files.join(", ")));
                }
                if let Some(data) = &res.data {
                    dep_lines.push(format!("  data: {}", serde_json::to_string(data).unwrap_or_default()));
                }
            }
        }
        if !dep_lines.is_empty() {
            sections.push(format!("Upstream handovers:\n{}", dep_lines.join("\n")));
        }
    }

    if let Some(ctx) = &task.context
        && !ctx.is_null()
    {
        sections.push(format!("Foreach / context item:\n{}", serde_json::to_string_pretty(ctx).unwrap_or_default()));
    }

    if let Some(h) = hint.filter(|s| !s.trim().is_empty()) {
        sections.push(format!("Retry / coordinator hint:\n{h}"));
    }

    let protocol_instr = format!(
        "Completion protocol (REQUIRED):\n\
         When you finish, or if you need coordinator input, use the `h2 drove worker` CLI (or `drove worker`):\n\
         - Report completion: `h2 drove worker done --outcome done --summary \"...\" [--data '{{\"key\": \"val\"}}']`\n\
         - Report failure: `h2 drove worker done --outcome failed --reason \"...\"`\n\
         - Ask coordinator (blocks until reply): `h2 drove worker ask \"Question?\" [--options \"A,B\"]`\n\
         - Send progress note: `h2 drove worker note \"...\"`\n\
         - Read inbox messages: `h2 drove worker inbox`\n\
         Worker env vars (`H2_DROVE_URL`, `H2_DROVE_WORKER_TOKEN`, `H2_DROVE_ATTEMPT_ID`) are pre-set for you (`attemptId`: `{}`).\n\
         NEVER call `drove worker done` until all changes and checks for this attempt are complete.",
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
