//! `h2 skill`: detect installed coding agents and install/update bundled Herdr2 companion skills
//! (`herdr2`, `drove`, `drove-add`, `drove-setup`), optionally using Core AI (`ai.complete`)
//! to discover non-standard agent skill directories and explain installation results.

use super::{Out, call};
use crate::paths::Paths;
use crate::rpc::RpcError;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

const SKILL_HERDR2: &str = include_str!("../../../../skills/herdr2/SKILL.md");
const SKILL_DROVE: &str = include_str!("../../../../../skills/drove/SKILL.md");
const SKILL_DROVE_ADD: &str = include_str!("../../../../../skills/drove-add/SKILL.md");
const SKILL_DROVE_SETUP: &str = include_str!("../../../../../skills/drove-setup/SKILL.md");

#[derive(Clone, Copy)]
pub(crate) struct BundledSkill {
    pub id: &'static str,
    pub content: &'static str,
    pub summary_en: &'static str,
    pub summary_zh: &'static str,
}

pub(crate) const BUNDLED_SKILLS: &[BundledSkill] = &[
    BundledSkill {
        id: "herdr2",
        content: SKILL_HERDR2,
        summary_en: "Core h2 CLI orchestration, ACP sub-agents, permissions, and reuse rules.",
        summary_zh: "核心 h2 CLI 编排、无头 ACP 子智能体、权限审批与复用规则。",
    },
    BundledSkill {
        id: "drove",
        content: SKILL_DROVE,
        summary_en: "Multi-agent DAG planning, role routing, worktree isolation, and review gates.",
        summary_zh: "Drove 多智能体 DAG 编排、角色路由、Worktree 隔离与交叉审查。",
    },
    BundledSkill {
        id: "drove-add",
        content: SKILL_DROVE_ADD,
        summary_en: "Append incremental tasks to an active Drove Run.",
        summary_zh: "向运行中的 Drove Run 动态追加新任务。",
    },
    BundledSkill {
        id: "drove-setup",
        content: SKILL_DROVE_SETUP,
        summary_en: "Configure Drove role candidates, fallback chains, and AI endpoints.",
        summary_zh: "配置 Drove 角色候选链、故障顺延与 AI 端点。",
    },
];

#[derive(Args)]
pub(crate) struct SkillCli {
    #[command(subcommand)]
    pub action: SkillAction,
}

#[derive(Subcommand)]
pub(crate) enum SkillAction {
    /// Inspect detected coding agent skill directories and installed Herdr2 skill versions.
    Status {
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Install or update bundled Herdr2 companion skills (`herdr2`, `drove`, `drove-add`, `drove-setup`).
    Install {
        /// Install into all detected agent skill directories.
        #[arg(long)]
        all: bool,
        /// Only install into agents actually detected on disk/PATH (do not create fallback dirs).
        #[arg(long)]
        detected_only: bool,
        /// Target a specific known agent ID (antigravity, claude, codex, cursor, opencode, windsurf).
        #[arg(long, value_name = "AGENT")]
        agent: Option<String>,
        /// Target a custom skills root directory.
        #[arg(long, value_name = "DIR")]
        target: Option<PathBuf>,
        /// Comma-separated subset of skills to install (default: all bundled skills).
        #[arg(long, value_delimiter = ',')]
        skills: Vec<String>,
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Update all already-installed bundled Herdr2 skills to this h2 build's version.
    Sync {
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
    /// Use built-in Core AI to inspect local or remote coding agents and install Herdr2 skills automatically.
    AiInstall {
        /// Optional hint about a custom agent name or desired skill location.
        #[arg(long, value_name = "TEXT")]
        query: Option<String>,
        /// Preferred explanation language (`zh-CN` or `en`).
        #[arg(long, value_name = "LANG")]
        lang: Option<String>,
        /// Plan only without writing skill files to disk.
        #[arg(long)]
        dry_run: bool,
        /// Target remote machine (`h2 machine ls`).
        #[arg(long, value_name = "MACHINE")]
        on: Option<String>,
    },
}

struct KnownAgentSpec {
    id: &'static str,
    name: &'static str,
    bins: &'static [&'static str],
    marker_rel: &'static [&'static str],
    skills_rel: &'static [&'static str],
}

const KNOWN_AGENTS: &[KnownAgentSpec] = &[
    KnownAgentSpec {
        id: "antigravity",
        name: "Antigravity / Gemini",
        bins: &["agy", "gemini"],
        marker_rel: &[".gemini"],
        skills_rel: &[".gemini", "skills"],
    },
    KnownAgentSpec {
        id: "claude",
        name: "Claude Code",
        bins: &["claude"],
        marker_rel: &[".claude"],
        skills_rel: &[".claude", "skills"],
    },
    KnownAgentSpec {
        id: "codex",
        name: "Codex",
        bins: &["codex"],
        marker_rel: &[".codex"],
        skills_rel: &[".codex", "skills"],
    },
    KnownAgentSpec {
        id: "cursor",
        name: "Cursor",
        bins: &["cursor", "cursor-agent"],
        marker_rel: &[".cursor"],
        skills_rel: &[".cursor", "skills"],
    },
    KnownAgentSpec {
        id: "opencode",
        name: "OpenCode",
        bins: &["opencode"],
        marker_rel: &[".config", "opencode"],
        skills_rel: &[".config", "opencode", "skills"],
    },
    KnownAgentSpec {
        id: "windsurf",
        name: "Windsurf",
        bins: &["windsurf"],
        marker_rel: &[".codeium", "windsurf"],
        skills_rel: &[".codeium", "windsurf", "skills"],
    },
];

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentSkillStatus {
    id: String,
    name: String,
    detected: bool,
    detected_by: Vec<String>,
    skills_dir: String,
    status: String,
    installed_skills: Vec<String>,
    outdated_skills: Vec<String>,
    missing_skills: Vec<String>,
}

