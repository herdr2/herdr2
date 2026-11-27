//! Agent execution status reports derived from ACP turns and permission prompts.
//! The turn goes up each time the root record leaves `working`, for any reason.

use crate::agents::Agent;
use crate::terminals::Terminal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const MAX_RECORDS: usize = 256;

/// Structured status update analogous to legacy status reports.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusReport {
    pub state: &'static str,
    pub kind: Option<&'static str>,
    pub progress: Option<u8>,
    pub id: String,
    pub app: String,
    pub title: String,
    pub message: String,
}

impl StatusReport {
    pub fn root(state: &'static str) -> Self {
        Self {
            state,
            kind: None,
            progress: None,
            id: String::new(),
            app: String::new(),
            title: String::new(),
            message: String::new(),
        }
    }
}

/// One status record. Words describe ACP execution state cleanly without translation.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct Status {
    /// idle, working, done, blocked or error.
    pub state: String,
    /// permission, question or auth (blocked only, when relevant).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// 0..100 (working or blocked, when reported).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub app: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub msg: String,
}

impl Status {
    fn from(r: &StatusReport) -> Status {
        Status {
            state: r.state.into(),
            kind: r.kind.map(Into::into),
            progress: r.progress,
            app: r.app.clone(),
            title: r.title.clone(),
            msg: r.message.clone(),
        }
    }

    /// The report again as OSC 7501, for a new viewer's first paint.
    pub fn encode(&self, id: &str) -> String {
        let mut fields = vec![format!("state={}", self.state)];
        let mut push = |key: &str, value: &str| {
            if !value.is_empty() {
                fields.push(format!("{key}={value}"));
            }
        };
        push("id", id);
        push("kind", self.kind.as_deref().unwrap_or(""));
        push("progress", &self.progress.map(|p| p.to_string()).unwrap_or_default());
        push("app", &self.app);
        push("title", &crate::put::encode(self.title.as_bytes()));
        push("msg", &crate::put::encode(self.msg.as_bytes()));
        format!("\x1b]7501;{}\x1b\\", fields.join(":"))
    }
}

/// Shared status-state target for both headless ACP agents and PTY terminals.
trait StatusTarget {
    fn status(&self) -> Option<&Status>;
    fn set_status(&mut self, s: Option<Status>);
    fn records_mut(&mut self) -> &mut BTreeMap<String, Status>;
    fn inc_turn(&mut self);
    fn set_status_ms(&mut self, ms: Option<u64>);
    fn working_since(&self) -> Option<u64>;
    fn set_working_since(&mut self, ms: Option<u64>);
    fn work_ms(&self) -> Option<u64>;
    fn set_work_ms(&mut self, ms: Option<u64>);
    fn total_work_ms(&self) -> u64;
    fn set_total_work_ms(&mut self, ms: u64);
    fn set_reported(&mut self);
}

impl StatusTarget for Agent {
    fn status(&self) -> Option<&Status> {
        self.status.as_ref()
    }
    fn set_status(&mut self, s: Option<Status>) {
        self.status = s;
    }
    fn records_mut(&mut self) -> &mut BTreeMap<String, Status> {
        &mut self.records
    }
    fn inc_turn(&mut self) {
        self.turn += 1;
    }
    fn set_status_ms(&mut self, ms: Option<u64>) {
        self.status_ms = ms;
    }
    fn working_since(&self) -> Option<u64> {
        self.working_since
    }
    fn set_working_since(&mut self, ms: Option<u64>) {
        self.working_since = ms;
    }
    fn work_ms(&self) -> Option<u64> {
        self.work_ms
    }
    fn set_work_ms(&mut self, ms: Option<u64>) {
        self.work_ms = ms;
    }
    fn total_work_ms(&self) -> u64 {
        self.total_work_ms
    }
    fn set_total_work_ms(&mut self, ms: u64) {
        self.total_work_ms = ms;
    }
    fn set_reported(&mut self) {
        self.reported = true;
    }
}

