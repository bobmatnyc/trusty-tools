#!/usr/bin/env python3
"""Score a `compression.jsonl` telemetry file against epic #2343's targets
and render a Markdown report (issue #3869, epic #3866 Slice C).

Why: Slice A (#3867/#3880) and Slice B (#3868/#3885) made compression
effectiveness durable telemetry — `~/.trusty-code/compression.jsonl` (one
JSON line per `CompressionEvent`, schema:
`crates/trusty-code/src/agent_loop/telemetry.rs`). Nothing yet READS that
file and scores it against epic #2343's stated success metric: "a 500+ turn
interactive session with `compaction_events == 0` and working context never
below 60%". This script is that reducer — a pure function library
(`load_events`/`compute_*`/`render_markdown`) plus a thin CLI wrapper, kept
dependency-free (stdlib only) so it runs anywhere `python3` does.

What: reads a JSONL file (optionally scoped to one `session_id`), computes:
  - the `ratio` distribution (min/median/p95/max) across `tcode-cadence`
    events,
  - the working-context floor (min `working_context_pct_after`) plus a
    turn-ordered time series, flagging any sample below 60%,
  - the `tcode-threshold` `compaction_event: true` count (target: 0),
  - per-surface event counts,
and renders all four as one Markdown report with an explicit PASS/FAIL
verdict against epic #2343's two targets.

Test: `compression_report_test.py` (a hand-crafted fixture JSONL, run via
`python3 -m unittest crates/trusty-code/scripts/compression_report_test.py`)
— exercises the ratio-distribution math, the working-context floor
detection, the compaction count, and that a single `compaction_event: true`
row flips the verdict to FAIL, independent of the real soak's output.
Usage: `python3 compression_report.py <compression.jsonl> [--session-id ID]
[--out report.md]`.
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path
from typing import Any

SURFACE_CADENCE = "tcode-cadence"
SURFACE_THRESHOLD = "tcode-threshold"

# Epic #2343's stated success-metric thresholds.
TARGET_MIN_WORKING_CONTEXT_PCT = 60
TARGET_MAX_COMPACTION_EVENTS = 0


def load_events(path: Path, session_id: str | None = None) -> list[dict[str, Any]]:
    """Parse `path` as JSONL, skipping blank/unparseable lines.

    Why: a broken/partial trailing line (the soak process killed mid-write)
    must not take down the whole report — same fail-open posture
    `write_compression_event` itself uses on the write side.
    What: `session_id` (when given) filters to only that session's records —
    the report's job is to score ONE soak run, not every session that has
    ever touched this machine's telemetry file.
    """
    events: list[dict[str, Any]] = []
    with path.open(encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if session_id is not None and event.get("session_id") != session_id:
                continue
            events.append(event)
    return events


def _percentile(sorted_values: list[float], pct: float) -> float:
    """Linear-interpolation percentile (numpy's default method), `pct` in [0, 100].

    Why: stdlib `statistics` has no percentile function before 3.13's
    `quantiles(method="inclusive")` edge cases are fiddly to map onto an
    arbitrary p95 — a small self-contained implementation is more portable
    and easier to unit-test directly.
    """
    if not sorted_values:
        return 0.0
    if len(sorted_values) == 1:
        return sorted_values[0]
    rank = (pct / 100.0) * (len(sorted_values) - 1)
    lo = int(rank)
    hi = min(lo + 1, len(sorted_values) - 1)
    frac = rank - lo
    return sorted_values[lo] + (sorted_values[hi] - sorted_values[lo]) * frac


def compute_ratio_stats(events: list[dict[str, Any]]) -> dict[str, float] | None:
    """Min/median/p95/max `ratio` across `tcode-cadence` events.

    Returns `None` if there are no cadence events at all — the report must
    say so explicitly rather than rendering misleading zeros.
    """
    ratios = sorted(
        e["ratio"] for e in events if e.get("surface") == SURFACE_CADENCE
    )
    if not ratios:
        return None
    return {
        "min": ratios[0],
        "median": statistics.median(ratios),
        "p95": _percentile(ratios, 95),
        "max": ratios[-1],
        "count": len(ratios),
    }


def compute_context_floor(
    events: list[dict[str, Any]],
) -> dict[str, Any]:
    """The working-context floor over the run: min pct, full ordered series,
    and which samples (if any) dropped below the 60% target.

    Why: `working_context_pct_after` is `None` for surfaces with no
    `CadenceOutcome` to derive it from (per `telemetry.rs`'s doc note) — those
    samples are excluded from the series entirely rather than coerced to a
    sentinel, so a `None` can never masquerade as a real (and possibly
    target-violating) percentage.
    """
    series = [
        (i, e["working_context_pct_after"])
        for i, e in enumerate(events)
        if e.get("working_context_pct_after") is not None
    ]
    pcts = [pct for _, pct in series]
    below_target = [
        (i, pct) for i, pct in series if pct < TARGET_MIN_WORKING_CONTEXT_PCT
    ]
    return {
        "series": series,
        "min_pct": min(pcts) if pcts else None,
        "below_target": below_target,
        "sample_count": len(series),
    }


def compute_compaction_count(events: list[dict[str, Any]]) -> int:
    """Count of `tcode-threshold` events with `compaction_event: true`.

    Why: `record_threshold_event` always writes `compaction_event: true` for
    every `tcode-threshold` row (see `telemetry.rs`) — the explicit boolean
    check (rather than just counting `tcode-threshold` rows) future-proofs
    this against that invariant ever loosening.
    """
    return sum(
        1
        for e in events
        if e.get("surface") == SURFACE_THRESHOLD and e.get("compaction_event") is True
    )


def compute_surface_counts(events: list[dict[str, Any]]) -> dict[str, int]:
    """Event count per `surface` value, for the report's overview table."""
    counts: dict[str, int] = {}
    for e in events:
        surface = e.get("surface", "unknown")
        counts[surface] = counts.get(surface, 0) + 1
    return counts


def compute_verdict(
    context_floor: dict[str, Any], compaction_count: int
) -> tuple[bool, str]:
    """PASS/FAIL against epic #2343's two targets.

    What: PASS requires BOTH working_context_pct_after >= 60 at every
    sampled point AND compaction_count == 0. Either violation alone flips
    the whole verdict to FAIL — this is an AND of two independent gates, not
    an average.
    """
    reasons = []
    if context_floor["min_pct"] is None:
        reasons.append("no working-context samples were recorded")
    elif context_floor["below_target"]:
        reasons.append(
            f"working context dropped below {TARGET_MIN_WORKING_CONTEXT_PCT}% "
            f"at {len(context_floor['below_target'])} sample(s) "
            f"(floor: {context_floor['min_pct']}%)"
        )
    if compaction_count > TARGET_MAX_COMPACTION_EVENTS:
        reasons.append(
            f"{compaction_count} threshold-compaction event(s) fired (target: 0)"
        )
    return (len(reasons) == 0, "; ".join(reasons) if reasons else "both targets met")


def render_markdown(
    *,
    session_id: str | None,
    source_path: str,
    total_events: int,
    ratio_stats: dict[str, float] | None,
    context_floor: dict[str, Any],
    compaction_count: int,
    surface_counts: dict[str, int],
    verdict_pass: bool,
    verdict_reason: str,
) -> str:
    lines: list[str] = []
    lines.append("## Verdict")
    lines.append("")
    lines.append(f"**{'PASS' if verdict_pass else 'FAIL'}** — {verdict_reason}")
    lines.append("")
    lines.append(
        f"Targets (epic #2343): working context >= {TARGET_MIN_WORKING_CONTEXT_PCT}% "
        f"at all times; `compaction_events == {TARGET_MAX_COMPACTION_EVENTS}`."
    )
    lines.append("")

    lines.append("## Source")
    lines.append("")
    lines.append(f"- File: `{source_path}`")
    lines.append(f"- Session id filter: `{session_id or '(none — whole file)'}`")
    lines.append(f"- Total events scored: {total_events}")
    lines.append("")

    lines.append("## Event counts per surface")
    lines.append("")
    lines.append("| Surface | Count |")
    lines.append("|---|---|")
    for surface, count in sorted(surface_counts.items()):
        lines.append(f"| `{surface}` | {count} |")
    lines.append("")

    lines.append("## Compression-ratio distribution (`tcode-cadence`)")
    lines.append("")
    if ratio_stats is None:
        lines.append("No `tcode-cadence` events found — cadence never fired.")
    else:
        lines.append("| Stat | Value |")
        lines.append("|---|---|")
        lines.append(f"| Events | {ratio_stats['count']} |")
        lines.append(f"| Min | {ratio_stats['min']:.4f} |")
        lines.append(f"| Median | {ratio_stats['median']:.4f} |")
        lines.append(f"| P95 | {ratio_stats['p95']:.4f} |")
        lines.append(f"| Max | {ratio_stats['max']:.4f} |")
        lines.append(
            "\n(`ratio` = tokens_after / tokens_before; lower is more aggressive "
            "compression.)"
        )
    lines.append("")

    lines.append("## Working-context floor")
    lines.append("")
    if context_floor["min_pct"] is None:
        lines.append("No `working_context_pct_after` samples were recorded.")
    else:
        lines.append(
            f"- Minimum observed: **{context_floor['min_pct']}%** "
            f"({context_floor['sample_count']} samples)"
        )
        if context_floor["below_target"]:
            lines.append(
                f"- **{len(context_floor['below_target'])} sample(s) dropped below "
                f"{TARGET_MIN_WORKING_CONTEXT_PCT}%** — flagged in the table below."
            )
        else:
            lines.append(
                f"- Never dropped below {TARGET_MIN_WORKING_CONTEXT_PCT}% at any sample."
            )
        lines.append("")
        lines.append("| # | working_context_pct_after | |")
        lines.append("|---|---|---|")
        for i, pct in context_floor["series"]:
            flag = " **BELOW TARGET**" if pct < TARGET_MIN_WORKING_CONTEXT_PCT else ""
            lines.append(f"| {i} | {pct}%{flag} |")
    lines.append("")

    lines.append("## Threshold compaction (`tcode-threshold`)")
    lines.append("")
    if compaction_count > 0:
        lines.append(
            f"**{compaction_count} compaction event(s) fired — target is 0.** "
            "This is a FINDING, not a rounding error: the reactive fallback "
            "compactor firing under `cadence: Some(_)` means the cadence "
            "compressor failed to keep the transcript under budget on its own."
        )
    else:
        lines.append("0 threshold-compaction events fired. Target met.")
    lines.append("")

    return "\n".join(lines)


def build_report(
    jsonl_path: Path, session_id: str | None
) -> tuple[str, bool]:
    events = load_events(jsonl_path, session_id)
    ratio_stats = compute_ratio_stats(events)
    context_floor = compute_context_floor(events)
    compaction_count = compute_compaction_count(events)
    surface_counts = compute_surface_counts(events)
    verdict_pass, verdict_reason = compute_verdict(context_floor, compaction_count)
    markdown = render_markdown(
        session_id=session_id,
        source_path=str(jsonl_path),
        total_events=len(events),
        ratio_stats=ratio_stats,
        context_floor=context_floor,
        compaction_count=compaction_count,
        surface_counts=surface_counts,
        verdict_pass=verdict_pass,
        verdict_reason=verdict_reason,
    )
    return markdown, verdict_pass


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("jsonl_path", type=Path, help="Path to compression.jsonl")
    parser.add_argument(
        "--session-id", default=None, help="Scope the report to one session_id"
    )
    parser.add_argument(
        "--out", type=Path, default=None, help="Write Markdown here (default: stdout)"
    )
    args = parser.parse_args(argv)

    markdown, verdict_pass = build_report(args.jsonl_path, args.session_id)
    if args.out is not None:
        args.out.write_text(markdown, encoding="utf-8")
        print(f"wrote {args.out}", file=sys.stderr)
    else:
        print(markdown)
    return 0 if verdict_pass else 1


if __name__ == "__main__":
    raise SystemExit(main())
