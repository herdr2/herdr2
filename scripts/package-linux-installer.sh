#!/usr/bin/env bash
# Builds self-contained Linux installers containing BOTH Herdr2 desktop app and h2 core:
#   1. <out-dir>/herdr2_<version>_<deb-arch>.deb       (Debian/Ubuntu single-package installer)
#   2. <out-dir>/herdr2-<version>-linux-<arch>.tar.gz  (universal Linux bundle with ./install.sh)
#
# Usage:
#   scripts/package-linux-installer.sh <app-bin> <core-bin> <version> <arch> <out-dir>
set -euo pipefail

APP_BIN="${1:?usage: package-linux-installer.sh <app-bin> <core-bin> <version> <arch> <out-dir>}"
CORE_BIN="${2:?usage: package-linux-installer.sh <app-bin> <core-bin> <version> <arch> <out-dir>}"
VERSION="${3:?missing version}"
ARCH="${4:-x86_64}"
OUT_DIR="${5:?missing out-dir}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/var/tmp}/herdr2-linux-pkg.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$OUT_DIR"

# 1. Universal self-contained tar.gz bundle (App + h2 Core + ./install.sh)
BUNDLE="$WORK/herdr2-$VERSION-linux-$ARCH"
mkdir -p "$BUNDLE"
install -m 755 "$APP_BIN" "$BUNDLE/herdr2"
install -m 755 "$CORE_BIN" "$BUNDLE/h2"
cp "$ROOT/herdr-app/assets/icons/herdr-ui-icon-clean.svg" "$BUNDLE/herdr2.svg"
cp "$ROOT/herdr-app/LICENSE" "$BUNDLE/LICENSE"
cp "$ROOT/herdr-app/THIRD-PARTY-NOTICES" "$BUNDLE/THIRD-PARTY-NOTICES"

cat > "$BUNDLE/install.sh" <<'EOF'
#!/bin/sh
# Installs Herdr2 desktop app + h2 core runtime from this bundle into ~/.local (no root required).
set -eu
DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
BIN="${H2_INSTALL_DIR:-$HOME/.local/bin}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_HOME="$DATA/herdr2"

mkdir -p "$BIN" "$DATA/applications" "$DATA/icons/hicolor/scalable/apps"

rm -rf "$APP_HOME.new"
mkdir -p "$APP_HOME.new"
install -m 755 "$DIR/herdr2" "$APP_HOME.new/herdr2"
install -m 755 "$DIR/h2" "$APP_HOME.new/h2"
cp "$DIR/herdr2.svg" "$APP_HOME.new/herdr2.svg"
cp "$DIR/LICENSE" "$APP_HOME.new/LICENSE"
cp "$DIR/THIRD-PARTY-NOTICES" "$APP_HOME.new/THIRD-PARTY-NOTICES"
rm -rf "$APP_HOME"
mv "$APP_HOME.new" "$APP_HOME"

# Core goes to herdr2-core + h2 (zero collision with desktop symlink `herdr2`).
install -m 755 "$DIR/h2" "$BIN/herdr2-core.new"
mv -f "$BIN/herdr2-core.new" "$BIN/herdr2-core"
ln -sfn herdr2-core "$BIN/h2"
ln -sfn "$APP_HOME/herdr2" "$BIN/herdr2"

cp "$APP_HOME/herdr2.svg" "$DATA/icons/hicolor/scalable/apps/herdr2.svg"
cat > "$DATA/applications/herdr2.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Herdr2
Comment=Terminals and headless ACP coding agents that keep running
Exec=$BIN/herdr2
Icon=herdr2
Terminal=false
Categories=Development;
StartupWMClass=herdr2
DESKTOP

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$DATA/applications" 2>/dev/null || true
[ -f "$DATA/icons/hicolor/icon-theme.cache" ] && gtk-update-icon-cache -f -t "$DATA/icons/hicolor" >/dev/null 2>&1 || true

echo "Herdr2 + h2 installed cleanly!"
echo "  App:  $BIN/herdr2"
echo "  Core: $BIN/h2"
echo "  Run:  herdr2"
echo "  Skills: h2 skill install --all"
case ":$PATH:" in
  *":$BIN:"*) ;;
  *) echo "Note: add $BIN to your PATH." ;;
esac
EOF
chmod 755 "$BUNDLE/install.sh"

TAR_OUT="$OUT_DIR/herdr2-$VERSION-linux-$ARCH.tar.gz"
tar -czf "$TAR_OUT" -C "$WORK" "herdr2-$VERSION-linux-$ARCH"
echo "package-linux-installer: created $TAR_OUT"

# 2. Debian/Ubuntu .deb package (when dpkg-deb is available)
if command -v dpkg-deb >/dev/null 2>&1; then
  case "$ARCH" in
    x86_64|amd64) DEB_ARCH="amd64" ;;
    aarch64|arm64) DEB_ARCH="arm64" ;;
    *) DEB_ARCH="$ARCH" ;;
  esac
  DEB_ROOT="$WORK/deb"
  mkdir -p \
    "$DEB_ROOT/DEBIAN" \
    "$DEB_ROOT/opt/herdr2" \
    "$DEB_ROOT/usr/bin" \
    "$DEB_ROOT/usr/share/applications" \
    "$DEB_ROOT/usr/share/icons/hicolor/scalable/apps"

  install -m 755 "$APP_BIN" "$DEB_ROOT/opt/herdr2/herdr2"
  install -m 755 "$CORE_BIN" "$DEB_ROOT/opt/herdr2/herdr2-core"
  cp "$ROOT/herdr-app/assets/icons/herdr-ui-icon-clean.svg" "$DEB_ROOT/opt/herdr2/herdr2.svg"
  cp "$ROOT/herdr-app/LICENSE" "$DEB_ROOT/opt/herdr2/LICENSE"
  cp "$ROOT/herdr-app/THIRD-PARTY-NOTICES" "$DEB_ROOT/opt/herdr2/THIRD-PARTY-NOTICES"
  ln -s /opt/herdr2/herdr2 "$DEB_ROOT/usr/bin/herdr2"
  ln -s /opt/herdr2/herdr2-core "$DEB_ROOT/usr/bin/h2"
  ln -s /opt/herdr2/herdr2-core "$DEB_ROOT/usr/bin/herdr2-core"
  cp "$ROOT/herdr-app/assets/icons/herdr-ui-icon-clean.svg" "$DEB_ROOT/usr/share/icons/hicolor/scalable/apps/herdr2.svg"

  cat > "$DEB_ROOT/usr/share/applications/herdr2.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=Herdr2
Comment=Terminals and headless ACP coding agents that keep running
Exec=/usr/bin/herdr2
Icon=herdr2
Terminal=false
Categories=Development;
StartupWMClass=herdr2
DESKTOP

  cat > "$DEB_ROOT/DEBIAN/control" <<CONTROL
Package: herdr2
Version: $VERSION
Section: devel
Priority: optional
Architecture: $DEB_ARCH
Maintainer: Herdr2 <dev@herdr2.dev>
Depends: libc6, libvulkan1
Description: Herdr2 desktop workspace and h2 headless ACP multi-agent runtime
 Hybrid Ghostty terminal workspace and headless ACP coding agent orchestrator.
 Includes both the Herdr2 desktop app and the h2 core CLI/daemon.
CONTROL

  DEB_OUT="$OUT_DIR/herdr2_${VERSION}_${DEB_ARCH}.deb"
  dpkg-deb --build --root-owner-group "$DEB_ROOT" "$DEB_OUT"
  echo "package-linux-installer: created $DEB_OUT"
fi
