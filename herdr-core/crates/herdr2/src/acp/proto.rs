//! ACP (Agent Client Protocol) JSON-RPC 2.0 wire types and resilient parsers.

use crate::permissions::PublicPermissionOption;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FsCapabilities {
    pub read_text_file: bool,
    pub write_text_file: bool,
}

#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct BooleanConfigCapability {}

#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigOptionsCapability {
    pub boolean: BooleanConfigCapability,
}

#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionClientCapabilities {
    pub config_options: SessionConfigOptionsCapability,
}

#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilitiesMeta {
    pub parameterized_model_picker: bool,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    pub fs: FsCapabilities,
    pub terminal: bool,
    pub session: SessionClientCapabilities,
    #[serde(rename = "_meta")]
    pub meta: ClientCapabilitiesMeta,
}

impl ClientCapabilities {
    /// Advertises strictly headless orchestration: no client FS or terminal access.
    pub fn headless() -> Self {
        Self {
            fs: FsCapabilities { read_text_file: false, write_text_file: false },
            terminal: false,
            session: SessionClientCapabilities::default(),
            meta: ClientCapabilitiesMeta { parameterized_model_picker: true },
        }
    }
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct AcpConfigChoice {
    pub value: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

fn default_config_source() -> String {
    "config".into()
}

fn is_default_config_source(s: &str) -> bool {
    s == "config"
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct AcpConfigOption {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// `"select"` or `"boolean"`.
    pub kind: String,
    pub current_value: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<AcpConfigChoice>,
    /// `"config"` (default), `"legacy_model"`, or `"legacy_mode"`.
    #[serde(default = "default_config_source", skip_serializing_if = "is_default_config_source")]
    pub source: String,
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
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
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
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub option_type: Option<String>,
    pub value: Value,
}

/// Parses ACP v1/v2 `configOptions` plus legacy `models` / `modes` blocks into unified descriptors.
pub fn parse_session_config(val: &Value) -> Vec<AcpConfigOption> {
    let mut out = Vec::new();
    if let Some(arr) = val.get("configOptions").and_then(Value::as_array) {
        for item in arr {
            let Some(id) = item
                .get("configId")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let name = item.get("name").and_then(Value::as_str).unwrap_or(id).to_string();
            let description = item.get("description").and_then(Value::as_str).map(String::from);
            let category = item.get("category").and_then(Value::as_str).map(String::from);
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("select").to_string();
            let current_value = item.get("currentValue").cloned().unwrap_or(Value::Null);
            let options = parse_select_choices(item.get("options"));
            out.push(AcpConfigOption {
                id: id.to_string(),
                name,
                description,
                category,
                kind,
                current_value,
                options,
                source: "config".into(),
            });
        }
    }

    let has_model = out.iter().any(|o| o.id == "model" || o.category.as_deref() == Some("model"));
    if !has_model && let Some(models) = val.get("models") {
        let mut choices = Vec::new();
        if let Some(arr) = models.get("availableModels").and_then(Value::as_array) {
            for m in arr {
                if let Some(id) = m.get("modelId").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    choices.push(AcpConfigChoice {
                        value: id.to_string(),
                        name: m.get("name").and_then(Value::as_str).unwrap_or(id).to_string(),
                        description: m.get("description").and_then(Value::as_str).map(String::from),
                        group: None,
                    });
                }
            }
        }
        let current = models.get("currentModelId").cloned().unwrap_or(Value::Null);
        if !choices.is_empty() || !current.is_null() {
            out.push(AcpConfigOption {
                id: "model".into(),
                name: "Model".into(),
                description: None,
                category: Some("model".into()),
                kind: "select".into(),
                current_value: current,
                options: choices,
                source: "legacy_model".into(),
            });
        }
    }

    let has_mode = out.iter().any(|o| o.id == "mode" || o.category.as_deref() == Some("mode"));
    if !has_mode && let Some(modes) = val.get("modes") {
        let mut choices = Vec::new();
        if let Some(arr) = modes.get("availableModes").and_then(Value::as_array) {
            for m in arr {
                if let Some(id) = m.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    choices.push(AcpConfigChoice {
                        value: id.to_string(),
                        name: m.get("name").and_then(Value::as_str).unwrap_or(id).to_string(),
                        description: m.get("description").and_then(Value::as_str).map(String::from),
                        group: None,
                    });
                }
            }
        }
        let current = modes.get("currentModeId").cloned().unwrap_or(Value::Null);
        if !choices.is_empty() || !current.is_null() {
            out.push(AcpConfigOption {
                id: "mode".into(),
                name: "Mode".into(),
                description: None,
                category: Some("mode".into()),
                kind: "select".into(),
                current_value: current,
                options: choices,
                source: "legacy_mode".into(),
            });
        }
    }
    out
}

fn parse_select_choices(raw: Option<&Value>) -> Vec<AcpConfigChoice> {
    let Some(arr) = raw.and_then(Value::as_array) else { return Vec::new() };
    let mut choices = Vec::new();
    for entry in arr {
        if let Some(group_opts) = entry.get("options").and_then(Value::as_array) {
            let group = entry
                .get("name")
                .or_else(|| entry.get("group"))
                .or_else(|| entry.get("groupId"))
                .and_then(Value::as_str)
                .map(String::from);
            for opt in group_opts {
                if let Some(val) = opt.get("value").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    choices.push(AcpConfigChoice {
                        value: val.to_string(),
                        name: opt.get("name").and_then(Value::as_str).unwrap_or(val).to_string(),
                        description: opt.get("description").and_then(Value::as_str).map(String::from),
                        group: group.clone(),
                    });
                }
            }
        } else if let Some(val) = entry.get("value").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            choices.push(AcpConfigChoice {
                value: val.to_string(),
                name: entry.get("name").and_then(Value::as_str).unwrap_or(val).to_string(),
                description: entry.get("description").and_then(Value::as_str).map(String::from),
                group: None,
            });
        }
    }
    choices
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

