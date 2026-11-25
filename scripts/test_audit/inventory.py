#!/usr/bin/env python3
"""inventory.py — Phase 0 of the unit-test audit: list every Rust test, flag candidates (#11268).

Why: the audit logs one verdict per test so no test is judged twice. Phase 0
  builds that log's rows without a model: a stable key, a body hash, and the
  deterministic candidate rules R1, R3, R5, R6 (docs/test-audit/README.md;
  R2 and R4 are dropped).
What: reads tracked `.rs` files under `crates/<dir>/` (git ls-files; nothing
  is built or run), extracts test fns (extract.py), applies R1 (dupes.py) and
  the active per-test rules (rules.py), and merges the rows into the verdict log (verdict_log.py).
  Prints a per-crate summary: tests found, the `git grep -cE
  '#\\[(tokio::)?test'` line count, its code-only share, and rule hits.
    python3 -I scripts/test_audit/inventory.py --crates trusty-secrets trusty-installer
    python3 -I scripts/test_audit/inventory.py --all --check   # exit 1 if the log would change
  R7 (slowest by CI time) is not implemented: CI keeps no per-test timings.
Test: scripts/test_audit/tests/
"""
from __future__ import annotations

import argparse
import datetime as dt
import re
import subprocess
import sys
import tomllib
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))  # scripts/, for `test_audit`

from test_audit.assertions import asserting_helpers  # noqa: E402
from test_audit.dupes import MIN_TOKENS, THRESHOLD, duplicate_rules  # noqa: E402
from test_audit.extract import FileScan, TestFn, extract_tests  # noqa: E402
from test_audit.rules import per_test_rules  # noqa: E402
from test_audit.verdict_log import (  # noqa: E402
    dumps, merge, migrate_legacy_sha, read_log, row, write_log)

RULE_IDS = ("R1", "R3", "R5", "R6")  # #11268: R2 and R4 dropped (rules.DROPPED)
GREP_RE = re.compile(r"#\[(tokio::)?test")
DEFAULT_LOG = Path("docs/test-audit/verdicts.jsonl")


def git(root: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(root), *args], check=True,
                          capture_output=True, text=True).stdout


def crate_dirs(root: Path, names: list[str] | None) -> list[str]:
    if names:
        for n in names:
            if not (root / "crates" / n / "Cargo.toml").is_file():
                raise SystemExit(f"no crate directory crates/{n} with a Cargo.toml")
        return sorted(names)
    tracked = git(root, "ls-files", "crates/*/Cargo.toml").split()
    return sorted(p.split("/")[1] for p in tracked)


def package_name(root: Path, rel: str, cache: dict[str, str]) -> str:
    """`[package] name` of the nearest Cargo.toml at or above the file, up to crates/<dir>."""
    parts = rel.split("/")
    for depth in range(len(parts) - 1, 1, -1):
        d = "/".join(parts[:depth])
        if d not in cache:
            manifest = root / d / "Cargo.toml"
            name = ""
            if manifest.is_file():
                name = tomllib.loads(manifest.read_text(encoding="utf-8")).get(
                    "package", {}).get("name", "")
            cache[d] = name
        if cache[d]:
            return cache[d]
    return parts[1]


def scan_crate(root: Path, crate_dir: str, cache: dict[str, str]) -> tuple[list[tuple[TestFn, FileScan]], dict[str, int]]:
    files = sorted(f for f in git(root, "ls-files", "-z", f"crates/{crate_dir}").split("\0")
                   if f.endswith(".rs"))
    found: list[tuple[TestFn, FileScan]] = []
    stats = Counter()
    for rel in files:
        src = (root / rel).read_text(encoding="utf-8", errors="replace")
        stats["grep_lines"] += sum(1 for line in src.splitlines() if GREP_RE.search(line))
        scan = extract_tests(package_name(root, rel, cache), rel, src)
        stats["code_attr_hits"] += scan.attr_hits
        stats["macro_templates"] += scan.macro_templates
        found.extend((t, scan) for t in scan.tests)
    return found, stats


