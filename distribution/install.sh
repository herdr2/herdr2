#!/bin/sh
# Installs herdr2 (h2), or updates it:
#
#   curl -fsSL https://releases.herdr.dev/core/install.sh | sh
#
# Into ~/.local/bin/herdr2, with `h2` beside it only if no `h2` exists.
# H2_VERSION=0.2.0 picks a version; HERDR2_RELEASES_URL points elsewhere (tests).
set -eu

releases=${HERDR2_RELEASES_URL:-https://releases.herdr.dev}
bin=${H2_INSTALL_DIR:-$HOME/.local/bin}

say() { printf '%s\n' "$*"; }
fail() { say "h2: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "needs $1"; }
need curl
need tar

case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=macos ;;
  *) fail "h2 runs on Linux and macOS for now" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) fail "no build for $(uname -m)" ;;
esac
target="$os-$arch"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp" "$bin/herdr2.staged"' EXIT

manifest="$releases/core/latest.json"
[ -n "${H2_VERSION:-}" ] && manifest="$releases/core/$H2_VERSION/manifest.json"
curl -fsSL "$manifest" -o "$tmp/manifest.json" || fail "cannot reach $manifest"
flat=$(tr -d '\n ' < "$tmp/manifest.json")
entry=$(printf '%s' "$flat" | sed -n "s/.*\"$target\":{\([^}]*\)}.*/\1/p")
url=$(printf '%s' "$entry" | sed -n 's/.*"url":"\([^"]*\)".*/\1/p')
want=$(printf '%s' "$entry" | sed -n 's/.*"sha256":"\([^"]*\)".*/\1/p')
version=$(printf '%s' "$flat" | sed -n 's/^{"version":"\([^"]*\)".*/\1/p')
[ -n "$url" ] && [ -n "$want" ] || fail "no build for $target in $manifest"

src=install
if [ -x "$bin/herdr2" ]; then src=update; fi
case $url in
  *[?#]*) download=$url ;;
  "$releases"/*) download="$url?src=$src" ;;
  *) download=$url ;;
esac
curl -fsSL "$download" -o "$tmp/h2.tar.gz" || fail "download failed: $url"
[ "$(sha256 "$tmp/h2.tar.gz")" = "$want" ] || fail "the download is damaged (checksum mismatch); try again"
tar -xzf "$tmp/h2.tar.gz" -C "$tmp"
core=$tmp/herdr2
if ! "$core" --version >/dev/null 2>&1; then
  mkdir -p "$bin"
  core=$bin/herdr2.staged
  cp "$tmp/herdr2" "$core" && chmod 755 "$core" || fail "cannot stage h2 in $bin"
  "$core" --version >/dev/null 2>&1 || fail "the downloaded h2 does not run on this machine"
fi
if [ "${H2_BUNDLE_ONLY:-0}" = 1 ] && [ ! -x "$tmp/Herdr2.app/Contents/MacOS/herdr2" ]; then
  exit 0
fi
mkdir -p "$bin"

bundle=0
if [ "$os" = macos ] && [ "$(id -u)" != 0 ]; then
  case "$bin" in "$HOME"/*) bundle=1 ;; esac
fi
if [ "$bundle" = 1 ] && [ -x "$tmp/Herdr2.app/Contents/MacOS/herdr2" ]; then
  action=installed
  [ ! -x "$bin/herdr2" ] || action=updated
  bundled="$tmp/Herdr2.app/Contents/MacOS/herdr2"
  if [ "$action" = updated ]; then
    installed=$("$bin/herdr2" --version)
    candidate=$("$bundled" --version)
    awk -v old="$installed" -v new="$candidate" 'BEGIN {
      sub(/^h2 /, "", old); sub(/^h2 /, "", new)
      split(old, a, /[.-]/); split(new, b, /[.-]/)
      for (i=1; i<=3; i++) { if (a[i]+0 > b[i]+0) exit 1; if (a[i]+0 < b[i]+0) exit 0 }
    }' || fail "$candidate is older than $installed; use h2 update --from with --force to downgrade"
  fi
  case "$releases" in
    https://releases.herdr.dev | https://releases.herdr.dev/)
      /usr/bin/codesign --verify --strict -R '=identifier "dev.herdr2.core" and anchor apple generic and certificate leaf[subject.OU] = "2W9652Q7QR"' "$tmp/Herdr2.app" || fail "not a signed Herdr2 release" ;;
  esac
  "$bundled" --h2-install-bundle "$bundled" "$bin/herdr2" || fail "installing Herdr2.app failed"
  if [ "$action" = updated ]; then
    "$bin/herdr2" server restart >/dev/null || fail "restarting the core failed"
  fi
  say "h2 $version $action as Herdr2.app."
elif [ -x "$bin/herdr2" ]; then
  "$bin/herdr2" update --from "$core" >/dev/null || fail "updating $bin/herdr2 failed (see above)"
  say "h2 updated to $version."
else
  cp "$core" "$bin/herdr2.new"
  chmod 755 "$bin/herdr2.new"
  mv -f "$bin/herdr2.new" "$bin/herdr2"
  say "h2 $version installed into $bin/herdr2."
fi
if ! command -v h2 >/dev/null 2>&1 && [ ! -e "$bin/h2" ]; then
  ln -s herdr2 "$bin/h2"
fi

case ":$PATH:" in
  *":$bin:"*) ;;
  *) say "Add $bin to your PATH to use h2." ;;
esac
say "Agents: h2 --skill. People: h2 --help."
