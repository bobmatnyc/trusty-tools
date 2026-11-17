"""Brief ids: MB + 4 hex digits, allocated above the registry and media store, marker + H1 prefix, idempotent assign, check.

Test: tests/test_brief_id.py
"""
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import brief_ids as b  # noqa: E402


class BriefIdTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name
        self.reg = os.path.join(self.dir, "registry.jsonl")
        self.media = os.path.join(self.dir, "media")
        os.makedirs(self.media)

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, name, text):
        p = os.path.join(self.dir, name)
        with open(p, "w") as f:
            f.write(text)
        return p

    def assign(self, p):
        return b.assign(p, "architect", self.reg, self.media)

    def test_allocation_is_monotonic_and_skips_media_ids(self):
        open(os.path.join(self.media, "B0004.json"), "w").write("{}")
        p1, p2 = self.write("a.md", "# One\n"), self.write("b.md", "# Two\n")
        self.assertEqual(self.assign(p1)[0], "MB0005")
        self.assertEqual(self.assign(p2)[0], "MB0006")

    def test_never_reused_after_file_removed(self):
        p = self.write("a.md", "# One\n")
        self.assertEqual(self.assign(p)[0], "MB0000")
        os.remove(p)
        self.assertEqual(self.assign(self.write("b.md", "# Two\n"))[0], "MB0001")

    def test_assign_stamps_marker_and_h1_and_keeps_title(self):
        p = self.write("a.md", "intro\n\n# My title\nbody\n")
        self.assign(p)
        text = open(p).read()
        self.assertEqual(text.split("\n")[0], "<!-- brief-id: MB0000 -->")
        self.assertIn("# MB0000: My title\n", text)
        self.assertEqual(b.title_of(text), "My title")

    def test_assign_is_idempotent(self):
        p = self.write("a.md", "# One\n")
        self.assertEqual(self.assign(p), ("MB0000", True))
        first = open(p).read()
        self.assertEqual(self.assign(p), ("MB0000", False))
        self.assertEqual(open(p).read(), first)
        self.assertEqual(len(b.read_registry(self.reg)), 1)

    def test_frontmatter_and_missing_h1_and_fenced_h1(self):
        p = self.write("2026-10-01-some-thing.md", "---\nk: v\n---\n```\n# not a title\n```\ntext\n")
        self.assign(p)
        text = open(p).read()
        self.assertTrue(text.startswith("---\nk: v\n---\n<!-- brief-id: MB0000 -->\n# MB0000: Some thing\n"))
        self.assertEqual(b.marker_id(text), "MB0000")
        self.assertIn("# not a title", text)

    def test_check_reports_missing_mismatch_and_duplicates(self):
        p = self.write("a.md", "# One\n")
        self.write("c.md", "# No id\n")
        self.assign(p)
        probs = b.check(self.dir, self.reg)
        self.assertEqual(probs, [f"no brief id: {os.path.join(self.dir, 'c.md')}"])
        self.write("d.md", open(p).read())   # same id in two files
        self.assertTrue(any("duplicate id MB0000" in x for x in b.check(self.dir, self.reg)))

    def test_ids_exhaust_without_wrapping(self):
        open(os.path.join(self.media, "Bffff.json"), "w").write("{}")
        with self.assertRaises(RuntimeError):
            b.allocate(self.reg, self.media)

    def test_registry_rows_have_required_fields(self):
        self.assign(self.write("a.md", "# One\n"))
        row = json.loads(open(self.reg).read().splitlines()[0])
        self.assertEqual(set(row), {"id", "path", "title", "created", "project"})

    def test_media_allocator_skips_registry_ids(self):
        import dashboard_media as dm
        self.assign(self.write("a.md", "# One\n"))
        self.assertEqual(dm._used([], self.reg), {0})
        self.assertEqual(dm._used([]), set())

    def test_legacy_b_ids_are_read_and_the_next_id_clears_them(self):
        with open(self.reg, "w") as f:
            f.write(json.dumps({"id": "B09d7", "path": "x.md", "title": "t", "created": "c", "project": "architect"}) + "\n")
        p = self.write("l.md", "<!-- brief-id: B09d7 -->\n# B09d7: Old\n")
        self.assertEqual(b.marker_id(open(p).read()), "B09d7")
        self.assertEqual(b.title_of(open(p).read()), "Old")
        self.assertEqual(b.allocate(self.reg, self.media), "MB09d8")
        self.assertEqual(dm_used(self.reg), {0x09d7})


def dm_used(reg):
    import dashboard_media as dm
    return dm._used([], reg)


if __name__ == "__main__":
    unittest.main()
