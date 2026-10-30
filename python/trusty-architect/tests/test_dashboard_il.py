"""Consolidated Intent Ledger view (Df7): dashboard_il.aggregate and GET /api/il.

Covers: aggregation over fixture datg stores (the Architect plus two sessions, one legacy and one per-PM directory),
de-duplication of a project reached by both the schedule and the project allowlist, the JSON shape, a missing or corrupt
state.json as a per-session error row (endpoint stays 200), the endpoint refusing non-GET and a bad Host, session mode
not exposing the endpoint, the aggregator never writing, the page carrying the section gated to the architect, and an
unscheduled Architect row named by ARCHITECT_SESSION (default tm-architect).
Run: python3 -m unittest scripts/test_dashboard_il.py
Test: scripts/test_dashboard_il.py
"""
import json
import os
import re
import sys
import tempfile
import threading
import unittest
from unittest import mock
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_il as dil  # noqa: E402
import dashboard_store as ds  # noqa: E402
from test_dashboard_store import ServerCase, serve  # noqa: E402

NOW = 1792570300 + 600
PAGE = serve.PAGE


def state(code, tasks=(), items=(), goals=(), written=1792570300):
    return {"code": code, "v": 7, "config": {}, "goals": list(goals), "items": list(items), "tasks": list(tasks), "next": {},
            "session": {"written_at": "2026-10-21T08:11Z", "written_epoch": written, "task": f"{code} task", "next": f"{code} next"}}


def goal(code, n, status="active"):
    return {"id": f"{code}-G{n:02x}", "rank": n, "title": f"goal {n}", "finish": "f", "status": status, "maps": ""}


def task(code, n, status="open"):
    return {"id": f"{code}-T{n:02x}", "goal": f"{code}-G01", "title": f"task {n}", "status": status, "to": "", "outcome": ""}


def item(code, n, status="open", kind="question"):
    return {"id": f"{code}-D{n:02x}", "kind": kind, "status": status, "goal": f"{code}-G01", "title": f"item {n}",
            "opts": [], "answer": "yes" if status == "answered" else "", "outcome": "", "esc": None}


def put(project, st, sub=None):
    d = os.path.join(project, ".trusty-mpm", "datg", *([sub] if sub else []))
    os.makedirs(d, exist_ok=True)
    p = os.path.join(d, "state.json")
    with open(p, "w") as f:
        f.write(st if isinstance(st, str) else json.dumps(st))
    return p