fn fingerprint_hex(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn user_home() -> Result<PathBuf, RpcError> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| RpcError::invalid_params("HOME/USERPROFILE is unset; pass --target <DIR>"))
}

fn join_rel(base: &Path, parts: &[&str]) -> PathBuf {
    let mut p = base.to_path_buf();
    for part in parts {
        p.push(part);
    }
    p
}

fn has_command_on_path(bin: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD;.PS1".into())
            .split(';')
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path_var) {
        for ext in &exts {
            let cand = dir.join(format!("{bin}{ext}"));
            if cand.is_file() {
                return true;
            }
        }
    }
    false
}

fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n")
}

fn inspect_skills_dir(dir: &Path) -> (String, Vec<String>, Vec<String>, Vec<String>) {
    let mut installed = Vec::new();
    let mut outdated = Vec::new();
    let mut missing = Vec::new();
    for sk in BUNDLED_SKILLS {
        let p = dir.join(sk.id).join("SKILL.md");
        match std::fs::read_to_string(&p) {
            Ok(raw) => {
                if normalize_newlines(&raw) == normalize_newlines(sk.content) {
                    installed.push(sk.id.to_string());
                } else {
                    outdated.push(sk.id.to_string());
                }
            }
            Err(_) => missing.push(sk.id.to_string()),
        }
    }
    let status = if missing.is_empty() && outdated.is_empty() {
        "up_to_date"
    } else if !outdated.is_empty() && missing.is_empty() {
        "outdated"
    } else if installed.is_empty() && outdated.is_empty() {
        "not_installed"
    } else {
        "partial"
    };
    (status.to_string(), installed, outdated, missing)
}

fn detect_agents_in(home: &Path) -> Vec<AgentSkillStatus> {
    KNOWN_AGENTS
        .iter()
        .map(|spec| {
            let marker = join_rel(home, spec.marker_rel);
            let skills_dir = join_rel(home, spec.skills_rel);
            let mut detected_by = Vec::new();
            if marker.exists() {
                detected_by.push(format!("dir:{}", marker.display()));
            }
            for bin in spec.bins {
                if has_command_on_path(bin) {
                    detected_by.push(format!("cli:{bin}"));
                }
            }
            let (status, installed_skills, outdated_skills, missing_skills) = inspect_skills_dir(&skills_dir);
            AgentSkillStatus {
                id: spec.id.to_string(),
                name: spec.name.to_string(),
                detected: !detected_by.is_empty() || !installed_skills.is_empty() || !outdated_skills.is_empty(),
                detected_by,
                skills_dir: skills_dir.display().to_string(),
                status,
                installed_skills,
                outdated_skills,
                missing_skills,
            }
        })
        .collect()
}

fn select_skills(filter: &[String]) -> Result<Vec<&'static BundledSkill>, RpcError> {
    if filter.is_empty() {
        return Ok(BUNDLED_SKILLS.iter().collect());
    }
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in filter {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        let Some(found) = BUNDLED_SKILLS.iter().find(|s| s.id == name) else {
            let valid: Vec<_> = BUNDLED_SKILLS.iter().map(|s| s.id).collect();
            return Err(RpcError::invalid_params(format!(
                "unknown skill {name:?}; available: {}",
                valid.join(", ")
            )));
        };
        if seen.insert(found.id) {
            out.push(found);
        }
    }
    if out.is_empty() {
        return Ok(BUNDLED_SKILLS.iter().collect());
    }
    Ok(out)
}

const EXTRA_SKILL_DIRS: &[(&str, &[&str])] = &[
    ("agents", &[".agents", "skills"]),
    ("opencode", &[".opencode", "skills"]),
];

fn skill_targets_file(paths: &Paths) -> Option<PathBuf> {
    paths.config.parent().map(|dir| dir.join("skill_targets.json"))
}

