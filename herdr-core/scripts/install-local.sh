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
  # Respect existing layout: if ~/.local/bin/herdr2 is already a core binary (not the GUI app),
  # replace it in-place and point both `h2` and `herdr2-core` to it so running `herdr2 serve` daemons
  # are never left orphaned on an older copy.
  target="$bin/herdr2-core"
  if [ -f "$bin/herdr2" ] && [ ! -L "$bin/herdr2" ] && "$bin/herdr2" --version 2>/dev/null | grep -q '^h2 '; then
    target="$bin/herdr2"
    install -m 755 target/release/herdr2 "$target.new"
    mv -f "$target.new" "$target"
    ln -sfn herdr2 "$bin/h2"
    ln -sfn herdr2 "$bin/herdr2-core"
  else
    install -m 755 target/release/herdr2 "$target.new"
    mv -f "$target.new" "$target"
    ln -sfn herdr2-core "$bin/h2"
  fi
  sh scripts/restart-core.sh "$target"
fi
