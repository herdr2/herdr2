---
name: herdr2-release
description: "Run the end-to-end Herdr2 release workflow: pre-release verification (tests, docs, QA drift), version bump across Cargo.toml & Windows installer files, clean git commit, push to GitHub main, create/push release tag vX.Y.Z to trigger GitHub Actions installers (macOS .dmg, Windows .exe, Linux .deb/.tar.gz) & rolling OTA hot-update channel (`ota-latest`), monitor workflow status (handling macOS CI quirks & --failed reruns) until release assets are published, and optionally run an end-to-end OTA upgrade smoke test."
---

# Herdr2 Release Workflow Skill

Use this skill whenever the user wants to **publish a new version of Herdr2** (full multi-platform installers on GitHub Releases), **push an OTA hot-update** (`ota-latest` rolling channel), or **verify an end-to-end OTA upgrade**.

## Release Channels Overview

| Channel | Trigger | What Gets Published | Who Receives It |
| --- | --- | --- | --- |
| **UI Hot-Update (No Restart)** | Push to `main` touching `herdr-app/defaults/**` (or `ota-update.yml` dispatch) | `herdr2-ui-bundle.tar.gz` + `ui-latest.json` on tag `ota-latest` | Existing installed Herdr2 users see top-right **Update / 更新** button (~200ms live reload, zero terminal interruption) |
| **Core / App Rolling OTA** | Push to `main` touching `herdr-core/**`, `drove-web/**`, or `herdr-app/src/**` | `core-latest.json` / `app-latest.json` + platform archives on tag `ota-latest` | Existing installed users via top-right **Update / 更新** button or `h2 update` |
| **Full Versioned Release** | Push git tag `vX.Y.Z` (or `release.yml` dispatch) | `Herdr2-Setup-X.Y.Z-windows-x86_64.exe`, `Herdr2-X.Y.Z-macos-aarch64.dmg`, `herdr2_X.Y.Z_amd64.deb`, `herdr2-X.Y.Z-linux-x86_64.tar.gz`, `herdr2-install-SKILL.md`, `SHA256SUMS.txt` + syncs `ota-latest` | New & existing users downloading directly from GitHub Releases |

---

## Procedure: Full Versioned Release (`vX.Y.Z`)

### Step 1: Confirm Target Version & Clean Workspace

1. Inspect current versions, working tree, and remote tags:
   ```bash
   git status --short
   git branch -vv
   grep -m1 '^version' herdr-app/Cargo.toml herdr-core/crates/herdr2/Cargo.toml
   git ls-remote --tags github
   ```
2. Check if target tag `vX.Y.Z` already exists on `github` or locally:
   - If `vX.Y.Z` exists on `github` as a real released Herdr2 version, bump to the next semver version instead of overwriting unless the user explicitly asks to re-tag.
   - If `vX.Y.Z` only exists locally from legacy upstream tags (not on `github`), replace the local tag (`git tag -f -a vX.Y.Z -m "Herdr2 vX.Y.Z"`) after confirming `git ls-remote --tags github` has no conflicting `vX.Y.Z`.
3. Update `version = "X.Y.Z"` in ALL versioned files:
   - `herdr-app/Cargo.toml`
   - `herdr-core/crates/herdr2/Cargo.toml`
   - `herdr-app/distribution/windows/herdr2.iss` (`#define MyAppVersion "X.Y.Z"` default)
   - Refresh lockfiles so `--locked` passes in CI:
     ```bash
     (cd herdr-core && H2_SKIP_WEB_BUILD=1 cargo check --workspace)
     (cd herdr-app && cargo check --workspace)
     ```

### Step 2: Pre-Release Verification Gate

Run all fast checks before committing/tagging (do NOT skip). **Important**: keep `--lib cli::skill` and `--test cli_docs --test skill` as separate commands — passing a lib filter alongside `--test` causes Cargo to filter out the integration tests (`0 passed; N filtered out`).

