//! ACP (Agent Client Protocol) JSON-RPC 2.0 wire types and resilient parsers.

use crate::permissions::PublicPermissionOption;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FsCapabilities {
    pub read_text_file: bool,
    pub write_text_file: bool,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    pub fs: FsCapabilities,
    pub terminal: bool,
}

impl ClientCapabilities {
    /// Advertises strictly headless orchestration: no client FS or terminal access.
    pub fn headless() -> Self {
        Self { fs: FsCapabilities { read_text_file: false, write_text_file: false }, terminal: false }
    }
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: u16,
    pub client_capabilities: ClientCapabilities,
    pub client_info: ClientInfo,
}

impl InitializeParams {
    pub fn herdr2() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            client_capabilities: ClientCapabilities::headless(),
            client_info: ClientInfo { name: "herdr2", version: env!("CARGO_PKG_VERSION") },
        }
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default)]
    pub load_session: bool,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

impl AgentInfo {
    pub fn display_label(&self) -> Option<String> {
        let base = self.title.as_deref().filter(|s| !s.is_empty()).unwrap_or(&self.name);
        if base.is_empty() {
            return None;
        }
        Some(match self.version.as_deref().filter(|s| !s.is_empty()) {
            Some(v) => format!("{base} {v}"),
            None => base.into(),
        })
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    #[serde(default)]
    pub protocol_version: Option<u16>,
    #[serde(default)]
    pub agent_capabilities: Option<AgentCapabilities>,
    #[serde(default)]
    pub agent_info: Option<AgentInfo>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StdioMcpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<EnvVar>,
}

impl StdioMcpServer {
    /// Serializes with `"type": "stdio"` (modern ACP schema) or untagged fallback for strict v1 adapters.
    pub fn to_value(&self, tagged: bool) -> Value {
        if tagged {
            json!({
                "type": "stdio",
                "name": self.name,
                "command": self.command,
                "args": self.args,
                "env": self.env,
            })
        } else {
            json!({
                "name": self.name,
                "command": self.command,
                "args": self.args,
                "env": self.env,
            })
        }
    }
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionNewParams {
    pub cwd: String,
    pub mcp_servers: Vec<Value>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionNewResult {
    pub session_id: String,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionLoadParams {
    pub session_id: String,
    pub cwd: String,
    pub mcp_servers: Vec<Value>,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionSetModeParams {
    pub session_id: String,
    pub mode_id: String,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionSetConfigOptionParams {
    pub session_id: String,
    pub config_id: String,
    pub value: Value,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionSetModelParams {
    pub session_id: String,
    pub model_id: String,
}

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentBlock {
    Text { text: String },
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionPromptParams {
    pub session_id: String,
    pub prompt: Vec<ContentBlock>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionPromptResult {
    #[serde(default)]
    pub stop_reason: String,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionCancelParams {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedUpdate {
    AssistantChunk(String),
    ThoughtChunk(String),
    UserChunk(String),
    ToolCall { id: String, title: String, kind: Option<String>, status: String },
    Plan(Vec<String>),
    Other,
}

pub fn parse_session_update(params: &Value) -> Option<(String, ParsedUpdate)> {
    let session_id = params.get("sessionId").and_then(Value::as_str)?.to_string();
    let update = params.get("update")?;
    let kind = update.get("sessionUpdate").and_then(Value::as_str).unwrap_or_default();
    let parsed = match kind {
        "agent_message_chunk" => extract_text(update).map_or(ParsedUpdate::Other, ParsedUpdate::AssistantChunk),
        "agent_thought_chunk" => extract_text(update).map_or(ParsedUpdate::Other, ParsedUpdate::ThoughtChunk),
        "user_message_chunk" => extract_text(update).map_or(ParsedUpdate::Other, ParsedUpdate::UserChunk),
        "tool_call" | "tool_call_update" => {
            let id = update.get("toolCallId").and_then(Value::as_str).unwrap_or_default().to_string();
            let title = update.get("title").and_then(Value::as_str).unwrap_or_default().to_string();
            let tool_kind = update.get("kind").and_then(Value::as_str).map(String::from);
            let status = update
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or(if kind == "tool_call" { "pending" } else { "" })
                .to_string();
            ParsedUpdate::ToolCall { id, title, kind: tool_kind, status }
        }
        "plan" => {
            let items = update
                .get("entries")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            item.get("content")
                                .or_else(|| item.get("title"))
                                .and_then(Value::as_str)
                                .map(String::from)
                        })
                        .collect()
                })
                .unwrap_or_default();
            ParsedUpdate::Plan(items)
        }
        _ => ParsedUpdate::Other,
    };
    Some((session_id, parsed))
}

fn extract_text(update: &Value) -> Option<String> {
    let content = update.get("content")?;
    if let Some(text) = content.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    if let Some(arr) = content.as_array() {
        let joined: String = arr.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect();
        if !joined.is_empty() {
            return Some(joined);
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPermission {
    pub session_id: String,
    pub title: String,
    pub kind: Option<String>,
    pub options: Vec<PublicPermissionOption>,
}

/// Parses ACP v1 (`toolCall.{title,kind}`) and v2 (`title`/`description`/`kind`) permission requests.
pub fn parse_request_permission(params: &Value) -> Option<ParsedPermission> {
    let session_id = params.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_string();
    let tc = params.get("toolCall");
    let title = params
        .get("title")
        .and_then(Value::as_str)
        .or_else(|| tc.and_then(|t| t.get("title").and_then(Value::as_str)))
        .or_else(|| params.get("description").and_then(Value::as_str))
        .unwrap_or("Permission requested")
        .to_string();
    let kind = tc
        .and_then(|t| t.get("kind").and_then(Value::as_str))
        .or_else(|| params.get("kind").and_then(Value::as_str))
        .map(String::from);
    let options = params
        .get("options")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|o| {
                    Some(PublicPermissionOption {
                        option_id: o.get("optionId").and_then(Value::as_str)?.to_string(),
                        name: o.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                        kind: o.get("kind").and_then(Value::as_str).unwrap_or_default().to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(ParsedPermission { session_id, title, kind, options })
}

pub fn permission_selected(option_id: &str) -> Value {
    json!({ "outcome": { "outcome": "selected", "optionId": option_id } })
}

pub fn permission_cancelled() -> Value {
    json!({ "outcome": { "outcome": "cancelled" } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_params_explicitly_disable_terminal_and_filesystem() {
        let value = serde_json::to_value(InitializeParams::herdr2()).unwrap();
        assert_eq!(value["protocolVersion"], 1);
        assert_eq!(value["clientCapabilities"]["terminal"], false);
        assert_eq!(value["clientCapabilities"]["fs"]["readTextFile"], false);
        assert_eq!(value["clientCapabilities"]["fs"]["writeTextFile"], false);
        assert_eq!(value["clientInfo"]["name"], "herdr2");
    }

    #[test]
    fn parses_v1_and_v2_permission_requests_and_updates() {
        let v1 = json!({
            "sessionId": "s1",
            "toolCall": { "toolCallId": "tc1", "title": "Edit main.rs", "kind": "edit" },
            "options": [{ "optionId": "allow_once", "name": "Allow", "kind": "allow_once" }]
        });
        let parsed = parse_request_permission(&v1).unwrap();
        assert_eq!(parsed.title, "Edit main.rs");
        assert_eq!(parsed.kind.as_deref(), Some("edit"));
        assert_eq!(parsed.options[0].option_id, "allow_once");

        let v2 = json!({
            "sessionId": "s2",
            "title": "Read config",
            "kind": "read",
            "options": [{ "optionId": "ok", "name": "OK", "kind": "allow_once" }]
        });
        let parsed_v2 = parse_request_permission(&v2).unwrap();
        assert_eq!(parsed_v2.title, "Read config");
        assert_eq!(parsed_v2.kind.as_deref(), Some("read"));

        let update = json!({
            "sessionId": "s1",
            "update": { "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "hi" } }
        });
        assert_eq!(parse_session_update(&update), Some(("s1".into(), ParsedUpdate::AssistantChunk("hi".into()))));
    }

    #[test]
    fn mcp_server_serializes_tagged_and_untagged_forms() {
        let srv = StdioMcpServer {
            name: "herdr2".into(),
            command: "/usr/local/bin/herdr2".into(),
            args: vec!["mcp".into()],
            env: vec![EnvVar { name: "H2_AGENT_ID".into(), value: "a1".into() }],
        };
        assert_eq!(srv.to_value(true)["type"], "stdio");
        assert!(srv.to_value(false).get("type").is_none());
    }
}