impl StatusTarget for Terminal {
    fn status(&self) -> Option<&Status> {
        self.status.as_ref()
    }
    fn set_status(&mut self, s: Option<Status>) {
        self.status = s;
    }
    fn records_mut(&mut self) -> &mut BTreeMap<String, Status> {
        &mut self.records
    }
    fn inc_turn(&mut self) {
        self.turn += 1;
    }
    fn set_status_ms(&mut self, ms: Option<u64>) {
        self.status_ms = ms;
    }
    fn working_since(&self) -> Option<u64> {
        self.working_since
    }
    fn set_working_since(&mut self, ms: Option<u64>) {
        self.working_since = ms;
    }
    fn work_ms(&self) -> Option<u64> {
        self.work_ms
    }
    fn set_work_ms(&mut self, ms: Option<u64>) {
        self.work_ms = ms;
    }
    fn total_work_ms(&self) -> u64 {
        self.total_work_ms
    }
    fn set_total_work_ms(&mut self, ms: u64) {
        self.total_work_ms = ms;
    }
    fn set_reported(&mut self) {
        self.reported = true;
    }
}

/// What a report changed, for events and waiters.
#[derive(Default, Debug, PartialEq)]
pub struct Change {
    pub status: bool,
    /// A fresh root report, not a child record or an unchanged recovery replay.
    pub root_report: bool,
    pub turn_ended: bool,
    /// It just got stuck: blocked on the user or errored (wasn't that before).
    pub stuck: bool,
}

fn working<T: StatusTarget>(a: &T) -> bool {
    a.status().is_some_and(|s| s.state == "working")
}

fn end_turn<T: StatusTarget>(a: &mut T, was_working: bool) -> bool {
    let ended = was_working && !working(a);
    if ended {
        a.inc_turn();
    }
    ended
}

fn stuck<T: StatusTarget>(a: &T) -> Option<&str> {
    a.status().map(|s| s.state.as_str()).filter(|w| *w == "blocked" || *w == "error")
}

fn word<T: StatusTarget>(a: &T) -> Option<String> {
    a.status().map(|s| s.state.clone())
}

fn stamp<T: StatusTarget>(a: &mut T, was: Option<String>) {
    let now_word = word(a);
    if now_word != was {
        let now_ms = crate::runtime::now_ms();
        a.set_status_ms(Some(now_ms));
        let was_working = was.as_deref() == Some("working");
        let is_working = now_word.as_deref() == Some("working");
        if was_working && !is_working {
            let prev_run = a.work_ms().unwrap_or(0);
            let new_run = a.working_since().map_or(prev_run, |ws| now_ms.saturating_sub(ws).max(prev_run));
            let delta = new_run.saturating_sub(prev_run);
            a.set_work_ms(Some(new_run));
            a.set_total_work_ms(a.total_work_ms().saturating_add(delta));
            a.set_working_since(None);
        } else if !was_working && is_working {
            let resuming = matches!(was.as_deref(), Some("blocked" | "error")) && a.work_ms().is_some();
            let prior = if resuming { a.work_ms().unwrap_or(0) } else { 0 };
            a.set_work_ms(Some(prior));
            a.set_working_since(Some(now_ms.saturating_sub(prior)));
        }
    }
}

fn apply_target<T: StatusTarget>(a: &mut T, r: &StatusReport) -> Change {
    let (was_working, was_word) = (working(a), word(a));
    let was = stuck(a).map(String::from);
    a.set_reported();
    if r.state == "clear" {
        if r.id.is_empty() {
            a.set_status(None);
            a.records_mut().clear();
        } else {
            let child = format!("{}/", r.id);
            a.records_mut().retain(|id, _| *id != r.id && !id.starts_with(&child));
        }
    } else if r.id.is_empty() {
        a.set_status(Some(Status::from(r)));
    } else {
        // Over the limit, the record updated longest ago goes (approximated by first id).
        if !a.records_mut().contains_key(&r.id) && a.records_mut().len() >= MAX_RECORDS - 1 {
            let oldest = a.records_mut().keys().next().cloned().expect("non-empty");
            a.records_mut().remove(&oldest);
        }
        a.records_mut().insert(r.id.clone(), Status::from(r));
    }
    stamp(a, was_word);
    Change {
        status: true,
        root_report: r.id.is_empty(),
        turn_ended: end_turn(a, was_working),
        stuck: stuck(a).is_some() && stuck(a) != was.as_deref(),
    }
}

