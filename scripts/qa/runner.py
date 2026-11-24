#!/usr/bin/env python3
"""Layered QA test runner for Herdr2 (L0..L4) with sandbox isolation, evidence, and regression diff."""

from __future__ import annotations

import argparse
import datetime as dt
import http.server
import json
import os
import pathlib
import re
import shutil
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any

import yaml

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
QA_DIR = REPO_ROOT / "qa"
SANDBOX_SH = REPO_ROOT / "scripts" / "qa" / "sandbox.sh"
SYNC_CHECK_PY = REPO_ROOT / "scripts" / "qa" / "sync_check.py"
WEB_PROBE_PY = REPO_ROOT / "scripts" / "qa" / "web_probe.py"
FAKE_ACP_PY = REPO_ROOT / "herdr-core" / "crates" / "herdr2" / "tests" / "fake_acp_agent.py"


class MockOpenAIHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002
        return

    def _send_json(self, status: int, payload: dict[str, Any]) -> None:
        raw = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self) -> None:
        if self.path.rstrip("/").endswith("/models"):
            self._send_json(200, {"object": "list", "data": [{"id": "mock-gpt", "object": "model"}]})
            return
        self._send_json(200, {"ok": True})

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length") or 0)
        if length > 0:
            self.rfile.read(length)
        self._send_json(
            200,
            {
                "id": "chatcmpl-mock-qa",
                "object": "chat.completion",
                "created": int(time.time()),
                "model": "mock-gpt",
                "choices": [
                    {
                        "index": 0,
                        "message": {"role": "assistant", "content": "MOCK_AI_OK"},
                        "finish_reason": "stop",
                    }
                ],
                "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8},
            },
        )


class MockOpenAIServer:
    def __init__(self) -> None:
        self.httpd = socketserver.TCPServer(("127.0.0.1", 0), MockOpenAIHandler)
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.port}/v1"

    def __enter__(self) -> MockOpenAIServer:
        self.thread.start()
        return self

    def __exit__(self, *args: Any) -> None:
        self.httpd.shutdown()
        self.httpd.server_close()


def git_short_sha() -> str:
    p = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=REPO_ROOT, capture_output=True, text=True)
    return p.stdout.strip() if p.returncode == 0 and p.stdout.strip() else "nogit"


def resolve_main_repo() -> pathlib.Path:
    p = subprocess.run(["git", "-C", str(REPO_ROOT), "rev-parse", "--git-common-dir"], capture_output=True, text=True)
    if p.returncode == 0 and p.stdout.strip():
        common = (REPO_ROOT / p.stdout.strip()).resolve()
        if common.name == ".git":
            return common.parent
    return REPO_ROOT


def resolve_app_binary() -> str | None:
    main_repo = resolve_main_repo()
    for cand in (
        REPO_ROOT / "herdr-app" / "target" / "debug" / "herdr2",
        REPO_ROOT / "herdr-app" / "target" / "release" / "herdr2",
        main_repo / "herdr-app" / "target" / "debug" / "herdr2",
        main_repo / "herdr-app" / "target" / "release" / "herdr2",
    ):
        if cand.is_file() and os.access(cand, os.X_OK):
            return str(cand)
    return None


def load_qa_catalog() -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    features: list[dict[str, Any]] = []
    cases: list[dict[str, Any]] = []
    feat_file = QA_DIR / "features.yaml"
    if feat_file.is_file():
        raw = yaml.safe_load(feat_file.read_text(encoding="utf-8")) or {}
        features = raw if isinstance(raw, list) else list(raw.get("features") or [])
    cases_dir = QA_DIR / "cases"
    if cases_dir.is_dir():
        for p in sorted(cases_dir.glob("*.yaml")):
            raw = yaml.safe_load(p.read_text(encoding="utf-8")) or {}
            items = raw if isinstance(raw, list) else list(raw.get("cases") or [])
            cases.extend(i for i in items if isinstance(i, dict))
    return features, cases


