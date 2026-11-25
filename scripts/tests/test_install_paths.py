"""Tests for scripts/lib/install_paths.py, the Python twin of the InstallPaths resolver (#11821 P6a).

Each case mirrors a test in crates/trusty-common/src/install_paths/tests.rs, named in its docstring. An env is always
injected and home is always a temp dir, so no case reads the operator's environment or home.
Run: python3 -m unittest discover -s scripts/tests -p 'test_install_paths.py'
"""
import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path

LIB = Path(__file__).resolve().parents[1] / "lib" / "install_paths.py"
_spec = importlib.util.spec_from_file_location("install_paths", LIB)
ip = importlib.util.module_from_spec(_spec)
sys.modules["install_paths"] = ip
_spec.loader.exec_module(ip)


class Base(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.home = self._tmp.name

    def tearDown(self):
        self._tmp.cleanup()

    def env(self, **pairs):
        return {"HOME": self.home, **pairs}

    def write_file(self, body):
        d = os.path.join(self.home, ".trusty-tools")
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, "install.toml"), "w", encoding="utf-8") as f:
            f.write(body)


class Precedence(Base):
    def test_defaults_match_today(self):
        """Rust: defaults_match_today."""
        p = ip.resolve(env=self.env())
        self.assertEqual(p.projects_root, ip.ResolvedPath(os.path.join(self.home, "trusty-mpm-projects"), "default"))
        self.assertEqual(p.config_dir, ip.ResolvedPath(self.home, "default"))
        self.assertIsNone(p.claude_config_dir)
        self.assertIsNone(p.codex_home)
        self.assertEqual(p.tools_dir(), os.path.join(self.home, ".trusty-tools"))
        self.assertEqual(p.mpm_dir(), os.path.join(self.home, ".trusty-mpm"))

    def test_flag_beats_env_file_and_default(self):
        """Rust: flag_beats_env_file_and_default."""
        self.write_file('projects_root = "/from-file"\n')
        p = ip.resolve({"projects_root": "/from-flag"}, env=self.env(TRUSTY_MPM_WORKSPACE_ROOT="/from-env"))
        self.assertEqual(p.projects_root, ip.ResolvedPath("/from-flag", "flag"))

    def test_env_beats_file_and_default(self):
        """Rust: env_beats_file_and_default; env is trimmed and `~`-expanded."""
        self.write_file('projects_root = "/from-file"\nconfig_dir = "/cfg-file"\n')
        p = ip.resolve(env=self.env(TRUSTY_MPM_WORKSPACE_ROOT=" ~/ws ", TRUSTY_MPM_CONFIG_DIR="/cfg-env"))
        self.assertEqual(p.projects_root, ip.ResolvedPath(os.path.join(self.home, "ws"), "env"))
        self.assertEqual(p.config_dir, ip.ResolvedPath("/cfg-env", "env"))

    def test_file_beats_default_and_blank_env_is_unset(self):
        """Rust: file_beats_default; a whitespace-only env value is unset."""
        self.write_file('projects_root = "/p"\nconfig_dir = "/c"\nclaude_config_dir = "/cc"\ncodex_home = "/cx"\n')
        p = ip.resolve(env=self.env(TRUSTY_MPM_WORKSPACE_ROOT="  "))
        self.assertEqual([p.projects_root.path, p.config_dir.path, p.claude_config_dir.path, p.codex_home.path],
                         ["/p", "/c", "/cc", "/cx"])
        self.assertEqual({p.projects_root.source, p.codex_home.source}, {"install-file"})
        self.assertEqual(ip.managed_claude_config_dir(self.env()), "/c/.trusty-tools/trusty-mpm/claude-config")

    def test_install_file_is_anchored_at_home_not_config_dir(self):
        """Rust: install_file_is_anchored_at_home_not_config_dir."""
        self.assertEqual(ip.install_file_path(self.home), os.path.join(self.home, ".trusty-tools", "install.toml"))
        p = ip.resolve(env=self.env(TRUSTY_MPM_CONFIG_DIR="/elsewhere"))
        self.assertEqual(p.tools_dir(), "/elsewhere/.trusty-tools")