fn reset_target<T: StatusTarget>(a: &mut T) -> Change {
    let (was_working, was_word) = (working(a), word(a));
    let had = a.status().is_some() || !a.records_mut().is_empty();
    a.set_status(None);
    a.records_mut().clear();
    stamp(a, was_word);
    Change { status: had, turn_ended: end_turn(a, was_working), ..Default::default() }
}

pub fn apply(a: &mut Agent, r: &StatusReport) -> Change {
    apply_target(a, r)
}

/// The agent exited: its status facts reset.
pub fn reset(a: &mut Agent) -> Change {
    reset_target(a)
}

pub fn apply_terminal(t: &mut Terminal, r: &ghostty_vt::StatusReport) -> Change {
    let report = StatusReport {
        state: r.state,
        kind: r.kind,
        progress: r.progress,
        id: r.id.clone(),
        app: r.app.clone(),
        title: r.title.clone(),
        message: r.message.clone(),
    };
    apply_target(t, &report)
}

pub fn reset_terminal(t: &mut Terminal) -> Change {
    reset_target(t)
}

/// Concatenates all assistant text chunks emitted during `turn_num`.
pub fn acp_turn_assistant_text(entries: &[crate::transcript::Entry], turn_num: u64) -> String {
    use crate::transcript::Entry;
    let mut chunks: Vec<&str> = Vec::new();
    for e in entries {
        if e.turn() == Some(turn_num)
            && let Entry::Assistant { text, .. } = e
            && !text.is_empty()
        {
            chunks.push(text.as_str());
        }
    }
    chunks.concat()
}

/// Returns true if `turn_num` still has an explicitly pending interactive question tool call.
pub fn acp_turn_has_pending_question_tool(entries: &[crate::transcript::Entry], turn_num: u64) -> bool {
    use crate::transcript::Entry;
    entries.iter().any(|e| {
        e.turn() == Some(turn_num)
            && matches!(
                e,
                Entry::ToolCall { title, tool_kind, status, .. }
                    if (is_question_tool_name(&title.to_ascii_lowercase())
                        || is_question_tool_name(&tool_kind.as_deref().unwrap_or("").to_ascii_lowercase()))
                        && matches!(status.to_ascii_lowercase().as_str(), "pending" | "in_progress" | "waiting")
            )
    })
}

/// Detects whether a finished ACP turn stopped to ask the user a question or ask the user to choose.
/// Returns a concise summary string (for `Status.msg`) when user input/confirmation is required.
pub fn detect_acp_turn_needs_user_input(entries: &[crate::transcript::Entry], turn_num: u64) -> Option<String> {
    use crate::transcript::Entry;

    let mut assistant_chunks: Vec<&str> = Vec::new();
    let mut last_question_tool: Option<(String, bool)> = None;
    let mut saw_work_after_question_tool = false;

    for e in entries {
        if e.turn() != Some(turn_num) {
            continue;
        }
        match e {
            Entry::Assistant { text, .. } => {
                if !text.is_empty() {
                    assistant_chunks.push(text.as_str());
                }
            }
            Entry::ToolCall { title, tool_kind, status, .. } => {
                let low_title = title.to_ascii_lowercase();
                let low_kind = tool_kind.as_deref().unwrap_or("").to_ascii_lowercase();
                let is_q_tool = is_question_tool_name(&low_title) || is_question_tool_name(&low_kind);
                if is_q_tool {
                    let pending = matches!(status.to_ascii_lowercase().as_str(), "pending" | "in_progress" | "waiting");
                    last_question_tool = Some((title.clone(), pending));
                    saw_work_after_question_tool = false;
                } else if last_question_tool.is_some() {
                    saw_work_after_question_tool = true;
                }
            }
            _ => {}
        }
    }

    let combined = assistant_chunks.concat();
    if let Some(summary) = detect_text_needs_user_input(&combined) {
        return Some(summary);
    }

    if let Some((title, pending)) = last_question_tool
        && (pending || (!saw_work_after_question_tool && combined.trim().is_empty()))
    {
        return Some(compact_question_summary(&title));
    }

    None
}