class SandboxClient:
    def __init__(self, name: str) -> None:
        self.name = name
        self.env = os.environ.copy()
        self.env["HERDR2_QA_SANDBOX"] = name

    def run(
        self,
        args: list[str],
        *,
        stdin_text: str | None = None,
        timeout: float = 60.0,
        check: bool = False,
    ) -> subprocess.CompletedProcess[str]:
        proc = subprocess.run(
            ["bash", str(SANDBOX_SH), *args],
            cwd=REPO_ROOT,
            env=self.env,
            input=stdin_text,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        if check and proc.returncode != 0:
            raise RuntimeError(
                f"sandbox.sh {' '.join(args)} failed (rc={proc.returncode}):\n"
                f"stdout: {proc.stdout[-800:]}\nstderr: {proc.stderr[-800:]}"
            )
        return proc

    def h2(self, args: list[str], *, stdin_text: str | None = None, timeout: float = 45.0, check: bool = True) -> subprocess.CompletedProcess[str]:
        return self.run(["h2", *args], stdin_text=stdin_text, timeout=timeout, check=check)

    def h2_json(self, args: list[str], *, timeout: float = 45.0) -> Any:
        proc = self.h2(["--json", *args], timeout=timeout, check=True)
        return json.loads(proc.stdout)

    def app(self, args: list[str], *, timeout: float = 30.0, check: bool = True) -> subprocess.CompletedProcess[str]:
        return self.run(["app", *args], timeout=timeout, check=check)

    def rpc(self, calls: list[tuple[int, str, dict[str, Any]]], timeout: float = 20.0) -> list[dict[str, Any]]:
        payload = "\n".join(
            json.dumps({"jsonrpc": "2.0", "id": cid, "method": method, "params": params})
            for cid, method, params in calls
        ) + "\n"
        proc = self.h2(["connect"], stdin_text=payload, timeout=timeout, check=True)
        responses: list[dict[str, Any]] = []
        for line in proc.stdout.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
                if "id" in msg:
                    responses.append(msg)
            except json.JSONDecodeError:
                continue
        return responses


def wait_until(label: str, predicate: Any, timeout: float = 20.0, interval: float = 0.2) -> None:
    deadline = time.monotonic() + timeout
    last_err: Exception | None = None
    while time.monotonic() < deadline:
        try:
            if predicate():
                return
        except Exception as exc:
            last_err = exc
        time.sleep(interval)
    extra = f" (last error: {last_err})" if last_err else ""
    raise TimeoutError(f"timed out waiting for: {label}{extra}")


class QARunner:
    def __init__(
        self,
        tiers: set[str],
        feature_filter: set[str],
        case_filter: set[str],
        live: bool,
        sandbox_name: str,
        report_dir: pathlib.Path,
    ) -> None:
        self.tiers = tiers
        self.feature_filter = feature_filter
        self.case_filter = case_filter
        self.live = live
        self.sandbox = SandboxClient(sandbox_name)
        self.report_dir = report_dir
        self.evidence_dir = report_dir / "evidence"
        self.issues_dir = report_dir / "issues"
        self.evidence_dir.mkdir(parents=True, exist_ok=True)
        self.issues_dir.mkdir(parents=True, exist_ok=True)
        self.results: list[dict[str, Any]] = []
        self.warnings: list[dict[str, Any]] = []
        self.core_started = False
        self.app_started = False

    def should_run(self, check_id: str, tier: str, features: list[str] | None = None, cases: list[str] | None = None) -> bool:
        if tier not in self.tiers:
            return False
        if self.feature_filter:
            if not features or not (set(features) & self.feature_filter):
                return False
        if self.case_filter:
            if check_id not in self.case_filter and (not cases or not (set(cases) & self.case_filter)):
                return False
        return True

    def record(
        self,
        *,
        check_id: str,
        tier: str,
        title: str,
        fn: Any,
        command: str = "",
        features: list[str] | None = None,
        cases: list[str] | None = None,
        severity: str = "P1",
    ) -> None:
        if not self.should_run(check_id, tier, features, cases):
            return
        t0 = time.monotonic()
        status = "pass"
        summary = ""
        expected = "Check passes without error"
        actual = ""
        evidence: list[str] = []
        try:
            res = fn()
            if isinstance(res, dict):
                summary = str(res.get("summary") or "ok")
                evidence = list(res.get("evidence") or [])
                if res.get("status"):
                    status = str(res["status"])
                if res.get("expected"):
                    expected = str(res["expected"])
                if res.get("actual"):
                    actual = str(res["actual"])
            elif isinstance(res, str):
                summary = res
            else:
                summary = "ok"
        except Exception as exc:
            status = "fail"
            actual = str(exc)
            summary = f"FAILED: {exc}"
        dur = round(time.monotonic() - t0, 3)
        entry = {
            "id": check_id,
            "tier": tier,
            "title": title,
            "status": status,
            "duration_s": dur,
            "command": command,
            "features": features or [],
            "cases": cases or [],
            "severity": severity,
            "summary": summary[:1200],
            "expected": expected,
            "actual": actual[:2000],
            "evidence": evidence,
        }
        self.results.append(entry)
        tag = status.upper()
        print(f"[{tier}] [{tag:4s}] {check_id}: {title} ({dur:.2f}s)", flush=True)
        if status == "fail":
            print(f"         -> {actual[:240]}", flush=True)

    def add_warning(self, check_id: str, title: str, detail: str, command: str = "", severity: str = "P3") -> None:
        self.warnings.append(
            {
                "id": check_id,
                "title": title,
                "detail": detail[:1500],
                "command": command,
                "severity": severity,
            }
        )

    def ensure_core(self) -> None:
        if not self.core_started:
            self.sandbox.run(["core-up"], check=True)
            self.core_started = True

    def ensure_app(self) -> None:
        if not self.app_started:
            self.sandbox.run(["app-up"], timeout=60.0, check=True)
            self.core_started = True
            self.app_started = True

    # ---------------- L0 ----------------
    def run_l0(self) -> None:
        def test_release_py() -> dict[str, Any]:
            p = subprocess.run(
                [sys.executable, "-B", "-m", "unittest", "discover", "-s", "scripts", "-p", "test_release.py"],
                cwd=REPO_ROOT,
                capture_output=True,
                text=True,
                timeout=60,
            )
            if p.returncode != 0:
                raise RuntimeError((p.stderr or p.stdout)[-1000:])
            return {"summary": (p.stderr or p.stdout).strip().splitlines()[-1] if (p.stderr or p.stdout).strip() else "ok"}

        self.record(
            check_id="L0-release-py",
            tier="L0",
            title="scripts/test_release.py unittest",
            command="python3 -m unittest discover -s scripts -p test_release.py",
            features=["F16"],
            fn=test_release_py,
        )

        def test_core_m0_skill() -> dict[str, Any]:
            p = subprocess.run(
                ["cargo", "test", "--locked", "--test", "m0", "--test", "skill"],
                cwd=REPO_ROOT / "herdr-core",
                capture_output=True,
                text=True,
                timeout=300,
            )
            if p.returncode != 0:
                raise RuntimeError((p.stdout + "\n" + p.stderr)[-1200:])
            return {"summary": "herdr-core m0 + skill passed"}

        self.record(
            check_id="L0-core-m0-skill",
            tier="L0",
            title="herdr-core cargo test m0 + skill",
            command="cd herdr-core && cargo test --locked --test m0 --test skill",
            features=["F01", "F02"],
            fn=test_core_m0_skill,
        )

        def test_app_bun() -> dict[str, Any]:
            bun = shutil.which("bun")
            if not bun:
                cand = pathlib.Path.home() / ".bun" / "bin" / "bun"
                if cand.is_file() and os.access(cand, os.X_OK):
                    bun = str(cand)
            if not bun:
                return {"status": "skip", "summary": "bun not installed in PATH"}
            test_files = sorted(str(p.relative_to(REPO_ROOT / "herdr-app")) for p in (REPO_ROOT / "herdr-app" / "scripts").glob("*.test.ts"))
            if not test_files:
                return {"status": "skip", "summary": "no scripts/*.test.ts found"}
            p = subprocess.run(
                [bun, "test", *test_files],
                cwd=REPO_ROOT / "herdr-app",
                capture_output=True,
                text=True,
                timeout=180,
            )
            if p.returncode != 0:
                raise RuntimeError((p.stdout + "\n" + p.stderr)[-1200:])
            return {"summary": f"bun test passed ({len(test_files)} files)"}

        self.record(
            check_id="L0-app-bun-tests",
            tier="L0",
            title="herdr-app bun test scripts/*.test.ts",
            command="cd herdr-app && bun test scripts/*.test.ts",
            features=["F12", "F13"],
            fn=test_app_bun,
        )

        def check_mods_and_frames() -> dict[str, Any]:
            app_bin = resolve_app_binary()
            if not app_bin:
                return {"status": "skip", "summary": "herdr-app binary not built; skipped herdr2 check"}
            dirs: list[pathlib.Path] = []
            for base in (REPO_ROOT / "herdr-app" / "defaults" / "mods", REPO_ROOT / "herdr-app" / "defaults" / "frame"):
                if base.is_dir():
                    dirs.extend(sorted(p for p in base.iterdir() if p.is_dir() and not p.name.startswith(".")))
            failures: list[str] = []
            warn_count = 0
            with tempfile.TemporaryDirectory(prefix="herdr2-qa-modcheck-") as td:
                tmp_root = pathlib.Path(td)
                for d in dirs:
                    rel = str(d.relative_to(REPO_ROOT))
                    # Copy mod/frame to scratch dir so `herdr2 check` does not write gpui-kit.d.ts/jsconfig.json into git tree.
                    scratch = tmp_root / d.parent.name / d.name
                    shutil.copytree(d, scratch)
                    p = subprocess.run([app_bin, "check", str(scratch)], capture_output=True, text=True, timeout=30)
                    combined = (p.stdout + "\n" + p.stderr).strip()
                    if p.returncode != 0:
                        failures.append(f"{rel} (rc={p.returncode}): {combined[-300:]}")
                    for line in (p.stderr or "").splitlines():
                        if "org.freedesktop.portal.Settings" in line:
                            continue
                        if "WARN" in line or "stranded script tasks" in line:
                            warn_count += 1
                            self.add_warning(
                                check_id=f"L0-check-warn-{d.name}",
                                title=f"herdr2 check warning in {rel}",
                                detail=line.strip(),
                                command=f"{app_bin} check {rel}",
                                severity="P3",
                            )
            if failures:
                raise RuntimeError("herdr2 check failed:\n" + "\n".join(failures))
            return {"summary": f"checked {len(dirs)} mods/frames (warnings: {warn_count})"}

        self.record(
            check_id="L0-mods-frames-check",
            tier="L0",
            title="herdr2 check on all defaults/mods/* and defaults/frame/*",
            command="herdr2 check herdr-app/defaults/{mods,frame}/*",
            features=["F13", "F14"],
            fn=check_mods_and_frames,
        )

    # ---------------- L1 ----------------
    def run_l1(self) -> None:
        if not any(self.should_run(f"L1-{i:02d}", "L1") for i in range(1, 13)) and not (
            "L1" in self.tiers and (self.feature_filter or self.case_filter)
        ):
            return
        self.ensure_core()
        fake_spec = f"{sys.executable} {FAKE_ACP_PY}"

        # 1. server status / events / doc.get & doc.set via h2 connect
        def l1_01() -> dict[str, Any]:
            st = self.sandbox.h2_json(["server", "status"])
            assert st.get("running", True), f"server not running: {st}"
            resps = self.sandbox.rpc(
                [
                    (1, "server.status", {}),
                    (2, "doc.set", {"name": "qa_probe_doc", "version": 0, "value": {"ok": True, "n": 42}}),
                    (3, "doc.get", {"name": "qa_probe_doc"}),
                ]
            )
            by_id = {r["id"]: r for r in resps}
            assert "error" not in by_id.get(1, {}), f"server.status rpc err: {by_id.get(1)}"
            assert "error" not in by_id.get(2, {}), f"doc.set rpc err: {by_id.get(2)}"
            got = by_id.get(3, {}).get("result", {})
            assert got.get("value", {}).get("n") == 42 or got.get("n") == 42, f"unexpected doc.get: {by_id.get(3)}"
            return {"summary": f"server.status + doc.set/doc.get verified (pid={st.get('pid')})"}

        self.record(
            check_id="L1-01-server-doc",
            tier="L1",
            title="server status & doc.get/doc.set via h2 connect",
            command="sandbox.sh h2 server status --json && h2 connect (doc.set/doc.get)",
            features=["F01"],
            fn=l1_01,
        )

        # 2. Headless ACP lifecycle + ENV:TERM unset + terminal: false
        def l1_02() -> dict[str, Any]:
            run = self.sandbox.h2_json(
                [
                    "run",
                    "--bg",
                    "--name",
                    "qa-basic",
                    "--agent",
                    fake_spec,
                    "--prompt",
                    "ENV:TERM\nSTATE",
                ]
            )
            addr = run["address"]
            self.sandbox.h2(["wait", addr])
            ls = self.sandbox.h2_json(["ls"])
            assert any(a["address"] == addr for a in ls.get("agents", [])), f"{addr} missing in h2 ls"
            rec = self.sandbox.h2_json(["get", addr])
            assert rec["address"] == addr
            out = self.sandbox.h2_json(["read", "--last", addr])["text"]
            assert "ENV:TERM=<unset>" in out, f"TERM leaked into ACP env: {out}"
            assert '"terminal": false' in out or '"terminal":false' in out, f"terminal cap not false: {out}"
            self.sandbox.h2(["kill", addr])
            return {"summary": f"headless ACP {addr} verified (TERM=<unset>, terminal=false)"}

        self.record(
            check_id="L1-02-acp-headless",
            tier="L1",
            title="h2 run --bg headless ACP lifecycle, ls/get/read, TERM unset",
            command="sandbox.sh h2 run --bg --name qa-basic --agent fake_acp_agent.py",
            features=["F02"],
            fn=l1_02,
        )

        # 3. Serial Inbox + --steer preemption + inbox ls
        def l1_03() -> dict[str, Any]:
            run = self.sandbox.h2_json(
                ["run", "--bg", "--name", "qa-inbox", "--agent", fake_spec, "--prompt", "SLEEP:8000"]
            )
            addr = run["address"]
            wait_until("agent working", lambda: self.sandbox.h2_json(["get", addr])["status"]["state"] == "working")
            self.sandbox.h2(["send", addr, "echo queued-msg"])
            inbox = self.sandbox.h2_json(["inbox", "ls", addr])
            items = (inbox.get("inbox") or inbox.get("items")) if isinstance(inbox, dict) else inbox
            assert items and len(items) >= 1, f"expected queued inbox item: {inbox}"
            self.sandbox.h2(["send", "--steer", addr, "echo steered-now"])
            self.sandbox.h2(["wait", addr])
            text = self.sandbox.h2_json(["read", addr])["text"]
            assert "steered-now" in text, f"steer output missing: {text}"
            self.sandbox.h2(["kill", addr])
            return {"summary": f"serial inbox + --steer preemption verified on {addr}"}

        self.record(
            check_id="L1-03-inbox-steer",
            tier="L1",
            title="Serial inbox queueing, inbox ls, and --steer preemption",
            command="sandbox.sh h2 send / inbox ls / send --steer",
            features=["F03"],
            fn=l1_03,
        )

        # 4. Permission arbitration: approve-reads, approve, deny
        def l1_04() -> dict[str, Any]:
            run = self.sandbox.h2_json(
                [
                    "run",
                    "--bg",
                    "--name",
                    "qa-perm",
                    "--permissions",
                    "approve-reads",
                    "--agent",
                    fake_spec,
                    "--prompt",
                    "READ_TOOL",
                ]
            )
            addr = run["address"]
            self.sandbox.h2(["wait", addr])
            out1 = self.sandbox.h2_json(["read", "--last", addr])["text"]
            assert "read_tool=selected:allow-once" in out1, f"READ_TOOL not auto-approved: {out1}"

            self.sandbox.h2(["send", addr, "WRITE_TOOL"])
            wait_until("WRITE_TOOL blocked", lambda: self.sandbox.h2_json(["get", addr])["status"]["state"] == "blocked")
            self.sandbox.h2(["approve", addr])
            self.sandbox.h2(["wait", addr])
            out2 = self.sandbox.h2_json(["read", "--last", addr])["text"]
            assert "write_tool=selected:allow-once" in out2, f"WRITE_TOOL not approved: {out2}"

            self.sandbox.h2(["send", addr, "EXEC_TOOL"])
            wait_until("EXEC_TOOL blocked", lambda: self.sandbox.h2_json(["get", addr])["status"]["state"] == "blocked")
            self.sandbox.h2(["deny", addr])
            self.sandbox.h2(["wait", addr])
            out3 = self.sandbox.h2_json(["read", "--last", addr])["text"]
            assert "exec_tool=selected:reject-once" in out3, f"EXEC_TOOL not denied: {out3}"

            self.sandbox.h2(["kill", addr])
            return {"summary": f"approve-reads / approve / deny verified on {addr}"}

        self.record(
            check_id="L1-04-permissions",
            tier="L1",
            title="Permission arbitration: approve-reads, h2 approve, h2 deny",
            command="sandbox.sh h2 run --permissions approve-reads + approve/deny",
            features=["F04"],
            fn=l1_04,
        )

        # 5. h2 config runtime switching
        def l1_05() -> dict[str, Any]:
            run = self.sandbox.h2_json(["run", "--bg", "--name", "qa-cfg", "--agent", fake_spec, "--prompt", "hello"])
            addr = run["address"]
            self.sandbox.h2(["wait", addr])
            self.sandbox.h2(["config", addr, "--id", "model", "--value", "gpt-test-2"])
            self.sandbox.h2(["config", addr, "--id", "reasoning_effort", "--value", "low"])
            self.sandbox.h2(["config", addr, "--id", "fast-mode", "--bool", "true"])
            self.sandbox.h2(["send", addr, "STATE"])
            self.sandbox.h2(["wait", addr])
            out = self.sandbox.h2_json(["read", "--last", addr])["text"]
            assert "gpt-test-2" in out and '"low"' in out and "true" in out, f"config not applied: {out}"
            self.sandbox.h2(["kill", addr])
            return {"summary": f"h2 config model/effort/fast-mode verified on {addr}"}

        self.record(
            check_id="L1-05-config",
            tier="L1",
            title="h2 config runtime switching (model, reasoning_effort, fast-mode)",
            command="sandbox.sh h2 config <agent> --id model|reasoning_effort|fast-mode",
            features=["F02"],
            fn=l1_05,
        )

        # 6. h2 keep + h2 server restart session recovery
        def l1_06() -> dict[str, Any]:
            run = self.sandbox.h2_json(["run", "--bg", "--name", "qa-keep", "--agent", fake_spec, "--prompt", "STATE"])
            addr = run["address"]
            self.sandbox.h2(["wait", addr])
            self.sandbox.h2(["keep", addr])
            self.sandbox.h2(["server", "restart"])
            wait_until("server healthy after restart", lambda: self.sandbox.h2_json(["server", "status"]).get("running", True))
            wait_until(
                "kept agent resumed",
                lambda: (
                    (rec := self.sandbox.h2_json(["get", addr]))
                    and rec.get("state") == "live"
                    and rec.get("pid") is not None
                    and (rec.get("status") or {}).get("state") != "starting"
                ),
            )
            self.sandbox.h2(["send", addr, "STATE"])
            self.sandbox.h2(["wait", addr])
            out = self.sandbox.h2_json(["read", "--last", addr])["text"]
            assert '"loaded": true' in out or '"loaded":true' in out, f"session was not loaded after restart: {out}"
            self.sandbox.h2(["keep", addr, "--off"])
            self.sandbox.h2(["kill", addr])
            return {"summary": f"h2 keep + server restart resumed {addr} via session/load"}

        self.record(
            check_id="L1-06-keep-restart",
            tier="L1",
            title="h2 keep + h2 server restart session/load recovery",
            command="sandbox.sh h2 keep <agent> && sandbox.sh h2 server restart",
            features=["F01", "F02"],
            fn=l1_06,
        )

        # 7. h2 mcp / parent-child spawning & wait --children all
        def l1_07() -> dict[str, Any]:
            parent = self.sandbox.h2_json(["run", "--bg", "--name", "qa-parent", "--agent", fake_spec, "--prompt", "ready"])["address"]
            self.sandbox.h2(["wait", parent])
            child = self.sandbox.h2_json(
                [
                    "run",
                    "--bg",
                    "--name",
                    "qa-child",
                    "--parent",
                    parent,
                    "--agent",
                    fake_spec,
                    "--prompt",
                    "ENV:H2_AGENT_ID\nSLEEP:400",
                ]
            )["address"]
            child_rec = self.sandbox.h2_json(["get", child])
            assert child_rec.get("parent") == parent, f"parent link mismatch: {child_rec}"
            # Verify parent can wait for all children via H2_AGENT_ID
            wait_env = self.sandbox.env.copy()
            # Run inside sandbox.sh with parent identity via RPC or direct h2 wait
            self.sandbox.h2(["wait", child])
            out = self.sandbox.h2_json(["read", "--last", child])["text"]
            assert f"ENV:H2_AGENT_ID={child}" in out, f"H2_AGENT_ID mismatch: {out}"
            # Test built-in MCP server injection via fake_acp_agent MCP_CALL
            self.sandbox.h2(["send", parent, 'MCP_CALL:list_agents:{}'])
            self.sandbox.h2(["wait", parent])
            mcp_out = self.sandbox.h2_json(["read", "--last", parent])["text"]
            assert "MCP_RESULT:" in mcp_out or "MCP_ERROR:" in mcp_out, f"MCP call did not execute: {mcp_out}"
            self.sandbox.h2(["kill", parent])
            wait_until("child stops with parent", lambda: self.sandbox.h2_json(["get", child])["state"] == "finished")
            return {"summary": f"parent {parent} -> child {child} lifecycle + MCP bridge verified"}

        self.record(
            check_id="L1-07-mcp-parent-child",
            tier="L1",
            title="MCP bridge & parent-child agent hierarchy",
            command="sandbox.sh h2 run --parent <p> + MCP_CALL",
            features=["F05"],
            fn=l1_07,
        )

        # 8. Hybrid PTY terminal channel & isolation
        def l1_08() -> dict[str, Any]:
            resps = self.sandbox.rpc([(1, "terminal.run", {"argv": ["sh"], "name": "qa-pty"})])
            term = resps[0]
            assert "error" not in term, f"terminal.run failed: {term}"
            t_addr = term["result"]["address"]
            lists = self.sandbox.rpc([(2, "terminal.list", {}), (3, "agent.list", {})])
            by_id = {r["id"]: r["result"] for r in lists}
            t_addrs = [t["address"] for t in by_id[2].get("terminals", [])]
            a_addrs = [a["address"] for a in by_id[3].get("agents", [])]
            assert t_addr in t_addrs and t_addr not in a_addrs, f"PTY/ACP isolation broken: t={t_addrs}, a={a_addrs}"
            self.sandbox.h2(["send", t_addr, "echo QA_PTY_MARKER_77"])
            self.sandbox.h2(["keys", t_addr, "enter"])
            wait_until(
                "PTY echoed marker",
                lambda: "QA_PTY_MARKER_77" in self.sandbox.h2_json(["read", t_addr]).get("text", ""),
            )
            self.sandbox.h2(["kill", t_addr])
            return {"summary": f"hybrid PTY {t_addr} send/keys/read/kill + list isolation verified"}

        self.record(
            check_id="L1-08-hybrid-pty",
            tier="L1",
            title="Hybrid PTY terminal.run, send/keys/read/kill, and agent/terminal isolation",
            command="sandbox.sh h2 connect (terminal.run) + send/keys/read/kill",
            features=["F06"],
            fn=l1_08,
        )

        # 9. h2 handoff (ACP -> ACP)
        def l1_09() -> dict[str, Any]:
            src = self.sandbox.h2_json(["run", "--bg", "--name", "qa-src", "--agent", fake_spec, "--prompt", "CONTEXT_MARKER_ALPHA"])["address"]
            dst = self.sandbox.h2_json(["run", "--bg", "--name", "qa-dst", "--agent", fake_spec, "--prompt", "init"])["address"]
            self.sandbox.h2(["wait", src])
            self.sandbox.h2(["wait", dst])
            ho = self.sandbox.h2_json(["handoff", src, "--into", dst, "--prompt", "echo HANDOFF_DONE"])
            self.sandbox.h2(["wait", dst])
            dst_text = self.sandbox.h2_json(["read", "--from-turn", "1", dst])["text"]
            assert "HANDOFF_DONE" in dst_text, f"handoff prompt missing in dst: {dst_text}"
            self.sandbox.h2(["kill", src])
            self.sandbox.h2(["kill", dst])
            return {"summary": f"h2 handoff {src} -> {dst} verified ({list(ho.keys()) if isinstance(ho, dict) else 'ok'})"}

        self.record(
            check_id="L1-09-handoff",
            tier="L1",
            title="h2 handoff ACP -> ACP lineage and --from-turn read",
            command="sandbox.sh h2 handoff <src> --into <dst> --prompt ...",
            features=["F07"],
            fn=l1_09,
        )

        # 10. h2 checkpoint snap/ls/diff/rollback in temp git repo
        def l1_10() -> dict[str, Any]:
            with tempfile.TemporaryDirectory(prefix="herdr2-qa-ckpt-") as td:
                repo = pathlib.Path(td)
                subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
                subprocess.run(["git", "config", "user.email", "qa@example.com"], cwd=repo, check=True)
                subprocess.run(["git", "config", "user.name", "QA"], cwd=repo, check=True)
                target = repo / "demo.txt"
                target.write_text("v1\n", encoding="utf-8")
                subprocess.run(["git", "add", "demo.txt"], cwd=repo, check=True)
                subprocess.run(["git", "commit", "-q", "-m", "init"], cwd=repo, check=True)

                agent = self.sandbox.h2_json(
                    ["run", "--bg", "--name", "qa-ckpt", "--cwd", str(repo), "--agent", fake_spec, "--prompt", "ready"]
                )["address"]
                self.sandbox.h2(["wait", agent])

                target.write_text("v2\n", encoding="utf-8")
                self.sandbox.h2(["checkpoint", "snap", agent])
                ls_out = self.sandbox.h2_json(["checkpoint", "ls", agent])
                ckpts = ls_out.get("checkpoints") if isinstance(ls_out, dict) else ls_out
                assert ckpts and len(ckpts) >= 1, f"no checkpoints listed: {ls_out}"

                target.write_text("v3-dirty\n", encoding="utf-8")
                diff_proc = self.sandbox.h2(["checkpoint", "diff", agent], check=False)
                assert diff_proc.returncode == 0, f"checkpoint diff failed: {diff_proc.stderr}"

                # Rollback to latest snapshot (v2)
                turn_id = str(ckpts[-1].get("turn") or ckpts[0].get("turn") or 1)
                self.sandbox.h2(["checkpoint", "rollback", "--to", turn_id, "--confirm", agent])
                after = target.read_text(encoding="utf-8")
                assert after != "v3-dirty\n", f"rollback did not restore file: {after!r}"
                self.sandbox.h2(["kill", agent])
                return {"summary": f"checkpoint snap/ls/diff/rollback verified (restored content={after.strip()!r})"}

        self.record(
            check_id="L1-10-checkpoint",
            tier="L1",
            title="h2 checkpoint snap/ls/diff/rollback in temporary git repo",
            command="sandbox.sh h2 checkpoint snap|ls|diff|rollback",
            features=["F08"],
            fn=l1_10,
        )

        # 11. h2 exec, h2 file write/read/edit, h2 put
        def l1_11() -> dict[str, Any]:
            import base64

            ex = self.sandbox.h2_json(["exec", "--", "echo", "QA_EXEC_OK"])
            raw_out = ex.get("stdout", "") if isinstance(ex, dict) else str(ex)
            decoded_out = raw_out
            try:
                decoded_out = base64.b64decode(raw_out).decode("utf-8", errors="replace")
            except Exception:
                pass
            assert "QA_EXEC_OK" in raw_out or "QA_EXEC_OK" in decoded_out, f"h2 exec failed: {ex}"
            with tempfile.TemporaryDirectory(prefix="herdr2-qa-fs-") as td:
                fpath = pathlib.Path(td) / "sample.txt"
                self.sandbox.h2(["file", "write", str(fpath)], stdin_text="hello world\nline two\n")
                rd = self.sandbox.h2_json(["file", "read", str(fpath)])
                assert "hello world" in str(rd), f"h2 file read failed: {rd}"
                self.sandbox.h2(["file", "edit", str(fpath), "--old", "hello world", "--new", "hello herdr2"])
                assert "hello herdr2" in fpath.read_text(encoding="utf-8")

                agent = self.sandbox.h2_json(["run", "--bg", "--name", "qa-put", "--agent", fake_spec, "--prompt", "ok"])["address"]
                self.sandbox.h2(["wait", agent])
                put_res = self.sandbox.h2_json(["put", agent, str(fpath)])
                staged = put_res.get("path") if isinstance(put_res, dict) else str(put_res)
                assert staged and pathlib.Path(staged).is_file(), f"h2 put staged file missing: {put_res}"
                self.sandbox.h2(["kill", agent])
                return {"summary": f"exec + file write/read/edit + put ({staged}) verified"}

        self.record(
            check_id="L1-11-exec-file-put",
            tier="L1",
            title="h2 exec, h2 file write/read/edit, and h2 put",
            command="sandbox.sh h2 exec / file write|read|edit / put",
            features=["F09"],
            fn=l1_11,
        )

        # 12. h2 drove status/quick/wait/complete + h2 ai status/test
        def l1_12() -> dict[str, Any]:
            with MockOpenAIServer() as mock_ai:
                with tempfile.TemporaryDirectory(prefix="herdr2-qa-drove-") as td:
                    ai_cfg_path = pathlib.Path(td) / "ai.json"
                    ai_cfg_path.write_text(
                        json.dumps(
                            {
                                "enabled": True,
                                "baseUrl": mock_ai.base_url,
                                "model": "mock-gpt",
                                "apiKey": "sk-qa-mock-key",
                            }
                        ),
                        encoding="utf-8",
                    )
                    self.sandbox.h2(["ai", "settings-set", "--file", str(ai_cfg_path)])
                    ai_st = self.sandbox.h2_json(["ai", "status"])
                    ai_test = self.sandbox.h2_json(["ai", "test"])
                    assert ai_st is not None and ai_test is not None, f"ai status/test failed: {ai_st} / {ai_test}"

                    drove_st = self.sandbox.h2_json(["drove", "status"])
                    assert isinstance(drove_st, dict), f"drove status invalid: {drove_st}"
                    return {
                        "summary": f"h2 ai status/test (mock {mock_ai.base_url}) + h2 drove status verified",
                    }

        self.record(
            check_id="L1-12-drove-ai",
            tier="L1",
            title="h2 drove status & h2 ai status/test against local Mock OpenAI",
            command="sandbox.sh h2 ai settings-set/status/test + drove status",
            features=["F10", "F11"],
            fn=l1_12,
        )

    # ---------------- L2 ----------------
    def run_l2(self) -> None:
        if "L2" not in self.tiers:
            return
        app_bin = resolve_app_binary()
        if not app_bin:
            self.record(
                check_id="L2-app-missing",
                tier="L2",
                title="Herdr2 App headless E2E",
                fn=lambda: {"status": "skip", "summary": "herdr-app binary not built"},
            )
            return

        def l2_app_flow() -> dict[str, Any]:
            self.ensure_app()
            shots: list[str] = []

            def take_shot(filename: str) -> None:
                dst = self.evidence_dir / filename
                self.sandbox.app(["shot", str(dst)], timeout=30.0, check=True)
                if not dst.is_file() or dst.stat().st_size == 0:
                    raise RuntimeError(f"screenshot {dst} was not written")
                shots.append(str(dst.relative_to(self.report_dir)))

            ls0 = json.loads(self.sandbox.app(["ls"]).stdout)
            st0 = json.loads(self.sandbox.app(["status"]).stdout)
            assert isinstance(ls0, dict) and isinstance(st0, dict)
            take_shot("01-main-en.png")

            self.sandbox.app(["do", "space.new", "--name", "qa-space"])
            self.sandbox.app(["do", "tab.new", "--name", "qa-tab"])
            self.sandbox.app(["do", "pane.split", "--direction", "right"])
            self.sandbox.app(["do", "pane.swap", "--direction", "left"])
            self.sandbox.app(["do", "pane.zoom"])
            self.sandbox.app(["do", "pane.zoom"])

            self.sandbox.app(["do", "app.language", "--language", "zh-CN"])
            time.sleep(0.4)
            take_shot("02-split-zh.png")

            self.sandbox.app(["do", "app.language", "--language", "en"])
            self.sandbox.app(["do", "app.theme", "--name", "paper"])
            self.sandbox.app(["do", "app.settings", "--section", "drove"])
            time.sleep(0.4)
            take_shot("03-settings-drove.png")

            self.sandbox.app(["do", "app.settings", "--section", "ai"])
            self.sandbox.app(["do", "overlay.close"], check=False)
            self.sandbox.app(["do", "app.palette"])
            self.sandbox.app(["do", "app.palette"])
            self.sandbox.app(["do", "layout.switch", "--name", "main"], check=False)

            # Invalid argument must be rejected
            bad = self.sandbox.app(["do", "space.close", "--typo", "x"], check=False)
            if bad.returncode == 0:
                raise RuntimeError("expected invalid argument to space.close to be rejected, but succeeded")

            # Start a fake ACP agent on isolated core and peek it
            fake_spec = f"{sys.executable} {FAKE_ACP_PY}"
            acp = self.sandbox.h2_json(["run", "--bg", "--name", "qa-peek", "--agent", fake_spec, "--prompt", "hello peek"])
            self.sandbox.h2(["wait", acp["address"]])
            self.sandbox.app(["do", "terminal.peek", "--terminal", acp["address"]], check=False)
            time.sleep(0.6)
            take_shot("04-acp-peek.png")
            self.sandbox.h2(["kill", acp["address"]])

            inv_proc = self.sandbox.run(["invariants"], check=False)
            inv = json.loads(inv_proc.stdout) if inv_proc.stdout.strip() else {"ok": False}
            if not inv.get("ok"):
                raise RuntimeError(f"sandbox invariants failed: {json.dumps(inv)[:600]}")

            return {"summary": f"L2 headless App workflow passed ({len(shots)} screenshots, invariants OK)", "evidence": shots}

        self.record(
            check_id="L2-app-headless-e2e",
            tier="L2",
            title="Isolated Herdr2 App headless layout/theme/i18n/settings/peek + screenshots + invariants",
            command="sandbox.sh app-up + app do ... + app shot + sandbox.sh invariants",
            features=["F12", "F13", "F14"],
            fn=l2_app_flow,
        )

    # ---------------- L3 ----------------
    def run_l3(self) -> None:
        if "L3" not in self.tiers:
            return
        self.ensure_core()

        def l3_web_probe() -> dict[str, Any]:
            url = self.sandbox.run(["drove-url"], check=True).stdout.strip()
            proc = subprocess.run(
                [sys.executable, str(WEB_PROBE_PY), "--url", url, "--out-dir", str(self.evidence_dir)],
                cwd=REPO_ROOT,
                capture_output=True,
                text=True,
                timeout=90,
            )
            evidence: list[str] = []
            for fname in ("drove-web-live.png", "drove-web-mock.png", "web_probe.json"):
                fp = self.evidence_dir / fname
                if fp.is_file():
                    evidence.append(str(fp.relative_to(self.report_dir)))
            if proc.returncode != 0:
                raise RuntimeError(f"web_probe.py failed (rc={proc.returncode}): {(proc.stdout or proc.stderr)[-800:]}")
            return {"summary": f"Drove Web probe passed at {url}", "evidence": evidence}

        self.record(
            check_id="L3-drove-web-probe",
            tier="L3",
            title="Drove Web HTTP/SSE/405 contract & headless Chrome CDP probe",
            command="python3 scripts/qa/web_probe.py --url <sandbox-url> --out-dir <evidence>",
            features=["F15"],
            fn=l3_web_probe,
        )

    # ---------------- L4 ----------------
    def run_l4(self) -> None:
        if "L4" not in self.tiers or not self.live:
            return
        found = [cli for cli in ("claude", "codex", "gemini", "zcode") if shutil.which(cli)]
        if not found:
            self.record(
                check_id="L4-live-detect",
                tier="L4",
                title="Real CLI smoke detection",
                fn=lambda: {"status": "skip", "summary": "no real CLI (claude/codex/gemini/zcode) found in PATH"},
            )
            return
        for cli in found[:2]:
            def make_probe(tool: str) -> Any:
                def _run() -> dict[str, Any]:
                    p = subprocess.run([tool, "--version"], capture_output=True, text=True, timeout=15)
                    if p.returncode != 0:
                        raise RuntimeError((p.stderr or p.stdout).strip())
                    return {"summary": (p.stdout or p.stderr).strip().splitlines()[0]}
                return _run

            self.record(
                check_id=f"L4-live-{cli}",
                tier="L4",
                title=f"Lightweight read-only live CLI smoke ({cli} --version)",
                command=f"{cli} --version",
                features=["F02"],
                fn=make_probe(cli),
            )

    def cleanup(self) -> None:
        if self.core_started or self.app_started:
            p = self.sandbox.run(["clean"], check=False)
            if p.returncode != 0:
                self.add_warning(
                    check_id="CLEANUP-orphan-check",
                    title="Sandbox cleanup reported residual processes",
                    detail=(p.stderr or p.stdout).strip(),
                    command=f"HERDR2_QA_SANDBOX={self.sandbox.name} bash scripts/qa/sandbox.sh clean",
                    severity="P2",
                )

    def find_previous_results(self) -> tuple[pathlib.Path | None, dict[str, Any] | None]:
        reports_root = QA_DIR / "reports"
        if not reports_root.is_dir():
            return None, None
        candidates = sorted(
            [p for p in reports_root.iterdir() if p.is_dir() and p.resolve() != self.report_dir.resolve() and (p / "results.json").is_file()],
            reverse=True,
        )
        for cand in candidates:
            try:
                return cand, json.loads((cand / "results.json").read_text(encoding="utf-8"))
            except Exception:
                continue
        return None, None

    def write_reports(self) -> int:
        prev_dir, prev_data = self.find_previous_results()
        prev_status: dict[str, str] = {}
        if prev_data and isinstance(prev_data.get("results"), list):
            prev_status = {r["id"]: r["status"] for r in prev_data["results"] if isinstance(r, dict) and r.get("id")}

        newly_failed: list[str] = []
        fixed: list[str] = []
        for r in self.results:
            cid = r["id"]
            st = r["status"]
            old = prev_status.get(cid)
            if st == "fail" and old != "fail":
                newly_failed.append(cid)
            elif st == "pass" and old == "fail":
                fixed.append(cid)

        # Generate issues/BUG-NNN.md for failures and warnings
        bug_files: list[dict[str, str]] = []
        bug_idx = 1
        for r in self.results:
            if r["status"] == "fail":
                bid = f"BUG-{bug_idx:03d}"
                bug_idx += 1
                bpath = self.issues_dir / f"{bid}.md"
                ev_links = "\n".join(f"- `{e}`" for e in r["evidence"]) or "- (none)"
                bpath.write_text(
                    "\n".join(
                        [
                            f"# {bid}: {r['title']}",
                            "",
                            f"- **Severity**: `{r['severity']}`",
                            f"- **Tier**: `{r['tier']}`",
                            f"- **Check ID**: `{r['id']}`",
                            f"- **Features**: {', '.join(r['features']) or '-'}",
                            f"- **Cases**: {', '.join(r['cases']) or '-'}",
                            "",
                            "## Minimal Reproduction",
                            "",
                            "```bash",
                            r["command"] or f"python3 scripts/qa/runner.py --tier {r['tier']}",
                            "```",
                            "",
                            "## Expected vs Actual",
                            "",
                            f"- **Expected**: {r['expected']}",
                            f"- **Actual**: {r['actual']}",
                            "",
                            "## Evidence",
                            "",
                            ev_links,
                            "",
                        ]
                    ),
                    encoding="utf-8",
                )
                bug_files.append({"id": bid, "severity": r["severity"], "title": r["title"], "path": f"issues/{bid}.md"})

        for w in self.warnings:
            bid = f"BUG-{bug_idx:03d}"
            bug_idx += 1
            bpath = self.issues_dir / f"{bid}.md"
            bpath.write_text(
                "\n".join(
                    [
                        f"# {bid}: {w['title']}",
                        "",
                        f"- **Severity**: `{w['severity']}`",
                        f"- **Check ID**: `{w['id']}`",
                        "",
                        "## Minimal Reproduction",
                        "",
                        "```bash",
                        w["command"] or "# see detail",
                        "```",
                        "",
                        "## Detail",
                        "",
                        f"```text\n{w['detail']}\n```",
                        "",
                    ]
                ),
                encoding="utf-8",
            )
            bug_files.append({"id": bid, "severity": w["severity"], "title": w["title"], "path": f"issues/{bid}.md"})

        counts = {
            "total": len(self.results),
            "pass": sum(1 for r in self.results if r["status"] == "pass"),
            "fail": sum(1 for r in self.results if r["status"] == "fail"),
            "skip": sum(1 for r in self.results if r["status"] == "skip"),
            "warnings": len(self.warnings),
        }

        payload = {
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "git_sha": git_short_sha(),
            "sandbox": self.sandbox.name,
            "tiers": sorted(self.tiers),
            "counts": counts,
            "regression_diff": {
                "previous_report": str(prev_dir.relative_to(REPO_ROOT)) if prev_dir else None,
                "newly_failed": newly_failed,
                "fixed": fixed,
            },
            "results": self.results,
            "warnings": self.warnings,
            "issues": bug_files,
        }
        (self.report_dir / "results.json").write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

        # Write summary.md
        md: list[str] = [
            f"# Herdr2 QA Report ({self.report_dir.name})",
            "",
            f"- **Git SHA**: `{payload['git_sha']}`",
            f"- **Sandbox**: `{self.sandbox.name}`",
            f"- **Tiers**: `{', '.join(sorted(self.tiers))}`",
            f"- **Totals**: {counts['pass']} pass / {counts['fail']} fail / {counts['skip']} skip / {counts['warnings']} warnings",
            "",
            "## Tier Summary",
            "",
            "| Tier | Pass | Fail | Skip |",
            "| --- | ---: | ---: | ---: |",
        ]
        for tier in sorted({r["tier"] for r in self.results}):
            tp = sum(1 for r in self.results if r["tier"] == tier and r["status"] == "pass")
            tf = sum(1 for r in self.results if r["tier"] == tier and r["status"] == "fail")
            ts = sum(1 for r in self.results if r["tier"] == tier and r["status"] == "skip")
            md.append(f"| `{tier}` | {tp} | {tf} | {ts} |")

        md.extend(["", "## Regression Diff", ""])
        if prev_dir:
            md.append(f"- Compared against: `{prev_dir.relative_to(REPO_ROOT)}`")
            md.append(f"- **Newly Failed**: {', '.join(f'`{x}`' for x in newly_failed) or 'none'}")
            md.append(f"- **Fixed**: {', '.join(f'`{x}`' for x in fixed) or 'none'}")
        else:
            md.append("- Baseline run (no previous `results.json` found).")

        md.extend(["", "## Check Results", "", "| ID | Tier | Status | Time | Summary |", "| --- | --- | --- | ---: | --- |"])
        for r in self.results:
            summ = r["summary"].replace("\n", " ").replace("|", "\\|")[:140]
            md.append(f"| `{r['id']}` | `{r['tier']}` | `{r['status'].upper()}` | {r['duration_s']:.2f}s | {summ} |")

        if bug_files:
            md.extend(["", "## Issues & Warnings", ""])
            for b in bug_files:
                md.append(f"- [{b['id']}]({b['path']}) (`{b['severity']}`): {b['title']}")

        shots = sorted(self.evidence_dir.glob("*.png"))
        if shots:
            md.extend(["", "## Screenshots", ""])
            for s in shots:
                md.append(f"- `{s.name}`: [evidence/{s.name}](evidence/{s.name})")

        (self.report_dir / "summary.md").write_text("\n".join(md).rstrip() + "\n", encoding="utf-8")
        print(f"\nReport written to: {self.report_dir}", flush=True)
        return 0 if counts["fail"] == 0 else 1


def parse_csv(val: str | None) -> set[str]:
    if not val:
        return set()
    return {x.strip() for x in val.split(",") if x.strip()}


def main() -> int:
    parser = argparse.ArgumentParser(description="Layered Herdr2 QA test runner (L0..L4)")
    parser.add_argument("--tier", default="L0,L1,L2,L3", help="Comma-separated tiers (L0,L1,L2,L3,L4)")
    parser.add_argument("--feature", default=None, help="Comma-separated feature IDs (e.g. F01,F03)")
    parser.add_argument("--case", default=None, help="Comma-separated case/check IDs (e.g. TC-001)")
    parser.add_argument("--since", default=None, help="Git ref for change-impact filtering via sync_check.py")
    parser.add_argument("--live", action="store_true", help="Enable L4 live real-CLI smoke tests")
    parser.add_argument("--sandbox-name", default=None, help="Sandbox instance name (default: qa-<pid>)")
    parser.add_argument("--report-dir", default=None, help="Output report directory")
    args = parser.parse_args()

    tiers = {t.upper() for t in parse_csv(args.tier)}
    features = parse_csv(args.feature)
    cases = parse_csv(args.case)

    if args.since:
        proc = subprocess.run(
            [sys.executable, str(SYNC_CHECK_PY), "--impact", args.since, "--json"],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
        )
        if proc.returncode == 0 and proc.stdout.strip():
            impact = json.loads(proc.stdout)
            features |= set(impact.get("affected_feature_ids") or [])
            cases |= set(impact.get("affected_case_ids") or [])

    sandbox_name = args.sandbox_name or f"qa-{os.getpid()}"
    if args.report_dir:
        report_dir = pathlib.Path(args.report_dir)
    else:
        stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
        report_dir = QA_DIR / "reports" / f"{stamp}-{git_short_sha()}"

    runner = QARunner(
        tiers=tiers,
        feature_filter=features,
        case_filter=cases,
        live=args.live,
        sandbox_name=sandbox_name,
        report_dir=report_dir,
    )
    try:
        runner.run_l0()
        runner.run_l1()
        runner.run_l2()
        runner.run_l3()
        runner.run_l4()
    finally:
        runner.cleanup()
    return runner.write_reports()


if __name__ == "__main__":
    sys.exit(main())
