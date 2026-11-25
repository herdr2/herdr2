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

# Prepare resized PNG icons (256x256 and 512x512) from the 1024x1024 source PNG so
# Ubuntu/GNOME/KDE show the Herdr2 icon immediately without relying solely on SVG theme caches.
SRC_SVG="$ROOT/herdr-app/assets/icons/herdr-ui-icon-clean.svg"
SRC_PNG="$ROOT/herdr-app/assets/icons/herdr-ui-icon-clean.png"
PNG_256="$WORK/herdr2-256.png"
PNG_512="$WORK/herdr2-512.png"
if [[ -f "$SRC_PNG" ]]; then
  if command -v python3 >/dev/null 2>&1 && python3 -c '
import sys
src, dst256, dst512 = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    import gi
    gi.require_version("GdkPixbuf", "2.0")
    from gi.repository import GdkPixbuf
    for size, dst in [(256, dst256), (512, dst512)]:
        pb = GdkPixbuf.Pixbuf.new_from_file_at_scale(src, size, size, True)
        pb.savev(dst, "png", [], [])
except Exception:
    from PIL import Image
    im = Image.open(src)
    for size, dst in [(256, dst256), (512, dst512)]:
        im.resize((size, size), Image.LANCZOS).save(dst, "PNG")
' "$SRC_PNG" "$PNG_256" "$PNG_512" 2>/dev/null; then
    :
  else
    cp "$SRC_PNG" "$PNG_256"
    cp "$SRC_PNG" "$PNG_512"
  fi
fi

# 1. Universal self-contained tar.gz bundle (App + h2 Core + ./install.sh)
BUNDLE="$WORK/herdr2-$VERSION-linux-$ARCH"
mkdir -p "$BUNDLE"
install -m 755 "$APP_BIN" "$BUNDLE/herdr2"
install -m 755 "$CORE_BIN" "$BUNDLE/h2"
cp "$SRC_SVG" "$BUNDLE/herdr2.svg"
[[ -f "$PNG_256" ]] && cp "$PNG_256" "$BUNDLE/herdr2.png"
[[ -f "$PNG_512" ]] && cp "$PNG_512" "$BUNDLE/herdr2-512.png"
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

mkdir -p \
  "$BIN" \
  "$DATA/applications" \
  "$DATA/icons/hicolor/scalable/apps" \
  "$DATA/icons/hicolor/256x256/apps" \
  "$DATA/icons/hicolor/512x512/apps"

rm -rf "$APP_HOME.new"
mkdir -p "$APP_HOME.new"
install -m 755 "$DIR/herdr2" "$APP_HOME.new/herdr2"
install -m 755 "$DIR/h2" "$APP_HOME.new/h2"
cp "$DIR/herdr2.svg" "$APP_HOME.new/herdr2.svg"
[ -f "$DIR/herdr2.png" ] && cp "$DIR/herdr2.png" "$APP_HOME.new/herdr2.png"
[ -f "$DIR/herdr2-512.png" ] && cp "$DIR/herdr2-512.png" "$APP_HOME.new/herdr2-512.png"
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
DESKTOP_ICON="$DATA/icons/hicolor/scalable/apps/herdr2.svg"
if [ -f "$APP_HOME/herdr2.png" ]; then
  cp "$APP_HOME/herdr2.png" "$DATA/icons/hicolor/256x256/apps/herdr2.png"
  DESKTOP_ICON="$DATA/icons/hicolor/256x256/apps/herdr2.png"
fi
if [ -f "$APP_HOME/herdr2-512.png" ]; then
  cp "$APP_HOME/herdr2-512.png" "$DATA/icons/hicolor/512x512/apps/herdr2.png"
elif [ -f "$APP_HOME/herdr2.png" ]; then
  cp "$APP_HOME/herdr2.png" "$DATA/icons/hicolor/512x512/apps/herdr2.png"
fi