fn is_question_tool_name(name: &str) -> bool {
    name.contains("ask_question")
        || name.contains("ask_user_question")
        || name.contains("askuserquestion")
        || name.contains("request_user_input")
        || name.contains("ask_user")
        || name.contains("prompt_user")
}

/// Detects whether an agent's final response text stopped to ask the user a question or request a choice.
pub fn detect_text_needs_user_input(text: &str) -> Option<String> {
    let cleaned = strip_fenced_code_blocks(text);
    let lines: Vec<&str> = cleaned
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !is_decorative_line(l))
        .collect();
    if lines.is_empty() {
        return None;
    }

    let tail_start = lines.len().saturating_sub(14);
    let tail = &lines[tail_start..];
    let last = *tail.last()?;

    let is_blocking_question_line = |line: &str| -> bool {
        !is_polite_signoff(line)
            && !is_optional_followup_offer(tail, line)
            && (line_ends_with_question(line) || has_explicit_decision_request(line))
    };

    // 1. Last non-empty line is a direct question or explicit choice/confirmation request.
    if is_blocking_question_line(last) {
        return Some(compact_question_summary(last));
    }

    // 2. Question within the last 3 lines followed only by brief hints/options/parentheticals.
    let recent_start = tail.len().saturating_sub(3);
    for idx in (recent_start..tail.len().saturating_sub(1)).rev() {
        let line = tail[idx];
        if is_blocking_question_line(line)
            && tail[idx + 1..].iter().all(|after| is_option_list_line(after) || is_short_choice_hint(after))
        {
            return Some(compact_question_summary(line));
        }
    }

    // 3. Contiguous trailing option list (>= 2 choices at the end of the reply) immediately
    //    preceded by a question or explicit choice prompt (never matching completed numbered summaries).
    let mut end_idx = tail.len();
    while end_idx > 0 && (is_short_choice_hint(tail[end_idx - 1]) || is_polite_signoff(tail[end_idx - 1])) {
        end_idx -= 1;
    }
    let mut opt_start = end_idx;
    while opt_start > 0 && is_option_list_line(tail[opt_start - 1]) {
        opt_start -= 1;
    }
    if end_idx.saturating_sub(opt_start) >= 2 && opt_start > 0 {
        for idx in (opt_start.saturating_sub(2)..opt_start).rev() {
            let intro = tail[idx];
            if is_option_list_line(intro) || is_polite_signoff(intro) || is_optional_followup_offer(tail, intro) {
                continue;
            }
            if line_ends_with_question(intro)
                || has_explicit_decision_request(intro)
                || is_explicit_choice_header(intro)
            {
                return Some(compact_question_summary(intro));
            }
        }
    }

    None
}

pub(crate) fn strip_fenced_code_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    let mut fence_char = '`';
    for line in text.lines() {
        let t = line.trim_start();
        if !in_fence {
            if t.starts_with("```") {
                in_fence = true;
                fence_char = '`';
                continue;
            } else if t.starts_with("~~~") {
                in_fence = true;
                fence_char = '~';
                continue;
            }
            out.push_str(line);
            out.push('\n');
        } else if (fence_char == '`' && t.starts_with("```")) || (fence_char == '~' && t.starts_with("~~~")) {
            in_fence = false;
        }
    }
    out
}

fn is_decorative_line(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.chars().all(|c| matches!(c, '-' | '=' | '*' | '_' | '─' | '━' | '═' | ' '))
}

fn is_polite_signoff(line: &str) -> bool {
    let low = line.to_ascii_lowercase();
    const ZH_SIGNOFFS: &[&str] = &[
        "还有什么需要",
        "还有其他需要",
        "还需要我做什么",
        "还有什么可以帮",
        "还有其他问题吗",
        "还有什么问题吗",
        "有其他问题吗",
        "有其他需要吗",
        "有什么其他需要",
        "随时告诉我",
        "随时联系我",
        "欢迎随时",
        "如有问题",
        "如果有问题",
        "如有任何问题",
        "有问题请",
        "如有需要",
        "如果有需要",
        "如有其他",
        "需要调整请",
        "请随时",
        "请查看",
        "请查收",
        "请查阅",
        "请确认效果",
        "请确认是否正常",
        "你可以运行",
        "您可以运行",
        "可以直接运行",
    ];
    if ZH_SIGNOFFS.iter().any(|p| line.contains(p)) {
        return true;
    }
    const EN_SIGNOFFS: &[&str] = &[
        "anything else",
        "any other questions",
        "how else can i help",
        "can i help you with anything",
        "let me know if you need anything",
        "let me know if you'd like anything",
        "let me know if you want any",
        "let me know if you'd like any",
        "let me know if there",
        "please let me know if",
        "if you have any questions",
        "if you need any adjustments",
        "if you'd like any adjustments",
        "feel free to ask",
        "feel free to let me know",
        "happy to help",
    ];
    EN_SIGNOFFS.iter().any(|p| low.contains(p))
}

