#!/usr/bin/env python3
"""Drove Web HTTP/SSE/Read-Only Contract & Headless Chrome CDP Probe (stdlib only)."""

from __future__ import annotations

import argparse
import base64
import hashlib
import http.client
import json
import os
import pathlib
import re
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from typing import Any


def http_request(
    base_url: str,
    method: str,
    path: str,
    headers: dict[str, str] | None = None,
    body: bytes | None = None,
    timeout: float = 10.0,
) -> dict[str, Any]:
    parsed = urllib.parse.urlparse(base_url)
    host = parsed.hostname or "127.0.0.1"
    port = parsed.port or (443 if parsed.scheme == "https" else 80)
    conn_cls = http.client.HTTPSConnection if parsed.scheme == "https" else http.client.HTTPConnection
    conn = conn_cls(host, port, timeout=timeout)
    req_headers = dict(headers or {})
    if body is not None and "Content-Type" not in req_headers:
        req_headers["Content-Type"] = "application/json"
    try:
        conn.request(method, path, body=body, headers=req_headers)
        resp = conn.getresponse()
        raw = resp.read()
        text = raw.decode("utf-8", errors="replace")
        resp_headers = {k.lower(): v for k, v in resp.getheaders()}
        parsed_json = None
        try:
            parsed_json = json.loads(text)
        except Exception:
            pass
        return {
            "status": resp.status,
            "headers": resp_headers,
            "text": text,
            "json": parsed_json,
        }
    finally:
        conn.close()


def check_sse(base_url: str, path: str, headers: dict[str, str] | None = None, timeout: float = 5.0) -> dict[str, Any]:
    parsed = urllib.parse.urlparse(base_url)
    host = parsed.hostname or "127.0.0.1"
    port = parsed.port or (443 if parsed.scheme == "https" else 80)
    conn_cls = http.client.HTTPSConnection if parsed.scheme == "https" else http.client.HTTPConnection
    conn = conn_cls(host, port, timeout=timeout)
    req_headers = {"Accept": "text/event-stream", **(headers or {})}
    try:
        conn.request("GET", path, headers=req_headers)
        resp = conn.getresponse()
        ct = (resp.getheader("Content-Type") or "").lower()
        ok = resp.status == 200 and "text/event-stream" in ct
        return {
            "path": path,
            "ok": ok,
            "status": resp.status,
            "content_type": ct,
        }
    except Exception as exc:
        return {"path": path, "ok": False, "error": str(exc)}
    finally:
        conn.close()


