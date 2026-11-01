"""dashboard-serve.py refuses a second copy on the same store (#10571).

Covers: a second server on the same --root, even on another port, exits 0 (so launchd's KeepAlive does not loop) and
names the running one; the lock is released when the first exits, so a restart after a crash starts normally; a
different --root is a different dashboard and starts.
Test: tests/test_dashboard_single_instance.py
"""
import os
import socket
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
SERVE = os.path.join(os.path.dirname(HERE), "scripts", "dashboard-serve.py")


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def start(root, port):
    return subprocess.Popen([sys.executable, SERVE, "--mode", "session", "--root", root, "--port", str(port), "--title", "t"],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def wait_serving(port, proc, timeout=15):
    end = time.time() + timeout
    while time.time() < end:
        if proc.poll() is not None:
            raise AssertionError(f"server exited early: {proc.stderr.read()}")
        try:
            req = urllib.request.Request(f"http://127.0.0.1:{port}/config.json", headers={"Host": f"127.0.0.1:{port}"})
            with urllib.request.urlopen(req, timeout=2):
                return
        except OSError:
            time.sleep(0.2)
    raise AssertionError("server never answered")


class SingleInstance(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = os.path.join(tmp.name, "store")
        self.other = os.path.join(tmp.name, "other")

    def stop(self, proc):
        if proc.poll() is None:
            proc.terminate()
            proc.wait(10)

    def test_a_second_copy_on_the_same_store_is_refused_then_a_restart_works(self):
        p1 = free_port()
        first = start(self.root, p1)
        self.addCleanup(self.stop, first)
        wait_serving(p1, first)
        second = subprocess.run([sys.executable, SERVE, "--mode", "session", "--root", self.root, "--port", str(free_port()),
                                 "--title", "t"], capture_output=True, text=True, timeout=30)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertIn("already running", second.stderr)
        self.assertIn(f"pid {first.pid} on port {p1}", second.stderr)
        same = subprocess.run([sys.executable, SERVE, "--mode", "session", "--root", self.root, "--port", str(p1),
                               "--title", "t"], capture_output=True, text=True, timeout=30)
        self.assertEqual((same.returncode, "already running" in same.stderr), (0, True), same.stderr)   # lock before the port probe
        self.stop(first)
        p2 = free_port()
        again = start(self.root, p2)   # the kernel dropped the lock with the first process: no stale-lock refusal
        self.addCleanup(self.stop, again)
        wait_serving(p2, again)

    def test_another_store_is_another_dashboard(self):
        p1, p2 = free_port(), free_port()
        a, b = start(self.root, p1), start(self.other, p2)
        self.addCleanup(self.stop, a)
        self.addCleanup(self.stop, b)
        wait_serving(p1, a)
        wait_serving(p2, b)


if __name__ == "__main__":
    unittest.main()
