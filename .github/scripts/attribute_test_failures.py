#!/usr/bin/env python3
"""Attribute `cargo test --workspace` failures to their owning crate.

Why: the `Test` job used to run a dedicated `cargo test -p trusty-review`
step purely so trusty-review failures showed up individually attributed in
the GitHub Actions UI (closes #950). That re-ran ~1,550 already-covered
tests and forced a full recompile of trusty-common + tga + trusty-review
every run (`-p` resolves a different feature-unification set than
`--workspace`), for one crate's worth of attribution.

What: cross-references `cargo test`'s own `test <name> ... FAILED` lines
(unambiguous per test regardless of interleaved concurrent-binary output)
against a name-to-crate manifest built by `cargo nextest list` — a
non-executing, metadata-only command (see #4402: `cargo nextest run` was
tried first and reverted because its per-test process isolation surfaced a
real, pre-existing trusty-memory test-order bug; `list` runs zero test code
so it can't do that). This gives every crate the same attribution the old
step gave trusty-review alone, without a second recompile or test re-run.

Test: exercised against this PR's own CI run, which organically failed two
trusty-memory tests; this script correctly attributed both to
`trusty-memory` from that run's actual `test-output.log`.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

MANIFEST_PATH = Path("nextest-manifest.json")
LOG_PATH = Path("test-output.log")
FAILURE_LINE = re.compile(r"^test (\S.*?) \.\.\. FAILED$")


def load_name_to_crates(manifest_path: Path) -> dict[str, set[str]]:
    manifest = json.loads(manifest_path.read_text())
    name_to_crates: dict[str, set[str]] = {}
    for suite in manifest.get("rust-suites", {}).values():
        package = suite.get("package-name", "?")
        for name in suite.get("testcases", {}):
            name_to_crates.setdefault(name, set()).add(package)
    return name_to_crates


def main() -> int:
    name_to_crates = load_name_to_crates(MANIFEST_PATH)

    seen: set[str] = set()
    for line in LOG_PATH.read_text(errors="replace").splitlines():
        match = FAILURE_LINE.match(line)
        if not match:
            continue
        name = match.group(1)
        if name in seen:
            continue
        seen.add(name)

        crates = name_to_crates.get(name)
        if crates:
            print(f"::error::test failure in {'/'.join(sorted(crates))}: {name}")
        elif name.startswith("crates/"):
            # Doctest name, e.g. "crates/trusty-common/src/lib.rs - foo (line 12)".
            print(f"::error::doctest failure in {name.split('/', 2)[1]}: {name}")
        else:
            print(f"::error::test failure (crate unknown): {name}")

    if not seen:
        print(
            "::warning::cargo test exited non-zero but no '... FAILED' lines "
            "were found in test-output.log — check it directly"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
