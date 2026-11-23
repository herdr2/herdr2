#![cfg(unix)]

mod common;

use common::*;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn drove_session() -> Session {
    let mut s = Session::new();
    s.env.push(("H2_DROVE_NO_OPEN".into(), "1".into()));
    s.env.push(("H2_DROVE_PORT".into(), "0".into()));
    s
}

fn wait_web_port(s: &Session) -> (u16, PathBuf) {
    let mut port = 0u16;
    let mut key_path = PathBuf::new();
    wait_until("drove web port", || {
        let cfg = s.json(&["drove", "settings"]);
        port = cfg["webPort"].as_u64().unwrap_or(0) as u16;
        if let Some(p) = cfg["aiStatus"]["keyFilePath"].as_str() {
            key_path = PathBuf::from(p);
        }
        port > 0
    });
    (port, key_path)
}

fn read_coordinator_secret(key_file: &std::path::Path) -> String {
    let direct = key_file.parent().unwrap().join("coordinator.token");
    let token_path = if direct.exists() {
        direct
    } else {
        // Core AI key lives at `<state>/secrets/ai.key`, while Drove coordinator token lives at `<state>/drove/secrets/coordinator.token`.
        key_file
            .parent()
            .and_then(|p| p.parent())
            .unwrap()
            .join("drove")
            .join("secrets")
            .join("coordinator.token")
    };
    std::fs::read_to_string(&token_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", token_path.display()))
        .trim()
        .to_string()
}

fn read_web_token(port: u16) -> String {
    let index = http_req(port, "GET", "/", &[], None);
    assert_eq!(index.status, 200, "GET / failed: {}", index.body);
    let marker = "window.__DROVE_TOKEN__=\"";
    let start = index
        .body
        .find(marker)
        .unwrap_or_else(|| panic!("missing __DROVE_TOKEN__ in index HTML"));
    let rest = &index.body[start + marker.len()..];
    let end = rest
        .find('"')
        .unwrap_or_else(|| panic!("unterminated __DROVE_TOKEN__ in index HTML"));
    rest[..end].to_string()
}

struct HttpResp {
    status: u16,
    body: String,
    json: Option<Value>,
}

fn http_req(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&Value>,
) -> HttpResp {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();

    let payload = body.map(|v| serde_json::to_vec(v).unwrap()).unwrap_or_default();
    let mut raw = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    if body.is_some() {
        raw.push_str("Content-Type: application/json\r\n");
        raw.push_str(&format!("Content-Length: {}\r\n", payload.len()));
    }
    for (k, v) in headers {
        raw.push_str(&format!("{k}: {v}\r\n"));
    }
    raw.push_str("\r\n");
    stream.write_all(raw.as_bytes()).unwrap();
    if !payload.is_empty() {
        stream.write_all(&payload).unwrap();
    }
    stream.flush().unwrap();

    let mut resp_bytes = Vec::new();
    stream.read_to_end(&mut resp_bytes).unwrap();
    let text = String::from_utf8_lossy(&resp_bytes);
    let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let json = serde_json::from_str::<Value>(rest).ok();
    HttpResp { status, body: rest.to_string(), json }
}

fn open_sse(port: u16, path: &str) -> BufReader<TcpStream> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    // Consume HTTP response headers and expect `: connected`.
    let mut seen_connected = false;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        line.clear();
        let n = reader.read_line(&mut line).unwrap();
        assert!(n > 0, "SSE closed before : connected");
        if line.starts_with(": connected") {
            seen_connected = true;
            break;
        }
    }
    assert!(seen_connected, "did not receive : connected on SSE");
    reader
}

fn next_sse_event(reader: &mut BufReader<TcpStream>) -> (String, Value) {
    let start = Instant::now();
    let mut event_name = String::new();
    let mut data_line = String::new();
    while start.elapsed() < Duration::from_secs(5) {
        let mut line = String::new();
        let n = reader.read_line(&mut line).unwrap();
        assert!(n > 0, "SSE stream closed while waiting for event");
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        if let Some(ev) = trimmed.strip_prefix("event:") {
            event_name = ev.trim().to_string();
        } else if let Some(data) = trimmed.strip_prefix("data:") {
            data_line = data.trim().to_string();
        } else if trimmed.is_empty() && !event_name.is_empty() && !data_line.is_empty() {
            let parsed: Value = serde_json::from_str(&data_line)
                .unwrap_or_else(|e| panic!("bad SSE JSON ({e}): {data_line}"));
            return (event_name, parsed);
        }
    }
    panic!("timed out waiting for SSE event");
}