fn tail_announces_completion(tail: &[&str]) -> bool {
    const ZH_DONE: &[&str] = &[
        "已完成",
        "已经完成",
        "改完了",
        "修改完毕",
        "修复完成",
        "已修复",
        "已实现",
        "已更新",
        "全部通过",
        "验证通过",
        "测试通过",
    ];
    const EN_DONE: &[&str] = &[
        "all tests pass",
        "tests pass",
        "changes are complete",
        "implementation is complete",
        "implemented and verified",
        "successfully updated",
        "successfully fixed",
        "i've completed",
        "i have completed",
    ];
    tail.iter().any(|line| {
        let low = line.to_ascii_lowercase();
        ZH_DONE.iter().any(|p| line.contains(p)) || EN_DONE.iter().any(|p| low.contains(p))
    })
}

fn is_optional_followup_offer(tail: &[&str], line: &str) -> bool {
    if !tail_announces_completion(tail) {
        return false;
    }
    let low = line.to_ascii_lowercase();
    const ZH_OFFERS: &[&str] = &[
        "需要我顺便",
        "要不要顺便",
        "需要顺便",
        "是否需要顺便",
        "需要帮你提交",
        "需要我提交",
        "要不要帮你提交",
        "需要我帮你跑",
        "要不要跑一下",
    ];
    if ZH_OFFERS.iter().any(|p| line.contains(p)) {
        return true;
    }
    const EN_OFFERS: &[&str] = &[
        "would you like me to commit",
        "would you like me to push",
        "would you like me to also",
        "want me to commit",
        "want me to push",
        "want me to also",
        "shall i commit",
        "should i also commit",
    ];
    EN_OFFERS.iter().any(|p| low.contains(p))
}

fn line_ends_with_question(line: &str) -> bool {
    let trimmed = line
        .trim()
        .trim_end_matches(|c: char| matches!(c, '*' | '_' | '`' | '"' | '\'' | '”' | '’' | ')' | '）' | ']' | '】' | '>'))
        .trim();
    if !(trimmed.ends_with('?') || trimmed.ends_with('？')) {
        return false;
    }
    let last_token = trimmed.split_whitespace().last().unwrap_or("");
    if last_token.contains("://") {
        return false;
    }
    // Reject bare "?" or markdown heading/placeholder with no words.
    trimmed.chars().filter(|c| !c.is_whitespace() && !matches!(c, '?' | '？' | '-' | '*' | '#')).count() >= 2
}

fn has_explicit_decision_request(line: &str) -> bool {
    let low = line.to_ascii_lowercase();
    const ZH_CUES: &[&str] = &[
        "请选择",
        "请先确认",
        "请先选择",
        "请你选择",
        "请你确认",
        "请确认是否",
        "请确认要",
        "请确认哪",
        "请确认目",
        "请决定",
        "请指定",
        "请提供",
        "请补充",
        "等你确认",
        "等你决定",
        "等你回复",
        "等你选择",
        "等待你确认",
        "等待你的确认",
        "等待你选择",
        "等待你的选择",
        "告诉我你的选择",
        "你更倾向",
        "您更倾向",
        "回复数字",
        "回复编号",
        "输入编号",
    ];
    if ZH_CUES.iter().any(|p| line.contains(p)) {
        return true;
    }
    const EN_CUES: &[&str] = &[
        "please choose",
        "please select",
        "please confirm",
        "please specify",
        "please clarify",
        "please provide",
        "please decide",
        "choose an option",
        "select an option",
        "pick an option",
        "pick one",
        "let me know which",
        "tell me which",
        "waiting for your confirmation",
        "waiting for your input",
        "waiting for your decision",
        "before i proceed",
        "before proceeding",
    ];
    EN_CUES.iter().any(|p| low.contains(p))
}

