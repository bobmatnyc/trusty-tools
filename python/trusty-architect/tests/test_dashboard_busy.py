"""Post-write refresh and the main-pane Processing overlay (owner 2026-10-21 15:05Z).

Covers: the shim's refresh after a write starts at once even while a poll is in flight and drops the older, slower poll's
result (run in node against a fake /api); a failed write (409) still reloads and then reports an error event; the shim
emits start/settled/error events; the page carries the overlay (main pane only, role=status, aria-live, 150 ms delay,
error state with Dismiss, reduced motion). Skips the node tests when node is missing.
Run: python3 -m unittest scripts/test_dashboard_busy.py
Test: scripts/test_dashboard_busy.py
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
from test_dashboard_store import serve  # noqa: E402

NODE = shutil.which("node")
SHIM = re.search(r'<script>(.*?)</script>', serve.SHIM, re.S).group(1)
with open(serve.PAGE, encoding="utf-8") as f:
    PAGE = f.read()


@unittest.skipUnless(NODE, "node is not installed")
class TestShimRefresh(unittest.TestCase):
    def run_shim(self, scenario, shim=SHIM):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "shim.js")
            with open(p, "w") as f:
                f.write(shim)
            out = subprocess.run([NODE, os.path.join(HERE, "shim_harness.js"), p, scenario], capture_output=True, text=True, timeout=30)
        self.assertEqual(out.returncode, 0, out.stderr)
        return json.loads(out.stdout)

    def test_write_during_a_poll_refreshes_at_once_and_drops_the_stale_poll(self):
        r = self.run_shim("stale")
        self.assertEqual([v for _, v in r["snaps"]], [2])            # the stale v1 poll (400 ms) never reaches the page
        self.assertLess(r["snaps"][0][0] - r["write_at"], 300)      # v2 lands one fetch after the write, not on the next 2 s tick
        self.assertEqual([e["phase"] for _, e in r["events"]], ["start", "settled"])

    def test_the_old_shim_missed_the_refresh(self):
        """Proves the harness can fail: the pre-fix shim shows no v2 within 900 ms."""
        old = SHIM.replace("refreshColls(colls).then(function () { emit(\"settled\"); }, function () { emit(\"settled\"); });", "refreshAllOld();").replace(
            "  function refreshColls(colls) {", "  function refreshAllOld() { listeners.forEach(function (h) { h.tick(); }); }\n  function refreshColls(colls) {")
        old = old.replace("function refresh() { return run(true); }", "function refresh() { return run(false); }")   # the old tick() dropped the refresh while a poll was in flight
        r = self.run_shim("stale", old)
        self.assertNotIn(2, [v for _, v in r["snaps"]])

    def test_conflict_still_reloads_then_reports_the_error(self):
        r = self.run_shim("conflict")
        phases = [e.get("phase") or "thrown" for _, e in r["events"]]
        self.assertEqual(phases, ["start", "thrown", "error"])       # the write still rejects for the caller's own handling
        err = r["events"][-1][1]
        self.assertEqual(err["status"], 409)
        self.assertTrue(r["snaps"])                                   # the doc was re-read


class TestOverlayPage(unittest.TestCase):
    def test_overlay_is_in_the_main_pane_only_and_is_a_live_region(self):
        main = PAGE[PAGE.index('<main id="main">'):PAGE.index("</main>")]
        self.assertRegex(main, r'<div id="main-busy" role="status" aria-live="polite" hidden>')
        self.assertNotIn('id="main-busy"', PAGE[:PAGE.index('<main id="main">')])      # not in the header or the left nav
        self.assertRegex(PAGE, r"#main \{[^}]*position: relative")

    def test_delay_error_state_and_motion(self):
        block = PAGE[PAGE.index("BEGIN busyOverlay"):PAGE.index("END busyOverlay")]
        self.assertIn("}, 150)", block)                               # no flash for a fast write
        self.assertIn("dash:write", block)
        self.assertIn("409", block)
        self.assertIn('x.hidden = !isErr', block)                     # Dismiss only in the error state
        self.assertIn("prefers-reduced-motion: reduce) { .busy-spin", PAGE)
        self.assertIn("color-mix(in srgb, var(--bg) 70%, transparent)", PAGE)   # theme variables, no fixed colours

    def test_shim_emits_the_three_phases(self):
        for ph in ('emit("start")', 'emit("settled")', 'emit("error"'):
            self.assertIn(ph, SHIM)


if __name__ == "__main__":
    unittest.main()