```bash
# 1. Release script & OTA manifest tests
python3 -m unittest discover -s scripts -p "test_*.py"

# 2. Frontend Mod & Welcome / Settings / Tabs unit tests
(cd herdr-app && bun test scripts/*.test.ts)

# 3. Core CLI unit tests + docs + Skill integration tests (separate commands!)
(cd herdr-core && H2_SKIP_WEB_BUILD=1 cargo test --locked -p herdr2 --lib cli::skill)
(cd herdr-core && H2_SKIP_WEB_BUILD=1 cargo test --locked -p herdr2 --test cli_docs --test skill)
# Also run `--test machines` if `machines.rs` changed, and `(cd drove-web && npm run build)` if `drove-web/` changed.

# 4. App updater, core discovery, hotkeys, and GPUI QuickJS render tests
(cd herdr-app && cargo test --locked --bin herdr2 -- \
  update::tests ui_update::tests core_bin::tests keys::tests \
  mods::render_tests::settings_sections_and_updates \
  mods::render_tests::brand_and_panel_buttons_render_by_default)

# 5. QA contract drift check
python3 scripts/qa/sync_check.py --drift
```

If `cli_docs` reports `docs/commands.md is stale`, refresh it with:
```bash
(cd herdr-core && H2_SKIP_WEB_BUILD=1 H2_UPDATE_DOCS=1 cargo test --locked -p herdr2 --test cli_docs)
```

If platform-gated Rust code (`#[cfg(windows)]` / `#[cfg(target_os = "macos")]`) was modified, audit the symbol paths against `Cargo.toml` features before pushing to avoid 25-minute CI round-trips.

### Step 3: Commit All Intended Changes

1. Never stage build caches or Python bytecode:
   ```bash
   git checkout -- scripts/__pycache__ 2>/dev/null || true
   ```
2. Review `git status --short` carefully before staging so stray temp files or unrelated edits are not accidentally swept in.
3. Stage intentional changes and commit with a clear conventional commit message (`chore(release): release vX.Y.Z ...`).

### Step 4: Push `main` and Release Tag `vX.Y.Z` to GitHub

Always push `main` first, then create an annotated tag and push the tag to the `github` remote (`herdr2/herdr2`):

```bash
git push github main
git tag -a vX.Y.Z -m "Herdr2 vX.Y.Z"
git push github refs/tags/vX.Y.Z
```

*(If replacing an old unpushed local tag, use `git tag -f -a vX.Y.Z -m "Herdr2 vX.Y.Z"` before pushing.)*

### Step 5: Monitor GitHub Actions & Handle Known CI Quirks

1. List triggered workflows (prefer `gh api` with retries over long-lived `gh run watch`, which can exit on transient network EOFs):
   ```bash
   gh api repos/herdr2/herdr2/actions/runs?per_page=5 \
     --jq '.workflow_runs[] | {id, name, head_branch, status, conclusion, html_url}'
   ```
2. Inspect per-job progress:
   ```bash
   gh api repos/herdr2/herdr2/actions/runs/<run-id>/jobs \
     --jq '.jobs[] | {name, status, conclusion, started_at, completed_at, id, current_step: [.steps[] | select(.status=="in_progress") | .name]}'
   ```
3. **Known CI Characteristics & Recovery Playbook**:
   - **Expected durations**: `ubuntu-24.04` ~20–22m, `windows-latest` ~22–28m, `macos-15-intel` (cross-compiling `--target aarch64-apple-darwin` to avoid GitHub `macos-latest`/`macos-15` arm64 pool 15-minute capacity timeouts) ~35–65m depending on Intel host load.
   - **Linux GLIBC compatibility**: Keep Linux build jobs pinned to `ubuntu-24.04` (glibc 2.39). Do NOT use `ubuntu-latest` for Linux builds — GitHub migrated `ubuntu-latest` to Ubuntu 26.04 (glibc 2.43), which produces `herdr2` binaries that fail on Ubuntu 24.04 LTS with `GLIBC_2.43 not found`.
   - **Do NOT cancel `Build herdr-core & herdr-app + macOS DMG` early**: slow Intel hosts can legitimately take 50–65 minutes to compile both Rust release workspaces + Zig Ghostty + GPUI + QuickJS JIT.
   - **Hung `mlugg/setup-zig@v2`**: If step `Run mlugg/setup-zig@v2` stays `in_progress` for >10 minutes, the Zig mirror download has stalled. Cancel the run (`gh run cancel <run-id> --repo herdr2/herdr2`), wait ~20–30s for `status: completed`, then run:
     ```bash
     gh run rerun <run-id> --failed --repo herdr2/herdr2
     ```
     This preserves already-succeeded Linux/Windows jobs & artifacts and only reruns macOS + `publish`.
   - **Windows MSVC / Zig flags**: Both `herdr-core/vendor/ghostty` and `herdr-app/vendor/ghostty` must keep MSVC 14.52 / Zig 0.16 flags (`-D_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH`, `-Wno-unused-command-line-argument`, `-Wno-pragma-clang-attribute`, `-Wno-array-bounds`) in `pkg/simdutf/build.zig` and `src/build/SharedDeps.zig`.
