"""Tests for scripts/drift_test_mem_search.py: live-palace isolation and complete seeding (#11815).

Why: a read-only harness run served live `cto` and `supervisor` drawers through the
daemon's user-scope rulings leg, which raised their access_count, and scored a run
in which 3 drawers had not seeded. What: a fake daemon models the rulings leg (an
unscoped recall serves and access-bumps every drawer of each rulings palace) and
seed failures; the harness runs against it, never a live daemon.
Run: python3 -m unittest discover -s scripts/tests -p 'test_drift_mem_search_isolation.py'
"""
import contextlib
import importlib.util
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
_spec = importlib.util.spec_from_file_location("drift_isolation_under_test", SCRIPTS / "drift_test_mem_search.py")
M = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(M)

TEST_PREFIX = "drift-dream-test-"


class FakeDaemon:
    """In-memory trusty-memory: palaces of drawers with access counts, plus a rulings leg."""

    def __init__(self, palaces, rulings=(), fail_texts=(), ignore_scope=False):
        self.palaces = {p: {d: {"content": c, "access_count": 0} for d, c in ds.items()} for p, ds in palaces.items()}
        self.rulings, self.fail_texts, self.ignore_scope = list(rulings), set(fail_texts), ignore_scope
        self.served, self.clients, self.n = [], [], 0

    def snapshot(self, names):
        return {p: {d: v["access_count"] for d, v in self.palaces[p].items()} for p in names}

    def _serve(self, pal, did):
        self.palaces[pal][did]["access_count"] += 1
        self.served.append((pal, did))
        return {"drawer_id": did}

    def tool(self, name, a):
        pal = a.get("palace") or a.get("palace_id") or a.get("name")
        if name == "palace_list":
            return {"palaces": sorted(self.palaces)}
        if name == "palace_create":
            self.palaces[pal] = {}
        elif name == "room_list":
            return {"rooms": []}
        elif name == "memory_list":
            return {"drawers": [{"drawer_id": d, "content": v["content"]} for d, v in self.palaces[pal].items()]}
        elif name == "memory_remember":
            if a["text"] in self.fail_texts:
                raise ValueError("content rejected")
            self.n += 1
            self.palaces[pal][f"t{self.n}"] = {"content": a["text"], "access_count": 0}
            return {"drawer_id": f"t{self.n}"}
        elif name == "memory_recall":
            out = [self._serve(pal, d) for d in list(self.palaces[pal])[: a.get("top_k", 10)]]
            # The daemon's rulings leg runs only for an unscoped (RecallScope::All) recall.
            if self.ignore_scope or not (a.get("wing") or a.get("room")):
                out += [self._serve(r, d) for r in self.rulings if r != pal for d in self.palaces[r]]
            return {"results": out}
        elif name == "palace_delete":
            self.palaces.pop(pal, None)
        return {}

    def client_class(self):
        daemon = self

        class Client:
            def __init__(self, cmd):
                self.server, self.closed = {"version": "fake"}, False
                daemon.clients.append(cmd[0])

            def call(self, name, args, timeout=None):
                try:
                    return daemon.tool(name, args), 1.0, None
                except ValueError as e:
                    return None, 1.0, str(e)

            def close(self):
                self.closed = True

        return Client


@contextlib.contextmanager
def patched(daemon):
    old, M.Mcp = M.Mcp, daemon.client_class()
    try:
        with contextlib.redirect_stderr(io.StringIO()) as err:
            yield err
    finally:
        M.Mcp = old


def live_daemon(**kw):
    cto = {f"cto-{i:04d}": f"cto drawer {i}" for i in range(4)}
    sup = {f"sup-{i:04d}": f"standing ruling {i}" for i in range(3)}
    return FakeDaemon({"cto": cto, "supervisor": sup}, rulings=["supervisor", "cto"], **kw)


GOLDEN = {"search": [], "memory": [{"id": "q1", "palace": "cto", "query": "q", "expected": ["cto-0001"]}]}


def new_info():
    return {"versions": {}, "indexes": {}, "palaces": {}, "warmup_ms": {}}


