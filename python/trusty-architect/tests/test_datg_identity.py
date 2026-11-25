"""#11685: two PMs in one project folder keep distinct Intent Ledger identities.

Covers: the TMUX_PANE session (not the folder) picks each PM's ledger; a dbatg-only
session is refused with a clear error instead of falling back to another ledger;
every write keeps a restorable prior version.
Run: python3 -m unittest tests/test_datg_identity.py
"""
import json
import os
import sys
import tempfile
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import datg  # noqa: E402
import datg_store  # noqa: E402

GOAL = {"op": "add", "kind": "G", "title": "ship", "maps": "G0c"}


class IdentityCase(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.p = self._tmp.name
        env = {k: v for k, v in os.environ.items() if k not in ("DATG_SESSION", "DATG_STATE_DIR")}
        env["ARCHITECT_INBOX_DIR"] = os.path.join(self.p, "inbox")
        patcher = mock.patch.dict(os.environ, env, clear=True)
        patcher.start()
        self.addCleanup(patcher.stop)
        datg_store._SESSION = None
        datg_store._STATE_DIR = None
        self.addCleanup(setattr, datg_store, "_SESSION", None)

    def as_pane(self, session):
        """Run as the PM whose TMUX_PANE resolves to `session`; the folder is the same for both."""
        datg_store._SESSION = None
        return mock.patch.object(datg_store, "_tmux_session", return_value=session)

    def init(self, session, code):
        datg_store._SESSION = session
        datg.op_init(self.p, code)
        datg.write_ops(self.p, [GOAL])
        datg_store._SESSION = None

    def test_two_pms_in_one_folder_get_distinct_ledgers(self):
        self.init("tm-trusty-tools", "tt")
        self.init("tm-trusty-cloud", "tcl")
        dirs = {}
        for s, code in (("tm-trusty-tools", "tt"), ("tm-trusty-cloud", "tcl")):
            with self.as_pane(s):
                dirs[s] = datg.datg_dir(self.p)
                datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": s, "goal": f"{code}-G01"}])
        self.assertNotEqual(dirs["tm-trusty-tools"], dirs["tm-trusty-cloud"])
        for s, d in dirs.items():
            self.assertEqual(os.path.basename(d), s)
            with open(os.path.join(d, "state.json")) as f:
                self.assertEqual([t["title"] for t in json.load(f)["tasks"]], [s])

    def test_dbatg_only_session_is_refused_not_redirected(self):
        self.init("tm-trusty-tools", "tt")
        dbatg = os.path.join(self.p, ".trusty-mpm", "dbatg", "tm-trusty-cloud")
        os.makedirs(dbatg)
        with open(os.path.join(dbatg, "state.json"), "w") as f:
            f.write("{}")
        with self.as_pane("tm-trusty-cloud"):
            with self.assertRaisesRegex(datg.DatgError, "tm-trusty-cloud keeps its ledger in .trusty-mpm/dbatg"):
                datg.datg_dir(self.p)

    def test_every_write_keeps_a_restorable_prior_version(self):
        self.init("tm-trusty-tools", "tt")
        with self.as_pane("tm-trusty-tools"):
            d = datg.datg_dir(self.p)
            with open(os.path.join(d, "state.json")) as f:
                before = f.read()
            v = json.loads(before)["v"]
            datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": "x", "goal": "tt-G01"}])
        with open(os.path.join(d, "prior", f"state.v{v}.json")) as f:
            self.assertEqual(f.read(), before)

    def test_prior_versions_are_capped(self):
        self.init("tm-trusty-tools", "tt")
        with self.as_pane("tm-trusty-tools"):
            for i in range(datg_store.PRIOR_KEEP + 3):
                datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": f"t{i}", "goal": "tt-G01"}])
            d = datg.datg_dir(self.p)
        self.assertEqual(len(os.listdir(os.path.join(d, "prior"))), datg_store.PRIOR_KEEP)


if __name__ == "__main__":
    unittest.main()
