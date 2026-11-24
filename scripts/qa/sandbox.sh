#!/usr/bin/env bash
# Fully isolated QA sandbox controller for Herdr2 (Core + App + Drove Web).
# Never touches host H2_SESSION, ~/.config/herdr2*, ~/.local/state/herdr2,
# $XDG_RUNTIME_DIR/herdr2, or the real desktop.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
MAIN_REPO="$REPO_ROOT"
if git_common="$(git -C "$REPO_ROOT" rev-parse --git-common-dir 2>/dev/null)"; then
  git_common_abs="$(cd "$REPO_ROOT" && cd "$git_common" 2>/dev/null && pwd || true)"
  if [[ -n "$git_common_abs" && "$(basename "$git_common_abs")" == ".git" ]]; then
    MAIN_REPO="$(dirname "$git_common_abs")"
  fi
fi

SANDBOX_NAME="${HERDR2_QA_SANDBOX:-default}"
# Sanitize sandbox name to safe filesystem/session characters
if [[ ! "$SANDBOX_NAME" =~ ^[a-zA-Z0-9._-]+$ ]]; then
  echo "sandbox.sh: invalid HERDR2_QA_SANDBOX='$SANDBOX_NAME'" >&2
  exit 2
fi

ROOT="/var/tmp/herdr2-qa-${SANDBOX_NAME}"
SESSION_ID="herdr2-qa-${SANDBOX_NAME}"
BIN_DIR="$ROOT/bin"
CONFIG_DIR="$ROOT/config"
STATE_DIR="$ROOT/state"
RUN_DIR="$ROOT/run"
APP_HOME="$ROOT/app-home"
OPENED_LOG="$ROOT/opened.log"
PORT_FILE="$ROOT/drove-port"
APP_PID_FILE="$ROOT/app.pid"
APP_LOG="$ROOT/app.log"
DISPLAY_PID_FILE="$ROOT/display.pid"
DISPLAY_ENV_FILE="$ROOT/display.env"

resolve_core_bin() {
  if [[ -n "${HERDR2_QA_CORE_BIN:-}" && -x "${HERDR2_QA_CORE_BIN}" ]]; then
    echo "$HERDR2_QA_CORE_BIN"
    return 0
  fi
  local cand
  for cand in \
    "$REPO_ROOT/herdr-core/target/debug/herdr2" \
    "$REPO_ROOT/herdr-core/target/release/herdr2" \
    "$MAIN_REPO/herdr-core/target/debug/herdr2" \
    "$MAIN_REPO/herdr-core/target/release/herdr2"; do
    if [[ -x "$cand" ]]; then
      echo "$cand"
      return 0
    fi
  done
  local found
  found="$(command -v h2 2>/dev/null || true)"
  if [[ -n "$found" && -x "$found" ]]; then
    echo "$found"
    return 0
  fi
  echo "sandbox.sh: cannot locate herdr2 core binary (checked target/debug, target/release, PATH h2)" >&2
  return 1
}

resolve_app_bin() {
  if [[ -n "${HERDR2_QA_APP_BIN:-}" && -x "${HERDR2_QA_APP_BIN}" ]]; then
    echo "$HERDR2_QA_APP_BIN"
    return 0
  fi
  local cand
  for cand in \
    "$REPO_ROOT/herdr-app/target/debug/herdr2" \
    "$REPO_ROOT/herdr-app/target/release/herdr2" \
    "$MAIN_REPO/herdr-app/target/debug/herdr2" \
    "$MAIN_REPO/herdr-app/target/release/herdr2"; do
    if [[ -x "$cand" ]]; then
      echo "$cand"
      return 0
    fi
  done
  echo "sandbox.sh: cannot locate herdr2 desktop app binary (checked herdr-app/target/debug and release)" >&2
  return 1
}

resolve_drove_web_dir() {
  if [[ -d "$REPO_ROOT/drove-web/dist" ]]; then
    echo "$REPO_ROOT/drove-web/dist"
  elif [[ -d "$MAIN_REPO/drove-web/dist" ]]; then
    echo "$MAIN_REPO/drove-web/dist"
  else
    echo "$REPO_ROOT/drove-web/dist"
  fi
}

pick_free_port() {
  python3 -c '
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", 0))
port = s.getsockname()[1]
s.close()
print(port)
'
}