class LivePalaceIsolation(unittest.TestCase):
    def test_recall_leaves_live_palaces_untouched(self):
        d = live_daemon()
        before = d.snapshot(["cto", "supervisor"])
        rows = []
        with patched(d):
            M.run_memory(GOLDEN, rows, new_info(), "/nonexistent")
        self.assertEqual(d.snapshot(["cto", "supervisor"]), before, "a recall raised access_count on a live drawer")
        foreign = [s for s in d.served if not s[0].startswith(TEST_PREFIX)]
        self.assertEqual(foreign, [], "a recall served a drawer from a live palace")
        self.assertEqual(len(rows), 1)

    def test_foreign_hit_aborts_the_lane(self):
        d, rows, raised = live_daemon(ignore_scope=True), [], None
        with patched(d):
            try:
                M.run_memory(GOLDEN, rows, new_info(), "/nonexistent")
            except Exception as e:  # noqa: BLE001 - the abort type is the harness's own
                raised = e
        self.assertIsNotNone(raised, "a recall that served a live drawer did not abort the lane")
        self.assertIn("another palace", str(raised))
        self.assertEqual(rows, [], "a run with a foreign hit was scored")
        self.assertFalse([p for p in d.palaces if p.startswith(TEST_PREFIX)], "test palace not deleted")

    def test_unscoped_recall_is_refused_by_the_client(self):
        g = M.PalaceGuard()
        pal = g.new_name()
        g.claim(pal, set())
        client = type("C", (), {"allowed": M.DREAM_TOOLS, "guard": g})()
        with self.assertRaises(PermissionError):
            M.Mcp.call(client, "memory_recall", {"palace": pal, "query": "q", "top_k": 1})


class Run(unittest.TestCase):
    def run_main(self, daemon, *extra):
        with tempfile.TemporaryDirectory() as tmp:
            golden = os.path.join(tmp, "golden.json")
            Path(golden).write_text(json.dumps(GOLDEN))
            results = os.path.join(tmp, "runs.jsonl")
            argv = ["drift_test_mem_search.py", "--golden-public", golden, "--golden-private",
                    os.path.join(tmp, "absent.json"), "--results", results, "--baseline",
                    os.path.join(tmp, "baseline.json"), *extra]
            old_argv, sys.argv = sys.argv, argv
            out = io.StringIO()
            try:
                with patched(daemon) as err, contextlib.redirect_stdout(out):
                    try:
                        code = M.main()
                    except SystemExit as e:
                        code = e.code
            finally:
                sys.argv = old_argv
            rows = Path(results).read_text().splitlines() if os.path.exists(results) else []
            return code, out.getvalue() + err.getvalue(), rows

    def test_partial_seed_aborts_and_names_drawers(self):
        d = live_daemon(fail_texts={"cto drawer 2", "cto drawer 3"})
        code, text, rows = self.run_main(d)
        self.assertNotEqual(code, 0, "a partial seed exited 0")
        for did in ("cto-0002", "cto-0003"):
            self.assertIn(did, text, f"unseeded drawer {did} not named")
        self.assertEqual(rows, [], "a partial seed produced a recorded, scored run")
        self.assertFalse([p for p in d.palaces if p.startswith(TEST_PREFIX)], "test palace not deleted")

    def test_lane_selector_runs_one_lane(self):
        d = live_daemon()
        code, text, rows = self.run_main(d, "--lanes", "search")
        self.assertEqual(d.clients, ["trusty-search"], "--lanes search started another lane's client")
        self.assertEqual(json.loads(rows[0])["lanes"], ["search"])
        self.assertNotIn("lane memory", text)
        d2 = live_daemon()
        self.run_main(d2)
        self.assertEqual(d2.clients, ["trusty-search", "trusty-memory"], "default no longer runs both lanes")

    def test_lane_selector_rejects_unknown_lane(self):
        code, text, _ = self.run_main(live_daemon(), "--lanes", "search,vectors")
        self.assertEqual(code, 2)
        self.assertIn("--lanes", text)


if __name__ == "__main__":
    unittest.main()
