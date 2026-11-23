import json
import pathlib
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


if __name__ == "__main__":
    unittest.main()
