//! Bounded deterministic context handoff & lightweight session lineage.

use std::collections::BTreeMap;
use std::path::Path;

use clap::ValueEnum;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::transcript::{Entry, strip_h2_envelope};

/// Default byte budget for generated context handoff packages (~24 KiB).
pub const DEFAULT_HANDOFF_MAX_BYTES: usize = 24 * 1024;

/// How a derived session was created from an ancestor session.
#[derive(Serialize, Deserialize, JsonSchema, ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum LineageKind {
    /// Continue the same line of work in a new or different agent/provider.
    #[default]
    Handoff,
    /// Branch off to explore or re-implement independently from a turn/state.
    Fork,
    /// Open a temporary side-channel discussion without replacing the main session.
    #[serde(alias = "side_chat")]
    #[value(name = "side-chat", alias = "side_chat")]
    SideChat,
    /// Forward recent session context directly into another currently live session.
    #[serde(alias = "send_context")]
    #[value(name = "send-context", alias = "send_context")]
    SendContext,
}

impl LineageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Handoff => "handoff",
            Self::Fork => "fork",
            Self::SideChat => "side-chat",
            Self::SendContext => "send-context",
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            Self::Handoff => "Handoff",
            Self::Fork => "Fork",
            Self::SideChat => "Side Chat",
            Self::SendContext => "Shared Context",
        }
    }

    pub fn display_label_zh(self) -> &'static str {
        match self {
            Self::Handoff => "上下文接力",
            Self::Fork => "会话分叉",
            Self::SideChat => "旁路讨论",
            Self::SendContext => "发送上下文",
        }
    }
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Lightweight parent pointer stored on `Agent`, `Terminal`, and handoff results.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct Lineage {
    pub kind: LineageKind,
    /// Source address on this core (e.g. `a1` or `t2`).
    pub parent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_turn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_turn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_branch: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub worktree_merged: bool,
}

/// Inputs used to synthesize a deterministic cross-provider context handoff.
pub struct HandoffBuildInput<'a> {
    pub kind: LineageKind,
    pub source_address: &'a str,
    pub source_provider: Option<&'a str>,
    pub source_session_id: Option<&'a str>,
    pub source_model: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub entries: &'a [Entry],
    pub from_turn: Option<u64>,
    pub to_turn: Option<u64>,
    pub max_bytes: usize,
    pub note: Option<&'a str>,
    pub is_zh: bool,
    pub worktree_origin: Option<&'a str>,
    /// Optional structured semantic memory distilled from `omitted_turns`.
    pub omitted_summary: Option<&'a str>,
}

/// Output from [`build_handoff_package`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HandoffPackage {
    pub markdown: String,
    pub included_turns: Vec<u64>,
    pub omitted_turns: Vec<u64>,
    pub total_turns: u64,
    pub bytes: usize,
}

#[derive(Default)]
struct TurnBucket {
    users: Vec<String>,
    assistants: Vec<String>,
    tools: Vec<String>,
    errors: Vec<String>,
    plan: Option<Vec<String>>,
}

/// Wraps a Markdown handoff body in `<herdr2-handoff ...>` tags for ACP turn-1 injection
/// so `strip_h2_envelope` removes it cleanly if re-read or re-forwarded later.
pub fn wrap_handoff_envelope(kind: LineageKind, source_address: &str, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return String::new();
    }
    format!(
        "<herdr2-handoff kind=\"{}\" from=\"{}\">\n{}\n</herdr2-handoff>",
        kind.as_str(),
        source_address.replace('"', ""),
        body
    )
}

/// Formats sorted turn numbers into compact range notation (e.g. `1-3, 5, 8-10`).
pub fn format_turn_ranges(turns: &[u64]) -> String {
    if turns.is_empty() {
        return String::new();
    }
    let mut sorted = turns.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut ranges = Vec::new();
    let mut start = sorted[0];
    let mut prev = sorted[0];
    for &t in &sorted[1..] {
        if t == prev + 1 {
            prev = t;
        } else {
            ranges.push(if start == prev { start.to_string() } else { format!("{start}-{prev}") });
            start = t;
            prev = t;
        }
    }
    ranges.push(if start == prev { start.to_string() } else { format!("{start}-{prev}") });
    ranges.join(", ")
}

/// Returns contiguous `(from_turn, to_turn)` ranges from sorted turn numbers.
pub fn contiguous_turn_ranges(turns: &[u64]) -> Vec<(u64, u64)> {
    if turns.is_empty() {
        return Vec::new();
    }
    let mut sorted = turns.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = Vec::new();
    let mut start = sorted[0];
    let mut prev = sorted[0];
    for &t in &sorted[1..] {
        if t == prev + 1 {
            prev = t;
        } else {
            out.push((start, prev));
            start = t;
            prev = t;
        }
    }
    out.push((start, prev));
    out
}

