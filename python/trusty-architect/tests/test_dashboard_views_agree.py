"""#10679: one batch that changes a task, its session and the header is seen whole or not at all.

A reader that lands while a batch is between its files used to get the new task with the old session and header.
Each test pauses a real batch after its first file and reads from a second Store (another process in production).
Run: python3 -I -m pytest tests/test_dashboard_views_agree.py
"""
import http.client
import importlib.util
import json
import os
import sys
import tempfile
import threading
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


def _load_serve():
    spec = importlib.util.spec_from_file_location("dashboard_serve_views", os.path.join(SCRIPTS, "dashboard-serve.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


if __name__ == "__main__":
    unittest.main()
