//! `h2 update`: swap this h2 for another build, restarting core and resuming kept agents.

use super::*;

/// What a binary says it is (`h2 0.2.0`), if it is a working h2.
pub(super) fn version_of(path: &std::path::Path) -> Option<String> {
    let out = crate::platform::bg_command(path).arg("--version").stdin(std::process::Stdio::null()).output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && v.starts_with("h2 ")).then_some(v)
}

/// A release name for `--version`: `0.1.4` (it goes into a URL).
pub(super) fn release_version(s: &str) -> Result<String, String> {
    let parts: Vec<&str> = s.split('.').collect();
    let number = |p: &&str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    if parts.len() == 3 && parts.iter().all(number) {
        Ok(s.into())
    } else {
        Err(format!("expected a release like 0.1.4, got {s:?}"))
    }
}

/// `h2 0.1.9` is older than `h2 0.2.0`; anything unparsed is not older.
/// Tolerates optional build metadata suffixes such as `h2 0.2.0 (a1b2c3d 2026-11-23T08:15:00Z)`.
pub(super) fn older(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Option<Vec<u64>> {
        let semver = v.strip_prefix("h2 ")?.split_whitespace().next()?;
        semver.split(['.', '-']).take(3).map(|p| p.parse().ok()).collect()
    };
    matches!((parts(a), parts(b)), (Some(a), Some(b)) if a < b)
}

/// Strips an optional `h2 ` / `herdr2 ` / `hc ` prefix and build metadata, leaving `X.Y.Z`.
fn strip_version_prefix(v: &str) -> &str {
    let v = v.trim();
    let rest = v.strip_prefix("h2 ").or_else(|| v.strip_prefix("herdr2 ")).or_else(|| v.strip_prefix("hc ")).unwrap_or(v);
    rest.split_whitespace().next().unwrap_or("")
}

