// The installable skill is the repo's stub; it points agents to `h2 --skill`.
#[test]
fn the_installable_stub_is_a_valid_skill() {
    let stub = include_str!("../../../skills/herdr2/SKILL.md");
    let mut lines = stub.lines();
    assert_eq!(lines.next(), Some("---"));
    assert_eq!(lines.next(), Some("name: herdr2"));
    let description = lines.next().unwrap().strip_prefix("description: ").unwrap();
    let description: String = serde_json::from_str(description).expect("quoted YAML description");
    assert!(!description.is_empty() && description.chars().count() < 1024);
    assert_eq!(lines.next(), Some("---"));
    assert!(stub.contains("h2 --skill"), "the stub points to the manual");
}

#[cfg(unix)]
mod common;

#[cfg(unix)]
mod skill_cli {
    use super::common::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn isolated_path_env() -> (String, String) {
        // Minimal PATH without coding agent CLIs so detection depends solely on marker dirs in HOME.
        ("PATH".into(), "/usr/bin:/bin".into())
    }

    #[test]
    fn local_skill_status_install_detected_only_subset_and_target() {
        let mut s = Session::new();
        s.env.push(isolated_path_env());

        // 1. Clean HOME: no agents detected.
        let st0 = s.json(&["skill", "status"]);
        assert_eq!(st0["summary"]["detectedAgents"], 0, "st0: {st0}");
        assert_eq!(st0["summary"]["upToDateAgents"], 0);

        // 2. --all --detected-only with no detected agents creates NO fallback directories.
        let skipped = s.json(&["skill", "install", "--all", "--detected-only"]);
        assert_eq!(skipped["ok"], true);
        assert_eq!(skipped["installedCount"], 0);
        assert_eq!(skipped["targets"].as_array().unwrap().len(), 0);
        assert!(!s.root.join(".gemini/skills").exists());
        assert!(!s.root.join(".claude/skills").exists());

        // 3. Create .claude marker dir -> --all --detected-only installs only into .claude/skills.
        std::fs::create_dir_all(s.root.join(".claude")).unwrap();
        let inst = s.json(&["skill", "install", "--all", "--detected-only"]);
        assert_eq!(inst["ok"], true);
        assert_eq!(inst["installedCount"], 4);
        assert_eq!(inst["targets"].as_array().unwrap().len(), 1);
        assert_eq!(inst["targets"][0]["agent"], "claude");
        assert!(s.root.join(".claude/skills/herdr2/SKILL.md").exists());
        assert!(s.root.join(".claude/skills/drove/SKILL.md").exists());
        assert!(s.root.join(".claude/skills/drove-add/SKILL.md").exists());
        assert!(s.root.join(".claude/skills/drove-setup/SKILL.md").exists());
        assert!(!s.root.join(".gemini/skills").exists());
        assert_eq!(inst["status"]["summary"]["detectedAgents"], 1);
        assert_eq!(inst["status"]["summary"]["upToDateAgents"], 1);

        // 4. Custom --target with --skills subset writes only the requested skills.
        let custom_dir = s.root.join("custom-skills");
        let sub = s.json(&[
            "skill",
            "install",
            "--target",
            custom_dir.to_str().unwrap(),
            "--skills",
            "herdr2,drove",
        ]);
        assert_eq!(sub["installedCount"], 2);
        assert!(custom_dir.join("herdr2/SKILL.md").exists());
        assert!(custom_dir.join("drove/SKILL.md").exists());
        assert!(!custom_dir.join("drove-add/SKILL.md").exists());

        // 5. Unknown skill ID is rejected cleanly.
        let (_, bad) = s.err(&["--json", "skill", "install", "--target", custom_dir.to_str().unwrap(), "--skills", "nope"]);
        assert_eq!(bad["error"]["kind"], "invalid_params");
    }

