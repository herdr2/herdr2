#!/usr/bin/env python3
"""Sync, drift-check, catalog-render, and change-impact analyzer for Herdr2 QA."""

from __future__ import annotations

import argparse
import fnmatch
import json
import pathlib
import re
import shutil
import subprocess
import sys
from collections import defaultdict
from typing import Any

import yaml

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
QA_DIR = REPO_ROOT / "qa"
FEATURES_FILE = QA_DIR / "features.yaml"
CASES_DIR = QA_DIR / "cases"
CATALOG_FILE = QA_DIR / "CATALOG.md"
OPENRPC_FILE = REPO_ROOT / "herdr-core" / "schema" / "h2.openrpc.json"
APP_MODS_DIR = REPO_ROOT / "herdr-app" / "defaults" / "mods"
APP_FRAMES_DIR = REPO_ROOT / "herdr-app" / "defaults" / "frame"
APP_JUSTFILE = REPO_ROOT / "herdr-app" / "justfile"
APP_SCRIPTS_DIR = REPO_ROOT / "herdr-app" / "scripts"
APP_COMMANDS_RS = REPO_ROOT / "herdr-app" / "src" / "commands.rs"


def load_yaml(path: pathlib.Path) -> Any:
    if not path.is_file():
        return {}
    with path.open("r", encoding="utf-8") as fh:
        return yaml.safe_load(fh) or {}


def load_features() -> list[dict[str, Any]]:
    raw = load_yaml(FEATURES_FILE)
    if isinstance(raw, list):
        return raw
    if isinstance(raw, dict):
        return list(raw.get("features") or [])
    return []


def load_cases() -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []
    if not CASES_DIR.is_dir():
        return cases
    for path in sorted(CASES_DIR.glob("*.yaml")):
        raw = load_yaml(path)
        items = raw if isinstance(raw, list) else (raw.get("cases") or [] if isinstance(raw, dict) else [])
        for item in items:
            if isinstance(item, dict):
                copy = dict(item)
                copy["_source_file"] = str(path.relative_to(REPO_ROOT))
                cases.append(copy)
    return cases


def normalize_list(val: Any) -> list[str]:
    if val is None:
        return []
    if isinstance(val, str):
        return [val]
    if isinstance(val, list):
        return [str(v) for v in val if v is not None]
    return []


def extract_covers(obj: dict[str, Any], key: str) -> set[str]:
    covers = obj.get("covers")
    if not isinstance(covers, dict):
        return set()
    return set(normalize_list(covers.get(key)))


def glob_match(rel_path: str, pattern: str) -> bool:
    """Match a POSIX relative path against a glob supporting `**`, `*`, `?`."""
    path = rel_path.replace("\\", "/").lstrip("./")
    pat = pattern.replace("\\", "/").lstrip("./")
    if pat.endswith("/"):
        return path.startswith(pat)
    if "**" not in pat:
        return fnmatch.fnmatchcase(path, pat)

    # Convert glob with ** into regex
    tokens = re.split(r"(\*\*/|\*\*|\*|\?)", pat)
    regex_parts = ["^"]
    for tok in tokens:
        if tok == "**/":
            regex_parts.append("(?:.*/)?")
        elif tok == "**":
            regex_parts.append(".*")
        elif tok == "*":
            regex_parts.append("[^/]*")
        elif tok == "?":
            regex_parts.append("[^/]")
        else:
            regex_parts.append(re.escape(tok))
    regex_parts.append("$")
    return re.match("".join(regex_parts), path) is not None


def discover_openrpc_methods() -> list[str]:
    if not OPENRPC_FILE.is_file():
        return []
    data = json.loads(OPENRPC_FILE.read_text(encoding="utf-8"))
    return sorted({m["name"] for m in data.get("methods", []) if isinstance(m, dict) and m.get("name")})


def resolve_main_repo() -> pathlib.Path:
    p = subprocess.run(["git", "-C", str(REPO_ROOT), "rev-parse", "--git-common-dir"], capture_output=True, text=True)
    if p.returncode == 0 and p.stdout.strip():
        common = (REPO_ROOT / p.stdout.strip()).resolve()
        if common.name == ".git":
            return common.parent
    return REPO_ROOT


