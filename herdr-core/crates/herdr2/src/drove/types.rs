//! Shared domain models and wire contracts for Drove (matching `drove-web` TypeScript types).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Current UTC timestamp formatted as RFC 3339 / ISO-8601 (`YYYY-MM-DDTHH:MM:SS.sssZ`).
pub fn now_iso() -> String {
    let dur = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format_ms_iso(dur.as_millis() as u64)
}

/// Current local wall-clock time formatted for AI routing context
/// (`YYYY-MM-DDTHH:MM:SS+HH:MM (Weekday)`), so time-window notes such as
/// "2:00 PM - 6:00 PM" can be evaluated accurately in the user's local timezone.
pub fn now_local_time_context() -> String {
    const WDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    #[cfg(unix)]
    {
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { !libc::localtime_r(&secs, &mut tm).is_null() } {
            let year = tm.tm_year + 1900;
            let mon = tm.tm_mon + 1;
            let day = tm.tm_mday;
            let hour = tm.tm_hour;
            let min = tm.tm_min;
            let sec = tm.tm_sec;
            let wday = WDAYS.get(tm.tm_wday as usize).copied().unwrap_or("?");
            let off_secs = tm.tm_gmtoff as i64;
            let sign = if off_secs < 0 { '-' } else { '+' };
            let abs_min = off_secs.unsigned_abs() / 60;
            let oh = abs_min / 60;
            let om = abs_min % 60;
            return format!("{year:04}-{mon:02}-{day:02}T{hour:02}:{min:02}:{sec:02}{sign}{oh:02}:{om:02} ({wday})");
        }
    }
    #[cfg(windows)]
    {
        #[repr(C)]
        #[derive(Default)]
        struct SystemTimeWin {
            year: u16,
            month: u16,
            day_of_week: u16,
            day: u16,
            hour: u16,
            minute: u16,
            second: u16,
            milliseconds: u16,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetLocalTime(lp: *mut SystemTimeWin);
        }
        let mut st = SystemTimeWin::default();
        unsafe { GetLocalTime(&mut st) };
        let wday = WDAYS.get(st.day_of_week as usize).copied().unwrap_or("?");
        let local_as_utc_iso = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z",
            st.year, st.month, st.day, st.hour, st.minute, st.second
        );
        let utc_secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        if let Some(local_ms) = parse_iso_ms(&local_as_utc_iso) {
            let diff_min = ((local_ms as i64 / 1000) - utc_secs + 30).div_euclid(60);
            let sign = if diff_min < 0 { '-' } else { '+' };
            let abs_min = diff_min.unsigned_abs();
            let oh = abs_min / 60;
            let om = abs_min % 60;
            return format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{oh:02}:{om:02} ({wday})",
                st.year, st.month, st.day, st.hour, st.minute, st.second
            );
        }
        return format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02} ({wday})",
            st.year, st.month, st.day, st.hour, st.minute, st.second
        );
    }
    #[cfg(not(windows))]
    now_iso()
}

/// Formats epoch milliseconds into ISO-8601 UTC string.
pub fn format_ms_iso(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let millis = ms % 1000;
    let days = secs.div_euclid(86_400);
    let sec_of_day = secs.rem_euclid(86_400);
    let hour = sec_of_day / 3600;
    let min = (sec_of_day % 3600) / 60;
    let sec = sec_of_day % 60;

    // Howard Hinnant civil_from_days algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}.{millis:03}Z")
}

