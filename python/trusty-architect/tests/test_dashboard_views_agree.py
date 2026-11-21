"""#10679: one batch that changes a task, its session and the header is seen whole or not at all.

A reader that lands while a batch is between its files used to get the new task with the old session and header.
Each test pauses a real batch after its first file and reads from a second Store (another process in production).
Run: python3 -I -m pytest tests/test_dashboard_views_agree.py
"""
import contextlib
import fcntl
import http.client
import importlib.util
import json
import os
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
if SCRIPTS not in sys.path:
    sys.path.insert(0, SCRIPTS)
import dashboard_derive as derive  # noqa: E402
import dashboard_store as ds  # noqa: E402

VIEWS = ("items", "meta", "sessions")
OLD = {"items": ("D01", {"title": "Ship it?", "status": "open", "session": "tm-a"}),
       "sessions": ("tm-a", {"name": "tm-a", "state": "needs owner"}),
       "meta": ("state", {"fleet_line": "1 decision waiting"})}
NEW = {"items": {"status": "answered", "answer": "Yes"},
       "sessions": {"state": "working"},
       "meta": {"fleet_line": "0 decisions waiting"}}
WAIT = 0.3   # how long the paused batch gives a reader to slip in


def _view(snap):
    """(task status, session state, header line) from a {coll: docs} snapshot."""
    one = {c: {d["id"]: d["data"] for d in snap[c]} for c in VIEWS}
    return (one["items"]["D01"]["status"], one["sessions"]["tm-a"]["state"], one["meta"]["state"]["fleet_line"])


OLD_VIEW = ("open", "needs owner", "1 decision waiting")
NEW_VIEW = ("answered", "working", "0 decisions waiting")


class PausedBatch:
    """Runs one batch in a thread that stops after its first file write until release()."""

    def __init__(self, store):
        self.store, self.paused, self.go, self.error = store, threading.Event(), threading.Event(), None
        real, seen = ds._atomic_write, []

        def write(path, payload):
            real(path, payload)
            seen.append(path)
            if len(seen) == 1:
                self.paused.set()
                self.go.wait(10)

        self._patch = mock.patch.object(ds, "_atomic_write", write)
        ops = [{"op": "update", "collection": c, "id": OLD[c][0], "data": NEW[c]} for c in VIEWS]
        self.thread = threading.Thread(target=self._run, args=(ops,), daemon=True)

    def _run(self, ops):
        try:
            self.store.batch(ops, actor="test")
        except Exception as e:  # noqa: BLE001 - surfaced by the test
            self.error = e

    def __enter__(self):
        self._patch.start()
        self.thread.start()
        assert self.paused.wait(10), "the batch never wrote its first file"
        return self

    def __exit__(self, *exc):
        self.go.set()
        self.thread.join(10)
        self._patch.stop()
        return False