def resolve_app_binary() -> pathlib.Path | None:
    main_repo = resolve_main_repo()
    for cand in (
        REPO_ROOT / "herdr-app" / "target" / "debug" / "herdr2",
        REPO_ROOT / "herdr-app" / "target" / "release" / "herdr2",
        main_repo / "herdr-app" / "target" / "debug" / "herdr2",
        main_repo / "herdr-app" / "target" / "release" / "herdr2",
    ):
        if cand.is_file() and cand.stat().st_mode & 0o111:
            return cand
    return None


def discover_app_commands() -> tuple[list[str], str]:
    app_bin = resolve_app_binary()
    if app_bin is not None:
        proc = subprocess.run([str(app_bin), "commands"], capture_output=True, text=True, timeout=15)
        if proc.returncode == 0:
            try:
                parsed = json.loads(proc.stdout)
                if isinstance(parsed, list):
                    names = sorted({item["name"] for item in parsed if isinstance(item, dict) and item.get("name")})
                    if names:
                        return names, str(app_bin)
            except json.JSONDecodeError:
                pass

    # Fallback: parse herdr-app/src/commands.rs statically if binary not built yet
    if APP_COMMANDS_RS.is_file():
        src = APP_COMMANDS_RS.read_text(encoding="utf-8")
        names = sorted(set(re.findall(r'\bcmd\(\s*"([a-z0-9_.-]+)"', src)))
        return names, f"static:{APP_COMMANDS_RS.relative_to(REPO_ROOT)}"

    return [], "unavailable"


def discover_dir_names(base: pathlib.Path) -> list[str]:
    if not base.is_dir():
        return []
    return sorted(p.name for p in base.iterdir() if p.is_dir() and not p.name.startswith("."))


def check_justfile_bun_tests() -> dict[str, Any]:
    actual = sorted(p.name for p in APP_SCRIPTS_DIR.glob("*.test.ts")) if APP_SCRIPTS_DIR.is_dir() else []
    listed: set[str] = set()
    if APP_JUSTFILE.is_file():
        text = APP_JUSTFILE.read_text(encoding="utf-8")
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith("bun test "):
                for tok in stripped.split()[2:]:
                    if tok.endswith(".test.ts"):
                        listed.add(pathlib.Path(tok).name)
    missing = sorted(set(actual) - listed)
    stale = sorted(listed - set(actual))
    return {
        "actual_count": len(actual),
        "listed_count": len(listed),
        "missing_from_justfile": missing,
        "stale_in_justfile": stale,
    }


