set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

default:
    @just --list

# Zero-compile live hot-reload for JS mods, frame mods, layouts, themes, icons.
[unix]
dev-ui:
    bash scripts/dev.sh ui

[windows]
dev-ui:
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 ui

# Live watch build for drove-web (served directly from disk by herdr-core).
[unix]
dev-web:
    bash scripts/dev.sh web

[windows]
dev-web:
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 web

# Fast incremental debug build of herdr-core + zero-downtime h2 server restart.
[unix]
dev-core:
    bash scripts/dev.sh core

[windows]
dev-core:
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 core

# Fast incremental debug build of herdr-app + hot-restart window.
[unix]
dev-app:
    bash scripts/dev.sh app

[windows]
dev-app:
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 app

# Fast incremental Windows cross-build on Linux via cargo-xwin (app | core | all).
[unix]
dev-win target="all":
    bash scripts/dev.sh win {{target}}

# One-command hot-update for WSL + Windows:
#   just hot-sync ui                        (zero-compile instant JS/UI mod hot-reload)
#   just hot-sync core [release|debug]      (WSL + Windows herdr-core + UI defaults)
#   just hot-sync app  [release|debug] [--restart] (Windows herdr-app + UI defaults)
#   just hot-sync all  [release|debug] [--restart] (core + app + UI defaults)
[unix]
hot-sync *args:
    bash scripts/dev.sh hot-sync {{args}}

# Pack an OTA UI bundle (no-restart hot update) + generate ui/latest.json.
release-ui version base_url out="dist/releases" notes="":
    python3 scripts/release.py ui --version "{{version}}" --base-url "{{base_url}}" --out "{{out}}" --notes "{{notes}}"

# Generate app/latest.json and core/latest.json from packaged release archives.
release-manifest base_url dir="dist/releases" notes="":
    python3 scripts/release.py manifest --base-url "{{base_url}}" --dir "{{dir}}" --notes "{{notes}}"

# Serve a local static release directory for end-to-end upgrade testing.
release-serve dir="dist/releases" port="8787" host="127.0.0.1":
    python3 scripts/release.py serve --dir "{{dir}}" --host "{{host}}" --port "{{port}}"

# Run tests across release scripts, herdr-core, and herdr-app.
test:
    python3 -m unittest discover -s scripts -p "test_*.py"
    cd herdr-core && cargo test --locked
    cd herdr-app && cargo test --locked
