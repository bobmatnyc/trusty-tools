"""Sessions: four statuses (working, waiting, paused, inactive) and one managed-session count, in the server and in the page (the owner 2026-10-25 12:02Z)."""
import json
import os
import re
import shutil
import subprocess
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import dashboard_harness as h  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PAGE = os.path.join(ROOT, "templates/dashboard/index.html")
LIVE = os.path.join(ROOT, "templates/dashboard/live.html")


class TestSessionStatus(unittest.TestCase):
    def test_every_doc_state_maps_to_one_of_four(self):
        docs = ["working", "waiting-owner", "waiting-ci", "idle", "blocked", "parked", "refreshing", "exited", "", None, "unheard-of"]
        for alive in (True, False):
            for raw in (None, "working", "idle", "waiting", "exited"):
                for d in docs:
                    self.assertIn(h.session_status(d, alive, raw), h.STATUSES)
        self.assertEqual(h.session_status("idle", False, "working"), "inactive")      # no tmux session wins over everything
        self.assertEqual(h.session_status("working", True, "exited"), "inactive")     # pane no longer runs Claude
        self.assertEqual(h.session_status("parked", True, "idle"), "paused")
        self.assertEqual(h.session_status("refreshing", True, "working"), "paused")
        self.assertEqual(h.session_status("idle", True, "working"), "working")
        self.assertEqual(h.session_status("blocked", True, "idle"), "waiting")
        self.assertEqual(h.session_status("waiting-owner", True, "idle"), "waiting")
        self.assertEqual(h.session_status("idle", True, "idle", agents_running=True), "working")
        self.assertEqual(h.session_status("working", True, None), "working")           # no live row: the doc decides

    def test_count_is_tmux_ls_plus_recently_gone_docs(self):
        tmux = {"tm-a", "tm-b", "tm-c", "tm-architect", "tm-architect-poll", "other"}   # tm-c has no doc; "other" is not a tm-* session
        now = 1_800_000_000
        iso = lambda ago: __import__("datetime").datetime.fromtimestamp(now - ago, __import__("datetime").timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        docs = [{"name": n, "state": s, "updated": iso(a)} for n, s, a in [("tm-a", "exited", 99999), ("tm-b", "parked", 99999), ("tm-architect", "working", 5),
                ("tm-architect-poll", "idle", 5), ("tm-just-gone", "exited", 3600), ("tm-long-gone", "exited", 3 * 86400)]]
        live = {"tm-a": {"state": "working"}, "tm-b": {"state": "idle"}, "tm-c": {"state": "idle"}, "tm-architect": {"state": "working"}}
        f = h.fleet_status(docs, [], tmux, live, now=now)
        got = {n: v["status"] for n, v in f["sessions"].items()}
        self.assertEqual(got, {"tm-a": "working", "tm-b": "paused", "tm-c": "waiting", "tm-architect": "working", "tm-just-gone": "inactive"})
        self.assertEqual((f["count"], f["inactive"]), (5, 1))                           # poller, long-gone doc and non-tm session excluded
        self.assertEqual(f["count"], len(tmux - {"tm-architect-poll", "other"}) + 1)     # tmux ls (managed names) + the recently ended doc

    def test_tmux_unavailable_falls_back_to_live_file(self):
        f = h.fleet_status([{"name": "tm-a", "state": "idle"}], [], None, {"tm-a": {"state": "idle"}, "tm-z": {"state": "exited"}})
        self.assertEqual({n: v["status"] for n, v in f["sessions"].items()}, {"tm-a": "waiting", "tm-z": "inactive"})

    def test_tmux_sessions_parses_and_degrades(self):
        class R:
            def __init__(self, rc, out="", err=""): self.returncode, self.stdout, self.stderr = rc, out, err
        self.assertEqual(h.tmux_sessions(lambda *a, **k: R(0, "tm-a\ntm-b\n")), {"tm-a", "tm-b"})
        self.assertEqual(h.tmux_sessions(lambda *a, **k: R(1, "", "no server running on /tmp/x")), set())
        self.assertIsNone(h.tmux_sessions(lambda *a, **k: R(1, "", "boom")))

        def missing(*a, **k): raise FileNotFoundError("tmux")
        self.assertIsNone(h.tmux_sessions(missing))


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestPageStatus(unittest.TestCase):
    def run_js(self, body):
        src = open(PAGE, encoding="utf-8").read()
        blk = re.search(r"/\* BEGIN sessionStatus \*/(.*?)/\* END sessionStatus \*/", src, re.S).group(1)
        r = subprocess.run(["node", "-e", blk + "\n" + body], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return json.loads(r.stdout)

    def test_rows_and_count_follow_the_fleet(self):
        body = """
        const docs = [{ id: "S01", name: "tm-a", state: "exited" }, { id: "S02", name: "tm-architect-poll", state: "idle" }, { id: "S03", name: "tm-old", state: "exited" }];
        const fleet = { sessions: { "tm-a": { status: "working" }, "tm-c": { status: "waiting" } } };
        const rows = managedRows(docs, fleet);
        console.log(JSON.stringify({ rows: rows.map((r) => [r.name, r.status, !!r.virtual]), tip: sessionCountTip(rows, fleet),
          fb: managedRows(docs, null).map((r) => [r.name, r.status]), all: ["working","waiting-owner","blocked","idle","parked","refreshing","exited",""].map(docStatus) }));"""
        r = self.run_js(body)
        self.assertEqual(r["rows"], [["tm-a", "working", False], ["tm-c", "waiting", True]])   # stale doc and poller dropped; doc-less live session shown
        self.assertIn("2 in all, 2 live, 0 inactive", r["tip"])
        self.assertEqual(r["fb"], [["tm-a", "inactive"], ["tm-old", "inactive"]])               # no fleet data: poller still excluded
        self.assertEqual(r["all"], ["working", "waiting", "waiting", "waiting", "paused", "paused", "inactive", "waiting"])

    def test_header_line_uses_the_four_counts_only(self):
        src = open(PAGE, encoding="utf-8").read()
        self.assertIn('" sessions · " + cnt("working") + " working · " + cnt("waiting")', src)
        self.assertNotIn('"Agents " + tot', src)
        self.assertIn('c.key === "sessions" ? subs : [["all", "All"', src)   # no All tab for sessions: it added the ledgers to the count

    def test_no_other_labels_remain(self):
        src = open(PAGE, encoding="utf-8").read()
        self.assertRegex(src, r'key: "working".*\n.*key: "waiting".*\n.*key: "paused".*\n.*key: "inactive"')
        for gone in ('"Needs " + (ownerName', "Not classified", 'label: () => "Exited"', 'label: () => "Parked"', "st-exited", "st-refreshing", "Waiting on CI"):
            self.assertNotIn(gone, src)
        live = open(LIVE, encoding="utf-8").read()
        for gone in ("productive", "working slowly", "stalled or blocked", "paused or parked"):
            self.assertNotIn(gone, live)


if __name__ == "__main__":
    unittest.main()
