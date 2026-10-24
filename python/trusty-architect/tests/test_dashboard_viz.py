"""Visualize view: dashboard_viz.build and GET /api/viz, plus the page wiring.

Covers: directory -> project -> session -> goal attribution (name match, project-text match, goal-doc fallback, label-only
goal, worktree directories folding into their repo, unmapped directories), token sums per (hour, goal) inside the window,
the Architect's repo naming ARCHITECT_SESSION, closed IL tasks from archive.jsonl, the endpoint (shape, GET only, Host check, bad days, session mode 404), and the nav link.
Run: python3 -m unittest scripts/test_dashboard_viz.py
Test: scripts/test_dashboard_viz.py
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
import dashboard_store as ds  # noqa: E402
import dashboard_viz as dv  # noqa: E402
from test_dashboard_store import serve  # noqa: E402

NOW = 1792576800   # 2026-10-21T10:00:00Z
PAGE = serve.PAGE


class Fx(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        t = self._tmp.name
        self.store = ds.Store(os.path.join(t, "q"))
        self.alpha, self.beta, self.il = (os.path.join(t, n) for n in ("alpha", "beta", "il"))
        for d in (self.alpha, self.beta, self.il):
            os.makedirs(d)
        S = self.store.set
        S("sessions", "S01", {"name": "tm-alpha", "goal": "G01 · alpha goal", "project": "x"})
        S("sessions", "S02", {"name": "tm-beta-01", "goal": None, "project": "y"})
        S("sessions", "S03", {"name": "tm-architect", "goal": "Standing · supervision", "project": "z"})
        S("sessions", "S04", {"name": "tm-w", "goal": "G02 · other", "project": "repo-w (extra)"})
        S("goals", "G07", {"title": "beta goal", "rank": 2, "sessions": ["tm-beta-01"], "lifecycle": "active"})
        self.projects = {"alpha": self.alpha, "beta": self.beta, "repo-w": os.path.join(t, "w"), "architect": self.il}
        os.makedirs(self.projects["repo-w"])
        self.tok = os.path.join(t, "tok.jsonl")
        rows = [("2026-10-21T09", dv.enc(self.alpha), 100), ("2026-10-21T09", dv.enc(self.alpha) + "--claude-worktrees-agent-a1", 50),
                ("2026-10-21T09", dv.enc(self.beta), 7), ("2026-10-21T08", "-private-var-T", 3), ("2026-10-21T08", dv.enc(self.projects["repo-w"]), 9),
                ("2026-10-01T00", dv.enc(self.alpha), 999_999), ("2026-10-21T08", dv.enc(self.il), 11)]
        with open(self.tok, "w") as f:
            for h, p, n in rows:
                f.write(json.dumps({"hour": h, "project": p, "out_tokens": n, "turns": 2, "ctx_p50": n * 10}) + "\n")
            f.write("not json\n")
        d = os.path.join(self.il, ".trusty-mpm", "datg")
        os.makedirs(d)
        with open(os.path.join(d, "state.json"), "w") as f:
            json.dump({"goals": [], "items": [], "tasks": []}, f)
        with open(os.path.join(d, "archive.jsonl"), "w") as f:
            f.write(json.dumps({"closed_at": "2026-10-21T09:30Z", "kind": "task", "item": {"id": "ar-T01", "title": "new"}}) + "\n")
            f.write(json.dumps({"closed_at": "2026-10-01T09:30Z", "kind": "task", "item": {"id": "ar-T00", "title": "old"}}) + "\n")
            f.write(json.dumps({"closed_at": "2026-10-21T09:31Z", "kind": "question", "item": {"id": "ar-D01"}}) + "\n")
        self.sched = os.path.join(t, "sched.json")
        with open(self.sched, "w") as f:
            json.dump({"sessions": [{"name": "tm-architect", "project": self.il, "code": "ar"}]}, f)

    def build(self, days=7):
        return dv.build(self.store, self.tok, self.projects, self.sched, self.il, NOW, days)


class TestBuild(Fx):
    def test_tokens_sum_per_hour_and_goal_with_worktrees_folded(self):
        got = {(r["hour"], r["goal"]): r["out"] for r in self.build()["tokens"]}
        self.assertEqual(got[("2026-10-21T09", "G01")], 150)             # repo dir + its worktree dir
        self.assertEqual(got[("2026-10-21T09", "G07")], 7)               # goal found via the goals doc's sessions list
        self.assertEqual(got[("2026-10-21T08", "")], 3)                  # unmapped directory stays unattributed
        self.assertEqual(got[("2026-10-21T08", "G02")], 9)               # session found by project text
        self.assertEqual(got[("2026-10-21T08", "Standing · supervision")], 11)
        self.assertFalse(any(h == "2026-10-01T00" for h, _ in got))      # outside the window

    def test_projects_table_states_the_basis(self):
        p = {x["name"]: x for x in self.build()["projects"]}
        self.assertEqual((p["alpha"]["goal"], p["alpha"]["basis"], p["alpha"]["out"]), ("G01", "session name", 150))
        self.assertEqual(p["repo-w"]["basis"], "session project text")
        self.assertEqual(p[""]["basis"], "directory not in the project allowlist")

    def test_ctx_hours_estimate_turns_times_p50_per_hour_and_directory(self):
        ch = {(r["hour"], r["dir"]): r for r in self.build()["ctx_hours"]}
        a = ch[("2026-10-21T09", dv.enc(self.alpha))]
        self.assertEqual((a["turns"], a["ctx"]), (4, 2 * 1000 + 2 * 500))     # repo dir (2 x 100*10) + worktree dir (2 x 50*10), folded
        self.assertEqual(ch[("2026-10-21T08", "-private-var-T")]["ctx"], 60)   # unmapped directory still reported
        self.assertFalse(any(h == "2026-10-01T00" for h, _ in ch))             # outside the window
        self.assertEqual((a["out"], ch[("2026-10-21T08", "-private-var-T")]["out"]), (150, 3))   # treemap size: exact out_tokens, worktree folded

    def test_rows_without_turns_or_ctx_add_no_ctx(self):
        with open(self.tok, "w") as f:
            f.write(json.dumps({"hour": "2026-10-21T09", "project": dv.enc(self.alpha), "out_tokens": 5}) + "\n")
        r = self.build()
        self.assertEqual([(x["ctx"], x["out"]) for x in r["ctx_hours"]], [(0, 5)])   # never invent a ctx number; out stays exact
        self.assertEqual(r["tokens"][0]["out"], 5)

    def test_closed_il_tasks_window_kind_and_inferred_goal(self):
        c = self.build()["il_closed"]
        self.assertEqual([(x["id"], x["session"], x["goal"]) for x in c], [("ar-T01", "tm-architect", "Standing · supervision")])

    def test_architect_repo_without_votes_names_the_architect_session(self):
        # #10161: the session name is not built in; ARCHITECT_SESSION names it, default tm-architect
        with mock.patch.dict(os.environ, {"ARCHITECT_SESSION": "tm-boss"}):
            a = dv.attribute({"architect": self.il}, [], [])[dv.enc(self.il)]
        self.assertEqual((a["sessions"], a["basis"]), (["tm-boss"], "this repo is the Architect's"))
        with mock.patch.dict(os.environ, {"ARCHITECT_SESSION": ""}):
            self.assertEqual(dv.attribute({"architect": self.il}, [], [])[dv.enc(self.il)]["sessions"], ["tm-architect"])

    def test_missing_files_give_empty_not_error(self):
        os.remove(self.tok)
        r = self.build()
        self.assertEqual(r["tokens"], [])


class TestEndpoint(Fx):
    def setUp(self):
        super().setUp()
        self.srv = serve.DashboardServer(0, self.store, PAGE, projects=self.projects, il_schedule=self.sched, architect_il=self.il, tokens=self.tok)
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.addCleanup(self.srv.server_close)
        self.addCleanup(self.srv.shutdown)

    def call(self, path, method="GET", headers=None):
        req = urllib.request.Request(f"http://127.0.0.1:{self.srv.server_address[1]}{path}", method=method, headers=headers or {})
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.read()

    def test_shape_and_read_only(self):
        s, b = self.call("/api/viz?days=14")
        self.assertEqual(s, 200)
        self.assertEqual(set(json.loads(b)), {"generated_at", "since", "tokens", "projects", "ctx_hours", "il_closed"})
        for m in ("POST", "PUT", "DELETE"):
            self.assertEqual(self.call("/api/viz", m)[0], 405)

    def test_bad_days_and_bad_host(self):
        self.assertEqual(self.call("/api/viz?days=x")[0], 400)
        self.assertEqual(self.call("/api/viz", headers={"Host": "evil.example:7890"})[0], 403)

    def test_session_mode_does_not_expose_it(self):
        srv = serve.DashboardServer(0, self.store, PAGE, config=serve.session_config("t", "b/"), projects={}, il_schedule=self.sched, architect_il=self.il)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        with self.assertRaises(urllib.error.HTTPError) as cm:
            urllib.request.urlopen(f"http://127.0.0.1:{srv.server_address[1]}/api/viz", timeout=5)
        self.assertEqual(cm.exception.code, 404)


class TestPage(unittest.TestCase):
    def setUp(self):
        with open(PAGE, encoding="utf-8") as f:
            self.html = f.read()

    def test_single_nav_entry_with_graph_and_tree_tabs(self):
        self.assertRegex(self.html, r'\{ key: "visualize", label: "Visualize"[^\n]*subs: \[\["live", "Live"[^\n]*\["graph", "Graph"[^\n]*\["treemap", "Treemap"')
        self.assertIn('id="sec-visualize"', self.html)

    def test_viz_block_is_read_only_and_local(self):
        block = self.html[self.html.index("BEGIN viz"):self.html.index("END viz")]
        self.assertIn('"/api/viz', block)
        self.assertEqual(re.findall(r'\.open\("(\w+)"', block), [])
        self.assertEqual(re.findall(r"https?://[^\s\"']+", block), ["http://www.w3.org/2000/svg"])   # the SVG namespace only: no CDN, no outside host

    def test_treemap_sized_by_out_tokens_with_three_toggles_and_no_tree(self):
        block = self.html[self.html.index("BEGIN viz"):self.html.index("END viz")]
        for need in ("ctx_hours", "squarify", "r.out", "no token data", "tokensInWindow", "viz-tm-dim", "localStorage", "Task", "Goal", "Session"):
            self.assertIn(need, block)
        self.assertNotRegex(block, r"drawTree\b")                                   # the node-link tree is gone
        self.assertNotRegex(self.html, r"viz-tree\b")
        self.assertIn("winOf()", block)                                       # same window as the Graph
        self.assertIn('data-sub="treemap"', self.html)
        self.assertNotIn('data-sub="tree"', self.html)


if __name__ == "__main__":
    unittest.main()