fn load_recorded_targets(paths: &Paths) -> Vec<PathBuf> {
    let Some(file) = skill_targets_file(paths) else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(&file) else {
        return Vec::new();
    };
    serde_json::from_slice::<Vec<String>>(&bytes)
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

pub(crate) fn record_installed_target(paths: &Paths, dir: &Path) {
    let Some(file) = skill_targets_file(paths) else {
        return;
    };
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let rendered = canonical.display().to_string();
    if rendered.trim().is_empty() {
        return;
    }
    let mut existing: Vec<String> = std::fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    if existing.iter().any(|p| p == &rendered) {
        return;
    }
    existing.push(rendered);
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(&existing) {
        let _ = crate::paths::write_atomic(&file, &bytes);
    }
}

fn write_skills_to_dir(base: &Path, skills: &[&BundledSkill]) -> Result<Vec<String>, RpcError> {
    let mut written = Vec::new();
    for sk in skills {
        let dir = base.join(sk.id);
        std::fs::create_dir_all(&dir).map_err(|e| RpcError::internal(format!("mkdir {}: {e}", dir.display())))?;
        let path = dir.join("SKILL.md");
        std::fs::write(&path, sk.content).map_err(|e| RpcError::internal(format!("write {}: {e}", path.display())))?;
        written.push(path.display().to_string());
    }
    Ok(written)
}

fn sync_installed_in(paths: &Paths, home: &Path) -> Result<Out, RpcError> {
    let mut candidates: Vec<(String, PathBuf)> = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut push_candidate = |label: String, dir: PathBuf| {
        if !dir.is_dir() {
            return;
        }
        let key = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if seen.insert(key) {
            candidates.push((label, dir));
        }
    };

    for spec in KNOWN_AGENTS {
        push_candidate(spec.id.to_string(), join_rel(home, spec.skills_rel));
    }
    for (label, rel) in EXTRA_SKILL_DIRS {
        push_candidate((*label).to_string(), join_rel(home, rel));
    }
    for raw in load_recorded_targets(paths) {
        let resolved = expand_home_path(&raw.to_string_lossy(), home).unwrap_or(raw);
        push_candidate("custom".to_string(), resolved);
    }

    let mut targets = Vec::new();
    let mut total_updated = 0usize;
    let mut total_up_to_date = 0usize;

    for (label, dir) in &candidates {
        let mut updated_skills = Vec::new();
        let mut files = Vec::new();
        for sk in BUNDLED_SKILLS {
            let path = dir.join(sk.id).join("SKILL.md");
            if !path.is_file() {
                continue;
            }
            let current = std::fs::read_to_string(&path).ok();
            if current
                .as_deref()
                .is_some_and(|raw| normalize_newlines(raw) == normalize_newlines(sk.content))
            {
                total_up_to_date += 1;
                continue;
            }
            std::fs::write(&path, sk.content)
                .map_err(|e| RpcError::internal(format!("write {}: {e}", path.display())))?;
            total_updated += 1;
            updated_skills.push(sk.id.to_string());
            files.push(path.display().to_string());
        }
        if !updated_skills.is_empty() {
            targets.push(json!({
                "agent": label,
                "target": dir.display().to_string(),
                "updatedSkills": updated_skills,
                "files": files,
            }));
        }
    }

    let human = if total_updated == 0 {
        if total_up_to_date == 0 {
            "no installed Herdr2 skills found to update".to_string()
        } else {
            format!("all {total_up_to_date} installed skill file(s) are already up to date")
        }
    } else {
        let list = targets
            .iter()
            .map(|t| {
                let agent = t["agent"].as_str().unwrap_or("custom");
                let target = t["target"].as_str().unwrap_or("");
                format!("{agent} ({target})")
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("updated {total_updated} installed skill file(s) in {list}")
    };

    Ok(Out {
        json: json!({
            "ok": true,
            "updatedCount": total_updated,
            "upToDateCount": total_up_to_date,
            "targets": targets,
            "status": status_out(home).json,
        }),
        human,
    })
}

fn expand_home_path(raw: &str, home: &Path) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "~" {
        return Some(home.to_path_buf());
    }
    if let Some(rest) = trimmed.strip_prefix("~/").or_else(|| trimmed.strip_prefix("~\\")) {
        return Some(home.join(rest));
    }
    Some(PathBuf::from(trimmed))
}

/// Validates that an AI-recommended skills directory stays safely inside `home` with no `..` traversal.
fn validate_ai_target_dir(raw: &str, home: &Path) -> Option<PathBuf> {
    let path = expand_home_path(raw, home)?;
    if !path.is_absolute() {
        return None;
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let home_norm = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    if !path.starts_with(home) && !path.starts_with(&home_norm) {
        return None;
    }
    if path == home || path == home_norm {
        return None;
    }
    Some(path)
}

#[derive(Deserialize)]
struct AiInstallPlan {
    #[serde(default)]
    targets: Vec<AiInstallTarget>,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    tips: Vec<String>,
}

#[derive(Deserialize)]
struct AiInstallTarget {
    #[serde(default)]
    agent: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    reason: String,
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim().strip_suffix("```").unwrap_or(s).trim())
        .unwrap_or(trimmed);
    let start = body.find('{')?;
    let end = body.rfind('}')?;
    (end >= start).then(|| &body[start..=end])
}

fn list_home_dot_hints(home: &Path) -> Vec<String> {
    let mut hints = Vec::new();
    if let Ok(entries) = std::fs::read_dir(home) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.')
                && entry.file_type().is_ok_and(|ft| ft.is_dir())
                && matches!(
                    name.as_str(),
                    ".gemini"
                        | ".claude"
                        | ".codex"
                        | ".cursor"
                        | ".codeium"
                        | ".kiro"
                        | ".hermes"
                        | ".zcode"
                        | ".amp"
                        | ".copilot"
                        | ".qwen"
                        | ".kimi"
                        | ".pi"
                        | ".opencode"
                )
            {
                hints.push(format!("~/{name}"));
            }
        }
    }
    let xdg_cfg = home.join(".config");
    if let Ok(entries) = std::fs::read_dir(&xdg_cfg) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|ft| ft.is_dir())
                && matches!(
                    name.as_str(),
                    "opencode" | "claude" | "codex" | "cursor" | "windsurf" | "kiro" | "hermes" | "amp" | "goose"
                )
            {
                hints.push(format!("~/.config/{name}"));
            }
        }
    }
    hints.sort();
    hints.dedup();
    hints
}

