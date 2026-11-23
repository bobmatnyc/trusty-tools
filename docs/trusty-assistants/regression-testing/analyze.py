#!/usr/bin/env python3
"""Summarize open-mpm performance run files.

Why: docs/performance/runs/ accumulates one JSON file per workflow run. This
script reads them all and prints a compact sortable table (build, date,
workflow, phases, total_ms, total_cost_usd) so we can spot regressions and
track prompt-caching wins without opening each file.

What: Walks `runs/` (relative to this script's directory unless `--dir` is
given), loads each `*.json`, and prints a plain-text table on stdout.
Optional `--workflow <name>` filters to one workflow; `--json` dumps the
collated list as a JSON array (for downstream tooling).

Test: `python docs/performance/analyze.py` against an empty dir prints a
single header row and exits 0.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any, Iterable


def load_runs(runs_dir: Path) -> list[dict[str, Any]]:
    """Load every `<runs_dir>/*.json` into a list of dicts.

    Why: Callers want all records to sort/filter in one pass.
    What: Skips files that fail to parse (logs a warning to stderr), so a
    half-written run file cannot break the summary.
    Test: Pass a dir with one valid + one invalid json; assert len == 1 and
    a warning printed.
    """
    if not runs_dir.exists():
        return []
    out: list[dict[str, Any]] = []
    for path in sorted(runs_dir.glob("*.json")):
        try:
            with path.open("r", encoding="utf-8") as f:
                out.append(json.load(f))
        except (OSError, json.JSONDecodeError) as exc:  # pragma: no cover
            print(f"warning: skipping {path.name}: {exc}", file=sys.stderr)
    return out


def filter_by_workflow(
    runs: Iterable[dict[str, Any]], name: str | None
) -> list[dict[str, Any]]:
    if not name:
        return list(runs)
    return [r for r in runs if r.get("workflow") == name]


def format_table(runs: list[dict[str, Any]]) -> str:
    """Render a left-aligned, fixed-width table of runs.

    Why: Plain stdout keeps this script dependency-free (no `tabulate` / pandas).
    What: Header + rows: build, started_at, workflow, n_phases, total_ms,
    total_cost_usd (6 decimals), cache_read_tokens, cache_creation_tokens.
    Test: Empty input returns just the header line.
    """
    headers = [
        "build",
        "started_at",
        "workflow",
        "phases",
        "total_ms",
        "cost_usd",
        "cache_r",
        "cache_w",
    ]
    rows: list[list[str]] = []
    for r in sorted(runs, key=lambda x: (x.get("build", 0), x.get("started_at", ""))):
        totals = r.get("totals", {})
        rows.append(
            [
                str(r.get("build", "")),
                str(r.get("started_at", "")),
                str(r.get("workflow", "")),
                str(len(r.get("phases", []))),
                str(r.get("total_duration_ms", "")),
                f"{totals.get('cost_usd', 0.0):.6f}",
                str(totals.get("cache_read_tokens", 0)),
                str(totals.get("cache_creation_tokens", 0)),
            ]
        )

    widths = [
        max(len(h), *(len(row[i]) for row in rows)) if rows else len(h)
        for i, h in enumerate(headers)
    ]

    def fmt_row(cells: list[str]) -> str:
        return "  ".join(cell.ljust(widths[i]) for i, cell in enumerate(cells))

    lines = [fmt_row(headers), fmt_row(["-" * w for w in widths])]
    for row in rows:
        lines.append(fmt_row(row))
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    script_dir = Path(__file__).resolve().parent
    default_runs_dir = script_dir / "runs"

    parser = argparse.ArgumentParser(description="Summarize open-mpm perf runs.")
    parser.add_argument(
        "--dir",
        type=Path,
        default=default_runs_dir,
        help=f"runs directory (default: {default_runs_dir})",
    )
    parser.add_argument(
        "--workflow",
        type=str,
        default=None,
        help="filter to a single workflow name",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="emit the collated records as JSON on stdout instead of a table",
    )
    args = parser.parse_args(argv)

    runs = filter_by_workflow(load_runs(args.dir), args.workflow)
    if args.json:
        json.dump(runs, sys.stdout, indent=2)
        sys.stdout.write("\n")
        return 0

    sys.stdout.write(format_table(runs))
    return 0


if __name__ == "__main__":
    sys.exit(main())