pick_free_x_display() {
  local n
  for n in $(seq 99 199); do
    if [[ ! -e "/tmp/.X${n}-lock" && ! -e "/tmp/.X11-unix/X${n}" ]]; then
      echo ":${n}"
      return 0
    fi
  done
  echo "sandbox.sh: no free X11 display found in :99..:199" >&2
  return 1
}

ensure_sandbox_env() {
  mkdir -p "$BIN_DIR" "$CONFIG_DIR" "$STATE_DIR" "$RUN_DIR" "$APP_HOME"
  chmod 700 "$RUN_DIR"

  CORE_BIN="$(resolve_core_bin)"
  ln -sfn "$CORE_BIN" "$BIN_DIR/h2"
  ln -sfn "$CORE_BIN" "$BIN_DIR/herdr2-core"

  # Stub xdg-open / open so sandbox never opens host browser windows.
  cat > "$BIN_DIR/xdg-open" <<EOF
#!/usr/bin/env bash
printf '%s %s\n' "\$(date -Iseconds)" "\$*" >> "$OPENED_LOG"
exit 0
EOF
  chmod +x "$BIN_DIR/xdg-open"
  ln -sfn "$BIN_DIR/xdg-open" "$BIN_DIR/open"

  if [[ -f "$PORT_FILE" ]]; then
    DROVE_PORT="$(tr -d '[:space:]' < "$PORT_FILE")"
  else
    DROVE_PORT="$(pick_free_port)"
    printf '%s\n' "$DROVE_PORT" > "$PORT_FILE"
  fi

  # Strip host parent session & Drove worker identities to guarantee total isolation.
  unset H2_AGENT_ID H2_AGENT_SOCKET H2_TERMINAL_ID H2_TERMINAL_SOCKET H2_SCOPE
  unset H2_DROVE_URL H2_DROVE_WORKER_TOKEN H2_DROVE_WORKER_ID H2_DROVE_ATTEMPT_ID

  export H2_SESSION="$SESSION_ID"
  export XDG_CONFIG_HOME="$CONFIG_DIR"
  export XDG_STATE_HOME="$STATE_DIR"
  export XDG_RUNTIME_DIR="$RUN_DIR"
  export HERDR2_APP_HOME="$APP_HOME"
  export HERDR2_APP_NO_SOUND=1
  export H2_DROVE_NO_OPEN=1
  export H2_DROVE_PORT="$DROVE_PORT"
  export H2_DROVE_WEB_DIR="$(resolve_drove_web_dir)"
  export PATH="$BIN_DIR:$PATH"
}

load_display_env() {
  if [[ -f "$DISPLAY_ENV_FILE" ]]; then
    # shellcheck disable=SC1090
    source "$DISPLAY_ENV_FILE"
  fi
}

cmd_core_up() {
  ensure_sandbox_env
  "$CORE_BIN" server start >/dev/null
  local status_json
  status_json="$("$CORE_BIN" server status --json)"
  printf '%s\n' "$status_json"
}

cmd_core_down() {
  ensure_sandbox_env
  "$CORE_BIN" server stop >/dev/null 2>&1 || true
}

