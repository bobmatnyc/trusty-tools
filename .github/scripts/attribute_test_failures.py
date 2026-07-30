#!/usr/bin/env python3
"""Attribute `cargo test --workspace` failures to their owning crate.

Why: the `Test` job used to run a dedicated `cargo test -p trusty-review`
step purely so trusty-review failures showed up individually attributed in
the GitHub Actions UI (closes #950). That re-ran ~1,550 already-covered
tests and forced a full recompile of trusty-common + tga + trusty-review
every run (`-p` resolves a different feature-unification set than
`--workspace`), for one crate's worth of attribution.

What: a Rust panic message self-reports its own source path
(`thread '<test-name>' panicked at crates/<crate>/src/...`), which names
both the failing test and its owning crate in one line, with no dependency
on cross-binary output ordering. This was chosen over two earlier attempts
(#4402) that both proved too costly or too risky:
  1. `cargo nextest run --workspace` replacing the test-execution step
     entirely: nextest isolates every test in its own process (stricter
     than `cargo test`'s one-process-per-binary model), which surfaced a
     real, pre-existing trusty-memory test-order bug on this PR's own CI
     run. Reverted rather than ship a runner switch that can turn a
     previously-green suite red on ordering alone.
  2. `cargo nextest list --workspace` (metadata-only, no test execution)
     to build a name-to-crate manifest, cross-referenced against `cargo
     test`'s unchanged output: safe from the isolation issue above, but
     `cargo nextest list` resolves its own build fingerprint that doesn't
     match `cargo test --workspace --no-run`'s, and measured on this PR's
     CI forced a ~91s recompile of trusty-agents + trusty-agents-local —
     larger than the ~48s the old `-p trusty-review` step cost in the
     first place.
This version needs no extra tool, no extra build, and no execution-model
change: it only reads text `cargo test --workspace` already prints.

Test: exercised against this PR's own CI run, which organically failed
`trusty-memory`'s `add_alias_round_trip_through_prompt_cache` and
`dispatch_discover_aliases_inserts_new_and_dedupes`, and separately a local
run that failed `trusty-agents`'s
`python_skill::tests::execute_dispatches_to_python_and_parses_json`; this
script correctly attributed all three to their crate from the real panic
lines.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

LOG_PATH = Path("test-output.log")

# Unit/integration test failures: the panic message names both the test
# (the thread name libtest gives each test) and its source file in one line.
PANIC_LINE = re.compile(r"^thread '([^']+)'.*panicked at crates/([^/]+)/")

# Doctest failures: libtest's own summary line names the doctest as its
# source-relative path, e.g. "crates/trusty-common/src/lib.rs - foo (line 12)".
DOCTEST_FAILURE_LINE = re.compile(r"^test (crates/(?P<crate>[^/]+)/\S.*) \.\.\. FAILED$")


def main() -> int:
    text = LOG_PATH.read_text(errors="replace")

    attributed: dict[str, str] = {}  # test name -> crate
    for line in text.splitlines():
        m = PANIC_LINE.match(line)
        if m:
            name, crate = m.group(1), m.group(2)
            attributed.setdefault(name, crate)
            continue
        m = DOCTEST_FAILURE_LINE.match(line)
        if m:
            attributed.setdefault(m.group(1), m.group("crate"))

    for name, crate in attributed.items():
        print(f"::error::test failure in {crate}: {name}")

    # Any test libtest reported FAILED but whose panic line we didn't catch
    # (e.g. a bare `panic!()` with no location, or output ordering we
    # didn't anticipate) still gets surfaced, just without attribution.
    failed_names = {
        m.group(1)
        for m in re.finditer(r"^test (\S.*?) \.\.\. FAILED$", text, re.MULTILINE)
    }
    unattributed = failed_names - set(attributed)
    for name in sorted(unattributed):
        print(f"::error::test failure (crate unknown, see test-output.log): {name}")

    if not failed_names:
        print(
            "::warning::cargo test exited non-zero but no '... FAILED' lines "
            "were found in test-output.log — check it directly"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
