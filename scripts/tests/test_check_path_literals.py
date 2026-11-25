"""Tests for scripts/check_path_literals.py, the hard-coded path lint (#11821 P6a).

The failure cases run against a scratch git repo, never the checkout; one case runs the gate on the checkout itself.
Run: python3 -m unittest discover -s scripts/tests -p 'test_check_path_literals.py'
"""
import contextlib
import importlib.util
import io
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("check_path_literals", SCRIPTS / "check_path_literals.py")
lint = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(lint)


def run(*argv):
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        code = lint.main(list(argv))
    return code, buf.getvalue()


class Patterns(unittest.TestCase):
    def test_home_rooted_literals_hit(self):
        for line in ('root = os.path.expanduser("~/trusty-mpm-projects")',
                     'cfg = Path.home() / ".trusty-tools" / "x"',
                     'let d = dirs::home_dir()?.join(".trusty-mpm");',
                     'DIR="$HOME/.trusty-tools/cache"',
                     'DIR="${HOME}/trusty-mpm-projects"'):
            self.assertEqual(len(lint.hits_in(line)), 1, line)

    def test_non_paths_comments_and_waivers_do_not_hit(self):
        for line in ('base = os.path.join(project, ".trusty-mpm", "datg")',
                     '# default ~/trusty-mpm-projects',
                     '/// `~/.trusty-tools/<crate>`',
                     'x = "~/.trusty-mpm-owner.toml"',
                     'msg = "see ~/.trusty-tools"  # path-literal: allow'):
            self.assertEqual(lint.hits_in(line), [], line)


class Gate(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = self._tmp.name
        subprocess.run(["git", "init", "-q", self.root], check=True)
        self.base = os.path.join(self.root, "base.tsv")

    def tearDown(self):
        self._tmp.cleanup()

    def put(self, rel, body):
        path = os.path.join(self.root, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        Path(path).write_text(body)
        subprocess.run(["git", "-C", self.root, "add", rel], check=True)

    def gate(self, *extra):
        return run("--root", self.root, "--baseline", self.base, *extra)

    def test_new_literal_fails_and_baselined_one_passes(self):
        self.put("a.py", 'X = "~/.trusty-tools/x"\n')
        self.assertEqual(self.gate("--seed")[0], 0)
        self.assertEqual(self.gate()[0], 0)
        self.put("a.py", 'X = "~/.trusty-tools/x"\nY = "~/trusty-mpm-projects"\n')
        self.put("b.rs", 'let p = home.join(".trusty-mpm");\n')
        code, out = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("FAIL NEW: a.py: 2 hits, baseline 1", out)
        self.assertIn("FAIL NEW: b.rs: 1 hits, baseline 0", out)
        self.assertIn("b.rs:1: [home-join]", out)

    def test_migration_notes_and_shrink_only_lowers(self):
        self.put("a.py", 'X = "~/.trusty-tools/x"\nY = "~/.trusty-mpm/y"\n')
        self.put("c.py", 'Z = "~/.trusty-mpm/z"\n')
        self.gate("--seed")
        self.put("a.py", 'X = "~/.trusty-tools/x"\n')
        self.put("c.py", "Z = resolved\n")
        code, out = self.gate()
        self.assertEqual(code, 0)
        self.assertIn("NOTE: a.py: 1 hits, baseline 2", out)
        self.assertEqual(self.gate("--shrink")[0], 0)
        self.assertEqual(lint.read_baseline(self.base), {"a.py": 1})
        self.put("a.py", 'X = "~/.trusty-tools/x"\nY = "~/.trusty-mpm/y"\n')
        code, _ = self.gate("--shrink")
        self.assertEqual(code, 1, "shrink must not raise a count")
        self.assertEqual(lint.read_baseline(self.base), {"a.py": 1})

    def test_allowlisted_resolver_and_bad_baseline(self):
        self.put("scripts/lib/install_paths.py", 'D = "~/trusty-mpm-projects"\n')
        Path(self.base).write_text("a.py 3\n")
        self.assertEqual(self.gate()[0], 2)
        Path(self.base).write_text("")
        self.assertEqual(self.gate()[0], 0)


class Checkout(unittest.TestCase):
    def test_gate_passes_on_this_checkout(self):
        code, out = run()
        self.assertEqual(code, 0, out)


if __name__ == "__main__":
    unittest.main()
