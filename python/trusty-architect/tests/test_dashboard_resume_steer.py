"""Dashboard stale park, Resume, Steer and the sleep-start setting (node, no browser), on both HTML files.

Test: scripts/test_dashboard_resume_steer.py
"""
import json
import os
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import PAGES


def run(page, body):
    src = open(page, encoding="utf-8").read()
    park = re.search(r"/\* BEGIN park \*/(.*?)/\* END park \*/", src, re.S).group(1)
    js = "const txt = (v) => (v == null ? '' : String(v));\n" + park + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestResumeSteer(unittest.TestCase):
    def test_stale_park_counts_as_parked_and_shows_reason(self):
        body = """const st = { status: "open", parked: { until: null, choice: "stale", at: "t", by: "Architect", reason: "no update in 30 h" } };
        console.log(JSON.stringify([isParked(st), staleText(st), isParked({ status: "open", parked: { until: null, choice: "x" } }), isParked({ status: "open" })]));"""
        for page in PAGES:
            self.assertEqual(run(page, body), [True, "stale", False, False])   # short chip; the reason is staleReason() in the body

    def test_resume_patch_and_flag(self):
        body = """const p = resumePatch("owner", new Date("2026-10-11T04:00:00Z"));
        console.log(JSON.stringify([p, resumeRequested({ resume_request: p.resume_request }), resumeRequested({ resume_request: null })]));"""
        for page in PAGES:
            got = run(page, body)
            self.assertEqual(got[0], {"resume_request": {"at": "2026-10-11T04:00:00.000Z", "by": "owner"}, "updated": "2026-10-11T04:00:00.000Z"})
            self.assertEqual(got[1:], [True, False])

    def test_steer_appends_and_chip_tracks_relay(self):
        body = """const a = { steer_requests: [{ text: "one", at: "t0", by: "owner", relayed_at: "t1" }] };
        const p = steerPatch(a, "two", "owner", new Date("2026-10-11T04:00:00Z"));
        console.log(JSON.stringify([p.steer_requests.length, p.steer_requests[1], steerChipText(a), steerChipText({ steer_requests: p.steer_requests }), steerChipText({})]));"""
        for page in PAGES:
            got = run(page, body)
            self.assertEqual(got[0], 2)
            self.assertEqual(got[1], {"text": "two", "at": "2026-10-11T04:00:00.000Z", "by": "owner"})
            self.assertEqual(got[2:], ["steer relayed", "steer sent", ""])   # the text moved to the body

    def test_buttons_wired_on_both_pages(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            for needle in ('"Resume"', "steerControl(row, it)", "steer_requests", '"Cancel"'):
                self.assertIn(needle, src)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestSleepStart(unittest.TestCase):
    def test_single_start_input_on_15_minute_steps(self):
        src = open(PAGES[0], encoding="utf-8").read()
        pane = src[src.index('<dialog id="settings-pane"'):src.index("</dialog>")]
        self.assertIn('id="set-sleep-start" step="900"', pane)
        self.assertNotIn("sleep-end", src)
        self.assertNotIn("Sleep until", src)
        self.assertEqual(re.findall(r'<input type="time"[^>]*>', src), ['<input type="time" id="set-sleep-start" step="900">'])
        m = re.search(r"/\* BEGIN sleepHours \*/(.*?)/\* END sleepHours \*/", src, re.S).group(1)
        js = m + """console.log(JSON.stringify([sleepPayload("Europe/Rome","20:00"), sleepPayload("Europe/Rome","20:10").error, sleepPayload("Nope/Zone","20:00").error, sleepPayload("UTC","").error]));"""
        r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        got = json.loads(r.stdout)
        self.assertEqual(got[0], {"payload": {"tz": "Europe/Rome", "sleep_start": "20:00"}})
        self.assertTrue(all(isinstance(e, str) and e for e in got[1:]))

    def test_phone_width_css_keeps_inputs_inside(self):
        src = open(PAGES[0], encoding="utf-8").read()
        self.assertIn(".sp-group { border: 1px solid var(--line); border-radius: 8px; padding: 10px 16px;", src)
        self.assertIn('input[type="time"] { -webkit-appearance: none; appearance: none; display: block; max-width: 100%;', src)


if __name__ == "__main__":
    unittest.main()