fn is_explicit_choice_header(line: &str) -> bool {
    let t = line.trim();
    if !(t.ends_with(':') || t.ends_with('：')) {
        return false;
    }
    let low = t.to_ascii_lowercase();
    const ZH_HEADERS: &[&str] = &[
        "可选方案",
        "候选方案",
        "以下方案",
        "两种方案",
        "三种方案",
        "几种方案",
        "可选项",
        "以下选项",
    ];
    if ZH_HEADERS.iter().any(|p| t.contains(p))
        && !t.contains("已完成")
        && !t.contains("已实现")
        && !t.contains("已修改")
        && !t.contains("已修复")
    {
        return true;
    }
    const EN_HEADERS: &[&str] = &[
        "options:",
        "available options:",
        "possible approaches:",
        "two options:",
        "three options:",
    ];
    EN_HEADERS.iter().any(|p| low.ends_with(p))
}

fn is_option_list_line(line: &str) -> bool {
    let t = line.trim_start_matches(|c: char| matches!(c, '-' | '*' | '•' | '❯' | '›' | '>' | ' '));
    let t = t.trim_start_matches(|c: char| matches!(c, '*' | '_' | '`'));
    let low = t.to_ascii_lowercase();
    let bytes = t.as_bytes();
    let numbered = bytes.len() >= 2
        && bytes[0].is_ascii_digit()
        && matches!(bytes[1], b'.' | b')' | b':')
        || (bytes.len() >= 3 && bytes[0].is_ascii_digit() && t[1..].starts_with('、'));
    let lettered = bytes.len() >= 2
        && matches!(bytes[0], b'A'..=b'F' | b'a'..=b'f')
        && (matches!(bytes[1], b'.' | b')' | b':') || t[1..].starts_with('：') || t[1..].starts_with('、'));
    let named = t.starts_with("方案")
        || t.starts_with("选项")
        || low.starts_with("option ")
        || low.starts_with("approach ")
        || low.starts_with("choice ");
    numbered || lettered || named
}

fn is_short_choice_hint(line: &str) -> bool {
    let t = line.trim();
    let low = t.to_ascii_lowercase();
    t.chars().count() <= 48
        && ((t.starts_with('(') && t.ends_with(')'))
            || (t.starts_with('（') && t.ends_with('）'))
            || t.contains("推荐")
            || t.contains("默认")
            || t.contains("回复")
            || low.contains("recommended")
            || low.contains("default")
            || low.contains("reply"))
}