/// Default interactive CLI argv for a preset name when spawning a PTY terminal target.
pub fn default_pty_argv(preset: &str) -> Vec<String> {
    match preset.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => {
            vec!["claude".into(), "--dangerously-skip-permissions".into()]
        }
        "codex" => vec!["codex".into(), "--no-daemon".into(), "--dangerously-bypass-approvals-and-sandbox".into()],
        "gemini" | "gemini-cli" => vec!["gemini".into(), "--yolo".into()],
        "agy" | "antigravity" | "antigravity-cli" => {
            vec!["agy".into(), "--dangerously-skip-permissions".into()]
        }
        "qwen" | "qwen-code" => vec!["qwen".into()],
        "pi" | "omp" | "oh-my-pi" => vec!["pi".into()],
        "opencode" => vec!["opencode".into(), "--auto".into()],
        "cursor" | "cursor-agent" => {
            vec!["cursor-agent".into(), "--yolo".into(), "--trust".into()]
        }
        "zcode" | "zcode-cli" => vec!["zcode".into(), "--mode".into(), "yolo".into()],
        "trae" | "traecli" | "traex" | "trae-cli" => vec!["traecli".into()],
        "goose" => vec!["goose".into()],
        other if !other.is_empty() => vec![other.to_string()],
        _ => Vec::new(),
    }
}