start_headless_display() {
  if [[ -f "$DISPLAY_PID_FILE" ]]; then
    local old_pid
    old_pid="$(cat "$DISPLAY_PID_FILE" 2>/dev/null || true)"
    if [[ -n "$old_pid" ]] && kill -0 "$old_pid" 2>/dev/null; then
      load_display_env
      return 0
    fi
    rm -f "$DISPLAY_PID_FILE" "$DISPLAY_ENV_FILE"
  fi

  local mode="${HERDR2_QA_DISPLAY:-xvfb}"
  if [[ "$mode" == "weston" ]] && command -v weston >/dev/null 2>&1; then
    local sock="herdr2-qa-wayland-$$"
    if command -v setsid >/dev/null 2>&1; then
      setsid weston --backend=headless-backend.so --width=1440 --height=900 --socket="$sock" \
        >"$ROOT/display.log" 2>&1 </dev/null &
    else
      weston --backend=headless-backend.so --width=1440 --height=900 --socket="$sock" \
        >"$ROOT/display.log" 2>&1 </dev/null &
    fi
    local dpid=$!
    echo "$dpid" > "$DISPLAY_PID_FILE"
    cat > "$DISPLAY_ENV_FILE" <<EOF
export WAYLAND_DISPLAY="$sock"
unset DISPLAY
EOF
    load_display_env
    local i
    for i in $(seq 1 50); do
      [[ -S "$RUN_DIR/$sock" ]] && return 0
      sleep 0.1
    done
    echo "sandbox.sh: weston failed to create wayland socket $RUN_DIR/$sock" >&2
    tail -n 40 "$ROOT/display.log" >&2 || true
    return 1
  fi

  if ! command -v Xvfb >/dev/null 2>&1; then
    echo "sandbox.sh: Xvfb not found in PATH" >&2
    return 1
  fi

  local disp
  disp="$(pick_free_x_display)"
  if command -v setsid >/dev/null 2>&1; then
    setsid Xvfb "$disp" -screen 0 1440x900x24 -nolisten tcp >"$ROOT/display.log" 2>&1 </dev/null &
  else
    Xvfb "$disp" -screen 0 1440x900x24 -nolisten tcp >"$ROOT/display.log" 2>&1 </dev/null &
  fi
  local dpid=$!
  echo "$dpid" > "$DISPLAY_PID_FILE"
  cat > "$DISPLAY_ENV_FILE" <<EOF
export DISPLAY="$disp"
export WAYLAND_DISPLAY=""
EOF
  load_display_env

  local sock_num="${disp#:}"
  local i
  for i in $(seq 1 50); do
    if [[ -S "/tmp/.X11-unix/X${sock_num}" ]] && kill -0 "$dpid" 2>/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  echo "sandbox.sh: Xvfb failed to start on $disp" >&2
  tail -n 40 "$ROOT/display.log" >&2 || true
  return 1
}

stop_headless_display() {
  if [[ -f "$DISPLAY_PID_FILE" ]]; then
    local dpid
    dpid="$(cat "$DISPLAY_PID_FILE" 2>/dev/null || true)"
    if [[ -n "$dpid" ]] && kill -0 "$dpid" 2>/dev/null; then
      kill -TERM -- "-$dpid" 2>/dev/null || kill -TERM "$dpid" 2>/dev/null || true
      local i
      for i in $(seq 1 20); do
        kill -0 "$dpid" 2>/dev/null || break
        sleep 0.1
      done
      kill -KILL -- "-$dpid" 2>/dev/null || kill -KILL "$dpid" 2>/dev/null || true
    fi
    rm -f "$DISPLAY_PID_FILE" "$DISPLAY_ENV_FILE"
  fi
}

cmd_app_up() {
  cmd_core_up >/dev/null
  APP_BIN="$(resolve_app_bin)"

  # Write isolated settings.json pointing directly at our core binary.
  python3 - "$APP_HOME/settings.json" "$APP_HOME/shared.json" "$CORE_BIN" <<'PY'
import json, pathlib, sys
dst = pathlib.Path(sys.argv[1])
shared = pathlib.Path(sys.argv[2])
core_bin = sys.argv[3]
data = {
    "core": core_bin,
    "telemetry": False,
    "sounds": {"enabled": False},
}
dst.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
if not shared.exists():
    shared.write_text(json.dumps({"frame.welcomed": "finished"}) + "\n", encoding="utf-8")
PY

  start_headless_display

  if [[ -z "${VK_ICD_FILENAMES:-}" && -f "/usr/share/vulkan/icd.d/lvp_icd.json" ]]; then
    export VK_ICD_FILENAMES="/usr/share/vulkan/icd.d/lvp_icd.json"
  fi
  export HERDR2_DEV_DEFAULTS="$REPO_ROOT/herdr-app/defaults"

  # If already running, skip re-launching.
  if [[ -f "$APP_PID_FILE" ]]; then
    local old_app_pid
    old_app_pid="$(cat "$APP_PID_FILE" 2>/dev/null || true)"
    if [[ -n "$old_app_pid" ]] && kill -0 "$old_app_pid" 2>/dev/null; then
      if "$APP_BIN" status >/dev/null 2>&1; then
        "$APP_BIN" status
        return 0
      fi
    fi
  fi

  if command -v setsid >/dev/null 2>&1; then
    setsid "$APP_BIN" >"$APP_LOG" 2>&1 </dev/null &
  else
    "$APP_BIN" >"$APP_LOG" 2>&1 </dev/null &
  fi
  local apid=$!
  echo "$apid" > "$APP_PID_FILE"

  local timeout_sec="${HERDR2_QA_APP_TIMEOUT:-35}"
  local deadline=$(( $(date +%s) + timeout_sec ))
  local status_ok=0
  local shot_ok=0
  local ready_shot="$ROOT/ready-shot.png"

  while (( $(date +%s) < deadline )); do
    if ! kill -0 "$apid" 2>/dev/null; then
      echo "sandbox.sh: app exited prematurely (pid=$apid)" >&2
      tail -n 80 "$APP_LOG" >&2 || true
      return 1
    fi
    if [[ "$status_ok" -eq 0 ]]; then
      if "$APP_BIN" status 2>/dev/null | python3 -c 'import json,sys; json.load(sys.stdin)' 2>/dev/null; then
        status_ok=1
      fi
    fi
    if [[ "$status_ok" -eq 1 && "$shot_ok" -eq 0 ]]; then
      rm -f "$ready_shot"
      if "$APP_BIN" shot "$ready_shot" >/dev/null 2>&1 && [[ -s "$ready_shot" ]]; then
        shot_ok=1
        break
      fi
    fi
    sleep 0.3
  done

  if [[ "$status_ok" -ne 1 || "$shot_ok" -ne 1 ]]; then
    echo "sandbox.sh: timed out waiting for app ready (status_ok=$status_ok shot_ok=$shot_ok)" >&2
    tail -n 80 "$APP_LOG" >&2 || true
    return 1
  fi

  "$APP_BIN" status
}

