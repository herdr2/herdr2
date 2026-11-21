#!/bin/sh
# Builds a release binary (with the embedded Drove Web bundle) into ~/.local/bin,
# then restarts the core cleanly (without handing the build shell's env to the core).
set -eu
cd "$(dirname "$0")/.."
if [ -f "../drove-web/package.json" ] && [ "${H2_SKIP_WEB_BUILD:-0}" != 1 ]; then
  export H2_REQUIRE_WEB_BUNDLE=1
fi
nice -n 10 cargo build --release
bin="${H2_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$bin"
if [ "$(uname -s)" = Darwin ]; then
  app="target/release/Herdr2.app"
  rm -rf "$app"
  sh scripts/mac-bundle target/release/herdr2 "$app"
  bash scripts/mac-sign "$app"
  "$app/Contents/MacOS/herdr2" --h2-install-bundle "$app/Contents/MacOS/herdr2" "$bin/herdr2"
  [ -e "$bin/h2" ] || [ -L "$bin/h2" ] || ln -s herdr2 "$bin/h2"
  sh scripts/restart-core.sh "$bin/herdr2"
else
  # Install core as `herdr2-core` + `h2` so `~/.local/bin/herdr2` stays reserved for the desktop app.
  install -m 755 target/release/herdr2 "$bin/herdr2-core.new"
  mv -f "$bin/herdr2-core.new" "$bin/herdr2-core"
  ln -sfn herdr2-core "$bin/h2"
  sh scripts/restart-core.sh "$bin/herdr2-core"
fi