    #[test]
    fn remote_skill_status_and_install_over_ssh() {
        let rohan = Session::new();
        let mut s = Session::new();
        s.env.push(fake_ssh(&s, &[("rohan", &rohan)]));
        s.json(&["machine", "add", "rohan"]);
        wait_until("rohan connected", || s.json(&["machine", "ls"])["machines"][0]["state"] == "connected");

        // 1. Remote status with clean remote HOME.
        let st0 = s.json(&["skill", "status", "--on", "rohan"]);
        assert_eq!(st0["home"], rohan.root.display().to_string(), "st0: {st0}");
        assert_eq!(st0["summary"]["detectedAgents"], 0);

        // 2. Remote --all --detected-only skips fallback when nothing is detected on remote.
        let skipped = s.json(&["skill", "install", "--on", "rohan", "--all", "--detected-only"]);
        assert_eq!(skipped["installedCount"], 0, "skipped: {skipped}");
        assert!(!rohan.root.join(".claude/skills").exists());
        assert!(!rohan.root.join(".gemini/skills").exists());

        // 3. Remote has .codex marker -> remote install writes all 4 skills into remote .codex/skills.
        std::fs::create_dir_all(rohan.root.join(".codex")).unwrap();
        let inst = s.json(&["skill", "install", "--on", "rohan", "--all", "--detected-only"]);
        assert_eq!(inst["ok"], true);
        assert_eq!(inst["installedCount"], 4, "inst: {inst}");
        assert!(rohan.root.join(".codex/skills/herdr2/SKILL.md").exists());
        assert!(rohan.root.join(".codex/skills/drove/SKILL.md").exists());
        assert!(rohan.root.join(".codex/skills/drove-add/SKILL.md").exists());
        assert!(rohan.root.join(".codex/skills/drove-setup/SKILL.md").exists());

        let st1 = s.json(&["skill", "status", "--on", "rohan"]);
        assert_eq!(st1["summary"]["detectedAgents"], 1, "st1: {st1}");
        assert_eq!(st1["summary"]["upToDateAgents"], 1, "st1: {st1}");

        // 4. Remote --target with spaces in path tests shell quoting across SSH exec.
        let spaced = rohan.root.join("my custom skills");
        let sub = s.json(&[
            "skill",
            "install",
            "--on",
            "rohan",
            "--target",
            spaced.to_str().unwrap(),
            "--skills",
            "herdr2,drove-setup",
        ]);
        assert_eq!(sub["installedCount"], 2, "sub: {sub}");
        assert!(spaced.join("herdr2/SKILL.md").exists());
        assert!(spaced.join("drove-setup/SKILL.md").exists());
        assert!(!spaced.join("drove/SKILL.md").exists());
    }