/// Rewrites path-bounded references to `origin` inside carried history text to `wt` so a forked
/// worktree agent never gets baited into editing the parent workspace via stale absolute paths.
pub fn rewrite_origin_paths(text: &str, origin: &str, wt: &str) -> String {
    let orig = origin.trim().trim_end_matches(['/', '\\']);
    let target = wt.trim().trim_end_matches(['/', '\\']);
    if orig.is_empty() || target.is_empty() || orig == target || !text.contains(orig) {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let orig_len = orig.len();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < text.len() {
        if text[i..].starts_with(orig) {
            let left_ok = i == 0
                || !matches!(bytes[i - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/' | b'\\');
            let end = i + orig_len;
            let right_ok = end == text.len()
                || !matches!(bytes[end], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.');
            if left_ok && right_ok {
                out.push_str(target);
                i = end;
                continue;
            }
        }
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Builds a bounded deterministic Markdown context handoff package from transcript entries
/// and live workspace git status.
///
/// Layout priority within `max_bytes`:
/// 1. Source metadata header + optional caller note
/// 2. Live workspace git branch & `git status --short`
/// 3. Latest `Plan` (if present in selected turns)
/// 4. Turn 1 (or earliest selected turn) original objective
/// 5. Newest turns packed backwards until budget is full
/// 6. Omitted turns footer pointing to `h2 read <source> --from-turn X --to-turn Y`
pub fn build_handoff_package(input: &HandoffBuildInput<'_>) -> HandoffPackage {
    let budget = input.max_bytes.max(512);
    let lo = input.from_turn.unwrap_or(1);
    let hi = input.to_turn.unwrap_or(u64::MAX);

    let wt_pair = match (
        input.worktree_origin.map(str::trim).filter(|s| !s.is_empty()),
        input.cwd.map(str::trim).filter(|s| !s.is_empty()),
    ) {
        (Some(orig), Some(wt)) if orig.trim_end_matches(['/', '\\']) != wt.trim_end_matches(['/', '\\']) => {
            Some((orig, wt))
        }
        _ => None,
    };
    let map_text = |s: &str| -> String {
        match wt_pair {
            Some((orig, wt)) => rewrite_origin_paths(s, orig, wt),
            None => s.to_string(),
        }
    };

    let mut total_turns = 0u64;
    let mut per_turn: BTreeMap<u64, TurnBucket> = BTreeMap::new();
    for entry in input.entries {
        let Some(turn) = entry.turn() else { continue };
        total_turns = total_turns.max(turn);
        if turn == 0 || turn < lo || turn > hi {
            continue;
        }
        let bucket = per_turn.entry(turn).or_default();
        match entry {
            Entry::User { text, .. } => {
                let clean = strip_h2_envelope(text);
                if !clean.is_empty() {
                    bucket.users.push(map_text(&clean));
                }
            }
            Entry::Plan { items, .. } => {
                if !items.is_empty() {
                    bucket.plan = Some(items.iter().map(|it| map_text(it)).collect());
                }
            }
            Entry::ToolCall { title, status, detail, .. } => {
                let mut line = format!("- `[{status}]` {}", map_text(title));
                if status == "failed"
                    && let Some(d) = detail.as_deref().map(str::trim).filter(|s| !s.is_empty())
                {
                    let snippet = truncate_utf8_with_ellipsis(&map_text(d), 200).replace('\n', " ");
                    line.push_str(&format!(" — {snippet}"));
                }
                bucket.tools.push(line);
            }
            Entry::Assistant { text, .. } => {
                let clean = text.trim();
                if !clean.is_empty() {
                    bucket.assistants.push(map_text(clean));
                }
            }
            Entry::Error { message, .. } => {
                let clean = message.trim();
                if !clean.is_empty() {
                    bucket.errors.push(map_text(clean));
                }
            }
            Entry::Thought { .. }
            | Entry::Permission { .. }
            | Entry::Stderr { .. }
            | Entry::Checkpoint { .. }
            | Entry::Rewind { .. }
            | Entry::Handoff { .. } => {}
        }
    }

    // Header
    let mut header = String::new();
    let selected_turns: Vec<u64> = per_turn.keys().copied().collect();
    if input.is_zh {
        header.push_str(&format!(
            "# {}上下文交接包（来自 `{}`）\n\n",
            input.kind.display_label_zh(),
            input.source_address
        ));
        header.push_str(&format!("- **来源会话：** `{}`\n", input.source_address));
        header.push_str(&format!("- **交接类型：** `{}`\n", input.kind.display_label_zh()));
        if let Some(p) = input.source_provider.map(str::trim).filter(|s| !s.is_empty()) {
            header.push_str(&format!("- **来源提供商：** `{p}`\n"));
        }
        if let Some(m) = input.source_model.map(str::trim).filter(|s| !s.is_empty()) {
            header.push_str(&format!("- **来源模型：** `{m}`\n"));
        }
        if let Some(sid) = input.source_session_id.map(str::trim).filter(|s| !s.is_empty()) {
            header.push_str(&format!("- **来源会话 ID：** `{sid}`\n"));
        }
        if let Some(cwd) = input.cwd.map(str::trim).filter(|s| !s.is_empty()) {
            header.push_str(&format!("- **工作目录：** `{cwd}`\n"));
        }
        if !selected_turns.is_empty() {
            header.push_str(&format!(
                "- **携带轮次：** 第 {} 轮（共 {total_turns} 轮）\n",
                format_turn_ranges(&selected_turns)
            ));
        }
        if let Some(note) = input.note.map(str::trim).filter(|s| !s.is_empty()) {
            header.push_str(if input.kind == LineageKind::SendContext {
                "\n## 用户备注\n\n"
            } else {
                "\n## 交接备注\n\n"
            });
            header.push_str(note);
            header.push('\n');
        }
        match input.kind {
            LineageKind::Handoff => {
                header.push_str("\n## 会话模式：上下文接力\n\n");
                header.push_str("你正在从上游会话接手当前任务。**本轮作为交接初始化轮次，请仅读取并消化上述交接上下文，本轮不要修改任何文件、不要执行变更类命令、也不要提前开始改动。** 请简短确认已理解上下文，然后等待用户的下一条指令再开始操作。\n");
            }
            LineageKind::Fork => {
                header.push_str("\n## 会话模式：会话分叉\n\n");
                header.push_str("你正在从上游会话分叉出的独立探索分支中工作。**本轮作为分叉初始化轮次，请仅读取并消化上述交接上下文，本轮不要修改任何文件、不要执行变更类命令、也不要提前开始改动。** 请简短确认已理解上下文，然后等待用户的下一条指令。\n");
            }
            LineageKind::SideChat => {
                header.push_str("\n## 会话模式：只读旁路讨论\n\n");
                header.push_str("这是一个位于主智能体旁边的只读旁路讨论窗口。**本轮请仅读取并消化上述交接上下文，不要修改任何文件或执行变更类命令。** 除非用户在本对话中明确要求，否则请始终保持只读分析（不要改动工作区文件、不要执行 git 写操作或破坏性命令），简短确认已理解上下文后等待用户提问。\n");
            }
            LineageKind::SendContext => {
                header.push_str("\n## 上下文说明\n\n");
                header.push_str(&format!(
                    "以下内容由当前另一个活跃会话 `{}` 直接发送过来。请结合本轮附带的用户备注或问题（如有）理解上下文并直接回应或继续协助处理。\n",
                    input.source_address
                ));
            }
        }
        if let Some((orig, wt)) = wt_pair {
            header.push_str("\n## Worktree 隔离约束（必须严格遵守）\n\n");
            header.push_str(&format!(
                "- **当前独立 Worktree 目录（唯一允许操作的源码目录）：** `{wt}`\n- **上游主 Agent 原目录（严禁触碰）：** `{orig}`\n- **硬性规则：** 后续所有文件读取、编辑、测试与 git 命令都**必须严格限定在 `{wt}` 目录内**（使用相对路径或 `{wt}/...`），**绝对禁止读取、修改、写入或 `cd` 到 `{orig}`**。下方历史记录中的原目录路径已自动映射为 `{wt}`。\n"
            ));
        }
    } else {
        header.push_str(&format!(
            "# {} Context Package (from `{}`)\n\n",
            input.kind.display_label(),
            input.source_address
        ));
        header.push_str(&format!("- **Source address:** `{}`\n", input.source_address));
        header.push_str(&format!("- **Kind:** `{}`\n", input.kind.as_str()));
        if let Some(p) = input.source_provider.map(str::trim).filter(|s| !s.is_empty()) {
            header.push_str(&format!("- **Source provider:** `{p}`\n"));
        }
        if let Some(m) = input.source_model.map(str::trim).filter(|s| !s.is_empty()) {
            header.push_str(&format!("- **Source model:** `{m}`\n"));
        }
        if let Some(sid) = input.source_session_id.map(str::trim).filter(|s| !s.is_empty()) {
            header.push_str(&format!("- **Source session ID:** `{sid}`\n"));
        }
        if let Some(cwd) = input.cwd.map(str::trim).filter(|s| !s.is_empty()) {
            header.push_str(&format!("- **Working directory:** `{cwd}`\n"));
        }
        if !selected_turns.is_empty() {
            header.push_str(&format!(
                "- **Selected turns:** {} (of {total_turns} total)\n",
                format_turn_ranges(&selected_turns)
            ));
        }
        if let Some(note) = input.note.map(str::trim).filter(|s| !s.is_empty()) {
            header.push_str(if input.kind == LineageKind::SendContext {
                "\n## User Note\n\n"
            } else {
                "\n## Handoff Note\n\n"
            });
            header.push_str(note);
            header.push('\n');
        }
        match input.kind {
            LineageKind::Handoff => {
                header.push_str("\n## Session Mode: Context Handoff\n\n");
                header.push_str("You are taking over this task from the upstream session. **On this initial handoff turn, ONLY read and absorb the handoff context — do NOT modify any files, run mutating commands, or start making changes.** Briefly acknowledge that you have understood the context and wait for the user's next instruction before making any edits.\n");
            }
            LineageKind::Fork => {
                header.push_str("\n## Session Mode: Forked Exploration\n\n");
                header.push_str("You are exploring an independent branch of conversation from the upstream session. **On this initial fork turn, ONLY read and absorb the handoff context — do NOT modify any files, run mutating commands, or start making changes.** Briefly acknowledge the context and wait for the user's next instruction before proceeding.\n");
            }
            LineageKind::SideChat => {
                header.push_str("\n## Session Mode: Read-Only Side Chat\n\n");
                header.push_str("This is an isolated side-channel discussion window alongside the user's main agent. **On this initial turn, ONLY read and absorb the handoff context — do NOT modify any files or run mutating commands.** Keep this session read-only (Do NOT modify workspace files, run git write commands, or execute destructive/state-changing shell commands unless the user explicitly overrides this rule in this chat), briefly acknowledge the context, and wait for the user's question.\n");
            }
            LineageKind::SendContext => {
                header.push_str("\n## Context Note\n\n");
                header.push_str(&format!(
                    "Below is recent conversation context forwarded directly from another live session (`{}`). Use it together with the attached user note or request (if any) to respond or act directly.\n",
                    input.source_address
                ));
            }
        }
        if let Some((orig, wt)) = wt_pair {
            header.push_str("\n## Worktree Isolation Rules (STRICT)\n\n");
            header.push_str(&format!(
                "- **Isolated worktree directory (the ONLY source directory you may touch):** `{wt}`\n- **Upstream parent directory (FORBIDDEN):** `{orig}`\n- **Hard rule:** Every file read, edit, test, and shell/git command **MUST stay strictly inside `{wt}`** (use relative paths or `{wt}/...`). **NEVER read, edit, write, or `cd` into `{orig}`.** Historical paths below have already been remapped to `{wt}`.\n"
            ));
        }
    }

    // Git workspace state
    let git_section = input
        .cwd
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|c| git_workspace_summary(Path::new(c)))
        .unwrap_or_default();

    // Latest plan across selected turns
    let latest_plan = per_turn.values().rev().find_map(|b| b.plan.as_ref());
    let plan_section = match latest_plan {
        Some(items) if !items.is_empty() => {
            let mut s = String::from("\n## Latest Plan\n\n");
            for item in items.iter().take(30) {
                s.push_str(&format!("- {item}\n"));
            }
            truncate_utf8_with_ellipsis(&s, 2048)
        }
        _ => String::new(),
    };

    let distilled_section = input
        .omitted_summary
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|summary| {
            let heading = if input.is_zh {
                "\n## 省略轮次语义记忆蒸馏\n\n"
            } else {
                "\n## Distilled Memory of Omitted Turns\n\n"
            };
            let mut block = format!("{heading}{summary}");
            if !block.ends_with('\n') {
                block.push('\n');
            }
            let cap = (budget / 3).clamp(512, 3584);
            truncate_utf8_with_ellipsis(&block, cap)
        })
        .unwrap_or_default();

    let reserve_footer = 640usize + distilled_section.len();
    let mut fixed = String::new();
    fixed.push_str(&header);
    if fixed.len() + git_section.len() + reserve_footer < budget {
        fixed.push_str(&git_section);
    }
    if fixed.len() + plan_section.len() + reserve_footer < budget {
        fixed.push_str(&plan_section);
    }

    let first_turn = per_turn.keys().next().copied();
    let mut initial_section = String::new();
    let mut included_turns = Vec::new();

    if let Some(t1) = first_turn
        && let Some(b1) = per_turn.get(&t1)
    {
        let max_t1 = budget.saturating_sub(fixed.len() + reserve_footer).min(4096);
        let rendered_t1 = render_turn_section(t1, b1, max_t1, t1 == 1);
        if !rendered_t1.is_empty() && fixed.len() + rendered_t1.len() + reserve_footer <= budget {
            initial_section.push_str(&rendered_t1);
            included_turns.push(t1);
        }
    }

    let turns_budget = budget.saturating_sub(fixed.len() + initial_section.len() + reserve_footer);
    let mut recent_sections: Vec<(u64, String)> = Vec::new();
    let mut recent_used = 0usize;

    for (&turn, bucket) in per_turn.iter().rev() {
        if Some(turn) == first_turn && !initial_section.is_empty() {
            continue;
        }
        let avail = turns_budget.saturating_sub(recent_used);
        if avail < 120 {
            break;
        }
        let max_this = if recent_sections.is_empty() { avail } else { avail.min(6 * 1024) };
        let rendered = render_turn_section(turn, bucket, max_this, false);
        if rendered.is_empty() {
            continue;
        }
        if recent_used + rendered.len() <= turns_budget {
            recent_used += rendered.len();
            recent_sections.push((turn, rendered));
            included_turns.push(turn);
        } else if recent_sections.is_empty() {
            let clipped = truncate_utf8_with_ellipsis(&rendered, avail);
            if !clipped.is_empty() {
                recent_sections.push((turn, clipped));
                included_turns.push(turn);
            }
            break;
        } else {
            break;
        }
    }

    included_turns.sort_unstable();
    included_turns.dedup();
    let omitted_turns: Vec<u64> = per_turn.keys().copied().filter(|t| !included_turns.contains(t)).collect();

    recent_sections.reverse();

    let mut body = fixed;
    if !initial_section.is_empty() {
        body.push_str(&initial_section);
    }
    if !recent_sections.is_empty() {
        body.push_str("\n## Recent Turns\n");
        for (_, sec) in recent_sections {
            body.push_str(&sec);
        }
    }
    if !omitted_turns.is_empty() {
        if !distilled_section.is_empty() {
            body.push_str(&distilled_section);
        }
        body.push_str("\n## Omitted History (On-Demand Lookup)\n\n");
        body.push_str(&format!(
            "Middle turns ({}) were omitted to stay within the {} KiB handoff budget.\n",
            format_turn_ranges(&omitted_turns),
            budget / 1024
        ));
        body.push_str("Use `h2 read` (or MCP tool `h2_read`) to inspect specific omitted turn ranges:\n");
        for (a, b) in contiguous_turn_ranges(&omitted_turns) {
            body.push_str(&format!("- `h2 read {} --from-turn {a} --to-turn {b}`\n", input.source_address));
        }
    }

    if body.len() > budget {
        body = truncate_utf8_with_ellipsis(&body, budget);
    }
    let bytes = body.len();

    HandoffPackage { markdown: body, included_turns, omitted_turns, total_turns, bytes }
}

fn render_turn_section(turn: u64, bucket: &TurnBucket, max_bytes: usize, is_turn_one: bool) -> String {
    if max_bytes < 64 {
        return String::new();
    }
    let mut s = if is_turn_one {
        String::from("\n## Original Objective (Turn 1)\n\n")
    } else {
        format!("\n### Turn {turn}\n\n")
    };
    if !bucket.users.is_empty() {
        s.push_str("**User:**\n");
        for u in &bucket.users {
            s.push_str(u.trim());
            s.push_str("\n\n");
        }
    }
    if !bucket.tools.is_empty() {
        s.push_str("**Tool Activity:**\n");
        let tool_cap = 12usize;
        for line in bucket.tools.iter().take(tool_cap) {
            s.push_str(line);
            s.push('\n');
        }
        if bucket.tools.len() > tool_cap {
            s.push_str(&format!("- ... ({} more tool calls omitted)\n", bucket.tools.len() - tool_cap));
        }
        s.push('\n');
    }
    if !bucket.errors.is_empty() {
        s.push_str("**Errors:**\n");
        for e in &bucket.errors {
            s.push_str(&format!("- {e}\n"));
        }
        s.push('\n');
    }
    if !bucket.assistants.is_empty() {
        s.push_str("**Assistant:**\n");
        for a in &bucket.assistants {
            s.push_str(a.trim());
            s.push_str("\n\n");
        }
    }
    truncate_utf8_with_ellipsis(&s, max_bytes)
}

fn git_workspace_summary(cwd: &Path) -> Option<String> {
    if !cwd.exists() {
        return None;
    }
    let inside = crate::drove::bg_std_command("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !inside.status.success() || String::from_utf8_lossy(&inside.stdout).trim() != "true" {
        return None;
    }
    let branch_out = crate::drove::bg_std_command("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let status_out = crate::drove::bg_std_command("git")
        .args(["status", "--short"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
        .unwrap_or_default();

    let mut s = String::from("\n## Workspace Git State\n\n");
    if let Some(b) = branch_out.filter(|b| !b.is_empty()) {
        s.push_str(&format!("- **Branch:** `{b}`\n"));
    }
    if status_out.is_empty() {
        s.push_str("- **Working tree:** clean\n");
    } else {
        let mut lines: Vec<&str> = status_out.lines().take(40).collect();
        let total = status_out.lines().count();
        s.push_str("```\n");
        s.push_str(&lines.join("\n"));
        if total > lines.len() {
            s.push_str(&format!("\n... ({} more modified paths)", total - lines.len()));
        }
        let _ = &mut lines;
        s.push_str("\n```\n");
    }
    Some(truncate_utf8_with_ellipsis(&s, 2560))
}

fn truncate_utf8_with_ellipsis(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    if max_bytes <= 20 {
        let mut end = max_bytes.min(s.len());
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        return s[..end].to_string();
    }
    let suffix = "\n... (truncated)\n";
    let cap = max_bytes.saturating_sub(suffix.len());
    let mut end = cap.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_keeps_turn_1_newest_turns_and_omitted_lookup_hint() {
        let mut entries = Vec::new();
        for t in 1..=12u64 {
            let padding = "x".repeat(1200);
            entries.push(Entry::User {
                turn: t,
                from: None,
                text: format!("user turn {t}: {padding}"),
                attachments: vec![],
                ts_ms: t,
            });
            entries.push(Entry::Assistant {
                turn: t,
                text: format!("assistant turn {t}: {padding}"),
                attachments: vec![],
                ts_ms: t + 100,
            });
        }
        entries.push(Entry::Plan { turn: 11, items: vec!["Step A".into(), "Step B".into()], ts_ms: 999 });

        let pkg = build_handoff_package(&HandoffBuildInput {
            kind: LineageKind::Handoff,
            source_address: "a1",
            source_provider: Some("claude"),
            source_session_id: Some("sess-123"),
            source_model: Some("claude-sonnet-4"),
            cwd: None,
            entries: &entries,
            from_turn: None,
            to_turn: None,
            max_bytes: 8 * 1024,
            note: Some("Continue implementation"),
            is_zh: false,
            worktree_origin: None,
            omitted_summary: None,
        });

        assert!(pkg.bytes <= 8 * 1024, "package exceeded budget: {}", pkg.bytes);
        assert!(pkg.included_turns.contains(&1), "Turn 1 must be preserved");
        assert!(pkg.included_turns.contains(&12), "Latest turn must be preserved");
        assert!(!pkg.omitted_turns.is_empty(), "Middle turns should be omitted under 8 KiB budget");
        assert!(pkg.markdown.contains("Original Objective (Turn 1)"));
        assert!(pkg.markdown.contains("Step A"));
        assert!(pkg.markdown.contains("h2 read a1 --from-turn"));

        // When omitted_summary is provided, it is embedded ahead of the lookup hint within budget.
        let pkg_distilled = build_handoff_package(&HandoffBuildInput {
            kind: LineageKind::Handoff,
            source_address: "a1",
            source_provider: Some("claude"),
            source_session_id: Some("sess-123"),
            source_model: Some("claude-sonnet-4"),
            cwd: None,
            entries: &entries,
            from_turn: None,
            to_turn: None,
            max_bytes: 8 * 1024,
            note: Some("Continue implementation"),
            is_zh: true,
            worktree_origin: None,
            omitted_summary: Some("- **已排除的错误方案：** 不要使用全局锁"),
        });
        assert!(pkg_distilled.bytes <= 8 * 1024);
        assert!(pkg_distilled.markdown.contains("省略轮次语义记忆蒸馏"));
        assert!(pkg_distilled.markdown.contains("不要使用全局锁"));
        assert!(pkg_distilled.markdown.contains("h2 read a1 --from-turn"));
    }

    #[test]
    fn turn_range_formatting_and_pty_defaults() {
        assert_eq!(format_turn_ranges(&[2, 3, 4, 7, 9, 10]), "2-4, 7, 9-10");
        assert_eq!(default_pty_argv("claude"), vec!["claude", "--dangerously-skip-permissions"]);
        assert_eq!(default_pty_argv("codex")[0], "codex");
        assert_eq!(default_pty_argv("zcode"), vec!["zcode", "--mode", "yolo"]);
        assert_eq!(default_pty_argv("zcode-cli"), vec!["zcode", "--mode", "yolo"]);
        let wrapped = wrap_handoff_envelope(LineageKind::Fork, "t2", "# Body\nhi");
        assert_eq!(strip_h2_envelope(&format!("{wrapped}\n\nnext prompt")), "next prompt");
    }

    #[test]
    fn side_chat_package_includes_read_only_constraint_and_fork_worktree_roundtrips() {
        let pkg = build_handoff_package(&HandoffBuildInput {
            kind: LineageKind::SideChat,
            source_address: "t1",
            source_provider: Some("claude"),
            source_session_id: None,
            source_model: None,
            cwd: None,
            entries: &[Entry::User {
                turn: 1,
                from: None,
                text: "Check caching strategy".into(),
                attachments: vec![],
                ts_ms: 1,
            }],
            from_turn: None,
            to_turn: None,
            max_bytes: 8 * 1024,
            note: None,
            is_zh: false,
            worktree_origin: None,
            omitted_summary: None,
        });
        assert!(pkg.markdown.contains("Read-Only Side Chat"));
        assert!(pkg.markdown.contains("Do NOT modify workspace files"));

        let zh_pkg = build_handoff_package(&HandoffBuildInput {
            kind: LineageKind::Handoff,
            source_address: "t1",
            source_provider: Some("claude"),
            source_session_id: None,
            source_model: None,
            cwd: None,
            entries: &[Entry::User {
                turn: 1, from: None, text: "检查缓存策略".into(), attachments: vec![], ts_ms: 1
            }],
            from_turn: None,
            to_turn: None,
            max_bytes: 8 * 1024,
            note: None,
            is_zh: true,
            worktree_origin: None,
            omitted_summary: None,
        });
        assert!(zh_pkg.markdown.contains("会话模式：上下文接力"));
        assert!(zh_pkg.markdown.contains("本轮不要修改任何文件"));

        // Verify fork worktree creation carries uncommitted edits and merges back cleanly.
        let root = crate::test_tmp().join(format!("h2-fork-wt-{}", crate::platform::random_hex()));
        let repo = root.join("repo");
        let state = root.join("state");
        std::fs::create_dir_all(&repo).unwrap();
        let run_git = |cwd: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-C", &cwd.to_string_lossy()])
                .args(args)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@local")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@local")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        };
        run_git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("app.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "app.txt"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        // Uncommitted edit in origin repo must be rejected by preflight check.
        std::fs::write(repo.join("app.txt"), "base\norigin-dirty\n").unwrap();
        let blocked = crate::drove::integrate::create_fork_worktree(&state, &repo.to_string_lossy(), "t1").unwrap_err();
        assert!(blocked.contains("\"code\":\"dirty_worktree\""), "expected dirty_worktree blocker: {blocked}");

        // Commit the edit so origin repo is clean, then fork worktree succeeds directly off HEAD.
        run_git(&repo, &["add", "app.txt"]);
        run_git(&repo, &["commit", "-q", "-m", "origin clean"]);

        let (wt, branch) =
            crate::drove::integrate::create_fork_worktree(&state, &repo.to_string_lossy(), "t1").unwrap();
        let wt_path = Path::new(&wt);
        assert!(wt_path.is_dir());
        assert_eq!(branch, "herdr/fork-t1");
        assert_eq!(std::fs::read_to_string(wt_path.join("app.txt")).unwrap(), "base\norigin-dirty\n");

        // Fork worktree must be on a real branch (not detached HEAD) so the agent can commit freely.
        let head_branch = String::from_utf8_lossy(
            &std::process::Command::new("git")
                .args(["-C", &wt, "rev-parse", "--abbrev-ref", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();
        assert_eq!(head_branch, branch, "fork worktree must not be detached HEAD");

        // Commit a file on the fork branch + leave one uncommitted edit, then inspect (read-only).
        std::fs::write(wt_path.join("fork_committed.txt"), "committed on fork\n").unwrap();
        run_git(wt_path, &["add", "fork_committed.txt"]);
        run_git(wt_path, &["commit", "-q", "-m", "fork feature"]);
        std::fs::write(wt_path.join("fork_wip.txt"), "wip\n").unwrap();

        let summary = crate::drove::integrate::inspect_fork_worktree(&repo.to_string_lossy(), &wt, Some(&branch));
        assert_eq!(summary.branch, branch);
        assert!(summary.ahead_commits.iter().any(|c| c.contains("fork feature")), "summary={summary:?}");
        assert!(summary.dirty_files.iter().any(|f| f.contains("fork_wip.txt")), "summary={summary:?}");
        assert!(wt_path.is_dir(), "inspect must never delete the worktree");

        // Permanent cleanup must delete both the worktree directory and the fork Git branch.
        crate::drove::integrate::cleanup_fork_worktree(&repo.to_string_lossy(), wt_path, Some(&branch));
        assert!(!wt_path.exists(), "worktree dir should be removed on cleanup");
        let branch_exists = std::process::Command::new("git")
            .args(["-C", &repo.to_string_lossy(), "rev-parse", "--verify", &format!("refs/heads/{branch}")])
            .output()
            .unwrap()
            .status
            .success();
        assert!(!branch_exists, "fork branch must be deleted on cleanup");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn worktree_fork_rewrites_origin_paths_and_injects_isolation_rules() {
        let origin = "/home/user/proj";
        let wt = "/home/user/.local/state/herdr2/worktrees/forks/proj-fk1";
        assert_eq!(
            rewrite_origin_paths(
                "Edit /home/user/proj/calc.py and keep /home/user/proj-other/calc.py and /home/user/proj，完成",
                origin,
                wt
            ),
            format!("Edit {wt}/calc.py and keep /home/user/proj-other/calc.py and {wt}，完成")
        );

        let entries = vec![
            Entry::User {
                turn: 1,
                from: None,
                text: "请修改 /home/user/proj/calc.py 添加乘法".into(),
                attachments: vec![],
                ts_ms: 1,
            },
            Entry::ToolCall {
                turn: 1,
                id: "tc1".into(),
                title: "Write /home/user/proj/calc.py".into(),
                tool_kind: None,
                status: "completed".into(),
                detail: None,
                attachments: vec![],
                ts_ms: 2,
            },
            Entry::Assistant {
                turn: 1,
                text: "已更新 `/home/user/proj/calc.py`，未动 `/home/user/proj2/x.py`。".into(),
                attachments: vec![],
                ts_ms: 3,
            },
        ];

        let pkg_zh = build_handoff_package(&HandoffBuildInput {
            kind: LineageKind::Fork,
            source_address: "t1",
            source_provider: Some("claude"),
            source_session_id: None,
            source_model: None,
            cwd: Some(wt),
            entries: &entries,
            from_turn: None,
            to_turn: None,
            max_bytes: 16 * 1024,
            note: None,
            is_zh: true,
            worktree_origin: Some(origin),
            omitted_summary: None,
        });
        assert!(pkg_zh.markdown.contains("Worktree 隔离约束"));
        assert!(pkg_zh.markdown.contains(&format!("Write {wt}/calc.py")));
        assert!(!pkg_zh.markdown.contains(&format!("Write {origin}/calc.py")));
        assert!(pkg_zh.markdown.contains("/home/user/proj2/x.py"));

        let pkg_en = build_handoff_package(&HandoffBuildInput {
            kind: LineageKind::Fork,
            source_address: "t1",
            source_provider: Some("claude"),
            source_session_id: None,
            source_model: None,
            cwd: Some(wt),
            entries: &entries,
            from_turn: None,
            to_turn: None,
            max_bytes: 16 * 1024,
            note: None,
            is_zh: false,
            worktree_origin: Some(origin),
            omitted_summary: None,
        });
        assert!(pkg_en.markdown.contains("Worktree Isolation Rules (STRICT)"));
        assert!(pkg_en.markdown.contains(&format!("Write {wt}/calc.py")));
    }

    #[test]
    fn send_context_package_uses_collaborative_framing_and_user_note() {
        let entries = vec![
            Entry::User { turn: 1, from: None, text: "Check auth timeout".into(), attachments: vec![], ts_ms: 1 },
            Entry::Assistant { turn: 1, text: "Found 5s timeout in auth.rs".into(), attachments: vec![], ts_ms: 2 },
        ];
        let pkg_zh = build_handoff_package(&HandoffBuildInput {
            kind: LineageKind::SendContext,
            source_address: "t1",
            source_provider: Some("claude"),
            source_session_id: None,
            source_model: None,
            cwd: None,
            entries: &entries,
            from_turn: None,
            to_turn: None,
            max_bytes: 16 * 1024,
            note: Some("请帮我顺手看看是否需要加单元测试"),
            is_zh: true,
            worktree_origin: None,
            omitted_summary: None,
        });
        assert!(pkg_zh.markdown.contains("发送上下文"));
        assert!(pkg_zh.markdown.contains("## 用户备注"));
        assert!(pkg_zh.markdown.contains("请帮我顺手看看是否需要加单元测试"));
        assert!(!pkg_zh.markdown.contains("本轮不要修改任何文件"));
        assert!(pkg_zh.markdown.contains("Found 5s timeout in auth.rs"));

        let pkg_en = build_handoff_package(&HandoffBuildInput {
            kind: LineageKind::SendContext,
            source_address: "a2",
            source_provider: Some("codex"),
            source_session_id: None,
            source_model: None,
            cwd: None,
            entries: &entries,
            from_turn: None,
            to_turn: None,
            max_bytes: 16 * 1024,
            note: Some("Please review and suggest a fix"),
            is_zh: false,
            worktree_origin: None,
            omitted_summary: None,
        });
        assert!(pkg_en.markdown.contains("Shared Context"));
        assert!(pkg_en.markdown.contains("## User Note"));
        assert!(pkg_en.markdown.contains("Please review and suggest a fix"));
        assert!(!pkg_en.markdown.contains("do NOT modify any files"));
    }
}
