import json
import pathlib
import subprocess
import tarfile
import tempfile
import unittest

import release


class ReleaseScriptTests(unittest.TestCase):
    def test_pack_ui_bundle_and_checksum(self):
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            defaults = root / "defaults"
            (defaults / "mods" / "pane").mkdir(parents=True)
            (defaults / "frame" / "settings").mkdir(parents=True)
            (defaults / "mods" / "pane" / "main.js").write_text("export default {}\n", encoding="utf-8")
            (defaults / "mods" / "pane" / "jsconfig.json").write_text("{}\n", encoding="utf-8")
            (defaults / "frame" / "settings" / "main.js").write_text("export default {}\n", encoding="utf-8")

            out = root / "releases"
            manifest = release.pack_ui_bundle(
                defaults_dir=defaults,
                out_root=out,
                version="0.2.1-ui.1",
                base_url="http://127.0.0.1:8787/",
                app_version_min="0.2.0",
                app_version_max="0.2.9",
                host_api_generation=1,
                notes="Test UI bundle",
            )
            self.assertEqual(manifest["version"], "0.2.1-ui.1")
            self.assertEqual(manifest["url"], "http://127.0.0.1:8787/ui/0.2.1-ui.1/herdr2-ui-bundle.tar.gz")
            archive = out / "ui" / "0.2.1-ui.1" / "herdr2-ui-bundle.tar.gz"
            self.assertEqual(manifest["sha256"], release.sha256_file(archive))

            latest = json.loads((out / "ui" / "latest.json").read_text(encoding="utf-8"))
            self.assertEqual(latest, manifest)

            with tarfile.open(archive, "r:gz") as tar:
                names = set(tar.getnames())
            self.assertIn("bundle.json", names)
            self.assertIn("mods/pane/main.js", names)
            self.assertIn("frame/settings/main.js", names)
            self.assertNotIn("mods/pane/jsconfig.json", names)

    def test_write_binary_manifests(self):
        with tempfile.TemporaryDirectory() as td:
            out = pathlib.Path(td)
            (out / "app" / "0.2.1").mkdir(parents=True)
            (out / "core" / "0.2.1").mkdir(parents=True)
            app_pkg = out / "app" / "0.2.1" / "herdr2-linux-x86_64.tar.gz"
            core_pkg = out / "core" / "0.2.1" / "herdr2-windows-x86_64.zip"
            app_pkg.write_bytes(b"app-bytes")
            core_pkg.write_bytes(b"core-bytes")

            written = release.write_binary_manifests(
                out,
                base_url="https://dl.example.com",
                app_version="0.2.1",
                core_version="0.2.1",
                core_generation=1,
                notes="v0.2.1",
            )
            self.assertEqual(written["app"]["targets"]["linux-x86_64"]["sha256"], release.sha256_file(app_pkg))
            self.assertEqual(written["core"]["targets"]["windows-x86_64"]["sha256"], release.sha256_file(core_pkg))
            self.assertTrue((out / "app" / "latest.json").is_file())
            self.assertTrue((out / "core" / "latest.json").is_file())

    def test_sync_ui_defaults_updates_installed_json_and_prunes_edited(self):
        import os

        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            defaults = root / "defaults"
            app_dir = root / "app"
            (defaults / "mods" / "agents").mkdir(parents=True)
            (defaults / "frame" / "settings").mkdir(parents=True)
            (defaults / "mods" / "agents" / "main.js").write_text("// v2\n", encoding="utf-8")
            (defaults / "frame" / "settings" / "main.js").write_text("// settings v2\n", encoding="utf-8")

            # Destination has newer mtime than source, stale file, runtime helper, third-party mod, and *-edited copy.
            (app_dir / "mods" / "agents").mkdir(parents=True)
            (app_dir / "mods" / "agents-edited-2").mkdir(parents=True)
            (app_dir / "mods" / "custom-edited").mkdir(parents=True)
            dst_main = app_dir / "mods" / "agents" / "main.js"
            dst_main.write_text("// old\n", encoding="utf-8")
            os.utime(dst_main, (dst_main.stat().st_atime + 3600, dst_main.stat().st_mtime + 3600))
            (app_dir / "mods" / "agents" / "stale.js").write_text("// stale\n", encoding="utf-8")
            (app_dir / "mods" / "agents" / "jsconfig.json").write_text("{}\n", encoding="utf-8")
            (app_dir / "mods" / "custom-edited" / "main.js").write_text("// user\n", encoding="utf-8")

            res = release.sync_ui_defaults(defaults, app_dir, prune_edited=True)
            self.assertEqual(dst_main.read_text(encoding="utf-8"), "// v2\n")
            self.assertFalse((app_dir / "mods" / "agents" / "stale.js").exists())
            self.assertTrue((app_dir / "mods" / "agents" / "jsconfig.json").is_file())
            self.assertFalse((app_dir / "mods" / "agents-edited-2").exists())
            self.assertTrue((app_dir / "mods" / "custom-edited" / "main.js").is_file())
            self.assertEqual(res["pruned_edited"], 1)

            installed = json.loads((app_dir / "installed.json").read_text(encoding="utf-8"))
            self.assertEqual(
                installed["agents"]["main.js"],
                release.fnv1a64_hex(b"// v2\n"),
            )
            self.assertNotIn("jsconfig.json", installed["agents"])
            # Verify exact FNV-1a 64-bit empty-input offset basis.
            self.assertEqual(release.fnv1a64_hex(b""), "cbf29ce484222325")

            # Verify ui-bundle.json receipt prevents startup rollback on un-rebuilt binaries.
            receipt = json.loads((app_dir / "ui-bundle.json").read_text(encoding="utf-8"))
            self.assertEqual(receipt["app_version"], release.read_app_version())
            self.assertEqual(receipt["version"], f"{release.read_app_version()}-hot-sync")
            self.assertEqual(receipt["sha256"], res["sha256"])

            # Idempotent re-sync keeps applied_at unchanged when content is unchanged.
            applied_at_1 = receipt["applied_at"]
            res2 = release.sync_ui_defaults(defaults, app_dir)
            self.assertEqual(res2["updated_files"], 0)
            receipt2 = json.loads((app_dir / "ui-bundle.json").read_text(encoding="utf-8"))
            self.assertEqual(receipt2["applied_at"], applied_at_1)

    def test_sync_ui_defaults_prunes_stale_frame_and_shipped_assets_only(self):
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            defaults = root / "defaults"
            app_dir = root / "app"
            (defaults / "frame" / "settings").mkdir(parents=True)
            (defaults / "themes").mkdir(parents=True)
            (defaults / "icons").mkdir(parents=True)
            (defaults / "frame" / "settings" / "main.js").write_text("// s1\n", encoding="utf-8")
            (defaults / "themes" / "shipped-a.json").write_text("{}\n", encoding="utf-8")
            (defaults / "themes" / "shipped-b.json").write_text("{}\n", encoding="utf-8")
            (defaults / "icons" / "old.svg").write_text("<svg/>\n", encoding="utf-8")

            # Seed a stale frame file and a user-created custom theme/icon.
            (app_dir / "frame" / "obsolete").mkdir(parents=True)
            (app_dir / "frame" / "obsolete" / "old.js").write_text("// stale\n", encoding="utf-8")
            (app_dir / "themes").mkdir(parents=True)
            (app_dir / "themes" / "my-custom.json").write_text('{"custom":true}\n', encoding="utf-8")
            (app_dir / "icons").mkdir(parents=True)
            (app_dir / "icons" / "my-logo.svg").write_text("<svg id='user'/>\n", encoding="utf-8")

            res1 = release.sync_ui_defaults(defaults, app_dir, app_version="0.2.0")
            self.assertFalse((app_dir / "frame" / "obsolete" / "old.js").exists())
            self.assertFalse((app_dir / "frame" / "obsolete").exists())
            self.assertTrue((app_dir / "themes" / "shipped-b.json").is_file())
            self.assertTrue((app_dir / "themes" / "my-custom.json").is_file())
            self.assertTrue((app_dir / "icons" / "my-logo.svg").is_file())
            self.assertGreaterEqual(res1["removed_files"], 1)

            # Remove a shipped theme and shipped icon from defaults, then re-sync.
            (defaults / "themes" / "shipped-b.json").unlink()
            (defaults / "icons" / "old.svg").unlink()
            res2 = release.sync_ui_defaults(defaults, app_dir, app_version="0.2.0")
            self.assertEqual(res2["removed_files"], 2)
            self.assertFalse((app_dir / "themes" / "shipped-b.json").exists())
            self.assertFalse((app_dir / "icons" / "old.svg").exists())
            # User-created assets remain untouched.
            self.assertTrue((app_dir / "themes" / "my-custom.json").is_file())
            self.assertTrue((app_dir / "icons" / "my-logo.svg").is_file())

    def test_github_release_flat_ui_and_binary_manifests(self):
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            defaults = root / "defaults"
            (defaults / "mods" / "tabs").mkdir(parents=True)
            (defaults / "mods" / "tabs" / "main.js").write_text("export default {}\n", encoding="utf-8")

            out = root / "gh-ota"
            base = "https://github.com/herdr2/herdr2/releases/download/ota-latest"
            ui_m = release.pack_ui_bundle(
                defaults_dir=defaults,
                out_root=out,
                version="0.2.0-ui.42",
                base_url=base,
                app_version_min="0.2.0",
                app_version_max=None,
                host_api_generation=1,
                notes="GitHub push OTA",
                github_release=True,
            )
            self.assertEqual(ui_m["url"], f"{base}/herdr2-ui-bundle.tar.gz")
            self.assertTrue((out / "ui-latest.json").is_file())
            self.assertTrue((out / "herdr2-ui-bundle.tar.gz").is_file())

            (out / "herdr2-app-linux-x86_64.tar.gz").write_bytes(b"app-linux")
            (out / "herdr2-core-windows-x86_64.zip").write_bytes(b"core-win")
            written = release.write_binary_manifests(
                out,
                base_url=base,
                app_version="0.2.0",
                core_version="0.2.0",
                core_generation=1,
                github_release=True,
            )
            self.assertEqual(written["app"]["targets"]["linux-x86_64"]["url"], f"{base}/herdr2-app-linux-x86_64.tar.gz")
            self.assertEqual(written["core"]["targets"]["windows-x86_64"]["url"], f"{base}/herdr2-core-windows-x86_64.zip")
            self.assertTrue((out / "app-latest.json").is_file())
            self.assertTrue((out / "core-latest.json").is_file())
            self.assertTrue((out / "core-0.2.0-manifest.json").is_file())
            self.assertTrue((out / "app-install.sh").is_file())
            self.assertTrue((out / "core-install.ps1").is_file())

    def _init_git_repo(self, root: pathlib.Path) -> None:
        def git(*args: str) -> None:
            subprocess.run(["git", *args], cwd=str(root), check=True, capture_output=True, text=True)

        git("init", "-b", "main")
        git("config", "user.name", "Test User")
        git("config", "user.email", "test@example.com")
        (root / "README.md").write_text("init\n", encoding="utf-8")
        git("add", "README.md")
        git("commit", "-m", "chore: initial commit")
        git("tag", "-a", "v2.0.8", "-m", "Herdr2 v2.0.8")

    def test_release_notes_from_git_commits_and_fallback_priority(self):
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            self._init_git_repo(root)

            def git(*args: str) -> None:
                subprocess.run(["git", *args], cwd=str(root), check=True, capture_output=True, text=True)

            (root / "a.txt").write_text("a\n", encoding="utf-8")
            git("add", "a.txt")
            git("commit", "-m", "feat(ai): support Anthropic/OpenAI protocol auto-detection")

            (root / "b.txt").write_text("b\n", encoding="utf-8")
            git("add", "b.txt")
            git("commit", "-m", "fix(win): avoid curl revocation check failure")

            (root / "ver.txt").write_text("2.0.9\n", encoding="utf-8")
            git("add", "ver.txt")
            git("commit", "-m", "chore(release): release v2.0.9")
            git("tag", "-a", "v2.0.9", "-m", "Herdr2 v2.0.9")

            # Priority 3: no CHANGELOG.md and generic tag annotation -> categorized git commits
            notes_md, ota_summary = release.build_github_release_notes(
                tag="v2.0.9",
                version="2.0.9",
                repo="herdr2/herdr2",
                repo_root=root,
            )
            self.assertIn("### What's Changed", notes_md)
            self.assertIn("#### Features", notes_md)
            self.assertIn("**ai**: support Anthropic/OpenAI protocol auto-detection", notes_md)
            self.assertIn("#### Bug Fixes", notes_md)
            self.assertIn("**win**: avoid curl revocation check failure", notes_md)
            self.assertNotIn("chore(release): release v2.0.9", notes_md)
            self.assertIn("https://github.com/herdr2/herdr2/compare/v2.0.8...v2.0.9", notes_md)
            self.assertIn("Herdr2-Setup-2.0.9-windows-x86_64.exe", notes_md)
            self.assertIn("ai: support Anthropic/OpenAI protocol auto-detection", ota_summary)

            # Priority 2: detailed annotated tag body overrides raw commit log
            git("tag", "-f", "-a", "v2.0.9", "-m", "Herdr2 v2.0.9\n\n- Highlight 1 from tag\n- Highlight 2 from tag")
            tag_notes, tag_ota = release.build_github_release_notes(
                tag="v2.0.9",
                version="2.0.9",
                repo="herdr2/herdr2",
                repo_root=root,
            )
            self.assertIn("- Highlight 1 from tag", tag_notes)
            self.assertIn("Highlight 1 from tag; Highlight 2 from tag", tag_ota)

            # Priority 1: CHANGELOG.md entry overrides both tag body and git log
            (root / "CHANGELOG.md").write_text(
                "# Changelog\n\n"
                "## [v2.0.9] - 2026-11-26\n\n"
                "- **AI**: Curated changelog highlight\n"
                "- **Windows**: Fixed updater prompt\n\n"
                "## v2.0.8\n\n"
                "- Older release\n",
                encoding="utf-8",
            )
            cl_notes, cl_ota = release.build_github_release_notes(
                tag="v2.0.9",
                version="2.0.9",
                repo="herdr2/herdr2",
                repo_root=root,
            )
            self.assertIn("- **AI**: Curated changelog highlight", cl_notes)
            self.assertNotIn("Highlight 1 from tag", cl_notes)
            self.assertNotIn("Older release", cl_notes)
            self.assertIn("AI: Curated changelog highlight; Windows: Fixed updater prompt", cl_ota)

    def test_release_notes_cli_and_single_squashed_commit_bullets(self):
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            self._init_git_repo(root)

            def git(*args: str) -> None:
                subprocess.run(["git", *args], cwd=str(root), check=True, capture_output=True, text=True)

            (root / "c.txt").write_text("c\n", encoding="utf-8")
            git("add", "c.txt")
            git(
                "commit",
                "-m",
                "feat(release): ship v2.0.9 improvements",
                "-m",
                "- Added slash commands\n- Fixed default shell selector",
            )
            git("tag", "-a", "v2.0.9", "-m", "Herdr2 v2.0.9")

            out_md = root / "dist" / "notes.md"
            out_ota = root / "dist" / "ota.txt"
            rc = release.main([
                "notes",
                "--tag",
                "v2.0.9",
                "--version",
                "2.0.9",
                "--repo",
                "herdr2/herdr2",
                "--repo-root",
                str(root),
                "--out",
                str(out_md),
                "--ota-summary-out",
                str(out_ota),
            ])
            self.assertEqual(rc, 0)
            content = out_md.read_text(encoding="utf-8")
            self.assertIn("ship v2.0.9 improvements", content)
            self.assertIn("- Added slash commands", content)
            self.assertIn("- Fixed default shell selector", content)
            self.assertTrue(out_ota.read_text(encoding="utf-8").strip())


if __name__ == "__main__":
    unittest.main()
