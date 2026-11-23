//! Every method's params and results.

use crate::agents::Public;
use crate::permissions::PermissionMode;
use crate::rpc::RpcError;
use crate::wait::WaitResult;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

#[derive(Serialize, Deserialize, JsonSchema, Clone)]
pub struct CoreInfo {
    /// Permanent random id, reset only when state is wiped.
    pub id: String,
    /// Display name, the hostname by default.
    pub name: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Empty {}

// Request types below are both the API's params (serde, schema) and the CLI's
// arguments (clap): one definition, one doc comment for `--help` and schema.
// `#[arg(skip)]` fields are filled in by h2 itself.

/// A `key=value` label. The CLI takes it repeated (`--label k=v`); JSON takes
/// an object.
#[derive(Clone, Debug, PartialEq)]
pub struct Label(pub String, pub String);

impl std::str::FromStr for Label {
    type Err = String;
    fn from_str(s: &str) -> Result<Label, String> {
        match s.split_once('=') {
            Some((k, v)) if !k.is_empty() => Ok(Label(k.into(), v.into())),
            _ => Err(format!("expected key=value, got {s:?}")),
        }
    }
}

/// Labels as a JSON object of strings (the last of a repeated key wins).
mod label_map {
    use super::Label;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S: Serializer>(labels: &[Label], s: S) -> Result<S::Ok, S::Error> {
        super::labels(labels).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Label>, D::Error> {
        Ok(BTreeMap::<String, String>::deserialize(d)?.into_iter().map(|(k, v)| Label(k, v)).collect())
    }
}

pub fn labels(labels: &[Label]) -> BTreeMap<String, String> {
    labels.iter().map(|Label(k, v)| (k.clone(), v.clone())).collect()
}

/// The word a person or agent types for a value (`idle`, `any`, ...).
pub fn word<T: clap::ValueEnum>(v: &T) -> String {
    v.to_possible_value().map(|p| p.get_name().to_string()).unwrap_or_default()
}

#[derive(Clone, Copy, PartialEq, Debug, clap::ValueEnum, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum IdleOrWorking {
    Idle,
    Working,
}

#[derive(Clone, Copy, PartialEq, Debug, clap::ValueEnum, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Children {
    Any,
    All,
}

/// Seconds on the command line: 0 or more, finite (JSON has no NaN, so a
/// bad value must stop here, not turn into null on the way).
fn cli_seconds(s: &str) -> Result<f64, String> {
    match s.parse::<f64>() {
        Ok(v) if v.is_finite() && v >= 0.0 => Ok(v),
        _ => Err(format!("expected seconds, 0 or more, got {s:?}")),
    }
}

/// Seconds from a request, checked before anything happens.
pub(super) fn seconds(s: Option<f64>) -> Result<Option<Duration>, RpcError> {
    s.map(|s| {
        Duration::try_from_secs_f64(s)
            .map_err(|_| RpcError::invalid_params(format!("timeout must be 0 or more seconds, not {s}")))
    })
    .transpose()
}

