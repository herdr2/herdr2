#!/bin/sh
# Builds a release binary into ~/.local/bin (replacing any older one), then
# restarts the core cleanly (without handing the build shell's env to the core).
set -eu
cd "$(dirname "$0")/.."
nice -n 10 cargo build --release
bin="${H2_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$bin"
if [ "$(uname -s)" = Darwin ]; then
  app="target/release/Herdr2.app"
  rm -rf "$app"
  sh scripts/mac-bundle target/release/herdr2 "$app"
  bash scripts/mac-sign "$app"
  "$app/Contents/MacOS/herdr2" --h2-install-bundle "$app/Contents/MacOS/herdr2" "$bin/herdr2"
else
  install -m 755 target/release/herdr2 "$bin/herdr2.new"
  mv -f "$bin/herdr2.new" "$bin/herdr2"
fi
[ -e "$bin/h2" ] || [ -L "$bin/h2" ] || ln -s herdr2 "$bin/h2"
sh scripts/restart-core.sh "$bin/herdr2"
