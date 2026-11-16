//! Templates: `~/.config/herdr2/templates/<name>.md`, read fresh on every
//! use. Frontmatter says how to start the ACP agent; the body is its first message.

use crate::paths::Paths;
use crate::permissions::PermissionMode;
use crate::rpc::RpcError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    pub name: String,
    pub description: String,
    /// ACP command line or preset override (`command:` or `agent:`).
    pub command: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub permissions: Option<PermissionMode>,
    pub inject_mcp: Option<bool>,
    pub system_prompt: Option<String>,
    pub body: String,
}

#[derive(Serialize, JsonSchema)]
pub struct TemplateInfo {
    pub name: String,
    pub description: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Set when the file can't be used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn dir(paths: &Paths) -> PathBuf {
    paths.config.parent().expect("config file has a dir").join("templates")
}

fn parse(name: &str, text: &str) -> Result<Template, String> {
    let text = text.replace("\r\n", "\n");
    let rest = text.strip_prefix("---\n").ok_or("no frontmatter (start the file with ---)")?;
    let end = rest.find("\n---").ok_or("frontmatter never closes (---)")?;
    let (front, body) = (&rest[..end], &rest[end + 4..]);
    let mut t = Template {
        name: name.into(),
        description: String::new(),
        command: String::new(),
        agent: None,
        model: None,
        mode: None,
        permissions: None,
        inject_mcp: None,
        system_prompt: None,
        body: body.strip_prefix('\n').unwrap_or(body).trim_end().into(),
    };
    for line in front.lines().filter(|l| !l.trim().is_empty()) {
        let (key, value) = line.split_once(':').ok_or_else(|| format!("not `key: value`: {line:?}"))?;
        let value = value.trim().to_string();
        match key.trim() {
            "name" => {}
            "description" => t.description = value,
            "command" => t.command = value,
            "agent" => t.agent = (!value.is_empty()).then_some(value),
            "model" => t.model = (!value.is_empty()).then_some(value),
            "mode" => t.mode = (!value.is_empty()).then_some(value),
            "permissions" => t.permissions = Some(value.parse()?),
            "inject_mcp" => {
                t.inject_mcp = Some(value.parse::<bool>().map_err(|_| "inject_mcp must be true or false")?);
            }
            "system_prompt" => t.system_prompt = (!value.is_empty()).then_some(value),
            _ => {}
        }
    }
    if t.command.is_empty() && t.agent.is_none() {
        return Err("no agent or command".into());
    }
    Ok(t)
}

/// The template called `name`, if there is one.
pub fn load(paths: &Paths, name: &str) -> Result<Option<Template>, RpcError> {
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return Ok(None);
    }
    let path = dir(paths).join(format!("{name}.md"));
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(name, &text)
            .map(Some)
            .map_err(|e| RpcError::new("invalid_template", format!("{}: {e}", path.display()), "see h2 templates")),
        Err(_) => Ok(None),
    }
}

pub fn list(paths: &Paths) -> Vec<TemplateInfo> {
    let mut names: Vec<String> = std::fs::read_dir(dir(paths))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".md").map(String::from))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| match load(paths, &name) {
            Ok(Some(t)) => TemplateInfo {
                name,
                description: t.description,
                command: if t.command.is_empty() { t.agent.clone().unwrap_or_default() } else { t.command },
                agent: t.agent,
                model: t.model,
                error: None,
            },
            Ok(None) => TemplateInfo {
                name,
                description: String::new(),
                command: String::new(),
                agent: None,
                model: None,
                error: Some("unreadable".into()),
            },
            Err(e) => TemplateInfo {
                name,
                description: String::new(),
                command: String::new(),
                agent: None,
                model: None,
                error: Some(e.message),
            },
        })
        .collect()
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone, PartialEq)]
pub struct FirstMessage {
    pub sent: bool,
    /// Whether the agent started working on it (see agent.send).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started: Option<bool>,
    /// Why it was not sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The turn when it went in (see agent.send).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
}

/// The first message: template instructions, then the caller prompt.
pub fn first_message(t: &Template, prompt: &str) -> String {
    [t.body.trim(), prompt.trim()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_frontmatter_and_body_match_lf() {
        let text = "---\nname: reviewer\ndescription: Review code\nagent: codex\nmodel: o3\npermissions: approve-reads\n---\nFirst line.\nSecond line.\n";
        let expected = parse("reviewer", text).unwrap();
        assert_eq!(expected.agent.as_deref(), Some("codex"));
        assert_eq!(expected.model.as_deref(), Some("o3"));
        assert_eq!(expected.permissions, Some(PermissionMode::ApproveReads));
        assert_eq!(expected.body, "First line.\nSecond line.");
        assert_eq!(parse("reviewer", &text.replace('\n', "\r\n")).unwrap(), expected);
        let mixed = text.replacen("---\n", "---\r\n", 1).replace("Second line.\n", "Second line.\r\n");
        assert_eq!(parse("reviewer", &mixed).unwrap(), expected);
    }

    #[test]
    fn frontmatter_and_body_parse_like_the_spec_example() {
        let text = "---\nname: reviewer\ndescription: Read-only code review. Give it file paths and scope.\n\
                    command: pi-acp --fast\n---\n\
                    You are a focused review agent.\nReturn findings.\n";
        let t = parse("reviewer", text).unwrap();
        assert_eq!(t.command, "pi-acp --fast");
        assert_eq!(t.description, "Read-only code review. Give it file paths and scope.");
        assert_eq!(
            first_message(&t, "review src/mcp.rs"),
            "You are a focused review agent.\nReturn findings.\n\nreview src/mcp.rs"
        );
        assert!(parse("x", "no frontmatter").is_err());
        assert!(parse("x", "---\ndescription: d\n---\n").unwrap_err().contains("no agent or command"));
    }
}