cmd_app_down() {
  ensure_sandbox_env
  if [[ -f "$APP_PID_FILE" ]]; then
    local apid
    apid="$(cat "$APP_PID_FILE" 2>/dev/null || true)"
    if [[ -n "$apid" ]] && kill -0 "$apid" 2>/dev/null; then
      kill -TERM -- "-$apid" 2>/dev/null || kill -TERM "$apid" 2>/dev/null || true
      local i
      for i in $(seq 1 25); do
        kill -0 "$apid" 2>/dev/null || break
        sleep 0.1
      done
      kill -KILL -- "-$apid" 2>/dev/null || kill -KILL "$apid" 2>/dev/null || true
    fi
    rm -f "$APP_PID_FILE"
  fi
  stop_headless_display
}

cmd_h2() {
  ensure_sandbox_env
  exec "$CORE_BIN" "$@"
}

cmd_app() {
  ensure_sandbox_env
  load_display_env
  APP_BIN="$(resolve_app_bin)"
  if [[ -z "${VK_ICD_FILENAMES:-}" && -f "/usr/share/vulkan/icd.d/lvp_icd.json" ]]; then
    export VK_ICD_FILENAMES="/usr/share/vulkan/icd.d/lvp_icd.json"
  fi
  export HERDR2_DEV_DEFAULTS="$REPO_ROOT/herdr-app/defaults"
  exec "$APP_BIN" "$@"
}

cmd_drove_url() {
  ensure_sandbox_env
  local port="$DROVE_PORT"
  if [[ "$port" == "0" || -z "$port" ]]; then
    port="$("$CORE_BIN" drove settings --json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("webPort") or "")' 2>/dev/null || true)"
  fi
  echo "http://127.0.0.1:${port}"
}

