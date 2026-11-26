#!/usr/bin/env bash
# Fast development workflow for Herdr2 (Linux / WSL2 / macOS):
#   scripts/dev.sh ui     Zero-compile live hot-reload for defaults/{mods,frame,layouts,themes,icons}
#   scripts/dev.sh web    Live vite build --watch for drove-web (zero Rust recompile)
#   scripts/dev.sh core   Fast debug build of herdr-core + zero-downtime h2 server restart
#   scripts/dev.sh app    Fast debug build of herdr-app + hot restart window (terminals stay in h2)
#   scripts/dev.sh win    Fast incremental Windows cross-build (cargo-xwin) + stage to releases dir
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CMD="${1:-help}"
shift || true

export HERDR2_DEV_DEFAULTS="$ROOT/herdr-app/defaults"

find_app_bin() {
  if [[ -x "$ROOT/herdr-app/target/debug/herdr2" ]]; then
    echo "$ROOT/herdr-app/target/debug/herdr2"
  elif [[ -x "$ROOT/herdr-app/target/release/herdr2" ]]; then
    echo "$ROOT/herdr-app/target/release/herdr2"
  elif command -v herdr2 >/dev/null 2>&1; then
    command -v herdr2
  else
    echo ""
  fi
}

case "$CMD" in
  ui)
    BIN="$(find_app_bin)"
    if [[ -z "$BIN" ]]; then
      echo "[dev-ui] No herdr2 binary found yet; building fast debug binary once..."
      (cd "$ROOT/herdr-app" && cargo build --locked)
      BIN="$ROOT/herdr-app/target/debug/herdr2"
    fi
    echo "[dev-ui] Live hot-reload enabled for: $HERDR2_DEV_DEFAULTS"
    echo "[dev-ui] Edit any file under herdr-app/defaults/{mods,frame,layouts,themes,icons} and save!"
    exec "$BIN" "$@"
    ;;

  web)
    echo "[dev-web] Watching drove-web -> $ROOT/drove-web/dist (herdr-core serves disk dist live)"
    cd "$ROOT/drove-web"
    if [[ ! -d node_modules ]]; then
      npm ci
    fi
    exec npm run build -- --watch "$@"
    ;;

  core)
    echo "[dev-core] Fast incremental debug build (skipping drove-web rebuild)..."
    (cd "$ROOT/herdr-core" && H2_SKIP_WEB_BUILD=1 cargo build --locked --bin herdr2)
    BIN_DIR="${H2_INSTALL_DIR:-$HOME/.local/bin}"
    mkdir -p "$BIN_DIR"
    CORE_BIN="$BIN_DIR/herdr2-core"
    if [[ -f "$BIN_DIR/herdr2" && ! -L "$BIN_DIR/herdr2" ]] && "$BIN_DIR/herdr2" --version 2>/dev/null | grep -q '^h2 '; then
      CORE_BIN="$BIN_DIR/herdr2"
      ln -sfn "$ROOT/herdr-core/target/debug/herdr2" "$BIN_DIR/herdr2"
      ln -sfn herdr2 "$BIN_DIR/herdr2-core"
      ln -sfn herdr2 "$BIN_DIR/h2"
    else
      ln -sfn "$ROOT/herdr-core/target/debug/herdr2" "$BIN_DIR/herdr2-core"
      ln -sfn herdr2-core "$BIN_DIR/h2"
    fi
    echo "[dev-core] Hot-restarting h2 daemon (all running terminals & kept agents stay alive)..."
    H2_DROVE_WEB_DIR="$ROOT/drove-web/dist" "$ROOT/herdr-core/scripts/restart-core.sh" "$CORE_BIN"
    echo "[dev-core] Done: $("$BIN_DIR/h2" --version)"
    ;;

  hot-sync)
    # Unified one-command hot-update for WSL + Windows:
    #   scripts/dev.sh hot-sync ui                            (zero-compile instant JS/UI mod hot-reload)
    #   scripts/dev.sh hot-sync core [release|debug]          (WSL + Windows herdr-core + UI defaults)
    #   scripts/dev.sh hot-sync app  [release|debug] [--restart] (Windows herdr-app + UI defaults)
    #   scripts/dev.sh hot-sync all  [release|debug] [--restart] (core + app + UI defaults)
    # Backwards-compatible aliases:
    #   scripts/dev.sh hot-sync [release|debug]               -> core [release|debug]
    TARGET="core"
    MODE="release"
    DO_RESTART="${HOT_SYNC_RESTART:-0}"
    for arg in "$@"; do
      case "$arg" in
        ui|core|app|all) TARGET="$arg" ;;
        release|debug) MODE="$arg" ;;
        --restart|restart) DO_RESTART="1" ;;
        *)
          echo "usage: scripts/dev.sh hot-sync [ui|core|app|all] [release|debug] [--restart]" >&2
          exit 2
          ;;
      esac
    done

    if [[ "$MODE" == "release" ]]; then
      CARGO_PROFILE_FLAG="--release"
      PROFILE_DIR="release"
    else
      CARGO_PROFILE_FLAG=""
      PROFILE_DIR="debug"
    fi

    WIN_LOCAL_RAW=""
    WIN_HOME_RAW=""
    WIN_LOCAL_WSL=""
    WIN_HOME_WSL=""
    WIN_APP_DIR=""
    WIN_PS=""
    if [[ -x /mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe ]]; then
      WIN_PS="/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe"
    elif command -v powershell.exe >/dev/null 2>&1; then
      WIN_PS="$(command -v powershell.exe)"
    fi
    if [[ -x /mnt/c/Windows/System32/cmd.exe ]]; then
      WIN_ENV_OUT="$(timeout 5 /mnt/c/Windows/System32/cmd.exe /c 'echo LOCAL=%LOCALAPPDATA%&echo HOME=%USERPROFILE%' </dev/null 2>/dev/null | tr -d '\r' || true)"
      WIN_LOCAL_RAW="$(printf '%s\n' "$WIN_ENV_OUT" | sed -n 's/^LOCAL=//p' | head -n 1)"
      WIN_HOME_RAW="$(printf '%s\n' "$WIN_ENV_OUT" | sed -n 's/^HOME=//p' | head -n 1)"
      if [[ -n "$WIN_LOCAL_RAW" && -n "$WIN_HOME_RAW" ]]; then
        WIN_LOCAL_WSL="$(wslpath "$WIN_LOCAL_RAW")"
        WIN_HOME_WSL="$(wslpath "$WIN_HOME_RAW")"
        if [[ -d "$WIN_LOCAL_WSL/Programs/herdr2" ]]; then
          WIN_APP_DIR="$WIN_LOCAL_WSL/Programs/herdr2"
        else
          WIN_APP_DIR="$WIN_LOCAL_WSL/Programs/Herdr2"
        fi
      fi
    fi

    run_cargo_xwin() {
      local dir="$1"
      shift
      local lock_file="${XDG_CACHE_HOME:-$HOME/.cache}/cargo-xwin/.herdr2-xwin.lock"
      mkdir -p "$(dirname "$lock_file")"
      local attempt
      for attempt in 1 2 3; do
        if command -v flock >/dev/null 2>&1; then
          if (cd "$dir" && flock -w 600 "$lock_file" "$@"); then
            return 0
          fi
        else
          if (cd "$dir" && "$@"); then
            return 0
          fi
        fi
        if [[ "$attempt" -lt 3 ]]; then
          echo "[hot-sync] cargo-xwin attempt $attempt failed (possible concurrent clang-cl symlink race); retrying..." >&2
          sleep 2
        fi
      done
      return 1
    }

    win_herdr2_pids() {
      [[ -n "$WIN_PS" ]] || return 0
      timeout 8 "$WIN_PS" -NoProfile -Command 'Get-Process herdr2 -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id' </dev/null 2>/dev/null | tr -d '\r' | grep -E '^[0-9]+$' || true
    }

    verify_app_reload_status() {
      local bin="$1"
      local label="$2"
      local status_out
      if ! status_out="$(timeout 10 "$bin" status </dev/null 2>&1 | tr -d '\r')"; then
        echo "[hot-sync] ERROR: failed to query $label status after reload: $status_out" >&2
        return 1
      fi
      python3 - "$label" "$status_out" <<'PY'
import json, sys
label, raw = sys.argv[1], sys.argv[2]
try:
    data = json.loads(raw)
except Exception as exc:
    print(f"[hot-sync] ERROR: {label} status returned non-JSON after reload: {exc}\n{raw}", file=sys.stderr)
    sys.exit(1)
errors = data.get("errors") or []
ui_bundle = data.get("ui_bundle") or {}
active_override = ui_bundle.get("active_override")
if errors:
    print(f"[hot-sync] ERROR: {label} reported {len(errors)} mod load error(s) after app.reload_mods:", file=sys.stderr)
    for err in errors:
        print(f"  - {err}", file=sys.stderr)
    sys.exit(1)
if active_override is False:
    print(f"[hot-sync] WARNING: {label} reports ui_bundle.active_override=false; synced UI could revert on next restart.", file=sys.stderr)
print(f"[hot-sync] Verified {label} reload: 0 errors, active_override={active_override}, ui_version={ui_bundle.get('version')}")
PY
    }

    sync_ui_defaults() {
      local do_reload="${1:-1}"
      local synced=0
      if [[ -n "$WIN_HOME_WSL" && -d "$WIN_HOME_WSL/.config/herdr2-app" ]]; then
        local WIN_APP_CFG="$WIN_HOME_WSL/.config/herdr2-app"
        python3 "$ROOT/scripts/release.py" sync-ui --target "$WIN_APP_CFG"
        echo "[hot-sync] Synced UI defaults + installed.json + ui-bundle.json -> $WIN_APP_CFG"
        if [[ "$do_reload" == "1" && -n "$WIN_APP_DIR" && -x "$WIN_APP_DIR/herdr2.exe" ]]; then
          local pid_count
          pid_count="$(win_herdr2_pids | wc -l | tr -d ' ')"
          if [[ "${pid_count:-0}" -eq 0 ]]; then
            echo "[hot-sync] Windows herdr2.exe is not running; synced UI defaults will load on next launch."
          else
            local reload_out
            if ! reload_out="$(timeout 15 "$WIN_APP_DIR/herdr2.exe" do app.reload_mods </dev/null 2>&1 | tr -d '\r')"; then
              echo "[hot-sync] ERROR: Windows app.reload_mods failed: ${reload_out:-<no output>}" >&2
              return 1
            fi
            sleep 0.3
            verify_app_reload_status "$WIN_APP_DIR/herdr2.exe" "Windows Herdr2"
            if [[ "${pid_count:-0}" -gt 1 ]]; then
              echo "[hot-sync] WARNING: $pid_count Windows herdr2.exe instances are running; app.reload_mods only reaches the socket-owning window. Run 'scripts/dev.sh hot-sync app --restart' to consolidate." >&2
            fi
          fi
        fi
        synced=1
      fi
      if [[ -d "$HOME/.config/herdr2-app" ]]; then
        python3 "$ROOT/scripts/release.py" sync-ui --target "$HOME/.config/herdr2-app" >/dev/null
        echo "[hot-sync] Synced UI defaults + installed.json + ui-bundle.json -> $HOME/.config/herdr2-app"
        if [[ "$do_reload" == "1" && -S "$HOME/.config/herdr2-app/herdr2-app.sock" ]]; then
          local LINUX_APP_BIN
          LINUX_APP_BIN="$(find_app_bin)"
          if [[ -n "$LINUX_APP_BIN" ]]; then
            local linux_reload_out
            if ! linux_reload_out="$(timeout 15 "$LINUX_APP_BIN" do app.reload_mods </dev/null 2>&1)"; then
              echo "[hot-sync] ERROR: Linux app.reload_mods failed: ${linux_reload_out:-<no output>}" >&2
              return 1
            fi
            sleep 0.3
            verify_app_reload_status "$LINUX_APP_BIN" "Linux Herdr2"
          fi
        fi
        synced=1
      fi
      if [[ "$synced" == "0" ]]; then
        echo "[hot-sync] No ~/.config/herdr2-app directory found to sync UI defaults."
      fi
    }

    sync_core() {
      echo "[hot-sync] [core 1/4] Building Linux ($PROFILE_DIR) herdr2-core..."
      (cd "$ROOT/herdr-core" && H2_REQUIRE_WEB_BUNDLE=1 cargo build $CARGO_PROFILE_FLAG --locked --bin herdr2)
      local LINUX_BUILT="$ROOT/herdr-core/target/$PROFILE_DIR/herdr2"

      echo "[hot-sync] [core 2/4] Hot-updating WSL core..."
      local BIN_DIR="${H2_INSTALL_DIR:-$HOME/.local/bin}"
      mkdir -p "$BIN_DIR"
      local WSL_TARGET="$BIN_DIR/herdr2-core"
      if [[ -f "$BIN_DIR/herdr2" && ! -L "$BIN_DIR/herdr2" ]] && "$BIN_DIR/herdr2" --version 2>/dev/null | grep -q '^h2 '; then
        WSL_TARGET="$BIN_DIR/herdr2"
        install -m 755 "$LINUX_BUILT" "$WSL_TARGET.new"
        mv -f "$WSL_TARGET.new" "$WSL_TARGET"
        ln -sfn herdr2 "$BIN_DIR/h2"
        ln -sfn herdr2 "$BIN_DIR/herdr2-core"
      else
        install -m 755 "$LINUX_BUILT" "$WSL_TARGET.new"
        mv -f "$WSL_TARGET.new" "$WSL_TARGET"
        ln -sfn herdr2-core "$BIN_DIR/h2"
      fi
      sh "$ROOT/herdr-core/scripts/restart-core.sh" "$WSL_TARGET"
      echo "[hot-sync] WSL core: $("$BIN_DIR/h2" --version)"
      "$BIN_DIR/h2" server status --json || true

      if [[ -n "$WIN_LOCAL_WSL" ]]; then
        echo "[hot-sync] [core 3/4] Cross-building Windows ($PROFILE_DIR) herdr2-core..."
        export ZIG="$ROOT/scripts/zig-xwin.sh"
        run_cargo_xwin "$ROOT/herdr-core" env H2_REQUIRE_WEB_BUNDLE=1 cargo xwin build $CARGO_PROFILE_FLAG --locked --target x86_64-pc-windows-msvc --bin herdr2
        local WIN_BUILT="$ROOT/herdr-core/target/x86_64-pc-windows-msvc/$PROFILE_DIR/herdr2.exe"
        local WIN_H2_DIR="$WIN_LOCAL_WSL/Programs/h2"
        local WIN_H2_BIN="$WIN_H2_DIR/h2.exe"

        echo "[hot-sync] [core 4/4] Hot-updating Windows core ($WIN_H2_DIR)..."
        mkdir -p "$WIN_H2_DIR"
        cp "$ROOT/herdr-core/crates/herdr2/THIRD-PARTY-NOTICES" "$WIN_H2_DIR/THIRD-PARTY-NOTICES"
        local STAGE_NAME="herdr2.staged.$$.$(date +%s).exe"
        local STAGED_WIN_WSL="$WIN_H2_DIR/$STAGE_NAME"
        local STAGED_WIN_WIN="${WIN_LOCAL_RAW}\\Programs\\h2\\$STAGE_NAME"
        cp "$WIN_BUILT" "$STAGED_WIN_WSL"

        if [[ -x "$WIN_H2_BIN" ]] && timeout 30 "$WIN_H2_BIN" update --from "$STAGED_WIN_WIN" --force </dev/null; then
          echo "[hot-sync] Windows core updated via native h2 update --from"
        else
          echo "[hot-sync] Falling back to atomic rename swap + server restart..."
          local TS="$(date +%Y%m%d-%H%M%S)"
          for alias_exe in h2.exe herdr2-core.exe drove.exe; do
            if [[ -f "$WIN_H2_DIR/$alias_exe" ]]; then
              mv -f "$WIN_H2_DIR/$alias_exe" "$WIN_H2_DIR/$alias_exe.old-$TS" 2>/dev/null || true
            fi
            cp "$WIN_BUILT" "$WIN_H2_DIR/$alias_exe"
          done
          timeout 20 "$WIN_H2_BIN" server restart </dev/null
        fi
        rm -f "$STAGED_WIN_WSL" "$WIN_H2_DIR"/herdr2.staged*.exe 2>/dev/null || true

        # Verify all sibling core aliases stay byte-identical
        local NEW_SZ="$(stat -c%s "$WIN_BUILT")"
        for alias_exe in h2.exe herdr2-core.exe drove.exe; do
          if [[ -f "$WIN_H2_DIR/$alias_exe" ]]; then
            local CUR_SZ="$(stat -c%s "$WIN_H2_DIR/$alias_exe" 2>/dev/null || echo 0)"
            if [[ "$CUR_SZ" != "$NEW_SZ" ]]; then
              mv -f "$WIN_H2_DIR/$alias_exe" "$WIN_H2_DIR/$alias_exe.old-sync" 2>/dev/null || true
              cp "$WIN_BUILT" "$WIN_H2_DIR/$alias_exe"
            fi
          fi
        done
        for old_bak in "$WIN_H2_DIR"/*.old-*; do
          [[ -e "$old_bak" ]] && rm -f "$old_bak" 2>/dev/null || true
        done

        echo "[hot-sync] Windows core: $(timeout 10 "$WIN_H2_BIN" --version </dev/null | tr -d '\r')"
        timeout 10 "$WIN_H2_BIN" server status --json </dev/null | tr -d '\r' || true
      fi
    }

    sync_app() {
      if [[ -n "$WIN_APP_DIR" ]]; then
        echo "[hot-sync] [app 1/2] Cross-building Windows ($PROFILE_DIR) herdr2-app..."
        export ZIG="$ROOT/scripts/zig-xwin.sh"
        run_cargo_xwin "$ROOT/herdr-app" cargo xwin build $CARGO_PROFILE_FLAG --locked --target x86_64-pc-windows-msvc
        local WIN_APP_BUILT="$ROOT/herdr-app/target/x86_64-pc-windows-msvc/$PROFILE_DIR/herdr2.exe"
        mkdir -p "$WIN_APP_DIR"
        echo "[hot-sync] [app 2/2] Atomically swapping Windows app binaries ($WIN_APP_DIR)..."
        local TS="$(date +%Y%m%d-%H%M%S)"
        for bin_exe in herdr2.exe herdr2-app.exe; do
          if [[ -f "$WIN_APP_DIR/$bin_exe" ]]; then
            mv -f "$WIN_APP_DIR/$bin_exe" "$WIN_APP_DIR/$bin_exe.old-$TS" 2>/dev/null || true
          fi
          cp "$WIN_APP_BUILT" "$WIN_APP_DIR/$bin_exe"
        done
        [[ -f "$ROOT/herdr-app/assets/icons/herdr-app.ico" ]] && cp "$ROOT/herdr-app/assets/icons/herdr-app.ico" "$WIN_APP_DIR/herdr2.ico"
        [[ -f "$ROOT/herdr-app/assets/icons/herdr-ui-icon-clean.svg" ]] && cp "$ROOT/herdr-app/assets/icons/herdr-ui-icon-clean.svg" "$WIN_APP_DIR/herdr2.svg"
        if [[ -x "/mnt/c/Windows/System32/ie4uinit.exe" ]]; then
          timeout 5 /mnt/c/Windows/System32/ie4uinit.exe -show </dev/null >/dev/null 2>&1 || true
        fi
        local BUILT_SHA INSTALLED_SHA
        BUILT_SHA="$(sha256sum "$WIN_APP_BUILT" | awk '{print $1}')"
        INSTALLED_SHA="$(sha256sum "$WIN_APP_DIR/herdr2.exe" | awk '{print $1}')"
        if [[ "$BUILT_SHA" != "$INSTALLED_SHA" ]]; then
          echo "[hot-sync] ERROR: installed herdr2.exe hash mismatch ($INSTALLED_SHA != $BUILT_SHA)" >&2
          return 1
        fi
        ls -lh "$WIN_APP_DIR"/herdr2*.exe
        if [[ "$DO_RESTART" == "1" ]]; then
          local OLD_PIDS
          OLD_PIDS="$(win_herdr2_pids | tr '\n' ' ' | xargs || true)"
          echo "[hot-sync] Triggering clean Herdr2 window restart (old PIDs: ${OLD_PIDS:-none}; terminals stay alive in h2)..."
          if [[ -n "$OLD_PIDS" ]]; then
            timeout 15 "$WIN_APP_DIR/herdr2.exe" do app.restart </dev/null || true
          elif [[ -n "$WIN_PS" ]]; then
            local WIN_APP_EXE_WIN
            WIN_APP_EXE_WIN="$(wslpath -w "$WIN_APP_DIR/herdr2.exe")"
            timeout 10 "$WIN_PS" -NoProfile -Command "Start-Process '$WIN_APP_EXE_WIN'" </dev/null || true
          fi
          # Wait up to 20s for a fresh herdr2.exe PID (not in OLD_PIDS) and responsive status
          local NEW_PID=""
          for _ in $(seq 1 40); do
            sleep 0.5
            local CUR_PIDS
            CUR_PIDS="$(win_herdr2_pids || true)"
            for p in $CUR_PIDS; do
              if [[ " $OLD_PIDS " != *" $p "* ]]; then
                NEW_PID="$p"
                break
              fi
            done
            if [[ -n "$NEW_PID" ]] && timeout 3 "$WIN_APP_DIR/herdr2.exe" status </dev/null >/dev/null 2>&1; then
              break
            fi
          done
          # Close any lingering stale duplicate herdr2.exe windows from before restart
          # (terminals live in h2 daemon so closing stale GUI windows never kills terminals).
          if [[ -n "$OLD_PIDS" && -n "$WIN_PS" ]]; then
            local CUR_PIDS
            CUR_PIDS="$(win_herdr2_pids || true)"
            for old_p in $OLD_PIDS; do
              if printf '%s\n' "$CUR_PIDS" | grep -qx "$old_p"; then
                echo "[hot-sync] Closing stale pre-update herdr2.exe window (PID $old_p)..."
                timeout 10 "$WIN_PS" -NoProfile -Command "
                  \$p = Get-Process -Id $old_p -ErrorAction SilentlyContinue
                  if (\$p) {
                    \$null = \$p.CloseMainWindow()
                    if (-not \$p.WaitForExit(3000)) { Stop-Process -Id $old_p -Force -ErrorAction SilentlyContinue }
                  }
                " </dev/null >/dev/null 2>&1 || true
              fi
            done
          fi
          # If somehow no new instance came up after closing stale ones, launch the swapped binary
          if [[ -z "$NEW_PID" && -n "$WIN_PS" ]]; then
            local WIN_APP_EXE_WIN
            WIN_APP_EXE_WIN="$(wslpath -w "$WIN_APP_DIR/herdr2.exe")"
            echo "[hot-sync] Launching fresh Herdr2 window ($WIN_APP_EXE_WIN)..."
            timeout 10 "$WIN_PS" -NoProfile -Command "Start-Process '$WIN_APP_EXE_WIN'" </dev/null || true
            sleep 2
          fi
          # Clean up .old-* backups now that old herdr2.exe processes have exited
          for old_bak in "$WIN_APP_DIR"/*.old-*; do
            [[ -e "$old_bak" ]] && rm -f "$old_bak" 2>/dev/null || true
          done
          local FINAL_PIDS
          FINAL_PIDS="$(win_herdr2_pids | tr '\n' ' ' | xargs || true)"
          echo "[hot-sync] Windows app active PID(s): ${FINAL_PIDS:-none} (sha256=${INSTALLED_SHA:0:12})"
          timeout 8 "$WIN_APP_DIR/herdr2.exe" status </dev/null | tr -d '\r' || true
        else
          for old_bak in "$WIN_APP_DIR"/*.old-*; do
            [[ -e "$old_bak" ]] && rm -f "$old_bak" 2>/dev/null || true
          done
          echo "[hot-sync] Tip: pass --restart (or run 'herdr2.exe do app.restart') to reload the desktop window without losing terminals."
        fi
      else
        echo "[hot-sync] [app] Building Linux ($PROFILE_DIR) herdr2-app..."
        (cd "$ROOT/herdr-app" && cargo build $CARGO_PROFILE_FLAG --locked)
        local LINUX_APP_BUILT="$ROOT/herdr-app/target/$PROFILE_DIR/herdr2"
        mkdir -p "$HOME/.local/bin"
        install -m 755 "$LINUX_APP_BUILT" "$HOME/.local/bin/herdr2-app.new"
        mv -f "$HOME/.local/bin/herdr2-app.new" "$HOME/.local/bin/herdr2-app"
        if [[ "$DO_RESTART" == "1" ]]; then
          "$HOME/.local/bin/herdr2-app" do app.restart || true
        fi
      fi
    }

    case "$TARGET" in
      ui)
        sync_ui_defaults 1
        ;;
      core)
        sync_core
        sync_ui_defaults 1
        ;;
      app)
        if [[ "$DO_RESTART" == "1" ]]; then
          sync_ui_defaults 0
          sync_app
        else
          sync_app
          sync_ui_defaults 1
        fi
        ;;
      all)
        sync_core
        if [[ "$DO_RESTART" == "1" ]]; then
          sync_ui_defaults 0
          sync_app
        else
          sync_app
          sync_ui_defaults 1
        fi
        ;;
    esac
    ;;

  app)
    echo "[dev-app] Fast incremental debug build (deps are opt-level=2)..."
    (cd "$ROOT/herdr-app" && cargo build --locked)
    BIN="$ROOT/herdr-app/target/debug/herdr2"
    mkdir -p "$HOME/.local/bin"
    ln -sfn "$BIN" "$HOME/.local/bin/herdr2"
    if "$BIN" status >/dev/null 2>&1; then
      echo "[dev-app] Restarting open Herdr2 window cleanly (terminals remain alive in h2)..."
      "$BIN" do app.restart || true
    else
      echo "[dev-app] Launching Herdr2 with live defaults hot-reload..."
      exec "$BIN" "$@"
    fi
    ;;

  win)
    TARGET="${1:-all}"
    export ZIG="$ROOT/scripts/zig-xwin.sh"
    case "$TARGET" in
      core)
        (cd "$ROOT/herdr-core" && H2_SKIP_WEB_BUILD=1 cargo xwin build --locked --target x86_64-pc-windows-msvc --bin herdr2)
        echo "[dev-win] Built: $ROOT/herdr-core/target/x86_64-pc-windows-msvc/debug/herdr2.exe"
        ;;
      app)
        (cd "$ROOT/herdr-app" && cargo xwin build --locked --target x86_64-pc-windows-msvc)
        echo "[dev-win] Built: $ROOT/herdr-app/target/x86_64-pc-windows-msvc/debug/herdr2.exe"
        ;;
      all)
        (cd "$ROOT/herdr-core" && H2_SKIP_WEB_BUILD=1 cargo xwin build --locked --target x86_64-pc-windows-msvc --bin herdr2)
        (cd "$ROOT/herdr-app" && cargo xwin build --locked --target x86_64-pc-windows-msvc)
        echo "[dev-win] Built both Windows debug binaries."
        ;;
      *)
        echo "usage: scripts/dev.sh win [app|core|all]" >&2
        exit 2
        ;;
    esac
    ;;

  *)
    cat <<EOF
Herdr2 fast dev hot-reload workflow:
  scripts/dev.sh ui              Zero-compile live reload for defaults/{mods,frame,layouts,themes,icons}
  scripts/dev.sh web             Live watch rebuild for drove-web/dist (no Rust compile)
  scripts/dev.sh core            Fast debug build + zero-downtime h2 server restart
  scripts/dev.sh app             Fast debug build + hot-restart window (terminals stay in h2)
  scripts/dev.sh win [app|core]  Fast incremental Windows cross-build via cargo-xwin
  scripts/dev.sh hot-sync [ui|core|app|all] [release|debug] [--restart]
                                 One-command hot-update for WSL + Windows (UI live reload, core daemon, desktop app)
EOF
    ;;
esac
