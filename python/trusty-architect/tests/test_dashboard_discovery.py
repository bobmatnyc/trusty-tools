"""dashboard-serve.py discovery file (trusty-tools #9474): <data dir>/trusty-architect/http_addr.

Covers: file written after bind with 127.0.0.1:<port>, removed on SIGTERM and SIGINT,
removal skips a file holding another address, write/remove failures never raise,
and no file is written when the port is busy.
Run: python3 -m unittest scripts/test_dashboard_discovery.py
"""
import importlib.util
import io
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stderr
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
SERVE = os.path.join(SCRIPTS, "dashboard-serve.py")


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def load_serve():
    sys.path.insert(0, SCRIPTS)
    spec = importlib.util.spec_from_file_location("dashboard_serve_under_test", SERVE)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.data = os.path.join(self.tmp.name, "data")
        self.addr_file = os.path.join(self.data, "trusty-architect", "http_addr")
        self.page = os.path.join(self.tmp.name, "index.html")
        with open(self.page, "w") as f:
            f.write("<html><head></head><body>x</body></html>")

    def start(self, port):
        env = dict(os.environ, TRUSTY_DATA_DIR_OVERRIDE=self.data)
        p = subprocess.Popen(
            [sys.executable, SERVE, "--port", str(port), "--root", os.path.join(self.tmp.name, "store"),
             "--page", self.page], env=env, stderr=subprocess.PIPE, text=True)
        self.addCleanup(lambda: p.poll() is None and p.kill())
        return p

    def wait_for(self, pred, what, timeout=10):
        end = time.time() + timeout
        while time.time() < end:
            if pred():
                return
            time.sleep(0.05)
        self.fail(f"timed out waiting for {what}")


class ServerProcess(Base):
    def run_lifecycle(self, sig):
        port = free_port()
        p = self.start(port)
        self.wait_for(lambda: os.path.exists(self.addr_file), "http_addr file")
        with open(self.addr_file) as f:
            self.assertEqual(f.read().strip(), f"127.0.0.1:{port}")
        p.send_signal(sig)
        p.wait(timeout=10)
        self.assertFalse(os.path.exists(self.addr_file), "file should be removed on shutdown")

    def test_file_written_then_removed_on_sigterm(self):
        self.run_lifecycle(signal.SIGTERM)

    def test_file_removed_on_sigint(self):
        self.run_lifecycle(signal.SIGINT)

    def test_busy_port_refuses_and_writes_no_file(self):
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            s.listen(1)
            p = self.start(s.getsockname()[1])
            self.assertEqual(p.wait(timeout=10), 1)
        self.assertFalse(os.path.exists(self.addr_file))


class Helpers(Base):
    def setUp(self):
        super().setUp()
        self.mod = load_serve()
        env = mock.patch.dict(os.environ, {"TRUSTY_DATA_DIR_OVERRIDE": self.data})
        env.start()
        self.addCleanup(env.stop)

    def test_remove_skips_file_with_different_address(self):
        os.makedirs(os.path.dirname(self.addr_file))
        with open(self.addr_file, "w") as f:
            f.write("127.0.0.1:9999")
        self.mod.remove_addr_file("127.0.0.1:1234")
        with open(self.addr_file) as f:
            self.assertEqual(f.read(), "127.0.0.1:9999")

    def test_remove_deletes_file_with_own_address(self):
        self.mod.write_addr_file("127.0.0.1:1234")
        self.mod.remove_addr_file("127.0.0.1:1234")
        self.assertFalse(os.path.exists(self.addr_file))

    def test_write_failure_logs_one_line_and_does_not_raise(self):
        blocker = os.path.join(self.tmp.name, "blocker")
        open(blocker, "w").close()
        with mock.patch.dict(os.environ, {"TRUSTY_DATA_DIR_OVERRIDE": blocker}):
            err = io.StringIO()
            with redirect_stderr(err):
                self.mod.write_addr_file("127.0.0.1:1234")
        self.assertEqual(len(err.getvalue().strip().splitlines()), 1, err.getvalue())

    def test_data_dir_resolution(self):
        with mock.patch.dict(os.environ, {"TRUSTY_DATA_DIR_OVERRIDE": ""}, clear=False):
            os.environ.pop("XDG_DATA_HOME", None)
            with mock.patch.object(self.mod.sys, "platform", "darwin"):
                self.assertEqual(self.mod.data_dir(), os.path.expanduser("~/Library/Application Support"))
            with mock.patch.object(self.mod.sys, "platform", "linux"):
                self.assertEqual(self.mod.data_dir(), os.path.expanduser("~/.local/share"))
            with mock.patch.dict(os.environ, {"XDG_DATA_HOME": "/x/y"}):
                with mock.patch.object(self.mod.sys, "platform", "linux"):
                    self.assertEqual(self.mod.data_dir(), "/x/y")


if __name__ == "__main__":
    unittest.main()
