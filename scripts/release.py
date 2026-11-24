#!/usr/bin/env python3
"""Herdr2 unified release packer, static manifest generator, and local update test server.

Subcommands:
  ui        Pack herdr-app/defaults/{mods,frame,layouts,themes,icons} into a
            no-restart OTA UI bundle + ui/latest.json.
  manifest  Generate app/latest.json and/or core/latest.json from packaged
            archives under <dir>/{app,core}/<version>/.
  serve     Run a local HTTP server over <dir> for end-to-end upgrade testing.
"""
from __future__ import annotations

import argparse
import contextlib
import functools
import hashlib
import http.server
import io
import json
import os
import pathlib
import re
import shutil
import socketserver
import sys
import tarfile
import time
from typing import Any, Iterator

try:
    import fcntl
except ImportError:  # pragma: no cover - Windows native fallback
    fcntl = None  # type: ignore[assignment]

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
APP_DIR = REPO_ROOT / "herdr-app"
CORE_DIR = REPO_ROOT / "herdr-core"

UI_SUBDIRS = ("mods", "frame", "layouts", "themes", "icons")
SKIP_NAMES = {"node_modules", "gpui-kit.d.ts", "jsconfig.json"}


def sha256_file(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def fnv1a64_hex(data: bytes) -> str:
    """Matches `herdr-app/src/mods.rs::fingerprint` (64-bit FNV-1a hex)."""
    h = 0xCBF29CE484222325
    for b in data:
        h = ((h ^ b) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


def _write_if_changed(dst: pathlib.Path, data: bytes) -> bool:
    if dst.is_file() and dst.read_bytes() == data:
        return False
    dst.parent.mkdir(parents=True, exist_ok=True)
    tmp = dst.with_name(f".{dst.name}.{os.getpid()}.tmp")
    tmp.write_bytes(data)
    os.replace(tmp, dst)
    return True


@contextlib.contextmanager
def _sync_lock(target_app_dir: pathlib.Path) -> Iterator[None]:
    """Serialize concurrent sync-ui runs against the same Herdr2 app directory."""
    target_app_dir.mkdir(parents=True, exist_ok=True)
    if fcntl is None:
        yield
        return
    lock_path = target_app_dir / ".sync-ui.lock"
    with lock_path.open("a+", encoding="utf-8") as fh:
        fcntl.flock(fh.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(fh.fileno(), fcntl.LOCK_UN)


def _prune_empty_dirs(root: pathlib.Path) -> None:
    if not root.is_dir():
        return
    for d in sorted((p for p in root.rglob("*") if p.is_dir()), reverse=True):
        try:
            if not any(d.iterdir()):
                d.rmdir()
        except OSError:
            pass


def _write_hot_sync_receipt(
    target_app_dir: pathlib.Path,
    app_version: str,
    content_sha256: str,
) -> dict[str, Any]:
    """Write/refresh `ui-bundle.json` so `mods.rs::has_active_override()` stays true across restarts.

    Without a matching `ui-bundle.json` for the running `app_version`, restarting an
    un-rebuilt `herdr2` binary would overwrite `frame/` and revert `mods/` back to the
    binary's stale embedded defaults.
    """
    receipt_path = target_app_dir / "ui-bundle.json"
    if receipt_path.is_file():
        try:
            existing = json.loads(receipt_path.read_text(encoding="utf-8"))
            if (
                isinstance(existing, dict)
                and existing.get("app_version") == app_version
                and existing.get("sha256") == content_sha256
            ):
                return existing
        except Exception:
            pass

    receipt: dict[str, Any] = {
        "version": f"{app_version}-hot-sync",
        "app_version": app_version,
        "sha256": content_sha256,
        "applied_at": int(time.time()),
        "notes": "Local hot-sync UI defaults",
    }
    receipt_bytes = (json.dumps(receipt, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
    _write_if_changed(receipt_path, receipt_bytes)
    return receipt


def sync_ui_defaults(
    defaults_dir: pathlib.Path,
    target_app_dir: pathlib.Path,
    prune_edited: bool = False,
    write_receipt: bool = True,
    app_version: str | None = None,
) -> dict[str, Any]:
    """Content-based sync of defaults/{mods,frame,layouts,themes,icons} into a Herdr2 app dir.

    - Serialized via `.sync-ui.lock` so concurrent hot-sync runs cannot interleave writes.
    - Updates `installed.json` with exact `mods.rs` FNV-1a fingerprints for every shipped mod.
    - Prunes stale removed files in `mods/` and `frame/`, and prunes previously shipped files
      removed from `layouts/`, `themes/`, and `icons/` (preserving user-created custom files).
    - Writes/refreshes `ui-bundle.json` (when `write_receipt=True`) so restarting an un-rebuilt
      `herdr2` binary never rolls back hot-synced UI defaults.
    """
    with _sync_lock(target_app_dir):
        updated_files = 0
        removed_files = 0
        pruned_edited: list[str] = []
        digest_hasher = hashlib.sha256()

        installed_path = target_app_dir / "installed.json"
        installed: dict[str, Any] = {}
        if installed_path.is_file():
            try:
                parsed = json.loads(installed_path.read_text(encoding="utf-8"))
                if isinstance(parsed, dict):
                    installed = parsed
            except Exception:
                installed = {}

        shipped_mod_names: set[str] = set()
        mods_src = defaults_dir / "mods"
        mods_dst = target_app_dir / "mods"
        if mods_src.is_dir():
            mods_dst.mkdir(parents=True, exist_ok=True)
            for mod_dir in sorted(p for p in mods_src.iterdir() if p.is_dir() and not p.name.startswith(".")):
                mod_name = mod_dir.name
                shipped_mod_names.add(mod_name)
                dst_mod = mods_dst / mod_name
                dst_mod.mkdir(parents=True, exist_ok=True)
                mod_hashes: dict[str, str] = {}
                keep_rels: set[str] = set()
                for src_file in sorted(p for p in mod_dir.rglob("*") if p.is_file()):
                    rel = src_file.relative_to(mod_dir)
                    if any(part.startswith(".") or part in SKIP_NAMES for part in rel.parts):
                        continue
                    rel_posix = rel.as_posix()
                    keep_rels.add(rel_posix)
                    data = src_file.read_bytes()
                    digest_hasher.update(f"mods/{mod_name}/{rel_posix}\0".encode("utf-8"))
                    digest_hasher.update(data)
                    mod_hashes[rel_posix] = fnv1a64_hex(data)
                    if _write_if_changed(dst_mod / rel, data):
                        updated_files += 1
                for existing in sorted(p for p in dst_mod.rglob("*") if p.is_file()):
                    rel = existing.relative_to(dst_mod)
                    if any(part.startswith(".") or part in SKIP_NAMES for part in rel.parts):
                        continue
                    if rel.as_posix() not in keep_rels:
                        existing.unlink(missing_ok=True)
                        removed_files += 1
                _prune_empty_dirs(dst_mod)
                installed[mod_name] = dict(sorted(mod_hashes.items()))

            if prune_edited:
                for cand in sorted(p for p in mods_dst.iterdir() if p.is_dir()):
                    m = re.match(r"^(.+)-edited(?:-\d+)?$", cand.name)
                    if m and m.group(1) in shipped_mod_names:
                        shutil.rmtree(cand, ignore_errors=True)
                        pruned_edited.append(cand.name)

        # `frame/` is 100% app-owned; prune any stale files no longer in defaults/frame.
        frame_src = defaults_dir / "frame"
        frame_dst = target_app_dir / "frame"
        if frame_src.is_dir():
            frame_dst.mkdir(parents=True, exist_ok=True)
            keep_frame: set[str] = set()
            for src_file in sorted(p for p in frame_src.rglob("*") if p.is_file()):
                rel = src_file.relative_to(frame_src)
                if any(part.startswith(".") or part in SKIP_NAMES for part in rel.parts):
                    continue
                rel_posix = rel.as_posix()
                keep_frame.add(rel_posix)
                data = src_file.read_bytes()
                digest_hasher.update(f"frame/{rel_posix}\0".encode("utf-8"))
                digest_hasher.update(data)
                if _write_if_changed(frame_dst / rel, data):
                    updated_files += 1
            for existing in sorted(p for p in frame_dst.rglob("*") if p.is_file()):
                rel = existing.relative_to(frame_dst)
                if any(part.startswith(".") or part in SKIP_NAMES for part in rel.parts):
                    continue
                if rel.as_posix() not in keep_frame:
                    existing.unlink(missing_ok=True)
                    removed_files += 1
            _prune_empty_dirs(frame_dst)

        # For `layouts/`, `themes/`, and `icons/`, users may keep custom files alongside
        # shipped defaults. Track shipped paths in `.hot-sync-assets.json` so only previously
        # shipped files that were removed from defaults are pruned.
        assets_manifest_path = target_app_dir / ".hot-sync-assets.json"
        prev_shipped_assets: dict[str, list[str]] = {}
        if assets_manifest_path.is_file():
            try:
                parsed_assets = json.loads(assets_manifest_path.read_text(encoding="utf-8"))
                if isinstance(parsed_assets, dict):
                    prev_shipped_assets = {
                        k: [str(x) for x in v]
                        for k, v in parsed_assets.items()
                        if isinstance(k, str) and isinstance(v, list)
                    }
            except Exception:
                prev_shipped_assets = {}

        cur_shipped_assets: dict[str, list[str]] = {}
        for sub in ("layouts", "themes", "icons"):
            src_sub = defaults_dir / sub
            if not src_sub.is_dir():
                continue
            dst_sub = target_app_dir / sub
            dst_sub.mkdir(parents=True, exist_ok=True)
            keep_sub: set[str] = set()
            for src_file in sorted(p for p in src_sub.rglob("*") if p.is_file()):
                rel = src_file.relative_to(src_sub)
                if any(part.startswith(".") or part in SKIP_NAMES for part in rel.parts):
                    continue
                rel_posix = rel.as_posix()
                keep_sub.add(rel_posix)
                data = src_file.read_bytes()
                digest_hasher.update(f"{sub}/{rel_posix}\0".encode("utf-8"))
                digest_hasher.update(data)
                if _write_if_changed(dst_sub / rel, data):
                    updated_files += 1
            for old_rel in prev_shipped_assets.get(sub, []):
                if old_rel not in keep_sub:
                    stale_path = dst_sub / old_rel
                    if stale_path.is_file():
                        stale_path.unlink(missing_ok=True)
                        removed_files += 1
            _prune_empty_dirs(dst_sub)
            cur_shipped_assets[sub] = sorted(keep_sub)

        assets_bytes = (json.dumps(cur_shipped_assets, indent=2, ensure_ascii=False, sort_keys=True) + "\n").encode("utf-8")
        _write_if_changed(assets_manifest_path, assets_bytes)

        installed_bytes = (json.dumps(installed, indent=2, ensure_ascii=False, sort_keys=True) + "\n").encode("utf-8")
        _write_if_changed(installed_path, installed_bytes)

        content_sha256 = digest_hasher.hexdigest()
        receipt_version = None
        if write_receipt:
            ver = app_version or read_app_version()
            receipt = _write_hot_sync_receipt(target_app_dir, ver, content_sha256)
            receipt_version = receipt.get("version")

        return {
            "target": str(target_app_dir),
            "updated_files": updated_files,
            "removed_files": removed_files,
            "shipped_mods": len(shipped_mod_names),
            "pruned_edited": len(pruned_edited),
            "receipt_version": receipt_version,
            "sha256": content_sha256,
        }


def read_app_version(repo_root: pathlib.Path = REPO_ROOT) -> str:
    cargo = (repo_root / "herdr-app/Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r'^version\s*=\s*"([^"]+)"', cargo, re.M)
    if not m:
        raise RuntimeError("cannot read version from herdr-app/Cargo.toml")
    return m.group(1)


def read_ui_host_api_generation(repo_root: pathlib.Path = REPO_ROOT) -> int:
    mods_rs = (repo_root / "herdr-app/src/mods.rs").read_text(encoding="utf-8")
    m = re.search(r"pub const UI_HOST_API_GENERATION:\s*u32\s*=\s*(\d+);", mods_rs)
    if not m:
        raise RuntimeError("cannot read UI_HOST_API_GENERATION from herdr-app/src/mods.rs")
    return int(m.group(1))


def read_core_version_and_generation(repo_root: pathlib.Path = REPO_ROOT) -> tuple[str, int]:
    cargo = (repo_root / "herdr-core/crates/herdr2/Cargo.toml").read_text(encoding="utf-8")
    vm = re.search(r'^version\s*=\s*"([^"]+)"', cargo, re.M)
    api = (repo_root / "herdr-core/crates/herdr2/src/api.rs").read_text(encoding="utf-8")
    gm = re.search(r"pub const GENERATION:\s*u32\s*=\s*(\d+);", api)
    if not vm or not gm:
        raise RuntimeError("cannot read core version/generation")
    return vm.group(1), int(gm.group(1))


def pack_ui_bundle(
    defaults_dir: pathlib.Path,
    out_root: pathlib.Path,
    version: str,
    base_url: str,
    app_version_min: str,
    app_version_max: str | None,
    host_api_generation: int,
    notes: str | None = None,
    github_release: bool = False,
) -> dict[str, Any]:
    if github_release:
        ver_dir = out_root
        ver_dir.mkdir(parents=True, exist_ok=True)
        archive_path = ver_dir / "herdr2-ui-bundle.tar.gz"
    else:
        ver_dir = out_root / "ui" / version
        ver_dir.mkdir(parents=True, exist_ok=True)
        archive_path = ver_dir / "herdr2-ui-bundle.tar.gz"

    bundle_meta = {
        "version": version,
        "app_version_min": app_version_min,
        "app_version_max": app_version_max,
        "host_api_generation": host_api_generation,
        "notes": notes or f"Herdr2 UI {version}",
    }

    with tarfile.open(archive_path, "w:gz") as tar:
        meta_bytes = (json.dumps(bundle_meta, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
        info = tarfile.TarInfo("bundle.json")
        info.size = len(meta_bytes)
        info.mode = 0o644
        tar.addfile(info, io.BytesIO(meta_bytes))

        for sub in UI_SUBDIRS:
            src_sub = defaults_dir / sub
            if not src_sub.is_dir():
                continue
            for path in sorted(src_sub.rglob("*")):
                if not path.is_file():
                    continue
                rel = path.relative_to(defaults_dir)
                parts = rel.parts
                if any(p.startswith(".") or p in SKIP_NAMES for p in parts):
                    continue
                tar.add(path, arcname=rel.as_posix())

    digest = sha256_file(archive_path)
    base = base_url.rstrip("/")
    url = f"{base}/{archive_path.name}" if github_release else f"{base}/ui/{version}/{archive_path.name}"
    manifest: dict[str, Any] = {
        "version": version,
        "app_version_min": app_version_min,
        "host_api_generation": host_api_generation,
        "url": url,
        "sha256": digest,
        "notes": notes or f"Herdr2 UI {version}",
    }
    if app_version_max:
        manifest["app_version_max"] = app_version_max

    text = json.dumps(manifest, indent=2, ensure_ascii=False) + "\n"
    if github_release:
        (out_root / "ui-latest.json").write_text(text, encoding="utf-8")
    else:
        (ver_dir / "manifest.json").write_text(text, encoding="utf-8")
        (out_root / "ui" / "latest.json").write_text(text, encoding="utf-8")
    return manifest


def write_binary_manifests(
    out_root: pathlib.Path,
    base_url: str,
    app_version: str | None = None,
    core_version: str | None = None,
    core_generation: int | None = None,
    notes: str | None = None,
    github_release: bool = False,
) -> dict[str, Any]:
    base = base_url.rstrip("/")
    written: dict[str, Any] = {}

    if app_version:
        app_dir = out_root if github_release else (out_root / "app" / app_version)
        targets: dict[str, dict[str, str]] = {}
        if app_dir.is_dir():
            patterns = ("herdr2-app-*.tar.gz", "herdr2-app-*.zip") if github_release else ("herdr2-*.tar.gz", "herdr2-*.zip")
            archives = [p for pat in patterns for p in sorted(app_dir.glob(pat))]
            if github_release and not archives:
                archives = [
                    p
                    for pat in ("herdr2-*.tar.gz", "herdr2-*.zip")
                    for p in sorted(app_dir.glob(pat))
                    if not p.name.startswith("herdr2-core-") and not p.name.startswith("herdr2-ui-")
                ]
            for archive in archives:
                target = (
                    archive.name.removeprefix("herdr2-app-")
                    .removeprefix("herdr2-")
                    .removesuffix(".tar.gz")
                    .removesuffix(".zip")
                )
                targets[target] = {
                    "url": f"{base}/{archive.name}" if github_release else f"{base}/app/{app_version}/{archive.name}",
                    "sha256": sha256_file(archive),
                }
        if targets:
            doc = {
                "version": app_version,
                "notes": notes or f"Herdr2 {app_version}",
                "targets": targets,
            }
            text = json.dumps(doc, indent=2, ensure_ascii=False) + "\n"
            if github_release:
                (out_root / "app-latest.json").write_text(text, encoding="utf-8")
                for inst in ("install.sh", "install.ps1"):
                    src = APP_DIR / "distribution" / inst
                    if src.is_file():
                        shutil.copy2(src, out_root / f"app-{inst}")
            else:
                (app_dir / "manifest.json").write_text(text, encoding="utf-8")
                (out_root / "app" / "latest.json").write_text(text, encoding="utf-8")
                for inst in ("install.sh", "install.ps1"):
                    src = APP_DIR / "distribution" / inst
                    if src.is_file():
                        shutil.copy2(src, out_root / "app" / inst)
            written["app"] = doc

    if core_version:
        core_dir = out_root if github_release else (out_root / "core" / core_version)
        targets = {}
        if core_dir.is_dir():
            patterns = ("herdr2-core-*.tar.gz", "herdr2-core-*.zip") if github_release else ("herdr2-*.tar.gz", "herdr2-*.zip")
            for archive in [p for pat in patterns for p in sorted(core_dir.glob(pat))]:
                target = (
                    archive.name.removeprefix("herdr2-core-")
                    .removeprefix("herdr2-")
                    .removesuffix(".tar.gz")
                    .removesuffix(".zip")
                )
                targets[target] = {
                    "url": f"{base}/{archive.name}" if github_release else f"{base}/core/{core_version}/{archive.name}",
                    "sha256": sha256_file(archive),
                }
        if targets:
            doc = {
                "version": core_version,
                "generation": core_generation if core_generation is not None else 1,
                "notes": notes or f"herdr2-core {core_version}",
                "targets": targets,
            }
            text = json.dumps(doc, indent=2, ensure_ascii=False) + "\n"
            if github_release:
                (out_root / "core-latest.json").write_text(text, encoding="utf-8")
                (out_root / f"core-{core_version}-manifest.json").write_text(text, encoding="utf-8")
                for inst in ("install.sh", "install.ps1"):
                    src = CORE_DIR / "distribution" / inst
                    if src.is_file():
                        shutil.copy2(src, out_root / f"core-{inst}")
            else:
                (core_dir / "manifest.json").write_text(text, encoding="utf-8")
                (out_root / "core" / "latest.json").write_text(text, encoding="utf-8")
                for inst in ("install.sh", "install.ps1"):
                    src = CORE_DIR / "distribution" / inst
                    if src.is_file():
                        shutil.copy2(src, out_root / "core" / inst)
            written["core"] = doc

    return written


def serve_dir(directory: pathlib.Path, host: str, port: int) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(directory))
    socketserver.TCPServer.allow_reuse_address = True
    with socketserver.TCPServer((host, port), handler) as httpd:
        print(f"Serving Herdr2 releases from {directory} at http://{host}:{port}")
        print(f"Use: HERDR2_RELEASES_URL=http://{host}:{port}")
        try:
            httpd.serve_forever()
        except KeyboardInterrupt:
            print("\nStopped.")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Herdr2 release & OTA UI bundle helper")
    sub = parser.add_subparsers(dest="cmd", required=True)

    p_ui = sub.add_parser("ui", help="Pack an OTA UI bundle and write ui/latest.json")
    p_ui.add_argument("--version", required=True, help="UI bundle version, e.g. 0.2.0-ui.1 or 0.2.1")
    p_ui.add_argument("--base-url", required=True, help="Public or LAN base URL of the release server")
    p_ui.add_argument("--out", required=True, type=pathlib.Path, help="Release root output directory")
    p_ui.add_argument("--defaults", type=pathlib.Path, default=APP_DIR / "defaults")
    p_ui.add_argument("--app-min", default=None, help="Minimum compatible app version (default: current Cargo.toml)")
    p_ui.add_argument("--app-max", default=None, help="Optional maximum compatible app version")
    p_ui.add_argument("--generation", type=int, default=None, help="Host API generation (default: from mods.rs)")
    p_ui.add_argument("--notes", default=None, help="Release notes summary")
    p_ui.add_argument("--github-release", action="store_true", help="Write flat GitHub Releases assets (ui-latest.json + herdr2-ui-bundle.tar.gz)")

    p_man = sub.add_parser("manifest", help="Generate app/latest.json and core/latest.json")
    p_man.add_argument("--dir", required=True, type=pathlib.Path, help="Release root directory")
    p_man.add_argument("--base-url", required=True, help="Release server base URL")
    p_man.add_argument("--app-version", default=None)
    p_man.add_argument("--core-version", default=None)
    p_man.add_argument("--core-generation", type=int, default=None)
    p_man.add_argument("--notes", default=None)
    p_man.add_argument("--github-release", action="store_true", help="Write flat GitHub Releases manifests (app-latest.json, core-latest.json, *-install.*)")

    p_srv = sub.add_parser("serve", help="Run local static HTTP server for update testing")
    p_srv.add_argument("--dir", required=True, type=pathlib.Path)
    p_srv.add_argument("--host", default="127.0.0.1")
    p_srv.add_argument("--port", type=int, default=8787)

    p_sync = sub.add_parser("sync-ui", help="Sync UI defaults into a Herdr2 app dir and update installed.json + ui-bundle.json")
    p_sync.add_argument("--target", required=True, type=pathlib.Path, help="Target Herdr2 app config dir")
    p_sync.add_argument("--defaults", type=pathlib.Path, default=APP_DIR / "defaults")
    p_sync.add_argument("--prune-edited", action="store_true", help="Remove stale *-edited* copies of shipped mods")
    p_sync.add_argument("--no-receipt", action="store_true", help="Do not write/update ui-bundle.json active override receipt")
    p_sync.add_argument("--app-version", default=None, help="Override app_version written into ui-bundle.json")

    args = parser.parse_args(argv)
    if args.cmd == "ui":
        app_min = args.app_min or read_app_version()
        gen = args.generation if args.generation is not None else read_ui_host_api_generation()
        m = pack_ui_bundle(
            args.defaults,
            args.out,
            args.version,
            args.base_url,
            app_min,
            args.app_max,
            gen,
            args.notes,
            github_release=args.github_release,
        )
        print(json.dumps(m, indent=2, ensure_ascii=False))
        return 0

    if args.cmd == "manifest":
        core_ver, core_gen = read_core_version_and_generation()
        res = write_binary_manifests(
            args.dir,
            args.base_url,
            app_version=args.app_version or read_app_version(),
            core_version=args.core_version or core_ver,
            core_generation=args.core_generation if args.core_generation is not None else core_gen,
            notes=args.notes,
            github_release=args.github_release,
        )
        print(json.dumps(res, indent=2, ensure_ascii=False))
        return 0

    if args.cmd == "serve":
        serve_dir(args.dir, args.host, args.port)
        return 0

    if args.cmd == "sync-ui":
        res = sync_ui_defaults(
            args.defaults,
            args.target,
            prune_edited=args.prune_edited,
            write_receipt=not args.no_receipt,
            app_version=args.app_version,
        )
        print(json.dumps(res, indent=2, ensure_ascii=False))
        return 0

    return 1


if __name__ == "__main__":
    sys.exit(main())
