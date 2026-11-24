#!/usr/bin/env bash
# Builds a self-contained macOS .dmg installer containing Herdr2.app with bundled `h2` core
# inside `Herdr2.app/Contents/Helpers/h2` (so dragging Herdr2.app to Applications works out of the box).
#
# Usage:
#   scripts/package-macos-dmg.sh <Herdr2.app> <h2-core-binary> <output.dmg>
set -euo pipefail

APP_SRC="${1:?usage: package-macos-dmg.sh <Herdr2.app> <h2-core-binary> <output.dmg>}"
CORE_BIN="${2:?usage: package-macos-dmg.sh <Herdr2.app> <h2-core-binary> <output.dmg>}"
OUT_DMG="${3:?usage: package-macos-dmg.sh <Herdr2.app> <h2-core-binary> <output.dmg>}"

[[ "$(uname -s)" == "Darwin" ]] || { echo "package-macos-dmg: requires macOS" >&2; exit 1; }
[[ -d "$APP_SRC/Contents/MacOS" ]] || { echo "package-macos-dmg: invalid app bundle $APP_SRC" >&2; exit 1; }
[[ -x "$CORE_BIN" ]] || { echo "package-macos-dmg: missing executable core binary $CORE_BIN" >&2; exit 1; }

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/herdr2-dmg.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

STAGE="$WORK/dmg-root"
mkdir -p "$STAGE"
cp -R "$APP_SRC" "$STAGE/Herdr2.app"

# Embed `h2` core directly inside Herdr2.app/Contents/Helpers/h2 so Herdr2 finds it immediately.
mkdir -p "$STAGE/Herdr2.app/Contents/Helpers"
cp "$CORE_BIN" "$STAGE/Herdr2.app/Contents/Helpers/h2"
chmod 755 "$STAGE/Herdr2.app/Contents/Helpers/h2"

# Sign the embedded helper and re-sign the app bundle.
if [[ -n "${APPLE_CERTIFICATE_P12:-}" && -n "${APPLE_CERTIFICATE_PASSWORD:-}" ]]; then
  "$ROOT/herdr-app/scripts/mac-sign" "$STAGE/Herdr2.app/Contents/Helpers/h2"
  "$ROOT/herdr-app/scripts/mac-sign" "$STAGE/Herdr2.app"
else
  /usr/bin/codesign --force --sign - "$STAGE/Herdr2.app/Contents/Helpers/h2"
  /usr/bin/codesign --force --deep --sign - "$STAGE/Herdr2.app"
fi

ln -s /Applications "$STAGE/Applications"

mkdir -p "$(dirname "$OUT_DMG")"
rm -f "$OUT_DMG"
hdiutil create \
  -volname "Herdr2" \
  -srcfolder "$STAGE" \
  -ov \
  -format UDZO \
  "$OUT_DMG"

if [[ -n "${APPLE_CERTIFICATE_P12:-}" && -n "${APPLE_CERTIFICATE_PASSWORD:-}" ]]; then
  "$ROOT/herdr-app/scripts/mac-sign" "$OUT_DMG"
fi

echo "package-macos-dmg: created $OUT_DMG"
