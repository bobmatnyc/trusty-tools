"""GET /api/pane on a live dashboard server (#11065 phase 3).

Covers: the {session, lines, captured_at, truncated} shape with the 40-line default; the lines clamp and the 400s for a
non-integer lines or a missing session; a token-shaped string in the pane reaching the client only masked; 404 for a
session that is not running (exact name only) and 503 for a failed capture, neither carrying the tmux detail; the 3 s
cache; the Host guard; and the route being architect-only. The pane comes from a fake poller behind the real
PaneTailProvider, so the masking under test is the shipped one.
Every fake secret is assembled at run time, so no literal in this file matches a secret scanner or the leak gate.
Run: python3 -m pytest tests/test_dashboard_pane_route.py
Test: tests/test_dashboard_pane_route.py
"""
import http.client
import importlib.util
import json
import os
import random
import string
import tempfile
import threading
import unittest

import dashboard_pane_tail as pt

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
_rng = random.Random(110653)


def rand(n):
    return "".join(_rng.choice(string.ascii_letters + string.digits) for _ in range(n))


def load():
    spec = importlib.util.spec_from_file_location("dashboard_serve_pane", os.path.join(SCRIPTS, "dashboard-serve.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class FakeTmuxError(Exception):
    pass


class FakePoller:
    """list_panes/capture_pane/TmuxError, the three names PaneTailProvider reads from fleet-poll.py."""
    TmuxError = FakeTmuxError

    def __init__(self, text="", fail=None):
        self.text, self.fail = text, fail
        self.calls = []

    def list_panes(self):
        return [{"session": "work", "pane": "%1", "cmd": "claude", "active": "11"}]

    def capture_pane(self, p, **kw):
        self.calls.append(kw)
        if self.fail:
            raise FakeTmuxError(self.fail)
        return self.text


class PaneRoute(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.mod = load()

    def serve(self, poller, config=None):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        page = os.path.join(tmp.name, "index.html")
        with open(page, "w") as f:
            f.write("<html><head></head><body>x</body></html>")
        provider = pt.PaneTailProvider(poller, clock=lambda: 1_700_000_000.0)
        srv = self.mod.DashboardServer(0, self.mod.ds.Store(os.path.join(tmp.name, "store")), page, projects={},
                                       static=tmp.name, inbox=os.path.join(tmp.name, "inbox"), config=config,
                                       claude_projects=os.path.join(tmp.name, "claude"),
                                       pane_tail=provider.session_pane_tail)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        self.port = srv.server_address[1]

    def get(self, path, host=None):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        c.request("GET", path, headers={"Host": host or f"127.0.0.1:{self.port}"})
        r = c.getresponse()
        data = r.read()
        c.close()
        return r.status, json.loads(data)

    def test_shape_and_forty_line_default(self):
        poller = FakePoller("\n".join(f"line {i}" for i in range(100)))
        self.serve(poller)
        st, body = self.get("/api/pane?session=work")
        self.assertEqual(st, 200)
        self.assertEqual(set(body), {"session", "lines", "captured_at", "truncated"})
        self.assertEqual(body["lines"], [f"line {i}" for i in range(60, 100)])
        self.assertEqual((body["session"], body["truncated"]), ("work", True))
        self.assertRegex(body["captured_at"], r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
        self.assertEqual(poller.calls[0]["history"], 40)

    def test_lines_is_clamped_and_a_bad_request_is_a_400(self):
        poller = FakePoller("a\nb")
        self.serve(poller)
        self.assertEqual(self.get("/api/pane?session=work&lines=5")[0], 200)
        self.assertEqual(self.get("/api/pane?session=work&lines=500")[0], 200)
        self.assertEqual([c["history"] for c in poller.calls], [10, 200])
        for q in ("session=work&lines=abc", "session=work&lines=-3", "session=work&lines=%C2%B2", "", "session=",
                  "session=" + "w" * 201):
            st, body = self.get("/api/pane?" + q)
            self.assertEqual((st, body["code"]), (400, "bad_request"), q)

    def test_token_in_the_pane_reaches_the_client_only_masked(self):
        tok, bearer = "ghp_" + rand(36), rand(40)
        self.serve(FakePoller(f"git push with {tok}\n\ncurl -H 'Authorization: Bearer {bearer}'\n\ndone"))
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        c.request("GET", "/api/pane?session=work", headers={"Host": f"127.0.0.1:{self.port}"})
        raw = c.getresponse().read().decode("utf-8")
        c.close()
        self.assertNotIn(tok, raw)
        self.assertNotIn(tok[4:20], raw)   # no fragment of the token body either
        self.assertNotIn(bearer, raw)
        self.assertIn("[masked:gh-token]", raw)
        self.assertIn("[masked:auth]", raw)

    def test_not_running_is_404_and_the_match_is_exact(self):
        self.serve(FakePoller("x"))
        for name in ("wor", "work2", "Work"):
            st, body = self.get(f"/api/pane?session={name}")
            self.assertEqual((st, body["code"]), (404, "not_running"), name)

    def test_failed_capture_is_503_without_the_tmux_detail(self):
        detail = "server-detail-" + rand(8)
        self.serve(FakePoller(fail=detail))
        st, body = self.get("/api/pane?session=work")
        self.assertEqual((st, body["code"]), (503, "capture_failed"))
        self.assertNotIn(detail, json.dumps(body))

    def test_repeat_within_the_ttl_is_served_from_memory(self):
        poller = FakePoller("x")
        self.serve(poller)
        self.assertEqual(self.get("/api/pane?session=work")[0], 200)
        self.assertEqual(self.get("/api/pane?session=work")[0], 200)
        self.assertEqual(len(poller.calls), 1)

    def test_foreign_host_is_refused_before_any_capture(self):
        poller = FakePoller("x")
        self.serve(poller)
        st, body = self.get("/api/pane?session=work", host="evil.example.com")
        self.assertEqual((st, body["code"]), (403, "permission_denied"))
        self.assertEqual(poller.calls, [])

    def test_session_dashboard_does_not_serve_the_pane(self):
        poller = FakePoller("x")
        self.serve(poller, config={"mode": "session", "title": "t"})
        self.assertEqual(self.get("/api/pane?session=work")[0], 404)
        self.assertEqual(poller.calls, [])


if __name__ == "__main__":
    unittest.main()