class Fixture(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        t = self._tmp.name
        self.arch, self.a, self.b, self.c = (os.path.join(t, n) for n in ("il", "alpha", "beta", "gamma"))
        for d in (self.arch, self.a, self.b, self.c):
            os.makedirs(d)
        put(self.arch, state("ar", [task("ar", 1), task("ar", 2, "running"), task("ar", 3, "done")], [item("ar", 1)], [goal("ar", 1)]))
        put(self.a, state("al", [task("al", 1)], [item("al", 1), item("al", 2, "answered"), item("al", 3, "done")], [goal("al", 1), goal("al", 2)]))
        put(self.b, state("be", [], [], [goal("be", 1)]), sub="tm-beta")      # per-PM directory
        self.sched = os.path.join(t, "il-schedule.json")
        self.write_sched([{"name": "tm-architect", "project": self.arch, "code": "ar"},
                          {"name": "tm-alpha", "project": self.a, "code": "al"}])
        self.projects = {"alpha-proj": self.a, "beta-proj": self.b}

    def write_sched(self, sessions):
        with open(self.sched, "w") as f:
            json.dump({"sessions": sessions}, f)

    def agg(self, now=NOW):
        return dil.aggregate(self.projects, self.sched, self.arch, now)


class TestAggregate(Fixture):
    def test_architect_first_then_sessions_by_name_without_duplicates(self):
        names = [s["session"] for s in self.agg()["sessions"]]
        self.assertEqual(names, ["tm-architect", "tm-alpha", "tm-beta"])   # alpha reached by schedule and allowlist: one row

    def test_unscheduled_architect_row_takes_its_name_from_architect_session(self):
        # #10161: the session name is not built in; with no schedule entry for the Architect it comes from the env
        self.write_sched([{"name": "tm-alpha", "project": self.a, "code": "al"}])
        with mock.patch.dict(os.environ, {"ARCHITECT_SESSION": "tm-boss"}):
            names = [s["session"] for s in self.agg()["sessions"]]
        self.assertEqual(names, ["tm-boss", "tm-alpha", "tm-beta"])
        with mock.patch.dict(os.environ, {"ARCHITECT_SESSION": ""}):
            self.assertEqual(self.agg()["sessions"][0]["session"], "tm-architect")   # unset or blank: the default

    def test_row_carries_goals_open_work_and_session_tags(self):
        rows = {s["session"]: s for s in self.agg()["sessions"]}
        a = rows["tm-alpha"]
        self.assertEqual((a["code"], a["architect"], a["error"], a["v"]), ("al", False, None, 7))
        self.assertEqual([g["id"] for g in a["goals"]], ["al-G01", "al-G02"])
        self.assertEqual([i["id"] for i in a["items"]], ["al-D01", "al-D02"])           # done item omitted
        self.assertEqual([i["status"] for i in a["items"]], ["open", "answered"])
        self.assertEqual(a["counts"], {"goals": 2, "items_open": 1, "items_answered": 1, "tasks_open": 1, "tasks_running": 0})
        for x in a["goals"] + a["items"] + a["tasks"]:
            self.assertEqual(x["session"], "tm-alpha")
        r = rows["tm-architect"]
        self.assertTrue(r["architect"])
        self.assertEqual([t["status"] for t in r["tasks"]], ["open", "running"])      # done task omitted

    def test_freshness_and_stale_flag(self):
        s = {x["session"]: x for x in self.agg()["sessions"]}["tm-alpha"]
        self.assertEqual((s["age_s"], s["stale"], s["written_at"]), (600, False, "2026-10-21T08:11Z"))
        old = {x["session"]: x for x in self.agg(now=1792570300 + 3 * 86400)["sessions"]}["tm-alpha"]
        self.assertTrue(old["stale"])

    def test_per_pm_directory_gets_its_directory_name(self):
        b = {x["session"]: x for x in self.agg()["sessions"]}["tm-beta"]
        self.assertEqual((b["code"], b["error"]), ("be", None))

    def test_totals(self):
        self.assertEqual(self.agg()["totals"], {"sessions": 3, "errors": 0, "items_open": 2, "items_answered": 1,
                                                "tasks_open": 2, "tasks_running": 1})

    def test_corrupt_missing_and_misshaped_state_are_error_rows(self):
        put(self.a, "{not json")
        put(self.c, {"goals": "x"})
        self.write_sched([{"name": "tm-alpha", "project": self.a, "code": "al"}, {"name": "tm-gamma", "project": self.c, "code": "ga"},
                          {"name": "tm-ghost", "project": os.path.join(self._tmp.name, "nowhere"), "code": "gh"}])
        agg = self.agg()
        rows = {s["session"]: s for s in agg["sessions"]}
        self.assertIn("unreadable", rows["tm-alpha"]["error"])
        self.assertIn("bad shape", rows["tm-gamma"]["error"])
        self.assertIn("missing", rows["tm-ghost"]["error"])
        self.assertEqual(rows["tm-ghost"]["goals"], [])
        self.assertEqual(rows["tm-beta"]["error"], None)           # healthy neighbours unaffected
        self.assertEqual(agg["totals"]["errors"], 3)

    def test_missing_schedule_file_still_lists_architect_and_roots(self):
        agg = dil.aggregate(self.projects, os.path.join(self._tmp.name, "none.json"), self.arch, NOW)
        self.assertEqual([s["session"] for s in agg["sessions"]], ["tm-architect", "alpha-proj", "tm-beta"])

    def test_aggregate_writes_nothing(self):
        def snap():
            return sorted((os.path.join(d, f), os.path.getmtime(os.path.join(d, f)), os.path.getsize(os.path.join(d, f)))
                          for d, _, fs in os.walk(self._tmp.name) for f in fs)
        before = snap()
        self.agg()
        self.assertEqual(snap(), before)


class TestEndpoint(ServerCase):
    def setUp(self):
        super().setUp()
        self.fx = Fixture("setUp")
        self.fx.setUp()
        self.addCleanup(self.fx._tmp.cleanup)
        self.srv.il_schedule, self.srv.architect_il = self.fx.sched, self.fx.arch
        self.srv.projects = lambda: self.fx.projects

    def test_shape_and_read_only(self):
        status, body = self.call("GET", "/api/il")
        self.assertEqual(status, 200)
        j = json.loads(body)
        self.assertEqual(set(j), {"generated_at", "totals", "sessions"})
        self.assertEqual([s["session"] for s in j["sessions"]], ["tm-architect", "tm-alpha", "tm-beta"])
        self.assertTrue({"session", "code", "architect", "error", "written_at", "age_s", "goals", "items", "tasks", "counts"} <= set(j["sessions"][0]))
        for m in ("POST", "PUT", "DELETE"):
            self.assertEqual(self.call(m, "/api/il", body={})[0], 405)

    def test_corrupt_state_is_a_row_not_a_500(self):
        put(self.fx.a, "\x00garbage")
        status, body = self.call("GET", "/api/il")
        self.assertEqual(status, 200)
        row = {s["session"]: s for s in json.loads(body)["sessions"]}["tm-alpha"]
        self.assertTrue(row["error"])

    def test_host_check_applies(self):
        status, _ = self.call("GET", "/api/il", headers={"Host": "evil.example:7890"})
        self.assertEqual(status, 403)


class TestSessionMode(unittest.TestCase):
    def test_session_mode_does_not_expose_the_endpoint(self):
        with tempfile.TemporaryDirectory() as tmp:
            srv = serve.DashboardServer(0, ds.Store(os.path.join(tmp, "s")), PAGE, config=serve.session_config("t", "b/"),
                                        projects={}, il_schedule=os.path.join(tmp, "x.json"), architect_il=tmp)
            threading.Thread(target=srv.serve_forever, daemon=True).start()
            self.addCleanup(srv.server_close)
            self.addCleanup(srv.shutdown)
            with self.assertRaises(urllib.error.HTTPError) as cm:
                urllib.request.urlopen(f"http://127.0.0.1:{srv.server_address[1]}/api/il", timeout=5)
            self.assertEqual(cm.exception.code, 404)


class TestPage(unittest.TestCase):
    def setUp(self):
        with open(PAGE, encoding="utf-8") as f:
            self.html = f.read()

    def test_section_is_architect_only(self):
        self.assertRegex(self.html, r'<details class="sec" id="sec-il" data-arch-only data-cat="sessions" data-sub="ledgers">')
        self.assertIn('html[data-mode="session"] [data-arch-only] { display: none !important; }', self.html)

    def test_script_skips_session_mode_and_fetches_the_endpoint(self):
        block = self.html[self.html.index("BEGIN ilLedgers"):self.html.index("END ilLedgers")]
        self.assertRegex(block, r'getAttribute\("data-mode"\) === "session"\) return')
        self.assertIn('"/api/il"', block)
        self.assertEqual(re.findall(r'\.open\("(\w+)"', block), ["GET"])           # read-only: no write verbs


if __name__ == "__main__":
    unittest.main()
