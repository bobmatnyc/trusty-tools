"""#10780: an Intent Ledger answer carries its source, and the resume digest never restores an Architect
inference, an unsourced answer or an unsourced handoff line under the owner's name.

The incident: before a /clear the Architect's handoff said "Pace ~1.75%/h ... = two helpers (one per track)";
the next session read it as an owner ruling ("two-helper ruling 2026-11-06T23:3xZ") and kept the owner's
helper count at two instead of four.
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import datg  # noqa: E402

DATG = os.path.join(SCRIPTS, "datg.py")
OWNER = {"by": "owner", "cite": "chat 2026-11-06T20:48Z",
         "quote": "OK, unleash two more agents and don't stop at 98%."}
INFERENCE = {"by": "architect", "basis": "1.75%/h pace arithmetic after the 23:38Z reset"}


def cli(project, args, stdin=None):
    return subprocess.run([sys.executable, DATG, "--project", project, *args], input=stdin,
                          capture_output=True, text=True)


class Base(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.p = self._tmp.name
        datg.op_init(self.p, "ar")
        datg.write_ops(self.p, [{"op": "add", "kind": "G", "title": "use the reset by Monday"},
                                {"op": "add", "kind": "D", "title": "helpers until Monday", "goal": "ar-G01"},
                                {"op": "add", "kind": "D", "title": "pace after the reset", "goal": "ar-G01"},
                                {"op": "add", "kind": "D", "title": "weekend helpers", "goal": "ar-G01"},
                                {"op": "add", "kind": "D", "title": "old pace note", "goal": "ar-G01"}])

    def item(self, iid):
        st = datg.load_state(self.p)
        return next(i for i in st["items"] if i["id"] == iid)

    def digest(self):
        return datg.build_digest(self.p, 3000)


class SourceCase(Base):
    def test_owner_and_architect_sources_are_stored_in_order(self):
        datg.write_ops(self.p, [{"op": "update", "id": "ar-D01", "answer": "four helpers until Mon 09:00Z",
                                 "source": dict(OWNER, until="2026-11-09T09:00Z")},
                                {"op": "update", "id": "ar-D02", "answer": "two helpers", "source": INFERENCE}])
        self.assertEqual(list(self.item("ar-D01")["source"]), ["by", "cite", "quote", "until"])
        self.assertEqual(self.item("ar-D02")["source"], INFERENCE)

    def test_bad_sources_are_refused_and_leave_the_file(self):
        before = open(datg.paths(self.p)[1]).read()
        for src in ({"by": "owner", "cite": "chat 2026-11-06T23:38Z"},           # no quote
                    {"by": "architect", "basis": "x", "quote": "two helpers"},   # an inference with an owner quote
                    {"by": "the owner", "quote": "x", "cite": "y"},
                    dict(OWNER, until="Monday")):
            with self.assertRaises(datg.DatgError, msg=src):
                datg.write_ops(self.p, [{"op": "update", "id": "ar-D01", "answer": "x", "source": src}])
        with self.assertRaises(datg.DatgError):
            datg.write_ops(self.p, [{"op": "update", "id": "ar-D01", "source": OWNER}])   # no answer
        self.assertEqual(open(datg.paths(self.p)[1]).read(), before)

    def test_a_new_answer_without_a_source_drops_the_old_source(self):
        datg.write_ops(self.p, [{"op": "update", "id": "ar-D01", "answer": "four", "source": OWNER}])
        datg.write_ops(self.p, [{"op": "update", "id": "ar-D01", "answer": "two"}])
        self.assertNotIn("source", self.item("ar-D01"))

    def test_cli_flags_build_the_source(self):
        r = cli(self.p, ["update", "ar-D02", "--answer", "two helpers", "--source", "architect", "--basis", "pace"])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.item("ar-D02")["source"], {"by": "architect", "basis": "pace"})
        r = cli(self.p, ["update", "ar-D02", "--answer", "two", "--quote", "x"])
        self.assertEqual(r.returncode, 2)
        self.assertIn("need --source", r.stderr)


class ResumeCase(Base):
    def sections(self, text):
        out, cur = {}, None
        for ln in text.splitlines():
            if not ln.startswith("  "):
                cur = ln
                out[cur] = []
            elif cur:
                out[cur].append(ln)
        return out

    def test_an_inference_recorded_before_clear_never_restores_under_the_owner_heading(self):
        datg.write_ops(self.p, [
            {"op": "update", "id": "ar-D01", "answer": "four helpers, two per track, until Mon 09:00Z", "source": OWNER},
            {"op": "update", "id": "ar-D02", "answer": "two helpers, one per track", "source": INFERENCE},
            {"op": "update", "id": "ar-D03", "answer": "one helper per track",
             "source": dict(OWNER, cite="dashboard D70", quote="Keep one helper on the rules", until="2026-11-06T20:48Z")},
            {"op": "update", "id": "ar-D04", "answer": "two helpers"}])
        # The /clear: a fresh session's SessionStart context is all it has.
        ctx = datg.session_start_context(datg.load_state(self.p), self.p)
        sec = self.sections(ctx)
        owner = "\n".join(sec["OWNER RULINGS (with source)"])
        self.assertIn("ar-D01", owner)
        self.assertIn('owner turn: chat 2026-11-06T20:48Z "OK, unleash two more agents', owner)
        for iid in ("ar-D02", "ar-D03", "ar-D04"):
            self.assertNotIn(iid, owner)
        self.assertIn("ar-D02", "\n".join(sec["ARCHITECT INFERENCES (not owner rulings)"]))
        self.assertIn("ar-D03", "\n".join(sec["OWNER RULINGS EXPIRED (history only, not current)"]))
        self.assertIn("ar-D04", "\n".join(sec["UNSOURCED ANSWERS (not owner rulings; confirm the source before acting)"]))

    def test_an_unsourced_handoff_line_claiming_an_owner_ruling_is_flagged(self):
        state_md = os.path.join(self.p, "state.md")
        with open(state_md, "w") as f:
            f.write("## Handoff 03:21Z\n"
                    "- USAGE 4% at 03:04Z. Two helpers, one per track (Ada 23:39Z).\n"
                    "- two-helper ruling 2026-11-06T23:3xZ stays the limit\n"
                    "- Owner ruling: four helpers until Mon (owner turn: chat 2026-11-06T20:48Z)\n"
                    "- Architect inference: pace 1.75%/h\n")
        datg.write_ops(self.p, [{"op": "session", "task": "pace the reset", "next": "keep four busy",
                                 "hold": "User instruction (Ada): two helpers max", "refs": [{"key": "handoff", "value": "state.md"}]}])
        dig = self.digest()
        flags = [ln for ln in dig.splitlines() if "FLAG unsourced owner claim" in ln]
        self.assertEqual(len(flags), 3, dig)
        self.assertTrue(any(ln.startswith("  FLAG") and "hold:" in ln for ln in flags))
        self.assertTrue(any("state.md:2" in ln for ln in flags) and any("state.md:3" in ln for ln in flags))
        self.assertFalse(any("state.md:4" in ln or "state.md:5" in ln for ln in flags))
        r = cli(self.p, ["provenance", state_md])
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertEqual([ln.split(":")[1] for ln in r.stdout.splitlines()], ["2", "3"])
        self.assertEqual(cli(self.p, ["provenance", os.path.join(self.p, "nope.md")]).returncode, 2)


class ProvenanceCase(unittest.TestCase):
    def test_claim_detection(self):
        for text in ("User ruling (Ada): two helpers", "owner instruction: stop", "basis: the two-helper ruling",
                     "TWO helpers only (Ada 23:39Z)"):
            self.assertTrue(datg.unsourced_claim(text), text)
        for text in ("User ruling (Ada): \"x\" (owner turn: chat 2026-11-06T20:48Z)", "Architect answer: pace",
                     "[Ada Architect 23:39Z] sent", "rule r-075"):
            self.assertFalse(datg.unsourced_claim(text), text)


if __name__ == "__main__":
    unittest.main()