class SimpleCDPClient:
    """Minimal zero-dependency RFC6455 WebSocket client for Chrome DevTools Protocol."""

    def __init__(self, ws_url: str, timeout: float = 8.0) -> None:
        parsed = urllib.parse.urlparse(ws_url)
        host = parsed.hostname or "127.0.0.1"
        port = parsed.port or 80
        path = parsed.path or "/"
        if parsed.query:
            path = f"{path}?{parsed.query}"

        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.sock.settimeout(timeout)
        self._buf = bytearray()
        self._next_id = 1
        self.events: list[dict[str, Any]] = []

        key = base64.b64encode(os.urandom(16)).decode("ascii")
        handshake = (
            f"GET {path} HTTP/1.1\r\n"
            f"Host: {host}:{port}\r\n"
            f"Upgrade: websocket\r\n"
            f"Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            f"Sec-WebSocket-Version: 13\r\n\r\n"
        ).encode("ascii")
        self.sock.sendall(handshake)

        # Read HTTP 101 response
        header_buf = b""
        while b"\r\n\r\n" not in header_buf:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise RuntimeError("WebSocket handshake closed prematurely")
            header_buf += chunk
        head, leftover = header_buf.split(b"\r\n\r\n", 1)
        self._buf.extend(leftover)
        status_line = head.split(b"\r\n", 1)[0].decode("ascii", errors="replace")
        if " 101 " not in status_line:
            raise RuntimeError(f"WebSocket upgrade failed: {status_line}")

        expected_accept = base64.b64encode(
            hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")).digest()
        ).decode("ascii")
        if expected_accept.encode("ascii") not in head:
            raise RuntimeError("WebSocket Sec-WebSocket-Accept mismatch")

    def close(self) -> None:
        try:
            self._send_frame(0x8, b"")
        except Exception:
            pass
        try:
            self.sock.close()
        except Exception:
            pass

    def _send_frame(self, opcode: int, payload: bytes) -> None:
        mask_key = os.urandom(4)
        length = len(payload)
        header = bytearray([0x80 | (opcode & 0x0F)])
        if length < 126:
            header.append(0x80 | length)
        elif length < 65536:
            header.append(0x80 | 126)
            header.extend(struct.pack("!H", length))
        else:
            header.append(0x80 | 127)
            header.extend(struct.pack("!Q", length))
        header.extend(mask_key)
        masked = bytes(b ^ mask_key[i & 3] for i, b in enumerate(payload))
        self.sock.sendall(bytes(header) + masked)

    def _read_exact(self, n: int) -> bytes:
        while len(self._buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise EOFError("WebSocket connection closed")
            self._buf.extend(chunk)
        out = bytes(self._buf[:n])
        del self._buf[:n]
        return out

    def _recv_message(self) -> str | None:
        fragments: list[bytes] = []
        while True:
            b1, b2 = self._read_exact(2)
            fin = (b1 & 0x80) != 0
            opcode = b1 & 0x0F
            masked = (b2 & 0x80) != 0
            length = b2 & 0x7F
            if length == 126:
                length = struct.unpack("!H", self._read_exact(2))[0]
            elif length == 127:
                length = struct.unpack("!Q", self._read_exact(8))[0]
            mask = self._read_exact(4) if masked else b""
            payload = self._read_exact(length)
            if masked:
                payload = bytes(b ^ mask[i & 3] for i, b in enumerate(payload))

            if opcode == 0x8:  # Close
                return None
            if opcode == 0x9:  # Ping
                self._send_frame(0xA, payload)
                continue
            if opcode == 0xA:  # Pong
                continue
            fragments.append(payload)
            if fin:
                return b"".join(fragments).decode("utf-8", errors="replace")

    def call(self, method: str, params: dict[str, Any] | None = None, timeout: float = 10.0) -> dict[str, Any]:
        cid = self._next_id
        self._next_id += 1
        req = json.dumps({"id": cid, "method": method, "params": params or {}}).encode("utf-8")
        self._send_frame(0x1, req)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            rem = max(0.1, deadline - time.monotonic())
            self.sock.settimeout(rem)
            raw = self._recv_message()
            if raw is None:
                raise EOFError("CDP socket closed")
            msg = json.loads(raw)
            if msg.get("id") == cid:
                if "error" in msg:
                    raise RuntimeError(f"CDP {method} error: {msg['error']}")
                return msg.get("result") or {}
            if "method" in msg:
                self.events.append(msg)
        raise TimeoutError(f"CDP {method} timed out")

    def drain_events(self, duration: float = 2.0) -> None:
        deadline = time.monotonic() + duration
        while time.monotonic() < deadline:
            rem = max(0.05, deadline - time.monotonic())
            self.sock.settimeout(rem)
            try:
                raw = self._recv_message()
                if raw is None:
                    break
                msg = json.loads(raw)
                if "method" in msg:
                    self.events.append(msg)
            except socket.timeout:
                break
            except Exception:
                break


def resolve_chrome(explicit: str | None) -> str | None:
    if explicit and os.path.isfile(explicit) and os.access(explicit, os.X_OK):
        return explicit
    for cand in ("google-chrome", "google-chrome-stable", "chromium", "chromium-browser"):
        found = shutil.which(cand)
        if found:
            return found
    return None


def chrome_cli_screenshot(chrome: str, url: str, out_png: pathlib.Path) -> bool:
    cmd = [
        chrome,
        "--headless=new",
        "--disable-gpu",
        "--no-sandbox",
        "--disable-dev-shm-usage",
        "--window-size=1440,900",
        "--virtual-time-budget=3500",
        f"--screenshot={out_png}",
        url,
    ]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=25)
        return proc.returncode == 0 and out_png.is_file() and out_png.stat().st_size > 0
    except Exception:
        return False