fn status_out(home: &Path) -> Out {
    let agents = detect_agents_in(home);
    let bundled: Vec<Value> = BUNDLED_SKILLS
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "fingerprint": fingerprint_hex(s.content.as_bytes()),
                "summaryEn": s.summary_en,
                "summaryZh": s.summary_zh,
            })
        })
        .collect();
    let detected_count = agents.iter().filter(|a| a.detected).count();
    let installed_count = agents.iter().filter(|a| a.status == "up_to_date").count();
    let human_lines: Vec<String> = agents
        .iter()
        .map(|a| {
            let mark = if a.detected { "*" } else { " " };
            format!("{mark} {:<12} {:<14} {}", a.id, a.status, a.skills_dir)
        })
        .collect();
    Out {
        json: json!({
            "home": home.display().to_string(),
            "bundledSkills": bundled,
            "agents": agents,
            "summary": {
                "detectedAgents": detected_count,
                "upToDateAgents": installed_count,
            }
        }),
        human: format!(
            "Bundled skills: {}\n{}",
            BUNDLED_SKILLS.iter().map(|s| s.id).collect::<Vec<_>>().join(", "),
            human_lines.join("\n")
        ),
    }
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u8 = 0;
    for b in input.bytes() {
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        buf = (buf << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

fn decode_exec_field(res: &Value, key: &str) -> String {
    res.get(key)
        .and_then(Value::as_str)
        .and_then(decode_base64)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

fn sh_quote(arg: &str) -> String {
    if !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b)) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

fn remote_skill_exec(paths: &Paths, machine: &str, args: &[String]) -> Result<Value, RpcError> {
    let quoted: Vec<String> = args.iter().map(|a| sh_quote(a)).collect();
    let line = format!(
        "p=$(command -v h2 || command -v herdr2-core || command -v herdr2 || echo \"$HOME/.local/bin/h2\"); \
         [ -x \"$p\" ] || p=\"$HOME/.local/bin/herdr2\"; \
         \"$p\" skill {} --json",
        quoted.join(" ")
    );
    let r = call(paths, "machine.exec", json!({ "on": machine, "line": line }))?;
    let stdout = decode_exec_field(&r, "stdout");
    let stderr = decode_exec_field(&r, "stderr");
    let code = r
        .get("code")
        .or_else(|| r.get("exit_code"))
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    if code != 0 {
        let parsed_err = serde_json::from_str::<Value>(stderr.trim())
            .or_else(|_| serde_json::from_str::<Value>(stdout.trim()))
            .ok();
        let msg = parsed_err
            .as_ref()
            .and_then(|v| v.pointer("/error/message").and_then(Value::as_str))
            .map(String::from)
            .unwrap_or_else(|| {
                let raw = if !stderr.trim().is_empty() { stderr.trim() } else { stdout.trim() };
                if raw.is_empty() { format!("remote h2 skill failed on {machine} (exit {code})") } else { raw.to_string() }
            });
        return Err(RpcError::new("unavailable", msg, format!("ensure {machine} runs an up-to-date h2 (`h2 machine update {machine}`)")));
    }
    serde_json::from_str::<Value>(stdout.trim()).map_err(|e| {
        RpcError::internal(format!("invalid JSON from remote `h2 skill` on {machine}: {e}"))
    })
}

fn validate_remote_target_dir(raw: &str, remote_home: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains("..") || trimmed.contains('\n') || trimmed.contains('\r') {
        return None;
    }
    let home = remote_home.trim().trim_end_matches(['/', '\\']);
    if home.is_empty() {
        return None;
    }
    let expanded = if let Some(rest) = trimmed.strip_prefix("~/").or_else(|| trimmed.strip_prefix("~\\")) {
        format!("{home}/{rest}")
    } else if trimmed == "~" {
        return None;
    } else {
        trimmed.to_string()
    };
    if expanded == home {
        return None;
    }
    let sep_ok = expanded.starts_with(&format!("{home}/")) || expanded.starts_with(&format!("{home}\\"));
    sep_ok.then_some(expanded)
}

fn run_remote_ai_install(
    paths: &Paths,
    machine: &str,
    query: Option<String>,
    lang: Option<String>,
    dry_run: bool,
) -> Result<Out, RpcError> {
    let zh = lang.as_deref().unwrap_or("zh-CN").to_ascii_lowercase().starts_with("zh");
    let status_val = remote_skill_exec(paths, machine, &["status".into()])?;
    let remote_home = status_val.get("home").and_then(Value::as_str).unwrap_or("").to_string();
    let detected = status_val.get("agents").cloned().unwrap_or_else(|| json!([]));

    let system_prompt = "You are Herdr2's built-in Skill Installation Assistant for a remote SSH machine. \
Given the remote machine's home directory, detected coding agent CLIs/config folders, and optional user request, \
recommend the exact skill root directories on that remote machine (STRICTLY inside the remote user's home directory) where Herdr2's \
bundled skills (`herdr2`, `drove`, `drove-add`, `drove-setup`) should be installed.\n\
Return STRICT JSON only: \
{\"targets\":[{\"agent\":\"claude\",\"path\":\"~/.claude/skills\",\"skills\":[\"herdr2\",\"drove\",\"drove-add\",\"drove-setup\"],\"reason\":\"...\"}],\"summary\":\"...\",\"tips\":[\"...\"]}. \
Never suggest paths outside the remote home directory or containing '..'.";

    let user_payload = json!({
        "machine": machine,
        "remoteHome": remote_home,
        "language": if zh { "zh-CN" } else { "en" },
        "bundledSkills": BUNDLED_SKILLS.iter().map(|s| s.id).collect::<Vec<_>>(),
        "detectedAgents": detected,
        "userHint": query.as_deref().unwrap_or(""),
    });
    let user_prompt = format!(
        "Analyze this remote machine environment and produce the JSON installation plan (write summary & tips in {}):\n{}",
        if zh { "Simplified Chinese" } else { "English" },
        serde_json::to_string_pretty(&user_payload).unwrap_or_default()
    );

    // Use local Core AI (already configured in Welcome) so the remote server needs no API key.
    let ai_res = call(
        paths,
        "ai.complete",
        json!({
            "op": "skill_install",
            "systemPrompt": system_prompt,
            "userPrompt": user_prompt,
            "maxTokens": 1024,
        }),
    );

    let mut used_ai = false;
    let mut ai_model: Option<String> = None;
    let mut ai_error: Option<String> = None;
    let mut plan_summary = String::new();
    let mut plan_tips: Vec<String> = Vec::new();
    let mut targets: Vec<(String, String, Vec<String>, String)> = Vec::new();
    let mut seen = BTreeSet::new();

    match ai_res {
        Ok(val) => {
            ai_model = val.get("model").and_then(Value::as_str).map(String::from);
            let content = val.get("content").and_then(Value::as_str).unwrap_or_default();
            if let Some(json_str) = extract_json_object(content)
                && let Ok(plan) = serde_json::from_str::<AiInstallPlan>(json_str)
            {
                used_ai = true;
                plan_summary = plan.summary;
                plan_tips = plan.tips;
                for t in plan.targets {
                    if let Some(valid_path) = validate_remote_target_dir(&t.path, &remote_home)
                        && seen.insert(valid_path.clone())
                    {
                        let sks: Vec<String> = select_skills(&t.skills)
                            .unwrap_or_else(|_| BUNDLED_SKILLS.iter().collect())
                            .into_iter()
                            .map(|s| s.id.to_string())
                            .collect();
                        let agent_name = if t.agent.trim().is_empty() { "custom".into() } else { t.agent.trim().to_string() };
                        targets.push((agent_name, valid_path, sks, t.reason));
                    }
                }
            } else {
                ai_error = Some("AI returned unstructured output; fell back to remote agent detection.".into());
            }
        }
        Err(err) => {
            ai_error = Some(err.message);
        }
    }

    if targets.is_empty() {
        for a in status_val.get("agents").and_then(Value::as_array).into_iter().flatten() {
            if a.get("detected").and_then(Value::as_bool) == Some(true)
                && let Some(dir) = a.get("skillsDir").and_then(Value::as_str)
                && seen.insert(dir.to_string())
            {
                let id = a.get("id").and_then(Value::as_str).unwrap_or("agent").to_string();
                let name = a.get("name").and_then(Value::as_str).unwrap_or(&id).to_string();
                targets.push((
                    id,
                    dir.to_string(),
                    BUNDLED_SKILLS.iter().map(|s| s.id.to_string()).collect(),
                    if zh { format!("检测到远端已安装 {name}") } else { format!("Detected {name} on {machine}") },
                ));
            }
        }
        if targets.is_empty() && !remote_home.is_empty() {
            targets.push((
                "antigravity".into(),
                format!("{}/.gemini/skills", remote_home.trim_end_matches('/')),
                BUNDLED_SKILLS.iter().map(|s| s.id.to_string()).collect(),
                if zh { "默认远程技能目录".into() } else { "Default remote skills directory".into() },
            ));
        }
    }

    let mut applied = Vec::new();
    let mut total_files = 0usize;
    let mut latest_status = status_val;
    for (agent_name, dir, sks, reason) in &targets {
        if dry_run {
            applied.push(json!({
                "agent": agent_name,
                "target": dir,
                "skills": sks,
                "reason": reason,
                "files": [],
            }));
            continue;
        }
        let mut args = vec!["install".to_string(), "--target".to_string(), dir.clone()];
        if !sks.is_empty() {
            args.push("--skills".to_string());
            args.push(sks.join(","));
        }
        let res = remote_skill_exec(paths, machine, &args)?;
        total_files += res.get("installedCount").and_then(Value::as_u64).unwrap_or(0) as usize;
        if let Some(st) = res.get("status") {
            latest_status = st.clone();
        }
        let files = res
            .pointer("/targets/0/files")
            .cloned()
            .unwrap_or_else(|| json!([]));
        applied.push(json!({
            "agent": agent_name,
            "target": dir,
            "skills": sks,
            "reason": reason,
            "files": files,
        }));
    }

    if plan_summary.is_empty() {
        plan_summary = if zh {
            format!("已在远程主机 {machine} 的 {} 个目录安装 {} 个 Skill 文件。", applied.len(), total_files)
        } else {
            format!("Installed {} skill file(s) across {} target(s) on {machine}.", total_files, applied.len())
        };
    }

    Ok(Out {
        json: json!({
            "ok": true,
            "machine": machine,
            "usedAi": used_ai,
            "model": ai_model,
            "aiError": ai_error,
            "dryRun": dry_run,
            "summary": plan_summary,
            "tips": plan_tips,
            "targets": applied,
            "installedCount": total_files,
            "status": latest_status,
        }),
        human: plan_summary,
    })
}

pub(super) fn run(paths: &Paths, cli: SkillCli) -> Result<Out, RpcError> {
    match cli.action {
        SkillAction::Status { on: Some(machine) } => {
            let val = remote_skill_exec(paths, &machine, &["status".into()])?;
            let detected = val.pointer("/summary/detectedAgents").and_then(Value::as_u64).unwrap_or(0);
            let up_to_date = val.pointer("/summary/upToDateAgents").and_then(Value::as_u64).unwrap_or(0);
            Ok(Out {
                json: val,
                human: format!("{machine}: {up_to_date}/{detected} detected agent(s) up to date"),
            })
        }
        SkillAction::Install { all, detected_only, agent, target, skills, on: Some(machine) } => {
            let mut args = vec!["install".to_string()];
            if all {
                args.push("--all".into());
            }
            if detected_only {
                args.push("--detected-only".into());
            }
            if let Some(a) = agent {
                args.push("--agent".into());
                args.push(a);
            }
            if let Some(t) = target {
                args.push("--target".into());
                args.push(t.display().to_string());
            }
            if !skills.is_empty() {
                args.push("--skills".into());
                args.push(skills.join(","));
            }
            let val = remote_skill_exec(paths, &machine, &args)?;
            let count = val.get("installedCount").and_then(Value::as_u64).unwrap_or(0);
            Ok(Out {
                json: val,
                human: format!("installed {count} skill file(s) on {machine}"),
            })
        }
        SkillAction::Sync { on: Some(machine) } => {
            let val = remote_skill_exec(paths, &machine, &["sync".into()])?;
            let count = val.get("updatedCount").and_then(Value::as_u64).unwrap_or(0);
            let human = if count == 0 {
                format!("installed skills on {machine} are already up to date")
            } else {
                format!("updated {count} installed skill file(s) on {machine}")
            };
            Ok(Out { json: val, human })
        }
        SkillAction::AiInstall { query, lang, dry_run, on: Some(machine) } => {
            run_remote_ai_install(paths, &machine, query, lang, dry_run)
        }
        SkillAction::Status { on: None } => {
            let home = user_home()?;
            Ok(status_out(&home))
        }
        SkillAction::Sync { on: None } => {
            let home = user_home()?;
            sync_installed_in(paths, &home)
        }
        SkillAction::Install { all, detected_only, agent, target, skills, on: None } => {
            let home = user_home()?;
            let selected = select_skills(&skills)?;
            let mut dirs: Vec<(String, PathBuf)> = Vec::new();
            if let Some(dir) = target {
                let resolved = expand_home_path(&dir.to_string_lossy(), &home).unwrap_or(dir);
                dirs.push(("custom".into(), resolved));
            } else if let Some(agent_id) = agent {
                let low = agent_id.trim().to_ascii_lowercase();
                let Some(spec) = KNOWN_AGENTS.iter().find(|a| a.id == low) else {
                    let valid: Vec<_> = KNOWN_AGENTS.iter().map(|a| a.id).collect();
                    return Err(RpcError::invalid_params(format!(
                        "unknown agent {agent_id:?}; known: {} (or use --target <DIR>)",
                        valid.join(", ")
                    )));
                };
                dirs.push((spec.id.to_string(), join_rel(&home, spec.skills_rel)));
            } else {
                let detected = detect_agents_in(&home);
                for a in &detected {
                    if a.detected {
                        dirs.push((a.id.clone(), PathBuf::from(&a.skills_dir)));
                    }
                }
                if dirs.is_empty() && !detected_only {
                    if all {
                        dirs.push(("antigravity".into(), join_rel(&home, &[".gemini", "skills"])));
                        dirs.push(("claude".into(), join_rel(&home, &[".claude", "skills"])));
                    } else {
                        dirs.push(("antigravity".into(), join_rel(&home, &[".gemini", "skills"])));
                    }
                }
            }

            let mut results = Vec::new();
            let mut total_files = 0usize;
            for (label, dir) in &dirs {
                let written = write_skills_to_dir(dir, &selected)?;
                record_installed_target(paths, dir);
                total_files += written.len();
                results.push(json!({
                    "agent": label,
                    "target": dir.display().to_string(),
                    "files": written,
                }));
            }
            let summary = if dirs.is_empty() {
                "no local coding agents detected (skipped fallback due to --detected-only)".to_string()
            } else {
                let list = dirs
                    .iter()
                    .map(|(label, dir)| format!("{label} ({})", dir.display()))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("installed {total_files} skill file(s) into {list}")
            };
            Ok(Out {
                json: json!({
                    "ok": true,
                    "skills": selected.iter().map(|s| s.id).collect::<Vec<_>>(),
                    "installedCount": total_files,
                    "targets": results,
                    "status": status_out(&home).json,
                }),
                human: summary,
            })
        }
        SkillAction::AiInstall { query, lang, dry_run, on: None } => {
            let home = user_home()?;
            let detected = detect_agents_in(&home);
            let dir_hints = list_home_dot_hints(&home);
            let zh = lang.as_deref().unwrap_or("zh-CN").to_ascii_lowercase().starts_with("zh");
            let system_prompt = "You are Herdr2's built-in Skill Installation Assistant. \
Given the user's OS, home directory, detected coding agent CLIs/config folders, and optional user request, \
recommend the exact local skill root directories (STRICTLY inside the user's home directory) where Herdr2's \
bundled skills (`herdr2`, `drove`, `drove-add`, `drove-setup`) should be installed.\n\
Return STRICT JSON only (no markdown fences needed): \
{\"targets\":[{\"agent\":\"claude\",\"path\":\"~/.claude/skills\",\"skills\":[\"herdr2\",\"drove\",\"drove-add\",\"drove-setup\"],\"reason\":\"...\"}],\"summary\":\"...\",\"tips\":[\"...\"]}. \
Never suggest paths outside the user's home directory or containing '..'.";

            let user_payload = json!({
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "home": home.display().to_string(),
                "language": if zh { "zh-CN" } else { "en" },
                "bundledSkills": BUNDLED_SKILLS.iter().map(|s| s.id).collect::<Vec<_>>(),
                "detectedAgents": detected,
                "homeConfigDirs": dir_hints,
                "userHint": query.as_deref().unwrap_or(""),
            });
            let user_prompt = format!(
                "Analyze this local environment and produce the JSON installation plan (write summary & tips in {}):\n{}",
                if zh { "Simplified Chinese" } else { "English" },
                serde_json::to_string_pretty(&user_payload).unwrap_or_default()
            );

            let ai_res = call(
                paths,
                "ai.complete",
                json!({
                    "op": "skill_install",
                    "systemPrompt": system_prompt,
                    "userPrompt": user_prompt,
                    "maxTokens": 1024,
                }),
            );

            let mut used_ai = false;
            let mut ai_model: Option<String> = None;
            let mut ai_error: Option<String> = None;
            let mut plan_summary = String::new();
            let mut plan_tips: Vec<String> = Vec::new();
            let mut resolved_targets: Vec<(String, PathBuf, Vec<&'static BundledSkill>, String)> = Vec::new();
            let mut seen_paths = BTreeSet::new();

            match ai_res {
                Ok(val) => {
                    ai_model = val.get("model").and_then(Value::as_str).map(String::from);
                    let content = val.get("content").and_then(Value::as_str).unwrap_or_default();
                    if let Some(json_str) = extract_json_object(content)
                        && let Ok(plan) = serde_json::from_str::<AiInstallPlan>(json_str)
                    {
                        used_ai = true;
                        plan_summary = plan.summary;
                        plan_tips = plan.tips;
                        for t in plan.targets {
                            if let Some(valid_path) = validate_ai_target_dir(&t.path, &home) {
                                let key = valid_path.display().to_string();
                                if seen_paths.insert(key) {
                                    let chosen = select_skills(&t.skills).unwrap_or_else(|_| BUNDLED_SKILLS.iter().collect());
                                    let agent_name = if t.agent.trim().is_empty() {
                                        "custom".to_string()
                                    } else {
                                        t.agent.trim().to_string()
                                    };
                                    resolved_targets.push((agent_name, valid_path, chosen, t.reason));
                                }
                            }
                        }
                    } else {
                        ai_error = Some("AI returned unstructured output; fell back to local detection.".into());
                    }
                }
                Err(err) => {
                    ai_error = Some(err.message);
                }
            }

            // Always ensure detected agents (or default ~/.gemini/skills) are covered if AI returned empty targets.
            if resolved_targets.is_empty() {
                let fallback_agents: Vec<_> = detected.iter().filter(|a| a.detected).collect();
                if fallback_agents.is_empty() {
                    let dir = join_rel(&home, &[".gemini", "skills"]);
                    resolved_targets.push((
                        "antigravity".into(),
                        dir,
                        BUNDLED_SKILLS.iter().collect(),
                        if zh { "默认技能目录".into() } else { "Default skills directory".into() },
                    ));
                } else {
                    for a in fallback_agents {
                        let dir = PathBuf::from(&a.skills_dir);
                        resolved_targets.push((
                            a.id.clone(),
                            dir,
                            BUNDLED_SKILLS.iter().collect(),
                            if zh {
                                format!("检测到本地已安装 {}", a.name)
                            } else {
                                format!("Detected local {}", a.name)
                            },
                        ));
                    }
                }
            }

            let mut applied = Vec::new();
            let mut total_files = 0usize;
            for (agent_name, dir, sks, reason) in &resolved_targets {
                let files = if dry_run {
                    sks.iter().map(|s| dir.join(s.id).join("SKILL.md").display().to_string()).collect()
                } else {
                    let w = write_skills_to_dir(dir, sks)?;
                    record_installed_target(paths, dir);
                    total_files += w.len();
                    w
                };
                applied.push(json!({
                    "agent": agent_name,
                    "target": dir.display().to_string(),
                    "skills": sks.iter().map(|s| s.id).collect::<Vec<_>>(),
                    "reason": reason,
                    "files": files,
                }));
            }

            if plan_summary.is_empty() {
                plan_summary = if zh {
                    format!(
                        "已为 {} 个目标目录{} {} 个 Herdr2 配套 Skill。",
                        applied.len(),
                        if dry_run { "规划安装" } else { "完成安装" },
                        if dry_run { applied.len() * BUNDLED_SKILLS.len() } else { total_files }
                    )
                } else {
                    format!(
                        "{} {} Herdr2 skill files across {} target(s).",
                        if dry_run { "Planned" } else { "Installed" },
                        if dry_run { applied.len() * BUNDLED_SKILLS.len() } else { total_files },
                        applied.len()
                    )
                };
            }

            Ok(Out {
                json: json!({
                    "ok": true,
                    "usedAi": used_ai,
                    "model": ai_model,
                    "aiError": ai_error,
                    "dryRun": dry_run,
                    "summary": plan_summary,
                    "tips": plan_tips,
                    "targets": applied,
                    "installedCount": total_files,
                    "status": status_out(&home).json,
                }),
                human: plan_summary,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_skills_contain_herdr2_and_drove_suite() {
        let ids: Vec<_> = BUNDLED_SKILLS.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["herdr2", "drove", "drove-add", "drove-setup"]);
        for sk in BUNDLED_SKILLS {
            assert!(sk.content.contains(&format!("name: {}", sk.id)));
        }
    }

    #[test]
    fn validate_ai_target_dir_rejects_traversal_and_outside_home() {
        let home = PathBuf::from(if cfg!(windows) { r"C:\Users\tester" } else { "/home/tester" });
        assert_eq!(validate_ai_target_dir("~/.claude/skills", &home), Some(home.join(".claude/skills")));
        assert_eq!(validate_ai_target_dir("~/.gemini/skills", &home), Some(home.join(".gemini/skills")));
        assert_eq!(validate_ai_target_dir("~/../etc/skills", &home), None);
        assert_eq!(validate_ai_target_dir("/etc/herdr2", &home), None);
        assert_eq!(validate_ai_target_dir("~", &home), None);
        assert_eq!(validate_ai_target_dir("relative/skills", &home), None);
    }

    #[test]
    fn write_and_inspect_skills_detects_up_to_date_and_outdated() {
        let dir = std::env::temp_dir().join(format!("herdr2-skill-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let skills_dir = dir.join(".claude/skills");
        let (st, inst, out, miss) = inspect_skills_dir(&skills_dir);
        assert_eq!(st, "not_installed");
        assert!(inst.is_empty() && out.is_empty() && miss.len() == 4);

        let all: Vec<_> = BUNDLED_SKILLS.iter().collect();
        write_skills_to_dir(&skills_dir, &all).unwrap();
        let (st, inst, out, miss) = inspect_skills_dir(&skills_dir);
        assert_eq!(st, "up_to_date");
        assert_eq!(inst.len(), 4);
        assert!(out.is_empty() && miss.is_empty());

        std::fs::write(skills_dir.join("herdr2/SKILL.md"), "stale content").unwrap();
        let (st, inst, out, miss) = inspect_skills_dir(&skills_dir);
        assert_eq!(st, "outdated");
        assert_eq!(inst.len(), 3);
        assert_eq!(out, vec!["herdr2"]);
        assert!(miss.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_json_object_handles_fenced_and_prose_wrapped_json() {
        let fenced = "```json\n{\"targets\":[],\"summary\":\"ok\"}\n```";
        assert_eq!(extract_json_object(fenced), Some("{\"targets\":[],\"summary\":\"ok\"}"));
        let prose = "Here is the plan:\n{\"targets\":[],\"summary\":\"done\"}\nThanks!";
        assert_eq!(extract_json_object(prose), Some("{\"targets\":[],\"summary\":\"done\"}"));
    }

    #[test]
    fn validate_remote_target_dir_enforces_remote_home_boundary() {
        assert_eq!(
            validate_remote_target_dir("~/.claude/skills", "/home/dev"),
            Some("/home/dev/.claude/skills".into())
        );
        assert_eq!(
            validate_remote_target_dir("/home/dev/.gemini/skills", "/home/dev/"),
            Some("/home/dev/.gemini/skills".into())
        );
        assert_eq!(
            validate_remote_target_dir(r"~\.codex\skills", r"C:\Users\dev\"),
            Some(r"C:\Users\dev/.codex\skills".into())
        );
        assert_eq!(
            validate_remote_target_dir(r"C:\Users\dev\.claude\skills", r"C:\Users\dev"),
            Some(r"C:\Users\dev\.claude\skills".into())
        );
        // Reject sibling prefix collision, traversal, home itself, empty home, and newline injection.
        assert_eq!(validate_remote_target_dir("/home/dev2/.claude/skills", "/home/dev"), None);
        assert_eq!(validate_remote_target_dir("/home/dev", "/home/dev"), None);
        assert_eq!(validate_remote_target_dir("~/../etc/skills", "/home/dev"), None);
        assert_eq!(validate_remote_target_dir("/etc/skills", "/home/dev"), None);
        assert_eq!(validate_remote_target_dir("~", "/home/dev"), None);
        assert_eq!(validate_remote_target_dir("~/.claude/skills", "   "), None);
        assert_eq!(validate_remote_target_dir("~/.claude/skills\nrm -rf /", "/home/dev"), None);
    }

    #[test]
    fn sh_quote_escapes_unsafe_arguments_and_keeps_safe_tokens_plain() {
        assert_eq!(sh_quote("--detected-only"), "--detected-only");
        assert_eq!(sh_quote("/home/dev/.claude/skills"), "/home/dev/.claude/skills");
        assert_eq!(sh_quote("herdr2,drove"), "herdr2,drove");
        assert_eq!(sh_quote(""), "''");
        assert_eq!(sh_quote("/home/my user/skills"), "'/home/my user/skills'");
        assert_eq!(sh_quote("a'b;rm -rf /"), "'a'\\''b;rm -rf /'");
    }

    #[test]
    fn decode_base64_and_exec_field_handle_valid_whitespace_and_invalid_inputs() {
        assert_eq!(decode_base64("aGVsbG8=").as_deref(), Some(b"hello".as_slice()));
        assert_eq!(decode_base64("aGVs\nbG8=\r\n").as_deref(), Some(b"hello".as_slice()));
        assert_eq!(decode_base64(""), Some(Vec::new()));
        assert_eq!(decode_base64("!!!not-b64!!!"), None);

        let res = json!({
            "stdout": "eyJvayI6dHJ1ZX0=",
            "stderr": "bad!base64",
        });
        assert_eq!(decode_exec_field(&res, "stdout"), "{\"ok\":true}");
        assert_eq!(decode_exec_field(&res, "stderr"), "");
        assert_eq!(decode_exec_field(&res, "missing"), "");
    }

    #[test]
    fn inspect_skills_dir_reports_partial_and_normalizes_crlf() {
        let dir = std::env::temp_dir().join(format!("herdr2-skill-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let skills_dir = dir.join("skills");

        // Partial installation (only 2 of 4 skills, with CRLF line endings on one).
        let subset = select_skills(&["herdr2".into(), "drove".into(), "herdr2".into()]).unwrap();
        assert_eq!(subset.len(), 2);
        write_skills_to_dir(&skills_dir, &subset).unwrap();
        let crlf = SKILL_HERDR2.replace('\n', "\r\n");
        std::fs::write(skills_dir.join("herdr2/SKILL.md"), crlf).unwrap();

        let (st, inst, out, miss) = inspect_skills_dir(&skills_dir);
        assert_eq!(st, "partial");
        assert_eq!(inst, vec!["herdr2", "drove"]);
        assert!(out.is_empty());
        assert_eq!(miss, vec!["drove-add", "drove-setup"]);

        // Unknown skill filter fails cleanly.
        assert!(select_skills(&["nonexistent".into()]).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