#[test]
fn drove_settings_get_set_and_secret_key_storage() {
    let s = drove_session();
    let init = s.json(&["drove", "settings"]);
    let roles = init["config"]["roles"].as_object().unwrap();
    for id in ["coder", "tester", "writer", "illustrator", "reviewer", "researcher"] {
        assert!(roles.contains_key(id), "missing built-in role {id}");
    }
    assert_eq!(init["config"]["magpie"]["enabled"], false);
    assert_eq!(init["aiStatus"]["configured"], false);

    let req_file = s.root.join("settings-req.json");
    let secret = "sk-test-secret-key-987654";
    std::fs::write(
        &req_file,
        serde_json::to_vec(&json!({
            "roles": {
                "coder": {
                    "label": "Coder",
                    "access": "writable",
                    "primary": { "agent": "codex", "model": "gpt-5.4", "effort": "high" },
                    "fallbacks": []
                }
            },
            "ai": {
                "enabled": true,
                "baseUrl": "https://example.invalid/v1",
                "apiKey": secret
            },
            "magpie": {
                "enabled": true,
                "gatewayUrl": "http://127.0.0.1:19999"
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let updated = s.json(&["drove", "settings-set", "--file", req_file.to_str().unwrap(), "--consume"]);
    assert!(!req_file.exists(), "settings-set --consume should delete the temp file");
    assert_eq!(updated["config"]["roles"]["coder"]["primary"]["agent"], "codex");
    assert_eq!(updated["config"]["roles"]["coder"]["primary"]["model"], "gpt-5.4");
    assert_eq!(updated["config"]["magpie"]["enabled"], true);
    assert_eq!(updated["aiStatus"]["configured"], true);
    let masked = updated["aiStatus"]["keyMasked"].as_str().unwrap();
    assert!(!masked.contains("987654") || masked.contains('*'), "key should be masked: {masked}");

    // Ensure raw key lives only in 0600 ai.key and NOT in config file.
    let cfg_text = std::fs::read_to_string(updated["configPath"].as_str().unwrap()).unwrap();
    assert!(!cfg_text.contains(secret), "drove.json must never contain raw apiKey");

    let key_path = PathBuf::from(updated["aiStatus"]["keyFilePath"].as_str().unwrap());
    assert_eq!(std::fs::read_to_string(&key_path).unwrap().trim(), secret);
    let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "ai.key must be 0600, got {mode:o}");
}

#[test]
fn drove_ai_test_discovers_model_and_succeeds() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mock_port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let saw_auth = Arc::new(AtomicBool::new(false));

    let stop_srv = stop.clone();
    let auth_srv = saw_auth.clone();
    let srv = std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut conn, _)) => {
                    let mut buf = [0u8; 4096];
                    let n = conn.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    if req.contains("Authorization: Bearer sk-mock-probe-123")
                        || req.contains("authorization: Bearer sk-mock-probe-123")
                    {
                        auth_srv.store(true, Ordering::Relaxed);
                    }
                    let first_line = req.lines().next().unwrap_or("");
                    let body = if first_line.starts_with("GET /v1/models") {
                        r#"{"data":[{"id":"mock-fast-model"}]}"#
                    } else if first_line.starts_with("POST /v1/chat/completions") {
                        r#"{"id":"chatcmpl-1","choices":[{"message":{"role":"assistant","content":"OK"}}]}"#
                    } else {
                        r#"{"error":"not_found"}"#
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = conn.write_all(resp.as_bytes());
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    let s = drove_session();
    let req_file = s.root.join("ai-test.json");
    std::fs::write(
        &req_file,
        serde_json::to_vec(&json!({
            "baseUrl": format!("http://127.0.0.1:{mock_port}/v1"),
            "apiKey": "sk-mock-probe-123"
        }))
        .unwrap(),
    )
    .unwrap();

    let res = s.json(&["drove", "ai-test", "--file", req_file.to_str().unwrap(), "--consume"]);
    stop.store(true, Ordering::Relaxed);
    srv.join().unwrap();

    assert!(!req_file.exists(), "ai-test --consume should remove temp file");
    assert_eq!(res["ok"], true, "ai-test response: {res}");
    assert_eq!(res["discoveredModel"], "mock-fast-model");
    assert!(saw_auth.load(Ordering::Relaxed), "mock server did not see Bearer token");
}

#[test]
fn drove_http_server_and_sse_and_read_only_agents() {
    let s = drove_session();
    let (port, key_file) = wait_web_port(&s);
    let coord_secret = read_coordinator_secret(&key_file);

    let health = http_req(port, "GET", "/api/health", &[], None);
    assert_eq!(health.status, 200);
    assert_eq!(health.json.as_ref().unwrap()["ok"], true);

    // Magpie disabled by default -> no quotas card / no quotaError.
    let agents = http_req(port, "GET", "/api/agents", &[], None);
    assert_eq!(agents.status, 200);
    let agents_json = agents.json.unwrap();
    assert!(agents_json.get("quotas").is_none(), "quotas should be omitted when magpie disabled");

    // Read-only enforcement in Web UI.
    assert_eq!(http_req(port, "PUT", "/api/agents", &[], Some(&json!({}))).status, 405);
    assert_eq!(http_req(port, "PUT", "/api/ai/config", &[], Some(&json!({}))).status, 405);

    // HTML shell injects __DROVE_TOKEN__.
    let index = http_req(port, "GET", "/", &[], None);
    assert_eq!(index.status, 200);
    assert!(index.body.contains("window.__DROVE_TOKEN__"), "index HTML missing __DROVE_TOKEN__");

    // Unauthorized write is rejected; coordinator-secret write succeeds & fires SSE.
    let unauth = http_req(
        port,
        "POST",
        "/api/runs",
        &[],
        Some(&json!({ "hostSession": "t1", "objective": "fail", "cwd": "/tmp" })),
    );
    assert_eq!(unauth.status, 401);

    let mut sse = open_sse(port, "/api/events");
    let created = http_req(
        port,
        "POST",
        "/api/runs",
        &[
            ("x-drove-coordinator-secret", &coord_secret),
            ("x-drove-coordinator", "t1"),
        ],
        Some(&json!({ "hostSession": "t1", "objective": "Verify HTTP + SSE", "cwd": "/tmp" })),
    );
    assert_eq!(created.status, 200, "create run failed: {}", created.body);
    let run_id = created.json.unwrap()["id"].as_str().unwrap().to_string();

    let (ev_name, ev_data) = next_sse_event(&mut sse);
    assert_eq!(ev_name, "change");
    assert_eq!(ev_data["table"], "runs");
    assert_eq!(ev_data["runId"], run_id);

    let runs = http_req(port, "GET", "/api/runs", &[], None);
    assert_eq!(runs.status, 200);
    assert_eq!(runs.json.unwrap()["runs"].as_array().unwrap().len(), 1);
}

#[test]
fn drove_ssh_federation_proxies_runs_and_streams_events() {
    let rohan = drove_session();
    let (rohan_port, rohan_key) = wait_web_port(&rohan);
    let rohan_secret = read_coordinator_secret(&rohan_key);

    let mut local = drove_session();
    local.env.push(fake_ssh(&local, &[("rohan", &rohan)]));
    local.env.push(("H2_RECONNECT_MS".into(), "200".into()));
    local.json(&["machine", "add", "rohan"]);
    wait_until("rohan connected", || {
        local.json(&["machine", "ls"])["machines"][0]["state"] == "connected"
    });
    let (local_port, _) = wait_web_port(&local);

    let created = http_req(
        rohan_port,
        "POST",
        "/api/runs",
        &[
            ("x-drove-coordinator-secret", &rohan_secret),
            ("x-drove-coordinator", "t9"),
        ],
        Some(&json!({ "hostSession": "t9", "objective": "Remote SSH run", "cwd": "/tmp" })),
    );
    assert_eq!(created.status, 200, "remote run create failed: {}", created.body);
    let remote_run_id = created.json.unwrap()["id"].as_str().unwrap().to_string();
    let expected_id = format!("rohan/{remote_run_id}");

    // Local /api/runs aggregates remote run with `rohan/<id>` and `machine: "rohan"`.
    let listed = http_req(local_port, "GET", "/api/runs", &[], None);
    assert_eq!(listed.status, 200);
    let runs = listed.json.unwrap()["runs"].as_array().unwrap().clone();
    assert_eq!(runs.len(), 1, "expected federated run: {runs:?}");
    assert_eq!(runs[0]["id"], expected_id);
    assert_eq!(runs[0]["machine"], "rohan");

    // Proxied snapshot lookup (`%2F` encoded slash in path segment).
    let snap = http_req(local_port, "GET", &format!("/api/runs/rohan%2F{remote_run_id}"), &[], None);
    assert_eq!(snap.status, 200, "proxied run snapshot failed: {}", snap.body);
    let snap_json = snap.json.unwrap();
    assert_eq!(snap_json["run"]["id"], expected_id);
    assert_eq!(snap_json["run"]["machine"], "rohan");

    // Local SSE relays remote events over framed `drove.subscribe` channel.
    let mut sse = open_sse(local_port, "/api/events");
    let patched = http_req(
        rohan_port,
        "PATCH",
        &format!("/api/runs/{remote_run_id}"),
        &[("x-drove-coordinator-secret", &rohan_secret)],
        Some(&json!({ "name": "Renamed over SSH" })),
    );
    assert_eq!(patched.status, 200, "remote patch failed: {}", patched.body);

    let (ev_name, ev_data) = next_sse_event(&mut sse);
    assert_eq!(ev_name, "change");
    assert_eq!(ev_data["runId"], expected_id);
}

fn configure_fake_coder(s: &Session) {
    let req_file = s.root.join("fake-coder-cfg.json");
    let spec = fake_acp_spec();
    std::fs::write(
        &req_file,
        serde_json::to_vec(&json!({
            "roles": {
                "coder": {
                    "label": "Coder",
                    "access": "writable",
                    "primary": { "agent": spec, "effort": "high" },
                    "fallbacks": []
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    s.json(&["drove", "settings-set", "--file", req_file.to_str().unwrap(), "--consume"]);
}

#[test]
fn drove_dag_executes_with_native_acp_workers_and_tree_labels() {
    let s = drove_session();
    let (port, _) = wait_web_port(&s);
    configure_fake_coder(&s);

    let work_dir = s.root.join("work");
    std::fs::create_dir_all(&work_dir).unwrap();
    let coord = run_fake(&s, &[]);

    let plan_path = s.root.join("plan.yaml");
    std::fs::write(
        &plan_path,
        concat!(
            "objective: E2E Drove DAG\n",
            "maxConcurrency: 2\n",
            "tasks:\n",
            "  - key: step1\n",
            "    title: Step One\n",
            "    role: coder\n",
            "    spec:\n",
            "      target: workspace\n",
            "      change: |\n",
            "        SH:herdr2 drove worker done --outcome done --summary \"step1 built\"\n",
            "      acceptance: step1 done\n",
            "  - key: step2\n",
            "    title: Step Two\n",
            "    role: coder\n",
            "    deps: [step1]\n",
            "    spec:\n",
            "      target: workspace\n",
            "      change: |\n",
            "        SH:herdr2 drove worker done --outcome done --summary \"step2 verified\"\n",
            "      acceptance: step2 done\n",
            "  - key: shell_check\n",
            "    title: Shell Check\n",
            "    kind: exec\n",
            "    deps: [step2]\n",
            "    exec:\n",
            "      cmd: \"echo ok\"\n",
        ),
    )
    .unwrap();

    let out = s
        .cmd()
        .env("H2_AGENT_ID", &coord)
        .args([
            "--json",
            "drove",
            "plan",
            "--file",
            plan_path.to_str().unwrap(),
            "--cwd",
            work_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "drove plan failed: {}", String::from_utf8_lossy(&out.stderr));
    let planned: Value = serde_json::from_slice(&out.stdout).unwrap();
    let run_id = planned["run"]["id"].as_str().unwrap().to_string();

    let waited = s.json(&["drove", "wait", "--run", &run_id, "--timeout", "15"]);
    assert_eq!(waited["settled"], true, "wait did not settle: {waited}");

    let snap = s.json(&["drove", "show", &run_id]);
    let tasks = snap["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 3);
    for t in tasks {
        assert_eq!(t["status"], "succeeded", "task {} not succeeded: {t}", t["key"]);
    }

    // Warm worker reuse across sequential `step1 -> step2`: only 1 worker spawned!
    let workers = snap["workers"].as_array().unwrap();
    assert_eq!(workers.len(), 1, "expected 1 reused worker, got {workers:?}");
    let worker_id = workers[0]["id"].as_str().unwrap().to_string();
    let worker_addr = workers[0]["h2Agent"].as_str().unwrap().to_string();
    assert_eq!(workers[0]["state"], "idle");

    // Verify bottom-left `Agents` tree parent + display label metadata in `herdr2`.
    let listed = s.json(&["ls", "--all"]);
    let h2_ls_rec = listed["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["address"] == worker_addr)
        .unwrap_or_else(|| panic!("worker {worker_addr} missing in h2 ls: {listed}"));
    assert_eq!(h2_ls_rec["parent"], coord);
    let h2_rec = s.json(&["get", &worker_addr]);
    assert_eq!(h2_rec["parent"], coord);
    assert_eq!(h2_rec["title"], "coder · idle");
    assert_eq!(h2_rec["labels"]["drove.display"], "coder · idle");
    assert_eq!(h2_rec["labels"]["drove.run"], run_id);

    // Worker items streamed into Store.
    let items_resp = http_req(port, "GET", &format!("/api/workers/{worker_id}/items"), &[], None);
    assert_eq!(items_resp.status, 200);
    assert!(!items_resp.json.unwrap()["items"].as_array().unwrap().is_empty());

    // Complete run preserves idle worker ACP session for warm reuse across follow-up tasks.
    let done = s.json(&["drove", "complete", &run_id, "--summary", "DAG verified"]);
    assert_eq!(done["run"]["status"], "done");
    let after_complete = s.json(&["drove", "show", &run_id]);
    assert_eq!(after_complete["workers"][0]["state"], "idle");
    assert_eq!(s.json(&["get", &worker_addr])["state"], "live");

    // Appending a follow-up task reactivates the Done run and reuses the same warm worker.
    let add_out = s
        .cmd()
        .env("H2_AGENT_ID", &coord)
        .args([
            "--json",
            "drove",
            "add",
            "--run",
            &run_id,
            "--key",
            "step3",
            "--title",
            "Step Three",
            "--role",
            "coder",
            "--change",
            "SH:herdr2 drove worker done --outcome done --summary \"step3 warm reused\"",
        ])
        .output()
        .unwrap();
    assert!(add_out.status.success(), "drove add failed: {}", String::from_utf8_lossy(&add_out.stderr));
    let waited2 = s.json(&["drove", "wait", "--run", &run_id, "--timeout", "15"]);
    assert_eq!(waited2["settled"], true, "wait after add did not settle: {waited2}");

    let snap2 = s.json(&["drove", "show", &run_id]);
    assert_eq!(snap2["workers"].as_array().unwrap().len(), 1, "expected same warm worker after add: {snap2:?}");
    let step3 = snap2["tasks"].as_array().unwrap().iter().find(|t| t["key"] == "step3").unwrap();
    assert_eq!(step3["status"], "succeeded", "step3 not succeeded: {step3}");
    let step3_id = step3["id"].as_str().unwrap();
    let step3_att = snap2["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["taskId"] == step3_id)
        .unwrap();
    assert_eq!(step3_att["reused"], true, "step3 must reuse warm worker: {step3_att}");
    assert_eq!(step3_att["workerId"], worker_id);

    // Killing the coordinator (main agent) closes its idle sub-agent workers.
    s.json(&["kill", &coord]);
    wait_until("worker ACP agent finished after coordinator closed", || {
        s.json(&["get", &worker_addr])["state"] == "finished"
    });
    wait_until("drove worker closed after coordinator closed", || {
        s.json(&["drove", "show", &run_id])["workers"][0]["state"] == "closed"
    });
}

#[test]
fn drove_worker_ask_wakes_coordinator_and_unblocks_on_reply() {
    let s = drove_session();
    let _ = wait_web_port(&s);
    configure_fake_coder(&s);

    let work_dir = s.root.join("ask_work");
    std::fs::create_dir_all(&work_dir).unwrap();
    let coord = run_fake(&s, &[]);

    let prompt = "SH:ans=$(herdr2 drove worker ask \"Which port?\" --options \"8080,9090\" | python3 -c \"import sys,json; print(json.load(sys.stdin)['reply']['body'])\") && test \"$ans\" = \"8080\" && herdr2 drove worker done --outcome done --summary \"port $ans\"";
    let out = s
        .cmd()
        .env("H2_AGENT_ID", &coord)
        .args(["--json", "drove", "quick", "--cwd", work_dir.to_str().unwrap(), prompt])
        .output()
        .unwrap();
    assert!(out.status.success(), "drove quick failed: {}", String::from_utf8_lossy(&out.stderr));
    let started: Value = serde_json::from_slice(&out.stdout).unwrap();
    let run_id = started["run"]["id"].as_str().unwrap().to_string();

    let waited = s.json(&["drove", "wait", "--run", &run_id, "--timeout", "15"]);
    let msgs = waited["messages"].as_array().unwrap();
    let q = msgs
        .iter()
        .find(|m| m["type"] == "question")
        .unwrap_or_else(|| panic!("expected question message in {waited}"));
    let msg_id = q["id"].as_str().unwrap().to_string();
    assert_eq!(q["body"], "Which port?");

    // Coordinator agent received the wakeup notification.
    wait_until("coordinator transcript wakeup", || {
        let txt = s.json(&["read", "--lines", "100", &coord])["text"].as_str().unwrap_or("").to_string();
        txt.contains("[drove] Question")
    });

    s.json(&["drove", "reply", &msg_id, "8080"]);

    let settled = s.json(&["drove", "wait", "--run", &run_id, "--timeout", "15"]);
    assert_eq!(settled["settled"], true, "run did not settle after reply: {settled}");
    let task = &settled["tasks"][0];
    assert_eq!(task["status"], "succeeded", "task after ask reply: {task}");
    assert_eq!(task["result"]["summary"], "port 8080");
}

#[test]
fn drove_external_agent_kill_syncs_worker_and_recovers() {
    let s = drove_session();
    let _ = wait_web_port(&s);
    configure_fake_coder(&s);

    let work_dir = s.root.join("kill_work");
    std::fs::create_dir_all(&work_dir).unwrap();
    let marker = work_dir.join("attempt1_started");
    let prompt = "SH:if [ ! -f attempt1_started ]; then touch attempt1_started; sleep 10; else herdr2 drove worker done --outcome done --summary \"recovered\"; fi";
    let started = s.json(&["drove", "quick", "--cwd", work_dir.to_str().unwrap(), prompt]);
    let run_id = started["run"]["id"].as_str().unwrap().to_string();

    let mut first_worker_addr = String::new();
    wait_until("attempt1 marker and busy worker", || {
        if !marker.exists() {
            return false;
        }
        let snap = s.json(&["drove", "show", &run_id]);
        if let Some(w) = snap["workers"].as_array().and_then(|a| a.first())
            && w["state"] == "busy"
            && let Some(addr) = w["h2Agent"].as_str()
        {
            first_worker_addr = addr.to_string();
            return true;
        }
        false
    });

    // External kill on `a*` must sync WorkerState::Dead and auto-recover task on a new worker.
    s.json(&["kill", &first_worker_addr]);

    let mut final_snap = Value::Null;
    wait_until("recovered task after external kill", || {
        let snap = s.json(&["drove", "show", &run_id]);
        if snap["tasks"][0]["status"] == "succeeded" {
            final_snap = snap;
            return true;
        }
        false
    });

    assert_eq!(final_snap["tasks"][0]["result"]["summary"], "recovered");
    let workers = final_snap["workers"].as_array().unwrap();
    assert_eq!(workers[0]["state"], "dead");
    assert_eq!(workers.len(), 2, "expected replacement worker after kill: {workers:?}");
}

fn configure_fake_roles(s: &Session) {
    let req_file = s.root.join("fake-roles-cfg.json");
    let spec = fake_acp_spec();
    std::fs::write(
        &req_file,
        serde_json::to_vec(&json!({
            "roles": {
                "coder": {
                    "label": "Coder",
                    "access": "writable",
                    "primary": { "agent": spec, "effort": "high" },
                    "fallbacks": []
                },
                "tester": {
                    "label": "Tester",
                    "access": "scoped_write",
                    "allowedPaths": ["tests/**"],
                    "primary": { "agent": spec, "effort": "medium" },
                    "fallbacks": []
                },
                "reviewer": {
                    "label": "Reviewer",
                    "access": "readonly",
                    "primary": { "agent": spec, "effort": "high" },
                    "fallbacks": []
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    s.json(&["drove", "settings-set", "--file", req_file.to_str().unwrap(), "--consume"]);
}

#[test]
fn drove_role_scope_violation_rejects_and_recovers() {
    let s = drove_session();
    let _ = wait_web_port(&s);
    configure_fake_roles(&s);

    let work_dir = s.root.join("scope_work");
    std::fs::create_dir_all(&work_dir).unwrap();
    std::fs::write(work_dir.join("clean.txt"), "baseline\n").unwrap();

    // ScopedWrite `tester` (allowedPaths: ["tests/**"]) writes `illegal.txt` -> `worker done` rejects with 422;
    // deleting `illegal.txt` and writing inside `tests/ok.txt` succeeds.
    let prompt = "SH:echo dirty > illegal.txt && ! herdr2 drove worker done --outcome done --summary \"should fail\" && rm -f illegal.txt && mkdir -p tests && echo ok > tests/ok.txt && herdr2 drove worker done --outcome done --summary \"scoped write ok\"";
    let started = s.json(&[
        "drove",
        "quick",
        "--role",
        "tester",
        "--cwd",
        work_dir.to_str().unwrap(),
        prompt,
    ]);
    let run_id = started["run"]["id"].as_str().unwrap().to_string();

    let settled = s.json(&["drove", "wait", "--run", &run_id, "--timeout", "15"]);
    assert_eq!(settled["settled"], true, "wait result: {settled}");
    assert_eq!(settled["tasks"][0]["status"], "succeeded");
    assert_eq!(settled["tasks"][0]["result"]["summary"], "scoped write ok");
    assert!(!work_dir.join("illegal.txt").exists());
    assert_eq!(std::fs::read_to_string(work_dir.join("tests/ok.txt")).unwrap(), "ok\n");
}

#[test]
fn drove_verify_gate_rejects_failure_then_passes_with_artifact_log() {
    let s = drove_session();
    let (port, _) = wait_web_port(&s);
    configure_fake_coder(&s);

    let work_dir = s.root.join("verify_work");
    std::fs::create_dir_all(&work_dir).unwrap();
    let plan_path = s.root.join("verify-plan.yaml");
    std::fs::write(
        &plan_path,
        concat!(
            "objective: Verify gate test\n",
            "verify: \"test -f final.ok\"\n",
            "tasks:\n",
            "  - key: gated\n",
            "    title: Gated Task\n",
            "    role: coder\n",
            "    spec:\n",
            "      target: ready.flag\n",
            "      change: |\n",
            "        SH:! herdr2 drove worker done --outcome done --summary \"premature\" && touch ready.flag && herdr2 drove worker done --outcome done --summary \"verified ok\"\n",
            "      acceptance: ready.flag exists\n",
            "      verify: \"test -f ready.flag\"\n",
        ),
    )
    .unwrap();

    let planned = s.json(&[
        "drove",
        "plan",
        "--file",
        plan_path.to_str().unwrap(),
        "--cwd",
        work_dir.to_str().unwrap(),
    ]);
    let run_id = planned["run"]["id"].as_str().unwrap().to_string();

    let waited = s.json(&["drove", "wait", "--run", &run_id, "--timeout", "15"]);
    assert_eq!(waited["settled"], true, "waited: {waited}");
    assert_eq!(waited["tasks"][0]["status"], "succeeded");
    assert_eq!(waited["tasks"][0]["result"]["summary"], "verified ok");

    let snap = s.json(&["drove", "show", &run_id]);
    let att = &snap["attempts"][0];
    assert_eq!(att["verifyRejections"], 1);
    let art_id = att["verifyLogArtifactId"].as_str().unwrap();

    let log_resp = http_req(
        port,
        "GET",
        &format!("/api/runs/{run_id}/verify-log?artifact={art_id}"),
        &[],
        None,
    );
    assert_eq!(log_resp.status, 200, "verify-log response: {}", log_resp.body);
    assert!(log_resp.body.contains("ready.flag"));

    // Run-level verify fails before final.ok exists, then succeeds once created.
    let err_out = s
        .cmd()
        .args(["--json", "drove", "complete", &run_id])
        .output()
        .unwrap();
    assert!(!err_out.status.success(), "run complete should fail before final.ok");
    std::fs::write(work_dir.join("final.ok"), "ok\n").unwrap();
    let ok_done = s.json(&["drove", "complete", &run_id]);
    assert_eq!(ok_done["run"]["status"], "done");
}

#[test]
fn drove_worktree_isolation_merges_git_changes_cleanly() {
    let s = drove_session();
    let (port, _) = wait_web_port(&s);
    configure_fake_coder(&s);

    let repo = s.root.join("git_repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    git(&["init"]);
    git(&["config", "user.email", "test@drove.local"]);
    git(&["config", "user.name", "Drove Test"]);
    std::fs::write(repo.join("README.md"), "# Test\n").unwrap();
    git(&["add", "README.md"]);
    git(&["commit", "-m", "init"]);

    let plan_path = s.root.join("wt-plan.yaml");
    let repo_str = repo.display().to_string();
    let script = format!(
        concat!(
            "objective: Worktree isolation\n",
            "tasks:\n",
            "  - key: wt1\n",
            "    title: Worktree Task\n",
            "    isolation: worktree\n",
            "    spec:\n",
            "      target: feature.txt\n",
            "      change: |\n",
            "        SH:test \"$(pwd)\" != \"{repo_str}\" && echo \"worktree-ok\" > feature.txt && herdr2 drove worker done --outcome done --summary \"wt merged\"\n",
            "      acceptance: feature.txt merged\n",
        ),
        repo_str = repo_str,
    );
    std::fs::write(&plan_path, script).unwrap();

    let planned = s.json(&[
        "drove",
        "plan",
        "--file",
        plan_path.to_str().unwrap(),
        "--cwd",
        &repo_str,
    ]);
    let run_id = planned["run"]["id"].as_str().unwrap().to_string();

    let waited = s.json(&["drove", "wait", "--run", &run_id, "--timeout", "15"]);
    assert_eq!(waited["settled"], true, "worktree wait: {waited}");
    assert_eq!(waited["tasks"][0]["status"], "succeeded");
    assert_eq!(std::fs::read_to_string(repo.join("feature.txt")).unwrap(), "worktree-ok\n");

    let snap1 = s.json(&["drove", "show", &run_id]);
    let workers1 = snap1["workers"].as_array().unwrap();
    assert_eq!(workers1.len(), 1);
    assert_eq!(workers1[0]["state"], "idle", "worktree worker should stay idle for reuse: {workers1:?}");
    let worker_addr = workers1[0]["h2Agent"].as_str().unwrap().to_string();
    assert_eq!(s.json(&["get", &worker_addr])["state"], "live");

    // Follow-up task without --worktree still reuses the warm worktree worker and merges cleanly.
    s.json(&[
        "drove",
        "add",
        "--run",
        &run_id,
        "--key",
        "wt2",
        "--change",
        "SH:echo wt2-ok > feature2.txt && herdr2 drove worker done --outcome done --summary \"wt2 merged\"",
    ]);
    let waited2 = s.json(&["drove", "wait", "--run", &run_id, "--timeout", "15"]);
    assert_eq!(waited2["settled"], true, "wt2 wait: {waited2}");
    assert_eq!(std::fs::read_to_string(repo.join("feature2.txt")).unwrap(), "wt2-ok\n");

    let snap2 = s.json(&["drove", "show", &run_id]);
    assert_eq!(snap2["workers"].as_array().unwrap().len(), 1);
    assert_eq!(snap2["attempts"][1]["reused"], true);

    // Trashing & deleting the run via Web HTTP closes the warm worktree worker and removes its git worktree.
    let web_tok = read_web_token(port);
    let trashed = http_req(
        port,
        "POST",
        &format!("/api/runs/{run_id}/trash"),
        &[("x-drove-token", &web_tok)],
        None,
    );
    assert_eq!(trashed.status, 200, "trash failed: {}", trashed.body);
    let deleted = http_req(
        port,
        "DELETE",
        &format!("/api/runs/{run_id}"),
        &[("x-drove-token", &web_tok), ("x-drove-confirm", &run_id)],
        None,
    );
    assert_eq!(deleted.status, 200, "delete failed: {}", deleted.body);

    wait_until("worktree worker finished after web delete", || {
        s.json(&["get", &worker_addr])["state"] == "finished"
    });
    let wt_list = git(&["worktree", "list", "--porcelain"]);
    assert_eq!(
        wt_list.lines().filter(|l| l.starts_with("worktree ")).count(),
        1,
        "stale worktree remained: {wt_list}"
    );
}

#[test]
fn drove_adversarial_review_requests_changes_then_approves() {
    let s = drove_session();
    let (port, _) = wait_web_port(&s);
    configure_fake_roles(&s);

    let repo = s.root.join("rev_repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init"]);
    git(&["config", "user.email", "test@drove.local"]);
    git(&["config", "user.name", "Drove Test"]);
    std::fs::write(repo.join("out.txt"), "base\n").unwrap();
    git(&["add", "out.txt"]);
    git(&["commit", "-m", "init"]);

    let rev_script = s.root.join("mock_review.py");
    let state_file = s.root.join("rev_state.txt");
    std::fs::write(
        &rev_script,
        format!(
            "import json, os\np = {state:?}\nif not os.path.exists(p):\n  open(p, 'w').write('1')\n  print(json.dumps({{'verdict': 'changes_requested', 'summary': 'Need header', 'findings': [{{'severity': 'high', 'file': 'out.txt', 'message': 'Add header line'}}]}}))\nelse:\n  print(json.dumps({{'verdict': 'approve', 'summary': 'Clean now', 'findings': []}}))\n",
            state = state_file.display().to_string()
        ),
    )
    .unwrap();

    let plan_path = s.root.join("review-plan.yaml");
    let rev_cmd = format!("SH:python3 {}", rev_script.display());
    std::fs::write(
        &plan_path,
        format!(
            "objective: Review loop test\n\
             tasks:\n  \
               - key: reviewed\n    \
                 title: Reviewed Task\n    \
                 role: coder\n    \
                 review:\n      \
                   maxRounds: 2\n      \
                   criteria: {rev_cmd}\n    \
                 spec:\n      \
                   target: out.txt\n      \
                   change: |\n        \
                     SH:echo \"body\" >> out.txt && ! herdr2 drove worker done --outcome done --summary \"v1\" && echo \"header\" >> out.txt && herdr2 drove worker done --outcome done --summary \"v2\"\n      \
                   acceptance: approved output\n"
        ),
    )
    .unwrap();

    let planned = s.json(&[
        "drove",
        "plan",
        "--file",
        plan_path.to_str().unwrap(),
        "--cwd",
        repo.to_str().unwrap(),
    ]);
    let run_id = planned["run"]["id"].as_str().unwrap().to_string();

    let waited = s.json(&["drove", "wait", "--run", &run_id, "--timeout", "15"]);
    assert_eq!(waited["settled"], true, "review wait: {waited}");
    let task = &waited["tasks"][0];
    assert_eq!(task["status"], "succeeded", "task after review: {task}");
    assert_eq!(task["result"]["summary"], "v2");

    let snap = s.json(&["drove", "show", &run_id]);
    let author_att = snap["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["kind"] == "author")
        .unwrap();
    assert_eq!(author_att["reviewRound"]["round"], 2);
    assert_eq!(author_att["reviewRound"]["verdict"], "approve");

    let rev_atts: Vec<_> = snap["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["kind"] == "review")
        .collect();
    assert_eq!(rev_atts.len(), 2, "expected 2 review attempts: {rev_atts:?}");
    assert_eq!(
        rev_atts[0]["workerId"], rev_atts[1]["workerId"],
        "round 2 should reuse round 1 reviewer worker: {rev_atts:?}"
    );
    assert_eq!(
        rev_atts[1]["reused"], true,
        "round 2 review attempt should record reused=true: {rev_atts:?}"
    );

    let task_id = task["id"].as_str().unwrap();
    let patch_art = author_att["reviewRound"]["patchArtifactId"].as_str().unwrap();
    let diff_resp = http_req(
        port,
        "GET",
        &format!("/api/tasks/{task_id}/review-diff?artifact={patch_art}"),
        &[],
        None,
    );
    assert_eq!(diff_resp.status, 200, "review-diff response: {}", diff_resp.body);
    assert!(diff_resp.body.contains("out.txt"));
}

#[test]
fn drove_install_writes_all_three_skills() {
    let s = drove_session();
    let target = s.root.join("skills_out");
    let res = s.json(&["drove", "install", "--target", target.to_str().unwrap()]);
    assert_eq!(res["ok"], true);
    for name in ["drove", "drove-add", "drove-setup"] {
        let p = target.join(name).join("SKILL.md");
        let text = std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("missing skill {}: {e}", p.display()));
        assert!(text.contains(&format!("name: {name}")), "unexpected skill header in {p:?}");
    }
}

#[test]
fn drove_ai_auto_title_and_question_draft_hooks() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mock_port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();

    let srv = std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut conn, _)) => {
                    let mut buf = [0u8; 8192];
                    let n = conn.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let first = req.lines().next().unwrap_or("");
                    let body = if first.starts_with("GET /v1/models") {
                        r#"{"data":[{"id":"mock-ai-helper"}]}"#
                    } else if first.starts_with("POST /v1/chat/completions") {
                        if req.contains("Draft a concise answer") {
                            r#"{"usage":{"prompt_tokens":10,"completion_tokens":5},"choices":[{"message":{"role":"assistant","content":"{\"answer\":\"Use 8080\",\"confidence\":0.92}"}}]}"#
                        } else {
                            r#"{"usage":{"prompt_tokens":8,"completion_tokens":4},"choices":[{"message":{"role":"assistant","content":"Auth Middleware Cleanup"}}]}"#
                        }
                    } else {
                        r#"{}"#
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = conn.write_all(resp.as_bytes());
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    let s = drove_session();
    let (port, key_file) = wait_web_port(&s);
    let coord_secret = read_coordinator_secret(&key_file);

    let cfg_req = s.root.join("ai-hooks-cfg.json");
    std::fs::write(
        &cfg_req,
        serde_json::to_vec(&json!({
            "ai": {
                "enabled": true,
                "baseUrl": format!("http://127.0.0.1:{mock_port}/v1"),
                "apiKey": "sk-ai-hooks-test",
                "autoTitle": true
            }
        }))
        .unwrap(),
    )
    .unwrap();
    s.json(&["drove", "settings-set", "--file", cfg_req.to_str().unwrap(), "--consume"]);

    let created = http_req(
        port,
        "POST",
        "/api/runs",
        &[
            ("x-drove-coordinator-secret", &coord_secret),
            ("x-drove-coordinator", "t1"),
        ],
        Some(&json!({ "hostSession": "t1", "objective": "Clean up auth middleware", "cwd": "/tmp" })),
    );
    assert_eq!(created.status, 200);
    let run_id = created.json.unwrap()["id"].as_str().unwrap().to_string();

    wait_until("AI auto_title populated on Run", || {
        let snap = s.json(&["drove", "show", &run_id]);
        snap["run"]["name"] == "Auth Middleware Cleanup"
            && snap["run"]["aiUsage"]["calls"].as_u64().unwrap_or(0) >= 1
    });

    stop.store(true, Ordering::Relaxed);
    srv.join().unwrap();
}

#[test]
fn drove_models_cache_survives_daemon_restart() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mock_port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();

    let srv = std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut conn, _)) => {
                    let mut buf = [0u8; 4096];
                    let n = conn.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let first = req.lines().next().unwrap_or("");
                    let body = if first.starts_with("GET /v1/models") {
                        r#"{"data":[{"id":"saved-model-a"},{"id":"saved-model-b"}]}"#
                    } else if first.starts_with("POST /v1/chat/completions") {
                        r#"{"model":"saved-model-a","choices":[{"message":{"role":"assistant","content":"OK"}}]}"#
                    } else {
                        r#"{}"#
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = conn.write_all(resp.as_bytes());
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    let mut s = drove_session();
    let bin_dir = s.root.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let wrapper = bin_dir.join("codex-acp");
    std::fs::write(&wrapper, format!("#!/bin/sh\nexec python3 \"{}\" \"$@\"\n", fake_acp_path())).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    s.env.push(("PATH".into(), format!("{}:/usr/bin:/bin", bin_dir.display())));
    let req_file = s.root.join("ai-save-cfg.json");
    std::fs::write(
        &req_file,
        serde_json::to_vec(&json!({
            "ai": {
                "enabled": true,
                "baseUrl": format!("http://127.0.0.1:{mock_port}/v1"),
                "model": "saved-model-a",
                "apiKey": "sk-cache-test"
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let set_res = s.json(&["drove", "settings-set", "--file", req_file.to_str().unwrap(), "--consume"]);
    assert_eq!(set_res["aiStatus"]["availableModels"], json!(["saved-model-a", "saved-model-b"]));

    let probe_res = s.json(&["drove", "probe", "--refresh"]);
    assert_eq!(probe_res["fullProbe"], true, "expected fullProbe=true: {probe_res}");

    // Shut down mock server AND restart core daemon -> models must still load from models-cache.json!
    stop.store(true, Ordering::Relaxed);
    srv.join().unwrap();

    let _ = s.cmd().args(["server", "stop"]).output();
    let after = s.json(&["drove", "settings"]);
    assert_eq!(after["aiStatus"]["availableModels"], json!(["saved-model-a", "saved-model-b"]));
    assert_eq!(after["agents"]["fullProbe"], true, "expected persisted fullProbe after restart: {after}");
}

#[test]
fn drove_settings_persists_ai_policy_switches_and_codex_fast() {
    let s = drove_session();
    let initial = s.json(&["drove", "settings"]);
    let mut roles = initial["config"]["roles"].clone();
    roles["reviewer"]["primary"] = json!({
        "agent": "codex",
        "model": "gpt-6.1-sol",
        "effort": "high",
        "fast": true
    });

    let req_file = s.root.join("policies-and-fast.json");
    std::fs::write(
        &req_file,
        serde_json::to_vec(&json!({
            "roles": roles,
            "ai": {
                "verifyDigest": false,
                "handoverCompact": false,
                "autoTitle": false,
                "taskEnrich": false,
                "conflictPolicy": "auto_worktree",
                "questionMode": "auto_when_offline",
                "loopDetect": "steer_once"
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let updated = s.json(&["drove", "settings-set", "--file", req_file.to_str().unwrap(), "--consume"]);
    assert_eq!(updated["config"]["roles"]["reviewer"]["primary"]["fast"], true);
    assert_eq!(
        updated["aiStatus"]["switches"],
        json!({
            "verifyDigest": false,
            "handoverCompact": false,
            "autoTitle": false,
            "taskEnrich": false,
            "conflictPolicy": "auto_worktree",
            "questionMode": "auto_when_offline",
            "loopDetect": "steer_once"
        })
    );
}

#[test]
fn drove_chained_worktrees_inherit_dirty_merges_and_ignore_pycache() {
    let s = drove_session();
    let (port, _) = wait_web_port(&s);

    let req_file = s.root.join("chained-roles-cfg.json");
    let spec = fake_acp_spec();
    std::fs::write(
        &req_file,
        serde_json::to_vec(&json!({
            "roles": {
                "coder": {
                    "label": "Coder",
                    "access": "writable",
                    "primary": { "agent": spec, "effort": "high" },
                    "fallbacks": []
                },
                "tester": {
                    "label": "Tester",
                    "access": "scoped_write",
                    "allowedPaths": ["tests/**"],
                    "primary": { "agent": spec, "effort": "medium" },
                    "fallbacks": []
                },
                "writer": {
                    "label": "Writer",
                    "access": "scoped_write",
                    "allowedPaths": ["docs/**"],
                    "primary": { "agent": spec, "effort": "medium" },
                    "fallbacks": []
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    s.json(&["drove", "settings-set", "--file", req_file.to_str().unwrap(), "--consume"]);

    let repo = s.root.join("chained_repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    git(&["init"]);
    git(&["config", "user.email", "test@drove.local"]);
    git(&["config", "user.name", "Drove Test"]);
    std::fs::write(repo.join("src/lib.txt"), "skeleton\n").unwrap();
    std::fs::write(repo.join("docs/readme.txt"), "init\n").unwrap();
    git(&["add", "src/lib.txt", "docs/readme.txt"]);
    git(&["commit", "-m", "init"]);

    let plan_path = s.root.join("chained-wt-plan.yaml");
    std::fs::write(
        &plan_path,
        concat!(
            "objective: Chained worktrees\n",
            "maxConcurrency: 3\n",
            "tasks:\n",
            "  - key: impl\n",
            "    title: Implement\n",
            "    role: coder\n",
            "    isolation: worktree\n",
            "    spec:\n",
            "      target: src/lib.txt\n",
            "      change: |\n",
            "        SH:printf 'implemented\\n' > src/lib.txt && printf 'impl-doc\\n' > docs/readme.txt && herdr2 drove worker done --outcome done --summary \"impl ready\"\n",
            "      acceptance: impl done\n",
            "  - key: impl2\n",
            "    title: Implement v2\n",
            "    role: coder\n",
            "    deps: [impl]\n",
            "    isolation: worktree\n",
            "    spec:\n",
            "      target: src/lib.txt\n",
            "      change: |\n",
            "        SH:grep -q implemented src/lib.txt && printf 'implemented-v2\\n' > src/lib.txt && herdr2 drove worker done --outcome done --summary \"impl2 ready\"\n",
            "      acceptance: impl2 done\n",
            "  - key: tests\n",
            "    title: Tests\n",
            "    role: tester\n",
            "    deps: [impl2]\n",
            "    isolation: worktree\n",
            "    spec:\n",
            "      target: tests/unit.test.txt\n",
            "      change: |\n",
            "        SH:grep -q implemented-v2 src/lib.txt && mkdir -p src/__pycache__ tests && printf 'bytecode\\n' > src/__pycache__/lib.cpython-312.pyc && printf 'ok\\n' > tests/unit.test.txt && herdr2 drove worker done --outcome done --summary \"tests ok\"\n",
            "      acceptance: tests done\n",
            "  - key: docs\n",
            "    title: Docs\n",
            "    role: writer\n",
            "    deps: [impl]\n",
            "    isolation: worktree\n",
            "    spec:\n",
            "      target: docs/readme.txt\n",
            "      change: |\n",
            "        SH:grep -q impl-doc docs/readme.txt && printf 'writer-doc\\n' >> docs/readme.txt && herdr2 drove worker done --outcome done --summary \"docs ok\"\n",
            "      acceptance: docs done\n",
        ),
    )
    .unwrap();

    let planned = s.json(&[
        "drove",
        "plan",
        "--file",
        plan_path.to_str().unwrap(),
        "--cwd",
        repo.to_str().unwrap(),
    ]);
    let run_id = planned["run"]["id"].as_str().unwrap().to_string();

    let waited = s.json(&["drove", "wait", "--run", &run_id, "--timeout", "25"]);
    assert_eq!(waited["settled"], true, "chained wait: {waited}");

    let shown = s.json(&["drove", "show", &run_id]);
    let tasks = shown["tasks"].as_array().unwrap();
    for key in ["impl", "impl2", "tests", "docs"] {
        let task = tasks.iter().find(|t| t["key"] == key).unwrap();
        assert_eq!(task["status"], "succeeded", "task {key}: {task}");
    }

    assert_eq!(std::fs::read_to_string(repo.join("src/lib.txt")).unwrap(), "implemented-v2\n");
    assert_eq!(std::fs::read_to_string(repo.join("tests/unit.test.txt")).unwrap(), "ok\n");
    assert_eq!(std::fs::read_to_string(repo.join("docs/readme.txt")).unwrap(), "impl-doc\nwriter-doc\n");
    assert!(!repo.join("src/__pycache__").exists(), "ephemeral cache should not be merged");
    assert_eq!(git(&["rev-list", "--count", "HEAD"]).trim(), "1");

    let impl_id = tasks.iter().find(|t| t["key"] == "impl").unwrap()["id"]
        .as_str()
        .unwrap();
    let impl2_id = tasks.iter().find(|t| t["key"] == "impl2").unwrap()["id"]
        .as_str()
        .unwrap();
    assert_eq!(tasks.iter().find(|t| t["key"] == "impl2").unwrap()["deps"], json!([impl_id]));
    assert_eq!(tasks.iter().find(|t| t["key"] == "tests").unwrap()["deps"], json!([impl2_id]));
    assert_eq!(tasks.iter().find(|t| t["key"] == "docs").unwrap()["deps"], json!([impl_id]));

    let coder_workers: Vec<&Value> = shown["workers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["role"] == "coder")
        .collect();
    assert_eq!(coder_workers.len(), 1, "expected 1 reused coder worker: {coder_workers:?}");
    let impl2_att = shown["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["taskId"] == impl2_id)
        .unwrap();
    assert_eq!(impl2_att["reused"], true, "impl2 should reuse warm coder worker: {impl2_att}");

    for w in shown["workers"].as_array().unwrap() {
        assert_eq!(w["state"], "idle", "worktree worker should stay idle for reuse: {w}");
    }

    let web_tok = read_web_token(port);
    let trashed = http_req(
        port,
        "POST",
        &format!("/api/runs/{run_id}/trash"),
        &[("x-drove-token", &web_tok)],
        None,
    );
    assert_eq!(trashed.status, 200, "trash failed: {}", trashed.body);

    let shown_after_trash = s.json(&["drove", "show", &run_id]);
    for w in shown_after_trash["workers"].as_array().unwrap() {
        assert_eq!(w["state"], "closed", "worktree worker should close after trash: {w}");
        assert!(w["worktree"].is_null(), "worktree path should be cleared: {w}");
    }
    let wt_list = git(&["worktree", "list", "--porcelain"]);
    assert_eq!(
        wt_list.lines().filter(|l| l.starts_with("worktree ")).count(),
        1,
        "stale worktree remained: {wt_list}"
    );
}

#[test]
fn drove_retry_reopens_skipped_dependents_and_applies_agent_override() {
    let mut s = drove_session();
    let bin_dir = s.root.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let wrapper = bin_dir.join("codex-acp");
    std::fs::write(&wrapper, format!("#!/bin/sh\nexec python3 \"{}\" \"$@\"\n", fake_acp_path())).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    s.env.push(("PATH".into(), format!("{}:/usr/bin:/bin", bin_dir.display())));

    let _ = wait_web_port(&s);
    configure_fake_coder(&s);

    let work_dir = s.root.join("retry_skipped_work");
    std::fs::create_dir_all(&work_dir).unwrap();
    let allow_file = s.root.join("allow.ok");

    let plan_path = s.root.join("retry-skipped-plan.yaml");
    let allow_str = allow_file.display().to_string();
    std::fs::write(
        &plan_path,
        format!(
            concat!(
                "objective: Reopen skipped dependents on retry\n",
                "tasks:\n",
                "  - key: step1\n",
                "    title: Step One\n",
                "    role: coder\n",
                "    spec:\n",
                "      target: step1.txt\n",
                "      change: |\n",
                "        SH:if [ -f \"{allow}\" ]; then herdr2 drove worker done --outcome done --summary \"step1 ok\"; else herdr2 drove worker done --outcome failed --no-retry --reason \"not ready\"; fi\n",
                "      acceptance: step1 done\n",
                "  - key: step2\n",
                "    title: Step Two\n",
                "    role: coder\n",
                "    deps: [step1]\n",
                "    spec:\n",
                "      target: step2.txt\n",
                "      change: |\n",
                "        SH:herdr2 drove worker done --outcome done --summary \"step2 ok\"\n",
                "      acceptance: step2 done\n",
            ),
            allow = allow_str,
        ),
    )
    .unwrap();

    let planned = s.json(&[
        "drove",
        "plan",
        "--file",
        plan_path.to_str().unwrap(),
        "--cwd",
        work_dir.to_str().unwrap(),
    ]);
    let run_id = planned["run"]["id"].as_str().unwrap().to_string();

    let waited1 = s.json(&["drove", "wait", "--run", &run_id, "--timeout", "15"]);
    assert_eq!(waited1["settled"], true, "initial failure wait: {waited1}");
    let show1 = s.json(&["drove", "show", &run_id]);
    let tasks1 = show1["tasks"].as_array().unwrap();
    assert_eq!(tasks1.iter().find(|t| t["key"] == "step1").unwrap()["status"], "failed");
    assert_eq!(tasks1.iter().find(|t| t["key"] == "step2").unwrap()["status"], "skipped");

    std::fs::write(&allow_file, "ok\n").unwrap();
    let retried = s.json(&["drove", "retry", "step1", "--run", &run_id, "--agent", "codex", "--hint", "marker exists now"]);
    assert_eq!(retried["assign"]["source"], "user", "retry override must mark source=user: {retried}");
    assert_eq!(retried["assign"]["agent"], "codex");

    let waited2 = s.json(&["drove", "wait", "--run", &run_id, "--timeout", "15"]);
    assert_eq!(waited2["settled"], true, "retried wait: {waited2}");
    let show2 = s.json(&["drove", "show", &run_id]);
    let tasks2 = show2["tasks"].as_array().unwrap();
    let s1 = tasks2.iter().find(|t| t["key"] == "step1").unwrap();
    let s2 = tasks2.iter().find(|t| t["key"] == "step2").unwrap();
    assert_eq!(s1["status"], "succeeded", "step1 after retry: {s1}");
    assert_eq!(s2["status"], "succeeded", "step2 should reopen from skipped and succeed: {s2}");

    let s1_id = s1["id"].as_str().unwrap();
    let latest_s1_att = show2["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["taskId"] == s1_id)
        .last()
        .unwrap();
    assert_eq!(latest_s1_att["agent"], "codex", "retried attempt should use overridden agent: {latest_s1_att}");
}

#[test]
fn drove_coordinator_exposes_agent_model_and_host_transcript() {
    let mut s = drove_session();
    let bin_dir = s.root.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let wrapper = bin_dir.join("codex-acp");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\nexec python3 \"{}\" \"$@\"\n", fake_acp_path()),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    s.env.push(("PATH".into(), format!("{}:/usr/bin:/bin", bin_dir.display())));

    let (port, key_file) = wait_web_port(&s);
    let coord_secret = read_coordinator_secret(&key_file);

    let coord = s.json(&[
        "run",
        "--agent",
        "codex",
        "--model",
        "gpt-5.4",
        "--prompt",
        "ECHO:coordinator ready",
    ]);
    let coord_addr = coord["address"].as_str().unwrap().to_string();

    let created = http_req(
        port,
        "POST",
        "/api/runs",
        &[
            ("x-drove-coordinator-secret", &coord_secret),
            ("x-drove-coordinator", &coord_addr),
        ],
        Some(&json!({
            "hostSession": coord_addr,
            "objective": "Verify coordinator metadata & transcript",
            "cwd": "/tmp"
        })),
    );
    assert_eq!(created.status, 200, "create run failed: {}", created.body);
    let run_id = created.json.as_ref().unwrap()["id"].as_str().unwrap().to_string();

    let snap = http_req(port, "GET", &format!("/api/runs/{run_id}"), &[], None);
    assert_eq!(snap.status, 200, "snapshot failed: {}", snap.body);
    let snap_json = snap.json.unwrap();
    assert_eq!(snap_json["run"]["coordinator"]["agent"], "codex", "{snap_json}");
    assert_eq!(snap_json["run"]["coordinator"]["model"], "gpt-5.4", "{snap_json}");

    let tr = http_req(
        port,
        "GET",
        &format!("/api/runs/{run_id}/coordinator-transcript?limit=20"),
        &[],
        None,
    );
    assert_eq!(tr.status, 200, "transcript failed: {}", tr.body);
    let tr_json = tr.json.unwrap();
    assert_eq!(tr_json["available"], true, "{tr_json}");
    assert_eq!(tr_json["agent"], "codex", "{tr_json}");
    assert_eq!(tr_json["models"], json!(["gpt-5.4"]), "{tr_json}");
    let items = tr_json["items"].as_array().unwrap();
    assert!(!items.is_empty(), "expected host transcript items: {tr_json}");
    assert!(items.iter().all(|it| it["index"].is_u64() && it["kind"].is_string()), "{tr_json}");
    assert!(
        items.iter().any(|it| it["kind"] == "assistant"
            && it["text"].as_str().unwrap_or("").contains("coordinator ready")),
        "expected assistant text in coordinator transcript: {tr_json}"
    );
}

#[test]
fn drove_reviewer_fallback_and_warm_reuse_across_tasks_and_complete_guard() {
    let s = drove_session();
    let (port, _) = wait_web_port(&s);

    let acp_stub = |name: &str, reply_text: &str| -> String {
        let p = s.root.join(name);
        let script = format!(
            r#"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    rid = req.get("id")
    m = req.get("method", "")
    if m == "initialize":
        resp = {{"jsonrpc":"2.0","id":rid,"result":{{"protocolVersion":1,"agentCapabilities":{{"loadSession":False}}}}}}
    elif m == "session/new":
        resp = {{"jsonrpc":"2.0","id":rid,"result":{{"sessionId":"s1"}}}}
    elif m == "session/prompt":
        sid = req.get("params", {{}}).get("sessionId", "s1")
        sys.stdout.write(json.dumps({{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":sid,"update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":{reply_json}}}}}}}}}) + "\n")
        resp = {{"jsonrpc":"2.0","id":rid,"result":{{"stopReason":"end_turn"}}}}
    else:
        resp = {{"jsonrpc":"2.0","id":rid,"result":{{}}}}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#,
            reply_json = serde_json::to_string(reply_text).unwrap(),
        );
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        format!("python3 {}", p.display())
    };

    let bad_rev_spec = acp_stub(
        "bad_reviewer.py",
        r#"{"error":{"type":"invalid_request_error","message":"The 'gpt-6.2-sol' model is not supported","code":"model_not_found"}}"#,
    );
    let ok_rev_spec = acp_stub(
        "ok_reviewer.py",
        r#"{"verdict":"approve","summary":"Reviewed cleanly","findings":[]}"#,
    );

    let coder_spec = fake_acp_spec();
    let cfg_file = s.root.join("rev-reuse-cfg.json");
    std::fs::write(
        &cfg_file,
        serde_json::to_vec(&json!({
            "roles": {
                "coder": {
                    "label": "Coder",
                    "access": "writable",
                    "primary": { "agent": coder_spec, "effort": "high" },
                    "fallbacks": []
                },
                "reviewer": {
                    "label": "Reviewer",
                    "access": "readonly",
                    "primary": { "agent": bad_rev_spec, "effort": "high" },
                    "fallbacks": [{ "agent": ok_rev_spec, "effort": "high" }]
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    s.json(&["drove", "settings-set", "--file", cfg_file.to_str().unwrap(), "--consume"]);

    let work_dir = s.root.join("rev_repo");
    std::fs::create_dir_all(&work_dir).unwrap();
    std::fs::write(work_dir.join("a.txt"), "init\n").unwrap();
    for args in [
        &["init"][..],
        &["config", "user.email", "t@drove.local"][..],
        &["config", "user.name", "Drove Test"][..],
        &["add", "."][..],
        &["commit", "-m", "init"][..],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&work_dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    let plan_path = s.root.join("rev-reuse-plan.yaml");
    std::fs::write(
        &plan_path,
        concat!(
            "objective: Reviewer fallback, warm reuse, wait after fail, and complete guard\n",
            "maxConcurrency: 3\n",
            "tasks:\n",
            "  - key: rev_t1\n",
            "    title: Reviewed Task 1\n",
            "    role: coder\n",
            "    isolation: worktree\n",
            "    review:\n",
            "      maxRounds: 1\n",
            "      differentAgent: true\n",
            "    spec:\n",
            "      target: a.txt\n",
            "      change: |\n",
            "        SH:printf 'v1\\n' > a.txt && herdr2 drove worker done --outcome done --summary \"t1 ready\"\n",
            "      acceptance: t1 done\n",
            "  - key: rev_t2\n",
            "    title: Reviewed Task 2\n",
            "    role: coder\n",
            "    deps: [rev_t1]\n",
            "    isolation: worktree\n",
            "    review:\n",
            "      maxRounds: 1\n",
            "      differentAgent: true\n",
            "    spec:\n",
            "      target: a.txt\n",
            "      change: |\n",
            "        SH:printf 'v2\\n' > a.txt && herdr2 drove worker done --outcome done --summary \"t2 ready\"\n",
            "      acceptance: t2 done\n",
            "  - key: fail_leaf\n",
            "    title: Intentional Failure\n",
            "    role: coder\n",
            "    deps: [rev_t1]\n",
            "    spec:\n",
            "      target: b.txt\n",
            "      change: |\n",
            "        SH:herdr2 drove worker done --outcome failed --no-retry --reason \"intentional\"\n",
            "      acceptance: fails\n",
            "  - key: sibling_done\n",
            "    title: Sibling Task\n",
            "    role: coder\n",
            "    deps: [rev_t1]\n",
            "    spec:\n",
            "      target: c.txt\n",
            "      change: |\n",
            "        SH:sleep 1.2 && printf 'sib\\n' > c.txt && herdr2 drove worker done --outcome done --summary \"sib ok\"\n",
            "      acceptance: sib ok\n",
        ),
    )
    .unwrap();

    let planned = s.json(&[
        "drove",
        "plan",
        "--file",
        plan_path.to_str().unwrap(),
        "--cwd",
        work_dir.to_str().unwrap(),
    ]);
    let run_id = planned["run"]["id"].as_str().unwrap().to_string();

    // Wait until fail_leaf triggers a `failed` message, ack it, then verify `wait` still blocks until sibling/rev_t2 settle.
    let mut seq = 0u64;
    let mut settled = false;
    for _ in 0..10 {
        let w = s.json(&["drove", "wait", "--run", &run_id, "--timeout", "15", "--since-seq", &seq.to_string()]);
        for m in w["messages"].as_array().into_iter().flatten() {
            let mseq = m["seq"].as_u64().unwrap_or(0);
            seq = seq.max(mseq);
            let mid = m["id"].as_str().unwrap();
            let _ = s.json(&["drove", "ack", mid]);
        }
        if w["settled"] == true {
            settled = true;
            break;
        }
        assert!(
            w["timedOut"] != true,
            "drove wait timed out before settling after acking failure: {w}"
        );
    }
    assert!(settled, "run did not settle");

    let shown = s.json(&["drove", "show", &run_id]);
    let tasks = shown["tasks"].as_array().unwrap();
    let t1 = tasks.iter().find(|t| t["key"] == "rev_t1").unwrap();
    let t2 = tasks.iter().find(|t| t["key"] == "rev_t2").unwrap();
    let fl = tasks.iter().find(|t| t["key"] == "fail_leaf").unwrap();
    let sb = tasks.iter().find(|t| t["key"] == "sibling_done").unwrap();
    assert_eq!(t1["status"], "succeeded", "{t1}");
    assert_eq!(t1["failures"], 0);
    assert_eq!(t2["status"], "succeeded", "{t2}");
    assert_eq!(t2["failures"], 0);
    assert_eq!(fl["status"], "failed", "{fl}");
    assert_eq!(sb["status"], "succeeded", "{sb}");

    let attempts = shown["attempts"].as_array().unwrap();
    let rev_atts: Vec<&Value> = attempts.iter().filter(|a| a["kind"] == "review").collect();
    let failed_revs: Vec<&&Value> = rev_atts.iter().filter(|a| a["outcome"] == "failed").collect();
    assert_eq!(failed_revs.len(), 1, "broken primary should fail once on t1 and be deprioritized on t2: {rev_atts:?}");
    assert_eq!(failed_revs[0]["agent"], bad_rev_spec);
    assert_eq!(failed_revs[0]["failure"]["kind"], "agent_error");

    let ok_revs: Vec<&&Value> = rev_atts.iter().filter(|a| a["outcome"] == "succeeded").collect();
    assert_eq!(ok_revs.len(), 2, "both t1 and t2 must succeed review via fallback: {rev_atts:?}");
    assert_eq!(ok_revs[0]["reused"], false);
    assert_eq!(ok_revs[1]["reused"], true, "t2 should warm-reuse t1 reviewer worker: {ok_revs:?}");
    assert_eq!(ok_revs[0]["workerId"], ok_revs[1]["workerId"]);

    let rev_workers: Vec<&Value> = shown["workers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["agent"] == ok_rev_spec)
        .collect();
    assert_eq!(rev_workers.len(), 1, "expected 1 reused ok reviewer worker: {rev_workers:?}");

    // Complete without --force must reject because fail_leaf is failed; with --force it succeeds and keeps healthy workers idle until run is trashed/closed.
    let err_out = s.cmd().args(["--json", "drove", "complete", &run_id]).output().unwrap();
    assert!(!err_out.status.success(), "drove complete without --force must reject when a task is failed");
    let forced = s.json(&["drove", "complete", &run_id, "--force", "--summary", "done with force"]);
    assert_eq!(forced["run"]["status"], "done");
    let after_complete = http_req(port, "GET", &format!("/api/runs/{run_id}"), &[], None).json.unwrap();
    for w in after_complete["workers"].as_array().unwrap() {
        if w["agent"] == bad_rev_spec {
            assert_eq!(w["state"], "dead", "broken reviewer should be dead: {w}");
        } else {
            assert_eq!(w["state"], "idle", "healthy worker should stay idle on complete: {w}");
        }
    }

    let web_tok = read_web_token(port);
    let trashed = http_req(
        port,
        "POST",
        &format!("/api/runs/{run_id}/trash"),
        &[("x-drove-token", &web_tok)],
        None,
    );
    assert_eq!(trashed.status, 200, "trash failed: {}", trashed.body);
    let final_snap = http_req(port, "GET", &format!("/api/runs/{run_id}"), &[], None).json.unwrap();
    for w in final_snap["workers"].as_array().unwrap() {
        assert!(
            matches!(w["state"].as_str(), Some("closed" | "dead")),
            "worker should be closed or dead after run trash: {w}"
        );
    }
}

#[test]
fn drove_auto_worktree_isolates_parallel_scoped_writers_and_foreach_template_edges() {
    let s = drove_session();
    let (port, _) = wait_web_port(&s);

    let spec = fake_acp_spec();
    let cfg_file = s.root.join("autowt-cfg.json");
    std::fs::write(
        &cfg_file,
        serde_json::to_vec(&json!({
            "ai": {
                "enabled": false,
                "conflictPolicy": "auto_worktree"
            },
            "roles": {
                "researcher": {
                    "label": "Researcher",
                    "access": "readonly",
                    "primary": { "agent": spec, "effort": "high" },
                    "fallbacks": []
                },
                "coder": {
                    "label": "Coder",
                    "access": "writable",
                    "primary": { "agent": spec, "effort": "high" },
                    "fallbacks": []
                },
                "tester": {
                    "label": "Tester",
                    "access": "scoped_write",
                    "allowedPaths": ["tests/**"],
                    "primary": { "agent": spec, "effort": "high" },
                    "fallbacks": []
                },
                "writer": {
                    "label": "Writer",
                    "access": "scoped_write",
                    "allowedPaths": ["docs/**"],
                    "primary": { "agent": spec, "effort": "medium" },
                    "fallbacks": []
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    s.json(&["drove", "settings-set", "--file", cfg_file.to_str().unwrap(), "--consume"]);

    let repo = s.root.join("aw_repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("tests")).unwrap();
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    std::fs::write(repo.join("src/base.txt"), "base\n").unwrap();
    std::fs::write(repo.join("tests/t.txt"), "t0\n").unwrap();
    std::fs::write(repo.join("docs/d.txt"), "d0\n").unwrap();
    for args in [
        &["init"][..],
        &["config", "user.email", "t@drove.local"][..],
        &["config", "user.name", "Drove Test"][..],
        &["add", "."][..],
        &["commit", "-m", "init"][..],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }

    let plan_path = s.root.join("aw-plan.yaml");
    std::fs::write(
        &plan_path,
        concat!(
            "objective: Verify foreachFrom topology and conflictPolicy=auto_worktree\n",
            "maxConcurrency: 4\n",
            "tasks:\n",
            "  - key: discover\n",
            "    title: Discover items\n",
            "    role: researcher\n",
            "    outputSchema:\n",
            "      type: object\n",
            "      required: [items]\n",
            "      properties:\n",
            "        items:\n",
            "          type: array\n",
            "          items:\n",
            "            type: string\n",
            "    spec:\n",
            "      target: src/base.txt\n",
            "      change: |\n",
            "        SH:herdr2 drove worker done --outcome done --summary \"found items\" --data '{\"items\":[\"a\",\"b\"]}'\n",
            "      acceptance: emits items array\n",
            "  - key: fan\n",
            "    title: Generate modules\n",
            "    role: coder\n",
            "    deps: [discover]\n",
            "    isolation: worktree\n",
            "    foreachFrom:\n",
            "      task: discover\n",
            "      path: items\n",
            "    spec:\n",
            "      target: \"src/{{item}}.txt\"\n",
            "      change: |\n",
            "        SH:printf '%s\\n' \"{{item}}\" > \"src/{{item}}.txt\" && herdr2 drove worker done --outcome done --summary \"wrote {{item}}\"\n",
            "      acceptance: item file written\n",
            "  - key: tests\n",
            "    title: Run tests\n",
            "    role: tester\n",
            "    deps: [fan]\n",
            "    spec:\n",
            "      target: tests/t.txt\n",
            "      change: |\n",
            "        SH:grep -q a src/a.txt && grep -q b src/b.txt && sleep 0.6 && printf 't1\\n' > tests/t.txt && herdr2 drove worker done --outcome done --summary \"tests ok\"\n",
            "      acceptance: tests ok\n",
            "  - key: docs\n",
            "    title: Write docs\n",
            "    role: writer\n",
            "    deps: [fan]\n",
            "    spec:\n",
            "      target: docs/d.txt\n",
            "      change: |\n",
            "        SH:grep -q a src/a.txt && grep -q b src/b.txt && sleep 0.6 && printf 'd1\\n' > docs/d.txt && herdr2 drove worker done --outcome done --summary \"docs ok\"\n",
            "      acceptance: docs ok\n",
        ),
    )
    .unwrap();

    let planned = s.json(&[
        "drove",
        "plan",
        "--file",
        plan_path.to_str().unwrap(),
        "--cwd",
        repo.to_str().unwrap(),
    ]);
    let run_id = planned["run"]["id"].as_str().unwrap().to_string();

    let waited = s.json(&["drove", "wait", "--run", &run_id, "--timeout", "25"]);
    assert_eq!(waited["settled"], true, "auto_worktree wait: {waited}");

    let snap = http_req(port, "GET", &format!("/api/runs/{run_id}"), &[], None).json.unwrap();
    let tasks = snap["tasks"].as_array().unwrap();
    let fan = tasks.iter().find(|t| t["key"] == "fan").unwrap();
    let fan1 = tasks.iter().find(|t| t["key"] == "fan#1").unwrap();
    let fan2 = tasks.iter().find(|t| t["key"] == "fan#2").unwrap();
    let tests_t = tasks.iter().find(|t| t["key"] == "tests").unwrap();
    let docs_t = tasks.iter().find(|t| t["key"] == "docs").unwrap();

    assert_eq!(fan1["deps"], json!([fan["id"]]), "fan#1 must connect from template node: {fan1}");
    assert_eq!(fan2["deps"], json!([fan["id"]]), "fan#2 must connect from template node: {fan2}");
    assert_eq!(tests_t["deps"], json!([fan1["id"], fan2["id"]]), "tests must depend on fan children: {tests_t}");
    assert_eq!(docs_t["deps"], json!([fan1["id"], fan2["id"]]), "docs must depend on fan children: {docs_t}");

    assert_eq!(tests_t["status"], "succeeded", "{tests_t}");
    assert_eq!(docs_t["status"], "succeeded", "{docs_t}");
    assert_eq!(tests_t["failures"], 0);
    assert_eq!(docs_t["failures"], 0);
    assert_eq!(tests_t["isolation"], "worktree");
    assert_eq!(docs_t["isolation"], "worktree");

    let ev_resp = http_req(
        port,
        "GET",
        &format!("/api/daemon/events?runId={run_id}&category=schedule"),
        &[],
        None,
    )
    .json
    .unwrap();
    let auto_events: Vec<&Value> = ev_resp["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "auto_worktree")
        .collect();
    assert!(!auto_events.is_empty(), "expected auto_worktree schedule events: {ev_resp}");
    assert_eq!(std::fs::read_to_string(repo.join("tests/t.txt")).unwrap(), "t1\n");
    assert_eq!(std::fs::read_to_string(repo.join("docs/d.txt")).unwrap(), "d1\n");
}

#[test]
fn drove_ai_model_params_persist_clear_and_fallback_when_options_rejected() {
    use std::sync::Mutex;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mock_port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let seen_bodies: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));

    let stop_srv = stop.clone();
    let bodies_srv = seen_bodies.clone();
    let srv = std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut conn, _)) => {
                    let mut buf = [0u8; 16384];
                    let n = conn.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let first = req.lines().next().unwrap_or("");
                    let body_str = req.split_once("\r\n\r\n").map(|x| x.1).unwrap_or("");
                    let (status_line, resp_body) = if first.starts_with("GET /v1/models") {
                        ("200 OK", r#"{"data":[{"id":"mock-params-model"}]}"#)
                    } else if first.starts_with("POST /v1/chat/completions") {
                        let parsed: Value = serde_json::from_str(body_str).unwrap_or(Value::Null);
                        bodies_srv.lock().unwrap().push(parsed.clone());
                        if parsed.get("options").is_some() {
                            ("400 Bad Request", r#"{"error":{"message":"Unknown field: options"}}"#)
                        } else {
                            (
                                "200 OK",
                                r#"{"usage":{"prompt_tokens":12,"completion_tokens":4},"choices":[{"message":{"role":"assistant","content":"Model Params Verified"}}]}"#,
                            )
                        }
                    } else {
                        ("200 OK", r#"{}"#)
                    };
                    let resp = format!(
                        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp_body.len(),
                        resp_body
                    );
                    let _ = conn.write_all(resp.as_bytes());
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    let s = drove_session();
    let (port, key_file) = wait_web_port(&s);
    let coord_secret = read_coordinator_secret(&key_file);

    let set_file = s.root.join("ai-params-set.json");
    std::fs::write(
        &set_file,
        serde_json::to_vec(&json!({
            "ai": {
                "enabled": true,
                "baseUrl": format!("http://127.0.0.1:{mock_port}/v1"),
                "model": "mock-params-model",
                "effort": "high",
                "contextWindow": "64k",
                "maxTokens": 3072,
                "autoTitle": true
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let updated = s.json(&["drove", "settings-set", "--file", set_file.to_str().unwrap(), "--consume"]);
    assert_eq!(updated["config"]["ai"]["effort"], "high");
    assert_eq!(updated["config"]["ai"]["contextWindow"], "64k");
    assert_eq!(updated["config"]["ai"]["maxTokens"], 3072);
    assert_eq!(updated["aiStatus"]["effort"], "high");
    assert_eq!(updated["aiStatus"]["contextWindow"], "64k");
    assert_eq!(updated["aiStatus"]["maxTokens"], 3072);

    let created = http_req(
        port,
        "POST",
        "/api/runs",
        &[
            ("x-drove-coordinator-secret", &coord_secret),
            ("x-drove-coordinator", "t1"),
        ],
        Some(&json!({ "hostSession": "t1", "objective": "Verify AI model parameter propagation", "cwd": "/tmp" })),
    );
    assert_eq!(created.status, 200);
    let run_id = created.json.unwrap()["id"].as_str().unwrap().to_string();

    wait_until("AI auto_title completes after fallback stripping options", || {
        let snap = s.json(&["drove", "show", &run_id]);
        snap["run"]["name"] == "Model Params Verified"
    });

    let bodies = seen_bodies.lock().unwrap().clone();
    assert!(bodies.len() >= 2, "expected initial attempt + fallback attempt, got: {bodies:?}");
    assert_eq!(bodies[0]["options"]["num_ctx"], 65536);
    assert_eq!(bodies[0]["reasoning_effort"], "high");
    assert_eq!(bodies[0]["max_tokens"], 3072);
    assert!(bodies[1].get("options").is_none());
    assert_eq!(bodies[1]["reasoning_effort"], "high");
    assert_eq!(bodies[1]["max_tokens"], 3072);

    let clear_file = s.root.join("ai-params-clear.json");
    std::fs::write(
        &clear_file,
        serde_json::to_vec(&json!({
            "ai": {
                "effort": "auto",
                "contextWindow": "",
                "maxTokens": 0
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let cleared = s.json(&["drove", "settings-set", "--file", clear_file.to_str().unwrap(), "--consume"]);
    assert_eq!(cleared["config"]["ai"]["effort"], "auto");
    assert!(cleared["config"]["ai"].get("contextWindow").is_none());
    assert!(cleared["config"]["ai"].get("maxTokens").is_none());
    assert_eq!(cleared["aiStatus"]["effort"], "auto");
    assert!(cleared["aiStatus"].get("contextWindow").is_none());
    assert!(cleared["aiStatus"].get("maxTokens").is_none());

    stop.store(true, Ordering::Relaxed);
    srv.join().unwrap();
}

#[test]
fn drove_subagents_close_on_coordinator_trash_or_exit_and_web_delete() {
    let s = drove_session();
    let (port, _) = wait_web_port(&s);
    configure_fake_coder(&s);

    let work_dir = s.root.join("lifecycle_work");
    std::fs::create_dir_all(&work_dir).unwrap();

    // 1. Trashing the coordinator in herdr-app (`herdr.trashed` label) closes its idle Drove sub-agents.
    let coord1 = run_fake(&s, &[]);
    let out1 = s
        .cmd()
        .env("H2_AGENT_ID", &coord1)
        .args([
            "--json",
            "drove",
            "quick",
            "--cwd",
            work_dir.to_str().unwrap(),
            "SH:herdr2 drove worker done --outcome done --summary \"run1 ok\"",
        ])
        .output()
        .unwrap();
    assert!(out1.status.success(), "drove quick 1 failed: {}", String::from_utf8_lossy(&out1.stderr));
    let started1: Value = serde_json::from_slice(&out1.stdout).unwrap();
    let run1 = started1["run"]["id"].as_str().unwrap().to_string();

    let waited1 = s.json(&["drove", "wait", "--run", &run1, "--timeout", "15"]);
    assert_eq!(waited1["settled"], true);
    let snap1 = s.json(&["drove", "show", &run1]);
    let w1_addr = snap1["workers"][0]["h2Agent"].as_str().unwrap().to_string();
    assert_eq!(snap1["workers"][0]["state"], "idle");
    assert_eq!(s.json(&["get", &w1_addr])["state"], "live");

    s.json(&["label", &coord1, "herdr.trashed=123456"]);
    wait_until("worker finished when coordinator trashed", || {
        s.json(&["get", &w1_addr])["state"] == "finished"
    });
    wait_until("drove worker closed when coordinator trashed", || {
        s.json(&["drove", "show", &run1])["workers"][0]["state"] == "closed"
    });

    // 2. Web UI run trash + permanent delete closes idle Drove sub-agents and removes orphan agents.
    let coord2 = run_fake(&s, &[]);
    let out2 = s
        .cmd()
        .env("H2_AGENT_ID", &coord2)
        .args([
            "--json",
            "drove",
            "quick",
            "--cwd",
            work_dir.to_str().unwrap(),
            "SH:herdr2 drove worker done --outcome done --summary \"run2 ok\"",
        ])
        .output()
        .unwrap();
    assert!(out2.status.success(), "drove quick 2 failed: {}", String::from_utf8_lossy(&out2.stderr));
    let started2: Value = serde_json::from_slice(&out2.stdout).unwrap();
    let run2 = started2["run"]["id"].as_str().unwrap().to_string();

    let waited2 = s.json(&["drove", "wait", "--run", &run2, "--timeout", "15"]);
    assert_eq!(waited2["settled"], true);
    let snap2 = s.json(&["drove", "show", &run2]);
    let w2_addr = snap2["workers"][0]["h2Agent"].as_str().unwrap().to_string();
    assert_eq!(snap2["workers"][0]["state"], "idle");
    assert_eq!(s.json(&["get", &w2_addr])["state"], "live");

    let web_tok = read_web_token(port);
    let trash_resp = http_req(
        port,
        "POST",
        &format!("/api/runs/{run2}/trash"),
        &[("x-drove-token", &web_tok)],
        None,
    );
    assert_eq!(trash_resp.status, 200, "web trash failed: {}", trash_resp.body);
    assert_eq!(s.json(&["drove", "show", &run2])["workers"][0]["state"], "closed");
    wait_until("worker finished when run trashed in web", || {
        s.json(&["get", &w2_addr])["state"] == "finished"
    });

    let del_resp = http_req(
        port,
        "DELETE",
        &format!("/api/runs/{run2}"),
        &[("x-drove-token", &web_tok), ("x-drove-confirm", &run2)],
        None,
    );
    assert_eq!(del_resp.status, 200, "web delete failed: {}", del_resp.body);
    let missing_resp = http_req(port, "GET", &format!("/api/runs/{run2}"), &[], None);
    assert_eq!(missing_resp.status, 404);
    let ls_live = s.json(&["ls"]);
    assert!(
        !ls_live["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["address"] == w2_addr),
        "deleted run left live agent {w2_addr}: {ls_live}"
    );
}

#[test]
fn drove_roles_ai_configure_and_strengths_persist_end_to_end() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mock_port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();

    let s = drove_session();
    // Pre-seed a probed agent list by checking whatever agents are discovered in this test session.
    let initial = s.json(&["drove", "settings"]);
    let probed_agents = initial["agents"]["agents"].as_array().cloned().unwrap_or_default();
    let any_installed = probed_agents.iter().any(|a| a["installed"] == true);
    let pick_agent = |idx: usize| -> String {
        probed_agents
            .iter()
            .filter(|a| !any_installed || a["installed"] == true)
            .nth(idx)
            .or_else(|| probed_agents.iter().find(|a| !any_installed || a["installed"] == true))
            .and_then(|a| a["id"].as_str())
            .unwrap_or("claude")
            .to_string()
    };
    let agent_a = pick_agent(0);
    let agent_b = pick_agent(1);

    let ai_reply_json = json!({
        "rationale": "按架构与前端分工配置候选",
        "roles": {
            "coder": {
                "candidates": [
                    { "agent": agent_a, "model": null, "effort": "high", "strengths": ["后端架构", "复杂重构"] },
                    { "agent": agent_b, "model": null, "effort": "medium", "strengths": ["前端UI", "快速修复"] }
                ]
            }
        }
    })
    .to_string();

    let srv = std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut conn, _)) => {
                    let mut buf = [0u8; 8192];
                    let n = conn.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let first_line = req.lines().next().unwrap_or("");
                    let body = if first_line.starts_with("GET /v1/models") {
                        json!({"data":[{"id":"mock-planner-model"}]}).to_string()
                    } else if first_line.starts_with("POST /v1/chat/completions") {
                        json!({
                            "id": "chatcmpl-roles",
                            "choices": [{"message": {"role": "assistant", "content": ai_reply_json}}]
                        })
                        .to_string()
                    } else {
                        r#"{"error":"not_found"}"#.to_string()
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = conn.write_all(resp.as_bytes());
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    let req_file = s.root.join("ai-roles.json");
    std::fs::write(
        &req_file,
        serde_json::to_vec(&json!({
            "baseUrl": format!("http://127.0.0.1:{mock_port}/v1"),
            "instruction": "后端主力+前端分工",
            "apply": true,
            "language": "zh-CN"
        }))
        .unwrap(),
    )
    .unwrap();

    let res = s.json(&[
        "drove",
        "roles-ai-config",
        "--file",
        req_file.to_str().unwrap(),
        "--consume",
    ]);
    stop.store(true, Ordering::Relaxed);
    let _ = srv.join();

    assert_eq!(res["ok"], true, "roles-ai-config failed: {res}");
    assert_eq!(res["applied"], true);
    assert_eq!(res["modelUsed"], "mock-planner-model");
    assert_eq!(res["rationale"], "按架构与前端分工配置候选");

    // Verify persisted in drove settings & exposed via /api/agents.
    let updated = s.json(&["drove", "settings"]);
    let coder = &updated["config"]["roles"]["coder"];
    assert_eq!(coder["primary"]["agent"], agent_a);
    assert_eq!(coder["primary"]["strengths"], json!(["后端架构", "复杂重构"]));
    if agent_b != agent_a {
        assert_eq!(coder["fallbacks"][0]["agent"], agent_b);
        assert_eq!(coder["fallbacks"][0]["strengths"], json!(["前端UI", "快速修复"]));
    }

    let port = updated["webPort"].as_u64().unwrap() as u16;
    let agents_resp = http_req(port, "GET", "/api/agents", &[], None);
    assert_eq!(agents_resp.status, 200);
    let agents_json: Value = serde_json::from_str(&agents_resp.body).unwrap();
    assert_eq!(
        agents_json["profile"]["roles"]["coder"]["candidates"][0]["strengths"],
        json!(["后端架构", "复杂重构"])
    );
}

#[test]
fn core_ai_cli_and_worktree_preflight_guard() {
    let s = drove_session();
    let (port, key_file) = wait_web_port(&s);
    let coord_tok = read_coordinator_secret(&key_file);

    // 1. Shared Core AI CLI (`h2 ai status` + `h2 ai settings-set`).
    let ai_status = s.json(&["ai", "status"]);
    assert_eq!(ai_status["switches"]["ptyStatusArbitration"], true);
    assert_eq!(ai_status["switches"]["notesDigest"], true);
    assert_eq!(ai_status["switches"]["promptCacheEnabled"], true);
    assert!(ai_status["usage"].is_object());

    let ai_patch = s.root.join("ai-patch.json");
    std::fs::write(
        &ai_patch,
        serde_json::to_vec(&json!({
            "ptyStatusArbitration": false,
            "notesDigest": true,
            "effort": "low"
        }))
        .unwrap(),
    )
    .unwrap();
    let updated_ai = s.json(&["ai", "settings-set", "--file", ai_patch.to_str().unwrap(), "--consume"]);
    assert_eq!(updated_ai["switches"]["ptyStatusArbitration"], false);
    assert!(!ai_patch.exists(), "--consume should delete temp json file");

    // 2. POST /api/preflight/worktree catches non-git, unborn HEAD, and dirty warnings.
    let auth_hdrs = [
        ("x-drove-coordinator-secret", coord_tok.as_str()),
        ("x-drove-coordinator", "t1"),
    ];
    let plain_dir = s.root.join("plain_dir");
    std::fs::create_dir_all(&plain_dir).unwrap();
    let r1 = http_req(
        port,
        "POST",
        "/api/preflight/worktree",
        &auth_hdrs,
        Some(&json!({ "cwd": plain_dir.to_string_lossy(), "force": true })),
    );
    assert_eq!(r1.status, 422, "preflight plain_dir: {}", r1.body);
    let j1: Value = serde_json::from_str(&r1.body).unwrap();
    assert_eq!(j1["error"], "worktree_not_ready");
    assert!(j1["message"].as_str().unwrap_or("").contains("Git"));

    let empty_git = s.root.join("empty_git");
    std::fs::create_dir_all(&empty_git).unwrap();
    assert!(std::process::Command::new("git").args(["init"]).current_dir(&empty_git).output().unwrap().status.success());
    let r2 = http_req(
        port,
        "POST",
        "/api/preflight/worktree",
        &auth_hdrs,
        Some(&json!({ "cwd": empty_git.to_string_lossy(), "force": true })),
    );
    assert_eq!(r2.status, 422, "preflight empty_git: {}", r2.body);
    let j2: Value = serde_json::from_str(&r2.body).unwrap();
    assert_eq!(j2["error"], "worktree_not_ready");

    // Ready repo with uncommitted modification -> 200 ok + non-blocking dirtyWarning.
    let dirty_git = s.root.join("dirty_git");
    std::fs::create_dir_all(&dirty_git).unwrap();
    std::fs::write(dirty_git.join("a.txt"), "v1\n").unwrap();
    for args in [
        &["init"][..],
        &["config", "user.email", "t@drove.local"][..],
        &["config", "user.name", "Drove Test"][..],
        &["add", "."][..],
        &["commit", "-m", "init"][..],
    ] {
        assert!(std::process::Command::new("git").arg("-C").arg(&dirty_git).args(args).output().unwrap().status.success());
    }
    std::fs::write(dirty_git.join("a.txt"), "v2-uncommitted\n").unwrap();
    let r3 = http_req(
        port,
        "POST",
        "/api/preflight/worktree",
        &auth_hdrs,
        Some(&json!({ "cwd": dirty_git.to_string_lossy(), "force": true })),
    );
    assert_eq!(r3.status, 200, "preflight dirty_git: {}", r3.body);
    let j3: Value = serde_json::from_str(&r3.body).unwrap();
    assert_eq!(j3["ok"], true);
    assert_eq!(j3["checked"], true);
    assert!(j3["dirtyWarning"].as_str().is_some_and(|w| !w.is_empty()));

    // 3. Adding worktree tasks to a Run rooted in plain_dir returns 422 worktree_not_ready.
    let created = http_req(
        port,
        "POST",
        "/api/runs",
        &auth_hdrs,
        Some(&json!({ "hostSession": "t1", "objective": "preflight test", "cwd": plain_dir.to_string_lossy() })),
    );
    assert_eq!(created.status, 200, "create run: {}", created.body);
    let run_id = serde_json::from_str::<Value>(&created.body).unwrap()["id"].as_str().unwrap().to_string();

    let blocked = http_req(
        port,
        "POST",
        &format!("/api/runs/{run_id}/tasks"),
        &auth_hdrs,
        Some(&json!({
            "tasks": [{
                "key": "wt_fail",
                "title": "Needs worktree",
                "isolation": "worktree",
                "spec": { "target": "a.txt", "change": "x", "acceptance": "y" }
            }]
        })),
    );
    assert_eq!(blocked.status, 422, "expected 422 worktree_not_ready: {}", blocked.body);
    let bj: Value = serde_json::from_str(&blocked.body).unwrap();
    assert_eq!(bj["error"], "worktree_not_ready");
    assert!(bj["message"].as_str().unwrap_or("").contains("Git"));
}

#[test]
fn drove_internal_ai_respects_user_configured_language() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mock_port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();
    let captured: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let cap_srv = captured.clone();

    let srv = std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut conn, _)) => {
                    let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut raw = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        match conn.read(&mut buf) {
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
                    let req = String::from_utf8_lossy(&raw).into_owned();
                    let first_line = req.lines().next().unwrap_or("");
                    let body = if first_line.starts_with("GET /v1/models") {
                        json!({"data":[{"id":"mock-lang-model"}]}).to_string()
                    } else if first_line.starts_with("POST /v1/chat/completions") {
                        let req_body = req.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
                        cap_srv.lock().unwrap().push(req_body.to_string());
                        let content = if req_body.contains(r#"\"lang\":\"zh\""#) || req_body.contains(r#""lang":"zh""#) {
                            "中文自动标题"
                        } else {
                            "English Auto Title"
                        };
                        json!({
                            "id": "chatcmpl-lang",
                            "choices": [{"message": {"role": "assistant", "content": content}}]
                        })
                        .to_string()
                    } else {
                        r#"{"error":"not_found"}"#.to_string()
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = conn.write_all(resp.as_bytes());
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    let s = drove_session();
    let (port, key_file) = wait_web_port(&s);
    let coord_sec = read_coordinator_secret(&key_file);
    let web_tok = read_web_token(port);

    // Configure local Core AI pointing to our mock server with language = zh-CN.
    let cfg_file = s.root.join("ai-cfg.json");
    std::fs::write(
        &cfg_file,
        serde_json::to_vec(&json!({
            "enabled": true,
            "baseUrl": format!("http://127.0.0.1:{mock_port}/v1"),
            "model": "mock-lang-model",
            "language": "zh-CN"
        }))
        .unwrap(),
    )
    .unwrap();
    let st = s.json(&["ai", "settings-set", "--file", cfg_file.to_str().unwrap(), "--consume"]);
    assert_eq!(st["language"], "zh-CN");

    // 1. Creating a Run with zh-CN configured sends `"lang":"zh"` for auto_title.
    let work_dir = s.root.join("lang_work");
    std::fs::create_dir_all(&work_dir).unwrap();
    let auth_hdrs = [
        ("x-drove-coordinator-secret", coord_sec.as_str()),
        ("x-drove-coordinator", "t1"),
    ];
    let r1 = http_req(
        port,
        "POST",
        "/api/runs",
        &auth_hdrs,
        Some(&json!({ "hostSession": "t1", "objective": "fix cache invalidation", "cwd": work_dir.to_string_lossy() })),
    );
    assert_eq!(r1.status, 200, "create run 1: {}", r1.body);
    let run1 = serde_json::from_str::<Value>(&r1.body).unwrap()["id"].as_str().unwrap().to_string();

    wait_until("run 1 gets Chinese auto title", || {
        s.json(&["drove", "show", &run1])["run"]["name"] == "中文自动标题"
    });
    {
        let reqs = captured.lock().unwrap();
        assert!(!reqs.is_empty(), "expected at least one AI completion call");
        assert!(
            reqs[0].contains(r#"\"lang\":\"zh\""#),
            "expected lang=zh in auto_title payload, got: {}",
            reqs[0]
        );
    }

    // 2. Switching language via Drove Web `PUT /api/ai/language` to "en" updates subsequent Drove AI calls.
    let lang_resp = http_req(
        port,
        "PUT",
        "/api/ai/language",
        &[("x-drove-token", web_tok.as_str())],
        Some(&json!({ "language": "en" })),
    );
    assert_eq!(lang_resp.status, 200, "PUT /api/ai/language failed: {}", lang_resp.body);
    let lang_json: Value = serde_json::from_str(&lang_resp.body).unwrap();
    assert_eq!(lang_json["language"], "en");

    let r2 = http_req(
        port,
        "POST",
        "/api/runs",
        &auth_hdrs,
        Some(&json!({ "hostSession": "t1", "objective": "add metrics dashboard", "cwd": work_dir.to_string_lossy() })),
    );
    assert_eq!(r2.status, 200, "create run 2: {}", r2.body);
    let run2 = serde_json::from_str::<Value>(&r2.body).unwrap()["id"].as_str().unwrap().to_string();

    wait_until("run 2 gets English auto title", || {
        s.json(&["drove", "show", &run2])["run"]["name"] == "English Auto Title"
    });
    {
        let reqs = captured.lock().unwrap();
        assert!(reqs.len() >= 2, "expected second AI completion call");
        assert!(
            reqs.last().unwrap().contains(r#"\"lang\":\"en\""#),
            "expected lang=en in second auto_title payload, got: {}",
            reqs.last().unwrap()
        );
    }

    stop.store(true, Ordering::Relaxed);
    let _ = srv.join();
}
