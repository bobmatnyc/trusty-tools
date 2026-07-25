#!/usr/bin/env python3
"""Unit tests for `compression_report.py` (issue #3869's mandatory report-
generator test coverage) against a small, hand-crafted `compression.jsonl`
fixture — never against a real soak run.

Run: `python3 -m unittest crates/trusty-code/scripts/compression_report_test.py`
(or `cd crates/trusty-code/scripts && python3 -m unittest compression_report_test`).

What's covered (per issue #3869's "Test expectations"):
  - the ratio-distribution math (min/median/p95/max) over a handful of
    `tcode-cadence` events,
  - working-context floor detection, including flagging a sample below 60%,
  - the `tcode-threshold` `compaction_event: true` count,
  - that a single `compaction_event: true` row flips the verdict to FAIL.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import compression_report as cr  # noqa: E402


def cadence_event(ratio: float, working_context_pct_after: int, session_id="s1"):
    return {
        "ts": "2026-07-24T00:00:00Z",
        "session_id": session_id,
        "surface": cr.SURFACE_CADENCE,
        "surface_detail": "cadence",
        "tokens_before": 1000,
        "tokens_after": int(1000 * ratio),
        "ratio": ratio,
        "working_context_pct_after": working_context_pct_after,
        "overhead_pct_after": 100 - working_context_pct_after,
        "compaction_event": False,
        "duration_ms": 5,
        "rounds": 1,
    }


def threshold_event(compaction_event: bool, session_id="s1"):
    return {
        "ts": "2026-07-24T00:00:00Z",
        "session_id": session_id,
        "surface": cr.SURFACE_THRESHOLD,
        "surface_detail": "threshold",
        "tokens_before": 5000,
        "tokens_after": 2000,
        "ratio": 0.4,
        "working_context_pct_after": 55,
        "overhead_pct_after": 45,
        "compaction_event": compaction_event,
        "duration_ms": 12,
        "rounds": 1,
    }


class RatioDistributionTests(unittest.TestCase):
    def test_min_median_p95_max(self):
        events = [
            cadence_event(0.9, 90),
            cadence_event(0.5, 80),
            cadence_event(0.7, 85),
            cadence_event(0.3, 95),
            cadence_event(0.6, 88),
        ]
        stats = cr.compute_ratio_stats(events)
        self.assertIsNotNone(stats)
        self.assertEqual(stats["count"], 5)
        self.assertAlmostEqual(stats["min"], 0.3)
        self.assertAlmostEqual(stats["max"], 0.9)
        self.assertAlmostEqual(stats["median"], 0.6)
        # p95 of [0.3, 0.5, 0.6, 0.7, 0.9] via linear interpolation:
        # rank = 0.95 * 4 = 3.8 -> between index 3 (0.7) and 4 (0.9)
        self.assertAlmostEqual(stats["p95"], 0.7 + (0.9 - 0.7) * 0.8)

    def test_no_cadence_events_returns_none(self):
        events = [threshold_event(False)]
        self.assertIsNone(cr.compute_ratio_stats(events))


class WorkingContextFloorTests(unittest.TestCase):
    def test_floor_and_series_order(self):
        events = [
            cadence_event(0.8, 90),
            cadence_event(0.8, 62),
            cadence_event(0.8, 75),
        ]
        floor = cr.compute_context_floor(events)
        self.assertEqual(floor["min_pct"], 62)
        self.assertEqual([pct for _, pct in floor["series"]], [90, 62, 75])
        self.assertEqual(floor["below_target"], [])

    def test_flags_sample_below_60_percent(self):
        events = [
            cadence_event(0.8, 90),
            cadence_event(0.8, 55),  # below the 60% target
            cadence_event(0.8, 75),
        ]
        floor = cr.compute_context_floor(events)
        self.assertEqual(floor["min_pct"], 55)
        self.assertEqual(len(floor["below_target"]), 1)
        self.assertEqual(floor["below_target"][0][1], 55)

    def test_none_percentages_excluded_not_coerced(self):
        events = [
            cadence_event(0.8, 90),
            {**cadence_event(0.8, 90), "working_context_pct_after": None},
        ]
        floor = cr.compute_context_floor(events)
        self.assertEqual(floor["sample_count"], 1)

    def test_floor_breach_row_does_not_double_count_a_real_breach(self):
        """Regression test for the exact bug a code-review pass caught: a
        floor breach writes BOTH a `tcode-cadence` row (the real
        measurement) AND a `tcode-cadence-floor-breach` alarm row for the
        SAME turn. Before the fix, the breach row also carried
        `working_context_pct_after`, so `compute_context_floor` scored the
        one real breach TWICE (a soak run with 21 real breaches reported 42
        below-target samples). `floor_breach_event()`'s `None` percentages
        (matching the fixed production behavior) must collapse this back to
        exactly one sample per breach."""
        events = [
            cadence_event(0.8, 90),
            cadence_event(0.8, 48),  # the one real breach measurement
            floor_breach_event(),  # paired alarm row for the SAME turn
        ]
        floor = cr.compute_context_floor(events)
        self.assertEqual(floor["sample_count"], 2, "not 3 — the breach row adds no sample")
        self.assertEqual(
            len(floor["below_target"]), 1, "not 2 — the one real breach, counted once"
        )
        self.assertEqual(floor["min_pct"], 48)


class CompactionCountTests(unittest.TestCase):
    def test_counts_only_true_compaction_events(self):
        events = [
            threshold_event(True),
            threshold_event(False),
            threshold_event(True),
            cadence_event(0.8, 90),  # never counted — wrong surface
        ]
        self.assertEqual(cr.compute_compaction_count(events), 2)

    def test_zero_when_no_threshold_events(self):
        events = [cadence_event(0.8, 90)]
        self.assertEqual(cr.compute_compaction_count(events), 0)


def floor_breach_event(session_id="s1"):
    """A #3911 cadence floor-breach backstop record — mechanically distinct
    from `threshold_event` above (different surface, same `compaction_event:
    True` alarm-worthy shape).

    `working_context_pct_after`/`overhead_pct_after` are `None`, matching
    production (`telemetry::record_cadence_floor_breach`, post-review fix):
    the paired `tcode-cadence` row from the SAME turn already carries the
    one real measurement — populating it here too would double-count a
    single breach as two floor samples (the exact bug a code-review pass
    caught: a soak run with 21 real breaches reported 42 below-target
    samples before this fix)."""
    return {
        "ts": "2026-07-25T00:00:00Z",
        "session_id": session_id,
        "surface": cr.SURFACE_CADENCE_FLOOR_BREACH,
        "surface_detail": "cadence-floor-breach",
        "tokens_before": 200_000,
        "tokens_after": 104_000,
        "ratio": 0.52,
        "working_context_pct_after": None,
        "overhead_pct_after": None,
        "compaction_event": True,
        "duration_ms": 6,
        "rounds": 7,
    }


class FloorBreachCountTests(unittest.TestCase):
    """Issue #3911: the backstop's own fire count must be a DISTINCT signal
    from `compute_compaction_count` (the threshold-compactor's count) —
    neither surface's rows may be double-counted as the other's."""

    def test_counts_only_floor_breach_surface(self):
        events = [
            floor_breach_event(),
            threshold_event(True),  # different surface — not counted
            cadence_event(0.8, 90),  # different surface — not counted
        ]
        self.assertEqual(cr.compute_floor_breach_count(events), 1)
        self.assertEqual(cr.compute_compaction_count(events), 1)

    def test_zero_when_no_floor_breach_events(self):
        events = [cadence_event(0.8, 90), threshold_event(True)]
        self.assertEqual(cr.compute_floor_breach_count(events), 0)


class VerdictTests(unittest.TestCase):
    def test_pass_when_both_targets_met(self):
        events = [cadence_event(0.8, 90), cadence_event(0.8, 75)]
        floor = cr.compute_context_floor(events)
        compaction_count = cr.compute_compaction_count(events)
        verdict_pass, _ = cr.compute_verdict(floor, compaction_count)
        self.assertTrue(verdict_pass)

    def test_fail_when_context_floor_violated(self):
        events = [cadence_event(0.8, 90), cadence_event(0.8, 50)]
        floor = cr.compute_context_floor(events)
        compaction_count = cr.compute_compaction_count(events)
        verdict_pass, reason = cr.compute_verdict(floor, compaction_count)
        self.assertFalse(verdict_pass)
        self.assertIn("60", reason)

    def test_single_compaction_event_flips_verdict_to_fail(self):
        """The exact regression issue #3869 calls out: one `compaction_event:
        true` row must flip an otherwise-healthy run's verdict to FAIL."""
        events = [
            cadence_event(0.8, 90),
            cadence_event(0.8, 85),
            cadence_event(0.8, 80),
            threshold_event(True),
        ]
        floor = cr.compute_context_floor(events)
        compaction_count = cr.compute_compaction_count(events)
        verdict_pass, reason = cr.compute_verdict(floor, compaction_count)
        self.assertFalse(verdict_pass)
        self.assertIn("threshold-compaction", reason)


class SessionScopingTests(unittest.TestCase):
    def test_load_events_filters_by_session_id(self):
        import json
        import tempfile

        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".jsonl", delete=False
        ) as f:
            f.write(json.dumps(cadence_event(0.8, 90, session_id="keep")) + "\n")
            f.write(json.dumps(cadence_event(0.8, 90, session_id="drop")) + "\n")
            f.write("\n")  # blank line must be skipped, not error
            f.write("not json\n")  # malformed line must be skipped, not error
            path = Path(f.name)
        try:
            events = cr.load_events(path, session_id="keep")
            self.assertEqual(len(events), 1)
            self.assertEqual(events[0]["session_id"], "keep")
        finally:
            path.unlink()


class RenderMarkdownTests(unittest.TestCase):
    def test_build_report_end_to_end_pass(self):
        import json
        import tempfile

        events = [cadence_event(0.8, 90), cadence_event(0.7, 85)]
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".jsonl", delete=False
        ) as f:
            for e in events:
                f.write(json.dumps(e) + "\n")
            path = Path(f.name)
        try:
            markdown, verdict_pass = cr.build_report(path, session_id=None)
            self.assertTrue(verdict_pass)
            self.assertIn("PASS", markdown)
            self.assertIn("tcode-cadence", markdown)
        finally:
            path.unlink()

    def test_build_report_end_to_end_fail_on_compaction(self):
        import json
        import tempfile

        events = [cadence_event(0.8, 90), threshold_event(True)]
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".jsonl", delete=False
        ) as f:
            for e in events:
                f.write(json.dumps(e) + "\n")
            path = Path(f.name)
        try:
            markdown, verdict_pass = cr.build_report(path, session_id=None)
            self.assertFalse(verdict_pass)
            self.assertIn("FAIL", markdown)
        finally:
            path.unlink()

    def test_report_flags_a_breach_the_backstop_never_caught(self):
        """Issue #3911's core reporting acceptance criterion: a floor breach
        with ZERO backstop fires must be called out explicitly — this is
        exactly what the original soak (pre-#3911) observed."""
        import json
        import tempfile

        events = [cadence_event(0.8, 90), cadence_event(0.8, 48)]  # breach, no backstop row
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".jsonl", delete=False
        ) as f:
            for e in events:
                f.write(json.dumps(e) + "\n")
            path = Path(f.name)
        try:
            markdown, _ = cr.build_report(path, session_id=None)
            self.assertIn("FINDING", markdown)
            self.assertIn("backstop fired 0 times", markdown)
        finally:
            path.unlink()

    def test_report_confirms_backstop_engaged_on_a_breach(self):
        import json
        import tempfile

        events = [cadence_event(0.8, 90), cadence_event(0.8, 48), floor_breach_event()]
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".jsonl", delete=False
        ) as f:
            for e in events:
                f.write(json.dumps(e) + "\n")
            path = Path(f.name)
        try:
            markdown, _ = cr.build_report(path, session_id=None)
            self.assertIn("1 backstop fire(s) recorded", markdown)
        finally:
            path.unlink()


if __name__ == "__main__":
    unittest.main()