def probe_chrome_cdp(chrome: str, targets: list[tuple[str, str, pathlib.Path]]) -> dict[str, Any]:
    result: dict[str, Any] = {
        "chrome": chrome,
        "available": True,
        "pages": {},
        "console_errors": [],
        "exceptions": [],
        "failed_requests": [],
        "cdp_ok": False,
    }
    user_data = tempfile.mkdtemp(prefix="herdr2-qa-chrome-")
    proc: subprocess.Popen[str] | None = None
    try:
        proc = subprocess.Popen(
            [
                chrome,
                "--headless=new",
                "--disable-gpu",
                "--no-sandbox",
                "--disable-dev-shm-usage",
                "--remote-debugging-port=0",
                f"--user-data-dir={user_data}",
                "--window-size=1440,900",
                "about:blank",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
        port_file = pathlib.Path(user_data) / "DevToolsActivePort"
        debug_port = None
        deadline = time.monotonic() + 12.0
        while time.monotonic() < deadline:
            if port_file.is_file():
                lines = port_file.read_text(encoding="utf-8", errors="replace").splitlines()
                if lines and lines[0].strip().isdigit():
                    debug_port = int(lines[0].strip())
                    break
            if proc.poll() is not None:
                break
            time.sleep(0.15)

        if debug_port is None:
            raise RuntimeError("DevToolsActivePort did not appear")

        ws_url = None
        for _ in range(25):
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{debug_port}/json/list", timeout=3) as resp:
                    items = json.loads(resp.read().decode("utf-8"))
                page = next((i for i in items if i.get("type") == "page" and i.get("webSocketDebuggerUrl")), None)
                if page:
                    ws_url = page["webSocketDebuggerUrl"]
                    break
            except Exception:
                time.sleep(0.2)
        if not ws_url:
            raise RuntimeError("No debuggable page target returned by Chrome")

        cdp = SimpleCDPClient(ws_url, timeout=10.0)
        try:
            cdp.call("Runtime.enable")
            cdp.call("Log.enable")
            cdp.call("Network.enable")
            cdp.call("Page.enable")
            cdp.call(
                "Emulation.setDeviceMetricsOverride",
                {"width": 1440, "height": 900, "deviceScaleFactor": 1, "mobile": False},
            )
            result["cdp_ok"] = True

            for label, url, png_path in targets:
                cdp.events.clear()
                cdp.call("Page.navigate", {"url": url})
                cdp.drain_events(2.5)

                shot_ok = False
                try:
                    shot = cdp.call("Page.captureScreenshot", {"format": "png"}, timeout=10.0)
                    b64 = shot.get("data")
                    if b64:
                        png_path.parent.mkdir(parents=True, exist_ok=True)
                        png_path.write_bytes(base64.b64decode(b64))
                        shot_ok = png_path.is_file() and png_path.stat().st_size > 0
                except Exception:
                    shot_ok = False

                page_console_errs: list[str] = []
                page_exceptions: list[str] = []
                page_failed_reqs: list[dict[str, Any]] = []

                for ev in cdp.events:
                    m = ev.get("method")
                    p = ev.get("params") or {}
                    if m == "Runtime.consoleAPICalled" and p.get("type") == "error":
                        args = p.get("args") or []
                        msg = " ".join(str(a.get("value") or a.get("description") or "") for a in args).strip()
                        page_console_errs.append(msg or "console.error")
                    elif m == "Log.entryAdded":
                        entry = p.get("entry") or {}
                        if entry.get("level") == "error":
                            page_console_errs.append(str(entry.get("text") or "log.error"))
                    elif m == "Runtime.exceptionThrown":
                        details = p.get("exceptionDetails") or {}
                        exc = details.get("exception") or {}
                        desc = exc.get("description") or details.get("text") or "uncaught exception"
                        page_exceptions.append(str(desc))
                    elif m == "Network.loadingFailed":
                        if not p.get("canceled"):
                            page_failed_reqs.append(
                                {
                                    "page": label,
                                    "requestId": p.get("requestId"),
                                    "errorText": p.get("errorText"),
                                    "type": p.get("type"),
                                }
                            )
                    elif m == "Network.responseReceived":
                        resp_obj = p.get("response") or {}
                        status = int(resp_obj.get("status") or 0)
                        if status >= 400:
                            page_failed_reqs.append(
                                {
                                    "page": label,
                                    "url": resp_obj.get("url"),
                                    "status": status,
                                }
                            )

                result["pages"][label] = {
                    "url": url,
                    "screenshot": str(png_path) if shot_ok else None,
                    "console_errors": page_console_errs,
                    "exceptions": page_exceptions,
                    "failed_requests": page_failed_reqs,
                }
                result["console_errors"].extend([f"[{label}] {e}" for e in page_console_errs])
                result["exceptions"].extend([f"[{label}] {e}" for e in page_exceptions])
                result["failed_requests"].extend(page_failed_reqs)
        finally:
            cdp.close()
    except Exception as exc:
        result["cdp_error"] = str(exc)
    finally:
        if proc is not None and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
        shutil.rmtree(user_data, ignore_errors=True)

    # Guarantee screenshots via CLI fallback if CDP didn't write them.
    for label, url, png_path in targets:
        page_info = result["pages"].setdefault(label, {"url": url, "screenshot": None})
        if not page_info.get("screenshot") or not png_path.is_file():
            png_path.parent.mkdir(parents=True, exist_ok=True)
            if chrome_cli_screenshot(chrome, url, png_path):
                page_info["screenshot"] = str(png_path)

    return result


def main() -> int:
    parser = argparse.ArgumentParser(description="Probe Drove Web HTTP/SSE endpoints and headless Chrome rendering")
    parser.add_argument("--url", required=True, help="Base Drove Web URL, e.g. http://127.0.0.1:7799")
    parser.add_argument("--out-dir", required=True, help="Directory to write screenshots and web_probe.json")
    parser.add_argument("--chrome", default=None, help="Optional explicit path to google-chrome / chromium binary")
    args = parser.parse_args()

    base_url = args.url.rstrip("/")
    out_dir = pathlib.Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    report: dict[str, Any] = {
        "url": base_url,
        "ok": True,
        "checks": {},
        "chrome": {"available": False},
    }

    # 1. GET / and extract window.__DROVE_TOKEN__
    token = ""
    try:
        idx = http_request(base_url, "GET", "/")
        m = re.search(r'window\.__DROVE_TOKEN__\s*=\s*"([^"]+)"', idx["text"])
        token = m.group(1) if m else ""
        index_ok = idx["status"] == 200 and bool(token)
        report["checks"]["index_html"] = {
            "ok": index_ok,
            "status": idx["status"],
            "has_token": bool(token),
            "bytes": len(idx["text"]),
        }
        if not index_ok:
            report["ok"] = False
    except Exception as exc:
        report["checks"]["index_html"] = {"ok": False, "error": str(exc)}
        report["ok"] = False

    auth_headers = {}
    if token:
        auth_headers["Authorization"] = f"Bearer {token}"
        auth_headers["x-drove-token"] = token

    # 2. API GET endpoints
    first_run_id = None
    for name, path in (("api_runs", "/api/runs"), ("api_agents", "/api/agents")):
        try:
            res = http_request(base_url, "GET", path, headers=auth_headers)
            ok = res["status"] == 200 and res["json"] is not None
            report["checks"][name] = {
                "ok": ok,
                "status": res["status"],
                "json_type": type(res["json"]).__name__ if res["json"] is not None else None,
            }
            if name == "api_runs" and ok:
                runs_list = res["json"].get("runs") if isinstance(res["json"], dict) else res["json"]
                if isinstance(runs_list, list) and runs_list and isinstance(runs_list[0], dict):
                    first_run_id = runs_list[0].get("id")
            if not ok:
                report["ok"] = False
        except Exception as exc:
            report["checks"][name] = {"ok": False, "error": str(exc)}
            report["ok"] = False

    # Optional /api/daemon or /api/daemon/events
    daemon_res = None
    for dpath in ("/api/daemon", "/api/daemon/events"):
        try:
            res = http_request(base_url, "GET", dpath, headers=auth_headers)
            if res["status"] == 200 and res["json"] is not None:
                daemon_res = {"ok": True, "path": dpath, "status": res["status"]}
                break
            daemon_res = {"ok": False, "path": dpath, "status": res["status"]}
        except Exception as exc:
            daemon_res = {"ok": False, "path": dpath, "error": str(exc)}
    report["checks"]["api_daemon"] = daemon_res or {"ok": False, "error": "not probed"}
    if not report["checks"]["api_daemon"]["ok"]:
        report["ok"] = False

    # 3. Read-only protection contract (405 + configured_in_app)
    for name, path in (("readonly_put_agents", "/api/agents"), ("readonly_put_ai_config", "/api/ai/config")):
        try:
            res = http_request(base_url, "PUT", path, headers=auth_headers, body=b"{}")
            ok = res["status"] == 405 and "configured_in_app" in res["text"]
            report["checks"][name] = {
                "ok": ok,
                "status": res["status"],
                "has_configured_in_app": "configured_in_app" in res["text"],
                "body_excerpt": res["text"][:200],
            }
            if not ok:
                report["ok"] = False
        except Exception as exc:
            report["checks"][name] = {"ok": False, "error": str(exc)}
            report["ok"] = False

    # 4. SSE stream connectivity
    sse_events = check_sse(base_url, "/api/events", headers=auth_headers)
    report["checks"]["sse_events"] = sse_events
    if not sse_events.get("ok"):
        report["ok"] = False
    if first_run_id:
        sse_run = check_sse(base_url, f"/api/runs/{urllib.parse.quote(str(first_run_id), safe='')}/events", headers=auth_headers)
        report["checks"]["sse_run_events"] = sse_run
        if not sse_run.get("ok"):
            report["ok"] = False

    # 5. Headless Chrome probe
    chrome_bin = resolve_chrome(args.chrome)
    if chrome_bin:
        targets = [
            ("live", f"{base_url}/", out_dir / "drove-web-live.png"),
            ("mock", f"{base_url}/?mock=1", out_dir / "drove-web-mock.png"),
        ]
        chrome_res = probe_chrome_cdp(chrome_bin, targets)
        report["chrome"] = chrome_res
        if chrome_res.get("exceptions") or chrome_res.get("console_errors"):
            report["ok"] = False
    else:
        report["chrome"] = {"available": False, "reason": "google-chrome/chromium not found in PATH"}

    (out_dir / "web_probe.json").write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2, ensure_ascii=False))
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