def cmd_render() -> int:
    features = load_features()
    cases = load_cases()
    feat_by_id = {str(f.get("id")): f for f in features if isinstance(f, dict) and f.get("id")}
    cases_by_feature: dict[str, list[dict[str, Any]]] = defaultdict(list)
    orphan_cases: list[dict[str, Any]] = []

    tier_counts: dict[str, int] = defaultdict(int)
    mode_counts: dict[str, int] = defaultdict(int)
    prio_counts: dict[str, int] = defaultdict(int)

    for c in cases:
        fid = str(c.get("feature") or "")
        if fid in feat_by_id:
            cases_by_feature[fid].append(c)
        else:
            orphan_cases.append(c)
        tier_counts[str(c.get("tier") or "unknown")] += 1
        mode_counts[str(c.get("mode") or "unknown")] += 1
        prio_counts[str(c.get("priority") or "unknown")] += 1

    lines: list[str] = [
        "# Herdr2 QA Test Catalog",
        "",
        "> Auto-generated by `python3 scripts/qa/sync_check.py --render`. Do not edit by hand.",
        "",
        "## Summary",
        "",
        f"- **Features**: {len(features)}",
        f"- **Test Cases**: {len(cases)}",
        f"- **By Tier**: " + (", ".join(f"`{k}`: {v}" for k, v in sorted(tier_counts.items())) or "none"),
        f"- **By Mode**: " + (", ".join(f"`{k}`: {v}" for k, v in sorted(mode_counts.items())) or "none"),
        f"- **By Priority**: " + (", ".join(f"`{k}`: {v}" for k, v in sorted(prio_counts.items())) or "none"),
        "",
        "## Feature Coverage",
        "",
        "| Feature | Subsystem | Name | Tiers | Cases | Covers (RPC / App / Mods / Frames) |",
        "| --- | --- | --- | --- | ---: | --- |",
    ]

    for feat in features:
        fid = str(feat.get("id") or "")
        sub = str(feat.get("subsystem") or "")
        name = str(feat.get("name") or "")
        tiers = ", ".join(normalize_list(feat.get("tiers")))
        fcases = cases_by_feature.get(fid, [])
        rpc_n = len(set().union(*(extract_covers(c, "rpc") for c in fcases))) if fcases else 0
        app_n = len(set().union(*(extract_covers(c, "app_commands") for c in fcases))) if fcases else 0
        mod_n = len(set().union(*(extract_covers(c, "mods") for c in fcases))) if fcases else 0
        frm_n = len(set().union(*(extract_covers(c, "frames") for c in fcases))) if fcases else 0
        lines.append(
            f"| `{fid}` | `{sub}` | {name} | {tiers} | {len(fcases)} | "
            f"{rpc_n} / {app_n} / {mod_n} / {frm_n} |"
        )

    lines.extend(["", "## Cases by Feature", ""])

    for feat in features:
        fid = str(feat.get("id") or "")
        name = str(feat.get("name") or "")
        desc = str(feat.get("description") or "").strip()
        globs = normalize_list(feat.get("source_globs"))
        lines.append(f"### {fid}: {name}")
        lines.append("")
        if desc:
            lines.append(desc)
            lines.append("")
        if globs:
            lines.append("**Source globs**: " + ", ".join(f"`{g}`" for g in globs))
            lines.append("")

        fcases = cases_by_feature.get(fid, [])
        if not fcases:
            lines.append("_No test cases linked yet._")
            lines.append("")
            continue

        lines.extend(
            [
                "| ID | Title | Tier | Mode | Priority | Platforms | Covers |",
                "| --- | --- | --- | --- | --- | --- | --- |",
            ]
        )
        for c in fcases:
            cid = str(c.get("id") or "")
            title = str(c.get("title") or "").replace("|", "\\|")
            tier = str(c.get("tier") or "")
            mode = str(c.get("mode") or "")
            prio = str(c.get("priority") or "")
            plats = ", ".join(normalize_list(c.get("platforms")))
            cov_parts = []
            for key, label in (("rpc", "rpc"), ("app_commands", "app"), ("mods", "mods"), ("frames", "frames")):
                vals = sorted(extract_covers(c, key))
                if vals:
                    cov_parts.append(f"{label}:{len(vals)}")
            cov_str = ", ".join(cov_parts) if cov_parts else "-"
            lines.append(f"| `{cid}` | {title} | `{tier}` | `{mode}` | `{prio}` | {plats} | {cov_str} |")
        lines.append("")

    if orphan_cases:
        lines.extend(["### Unassigned Feature Cases", ""])
        for c in orphan_cases:
            lines.append(f"- `{c.get('id')}` (feature=`{c.get('feature')}`): {c.get('title')}")
        lines.append("")

    CATALOG_FILE.parent.mkdir(parents=True, exist_ok=True)
    CATALOG_FILE.write_text("\n".join(lines).rstrip() + "\n", encoding="utf-8")
    print(f"Rendered {CATALOG_FILE.relative_to(REPO_ROOT)} ({len(features)} features, {len(cases)} cases)")
    return 0


