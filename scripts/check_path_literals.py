#!/usr/bin/env python3
"""check_path_literals.py — refuse a new hard-coded project-home or config-dir path (#11821 P6a).

Why: #11821 makes the projects root and the config dir configurable per install.
A path built from `$HOME` plus `trusty-mpm-projects`, `.trusty-tools` or
`.trusty-mpm` ignores that setting. Code must ask the resolver instead:
`trusty_common::install_paths` in Rust, `scripts/lib/install_paths.py` in Python.
What: scans tracked source files (EXTENSIONS) line by line, skipping comment-only
lines and lines that carry the waiver `path-literal: allow`. A line is a hit when
it matches one of PATTERNS:
  tilde       `~/`, `$HOME/` or `${HOME}/` followed by one of the three names;
  home-join   a word containing `home` followed, within 60 characters on the
              same line, by one of the three names as a quoted path segment
              (`Path.home() / ".trusty-tools"`, `home_dir().join(".trusty-mpm")`).
A per-project `<project>/.trusty-mpm` is not home-rooted and does not match.
ALLOW lists the resolvers themselves. The existing hits are counted per file in
BASELINE. A file whose hit count exceeds its baseline count fails (FAIL NEW; every
hit in the file is printed, since a count cannot say which line is new). A file
below its baseline is a NOTE, never a failure, so the P3 and P4 migrations need
not edit the baseline in the same PR.
Shrinking the baseline: after a migration, `--shrink` lowers each row to the
current count and drops rows at zero. It never raises a count. `--seed` writes
the baseline from scratch; it exists for #11821 P6a and a count it raises
needs review.
Test: scripts/tests/test_check_path_literals.py.
Usage: scripts/check_path_literals.py [--root DIR] [--baseline FILE] [--shrink | --seed]
  Exit 0: no file over its baseline. Exit 1: FAIL lines. Exit 2: usage or read error.
"""
from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys

NAMES = r"(?:trusty-mpm-projects|\.trusty-tools|\.trusty-mpm)"
PATTERNS = (
    ("tilde", re.compile(r"(?:~|\$HOME|\$\{HOME\})/" + NAMES + r"(?![\w.-])")),
    ("home-join", re.compile(r"(?i:home)[^\n]{0,60}?[\"']/?" + NAMES + r"(?=[\"'/])")),
)
EXTENSIONS = (".py", ".sh", ".bash", ".rs", ".ts", ".js", ".mjs", ".cjs", ".svelte")
COMMENT = re.compile(r"^\s*(?:#|//|/\*|\*|--)")
WAIVER = "path-literal: allow"
ALLOW = (
    "crates/trusty-common/src/install_paths/",
    "scripts/lib/install_paths.py",
    "scripts/check_path_literals.py",
    "scripts/tests/test_check_path_literals.py",
    "scripts/tests/test_install_paths.py",
)
DEFAULT_BASELINE = "scripts/path_literals_baseline.tsv"
HEADER = (
    "# Hard-coded project-home / config-dir literals per file (#11821 P6a).\n"
    "# Written by scripts/check_path_literals.py. A count may only fall: run `--shrink` after a migration.\n"
)


def tracked(root: str) -> list[str]:
    out = subprocess.run(["git", "-C", root, "ls-files", "-z"], capture_output=True, check=True)
    return sorted(p for p in out.stdout.decode().split("\0") if p.endswith(EXTENSIONS))


def hits_in(text: str) -> list[tuple[int, str, str]]:
    """(line number, pattern name, line) for each hit line in `text`."""
    found = []
    for n, line in enumerate(text.splitlines(), 1):
        if COMMENT.match(line) or WAIVER in line:
            continue
        for name, rx in PATTERNS:
            if rx.search(line):
                found.append((n, name, line.strip()))
                break
    return found


def scan(root: str) -> dict[str, list[tuple[int, str, str]]]:
    result = {}
    for rel in tracked(root):
        if rel.startswith(ALLOW):
            continue
        try:
            with open(os.path.join(root, rel), encoding="utf-8", errors="replace") as f:
                found = hits_in(f.read())
        except FileNotFoundError:  # deleted in the worktree but still in the index
            continue
        if found:
            result[rel] = found
    return result


def read_baseline(path: str) -> dict[str, int]:
    rows = {}
    with open(path, encoding="utf-8") as f:
        for n, line in enumerate(f, 1):
            if not line.strip() or line.startswith("#"):
                continue
            parts = line.rstrip("\n").split("\t")
            if len(parts) != 2 or not parts[1].isdigit():
                raise ValueError(f"{path}:{n}: expected `<path>\\t<count>`")
            rows[parts[0]] = int(parts[1])
    return rows


def write_baseline(path: str, rows: dict[str, int]) -> None:
    with open(path, "w", encoding="utf-8") as f:
        f.write(HEADER)
        for rel in sorted(rows):
            f.write(f"{rel}\t{rows[rel]}\n")


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--root", default=os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    ap.add_argument("--baseline", default=None, help=f"default: <root>/{DEFAULT_BASELINE}")
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--shrink", action="store_true", help="lower baseline rows to the current counts")
    mode.add_argument("--seed", action="store_true", help="write the baseline from the current counts")
    a = ap.parse_args(argv)
    baseline_path = a.baseline or os.path.join(a.root, DEFAULT_BASELINE)
    try:
        current = scan(a.root)
        base = {} if a.seed else read_baseline(baseline_path)
    except (OSError, ValueError, subprocess.CalledProcessError) as e:
        print(f"FAIL READ: {e}")
        return 2
    counts = {rel: len(h) for rel, h in current.items()}
    if a.seed:
        write_baseline(baseline_path, counts)
        print(f"seeded {baseline_path}: {len(counts)} files, {sum(counts.values())} hits")
        return 0
    if a.shrink:
        shrunk = {rel: min(n, counts.get(rel, 0)) for rel, n in base.items()}
        write_baseline(baseline_path, {rel: n for rel, n in shrunk.items() if n})
        print(f"shrunk {baseline_path}: {sum(base.values())} -> {sum(shrunk.values())} hits")
    failed = 0
    for rel, found in current.items():
        if len(found) > base.get(rel, 0):
            failed += 1
            print(f"FAIL NEW: {rel}: {len(found)} hits, baseline {base.get(rel, 0)}; resolve the path through "
                  f"install_paths, or waive a non-path line with `{WAIVER}`")
            for n, name, line in found:
                print(f"  {rel}:{n}: [{name}] {line[:160]}")
    if not a.shrink:
        for rel, n in sorted(base.items()):
            if counts.get(rel, 0) < n:
                print(f"NOTE: {rel}: {counts.get(rel, 0)} hits, baseline {n}; `--shrink` can lower it")
    if failed:
        return 1
    print(f"check_path_literals: ok ({sum(counts.values())} baselined hits in {len(counts)} files)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