#[derive(Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AcpContextUsage {
    /// Tokens currently used in the session context window.
    pub used: u64,
    /// Total token capacity of the model's context window.
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedUpdate {
    AssistantChunk(String),
    ThoughtChunk(String),
    UserChunk(String),
    ToolCall { id: String, title: String, kind: Option<String>, status: String, detail: Option<String> },
    Plan(Vec<String>),
    ConfigOptions(Vec<AcpConfigOption>),
    CurrentMode(String),
    Usage(AcpContextUsage),
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
            let detail = extract_tool_detail(update);
            ParsedUpdate::ToolCall { id, title, kind: tool_kind, status, detail }
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
        "config_option_update" => ParsedUpdate::ConfigOptions(parse_session_config(update)),
        "current_mode_update" => update
            .get("currentModeId")
            .and_then(Value::as_str)
            .map_or(ParsedUpdate::Other, |m| ParsedUpdate::CurrentMode(m.to_string())),
        "usage_update" => {
            let used = update
                .get("used")
                .or_else(|| update.get("usedTokens"))
                .or_else(|| update.get("totalTokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let size = update
                .get("size")
                .or_else(|| update.get("contextWindow"))
                .or_else(|| update.get("maxTokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if size > 0 { ParsedUpdate::Usage(AcpContextUsage { used, size }) } else { ParsedUpdate::Other }
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

fn extract_tool_detail(update: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(arr) = update.get("content").and_then(Value::as_array) {
        for item in arr {
            match item.get("type").and_then(Value::as_str) {
                Some("content") => {
                    if let Some(text) = item.pointer("/content/text").and_then(Value::as_str)
                        && !text.trim().is_empty()
                    {
                        parts.push(text.trim().to_string());
                    }
                }
                Some("diff") => {
                    let path = item.get("path").and_then(Value::as_str).unwrap_or("file");
                    let old_t = item.get("oldText").and_then(Value::as_str).unwrap_or("");
                    let new_t = item.get("newText").and_then(Value::as_str).unwrap_or("");
                    let mut diff = format!("--- {path}\n+++ {path}");
                    if !old_t.trim().is_empty() {
                        for line in old_t.lines().take(25) {
                            diff.push_str("\n- ");
                            diff.push_str(line);
                        }
                    }
                    if !new_t.trim().is_empty() {
                        for line in new_t.lines().take(25) {
                            diff.push_str("\n+ ");
                            diff.push_str(line);
                        }
                    }
                    parts.push(diff);
                }
                Some("terminal") => {
                    if let Some(tid) = item.get("terminalId").and_then(Value::as_str) {
                        parts.push(format!("terminal: {tid}"));
                    }
                }
                _ => {
                    if let Some(text) = item.get("text").and_then(Value::as_str)
                        && !text.trim().is_empty()
                    {
                        parts.push(text.trim().to_string());
                    }
                }
            }
        }
    } else if let Some(text) = extract_text(update)
        && !text.trim().is_empty()
    {
        parts.push(text.trim().to_string());
    }

    if parts.is_empty()
        && let Some(raw_out) = update.get("rawOutput")
    {
        match raw_out {
            Value::String(s) if !s.trim().is_empty() => parts.push(s.trim().to_string()),
            Value::Object(obj) => {
                if let Some(s) = obj
                    .get("stdout")
                    .or_else(|| obj.get("output"))
                    .or_else(|| obj.get("text"))
                    .and_then(Value::as_str)
                    && !s.trim().is_empty()
                {
                    parts.push(s.trim().to_string());
                } else if let Some(s) = obj.get("stderr").and_then(Value::as_str)
                    && !s.trim().is_empty()
                {
                    parts.push(s.trim().to_string());
                }
            }
            _ => {}
        }
    }

    if parts.is_empty()
        && let Some(locs) = update.get("locations").and_then(Value::as_array)
    {
        let paths: Vec<String> = locs
            .iter()
            .filter_map(|loc| {
                let p = loc.get("path").and_then(Value::as_str)?.trim();
                if p.is_empty() {
                    return None;
                }
                Some(match loc.get("line").and_then(Value::as_u64) {
                    Some(l) => format!("{p}:{l}"),
                    None => p.to_string(),
                })
            })
            .collect();
        if !paths.is_empty() {
            parts.push(paths.join("\n"));
        }
    }

    if parts.is_empty() {
        None
    } else {
        crate::transcript::cap_tool_detail(&parts.join("\n\n"))
    }
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
        assert_eq!(value["clientCapabilities"]["session"]["configOptions"]["boolean"], json!({}));
        assert_eq!(value["clientCapabilities"]["_meta"]["parameterizedModelPicker"], true);
        assert_eq!(value["clientInfo"]["name"], "herdr2");
    }

    #[test]
    fn parses_session_config_options_grouped_boolean_and_legacy_fallback() {
        let modern = json!({
            "sessionId": "s1",
            "configOptions": [
                {
                    "id": "model",
                    "name": "Model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "gpt-6.1-sol",
                    "options": [{
                        "group": "OpenAI",
                        "options": [{ "value": "gpt-6.1-sol", "name": "6.1 Sol", "description": "Workhorse" }]
                    }]
                },
                {
                    "configId": "reasoning_effort",
                    "name": "Reasoning effort",
                    "category": "thought_level",
                    "type": "select",
                    "currentValue": "high",
                    "options": [
                        { "value": "low", "name": "Low" },
                        { "value": "high", "name": "High" }
                    ]
                },
                {
                    "id": "fast-mode",
                    "name": "Fast mode",
                    "category": "model_config",
                    "type": "boolean",
                    "currentValue": false
                }
            ]
        });
        let parsed = parse_session_config(&modern);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].options[0].group.as_deref(), Some("OpenAI"));
        assert_eq!(parsed[1].id, "reasoning_effort");
        assert_eq!(parsed[2].kind, "boolean");
        assert_eq!(parsed[2].current_value, json!(false));

        let legacy = json!({
            "sessionId": "s2",
            "models": {
                "currentModelId": "m1",
                "availableModels": [{ "modelId": "m1", "name": "Model 1" }]
            },
            "modes": {
                "currentModeId": "agent",
                "availableModes": [{ "id": "agent", "name": "Auto review" }]
            }
        });
        let legacy_parsed = parse_session_config(&legacy);
        assert_eq!(legacy_parsed.len(), 2);
        assert_eq!(legacy_parsed[0].source, "legacy_model");
        assert_eq!(legacy_parsed[0].current_value, json!("m1"));
        assert_eq!(legacy_parsed[1].source, "legacy_mode");
        assert_eq!(legacy_parsed[1].current_value, json!("agent"));

        let upd = json!({
            "sessionId": "s1",
            "update": {
                "sessionUpdate": "config_option_update",
                "configOptions": [{ "id": "fast-mode", "name": "Fast mode", "type": "boolean", "currentValue": true }]
            }
        });
        assert_eq!(
            parse_session_update(&upd),
            Some((
                "s1".into(),
                ParsedUpdate::ConfigOptions(vec![AcpConfigOption {
                    id: "fast-mode".into(),
                    name: "Fast mode".into(),
                    description: None,
                    category: None,
                    kind: "boolean".into(),
                    current_value: json!(true),
                    options: vec![],
                    source: "config".into(),
                }])
            ))
        );
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
    fn parses_usage_update_context_window() {
        let update = json!({
            "sessionId": "s1",
            "update": { "sessionUpdate": "usage_update", "used": 28400, "size": 200000 }
        });
        assert_eq!(
            parse_session_update(&update),
            Some(("s1".into(), ParsedUpdate::Usage(AcpContextUsage { used: 28400, size: 200000 })))
        );
        let zero = json!({
            "sessionId": "s1",
            "update": { "sessionUpdate": "usage_update", "used": 10, "size": 0 }
        });
        assert_eq!(parse_session_update(&zero), Some(("s1".into(), ParsedUpdate::Other)));
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