    fn spawn_mock_ai_server(replies: Vec<String>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_bg = calls.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            raw.extend_from_slice(&buf[..n]);
                            if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&raw[..pos]);
                                let cl = head
                                    .lines()
                                    .find_map(|l| {
                                        let (k, v) = l.split_once(':')?;
                                        k.eq_ignore_ascii_case("content-length")
                                            .then(|| v.trim().parse::<usize>().ok())
                                            .flatten()
                                    })
                                    .unwrap_or(0);
                                if raw.len() >= pos + 4 + cl {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                let req = String::from_utf8_lossy(&raw);
                let first_line = req.lines().next().unwrap_or("");
                let body = if first_line.starts_with("GET /v1/models") {
                    json!({"data": [{"id": "mock-skill-model"}]}).to_string()
                } else {
                    let idx = calls_bg.fetch_add(1, Ordering::SeqCst);
                    let content = replies.get(idx).cloned().unwrap_or_else(|| "{}".into());
                    serde_json::to_string(&json!({
                        "id": format!("chatcmpl-{idx}"),
                        "model": "mock-skill-model",
                        "choices": [{
                            "index": 0,
                            "message": { "role": "assistant", "content": content },
                            "finish_reason": "stop"
                        }],
                        "usage": { "prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30 }
                    }))
                    .unwrap()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://127.0.0.1:{}/v1", addr.port()), calls)
    }

    #[test]
    fn remote_skill_ai_install_uses_local_core_ai_filters_traversal_and_falls_back() {
        let rohan = Session::new();
        std::fs::create_dir_all(rohan.root.join(".claude")).unwrap();

        let mut s = Session::new();
        s.env.push(fake_ssh(&s, &[("rohan", &rohan)]));
        s.json(&["machine", "add", "rohan"]);
        wait_until("rohan connected", || s.json(&["machine", "ls"])["machines"][0]["state"] == "connected");

        // Reply 1 (dry-run) & Reply 2 (apply): valid AI plan with safe remote dir + traversal attack + outside-home dir.
        let valid_plan = json!({
            "targets": [
                { "agent": "claude", "path": "~/.claude/skills", "skills": ["herdr2", "drove"], "reason": "Detected Claude" },
                { "agent": "evil1", "path": "~/../etc/skills", "skills": ["herdr2"], "reason": "Traversal" },
                { "agent": "evil2", "path": "/etc/herdr2-skills", "skills": ["herdr2"], "reason": "Outside home" }
            ],
            "summary": "Planned safe Claude remote install.",
            "tips": ["Restart Claude session on rohan."]
        })
        .to_string();
        // Reply 3: unstructured text -> must fall back to remote detected agent detection.
        let bad_plan = "not json at all".to_string();

        let (base_url, calls) = spawn_mock_ai_server(vec![valid_plan.clone(), valid_plan, bad_plan]);

        // Configure ONLY local Core AI (remote `rohan` has no AI key configured).
        let ai_cfg = s.root.join("ai-cfg.json");
        std::fs::write(
            &ai_cfg,
            serde_json::to_vec(&json!({
                "enabled": true,
                "baseUrl": base_url,
                "model": "mock-skill-model",
                "apiKey": "sk-test-local-only"
            }))
            .unwrap(),
        )
        .unwrap();
        s.json(&["ai", "settings-set", "--file", ai_cfg.to_str().unwrap(), "--consume"]);

        // 1. Dry-run filters out unsafe paths and writes nothing on remote.
        let dry = s.json(&["skill", "ai-install", "--on", "rohan", "--dry-run", "--lang", "en"]);
        assert_eq!(dry["ok"], true, "dry: {dry}");
        assert_eq!(dry["machine"], "rohan", "dry: {dry}");
        assert_eq!(dry["usedAi"], true, "dry: {dry}");
        assert_eq!(dry["dryRun"], true, "dry: {dry}");
        assert_eq!(dry["installedCount"], 0, "dry: {dry}");
        let dry_targets = dry["targets"].as_array().unwrap();
        assert_eq!(dry_targets.len(), 1, "unsafe paths must be filtered out: {dry}");
        assert_eq!(dry_targets[0]["agent"], "claude");
        assert_eq!(dry_targets[0]["target"], format!("{}/.claude/skills", rohan.root.display()));
        assert!(!rohan.root.join(".claude/skills/herdr2/SKILL.md").exists());

        // 2. Real apply installs the AI-selected skills onto remote host.
        let applied = s.json(&["skill", "ai-install", "--on", "rohan", "--lang", "en", "--query", "claude only"]);
        assert_eq!(applied["ok"], true, "applied: {applied}");
        assert_eq!(applied["usedAi"], true);
        assert_eq!(applied["installedCount"], 2);
        assert!(rohan.root.join(".claude/skills/herdr2/SKILL.md").exists());
        assert!(rohan.root.join(".claude/skills/drove/SKILL.md").exists());
        assert!(!rohan.root.join(".claude/skills/drove-add/SKILL.md").exists());

        // 3. Unstructured AI response falls back cleanly to detected remote agents (installs all 4 skills).
        let fallback = s.json(&["skill", "ai-install", "--on", "rohan", "--lang", "en", "--query", "force fallback"]);
        assert_eq!(fallback["ok"], true, "fallback: {fallback}");
        assert_eq!(fallback["usedAi"], false);
        assert!(fallback["aiError"].as_str().unwrap_or("").contains("unstructured"));
        assert_eq!(fallback["installedCount"], 4);
        assert!(rohan.root.join(".claude/skills/drove-add/SKILL.md").exists());
        assert!(rohan.root.join(".claude/skills/drove-setup/SKILL.md").exists());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
