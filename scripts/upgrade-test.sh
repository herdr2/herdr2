#!/bin/sh
# Builds a previous release (a git ref) and checks updating from it to this tree.
set -eu
ref=${1:?usage: scripts/upgrade-test.sh <git ref of the previous release>}
root=$(git rev-parse --show-toplevel)
sha=$(git -C "$root" rev-parse --short "$ref")
bin=/var/tmp/h2-upgrade/$sha/herdr2
if [ ! -x "$bin" ]; then
    src=/var/tmp/h2-upgrade/$sha-src
    rm -rf "$src"
    git -C "$root" worktree add --detach "$src" "$ref"
    (cd "$src" && CARGO_TARGET_DIR=/var/tmp/h2-upgrade/target cargo build -q --bin herdr2)
    mkdir -p "$(dirname "$bin")"
    cp /var/tmp/h2-upgrade/target/debug/herdr2 "$bin"
    git -C "$root" worktree remove --force "$src"
fi
cd "$root"
H2_OLD_BIN="$bin" cargo test -q --test acp_lifecycle -- --nocapture
