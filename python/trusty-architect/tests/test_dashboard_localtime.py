"""Dashboard absolute times render in the viewer's local time with the local weekday (both HTML files).
Runs the page's localTime helper under node with TZ=America/New_York (skipped when node is absent)."""
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PAGES = [os.path.join(ROOT, p) for p in ("templates/dashboard/index.html",)]


def render(page, iso, tz="America/New_York"):
    src = open(page, encoding="utf-8").read()
    m = re.search(r"/\* BEGIN localTime \*/(.*?)/\* END localTime \*/", src, re.S)
    assert m
    r = subprocess.run(["node", "-e", m.group(1) + "\nconsole.log(localTime(%r));" % iso],
                       capture_output=True, text=True, timeout=30, env=dict(os.environ, TZ=tz))
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardLocalTime(unittest.TestCase):
    def test_local_weekday_and_hour(self):
        for page in PAGES:
            with self.subTest(page=page):
                self.assertEqual(render(page, "2026-10-09T01:45:00Z"), "Thu 9:45 PM")   # 2026-10-09 01:45Z is Fri in UTC, Thu in New York
                self.assertEqual(render(page, "2026-10-09T04:05:00Z"), "Fri 12:05 AM")
                self.assertEqual(render(page, "2026-10-09T01:45:00Z", "UTC"), "Fri 1:45 AM")
                self.assertEqual(render(page, "nope"), "")

    def test_footer_says_local(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertIn("Times are shown in your local time.", src)
            self.assertNotIn("Times are UTC", src)


def attention(page, body):
    src = open(page, encoding="utf-8").read()
    blocks = [re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (n, n), src, re.S).group(1) for n in ("askKind", "attention")]
    parked = re.search(r"  function isParked\(it\)[^\n]*\n", src).group(0)
    js = parked + "\n".join(blocks) + "\nconst D = (id, extra) => Object.assign({ id: id, status: 'open' }, extra);\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardAttention(unittest.TestCase):
    def test_new_decision_marks_first_load_is_quiet_answer_clears(self):
        body = """
        const st0 = { pending: new Set(), flash: false };
        const first = attentionUpdate(st0, [D("D01"), D("A01"), D("D02", { status: "answered" })], [], true);   /* first load */
        const added = attentionUpdate(first, [D("D01"), D("D03"), D("A01")], ["D03", "A01"], false);          /* a new decision and a new ask */
        const quiet = attentionUpdate(first, [D("D01"), D("A01")], ["A01"], false);                           /* only an ask changed */
        const parkedNew = attentionUpdate(first, [D("D01"), D("D04", { parked: { until: "2099-01-01T00:00:00Z" } })], ["D04"], false);
        const answered = attentionUpdate(added, [D("D01"), D("D03", { status: "answered" })], ["D03"], false);
        const out = (x) => [Array.from(x.pending), x.flash];
        console.log(JSON.stringify([out(first), out(added), out(quiet), out(parkedNew), out(answered)]));"""
        for page in PAGES:
            with self.subTest(page=page):
                first, added, quiet, parked, answered = attention(page, body)
                self.assertEqual(first, [["D01"], False])             # existing decision: steady border, no flash
                self.assertEqual(added, [["D01", "D03"], True])       # new open decision flashes; the ask does not mark
                self.assertEqual(quiet, [["D01"], False])             # an ask change never flashes
                self.assertEqual(parked, [["D01"], False])            # parked decision is not open for attention
                self.assertEqual(answered, [["D01"], False])         # answering D03 clears it; no flash

    def test_css_has_frame_and_reduced_motion(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertIn("#attn-frame", src)
            self.assertRegex(src, r"prefers-reduced-motion: reduce\) \{ body\.attn-flash #attn-frame \{ animation: none")


if __name__ == "__main__":
    unittest.main()