# Use an absolute PNG Icon path so already-running Ubuntu/GNOME/Wayland sessions show the icon immediately.
cat > "$DATA/applications/herdr2.desktop" <<DESKTOP
[Desktop Entry]
Version=1.0
Type=Application
Name=Herdr2
Comment=Terminals and headless ACP coding agents that keep running
Exec=$BIN/herdr2
Icon=$DESKTOP_ICON
Terminal=false
StartupNotify=true
Categories=Development;
StartupWMClass=herdr2
DESKTOP

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$DATA/applications" 2>/dev/null || true
touch "$DATA/icons/hicolor" "$DATA/applications/herdr2.desktop" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -f -t "$DATA/icons/hicolor" >/dev/null 2>&1 || true
command -v dbus-send >/dev/null 2>&1 &&
  dbus-send --session --type=signal /KIconLoader org.kde.KIconLoader.iconChanged int32:0 2>/dev/null || true

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
    "$DEB_ROOT/usr/share/pixmaps" \
    "$DEB_ROOT/usr/share/icons/hicolor/scalable/apps" \
    "$DEB_ROOT/usr/share/icons/hicolor/256x256/apps" \
    "$DEB_ROOT/usr/share/icons/hicolor/512x512/apps"

  install -m 755 "$APP_BIN" "$DEB_ROOT/opt/herdr2/herdr2"
  install -m 755 "$CORE_BIN" "$DEB_ROOT/opt/herdr2/herdr2-core"
  cp "$SRC_SVG" "$DEB_ROOT/opt/herdr2/herdr2.svg"
  [[ -f "$PNG_256" ]] && cp "$PNG_256" "$DEB_ROOT/opt/herdr2/herdr2.png"
  cp "$ROOT/herdr-app/LICENSE" "$DEB_ROOT/opt/herdr2/LICENSE"
  cp "$ROOT/herdr-app/THIRD-PARTY-NOTICES" "$DEB_ROOT/opt/herdr2/THIRD-PARTY-NOTICES"
  ln -s /opt/herdr2/herdr2 "$DEB_ROOT/usr/bin/herdr2"
  ln -s /opt/herdr2/herdr2-core "$DEB_ROOT/usr/bin/h2"
  ln -s /opt/herdr2/herdr2-core "$DEB_ROOT/usr/bin/herdr2-core"
  install -m 644 "$SRC_SVG" "$DEB_ROOT/usr/share/icons/hicolor/scalable/apps/herdr2.svg"
  DEB_ICON="/usr/share/icons/hicolor/scalable/apps/herdr2.svg"
  if [[ -f "$PNG_256" ]]; then
    install -m 644 "$PNG_256" "$DEB_ROOT/usr/share/icons/hicolor/256x256/apps/herdr2.png"
    install -m 644 "$PNG_256" "$DEB_ROOT/usr/share/pixmaps/herdr2.png"
    DEB_ICON="/usr/share/icons/hicolor/256x256/apps/herdr2.png"
  fi
  if [[ -f "$PNG_512" ]]; then
    install -m 644 "$PNG_512" "$DEB_ROOT/usr/share/icons/hicolor/512x512/apps/herdr2.png"
  fi

  cat > "$DEB_ROOT/usr/share/applications/herdr2.desktop" <<DESKTOP
[Desktop Entry]
Version=1.0
Type=Application
Name=Herdr2
Comment=Terminals and headless ACP coding agents that keep running
Exec=/usr/bin/herdr2
Icon=$DEB_ICON
Terminal=false
StartupNotify=true
Categories=Development;
StartupWMClass=herdr2
DESKTOP
  chmod 644 "$DEB_ROOT/usr/share/applications/herdr2.desktop"

  cat > "$DEB_ROOT/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q -f -t /usr/share/icons/hicolor || true
fi
POSTINST
  chmod 755 "$DEB_ROOT/DEBIAN/postinst"

  cat > "$DEB_ROOT/DEBIAN/postrm" <<'POSTRM'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q -f -t /usr/share/icons/hicolor || true
fi
POSTRM
  chmod 755 "$DEB_ROOT/DEBIAN/postrm"

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