def cmd_drift(as_json: bool) -> int:
    cases = load_cases()
    covered_rpc: set[str] = set()
    covered_app: set[str] = set()
    covered_mods: set[str] = set()
    covered_frames: set[str] = set()

    for c in cases:
        covered_rpc |= extract_covers(c, "rpc")
        covered_app |= extract_covers(c, "app_commands")
        covered_mods |= extract_covers(c, "mods")
        covered_frames |= extract_covers(c, "frames")

    actual_rpc = set(discover_openrpc_methods())
    app_cmds_list, app_cmds_source = discover_app_commands()
    actual_app = set(app_cmds_list)
    actual_mods = set(discover_dir_names(APP_MODS_DIR))
    actual_frames = set(discover_dir_names(APP_FRAMES_DIR))

    def diff_contract(actual: set[str], covered: set[str]) -> dict[str, Any]:
        missing = sorted(actual - covered)
        stale = sorted(covered - actual)
        return {
            "actual_count": len(actual),
            "covered_count": len(actual & covered),
            "missing": missing,
            "stale": stale,
            "ok": not missing and not stale,
        }

    contracts = {
        "rpc": diff_contract(actual_rpc, covered_rpc),
        "app_commands": {**diff_contract(actual_app, covered_app), "source": app_cmds_source},
        "mods": diff_contract(actual_mods, covered_mods),
        "frames": diff_contract(actual_frames, covered_frames),
    }
    bun_warnings = check_justfile_bun_tests()
    overall_ok = all(section["ok"] for section in contracts.values())

    report = {
        "ok": overall_ok,
        "cases_loaded": len(cases),
        "contracts": contracts,
        "warnings": {"herdr_app_bun_tests": bun_warnings},
    }

    if as_json:
        print(json.dumps(report, indent=2, ensure_ascii=False))
    else:
        print(f"QA Contract Drift Check (cases loaded: {len(cases)})")
        for key, sec in contracts.items():
            status = "OK" if sec["ok"] else "DRIFT"
            extra = f" [source={sec['source']}]" if "source" in sec else ""
            print(f"  - {key}: {status} ({sec['covered_count']}/{sec['actual_count']}){extra}")
            if sec["missing"]:
                print(f"      missing ({len(sec['missing'])}): {', '.join(sec['missing'])}")
            if sec["stale"]:
                print(f"      stale   ({len(sec['stale'])}): {', '.join(sec['stale'])}")
        print("Warnings (non-blocking):")
        if bun_warnings["missing_from_justfile"]:
            print(
                f"  - herdr-app/justfile bun test missing ({len(bun_warnings['missing_from_justfile'])}): "
                + ", ".join(bun_warnings["missing_from_justfile"])
            )
        else:
            print("  - herdr-app/justfile bun test: all scripts/*.test.ts covered")
        if bun_warnings["stale_in_justfile"]:
            print("  - herdr-app/justfile bun test stale: " + ", ".join(bun_warnings["stale_in_justfile"]))

    return 0 if overall_ok else 1


def git_changed_files(git_ref: str | None) -> tuple[list[str], set[str], dict[str, int]]:
    changed: set[str] = set()
    added: set[str] = set()
    churn: dict[str, int] = defaultdict(int)

    def run_git(args: list[str]) -> str:
        p = subprocess.run(["git", *args], cwd=REPO_ROOT, capture_output=True, text=True)
        return p.stdout if p.returncode == 0 else ""

    for cmd in (
        ["diff", "--name-only"],
        ["diff", "--cached", "--name-only"],
        ["ls-files", "--others", "--exclude-standard"],
    ):
        for line in run_git(cmd).splitlines():
            line = line.strip()
            if line:
                changed.add(line)

    for line in run_git(["ls-files", "--others", "--exclude-standard"]).splitlines():
        if line.strip():
            added.add(line.strip())

    for cmd in (["diff", "--name-status"], ["diff", "--cached", "--name-status"]):
        for line in run_git(cmd).splitlines():
            parts = line.split("\t")
            if len(parts) >= 2 and parts[0].startswith("A"):
                added.add(parts[1].strip())

    if git_ref:
        ref_out = run_git(["diff", "--name-status", f"{git_ref}...HEAD"])
        if not ref_out:
            ref_out = run_git(["diff", "--name-status", git_ref])
        for line in ref_out.splitlines():
            parts = line.split("\t")
            if len(parts) >= 2:
                fpath = parts[-1].strip()
                changed.add(fpath)
                if parts[0].startswith("A"):
                    added.add(fpath)

    numstat_cmds = [["diff", "--numstat"], ["diff", "--cached", "--numstat"]]
    if git_ref:
        numstat_cmds.append(["diff", "--numstat", f"{git_ref}...HEAD"])
    for cmd in numstat_cmds:
        for line in run_git(cmd).splitlines():
            parts = line.split("\t")
            if len(parts) >= 3 and parts[0].isdigit() and parts[1].isdigit():
                churn[parts[2].strip()] += int(parts[0]) + int(parts[1])

    return sorted(changed), added, dict(churn)