#[derive(Deserialize, JsonSchema)]
pub struct HelloParams {
    /// Protocol generation the caller speaks.
    pub generation: Option<u32>,
    /// Switch this connection to framed mode after the reply.
    #[serde(default)]
    pub framed: bool,
    /// Another core dialing in: who it is. Its agents may then be parents
    /// of agents here, and hear from them.
    pub core: Option<CoreInfo>,
    /// What that core calls this one (`rohan`), so news about `a7` reaches it as `rohan/a7`.
    pub calls_you: Option<String>,
    /// Features supported by a core dialing in.
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct HelloResult {
    pub generation: u32,
    pub version: String,
    pub core: CoreInfo,
    pub framed: bool,
    pub methods: Vec<String>,
    pub features: Vec<String>,
    /// OS login inherited by this core. Absent when unavailable or unsupported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_session: Option<crate::platform::LoginSession>,
    /// True: systemd lingering is enabled; false: programs stop at last logout.
    /// Null: unsupported, not applicable or unknown.
    pub keeps_running_after_logout: Option<bool>,
    /// Account for the administrator's loginctl enable-linger command.
    pub logout_linger_user: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct StatusResult {
    pub core: CoreInfo,
    pub version: String,
    pub pid: u32,
    pub uptime_ms: u64,
    pub live: usize,
    pub finished: usize,
    /// The package manager this h2 came from (homebrew, nix, mise, or
    /// system package manager): it updates h2, `h2 update` won't.
    /// Null for a replaceable plain install.
    pub installed_by: Option<String>,
    /// True: systemd lingering is enabled; false: agents stop at last logout.
    /// Null: unsupported, not applicable or unknown. Checked on every request.
    pub keeps_running_after_logout: Option<bool>,
    /// Account for the administrator's loginctl enable-linger command.
    pub logout_linger_user: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocGetParams {
    /// Document name, using the agent name rules.
    pub name: String,
    /// Another machine; omitted for this core.
    pub on: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocSetParams {
    pub name: String,
    /// Any JSON value, at most 1 MiB when serialized; null deletes it.
    pub value: serde_json::Value,
    /// Current version from doc.get; zero for a name never written.
    pub version: u64,
    pub on: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DocGetResult {
    pub value: serde_json::Value,
    /// Zero when never written. Deletions retain their version.
    pub version: u64,
}

#[derive(Serialize, JsonSchema)]
pub struct DocSetResult {
    pub version: u64,
}

#[derive(Deserialize, JsonSchema)]
pub struct RestartParams {
    /// Replacement environment for the next core; requires PATH. Omit to preserve
    /// this core's environment. Normal startup filtering applies; session paths
    /// and service bookkeeping still belong to the next launcher.
    pub environment: Option<BTreeMap<String, String>>,
}

#[derive(Serialize, JsonSchema)]
pub struct StopResult {
    pub stopped: bool,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema, Default)]
pub struct RunParams {
    /// ACP preset name (`claude`, `codex`, `gemini`, ...) or explicit command line.
    #[arg(long, value_name = "AGENT")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Initial prompt queued right after ACP session setup.
    #[arg(long, value_name = "TEXT")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// System prompt appended via `_meta.systemPrompt` on session creation.
    #[arg(long = "system-prompt", value_name = "TEXT")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Model override passed to `session/set_config_option` (`model`).
    #[arg(long, value_name = "MODEL")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning effort override passed to `session/set_config_option` (`reasoning_effort` / `thought_level`).
    #[arg(long, value_name = "EFFORT")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Session mode override passed to `session/set_mode`.
    #[arg(long, value_name = "MODE")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Permission policy (`approve-all`, `approve-reads`, `deny-all`, `ask`, `dangerously-skip-permissions`).
    #[arg(long, value_name = "MODE")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<PermissionMode>,
    /// Skip all permission checks (`approve-all` policy plus `bypassPermissions` session mode when supported).
    #[arg(long = "dangerously-skip-permissions")]
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dangerously_skip_permissions: bool,
    /// Disable injecting the built-in `herdr2` MCP server into `mcpServers`.
    #[arg(long = "no-mcp")]
    #[serde(default)]
    pub no_mcp: bool,
    /// Preset/command and arguments, or a template name and its task.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    #[serde(default)]
    pub argv: Vec<String>,
    /// Name, unique among live agents: 1-32 of a-z 0-9 - _ .
    #[arg(long)]
    pub name: Option<String>,
    /// Working directory (default: where h2 runs; through the API, the core's;
    /// with --on, that machine's home).
    #[arg(long)]
    pub cwd: Option<String>,
    /// Run it on another machine (see `h2 machine ls`); its address is `machine/a7`.
    #[arg(long, value_name = "MACHINE")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    /// Extra environment variables merged onto the core's environment.
    #[arg(skip)]
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Labels, key=value (repeatable).
    #[arg(long = "label", value_name = "K=V")]
    #[serde(default, with = "label_map")]
    #[schemars(with = "BTreeMap<String, String>")]
    pub labels: Vec<Label>,
    /// Parent agent (default: the agent it is started from).
    #[arg(long, conflicts_with = "no_parent")]
    pub parent: Option<String>,
    /// No parent, even when started from an agent.
    #[arg(long)]
    #[serde(default)]
    pub no_parent: bool,
    /// Don't send automated notes about it to its parent.
    #[arg(long)]
    #[serde(default)]
    pub no_notify: bool,
    /// Keep it running when its parent finishes (by default it ends too).
    #[arg(long)]
    #[serde(default)]
    pub outlive_parent: bool,
    /// Ignored: children end with their parent unless `outlive_parent`.
    #[arg(long, hide = true)]
    #[serde(default)]
    pub die_with_parent: bool,
    /// Remove it right after it exits (once a waiting parent has seen it).
    #[arg(long)]
    #[serde(default)]
    pub rm: bool,
    /// Keep recovering via `session/load` (if supported) after crash or restart.
    #[arg(long)]
    #[serde(default)]
    pub keep: bool,
    /// Then wait until it is not working, like `agent.wait`, and return its
    /// transcript (the CLI waits unless --bg).
    #[arg(skip)]
    #[serde(default)]
    pub wait: bool,
    /// Start it and return immediately.
    #[arg(long)]
    #[serde(skip)]
    #[schemars(skip)]
    pub bg: bool,
    /// Return the created `"starting"` agent record immediately and run the ACP handshake in the background.
    #[arg(skip)]
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[schemars(skip)]
    pub async_start: bool,
    /// Seconds before a wait checks in, leaving it running (default: config
    /// `wait_timeout`, else 180).
    #[arg(long, value_parser = cli_seconds)]
    pub timeout: Option<f64>,
    /// Who is calling (`H2_AGENT_ID` inside an agent).
    #[arg(skip)]
    pub caller: Option<String>,
    /// Optional parent lineage attached by `agent.handoff`.
    #[arg(skip)]
    #[serde(skip)]
    #[schemars(skip)]
    pub lineage: Option<crate::handoff::Lineage>,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct AgentParams {
    /// The agent: `a7`, a name, or `machine/a7`.
    #[arg(default_value = "", hide_default_value = true)]
    pub agent: String,
    /// Who is calling; a parent looking at its child has seen its news.
    #[arg(skip)]
    pub caller: Option<String>,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct ListParams {
    /// Include finished agents.
    #[arg(long)]
    #[serde(default)]
    pub all: bool,
    /// Only agents carrying all of these labels (repeatable).
    #[arg(long = "label", value_name = "K=V")]
    #[serde(default, with = "label_map")]
    #[schemars(with = "BTreeMap<String, String>")]
    pub labels: Vec<Label>,
}

#[derive(Serialize, JsonSchema)]
pub struct ListResult {
    /// This machine's agents, then each connected machine's (`machine/a7`).
    #[schemars(with = "Vec<Public>")]
    pub agents: Vec<serde_json::Value>,
    /// Finished agents left out (see `all`).
    pub hidden_finished: usize,
    /// Machines added here and their lines: one that is not connected is
    /// listed here, not waited on.
    pub machines: Vec<crate::remote::MachineView>,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct LabelParams {
    /// The agent: `a7`, a name, or `machine/a7`.
    pub agent: String,
    /// Labels to set, key=value; `key=` removes one.
    #[arg(required = true, value_name = "K=V")]
    #[serde(with = "label_map")]
    #[schemars(with = "BTreeMap<String, String>")]
    pub labels: Vec<Label>,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct KeepParams {
    /// The agent: `a7`, a name, or `machine/a7`.
    pub agent: String,
    /// Keep this live agent recoverable across crash or planned restart.
    #[arg(long = "off", action = clap::ArgAction::SetFalse, default_value_t = true)]
    pub keep: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct RemoveResult {
    pub removed: String,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct ReadParams {
    /// The agent: `a7`, a name, or `machine/a7`.
    #[arg(default_value = "", hide_default_value = true)]
    pub agent: String,
    #[arg(skip)]
    pub caller: Option<String>,
    /// Trailing rendered transcript lines (default: full transcript).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<usize>,
    /// Print only the final assistant text from the latest turn.
    #[arg(long)]
    #[serde(default)]
    pub last: bool,
    /// Wait for the agent to finish its current turn or block before reading.
    #[arg(long)]
    #[serde(default)]
    pub wait: bool,
    /// With `--wait`: seconds before returning early (default: config, else 180).
    #[arg(long, value_parser = cli_seconds)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    /// First turn to include when slicing transcript entries (1-based, inclusive).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_turn: Option<u64>,
    /// Last turn to include when slicing transcript entries (1-based, inclusive).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_turn: Option<u64>,
}

/// Session configuration & context-usage metadata inferred from a transcript.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadSessionMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<crate::acp::proto::AcpContextUsage>,
}

impl ReadSessionMeta {
    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.reasoning_effort.is_none()
            && self.mode.is_none()
            && self.provider.is_none()
            && self.session_id.is_none()
            && self.context_usage.is_none()
    }
}

#[derive(Serialize, JsonSchema)]
pub struct ReadResult {
    pub text: String,
    /// False when older lines were truncated or history is incomplete.
    pub complete: bool,
    /// "transcript", "last", or "history".
    pub how: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// Structured JSONL transcript entries (included for default full transcript reads).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<crate::transcript::Entry>,
    /// Model/mode/context usage metadata inferred from CLI agent session logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_meta: Option<ReadSessionMeta>,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema, Default)]
pub struct SubscribeParams {
    /// Only this agent.
    #[arg(long)]
    pub agent: Option<String>,
    /// Only this terminal.
    #[arg(long)]
    #[serde(default)]
    pub terminal: Option<String>,
    /// Only children of this agent or terminal.
    #[arg(long)]
    pub children_of: Option<String>,
    /// Only records carrying all of these labels (repeatable).
    #[arg(long = "label", value_name = "K=V")]
    #[serde(default, with = "label_map")]
    #[schemars(with = "BTreeMap<String, String>")]
    pub labels: Vec<Label>,
    /// Documents by name (repeatable); a name ending in `*` is a prefix. Naming
    /// narrows, so documents alone hear only them — name an agent or terminal for both.
    #[arg(long = "doc", value_name = "NAME")]
    #[serde(default)]
    pub docs: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct SubscribeResult {
    /// Events after this sequence follow. After a `missed` snapshot, ignore
    /// events whose seq is not above the snapshot's.
    pub seq: u64,
    /// Matching agents here and on connected machines (`machine/a7`).
    #[schemars(with = "Vec<Public>")]
    pub agents: Vec<serde_json::Value>,
    /// Matching terminals here and on connected machines (`machine/t7`).
    #[serde(default)]
    #[schemars(with = "Vec<crate::terminals::Public>")]
    pub terminals: Vec<serde_json::Value>,
    /// Framed mode: the channel events arrive on (JSON lines, with credit).
    /// Plain mode: none; this connection now carries only notifications.
    pub channel: Option<u32>,
}

/// A plain-mode subscription handed to the connection after the reply.
pub struct PlainSub {
    pub rx: tokio::sync::mpsc::Receiver<String>,
    pub behind: Arc<std::sync::atomic::AtomicBool>,
    pub filter: crate::events::Filter,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema, Default)]
pub struct SendParams {
    /// The agent: `a7`, a name, or `machine/a7`.
    pub agent: String,
    /// Prompt text queued for the agent's ACP inbox (may be empty when sending `--image`/`--resource`/`content`).
    #[arg(default_value = "", hide_default_value = true)]
    #[serde(default)]
    pub text: String,
    /// Optional ACP multimodal content blocks (`image`, `audio`, `resource`, `resource_link`, `text`).
    #[arg(skip)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<crate::acp::proto::ContentBlock>,
    /// Attach one or more local image files (PNG/JPEG/WebP/GIF/SVG, <= 8 MB) as native ACP image blocks.
    #[arg(long = "image", value_name = "FILE")]
    #[serde(skip)]
    #[schemars(skip)]
    pub images: Vec<String>,
    /// Attach one or more local files as ACP embedded resource blocks (text or binary <= 8 MB).
    #[arg(long = "resource", value_name = "FILE")]
    #[serde(skip)]
    #[schemars(skip)]
    pub resources: Vec<String>,
    /// Attach one or more URIs or file paths as ACP `resource_link` blocks.
    #[arg(long = "resource-link", value_name = "URI")]
    #[serde(skip)]
    #[schemars(skip)]
    pub resource_links: Vec<String>,
    /// Cancel the active ACP turn (`session/cancel`) and deliver this prompt next.
    #[arg(long)]
    #[serde(default)]
    pub steer: bool,
    /// Only enqueue if the status is exactly this.
    #[arg(long = "if", value_name = "STATUS")]
    #[serde(rename = "if")]
    pub only_if: Option<IdleOrWorking>,
    /// Wait for the turn this send triggers.
    #[arg(long)]
    #[serde(default)]
    pub wait: bool,
    /// With wait: seconds before it checks in (default: config, else 180).
    #[arg(long, value_parser = cli_seconds)]
    pub timeout: Option<f64>,
    /// Sender address attributed in `<h2 from="...">` tags (default: caller).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Who is sending (`H2_AGENT_ID`, else the calling process's parent), so a
    /// bare wait from the same caller waits for this send's turn.
    #[arg(skip)]
    pub caller: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct SendResult {
    /// The agent's status word when queued.
    pub status: Option<String>,
    /// The turn when queued.
    pub turn: u64,
    /// Whether the queued prompt started a turn or went into the serial inbox.
    pub started: Option<bool>,
    /// Total queued items waiting behind the current turn.
    pub inbox_len: usize,
    /// With `wait`: how that turn ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<crate::wait::WaitResult>,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct InboxEditParams {
    /// The agent: `a7`, a name, or `machine/a7`.
    pub agent: String,
    /// Queued inbox item id (`inbox[].id` from `h2 get`).
    pub id: u64,
    /// Replacement prompt text.
    #[arg(allow_hyphen_values = true)]
    pub text: String,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct InboxRemoveParams {
    /// The agent: `a7`, a name, or `machine/a7`.
    pub agent: String,
    /// Queued inbox item id (`inbox[].id` from `h2 get`).
    pub id: u64,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct InboxSendParams {
    /// The agent: `a7`, a name, or `machine/a7`.
    pub agent: String,
    /// Queued inbox item id (`inbox[].id` from `h2 get`).
    pub id: u64,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct WaitParams {
    /// The agent; with `children`, whose children (default: the caller).
    pub agent: Option<String>,
    /// Wait on children instead: all = until none is working, any = next news or a message to you.
    #[arg(long)]
    pub children: Option<Children>,
    /// End when streamed transcript output after the wait starts contains this text.
    /// Blocked, error or exit end it too.
    #[arg(long)]
    pub output: Option<String>,
    /// Seconds before it checks in (default: config `wait_timeout`, else 180).
    #[arg(long, value_parser = cli_seconds)]
    pub timeout: Option<f64>,
    /// Wait indefinitely until status, exit, output, or message (for background runners).
    #[arg(long)]
    #[serde(default)]
    pub no_timeout: bool,
    /// See `agent.send`.
    #[arg(skip)]
    pub caller: Option<String>,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct InterruptParams {
    /// The agent: `a7`, a name, or `machine/a7`.
    #[arg(default_value = "", hide_default_value = true)]
    pub agent: String,
    /// Seconds to wait for cancellation effect (default 10).
    #[arg(long, value_parser = cli_seconds)]
    pub timeout: Option<f64>,
}

#[derive(Serialize, JsonSchema)]
pub struct InterruptResult {
    /// stopped, exited, still_working, or unknown.
    pub outcome: String,
    /// ACP cancellation method (`session/cancel`).
    pub method: String,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct ApproveParams {
    /// The agent: `a7`, a name, or `machine/a7`.
    #[arg(default_value = "", hide_default_value = true)]
    pub agent: String,
    /// Explicit ACP permission `optionId` to choose (default: first matching option).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<String>,
    #[arg(skip)]
    pub caller: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct ApproveResult {
    pub agent: String,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub option_id: Option<String>,
    pub outcome: String,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct ConfigParams {
    /// The agent: `a7`, a name, or `machine/a7`.
    pub agent: String,
    /// Option id to set (e.g. `model`, `reasoning_effort`, `fast-mode`, `mode`).
    #[arg(long = "id", value_name = "ID")]
    pub config_id: String,
    /// String option value (for select options such as model, reasoning effort, or mode).
    #[arg(long, value_name = "VALUE", conflicts_with = "bool_value")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Boolean option value (`true` or `false`, e.g. for `fast-mode`).
    #[arg(long = "bool", value_name = "BOOL", conflicts_with = "value", num_args = 1)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bool_value: Option<bool>,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct PutParams {
    /// The agent: `a7`, a name, or `machine/a7`.
    pub agent: String,
    /// The file's name, kept so `.png` stays `.png` (CLI: the local file to send).
    #[arg(value_name = "FILE")]
    pub name: String,
    /// Its bytes, base64; at most 8 MB raw.
    #[arg(skip)]
    pub data: String,
}

#[derive(Serialize, JsonSchema)]
pub struct PutResult {
    /// Where it landed, as the agent's machine sees it.
    pub path: String,
    pub bytes: usize,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct ExecParams {
    /// Program and arguments, run as given (no shell).
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    /// A shell line instead of argv, run by that machine's shell (`sh -c`;
    /// Windows `%COMSPEC% /d /s /c`). The CLI sends one string with spaces or
    /// shell signs as a line.
    #[arg(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
    /// Run it on another machine (see `h2 machine ls`).
    #[arg(long, value_name = "MACHINE")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    /// Working directory (default: where h2 runs; through the API, the core's;
    /// with --on, that machine's home).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Set on top of the core's environment, key=value (repeatable).
    #[arg(long = "env", value_name = "K=V")]
    #[serde(default, with = "label_map")]
    #[schemars(with = "BTreeMap<String, String>")]
    pub env: Vec<Label>,
    /// Seconds before it and everything it started are ended (default 60, at most 600).
    #[arg(long, value_parser = cli_seconds)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    /// Bytes for its stdin, base64, at most 8 MB raw (without: stdin is closed).
    #[arg(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// Feed what is piped into h2 to its stdin.
    #[arg(short = 'i', long)]
    #[serde(skip)]
    #[schemars(skip)]
    pub stdin: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct ExecResult {
    /// Its exit code; null when a signal ended it.
    pub code: Option<i32>,
    /// The signal that ended it (Unix).
    pub signal: Option<i32>,
    /// What it wrote, base64: the first 8 MB.
    pub stdout: String,
    /// What it wrote, base64: the first 1 MB.
    pub stderr: String,
    /// It wrote more than was kept (it still ran to its end).
    pub truncated: bool,
    /// It ran out of time and was ended: the output is what came so far.
    pub timed_out: bool,
    /// How long it ran, in milliseconds.
    pub ms: u64,
}

/// A file here or on a machine.
#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct FileAt {
    /// The file: absolute, or from `cwd`.
    pub path: String,
    /// On another machine (see `h2 machine ls`).
    #[arg(long, value_name = "MACHINE")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    /// Where a relative path starts (default: where h2 runs; through the API,
    /// the core's; with --on, that machine's home).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct FileReadParams {
    #[command(flatten)]
    #[serde(flatten)]
    pub at: FileAt,
    /// The first line to give, from 1.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
    /// At most this many lines (default: all, up to 8 MB).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Serialize, JsonSchema)]
pub struct FileReadResult {
    /// The lines as they are in the file, line ends kept.
    pub text: String,
    /// The first and last line given (last < first: none).
    pub first: usize,
    pub last: usize,
    /// Lines in the file.
    pub lines: usize,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct FileWriteParams {
    #[command(flatten)]
    #[serde(flatten)]
    pub at: FileAt,
    /// Its new content, base64, at most 8 MB raw (CLI: what is piped in).
    #[arg(skip)]
    pub data: String,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct FileEditParams {
    #[command(flatten)]
    #[serde(flatten)]
    pub at: FileAt,
    /// Replacements, all made or none (CLI: --old/--new pairs).
    #[arg(skip)]
    pub edits: Vec<Edit>,
    /// Exact text found once in the file (repeatable, paired with --new in order).
    #[arg(long, required = true)]
    #[serde(skip)]
    #[schemars(skip)]
    pub old: Vec<String>,
    /// What replaces it.
    #[arg(long, required = true)]
    #[serde(skip)]
    #[schemars(skip)]
    pub new: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct Edit {
    /// Exact text, found exactly once in the file as it was.
    pub old: String,
    pub new: String,
}

#[derive(Serialize, JsonSchema)]
pub struct FileResult {
    /// The file written, absolute.
    pub path: String,
    pub bytes: usize,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct MachineAddParams {
    /// How ssh reaches it: `vps`, `can@10.0.0.7`, `can@10.0.0.7:2222`, a Host from ~/.ssh/config.
    #[arg(required_unless_present = "via", conflicts_with = "via")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Reach it with this command instead of ssh: it is given a shell line to
    /// run there as its last argument, like ssh (`ssh -p 2222 -i key user@ip`,
    /// `fly ssh console -a app --pty=false -C`).
    #[arg(long, value_name = "COMMAND")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// Mark it short-lived (a sandbox for a task), so clients can show it apart
    /// from machines that stay. h2 treats it like any other; remove it with
    /// `h2 machine rm`.
    #[arg(long = "temp")]
    #[serde(default)]
    pub temporary: bool,
    /// Copy this h2 there when it has none or one too old, without asking.
    #[arg(long)]
    #[serde(skip)]
    #[schemars(skip)]
    pub install: bool,
    /// What to call it here (default: the target's host).
    #[arg(long)]
    pub name: Option<String>,
    /// The h2 session on that machine (default: its default).
    #[arg(long)]
    pub session: Option<String>,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct MachineParams {
    /// The machine's name.
    pub machine: String,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct MachineRenameParams {
    /// The machine's name now.
    pub machine: String,
    /// Its new name. Addresses with the old one stop working.
    pub new_name: String,
}

#[derive(Serialize, JsonSchema)]
pub struct MachineListResult {
    pub machines: Vec<crate::remote::MachineView>,
}

#[derive(Serialize, JsonSchema)]
pub struct RunResult {
    #[serde(flatten)]
    pub agent: Public,
    /// Whether the initial prompt went into the agent inbox.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_message: Option<crate::templates::FirstMessage>,
    /// With `wait`: how the wait ended (status, exit, or timeout: still running).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<WaitResult>,
    /// With `wait`: transcript output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct TemplatesResult {
    pub templates: Vec<crate::templates::TemplateInfo>,
}

#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub struct InitialSize {
    /// Initial terminal width in columns (at least 1).
    #[schemars(range(min = 1))]
    pub cols: u16,
    /// Initial terminal height in rows (at least 1).
    #[schemars(range(min = 1))]
    pub rows: u16,
}

#[derive(Serialize, Deserialize, JsonSchema, Default)]
pub struct TerminalRunParams {
    /// Program and arguments, or a template name and its task. With neither
    /// argv nor line: that machine's own shell.
    #[serde(default)]
    pub argv: Vec<String>,
    /// A shell line instead of argv, run by that machine's shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
    /// Initial size before the program starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<InitialSize>,
    /// The caller's colors and cell size, answering the program's display queries from its start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<crate::viewers::Display>,
    /// Name, unique among live agents and terminals: 1-32 of a-z 0-9 - _ .
    #[serde(default)]
    pub name: Option<String>,
    /// Working directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Run it on another machine; its address is `machine/t7`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    /// Override the core's environment and terminal identity.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Labels, key=value.
    #[serde(default, with = "label_map")]
    #[schemars(with = "BTreeMap<String, String>")]
    pub labels: Vec<Label>,
    /// Parent terminal or agent.
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub no_parent: bool,
    #[serde(default)]
    pub no_notify: bool,
    #[serde(default)]
    pub outlive_parent: bool,
    #[serde(default)]
    pub die_with_parent: bool,
    #[serde(default)]
    pub rm: bool,
    #[serde(default)]
    pub keep: bool,
    #[serde(default)]
    pub wait: bool,
    #[serde(default)]
    pub timeout: Option<f64>,
    #[serde(default)]
    pub caller: Option<String>,
    /// Optional parent lineage attached by `agent.handoff`.
    #[serde(skip)]
    #[schemars(skip)]
    pub lineage: Option<crate::handoff::Lineage>,
}

#[derive(Serialize, JsonSchema)]
pub struct TerminalRunResult {
    #[serde(flatten)]
    pub terminal: crate::terminals::Public,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_message: Option<crate::templates::FirstMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<WaitResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TerminalParams {
    /// The terminal: `t7`, a name, or `machine/t7`.
    pub terminal: String,
    #[serde(default)]
    pub caller: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct TerminalListResult {
    /// This machine's terminals, then each connected machine's (`machine/t7`).
    #[schemars(with = "Vec<crate::terminals::Public>")]
    pub terminals: Vec<serde_json::Value>,
    /// Finished terminals left out (see `all`).
    pub hidden_finished: usize,
    pub machines: Vec<crate::remote::MachineView>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TerminalLabelParams {
    pub terminal: String,
    #[serde(with = "label_map")]
    #[schemars(with = "BTreeMap<String, String>")]
    pub labels: Vec<Label>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TerminalKeepParams {
    pub terminal: String,
    pub keep: bool,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TerminalReadParams {
    pub terminal: String,
    #[serde(default)]
    pub caller: Option<String>,
    /// Lines of history above the screen.
    #[serde(default)]
    pub lines: usize,
}

#[derive(Serialize, JsonSchema)]
pub struct TerminalReadResult {
    pub text: String,
    pub complete: bool,
    pub how: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct AttachParams {
    /// The terminal: `t7`, a name, or `machine/t7`.
    #[arg(default_value = "", hide_default_value = true)]
    pub terminal: String,
    /// Watch only: never type or resize.
    #[arg(long)]
    #[serde(default)]
    pub observe: bool,
    /// Observer understands core-to-viewer RESIZE before replacement paints.
    #[arg(skip)]
    #[serde(default)]
    pub resize_notifications: bool,
    /// Scrollback lines to show first (main screen only).
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub history: usize,
    /// The viewer's size in cells.
    #[arg(skip)]
    pub cols: u16,
    #[arg(skip)]
    pub rows: u16,
    /// The viewer's own colors and cell size, used to answer display queries.
    #[arg(skip)]
    #[serde(default)]
    pub display: crate::viewers::Display,
    /// Back after a dropped line: sets the size only once active again.
    #[arg(skip)]
    #[serde(default)]
    pub resumed: bool,
    /// Who is calling (`H2_TERMINAL_ID` inside a terminal): never itself.
    #[arg(skip)]
    #[serde(default)]
    pub caller: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct AttachResult {
    pub channel: u32,
    pub terminal: crate::terminals::Public,
    pub resize_notifications: bool,
    pub automatic_input: bool,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TerminalSendParams {
    pub terminal: String,
    pub text: String,
    #[serde(default)]
    pub force: bool,
    #[serde(rename = "if", default)]
    pub only_if: Option<IdleOrWorking>,
    #[serde(default)]
    pub wait: bool,
    #[serde(default)]
    pub timeout: Option<f64>,
    #[serde(default)]
    pub caller: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct TerminalSendResult {
    pub status: Option<String>,
    pub turn: u64,
    pub started: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<WaitResult>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TerminalWaitParams {
    pub terminal: Option<String>,
    pub children: Option<Children>,
    pub output: Option<String>,
    pub timeout: Option<f64>,
    #[serde(default)]
    pub caller: Option<String>,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema)]
pub struct KeysParams {
    /// The terminal: `t7`, a name, or `machine/t7`.
    pub terminal: String,
    #[arg(skip)]
    #[serde(default)]
    pub caller: Option<String>,
    /// Key names: enter, esc, tab, up, ctrl+c, alt+x, f5, a, ...
    #[arg(required = true)]
    pub keys: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct KeysResult {
    pub status: Option<String>,
    pub turn: u64,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TerminalInterruptParams {
    pub terminal: String,
    #[serde(default)]
    pub timeout: Option<f64>,
}

#[derive(Serialize, JsonSchema)]
pub struct TerminalInterruptResult {
    pub outcome: String,
    pub key: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TerminalPutParams {
    pub terminal: String,
    pub name: String,
    pub data: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct DroveOnParams {
    /// Another machine; omitted for this core.
    pub on: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DroveProbeParams {
    /// Another machine; omitted for this core.
    pub on: Option<String>,
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DroveAiTestParams {
    #[serde(default)]
    pub on: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DroveMagpieTestParams {
    #[serde(default)]
    pub on: Option<String>,
    #[serde(default)]
    pub gateway_url: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DroveWebOpenParams {
    #[serde(default)]
    pub on: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub no_open: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct DroveSubscribeResult {
    pub channel: u32,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CheckpointListParams {
    /// Agent (`a1`), terminal (`t1`), remote (`machine/t1`), or unique name.
    pub target: String,
}

#[derive(Serialize, JsonSchema)]
pub struct CheckpointListResult {
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<crate::checkpoint_safety::WorkspaceIsolation>,
    pub checkpoints: Vec<crate::checkpoint::CheckpointMeta>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CheckpointCaptureParams {
    /// Agent (`a1`), terminal (`t1`), remote (`machine/t1`), or unique name (optional inside managed panes).
    #[serde(default)]
    pub target: Option<String>,
    /// Optional human bookmark label recorded alongside this checkpoint.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct CheckpointCaptureResult {
    pub target: String,
    pub checkpoint: crate::checkpoint::CheckpointMeta,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CheckpointDiffParams {
    /// Agent (`a1`), terminal (`t1`), remote (`machine/t1`), or unique name.
    pub target: String,
    /// Starting checkpoint turn (defaults to highest snapshot strictly below `to`).
    #[serde(default)]
    pub from: Option<u64>,
    /// Ending checkpoint turn (defaults to the latest captured checkpoint).
    #[serde(default)]
    pub to: Option<u64>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CheckpointRollbackParams {
    /// Agent (`a1`), terminal (`t1`), remote (`machine/t1`), or unique name.
    pub target: String,
    /// Target checkpoint turn to roll back to (`0` = baseline before Turn 1).
    pub to: u64,
    /// Restore workspace files back to `to` (false rewinds conversation memory only).
    #[serde(default = "default_true")]
    pub restore_files: bool,
    /// Confirm destructive file restore on non-worktree checkouts or when idle shells share the folder.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CheckpointHookParams {
    pub terminal: String,
    pub source: String,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Serialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointHookResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inject_steps: Vec<serde_json::Value>,
}

#[derive(clap::Args, Serialize, Deserialize, JsonSchema, Default)]
pub struct HandoffParams {
    /// Source agent or terminal (`a1`, `t1`, name, or `machine/a1`).
    pub source: String,
    /// Lineage kind (`handoff`, `fork`, or `side-chat`).
    #[arg(long, value_enum, default_value_t = crate::handoff::LineageKind::Handoff)]
    #[serde(default)]
    pub kind: crate::handoff::LineageKind,
    /// Target preset/program (`claude`, `codex`, `gemini`, `agy`, ...).
    #[arg(long, value_name = "AGENT")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// Launch an interactive PTY terminal (`t*`) target instead of headless ACP (`a*`).
    #[arg(long)]
    #[serde(default)]
    pub pty: bool,
    /// Deliver delta/summary into an existing agent or terminal instead of creating a new one.
    #[arg(long, value_name = "TARGET")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub into: Option<String>,
    /// First turn from the source transcript to include (1-based, inclusive).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_turn: Option<u64>,
    /// Last turn from the source transcript to include (1-based, inclusive).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_turn: Option<u64>,
    /// Include only the most recent N turns from the source transcript (ignored when `from_turn` is set; 0 = all).
    #[arg(long, value_name = "N")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_turns: Option<u64>,
    /// Maximum byte budget for the generated handoff package (defaults to 24 KiB).
    #[arg(long, value_name = "BYTES")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
    /// Optional initial instruction sent after staging handoff context.
    #[arg(long, value_name = "TEXT")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Short user note included in the generated handoff package header.
    #[arg(long, value_name = "TEXT")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Working directory override (defaults to source agent/terminal cwd).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Optional unique name for the newly spawned target.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Model override for newly spawned ACP or PTY agent targets.
    #[arg(long, value_name = "MODEL")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning effort override (`low`, `medium`, `high`, `xhigh`, `max`) for newly spawned targets.
    #[arg(long, value_name = "EFFORT")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Keep newly spawned target alive across restarts (`--keep`).
    #[arg(long)]
    #[serde(default)]
    pub keep: bool,
    /// When `kind == fork`, create an isolated Git worktree on its own branch (carrying uncommitted changes) so the forked session can commit freely.
    #[arg(long)]
    #[serde(default)]
    pub worktree: bool,
    /// When merging back (`--into`), instruct the target/parent agent to merge code from the source session's fork worktree branch into the main workspace without deleting the worktree or branch.
    #[arg(long)]
    #[serde(default)]
    pub merge_worktree: bool,
    /// Skip semantic memory distillation / merge-back takeaway compression and send raw bounded turns.
    #[arg(long)]
    #[serde(default)]
    pub raw: bool,
    /// With `--prompt`: wait for the first turn on the target to finish.
    #[arg(long)]
    #[serde(default)]
    pub wait: bool,
    /// With `--wait`: seconds before returning early (default: config, else 180).
    #[arg(long, value_parser = cli_seconds)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    /// Initial columns when spawning a PTY target (`--pty`).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    /// Initial rows when spawning a PTY target (`--pty`).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
    /// Explicit target command/arguments when spawning with `--pty` (or custom ACP command).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    #[serde(default)]
    pub argv: Vec<String>,
    /// Caller display colors and cell size for newly spawned PTY targets (answers startup OSC 10/11 queries).
    #[arg(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<crate::viewers::Display>,
    /// Caller UI language (e.g. `zh-CN` or `en`) for localized handoff prompts and context headers.
    #[arg(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Who is calling (`H2_AGENT_ID` / `H2_TERMINAL_ID`).
    #[arg(skip)]
    #[serde(default)]
    pub caller: Option<String>,
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Serialize, JsonSchema)]
pub struct HandoffResult {
    pub target: String,
    pub target_kind: String,
    pub kind: crate::handoff::LineageKind,
    pub source: String,
    pub lineage: crate::handoff::Lineage,
    pub included_turns: Vec<u64>,
    pub omitted_turns: Vec<u64>,
    pub bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<String>,
    pub native_fork: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub distilled: bool,
    pub prompt_sent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_branch: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub worktree_merge_requested: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<WaitResult>,
}

fn default_review_side() -> String {
    "new".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReviewComment {
    /// Repository-relative file path (e.g. `src/api.rs`).
    pub path: String,
    /// Diff side: `new`, `old`, or `context`.
    #[serde(default = "default_review_side")]
    pub side: String,
    /// Inclusive 1-based start line number on the indicated side.
    pub start_line: u32,
    /// Inclusive 1-based end line number (defaults to `start_line`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    /// Optional enclosing `@@ ... @@` hunk header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hunk_header: Option<String>,
    /// Optional surrounding unified diff lines (`+`/`-`/` `) for grounding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// The reviewer's comment or requested change.
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReviewParams {
    /// Target agent (`a*`), terminal (`t*`), name, or `machine/addr`.
    pub target: String,
    /// Origin surface (`changes` or `checkpoints`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Optional checkpoint turn number when reviewing a checkpoint diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_turn: Option<u64>,
    /// Optional repository root path for grounding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
    /// Optional overall instruction accompanying the line comments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// One or more line-level review comments.
    pub comments: Vec<ReviewComment>,
    /// Preempt a working ACP agent via `session/cancel`, or force PTY delivery.
    #[serde(default)]
    pub steer: bool,
    /// Caller UI language (`en` or `zh-CN`) for localized headers and prompts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Who is calling (`H2_AGENT_ID` / `H2_TERMINAL_ID`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReviewResult {
    pub target: String,
    /// `agent` or `terminal`.
    pub target_kind: String,
    pub comment_count: usize,
    pub file_count: usize,
    /// Path to staged `diff-review*.md` when staged for PTY or oversized ACP payloads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged_path: Option<String>,
    pub status: Option<String>,
    pub turn: u64,
    pub inbox_len: usize,
}