class ViewsAgree(unittest.TestCase):
    def seed(self, fmt):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = os.path.join(tmp.name, "oq")
        store = ds.Store(root, fmt=fmt, schema_mode="off")
        store.batch([{"op": "set", "collection": c, "id": OLD[c][0], "data": OLD[c][1]} for c in VIEWS], actor="test")
        return root, store

    def test_a_read_during_a_batch_sees_task_session_and_header_together(self):
        for fmt in ds.FORMATS:
            with self.subTest(fmt=fmt):
                root, writer = self.seed(fmt)
                reader, got = ds.Store(root, fmt=fmt, schema_mode="off"), []
                with PausedBatch(writer) as pb:
                    t = threading.Thread(target=lambda: got.append(_view(derive.snapshot(reader, VIEWS))), daemon=True)
                    t.start()
                    t.join(WAIT)   # a torn read returns here, mid-batch
                t.join(10)
                self.assertIsNone(pb.error)
                self.assertEqual(len(got), 1)
                self.assertIn(got[0], (OLD_VIEW, NEW_VIEW), f"{fmt}: task, session and header disagree: {got[0]}")
                self.assertEqual(_view(derive.snapshot(reader, VIEWS)), NEW_VIEW)

    def test_http_reads_during_a_batch_never_serve_a_stale_header_or_session(self):
        serve = _load_serve()
        for fmt in ds.FORMATS:
            with self.subTest(fmt=fmt):
                root, writer = self.seed(fmt)
                page = os.path.join(os.path.dirname(root), "index.html")
                with open(page, "w") as f:
                    f.write("<html><head></head><body>x</body></html>")
                srv = serve.DashboardServer(0, ds.Store(root, fmt=fmt, schema_mode="off"), page)
                threading.Thread(target=srv.serve_forever, daemon=True).start()
                self.addCleanup(srv.server_close)
                self.addCleanup(srv.shutdown)
                got = []

                def read_views(port=srv.server_address[1]):
                    snap = {}
                    for coll in VIEWS:
                        c = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
                        c.request("GET", "/api/" + coll)
                        snap[coll] = json.loads(c.getresponse().read())["docs"]
                        c.close()
                    got.append(_view(snap))

                with PausedBatch(writer) as pb:
                    t = threading.Thread(target=read_views, daemon=True)
                    t.start()
                    t.join(WAIT)
                t.join(10)
                self.assertIsNone(pb.error)
                self.assertEqual(got, [NEW_VIEW], f"{fmt}: /api served a mix mid-batch: {got}")

    def test_api_harness_reads_sessions_items_goals_and_settings_from_one_batch(self):
        # #10679: /api/harness read four collections under four locks; a batch committing between them tore the page.
        serve = _load_serve()
        old = {"sessions": ("tm-a", {"name": "tm-a", "state": "needs owner"}), "items": ("D01", {"title": "t", "status": "open"}),
               "goals": ("G1", {"title": "g", "status": "open"}), "meta": ("settings", {"user_state": "awake"})}
        new = {"sessions": {"state": "working"}, "items": {"status": "answered"}, "goals": {"status": "done"}, "meta": {"user_state": "asleep"}}
        for fmt in ds.FORMATS:
            with self.subTest(fmt=fmt):
                tmp = tempfile.TemporaryDirectory()
                self.addCleanup(tmp.cleanup)
                root = os.path.join(tmp.name, "oq")
                writer = ds.Store(root, fmt=fmt, schema_mode="off")
                writer.batch([{"op": "set", "collection": c, "id": i, "data": d} for c, (i, d) in old.items()], actor="test")
                reader, fired = ds.Store(root, fmt=fmt, schema_mode="off"), []
                real_lock = reader._read_lock

                @contextlib.contextmanager
                def lock_then_commit():
                    with real_lock():
                        yield
                    if not fired:   # the moment the reader lets go, a whole batch commits
                        fired.append(1)
                        writer.batch([{"op": "update", "collection": c, "id": old[c][0], "data": new[c]} for c in new], actor="test")

                reader._read_lock = lock_then_commit
                page = os.path.join(tmp.name, "index.html")
                with open(page, "w") as f:
                    f.write("<html><head></head><body>x</body></html>")
                srv = serve.DashboardServer(0, reader, page, projects={}, live_page=page, inbox=tmp.name, claude_projects=tmp.name)
                seen = []

                def build(_inbox, _proj, sdocs, idocs, goal_docs=None, user_state=None, **_kw):
                    seen.append((sdocs[0]["state"], idocs[0]["status"], goal_docs[0]["status"], user_state))
                    return {"ok": True}

                with mock.patch.object(serve.dashboard_harness, "build", build), \
                        mock.patch.object(serve.dashboard_harness, "tmux_sessions", lambda: []):
                    threading.Thread(target=srv.serve_forever, daemon=True).start()
                    self.addCleanup(srv.server_close)
                    self.addCleanup(srv.shutdown)
                    c = http.client.HTTPConnection("127.0.0.1", srv.server_address[1], timeout=10)
                    c.request("GET", "/api/harness")
                    self.assertEqual(c.getresponse().status, 200)
                    c.close()
                self.assertEqual(fired, [1])
                self.assertIn(seen, ([("needs owner", "open", "open", "awake")], [("working", "answered", "done", "asleep")]),
                              f"{fmt}: /api/harness mixed two batches: {seen}")

    def test_a_read_behind_a_held_writer_lock_is_503_within_the_wait(self):
        # #10679: a shared-lock wait with no bound hung every dashboard GET behind a hung writer.
        serve = _load_serve()
        root, _ = self.seed("dir")
        store = ds.Store(root, fmt="dir", schema_mode="off")
        store.lock_wait = 0.2
        page = os.path.join(os.path.dirname(root), "index.html")
        with open(page, "w") as f:
            f.write("<html><head></head><body>x</body></html>")
        srv = serve.DashboardServer(0, store, page)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        held = open(os.path.join(root, ".lock"), "a")
        fcntl.flock(held, fcntl.LOCK_EX)   # a writer that never lets go
        self.addCleanup(held.close)
        self.addCleanup(fcntl.flock, held, fcntl.LOCK_UN)
        start = time.monotonic()
        c = http.client.HTTPConnection("127.0.0.1", srv.server_address[1], timeout=5)   # an unbounded wait times out here
        c.request("GET", "/api/items")
        r = c.getresponse()
        body = json.loads(r.read())
        c.close()
        self.assertEqual((r.status, body.get("code")), (503, "unavailable"), body)
        self.assertLess(time.monotonic() - start, 3.0)
        with self.assertRaises(ds.Unavailable):   # the read fails; it never reads unlocked
            store.list_docs("items")
        self.assertEqual(ds.LOCK_WAIT, 10.0)   # the Rust store's LOCK_WAIT

    def serve_behind_held_lock(self, lock_wait):
        """A DashboardServer over a seeded dir store whose .lock a writer holds; returns its port."""
        serve = _load_serve()
        root, _ = self.seed("dir")
        store = ds.Store(root, fmt="dir", schema_mode="off")
        store.lock_wait = lock_wait
        page = os.path.join(os.path.dirname(root), "index.html")
        with open(page, "w") as f:
            f.write("<html><head></head><body>x</body></html>")
        srv = serve.DashboardServer(0, store, page)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        held = open(os.path.join(root, ".lock"), "a")
        fcntl.flock(held, fcntl.LOCK_EX)
        self.addCleanup(held.close)
        self.addCleanup(fcntl.flock, held, fcntl.LOCK_UN)
        return srv.server_address[1]

    def test_api_viz_behind_a_held_writer_lock_is_503_not_empty_data(self):
        # #10699: _bodies swallowed Unavailable, so /api/viz answered 200 with no sessions and no goals.
        port = self.serve_behind_held_lock(0.2)
        status, body = _get(port, "/api/viz")
        self.assertEqual((status, body.get("code")), (503, "unavailable"), body)

    def test_concurrent_fleet_gets_behind_a_held_writer_lock_share_one_wait(self):
        # #10698: the fleet cache re-ran a failed build per queued request, so the Nth request waited N lock waits.
        port = self.serve_behind_held_lock(0.5)
        out = []
        threads = [threading.Thread(target=lambda: out.append(_get(port, "/api/fleet"))) for _ in range(4)]
        start = time.monotonic()
        for t in threads:
            t.start()
        for t in threads:
            t.join(10)
        elapsed = time.monotonic() - start
        self.assertEqual(sorted(s for s, _ in out), [503] * 4, out)
        self.assertLess(elapsed, 0.9, f"4 requests took {elapsed:.2f}s: each re-ran the failed build")

    def test_id_resolution_behind_a_held_writer_lock_stays_unavailable(self):
        # #10679: store_sid / alias_rules caught StoreError, so a hung writer read as "no session, no aliases".
        import dashboard_qual as qual
        root, store = self.seed("dir")
        store.set("meta", "id-aliases", {"rules": [{"from": "A", "to": "B"}]}, actor="test")
        self.assertEqual(qual.alias_rules(store), [("A", "B")])
        self.assertIsNone(qual.store_sid(store))   # meta/state has no session_id: unknown, as before
        store.lock_wait = 0.2
        held = open(os.path.join(root, ".lock"), "a")
        fcntl.flock(held, fcntl.LOCK_EX)
        self.addCleanup(held.close)
        self.addCleanup(fcntl.flock, held, fcntl.LOCK_UN)
        with self.assertRaises(ds.Unavailable):
            qual.store_sid(store)
        with self.assertRaises(ds.Unavailable):
            qual.alias_rules(store)
        fcntl.flock(held, fcntl.LOCK_UN)
        store.delete("meta", "id-aliases", actor="test")
        self.assertEqual(qual.alias_rules(store), [])   # an absent doc still reads as no rules


def _get(port, path):
    """(status, JSON body) of one GET."""
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    try:
        c.request("GET", path)
        r = c.getresponse()
        return r.status, json.loads(r.read())
    finally:
        c.close()


def _load_serve():
    spec = importlib.util.spec_from_file_location("dashboard_serve_views", os.path.join(SCRIPTS, "dashboard-serve.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


if __name__ == "__main__":
    unittest.main()
