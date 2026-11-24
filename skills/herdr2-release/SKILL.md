---
name: herdr2-release
description: "Run the end-to-end Herdr2 release workflow: pre-release verification (tests, docs, QA drift), version bump across Cargo.toml files, clean git commit, push to GitHub main, create/push release tag vX.Y.Z to trigger GitHub Actions installers (macOS .dmg, Windows .exe, Linux .deb/.tar.gz) & rolling OTA hot-update channel (`ota-latest`), and monitor workflow status until release assets are published. Use when the user asks to release a new version of Herdr2, publish a hot-update, tag a release, or check release workflow progress."
---

# Herdr2 Release Workflow Skill

Use this skill whenever the user wants to **publish a new version of Herdr2** (full multi-platform installers on GitHub Releases) or **push an OTA hot-update** (`ota-latest` rolling channel).

## Release Channels Overview

| Channel | Trigger | What Gets Published | Who Receives It |
| --- | --- | --- | --- |
| **UI Hot-Update (No Restart)** | Push to `main` touching `herdr-app/defaults/**` (or `ota-update.yml` dispatch) | `herdr2-ui-bundle.tar.gz` + `ui-latest.json` on tag `ota-latest` | Existing installed Herdr2 users see top-right **Update / 更新** button (~200ms live reload, zero terminal interruption) |
| **Core / App Rolling OTA** | Push to `main` touching `herdr-core/**`, `drove-web/**`, or `herdr-app/src/**` | `core-latest.json` / `app-latest.json` + platform archives on tag `ota-latest` | Existing installed users via top-right **Update / 更新** button or `h2 update` |
| **Full Versioned Release** | Push git tag `vX.Y.Z` (or `release.yml` dispatch) | `Herdr2-Setup-X.Y.Z-windows-x86_64.exe`, `Herdr2-X.Y.Z-macos-aarch64.dmg`, `herdr2_X.Y.Z_amd64.deb`, `herdr2-X.Y.Z-linux-x86_64.tar.gz`, `herdr2-install-SKILL.md`, `SHA256SUMS.txt` + syncs `ota-latest` | New & existing users downloading directly from GitHub Releases |

---

## Procedure: Full Versioned Release (`vX.Y.Z`)

### Step 1: Confirm Target Version & Clean Workspace

1. Inspect current versions and git state:
   ```bash
   git status --short
   git branch -vv
   grep -m1 '^version' herdr-app/Cargo.toml herdr-core/crates/herdr2/Cargo.toml
   git ls-remote --tags github
   ```
2. Check if target tag `vX.Y.Z` already exists on `github` or locally:
   - If `vX.Y.Z` exists on `github` as a real released Herdr2 version, bump to the next semver version instead of overwriting unless the user explicitly asks to re-tag.
   - If `vX.Y.Z` only exists locally from legacy upstream tags (not on `github`), replace the local tag (`git tag -f -a vX.Y.Z -m "Herdr2 vX.Y.Z"`) after confirming `git ls-remote --tags github` has no conflicting `vX.Y.Z`.
3. Update `version = "X.Y.Z"` in BOTH files if bumping to a new version:
   - `herdr-app/Cargo.toml`
   - `herdr-core/crates/herdr2/Cargo.toml`
   - Refresh lockfiles (`cargo check --workspace` in `herdr-core` and `herdr-app`) if Cargo versions changed.

### Step 2: Pre-Release Verification Gate

Run all fast checks before committing/tagging (do NOT skip):

```bash
# 1. Release script & OTA manifest tests
python3 -m unittest discover -s scripts -p "test_*.py"

# 2. Frontend Mod & Welcome / Settings / Tabs unit tests
(cd herdr-app && bun test scripts/*.test.ts)

# 3. Core CLI docs + Skill CLI + Skill stub tests
(cd herdr-core && H2_SKIP_WEB_BUILD=1 cargo test --locked -p herdr2 --lib cli::skill --test cli_docs --test skill)

# 4. App updater, core discovery, and GPUI QuickJS render tests
(cd herdr-app && cargo test --locked --bin herdr2 -- \
  update::tests ui_update::tests core_bin::tests \
  mods::render_tests::settings_sections_and_updates \
  mods::render_tests::brand_and_panel_buttons_render_by_default)

# 5. QA contract drift check
python3 scripts/qa/sync_check.py --drift
```

If `cli_docs` reports `docs/commands.md is stale`, refresh it with:
```bash
(cd herdr-core && H2_SKIP_WEB_BUILD=1 H2_UPDATE_DOCS=1 cargo test --locked -p herdr2 --test cli_docs)
```

### Step 3: Commit All Intended Changes

1. Never stage build caches or Python bytecode (`git checkout -- scripts/__pycache__` if dirty).
2. Stage all intentional changes and commit with a clear conventional commit message (`feat(...)`, `fix(...)`, `chore(release): ...`).
3. Verify `git status --short` is clean (ignoring untracked local scratch if any).

### Step 4: Push `main` and Release Tag `vX.Y.Z` to GitHub

Always push `main` first, then create an annotated tag and push the tag to the `github` remote (`herdr2/herdr2`):

```bash
git push github main
git tag -a vX.Y.Z -m "Herdr2 vX.Y.Z"
git push github refs/tags/vX.Y.Z
```

*(If replacing an old unpushed local tag from upstream history, use `git tag -f -a vX.Y.Z -m "Herdr2 vX.Y.Z"` before pushing.)*

### Step 5: Monitor GitHub Actions & Verify Published Release

1. Verify workflows started on GitHub:
   ```bash
   gh run list --repo herdr2/herdr2 --limit 5
   ```
2. Watch the `Herdr2 Release Installers` run (or report run URL so the user can follow progress):
   ```bash
   gh run watch <run-id> --repo herdr2/herdr2
   ```
3. Once complete, verify assets on both `vX.Y.Z` and `ota-latest`:
   ```bash
   gh release view vX.Y.Z --repo herdr2/herdr2
   gh release view ota-latest --repo herdr2/herdr2
   ```
   Confirm all 4 user installers are present on `vX.Y.Z`:
   - `Herdr2-Setup-X.Y.Z-windows-x86_64.exe`
   - `Herdr2-X.Y.Z-macos-aarch64.dmg`
   - `herdr2_X.Y.Z_amd64.deb`
   - `herdr2-X.Y.Z-linux-x86_64.tar.gz`
   - `SHA256SUMS.txt` & `herdr2-install-SKILL.md`

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