/// Parses ISO-8601 `YYYY-MM-DDTHH:MM:SS[.sss](Z|+HH:MM|-HH:MM)` timestamp into UTC epoch milliseconds.
pub fn parse_iso_ms(iso: &str) -> Option<u64> {
    let trimmed = iso.trim();
    let (date, time_and_tz) = trimmed.split_once('T').or_else(|| trimmed.split_once('t'))?;
    let (time, tz_offset_secs) =
        if let Some(t) = time_and_tz.strip_suffix('Z').or_else(|| time_and_tz.strip_suffix('z')) {
            (t, 0_i64)
        } else if let Some(pos) = time_and_tz.rfind(['+', '-']) {
            let (t, off_str) = time_and_tz.split_at(pos);
            let sign = if off_str.starts_with('-') { -1_i64 } else { 1_i64 };
            let digits: String = off_str[1..].chars().filter(|c| c.is_ascii_digit()).collect();
            let (hh, mm) = match digits.len() {
                2 => (digits.parse::<i64>().ok()?, 0_i64),
                4 => (digits[..2].parse::<i64>().ok()?, digits[2..4].parse::<i64>().ok()?),
                _ => return None,
            };
            (t, sign * (hh * 3600 + mm * 60))
        } else {
            return None;
        };
    let mut dp = date.split('-');
    let year: i64 = dp.next()?.parse().ok()?;
    let month: u64 = dp.next()?.parse().ok()?;
    let day: u64 = dp.next()?.parse().ok()?;
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut tp = hms.split(':');
    let hour: u64 = tp.next()?.parse().ok()?;
    let min: u64 = tp.next()?.parse().ok()?;
    let sec: u64 = tp.next()?.parse().ok()?;
    let millis_str = format!("{frac:0<3}");
    let millis: u64 = millis_str.get(..3)?.parse().ok()?;

    // days_from_civil
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m_adj + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + (doe as i64) - 719_468;
    let total_secs = days * 86_400 + (hour * 3600 + min * 60 + sec) as i64 - tz_offset_secs;
    (total_secs >= 0).then_some(total_secs as u64 * 1000 + millis)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Default for Effort {
    fn default() -> Self {
        Self::Medium
    }
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    Ready,
    Running,
    Succeeded,
    Failed,
    Blocked,
    Cancelled,
    Skipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WorkerState {
    Starting,
    Idle,
    Busy,
    Dead,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Succeeded,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    AgentError,
    NoDone,
    Timeout,
    Schema,
    Cancelled,
    MergeConflict,
    ProcessExited,
    WorkerReported,
    QuotaExhausted,
    VerificationFailed,
    ReviewRejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Question,
    Escalation,
    Failed,
    Done,
    Budget,
    RunDone,
    Note,
    Reply,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TriggerRule {
    AllSuccess,
    AnySuccess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TaskPhase {
    Author,
    Verifying,
    Review,
    Rework,
    Integrating,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Active,
    Paused,
    Done,
    Archived,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    None,
    Worktree,
}

impl Default for Isolation {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleAccessMode {
    Readonly,
    ScopedWrite,
    Writable,
}

impl Default for RoleAccessMode {
    fn default() -> Self {
        Self::Writable
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum VerifySpec {
    Cmd(String),
    Obj(VerifySpecObj),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VerifySpecObj {
    pub cmd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rejections: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_stagnant_rejections: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoalPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_repair: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunFinalization {
    pub state: String,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_tree_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    #[serde(default)]
    pub repairs_used: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_artifact_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parse_json: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ForeachFrom {
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub different_agent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewVerdict {
    pub verdict: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings: Option<Vec<ReviewFinding>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewState {
    pub round: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_tree_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reviewed_tree_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_tree_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_tree_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings: Option<Vec<ReviewFinding>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompletionLease {
    pub completion_id: String,
    pub payload_hash: String,
    pub generation: u64,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttemptReview {
    pub round: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_tree_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_patch_artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings: Option<Vec<ReviewFinding>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Spec {
    pub target: String,
    pub change: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraints: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<String>,
    pub acceptance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<VerifySpec>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Assign {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoordinatorInfo {
    pub host: String,
    pub host_session: String,
    pub generation: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_bound: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiUsage {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_hit_tokens: u64,
    pub cost: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub objective: String,
    pub cwd: String,
    pub coordinator: CoordinatorInfo,
    pub max_concurrency: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Budget>,
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<VerifySpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalization: Option<RunFinalization>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trashed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_before_trash: Option<RunStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_usage: Option<AiUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    #[serde(flatten)]
    pub run: Run,
    pub task_counts: BTreeMap<String, u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskWhen {
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "in")]
    pub in_values: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskResult {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_modified: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blockers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_file_hashes: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub run_id: String,
    pub key: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub spec: Spec,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    #[serde(default)]
    pub isolation: Isolation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_warn_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_kill_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<TaskWhen>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assign: Option<Assign>,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResult>,
    #[serde(default)]
    pub failures: u32,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_phase: Option<TaskPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_rule: Option<TriggerRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<ExecSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreach_from: Option<ForeachFrom>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_state: Option<ReviewState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_result: Option<TaskResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_from: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UsageEstimate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttemptFailure {
    pub kind: FailureKind,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttemptAiDiagnosis {
    pub at: String,
    pub based_on_updated_at: String,
    pub root_cause: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    pub recommended_action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Attempt {
    pub id: String,
    pub task_id: String,
    pub run_id: String,
    pub worker_id: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<bool>,
    pub route_reason: String,
    pub reused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_late_done: Option<String>,
    #[serde(default)]
    pub tokens: TokenCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated: Option<UsageEstimate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<AttemptFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_round: Option<AttemptReview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifying: Option<CompletionLease>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_rejections: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stagnant_rejections: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verify_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tree_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_log_artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_ai_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_diagnosis: Option<AttemptAiDiagnosis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spinning_detected_at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprint {
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub task_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkerContext {
    pub used: u64,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkerTotals {
    pub tokens: u64,
    pub cost: f64,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated: Option<UsageEstimate>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Worker {
    pub id: String,
    pub run_id: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<bool>,
    pub session_key: String,
    /// Native `herdr2` ACP agent address (`a1`, `a2`, ...) owning this worker session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h2_agent: Option<String>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    pub state: WorkerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default)]
    pub fingerprint: Fingerprint,
    #[serde(default)]
    pub context: WorkerContext,
    #[serde(default)]
    pub totals: WorkerTotals,
    pub created_at: String,
    pub last_active_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_gen: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_dir: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MessageAiDraft {
    pub answer: String,
    pub confidence: f64,
    pub category: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_replied: Option<bool>,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub seq: u64,
    pub run_id: String,
    #[serde(rename = "type")]
    pub kind: MessageType,
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raised_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raised_by_worker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_draft: Option<MessageAiDraft>,
    pub needs_action: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handled_at: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemTool {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: u64,
    pub worker_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    pub ts: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ItemTool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ItemUsage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DaemonEventTokens {
    pub input: u64,
    pub output: u64,
    pub cache_hit: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DaemonEvent {
    pub id: u64,
    pub ts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub category: String,
    pub kind: String,
    pub status: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<DaemonEventTokens>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StallSignal {
    pub kind: String,
    pub summary: String,
    pub since: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_ids: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Diagnosis {
    pub task: Task,
    #[serde(default)]
    pub attempts: Vec<Attempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<Attempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<AttemptFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_reason: Option<String>,
    #[serde(default)]
    pub last_items: Vec<Item>,
    #[serde(default)]
    pub files_modified: Vec<String>,
    pub git_status: String,
    pub suggestion: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<AttemptAiDiagnosis>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunStats {
    pub tokens: u64,
    pub cost: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_cost: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunSnapshot {
    pub run: Run,
    pub tasks: Vec<Task>,
    pub workers: Vec<Worker>,
    pub attempts: Vec<Attempt>,
    pub pending_messages: Vec<Message>,
    pub messages: Vec<Message>,
    pub stats: RunStats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stalls: Option<Vec<StallSignal>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatsEntry {
    pub n: u32,
    pub success: u32,
    pub avg_tokens: f64,
    pub avg_cost: f64,
    pub avg_duration_ms: f64,
    pub retry_rate: f64,
    pub no_done_rate: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuotaWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlimited: Option<bool>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    #[serde(default)]
    pub remaining: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuotaCard {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<Vec<QuotaWindow>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, alias = "readAt", skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChangeEvent {
    pub table: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

// --- Roles-only configuration, minimal AI URL+Key, and Optional Magpie ---

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoleTarget {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Effort,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Work types / task specialties this agent+model excels at (used by router & coordinator).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strengths: Vec<String>,
    /// Optional usage/routing note for this agent+model candidate (read by AI when selecting/launching sub-agents).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoleConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<RoleAccessMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_paths: Vec<String>,
    pub primary: RoleTarget,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<RoleTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_ai_effort")]
    pub effort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default = "default_true")]
    pub auto_title: bool,
    #[serde(default = "default_true")]
    pub verify_digest: bool,
    #[serde(default = "default_true")]
    pub handover_compact: bool,
    #[serde(default = "default_true")]
    pub task_enrich: bool,
    #[serde(default = "default_question_mode")]
    pub question_mode: String,
    #[serde(default = "default_conflict_policy")]
    pub conflict_policy: String,
    #[serde(default = "default_loop_detect")]
    pub loop_detect: String,
}

fn default_true() -> bool {
    true
}
fn default_ai_effort() -> String {
    "low".into()
}
fn default_question_mode() -> String {
    "draft".into()
}
fn default_conflict_policy() -> String {
    "warn".into()
}
fn default_loop_detect() -> String {
    "off".into()
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            base_url: String::new(),
            model: String::new(),
            effort: default_ai_effort(),
            context_window: None,
            max_tokens: None,
            language: None,
            auto_title: true,
            verify_digest: true,
            handover_compact: true,
            task_enrich: true,
            question_mode: default_question_mode(),
            conflict_policy: default_conflict_policy(),
            loop_detect: default_loop_detect(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MagpieSettings {
    /// Disabled by default: when false, never probe localhost:3425 and never show quota cards.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DroveConfig {
    #[serde(default = "default_config_version")]
    pub version: u32,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default = "default_concurrency")]
    pub max_concurrency_default: u32,
    /// Optional global routing/scheduling note for Role -> Agent + Model selection
    /// (read by AI routers and coordinators when choosing or launching sub-agents).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub roles: BTreeMap<String, RoleConfig>,
    #[serde(default)]
    pub ai: AiSettings,
    #[serde(default)]
    pub magpie: MagpieSettings,
}

fn default_config_version() -> u32 {
    1
}
fn default_concurrency() -> u32 {
    4
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProbedAgent {
    pub id: String,
    pub command: Vec<String>,
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_info: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default)]
    pub efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_contexts: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_efforts: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProbeSnapshot {
    pub probed_at: String,
    #[serde(default)]
    pub full_probe: bool,
    pub agents: Vec<ProbedAgent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiSwitches {
    pub verify_digest: bool,
    pub handover_compact: bool,
    pub auto_title: bool,
    pub task_enrich: bool,
    pub conflict_policy: String,
    pub question_mode: String,
    pub loop_detect: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DaemonAiStatus {
    pub enabled: bool,
    pub configured: bool,
    pub key_source: String,
    pub key_file_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_masked: Option<String>,
    pub base_url: String,
    pub model: String,
    pub effort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default)]
    pub available_models: Vec<String>,
    pub circuit_open: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until: Option<String>,
    pub consecutive_failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub switches: AiSwitches,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DroveAiPatch {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub context_window: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub auto_title: Option<bool>,
    #[serde(default)]
    pub verify_digest: Option<bool>,
    #[serde(default)]
    pub handover_compact: Option<bool>,
    #[serde(default)]
    pub task_enrich: Option<bool>,
    #[serde(default)]
    pub question_mode: Option<String>,
    #[serde(default)]
    pub conflict_policy: Option<String>,
    #[serde(default)]
    pub loop_detect: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_key: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DroveMagpiePatch {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub gateway_url: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DroveSettingsSetParams {
    #[serde(default)]
    pub on: Option<String>,
    #[serde(default)]
    pub config: Option<DroveConfig>,
    #[serde(default)]
    pub max_concurrency_default: Option<u32>,
    /// Optional global routing/scheduling note update (empty string clears the note).
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub roles: Option<BTreeMap<String, RoleConfig>>,
    #[serde(default)]
    pub ai: Option<DroveAiPatch>,
    #[serde(default)]
    pub magpie: Option<DroveMagpiePatch>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_ai_key: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DroveSettingsResult {
    pub machine: String,
    pub web_port: u16,
    pub config_path: String,
    pub config: DroveConfig,
    pub ai_status: DaemonAiStatus,
    pub magpie_gateway_resolved: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magpie_quotas: Option<Vec<QuotaCard>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magpie_error: Option<String>,
    pub agents: ProbeSnapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DroveWebOpenResult {
    pub url: String,
    pub opened: bool,
    pub via_peer: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DroveRolesAiConfigureParams {
    #[serde(default)]
    pub on: Option<String>,
    /// Configuration scope: `"models"` (agents/models/effort only), `"strengths"` (candidate task strengths only),
    /// `"notes"` (global and/or per-candidate routing notes only), or `"all"` (default: models + strengths + notes).
    #[serde(default)]
    pub mode: Option<String>,
    /// Optional target scope when `mode == "notes"`: `"global"`, `"candidate"`, or `"all"` (default).
    #[serde(default)]
    pub note_scope: Option<String>,
    /// Optional role ID when `noteScope == "candidate"`.
    #[serde(default)]
    pub target_role: Option<String>,
    /// Optional 0-based candidate index inside `targetRole` when `noteScope == "candidate"`.
    #[serde(default)]
    pub target_candidate_index: Option<usize>,
    /// Optional natural-language preference hint (e.g. preferred agents/models, cost/speed priority, or draft note to refine).
    #[serde(default)]
    pub instruction: Option<String>,
    /// Optional unsaved AI Assistant endpoint override.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Optional unsaved AI Assistant API key override.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Optional unsaved AI Assistant model override.
    #[serde(default)]
    pub model: Option<String>,
    /// Persist generated roles/note immediately (defaults to true).
    #[serde(default)]
    pub apply: Option<bool>,
    /// Preferred output language ("zh-CN" | "en") for strengths tags, notes, and rationale.
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DroveRolesAiConfigureResult {
    pub ok: bool,
    pub applied: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub probed_refreshed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_used: Option<String>,
    pub latency_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// Generated routing note (global note for `all`/`notes`/`global`, or single candidate note for `candidate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<BTreeMap<String, RoleConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<DroveSettingsResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
