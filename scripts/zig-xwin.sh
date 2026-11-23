#!/usr/bin/env bash
# Zig wrapper that injects MSVC/UCRT --libc from cargo-xwin's cache when
# cross-compiling libghostty-vt for x86_64-windows-msvc on Linux.
set -euo pipefail

REAL_ZIG="${REAL_ZIG:-$(command -v zig)}"
XWIN_DIR="${XWIN_CACHE_DIR:-$HOME/.cache/cargo-xwin/xwin}"

if [[ "${1:-}" == "build" && "$*" == *"windows-msvc"* && -d "$XWIN_DIR/crt" ]]; then
  LIBC_FILE="$XWIN_DIR/zig-libc-x86_64.txt"
  cat > "$LIBC_FILE" <<EOF
include_dir=$XWIN_DIR/sdk/include/ucrt
sys_include_dir=$XWIN_DIR/crt/include
crt_dir=$XWIN_DIR/sdk/lib/ucrt/x86_64
msvc_lib_dir=$XWIN_DIR/crt/lib/x86_64
kernel32_lib_dir=$XWIN_DIR/sdk/lib/um/x86_64
gcc_dir=
EOF
  shift
  exec "$REAL_ZIG" build --libc "$LIBC_FILE" "$@"
fi

exec "$REAL_ZIG" "$@"
