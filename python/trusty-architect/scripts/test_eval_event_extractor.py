"""Tests for scripts/eval-event-extractor.py (T07): scoring and fixture sanity.
The claude runs themselves are not tested here; they cost tokens."""

import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("eval_ee", os.path.join(HERE, "eval-event-extractor.py"))
ee = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ee)
_vspec = importlib.util.spec_from_file_location("verify_claim", os.path.join(HERE, "verify-claim.py"))
vc = importlib.util.module_from_spec(_vspec)
_vspec.loader.exec_module(vc)


class Scoring(unittest.TestCase):
    EXP = {"kind": "merged", "repo": "r", "pr": 5, "head": "ABCDEF0123", "base": None,
           "merge_sha": None, "version": None}

    def test_field_scoring_normalises_case_pr_type_and_null(self):
        got = {"kind": "merged", "repo": "r", "pr": "5", "head": "abcdef0123", "base": "", "merge_sha": None}
        self.assertTrue(all(ee.score_event(self.EXP, got).values()))
        bad = dict(got, head="abcdef0124", kind="fyi")
        sc = ee.score_event(self.EXP, bad)
        self.assertFalse(sc["head"])
        self.assertFalse(sc["kind"])
        self.assertTrue(sc["pr"])
        self.assertFalse(any(ee.score_event(self.EXP, None).values()))

    def test_bar_needs_every_sha_and_pr_and_90_percent_kind(self):
        def row(**over):
            got = dict(self.EXP, **over)
            return {"score": ee.score_event(self.EXP, got)}
        self.assertTrue(ee.summarize([row()] * 10)["pass"])
        self.assertFalse(ee.summarize([row()] * 9 + [row(head="x")])["pass"])  # one SHA wrong
        self.assertTrue(ee.summarize([row()] * 9 + [row(kind="fyi")])["pass"])  # kind 90%
        self.assertFalse(ee.summarize([row()] * 8 + [row(kind="fyi")] * 2)["pass"])  # kind 80%
        self.assertFalse(ee.summarize([])["pass"])

    def test_fabricated_flags_shas_missing_from_the_text(self):
        got = {"head": "aaaaaaaaaa", "base": "bbbbbbbbbb", "merge_sha": None}
        self.assertEqual(ee.fabricated("head aaaaaaaaaa and nothing else", got), ["base"])
        self.assertEqual(ee.fabricated("x", None), [])


class Fixtures(unittest.TestCase):
    def test_fixtures_are_well_formed_and_expected_values_appear_in_the_text(self):
        fx = ee.load_fixtures()
        self.assertGreaterEqual(len(fx), 10)
        self.assertEqual(len({f["id"] for f in fx}), len(fx))
        senders = {f["from"] for f in fx}
        self.assertTrue({"tm-dogfood", "tm-memory-01", "tm-webapp", "side-project-af",
                         "tm-mpm-utils-01"} <= senders)
        for f in fx:
            e = f["expected"]
            self.assertIn(e["kind"], vc.KINDS, f["id"])
            for k in ("head", "base", "merge_sha", "version"):
                if e[k]:
                    self.assertIn(e[k], f["text"], f"{f['id']} {k}")
            if e["pr"]:
                self.assertIn(f"#{e['pr']}", f["text"], f["id"])
            vc.parse_event({"kind": e["kind"], "pr": e["pr"]})


if __name__ == "__main__":
    unittest.main()
