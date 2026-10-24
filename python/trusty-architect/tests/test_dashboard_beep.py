"""Why owner heard no beep (2026-10-09): the AudioContext was created only on the toggle click, so a reload with beep saved as on left it null; a hidden tab neither polled nor beeped. Runs under node, no sound."""
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PAGES = [os.path.join(ROOT, p) for p in ("templates/dashboard/index.html",)]


def run(page, body):
    src = open(page, encoding="utf-8").read()
    m = re.search(r"/\* BEGIN reminder \*/(.*?)/\* END reminder \*/", src, re.S)
    r = subprocess.run(["node", "-e", "function bucketItems(){return{open:[]}} function askKind(){return 'D'}\n" + m.group(1) + body],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestBeepUnlock(unittest.TestCase):
    def test_context_is_created_and_resumed_by_a_gesture_not_only_the_toggle(self):
        body = """
        let created = 0, resumed = 0;
        class AC { constructor() { created++; this.state = "suspended"; } resume() { resumed++; this.state = "running"; return Promise.resolve(); } }
        const a = beepCtxEnsure(null, AC);          /* reload with beep saved on: the first gesture must build it */
        const b = beepCtxEnsure(a, AC);             /* already running: untouched */
        const sus = { state: "suspended", resume() { resumed++; return Promise.resolve(); } };
        beepCtxEnsure(sus, AC);                     /* suspended again (tab idle): resumed */
        console.log(JSON.stringify([created, resumed, a === b, a.state, beepCtxEnsure(null, undefined)]));"""
        for page in PAGES:
            self.assertEqual(run(page, body), [1, 2, True, "running", None])

    def test_beep_plays_in_a_hidden_tab(self):
        body = """
        const ctx = { state: "running", currentTime: 1, destination: {},
          createOscillator: () => ({ frequency: {}, connect() {}, start() {}, stop() {} }),
          createGain: () => ({ gain: { setValueAtTime() {}, linearRampToValueAtTime() {} }, connect() {} }) };
        console.log(JSON.stringify([beepPlay(true, ctx)]));"""
        for page in PAGES:
            self.assertEqual(run(page, body), [True])

    def test_page_wires_gesture_unlock_and_a_test_button(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertRegex(src, r'\["pointerdown", "keydown"[^\n]*addEventListener\(n, beepUnlock')
            self.assertIn('id="beep-test"', src)
            self.assertNotIn("pageHidden())", src.split("function noteFlash")[1].split("function remindTick")[0])


if __name__ == "__main__":
    unittest.main()