fn compact_question_summary(raw: &str) -> String {
    let clean: String = raw
        .trim()
        .trim_start_matches(|c: char| matches!(c, '#' | '>' | '-' | '*' | '•' | ' '))
        .replace("**", "")
        .replace('`', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = clean.chars();
    let head: String = chars.by_ref().take(48).collect();
    if chars.next().is_some() { format!("{head}…") } else { head }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{NewAgent, Registry};

    fn agent() -> Agent {
        let mut r = Registry::default();
        let new = NewAgent { argv: vec!["x".into()], cwd: "/".into(), ..Default::default() };
        let id = r.create(new, 0).unwrap();
        r.agents.remove(&id).unwrap()
    }

    fn report(state: &'static str, id: &str) -> StatusReport {
        StatusReport { id: id.into(), ..StatusReport::root(state) }
    }

    #[test]
    fn the_turn_counts_every_way_of_leaving_working() {
        let mut a = agent();
        assert!(!apply(&mut a, &report("idle", "")).turn_ended);
        apply(&mut a, &report("working", ""));
        assert!(apply(&mut a, &report("done", "")).turn_ended); // finished
        apply(&mut a, &report("working", ""));
        assert!(apply(&mut a, &report("idle", "")).turn_ended); // interrupted
        apply(&mut a, &report("working", ""));
        assert!(apply(&mut a, &report("blocked", "")).turn_ended); // asks permission
        apply(&mut a, &report("working", ""));
        assert!(apply(&mut a, &report("clear", "")).turn_ended); // cleared
        apply(&mut a, &report("working", ""));
        assert!(!apply(&mut a, &report("working", "sub")).turn_ended, "child records don't count");
        assert!(reset(&mut a).turn_ended); // exited
        assert_eq!(a.turn, 5);
        assert!(a.status.is_none() && a.reported);
    }

    #[test]
    fn the_status_time_moves_only_when_the_word_changes() {
        let mut a = agent();
        assert_eq!(a.status_ms, None, "never said anything");
        apply(&mut a, &report("working", ""));
        let since = a.status_ms.expect("its first word");
        a.status_ms = Some(1);
        apply(&mut a, &report("working", ""));
        apply(&mut a, &report("done", "sub"));
        assert_eq!(a.status_ms, Some(1), "the same word again, or a child record");
        apply(&mut a, &report("done", ""));
        assert!(a.status_ms >= Some(since));
        a.status_ms = Some(1);
        reset(&mut a);
        assert!(a.status_ms > Some(1), "exiting clears the word");
    }

    #[test]
    fn getting_stuck_is_news_even_outside_a_turn() {
        let mut a = agent();
        apply(&mut a, &report("idle", ""));
        assert!(apply(&mut a, &report("error", "")).stuck, "e.g. it failed while starting");
        assert!(!apply(&mut a, &report("error", "")).stuck, "once");
        assert!(apply(&mut a, &report("blocked", "")).stuck);
        assert!(!apply(&mut a, &report("idle", "")).stuck);
    }

    #[test]
    fn clear_removes_a_record_and_its_children() {
        let mut a = agent();
        for id in ["build", "build/test", "buildx", "deploy"] {
            apply(&mut a, &report("working", id));
        }
        apply(&mut a, &report("clear", "build"));
        assert_eq!(a.records.keys().collect::<Vec<_>>(), ["buildx", "deploy"]);
    }

    #[test]
    fn execution_timing_tracks_working_runs_and_pauses_during_blocked() {
        let mut a = agent();
        apply(&mut a, &report("starting", ""));
        assert_eq!(a.working_since, None);
        assert_eq!(a.work_ms, None);
        assert_eq!(a.total_work_ms, 0);

        apply(&mut a, &report("working", ""));
        assert!(a.working_since.is_some());
        assert_eq!(a.work_ms, Some(0));

        // Simulate 4s of active work before getting blocked for user approval.
        a.working_since = Some(a.working_since.unwrap().saturating_sub(4_000));
        apply(&mut a, &report("blocked", ""));
        assert_eq!(a.working_since, None);
        let paused_ms = a.work_ms.expect("work_ms saved on pause");
        assert!(paused_ms >= 4_000);
        assert_eq!(a.total_work_ms, paused_ms);

        // Resuming from blocked continues the same run without counting blocked wait time.
        apply(&mut a, &report("working", ""));
        let resumed_ws = a.working_since.expect("resumed working_since");
        assert!(resumed_ws <= crate::runtime::now_ms().saturating_sub(paused_ms));
        a.working_since = Some(resumed_ws.saturating_sub(2_000));
        apply(&mut a, &report("done", ""));
        let first_run = a.work_ms.expect("first run finalized");
        assert!(first_run >= paused_ms + 2_000);
        assert_eq!(a.total_work_ms, first_run);
        assert_eq!(a.working_since, None);

        // A second turn starts a fresh work_ms run while accumulating total_work_ms.
        apply(&mut a, &report("working", ""));
        assert_eq!(a.work_ms, Some(0));
        let second_ws = a.working_since.expect("second turn working_since");
        a.working_since = Some(second_ws.saturating_sub(1_500));
        apply(&mut a, &report("idle", ""));
        let second_run = a.work_ms.expect("second run finalized");
        assert!(second_run >= 1_500);
        assert_eq!(a.total_work_ms, first_run + second_run);
        assert_eq!(a.working_since, None);
    }

    #[test]
    fn detects_questions_and_choices_in_assistant_output() {
        assert!(detect_text_needs_user_input("我已经修好了这个 bug，并运行了全部单测，全部通过。").is_none());
        assert!(detect_text_needs_user_input("修改已完成！如果你还有其他问题，随时告诉我。").is_none());
        assert!(detect_text_needs_user_input("已完成所有修改并通过测试！如有任何问题请告诉我。").is_none());
        assert!(detect_text_needs_user_input("All tests pass. Let me know if you need anything else!").is_none());
        assert!(detect_text_needs_user_input("All changes are complete. Please let me know if you'd like any adjustments!").is_none());
        assert!(detect_text_needs_user_input("改完了，还有什么需要帮忙的吗？").is_none());
        assert!(detect_text_needs_user_input("已按你希望的方式完成状态栏优化，请确认效果。").is_none());
        assert!(detect_text_needs_user_input(
            "按照上述方案已完成本次重构：\n1. 修复了多标签页模型选择逻辑\n2. 补充了单元测试并确认全部通过"
        )
        .is_none());
        assert!(detect_text_needs_user_input(
            "为什么之前会超时？原因如下：\n1. 锁未释放\n2. 重试间隔过长\n现已彻底修复并通过测试。"
        )
        .is_none());
        assert!(detect_text_needs_user_input("所有改动已完成并验证通过。需要我顺便提交一下 git commit 吗？").is_none());
        assert!(detect_text_needs_user_input("All tests pass. Would you like me to commit these changes as well?").is_none());
        assert!(detect_text_needs_user_input("修复了如下代码：\n```ts\nconst x = ok ? 1 : 2;\n```\n已验证通过。").is_none());
        assert!(detect_text_needs_user_input("检查了 https://example.com/api?v=1 并更新为 v2。").is_none());

        assert!(detect_text_needs_user_input("请问应该修改全局配置还是仅修改当前项目？").is_some());
        assert!(detect_text_needs_user_input(
            "这里有两种实现方式，你更倾向哪种方案？\n1. 方案 A：仅修改前端\n2. 方案 B：前后端联动修改"
        )
        .is_some());
        assert!(detect_text_needs_user_input("可选方案：\n1. 方案 A：仅改前端\n2. 方案 B：全栈联动").is_some());
        assert!(detect_text_needs_user_input("请选择要部署的目标环境：\n- 方案 A：staging\n- 方案 B：production").is_some());
        assert!(detect_text_needs_user_input(
            "Before I proceed, should I migrate existing database rows or only apply this to new records?"
        )
        .is_some());
        assert!(detect_text_needs_user_input(
            "Which option would you prefer?\n1. Keep legacy API\n2. Break and clean up\n(Recommended: 1)"
        )
        .is_some());
        assert!(detect_text_needs_user_input("Please confirm which branch to target (main or release/2.0).").is_some());
    }

    #[test]
    fn detects_question_tool_call_in_acp_turn() {
        use crate::transcript::Entry;

        let pending_q = vec![Entry::ToolCall {
            turn: 1,
            id: "tc1".into(),
            title: "AskUserQuestion: Choose auth mode".into(),
            tool_kind: Some("ask_question".into()),
            status: "pending".into(),
            detail: None,
            attachments: vec![],
            ts_ms: 1,
        }];
        assert!(detect_acp_turn_needs_user_input(&pending_q, 1).is_some());

        // Completed question tool followed by actual work + final summary is NOT blocked.
        let answered_and_done = vec![
            Entry::ToolCall {
                turn: 1,
                id: "tc1".into(),
                title: "AskUserQuestion".into(),
                tool_kind: Some("ask_question".into()),
                status: "completed".into(),
                detail: None,
                attachments: vec![],
                ts_ms: 1,
            },
            Entry::ToolCall {
                turn: 1,
                id: "tc2".into(),
                title: "edit_file".into(),
                tool_kind: Some("edit".into()),
                status: "completed".into(),
                detail: None,
                attachments: vec![],
                ts_ms: 2,
            },
            Entry::Assistant {
                turn: 1,
                text: "已完成全部修改并通过测试。".into(),
                attachments: vec![],
                ts_ms: 3,
            },
        ];
        assert!(detect_acp_turn_needs_user_input(&answered_and_done, 1).is_none());
    }
}