cmd_invariants() {
  ensure_sandbox_env
  load_display_env
  local app_bin=""
  app_bin="$(resolve_app_bin 2>/dev/null || true)"
  python3 - "$SANDBOX_NAME" "$ROOT" "$SESSION_ID" "$CORE_BIN" "$app_bin" "$APP_PID_FILE" "$APP_LOG" <<'PY'
import json
import os
import pathlib
import re
import subprocess
import sys

sandbox_name, root, session_id, core_bin, app_bin, app_pid_file, app_log = sys.argv[1:8]

report = {
    "sandbox": sandbox_name,
    "root": root,
    "session": session_id,
    "ok": True,
    "core": {"healthy": False, "status": None, "error": None},
    "app": {"running": False, "errors": [], "panics": [], "status_error": None},
    "consistency": {"checked": False, "ok": True, "details": {}},
    "active_processes": [],
}

# 1. Core health
try:
    p = subprocess.run([core_bin, "server", "status", "--json"], capture_output=True, text=True, timeout=10)
    if p.returncode == 0:
        data = json.loads(p.stdout)
        report["core"]["status"] = data
        report["core"]["healthy"] = bool(data.get("running", True))
    else:
        report["core"]["error"] = (p.stderr or p.stdout).strip()
except Exception as exc:
    report["core"]["error"] = str(exc)

if not report["core"]["healthy"]:
    report["ok"] = False

# 2. App status & panic log scan
app_pid = None
pid_path = pathlib.Path(app_pid_file)
if pid_path.is_file():
    try:
        cand = int(pid_path.read_text(encoding="utf-8").strip())
        os.kill(cand, 0)
        app_pid = cand
        report["app"]["running"] = True
    except Exception:
        app_pid = None

if report["app"]["running"] and app_bin:
    try:
        p = subprocess.run([app_bin, "status"], capture_output=True, text=True, timeout=10)
        if p.returncode == 0:
            st = json.loads(p.stdout)
            errs = st.get("errors") or []
            report["app"]["errors"] = errs
            if errs:
                report["ok"] = False
        else:
            report["app"]["status_error"] = (p.stderr or p.stdout).strip()
            report["ok"] = False
    except Exception as exc:
        report["app"]["status_error"] = str(exc)
        report["ok"] = False

log_path = pathlib.Path(app_log)
if log_path.is_file():
    panic_re = re.compile(r"(PANIC|thread\s+'.*?'\s+panicked|panicked at)", re.IGNORECASE)
    for idx, line in enumerate(log_path.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
        if panic_re.search(line):
            report["app"]["panics"].append({"line": idx, "text": line[:300]})
    if report["app"]["panics"]:
        report["ok"] = False

# 3. Consistency between h2 ls --json and app ls
if report["core"]["healthy"] and report["app"]["running"] and app_bin:
    report["consistency"]["checked"] = True
    try:
        p_core = subprocess.run([core_bin, "ls", "--json"], capture_output=True, text=True, timeout=10)
        p_app = subprocess.run([app_bin, "ls"], capture_output=True, text=True, timeout=10)
        if p_core.returncode == 0 and p_app.returncode == 0:
            core_ls = json.loads(p_core.stdout)
            app_ls = json.loads(p_app.stdout)
            core_agents = {a.get("address") for a in core_ls.get("agents", []) if isinstance(a, dict)}
            core_terms = {t.get("address") for t in core_ls.get("terminals", []) if isinstance(t, dict)}
            app_refs = set()
            for sp in app_ls.get("spaces", []) if isinstance(app_ls, dict) else []:
                for tb in sp.get("tabs", []) if isinstance(sp, dict) else []:
                    for pn in tb.get("panes", []) if isinstance(tb, dict) else []:
                        term_ref = pn.get("terminal") if isinstance(pn, dict) else None
                        if isinstance(term_ref, str):
                            app_refs.add(term_ref)
                        elif isinstance(term_ref, dict) and term_ref.get("address"):
                            app_refs.add(term_ref["address"])
            core_all = core_agents | core_terms
            dangling = sorted(app_refs - core_all)
            report["consistency"]["details"] = {
                "core_agents": len(core_agents),
                "core_terminals": len(core_terms),
                "app_pane_terminals": len(app_refs),
                "dangling_app_terminals": dangling,
            }
            if dangling:
                report["consistency"]["ok"] = False
                report["ok"] = False
        else:
            report["consistency"]["ok"] = False
            report["consistency"]["details"] = {
                "core_rc": p_core.returncode,
                "app_rc": p_app.returncode,
            }
            report["ok"] = False
    except Exception as exc:
        report["consistency"]["ok"] = False
        report["consistency"]["details"] = {"error": str(exc)}
        report["ok"] = False

# 4. Active sandbox processes (via /proc/<pid>/environ)
self_pid = os.getpid()
ppid = os.getppid()
marker = f"H2_SESSION={session_id}".encode("utf-8")
for entry in pathlib.Path("/proc").iterdir():
    if not entry.name.isdigit():
        continue
    pid = int(entry.name)
    if pid in (self_pid, ppid):
        continue
    try:
        env_bytes = (entry / "environ").read_bytes()
        if marker in env_bytes.split(b"\x00"):
            cmd = (entry / "cmdline").read_bytes().replace(b"\x00", b" ").decode("utf-8", errors="replace").strip()
            report["active_processes"].append({"pid": pid, "cmd": cmd[:200]})
    except Exception:
        continue

print(json.dumps(report, indent=2, ensure_ascii=False))
sys.exit(0 if report["ok"] else 1)
PY
}

cmd_clean() {
  if [[ -d "$ROOT" ]]; then
    cmd_app_down || true
    cmd_core_down || true
  else
    stop_headless_display || true
  fi

  # Terminate any remaining processes carrying this sandbox's H2_SESSION env var.
  python3 - "$SESSION_ID" "$ROOT" <<'PY'
import os
import pathlib
import signal
import sys
import time

session_id, root = sys.argv[1], sys.argv[2]
marker = f"H2_SESSION={session_id}".encode("utf-8")

# Build ancestor PID set so we never kill our own shell chain.
ancestors = set()
cur = os.getpid()
while cur > 1 and cur not in ancestors:
    ancestors.add(cur)
    try:
        stat = pathlib.Path(f"/proc/{cur}/stat").read_text(encoding="utf-8", errors="replace")
        # Format: pid (comm) state ppid ...
        rparen = stat.rfind(")")
        parts = stat[rparen + 2 :].split()
        cur = int(parts[1])
    except Exception:
        break

def find_sandbox_pids():
    found = []
    for entry in pathlib.Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        pid = int(entry.name)
        if pid in ancestors:
            continue
        try:
            env_bytes = (entry / "environ").read_bytes()
            if marker in env_bytes.split(b"\x00"):
                found.append(pid)
        except Exception:
            continue
    return found

pids = find_sandbox_pids()
for pid in pids:
    try:
        os.kill(pid, signal.SIGTERM)
    except OSError:
        pass

if pids:
    deadline = time.monotonic() + 3.0
    while time.monotonic() < deadline:
        remaining = find_sandbox_pids()
        if not remaining:
            break
        time.sleep(0.1)
    for pid in find_sandbox_pids():
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass
    time.sleep(0.2)

leftover = find_sandbox_pids()
if leftover:
    print(f"sandbox.sh: warning: leftover sandbox processes: {leftover}", file=sys.stderr)
    sys.exit(1)
PY

  rm -rf "$ROOT"
}

usage() {
  cat <<'EOF'
Usage: scripts/qa/sandbox.sh <command> [args...]

Commands:
  core-up           Start isolated h2 core daemon and verify status
  core-down         Stop isolated h2 core daemon
  app-up            Start isolated core + headless Xvfb/weston + Herdr2 App
  app-down          Stop isolated Herdr2 App and headless display
  h2 <args...>      Run isolated h2 core CLI command
  app <args...>     Run isolated Herdr2 App CLI command (ls/status/do/shot/...)
  drove-url         Print isolated Drove Web URL (http://127.0.0.1:<port>)
  invariants        Print JSON health, panic, consistency, and process report
  clean             Stop everything, verify zero orphan processes, remove sandbox root

Environment:
  HERDR2_QA_SANDBOX       Sandbox instance name (default: default)
  HERDR2_QA_DISPLAY       Display backend: xvfb (default) or weston
  HERDR2_QA_APP_TIMEOUT   Seconds to wait for App readiness (default: 35)
  HERDR2_QA_CORE_BIN      Override core binary path
  HERDR2_QA_APP_BIN       Override app binary path
EOF
}

if [[ $# -lt 1 ]]; then
  usage >&2
  exit 2
fi

subcmd="$1"
shift

case "$subcmd" in
  core-up)    cmd_core_up "$@" ;;
  core-down)  cmd_core_down "$@" ;;
  app-up)     cmd_app_up "$@" ;;
  app-down)   cmd_app_down "$@" ;;
  h2)         cmd_h2 "$@" ;;
  app)        cmd_app "$@" ;;
  drove-url)  cmd_drove_url "$@" ;;
  invariants) cmd_invariants "$@" ;;
  clean)      cmd_clean "$@" ;;
  -h|--help|help) usage ;;
  *)
    echo "sandbox.sh: unknown command '$subcmd'" >&2
    usage >&2
    exit 2
    ;;
esac