class Refusals(Base):
    def assert_refused(self, fragment, fn):
        with self.assertRaises(ip.InstallPathsError) as cm:
            fn()
        self.assertIn(fragment, str(cm.exception))

    def test_relative_and_empty_values_are_rejected(self):
        """Rust: relative_file_value_is_rejected, empty_file_value_is_rejected, relative_env/flag_value_is_rejected."""
        self.write_file('projects_root = "rel/path"\n')
        self.assert_refused("projects_root from install-file must be an absolute path", lambda: ip.resolve(env=self.env()))
        self.write_file('config_dir = ""\n')
        self.assert_refused("config_dir from install-file is empty", lambda: ip.config_dir(self.env()))
        self.assert_refused("from env must be an absolute path",
                            lambda: ip.projects_root(self.env(TRUSTY_MPM_WORKSPACE_ROOT="rel")))
        self.write_file("")  # keys resolve in Rust order, so the bad config_dir above would be reported first
        self.assert_refused("from flag must be an absolute path",
                            lambda: ip.resolve({"codex_home": "rel"}, env=self.env()))

    def test_file_values_are_literal(self):
        """Rust: install.toml values are taken literally (mod.rs:344), so `~` is relative there."""
        self.write_file('projects_root = "~/ws"\n')
        self.assert_refused("must be an absolute path", lambda: ip.projects_root(self.env()))

    def test_unknown_key_and_bad_toml_are_rejected(self):
        """Rust: unknown_key_is_rejected, parse_error_names_line_and_column."""
        self.write_file('project_root = "/typo"\n')
        self.assert_refused("unknown field `project_root`", lambda: ip.config_dir(self.env()))
        self.write_file("projects_root = [\n")
        self.assert_refused("cannot parse", lambda: ip.projects_root(self.env()))
        self.write_file("projects_root = 3\n")  # tomllib parses it; the flat reader below 3.11 refuses the line
        self.assert_refused("cannot parse", lambda: ip.projects_root(self.env()))

    def test_flat_reader_below_311_reads_the_writer_subset_only(self):
        """The Python < 3.11 reader: what `toml::to_string` writes parses; anything richer refuses."""
        self.assertEqual(ip._flat_toml("# c\nprojects_root = \"/p\" # t\n\nconfig_dir = '/c'\n"),
                         {"projects_root": "/p", "config_dir": "/c"})
        for bad in ("[table]\n", 'a = "/x"\na = "/y"\n', 'a = "/x\\U' '0001F600"\n', "a = 3\n"):
            with self.assertRaises(ValueError, msg=bad):
                ip._flat_toml(bad)

    def test_env_wins_over_broken_file(self):
        """Rust: env_wins_over_broken_file; the file is never read for a key env decides."""
        self.write_file("not toml at all ===\n")
        self.assertEqual(ip.config_dir(self.env(TRUSTY_MPM_CONFIG_DIR="/cfg")), "/cfg")
        self.assert_refused("cannot parse", lambda: ip.projects_root(self.env(TRUSTY_MPM_CONFIG_DIR="/cfg")))

    def test_broken_file_never_falls_back_to_home(self):
        """Rust: save_fails_closed_on_broken_install_file; no default is returned for a writer."""
        self.write_file("[broken\n")
        for fn in (ip.projects_root, ip.config_dir, ip.tools_dir, ip.managed_claude_config_dir):
            self.assert_refused("cannot parse", lambda fn=fn: fn(self.env()))

    def test_cli_exits_1_on_a_broken_file(self):
        """The `python3 install_paths.py` report fails closed too."""
        import subprocess

        self.write_file("[broken\n")
        out = subprocess.run([sys.executable, "-I", str(LIB)], env={"HOME": self.home, "PATH": os.environ["PATH"]},
                             capture_output=True, text=True)
        self.assertEqual(out.returncode, 1, out.stderr)
        self.assertIn("cannot parse", out.stderr)


class Callers(Base):
    """The migrated scripts honour install.toml and refuse a broken one before they write."""

    SCRIPTS = LIB.parents[1]

    def run_script(self, *argv):
        import subprocess

        return subprocess.run([sys.executable, "-I", str(self.SCRIPTS / argv[0]), *argv[1:]],
                              env={"HOME": self.home, "PATH": os.environ["PATH"]}, capture_output=True, text=True)

    def test_pm_context_probe_uses_the_configured_config_dir(self):
        self.write_file(f'config_dir = "{self.home}/moved"\n')
        out = self.run_script("pm_context_probe.py", "run", "--levers", "no-such-lever")
        self.assertEqual(out.returncode, 2, out.stderr)
        self.assertIn(f"{self.home}/moved/.trusty-tools/trusty-mpm/claude-config", out.stderr)

    def test_pm_context_probe_refuses_a_broken_install_file(self):
        self.write_file("[broken\n")
        for argv in (("run", "--levers", "baseline"), ("print-env",)):
            out = self.run_script("pm_context_probe.py", *argv)
            self.assertEqual(out.returncode, 2, out.stderr)
            self.assertIn("refused: cannot parse", out.stderr)

    def test_drift_test_refuses_a_broken_install_file(self):
        self.write_file("[broken\n")
        out = self.run_script("drift_test_mem_search.py")
        self.assertEqual(out.returncode, 2, out.stderr)
        self.assertIn("refused: cannot parse", out.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.home, ".trusty-tools", "drift")))


if __name__ == "__main__":
    unittest.main()