4. Once `publish` succeeds, verify assets on BOTH `vX.Y.Z` and `ota-latest`:
   ```bash
   gh release view vX.Y.Z --repo herdr2/herdr2
   gh release view ota-latest --repo herdr2/herdr2
   ```
   Confirm all 6 release assets are present on `vX.Y.Z`:
   - `Herdr2-Setup-X.Y.Z-windows-x86_64.exe`
   - `Herdr2-X.Y.Z-macos-aarch64.dmg` (verify macOS job log shows `Mach-O 64-bit executable arm64` for both `herdr-core` and `Herdr2`)
   - `herdr2_X.Y.Z_amd64.deb`
   - `herdr2-X.Y.Z-linux-x86_64.tar.gz`
   - `SHA256SUMS.txt` & `herdr2-install-SKILL.md`

### Step 6: End-to-End OTA Hot-Update Smoke Test (Old Version -> `vX.Y.Z`)

When verifying OTA hot-update end-to-end on Linux (always in an isolated temp dir so the user's real `~/.local/bin` and `~/.config/herdr2-app` are untouched):

1. **Snapshot previous version (`vPREV`)** before or from `vPREV` GitHub Release (`herdr2-PREV-linux-x86_64.tar.gz` or prior `ota-latest` archives).
2. **Core OTA (`h2 update`)**:
   - Extract `vPREV` `herdr2` core binary into `/tmp/<test>/bin/h2`, confirm `./h2 --version` prints `h2 PREV`.
   - Run with isolated `HOME=/tmp/<test>/home` and live `HERDR2_RELEASES_URL=https://github.com/herdr2/herdr2/releases/download/ota-latest`:
     - `h2 update --check` (or `h2 update`) -> downloads `core-latest.json` + `herdr2-core-linux-x86_64.tar.gz`, verifies SHA-256, swaps binary in-place.
     - Confirm `./h2 --version` now prints `h2 X.Y.Z`.
3. **App OTA (`app-latest.json` + binary swap)**:
   - Extract `vPREV` app tarball (`herdr2/herdr2` + `herdr2/herdr2.svg`) into `/tmp/<test>/app/herdr2`, confirm `./herdr2 --version` prints `herdr2 PREV`.
   - Fetch live `app-latest.json`, verify `version == X.Y.Z` and target `linux-x86_64` SHA-256, and exercise real `herdr2` startup/check-and-swap (or `herdr-app` updater flow) against live `ota-latest`, confirming the upgraded executable prints `herdr2 X.Y.Z`.
4. **UI Hot-Update (`ui-latest.json` + `herdr2-ui-bundle.tar.gz`)**:
   - Verify live `ui-latest.json` version (`X.Y.Z-ui.N`), download `herdr2-ui-bundle.tar.gz`, verify SHA-256, and run `herdr-app` UI hot-update apply against live `ota-latest` in an isolated `app_dir`, verifying `ui-bundle.json` receipt and live mod/frame sync without restart.

---

## Procedure: UI-Only OTA Hot-Update (No New Version Tag Needed)

When you only changed JS UI/mods/layouts/themes under `herdr-app/defaults/`:

1. Run `(cd herdr-app && bun test scripts/*.test.ts)` and `python3 scripts/qa/sync_check.py --drift`.
2. Commit & push to `github main`:
   ```bash
   git push github main
   ```
3. `.github/workflows/ota-update.yml` automatically packs `herdr2-ui-bundle.tar.gz` + `ui-latest.json` into `ota-latest` within ~15 seconds.
4. Or trigger manually if needed:
   ```bash
   gh workflow run ota-update.yml --repo herdr2/herdr2 -f build_ui=true
   ```
5. Users' running Herdr2 windows detect the new `ui-latest.json` and display the top-right **Update / 更新** button for instant no-restart hot deployment.
