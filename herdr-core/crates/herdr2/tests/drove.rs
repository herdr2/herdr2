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
    let token_path = key_file.parent().unwrap().join("coordinator.token");
    std::fs::read_to_string(&token_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", token_path.display()))
        .trim()
        .to_string()
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
    for id in ["coder", "tester", "writer", "reviewer", "researcher"] {
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

    // Complete run cleanly closes idle worker ACP session.
    let done = s.json(&["drove", "complete", &run_id, "--summary", "DAG verified"]);
    assert_eq!(done["run"]["status"], "done");
    wait_until("worker ACP agent finished after complete", || {
        s.json(&["get", &worker_addr])["state"] == "finished"
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
    let _ = wait_web_port(&s);
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

    let s = drove_session();
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