fn skip_remote_machine_sync() -> bool {
    std::env::var("H2_UPDATE_NO_MACHINES")
        .ok()
        .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

fn machine_needs_sync(raw_state: &str, current_version: &str, target_version: &str, force: bool) -> bool {
    if force || raw_state == "needs_update" {
        return true;
    }
    let cur = strip_version_prefix(current_version);
    let tgt = strip_version_prefix(target_version);
    if cur.is_empty() {
        return !tgt.is_empty();
    }
    !tgt.is_empty() && older(&format!("h2 {cur}"), &format!("h2 {tgt}"))
}

/// Syncs all configured non-temporary SSH remote machines using the updated local binary `bin`.
/// Each remote is updated independently in parallel so an unreachable host never blocks others.
pub(super) fn sync_remote_machines(
    paths: &Paths,
    bin: &std::path::Path,
    target_version: &str,
    force: bool,
) -> Vec<Value> {
    if skip_remote_machine_sync() {
        return Vec::new();
    }
    let saved: Vec<crate::machines::Machine> = std::fs::read(paths.state.join("machines.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let saved: Vec<_> = saved.into_iter().filter(|m| !m.temporary).collect();
    if saved.is_empty() {
        return Vec::new();
    }

    let live: Vec<Value> = client::connect_existing(paths)
        .and_then(|stream| client::call(&stream, "machine.list", json!({})).ok())
        .and_then(|r| r["machines"].as_array().cloned())
        .map(|arr| arr.into_iter().filter(|m| m["temporary"] != true).collect())
        .unwrap_or_else(|| {
            saved
                .iter()
                .map(|m| {
                    json!({
                        "name": m.name,
                        "target": m.target,
                        "version": m.version,
                        "state": "unreachable",
                        "why": "local core is not running",
                    })
                })
                .collect()
        });

    let target_clean = strip_version_prefix(target_version);
    std::thread::scope(|scope| {
        let handles: Vec<_> = live
            .into_iter()
            .map(|m| {
                scope.spawn(move || {
                    let name = m["name"].as_str().unwrap_or_default().to_owned();
                    let raw_state = m["state"].as_str().unwrap_or("unreachable").to_owned();
                    let cur_ver = strip_version_prefix(m["version"].as_str().unwrap_or_default()).to_owned();
                    let why = m["why"].as_str().map(str::to_owned);
                    let unreachable = matches!(raw_state.as_str(), "unreachable" | "needs_login");

                    if !machine_needs_sync(&raw_state, &cur_ver, target_clean, force) {
                        if unreachable {
                            return json!({
                                "name": name,
                                "ok": false,
                                "updated": false,
                                "state": "needs_update",
                                "raw_state": raw_state,
                                "version": (!cur_ver.is_empty()).then_some(cur_ver),
                                "error": why.unwrap_or_else(|| raw_state.clone()),
                            });
                        }
                        return json!({
                            "name": name,
                            "ok": true,
                            "updated": false,
                            "state": raw_state,
                            "raw_state": raw_state,
                            "version": (!cur_ver.is_empty()).then_some(cur_ver),
                        });
                    }

                    let out = crate::platform::bg_command(bin)
                        .args(["--json", "machine", "update", &name])
                        .stdin(std::process::Stdio::null())
                        .output();
                    match out {
                        Ok(output) if output.status.success() => {
                            let reply: Value = serde_json::from_slice(&output.stdout).unwrap_or_default();
                            let new_ver = reply["version"]
                                .as_str()
                                .map(|v| strip_version_prefix(v).to_owned())
                                .filter(|v| !v.is_empty())
                                .or_else(|| (!target_clean.is_empty()).then(|| target_clean.to_owned()));
                            json!({
                                "name": name,
                                "ok": true,
                                "updated": true,
                                "state": "connected",
                                "raw_state": "connected",
                                "version": new_ver,
                            })
                        }
                        Ok(output) => {
                            let err_json: Value = serde_json::from_slice(&output.stderr).unwrap_or_default();
                            let msg = err_json
                                .pointer("/error/message")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                                .unwrap_or_else(|| {
                                    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                                    if stderr.is_empty() {
                                        String::from_utf8_lossy(&output.stdout).trim().to_owned()
                                    } else {
                                        stderr
                                    }
                                });
                            json!({
                                "name": name,
                                "ok": false,
                                "updated": false,
                                "state": "needs_update",
                                "raw_state": raw_state,
                                "version": (!cur_ver.is_empty()).then_some(cur_ver),
                                "error": if msg.is_empty() { "remote update failed".to_owned() } else { msg },
                            })
                        }
                        Err(e) => json!({
                            "name": name,
                            "ok": false,
                            "updated": false,
                            "state": "needs_update",
                            "raw_state": raw_state,
                            "version": (!cur_ver.is_empty()).then_some(cur_ver),
                            "error": format!("cannot run {}: {e}", bin.display()),
                        }),
                    }
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
}

/// Runs `<bin> skill sync --json` using the updated binary on disk so all already-installed
/// bundled skills are refreshed to the new build's content without installing new agents.
pub(super) fn attach_skill_sync(bin: &std::path::Path, out: &mut Out) {
    let Ok(output) = crate::platform::bg_command(bin)
        .args(["--json", "skill", "sync"])
        .stdin(std::process::Stdio::null())
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let Ok(reply) = serde_json::from_slice::<Value>(&output.stdout) else {
        return;
    };
    let updated_count = reply["updatedCount"].as_u64().unwrap_or(0);
    if updated_count > 0 {
        let targets: Vec<&str> = reply["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["agent"].as_str())
            .collect();
        if targets.is_empty() {
            out.human.push_str(&format!("; synced {updated_count} installed skill(s)"));
        } else {
            out.human.push_str(&format!("; synced {updated_count} installed skill(s) ({})", targets.join(", ")));
        }
    }
    out.json["skills"] = reply;
}

/// Attaches remote SSH machine sync results to `h2 update`'s output.
pub(super) fn attach_machine_sync(paths: &Paths, bin: &std::path::Path, out: &mut Out, force: bool) {
    let target = out.json["now"].as_str().unwrap_or(env!("CARGO_PKG_VERSION"));
    let results = sync_remote_machines(paths, bin, target, force);
    if results.is_empty() {
        return;
    }
    let updated: Vec<&str> = results
        .iter()
        .filter(|r| r["updated"] == true)
        .filter_map(|r| r["name"].as_str())
        .collect();
    let failed: Vec<String> = results
        .iter()
        .filter(|r| r["ok"] == false)
        .map(|r| {
            let name = r["name"].as_str().unwrap_or("?");
            let err = r["error"].as_str().unwrap_or("needs_update");
            format!("{name}: {err}")
        })
        .collect();
    if !updated.is_empty() {
        out.human.push_str(&format!("; synced remote machine(s): {}", updated.join(", ")));
    }
    if !failed.is_empty() {
        out.human.push_str(&format!("; remote needs_update: {}", failed.join("; ")));
    }
    out.json["machines"] = Value::Array(results);
}

#[cfg(test)]
mod tests {
    use super::{machine_needs_sync, older, strip_version_prefix};

    #[test]
    fn older_handles_build_fingerprint_suffixes() {
        assert!(older("h2 0.1.9", "h2 0.2.0 (a1b2c3d+dirty 2026-11-23T08:15:00Z)"));
        assert!(!older("h2 0.2.0 (a1b2c3d 2026-11-23T08:15:00Z)", "h2 0.2.0"));
        assert!(!older("h2 0.2.1 (abc)", "h2 0.2.0 (def)"));
    }

    #[test]
    fn machine_needs_sync_handles_versions_states_and_force() {
        assert_eq!(strip_version_prefix("h2 2.0.11 (abc 2026-11-26)"), "2.0.11");
        assert!(machine_needs_sync("connected", "2.0.10", "2.0.11", false));
        assert!(!machine_needs_sync("connected", "2.0.11", "2.0.11", false));
        assert!(machine_needs_sync("connected", "2.0.11", "2.0.11", true));
        assert!(machine_needs_sync("needs_update", "2.0.11", "2.0.11", false));
        assert!(machine_needs_sync("connected", "", "2.0.11", false));
        assert!(machine_needs_sync("unreachable", "2.0.10", "2.0.11", false));
        assert!(!machine_needs_sync("unreachable", "2.0.11", "2.0.11", false));
    }
}

/// The installed binary can be current while a core started before it was
/// replaced is still running. Do not start a core just to check its version.
pub(super) fn up_to_date(paths: &Paths, release: &str, human: String) -> Result<Out, RpcError> {
    let this = format!("h2 {}", env!("CARGO_PKG_VERSION"));
    let mut out = Out { json: json!({"release": release, "now": this, "updated": false}), human };
    if let Some(stream) = client::connect_existing(paths) {
        let status = client::call(&stream, "server.status", json!({}))?;
        if older(&format!("h2 {}", status["version"].as_str().unwrap_or_default()), &this) {
            client::call(&stream, "server.restart", json!({}))?;
            wait_closed(&stream);
            let status = call(paths, "server.status", json!({}))?;
            out.json["restarted"] = true.into();
            out.json["pid"] = status["pid"].clone();
            out.human.push_str(&format!("; older core restarted (pid {})", status["pid"]));
        }
    }
    Ok(out)
}

/// Swaps the binary this h2 runs from for `from`, then restarts the core cleanly.
pub(super) fn update(paths: &Paths, from: &std::path::Path, force: bool) -> Result<Out, RpcError> {
    let source = crate::platform::update_source(from);
    let from = source.as_path();
    let refused = |why: String, hint: &str| RpcError::new("refused", why, hint);
    let dest = std::env::current_exe().and_then(|e| e.canonicalize()).map_err(|e| RpcError::internal(e.to_string()))?;
    let shown = dest.display().to_string();
    if let Some(manager) = client::installed_by() {
        let how = match manager {
            "homebrew" => "brew upgrade herdr2",
            "nix" => "update it in your Nix configuration",
            "mise" => "mise upgrade herdr2",
            _ => {
                return Err(refused(
                    format!("{} isn't writable by you", dest.parent().unwrap().display()),
                    "update herdr2 the way it was installed (package manager, or sudo)",
                ));
            }
        };
        return Err(refused(format!("{shown} belongs to {manager}"), how));
    }
    let was = format!("h2 {}", env!("CARGO_PKG_VERSION"));
    let now = version_of(from).ok_or_else(|| refused(format!("{} is not a working h2", from.display()), ""))?;
    if older(&now, &was) && !force {
        return Err(refused(format!("{now} is older than this {was}"), "--force to go back anyway"));
    }
    // Staged beside it, checked, then swapped in: never a half-written h2.
    crate::platform::swap_in(from, &dest, |staged| version_of(staged).as_deref() == Some(now.as_str())).map_err(
        |e| match (e.kind(), crate::platform::bundle_update_hint()) {
            (std::io::ErrorKind::InvalidData, _) => refused(format!("the copy of {} does not run", from.display()), ""),
            (std::io::ErrorKind::InvalidInput, Some(hint)) => refused(e.to_string(), hint),
            _ => RpcError::internal(format!("cannot replace {shown}: {e}")),
        },
    )?;
    let restarted = match client::connect_existing(paths) {
        Some(stream) => {
            client::call(&stream, "server.restart", json!({}))?;
            wait_closed(&stream);
            true
        }
        None => false,
    };
    let status = call(paths, "server.status", json!({}))?;
    let human = format!(
        "{shown}: {was} -> {now}; core running again (pid {}){}",
        status["pid"],
        crate::platform::privacy_upgrade_notice(&dest)
    );
    Ok(Out {
        json: json!({"path": shown, "was": was, "now": now, "restarted": restarted, "pid": status["pid"]}),
        human,
    })
}
