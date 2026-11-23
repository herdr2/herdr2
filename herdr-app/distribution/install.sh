#!/bin/sh
# Installs the Herdr2 app, and herdr2-core (`h2`) if there's none yet:
#
#   HERDR2_RELEASES_URL=https://your-host curl -fsSL "$HERDR2_RELEASES_URL/app/install.sh" | sh
#
# Linux: the app into ~/.local/share/herdr2, `herdr2` in ~/.local/bin,
# a launcher entry and icon. macOS: Herdr2.app into ~/Applications.
set -eu

releases=${HERDR2_RELEASES_URL:-${HERDR_RELEASES_URL:-}}
[ -n "$releases" ] || { printf 'herdr2: set HERDR2_RELEASES_URL to your release server base URL\n' >&2; exit 1; }
releases=${releases%/}
bin="$HOME/.local/bin"

say() { printf '%s\n' "$*"; }
fail() { say "herdr2: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "needs $1"; }
need curl
need tar

case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=macos ;;
  MINGW* | MSYS* | CYGWIN*) fail "this installer is for Linux and macOS; on Windows, run this in PowerShell: \$env:HERDR2_RELEASES_URL='$releases'; irm $releases/app/install.ps1 | iex" ;;
  *) fail "no build for $(uname -s)" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) fail "no build for $(uname -m)" ;;
esac
if [ "$os" = macos ] && [ "$arch" = x86_64 ]; then
  if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ] ||
     [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
    arch=aarch64
  else
    fail "Intel Macs aren't supported; build from source instead."
  fi
fi
target="$os-$arch"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "$releases/app/latest.json" -o "$tmp/latest.json" || fail "cannot reach $releases"
entry=$(tr -d '\n ' < "$tmp/latest.json" | sed -n "s/.*\"$target\":{\([^}]*\)}.*/\1/p")
url=$(printf '%s' "$entry" | sed -n 's/.*"url":"\([^"]*\)".*/\1/p')
want=$(printf '%s' "$entry" | sed -n 's/.*"sha256":"\([^"]*\)".*/\1/p')
version=$(tr -d '\n ' < "$tmp/latest.json" | sed -n 's/^{"version":"\([^"]*\)".*/\1/p')
[ -n "$url" ] && [ -n "$want" ] || fail "no build for $target yet"

say "Installing Herdr2 ${version:-} for $target..."
curl -fsSL "$url" -o "$tmp/app.tar.gz" || fail "download failed"
[ "$(sha256 "$tmp/app.tar.gz")" = "$want" ] || fail "download is damaged (checksum mismatch); try again"
tar -xzf "$tmp/app.tar.gz" -C "$tmp"
mkdir -p "$bin"

if [ "$os" = linux ]; then
  data=${XDG_DATA_HOME:-$HOME/.local/share}
  home="$data/herdr2"
  mkdir -p "$data"
  rm -rf "$home.new"
  mv "$tmp/herdr2" "$home.new"
  rm -rf "$home"
  mv "$home.new" "$home"
  ln -sfn "$home/herdr2" "$bin/herdr2"
  mkdir -p "$data/applications" "$data/icons/hicolor/scalable/apps"
  cp "$home/herdr2.svg" "$data/icons/hicolor/scalable/apps/herdr2.svg"
  cat > "$data/applications/herdr2.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Herdr2
Comment=Terminals and coding agents that keep running
Exec=$bin/herdr2
Icon=herdr2
Terminal=false
Categories=Development;
StartupWMClass=herdr2
EOF
  command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$data/applications" 2>/dev/null || true
  [ -f "$data/icons/hicolor/icon-theme.cache" ] && gtk-update-icon-cache -f -t "$data/icons/hicolor" >/dev/null 2>&1 || true
  command -v dbus-send >/dev/null 2>&1 &&
    dbus-send --session --type=signal /KIconLoader org.kde.KIconLoader.iconChanged int32:0 2>/dev/null || true
else
  apps="$HOME/Applications"
  app="$apps/Herdr2.app"
  mkdir -p "$apps"
  signed=false
  if signature=$(/usr/bin/codesign -dvv "$tmp/Herdr2.app" 2>&1); then
    if printf '%s\n' "$signature" | grep -q '^Authority=Developer ID Application:'; then
      signed=true
    elif ! printf '%s\n' "$signature" | grep -qx 'Signature=adhoc'; then
      fail "cannot determine app signature: $signature"
    fi
  elif ! printf '%s\n' "$signature" | grep -q ': code object is not signed at all$'; then
    fail "cannot determine app signature: $signature"
  fi
  if [ "$signed" = true ]; then
    /usr/bin/codesign --verify --deep --strict "$tmp/Herdr2.app" || fail "invalid app signature"
  else
    /usr/libexec/PlistBuddy -c "Add :LSEnvironment:PATH string $bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin" "$tmp/Herdr2.app/Contents/Info.plist"
    /usr/bin/codesign --force --sign - "$tmp/Herdr2.app" >/dev/null 2>&1
  fi
  rm -rf "$app"
  mv "$tmp/Herdr2.app" "$app"
  ln -sfn "$app/Contents/MacOS/Herdr2" "$bin/herdr2"
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$app" 2>/dev/null || true
fi

if [ ! -e "$bin/h2" ] && ! command -v h2 >/dev/null 2>&1; then
  curl -fsSL "$releases/core/install.sh" | H2_INSTALL_DIR="$bin" HERDR2_RELEASES_URL="$releases" sh || say "herdr2-core didn't install; Herdr2 will offer it when it opens."
fi

say "Herdr2 is installed. Open it from your apps, or run: herdr2"
case ":$PATH:" in
  *":$bin:"*) ;;
  *) say "Add $bin to your PATH to use herdr2 and h2 from a terminal." ;;
esac
