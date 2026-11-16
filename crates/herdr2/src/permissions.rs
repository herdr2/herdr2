//! ACP permission policy evaluation (`approve-reads` default) and public approval types.

use clap::ValueEnum;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Serialize, Deserialize, JsonSchema, ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    #[serde(alias = "approve_all")]
    #[value(name = "approve-all", alias = "approve_all")]
    ApproveAll,
    #[default]
    #[serde(alias = "approve_reads")]
    #[value(name = "approve-reads", alias = "approve_reads")]
    ApproveReads,
    #[serde(alias = "deny_all")]
    #[value(name = "deny-all", alias = "deny_all")]
    DenyAll,
    Ask,
}

impl PermissionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApproveAll => "approve-all",
            Self::ApproveReads => "approve-reads",
            Self::DenyAll => "deny-all",
            Self::Ask => "ask",
        }
    }
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PermissionMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "approve-all" | "approve_all" => Ok(Self::ApproveAll),
            "approve-reads" | "approve_reads" => Ok(Self::ApproveReads),
            "deny-all" | "deny_all" => Ok(Self::DenyAll),
            "ask" => Ok(Self::Ask),
            other => Err(format!("unknown permission mode {other:?}; use approve-all, approve-reads, deny-all, or ask")),
        }
    }
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct PublicPermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct PublicPermission {
    pub request_id: String,
    pub session_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub options: Vec<PublicPermissionOption>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    AutoSelect(String),
    AutoCancel,
    AskUser,
}

/// Returns true for read-only ACP tool kinds allowed automatically under `approve-reads`.
pub fn is_read_only_kind(kind: Option<&str>) -> bool {
    let Some(raw) = kind else {
        return false;
    };
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "read" | "search" | "think" | "fetch" | "list" | "glob" | "grep"
    )
}

pub fn decide(mode: PermissionMode, kind: Option<&str>, options: &[PublicPermissionOption]) -> Decision {
    match mode {
        PermissionMode::ApproveAll => pick_approve(options, None).map_or(Decision::AskUser, Decision::AutoSelect),
        PermissionMode::ApproveReads => {
            if is_read_only_kind(kind) {
                pick_approve(options, None).map_or(Decision::AskUser, Decision::AutoSelect)
            } else {
                Decision::AskUser
            }
        }
        PermissionMode::DenyAll => pick_deny(options, None).map_or(Decision::AutoCancel, Decision::AutoSelect),
        PermissionMode::Ask => Decision::AskUser,
    }
}

/// Picks an approval option id: explicit match, else `allow_once`, `allow_always`, or any `allow*` kind.
pub fn pick_approve(options: &[PublicPermissionOption], explicit: Option<&str>) -> Option<String> {
    if let Some(id) = explicit {
        return options.iter().find(|o| o.option_id == id).map(|o| o.option_id.clone());
    }
    pick_by_kind(options, &["allow_once", "allow_always"], "allow")
}

/// Picks a denial option id: explicit match, else `reject_once`, `reject_always`, or any `reject*`/`deny*` kind.
pub fn pick_deny(options: &[PublicPermissionOption], explicit: Option<&str>) -> Option<String> {
    if let Some(id) = explicit {
        return options.iter().find(|o| o.option_id == id).map(|o| o.option_id.clone());
    }
    pick_by_kind(options, &["reject_once", "reject_always", "deny_once", "deny_always"], "reject")
        .or_else(|| pick_by_prefix(options, "deny"))
}

fn pick_by_kind(options: &[PublicPermissionOption], preferred: &[&str], fallback_prefix: &str) -> Option<String> {
    for target in preferred {
        if let Some(found) = options.iter().find(|o| o.kind.eq_ignore_ascii_case(target)) {
            return Some(found.option_id.clone());
        }
    }
    pick_by_prefix(options, fallback_prefix)
}

fn pick_by_prefix(options: &[PublicPermissionOption], prefix: &str) -> Option<String> {
    options
        .iter()
        .find(|o| o.kind.to_ascii_lowercase().starts_with(prefix))
        .map(|o| o.option_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_options() -> Vec<PublicPermissionOption> {
        vec![
            PublicPermissionOption { option_id: "allow-1".into(), name: "Allow once".into(), kind: "allow_once".into() },
            PublicPermissionOption {
                option_id: "allow-all".into(),
                name: "Allow always".into(),
                kind: "allow_always".into(),
            },
            PublicPermissionOption { option_id: "deny-1".into(), name: "Reject".into(), kind: "reject_once".into() },
        ]
    }

    #[test]
    fn approve_reads_allows_read_kinds_and_blocks_mutations() {
        let opts = sample_options();
        for kind in ["read", "Search", "THINK", "fetch", "list", "glob", "grep"] {
            assert_eq!(decide(PermissionMode::ApproveReads, Some(kind), &opts), Decision::AutoSelect("allow-1".into()));
        }
        for kind in [Some("edit"), Some("execute"), Some("delete"), None] {
            assert_eq!(decide(PermissionMode::ApproveReads, kind, &opts), Decision::AskUser);
        }
    }

    #[test]
    fn approve_all_deny_all_and_ask_follow_policy() {
        let opts = sample_options();
        assert_eq!(decide(PermissionMode::ApproveAll, Some("execute"), &opts), Decision::AutoSelect("allow-1".into()));
        assert_eq!(decide(PermissionMode::DenyAll, Some("read"), &opts), Decision::AutoSelect("deny-1".into()));
        assert_eq!(decide(PermissionMode::DenyAll, Some("read"), &[]), Decision::AutoCancel);
        assert_eq!(decide(PermissionMode::Ask, Some("read"), &opts), Decision::AskUser);
    }

    #[test]
    fn explicit_option_picks_exact_match_only() {
        let opts = sample_options();
        assert_eq!(pick_approve(&opts, Some("allow-all")).as_deref(), Some("allow-all"));
        assert_eq!(pick_approve(&opts, Some("missing")), None);
        assert_eq!(pick_deny(&opts, Some("deny-1")).as_deref(), Some("deny-1"));
        assert_eq!(pick_deny(&opts, Some("missing")), None);
    }

    #[test]
    fn permission_mode_parses_kebab_and_snake_case() {
        assert_eq!("approve-reads".parse::<PermissionMode>().unwrap(), PermissionMode::ApproveReads);
        assert_eq!("approve_all".parse::<PermissionMode>().unwrap(), PermissionMode::ApproveAll);
        assert_eq!("deny-all".parse::<PermissionMode>().unwrap(), PermissionMode::DenyAll);
        assert_eq!("ask".parse::<PermissionMode>().unwrap(), PermissionMode::Ask);
        assert!("bogus".parse::<PermissionMode>().is_err());
    }
}
