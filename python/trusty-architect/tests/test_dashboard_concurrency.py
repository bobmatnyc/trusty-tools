"""The dashboard server under concurrent and stalled clients (#10161, PR #10227 review).

Covers: a malformed or negative Content-Length is a 400 and never reaches rfile.read; each connection has a socket
timeout; concurrent Live builds never scan transcripts at the same time; concurrent HarnessCache.get calls share one
build. The race tests force the interleaving with a two-party barrier: without the lock both threads meet at it,
with the lock the first one's wait times out alone. No timing gamble decides the outcome.
Test: tests/test_dashboard_concurrency.py
"""
import os
import socket
import sys
import tempfile
import threading
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "scripts"))
import dashboard_harness as dh  # noqa: E402
import dashboard_http  # noqa: E402
from test_dashboard_store import ServerCase  # noqa: E402

BARRIER_WAIT_S = 2


class Overlap:
    """Wraps a callable; records the most calls inside it at once. Two concurrent callers meet at a barrier."""

    def __init__(self, fn):
        self.fn, self.active, self.peak, self.calls = fn, 0, 0, 0
        self.lock = threading.Lock()
        self.barrier = threading.Barrier(2)

    def __call__(self, *a, **k):
        with self.lock:
            self.active += 1
            self.calls += 1
            self.peak = max(self.peak, self.active)
        try:
            self.barrier.wait(BARRIER_WAIT_S)
        except threading.BrokenBarrierError:
            pass   # alone inside: the other caller is held outside
        try:
            return self.fn(*a, **k)
        finally:
            with self.lock:
                self.active -= 1


def run_two(target):
    errors = []

    def wrap():
        try:
            target()
        except Exception as e:  # noqa: BLE001 - surfaced by the assertion below
            errors.append(e)
    ts = [threading.Thread(target=wrap) for _ in range(2)]
    for t in ts:
        t.start()
    for t in ts:
        t.join(30)
    return errors


class TestHarnessRace(unittest.TestCase):
    def test_concurrent_builds_never_scan_transcripts_at_once(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = os.path.join(tmp, "-work-alpha")
            os.makedirs(d)
            with open(os.path.join(d, "s.jsonl"), "w") as f:
                f.write('{"type": "user"}\n')
            spy = Overlap(dh._scan_docs)
            orig, dh._scan_docs = dh._scan_docs, spy
            try:
                errors = run_two(lambda: dh.collect_docs(tmp, {"-work-alpha": "tm-alpha"}, None, time.time()))
            finally:
                dh._scan_docs = orig
        self.assertEqual(errors, [])
        self.assertEqual(spy.calls, 2)
        self.assertEqual(spy.peak, 1, "two transcript scans ran at once over the shared _doc_state")

    def test_concurrent_cache_gets_share_one_build(self):
        cache = dh.HarnessCache(ttl=60)
        build = Overlap(lambda: {"sessions": []})
        results = []
        self.assertEqual(run_two(lambda: results.append(cache.get(build))), [])
        self.assertEqual(build.calls, 1, "two requests each ran build()")
        self.assertEqual(results, [{"sessions": []}, {"sessions": []}])


class TestBodyGuard(ServerCase):
    def raw(self, content_length):
        s = socket.create_connection(("127.0.0.1", self.port), timeout=5)
        try:
            s.sendall((f"POST /api/items HTTP/1.1\r\nHost: 127.0.0.1:{self.port}\r\n"
                       f"Content-Type: application/json\r\nContent-Length: {content_length}\r\n\r\n").encode())
            try:
                return s.recv(200).decode("latin-1")
            except socket.timeout:
                return "TIMEOUT: the server is still waiting for a body"
        finally:
            s.close()

    def test_bad_content_length_is_400_before_any_read(self):
        for value in ("-1", "+5", "abc", "1e3", " -0x10"):
            self.assertTrue(self.raw(value).startswith("HTTP/1.0 400"), value)

    def test_each_connection_has_a_socket_timeout(self):
        self.assertEqual(dashboard_http.Handler.timeout, dashboard_http.REQUEST_TIMEOUT_S)
        self.assertEqual(dashboard_http.REQUEST_TIMEOUT_S, 30)


if __name__ == "__main__":
    unittest.main()