def build_rows(found: list[tuple[TestFn, FileScan]], rev: str, date: str,
               threshold: float, min_tokens: int) -> list[dict[str, object]]:
    dupes = duplicate_rules([t for t, _ in found], threshold, min_tokens)
    rows = []
    helpers: dict[int, frozenset[str]] = {}
    for t, scan in found:
        if id(scan) not in helpers:
            helpers[id(scan)] = asserting_helpers(scan.helper_bodies)
        hits = per_test_rules(t, scan.asset_names, helpers[id(scan)])
        if t.key in dupes:
            hits["R1"] = dupes[t.key]
        rules = sorted(hits)
        rows.append(row(key=t.key, body_sha=t.body_sha, rules=rules, verdict=None,
                        reason="; ".join(f"{r}: {hits[r]}" for r in rules),
                        evidence=f"{t.path}:{t.line}", reviewer="script",
                        audit_rev=rev, date=date, action="none"))
    return rows


def summary(per_crate: dict[str, tuple[int, Counter, Counter]]) -> str:
    head = ["crate", "tests", "grep -c", "in code", "macro tmpl", *RULE_IDS]
    lines = ["\t".join(head)]
    total = Counter()
    for crate, (n, stats, hits) in sorted(per_crate.items()):
        vals = [n, stats["grep_lines"], stats["code_attr_hits"], stats["macro_templates"],
                *(hits[r] for r in RULE_IDS)]
        total.update(dict(zip(head[1:], vals)))
        lines.append("\t".join([crate, *map(str, vals)]))
    lines.append("\t".join(["TOTAL", *(str(total[h]) for h in head[1:])]))
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sel = ap.add_mutually_exclusive_group(required=True)
    sel.add_argument("--crates", nargs="+", metavar="DIR", help="directories under crates/")
    sel.add_argument("--all", action="store_true", help="every crates/*/Cargo.toml")
    ap.add_argument("--root", type=Path, default=None, help="repo root (default: git toplevel)")
    ap.add_argument("--log", type=Path, default=DEFAULT_LOG, help="relative to the root")
    ap.add_argument("--rev", default=None, help="audit_rev for new rows (default: HEAD)")
    ap.add_argument("--date", default=None, help="date for new rows (default: today, UTC)")
    ap.add_argument("--near-threshold", type=float, default=THRESHOLD)
    ap.add_argument("--min-tokens", type=int, default=MIN_TOKENS)
    ap.add_argument("--check", action="store_true", help="write nothing; exit 1 if the log would change")
    args = ap.parse_args(argv)

    root = args.root or Path(git(Path.cwd(), "rev-parse", "--show-toplevel").strip())
    rev = args.rev or git(root, "rev-parse", "HEAD").strip()
    date = args.date or dt.datetime.now(dt.timezone.utc).date().isoformat()
    dirs = crate_dirs(root, None if args.all else args.crates)
    cache: dict[str, str] = {}
    found: list[tuple[TestFn, FileScan]] = []
    stats_by: dict[str, Counter] = {}
    for d in dirs:
        f, stats = scan_crate(root, d, cache)
        found.extend(f)
        stats_by[d] = stats
    fresh = build_rows(found, rev, date, args.near_threshold, args.min_tokens)
    log_path = root / args.log
    shas = {t.key: (t.body_only_sha, t.body_sha) for t, _ in found}
    old = migrate_legacy_sha(read_log(log_path), shas)
    merged = merge(old, fresh, [f"crates/{d}/" for d in dirs], rev, date)

    per_crate: dict[str, tuple[int, Counter, Counter]] = {}
    for d in dirs:
        rows = [r for r in fresh if str(r["key"]).split("::")[1].startswith(f"crates/{d}/")]
        hits = Counter(rule for r in rows for rule in r["rules"])  # type: ignore[union-attr]
        per_crate[d] = (len(rows), stats_by[d], hits)
    print(summary(per_crate))

    if args.check:
        current = log_path.read_text(encoding="utf-8") if log_path.exists() else ""
        changed = current != dumps(merged)
        print(f"check: {log_path} {'WOULD CHANGE' if changed else 'is current'}")
        return 1 if changed else 0
    changed = write_log(log_path, merged)
    print(f"{log_path}: {len(merged)} rows, {'written' if changed else 'unchanged'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
