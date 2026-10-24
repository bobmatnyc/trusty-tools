"""Header reset clocks (owner 2026-10-11): weeklyClock/resetClocks projection (run under node) and the
read-only GET /usage/<slug> route (24 h bound, numbers and times only, unknown account 404); Actions sits after Decisions (no Briefs section)."""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_store as ds  # noqa: E402
from test_dashboard_store import serve  # noqa: E402

PAGE = os.path.join(os.path.dirname(HERE), "templates/dashboard/index.html")
NOW = "2026-10-11T07:20:00Z"


def run_clock(doc, samples, docs=None):
    src = open(PAGE, encoding="utf-8").read()
    blocks = "".join(re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (b, b), src, re.S).group(1)
                     for b in ("localTime", "accounts", "resetClock"))
    js = "const ACCT_BEEP_PCT = 95;" + blocks + """
const now = Date.parse(%s), docs = %s;
console.log(JSON.stringify(resetClocks(docs, %s, now)));""" % (json.dumps(NOW), json.dumps(docs or [doc]), json.dumps(samples))
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


def doc(pctwk=43, resetwk="2026-10-17T15:00:00Z", reset5h="2026-10-11T08:02:00Z"):
    return {"account": "a@example.com", "pct5h": 10, "reset5h": reset5h, "pctwk": pctwk, "resetwk": resetwk, "ts": NOW}


def samples(*pairs):   # (hours ago, pctwk)
    t0 = time.mktime(time.strptime(NOW, "%Y-%m-%dT%H:%M:%SZ")) - time.timezone
    return [{"ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(t0 - h * 3600)), "pctwk": p} for h, p in pairs]


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestProjection(unittest.TestCase):
    def test_rising_trend_gives_login_estimate(self):
        out = run_clock(doc(), samples((10, 33), (0, 43)))      # 1 point/h, 57 left -> ~2d 9h
        self.assertEqual(out, ["5h reset in 42m", "weekly login ~in 2d 9h"])

    def test_projection_after_reset_is_ok_to_reset(self):
        out = run_clock(doc(resetwk="2026-10-12T15:00:00Z"), samples((10, 33), (0, 43)))
        self.assertRegex(out[1], r"^weekly ok to reset \(\w{3} \d+:\d\d [AP]M\)$")

    def test_flat_or_too_few_samples_shows_percent_and_reset(self):
        for s in (samples((10, 43), (0, 43)), samples((0, 43)), samples((0.1, 42), (0, 43)), []):
            self.assertRegex(run_clock(doc(), s)[1], r"^weekly 43%, resets \w{3} \d+:\d\d [AP]M$")

    def test_samples_older_than_24h_ignored_and_drop_restarts_trend(self):
        self.assertRegex(run_clock(doc(), samples((30, 10), (0, 43)))[1], r"^weekly 43%, resets")
        self.assertEqual(run_clock(doc(), samples((10, 90), (6, 30), (0, 36)))[1], "weekly login ~in 2d 9h")  # 1/h after the drop

    def test_past_5h_reset_is_due_and_newest_account_wins(self):
        self.assertEqual(run_clock(doc(reset5h="2026-10-11T07:00:00Z"), [])[0], "5h reset due")
        old = dict(doc(), account="b@example.com", ts="2026-10-10T00:00:00Z", reset5h="2026-10-11T07:30:00Z")
        self.assertEqual(run_clock(None, [], docs=[old, doc()])[0], "5h reset in 42m")
        self.assertEqual(run_clock(None, [], docs=[]), [])


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestHeaderFixes(unittest.TestCase):
    def test_no_window_open_shows_not_started_but_past_time_stays_due(self):
        d = dict(doc(reset5h=None), pct5h=None)
        self.assertEqual(run_clock(d, [])[0], "5h window not started")
        self.assertEqual(run_clock(doc(reset5h="2026-10-11T07:00:00Z"), [])[0], "5h reset due")

    def test_updated_is_newest_of_meta_and_doc_lists(self):
        src = open(PAGE, encoding="utf-8").read()
        blk = re.search(r"/\* BEGIN resetClock \*/(.*?)/\* END resetClock \*/", src, re.S).group(1)
        js = blk + """console.log(JSON.stringify([
 newestUpdated({updated: "2026-10-09T22:46:00Z"}, [[{updated: "2026-10-11T07:10:00Z"}], [{updated: "2026-10-10T01:00:00Z"}, {}], []]),
 newestUpdated({updated: "2026-10-09T22:46:00Z"}, [[], [{updated: "bad"}]]), newestUpdated(null, [])]));"""
        r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads(r.stdout), ["2026-10-11T07:10:00Z", "2026-10-09T22:46:00Z", ""])


class TestUsageRoute(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        t = tmp.name
        now = time.time()
        iso = lambda h: time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now - h * 3600))
        lines = [{"ts": iso(30), "account": "a@example.com", "pct5h": 1, "pctwk": 1},
                 {"ts": iso(2), "account": "a@example.com", "pct5h": 5, "reset5h": iso(-3), "pctwk": 40, "resetwk": iso(-90), "source": "s", "weekly_models": {"k": 1}},
                 {"ts": iso(1), "account": "b@example.com", "pct5h": 9, "pctwk": 9}]
        ledger = os.path.join(t, "ledger.jsonl")
        with open(ledger, "w") as f:
            f.write("\n".join(json.dumps(x) for x in lines) + "\nnot json\n")
        page = os.path.join(t, "index.html")
        open(page, "w").write("<html><head></head><body></body></html>")
        store = ds.Store(os.path.join(t, "oq"))
        store.set("accounts", "a-example-com", {"account": "a@example.com"})
        self.srv = serve.DashboardServer(0, store, page, os.path.join(t, "b"), ledger=ledger)
        self.port = self.srv.server_address[1]
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.addCleanup(self.srv.server_close)
        self.addCleanup(self.srv.shutdown)

    def req(self, path, method="GET"):
        try:
            with urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", method=method), timeout=5) as r:
                return r.status, json.loads(r.read())
        except urllib.error.HTTPError as e:
            return e.code, None

    def test_last_24h_numbers_and_times_only(self):
        st, body = self.req("/usage/a-example-com")
        self.assertEqual(st, 200)
        self.assertEqual(len(body["samples"]), 1)
        self.assertEqual(sorted(body["samples"][0]), ["pct5h", "pctwk", "reset5h", "resetwk", "ts"])
        self.assertNotIn("a@example.com", json.dumps(body))

    def test_unknown_account_404_and_writes_refused(self):
        self.assertEqual(self.req("/usage/nobody")[0], 404)
        self.assertEqual(self.req("/usage/a-example-com/x")[0], 404)
        self.assertEqual(self.req("/usage/a-example-com", "POST")[0], 405)


class TestPageOrder(unittest.TestCase):
    def test_asks_directly_after_decisions_no_briefs_section(self):
        ids = re.findall(r'<details class="sec" id="(sec-[a-z]+)"', open(PAGE, encoding="utf-8").read())
        self.assertEqual(ids[:4], ["sec-decisions", "sec-questions", "sec-asks", "sec-tasks"])


if __name__ == "__main__":
    unittest.main()