def cmd_impact(git_ref: str | None, as_json: bool) -> int:
    features = load_features()
    cases = load_cases()
    changed_files, added_files, churn = git_changed_files(git_ref)

    matched_features: dict[str, dict[str, Any]] = {}
    unmatched_files: list[str] = []

    for fpath in changed_files:
        hit = False
        for feat in features:
            fid = str(feat.get("id") or "")
            globs = normalize_list(feat.get("source_globs"))
            if any(glob_match(fpath, g) for g in globs):
                hit = True
                entry = matched_features.setdefault(
                    fid,
                    {
                        "id": fid,
                        "name": feat.get("name", ""),
                        "subsystem": feat.get("subsystem", ""),
                        "matched_files": [],
                    },
                )
                entry["matched_files"].append(fpath)
        if not hit:
            unmatched_files.append(fpath)

    affected_feature_ids = sorted(matched_features.keys())
    affected_cases = sorted(
        {
            str(c.get("id"))
            for c in cases
            if str(c.get("feature") or "") in matched_features and c.get("id")
        }
    )

    review_hints: list[dict[str, Any]] = []
    for fpath in changed_files:
        reasons = []
        if fpath in added_files:
            reasons.append("new_file")
        if churn.get(fpath, 0) >= 80:
            reasons.append(f"large_diff:{churn[fpath]}_lines")
        if reasons:
            review_hints.append({"file": fpath, "reasons": reasons})

    report = {
        "git_ref": git_ref,
        "changed_files": changed_files,
        "affected_features": [matched_features[fid] for fid in affected_feature_ids],
        "affected_feature_ids": affected_feature_ids,
        "affected_case_ids": affected_cases,
        "unmatched_files": unmatched_files,
        "case_update_hints": review_hints,
    }

    if as_json:
        print(json.dumps(report, indent=2, ensure_ascii=False))
    else:
        print(f"Change Impact Analysis (ref={git_ref or 'working-tree'})")
        print(f"  Changed files    : {len(changed_files)}")
        print(f"  Affected features: {', '.join(affected_feature_ids) or 'none'}")
        print(f"  Affected cases   : {', '.join(affected_cases) or 'none'}")
        if unmatched_files:
            print(f"  Unmatched files  : {', '.join(unmatched_files[:15])}")
        if review_hints:
            print("  Files likely needing QA case review/updates:")
            for h in review_hints:
                print(f"    - {h['file']} ({', '.join(h['reasons'])})")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Herdr2 QA catalog renderer, drift checker, and impact analyzer")
    parser.add_argument("--render", action="store_true", help="Render qa/CATALOG.md from features.yaml and cases/*.yaml")
    parser.add_argument("--drift", action="store_true", help="Check contract drift (OpenRPC, App commands, mods, frames)")
    parser.add_argument(
        "--impact",
        nargs="?",
        const="HEAD",
        default=None,
        metavar="GIT_REF",
        help="Analyze changed files (working tree + optional git ref) and list affected features/cases",
    )
    parser.add_argument("--json", action="store_true", help="Output JSON (for --drift / --impact)")
    args = parser.parse_args()

    if not (args.render or args.drift or args.impact is not None):
        parser.print_help()
        return 2

    rc = 0
    if args.render:
        rc = max(rc, cmd_render())
    if args.drift:
        rc = max(rc, cmd_drift(args.json))
    if args.impact is not None:
        ref = None if args.impact == "HEAD" else args.impact
        rc = max(rc, cmd_impact(ref, args.json))
    return rc


if __name__ == "__main__":
    try:
        sys.exit(main())
    except BrokenPipeError:
        sys.exit(0)
