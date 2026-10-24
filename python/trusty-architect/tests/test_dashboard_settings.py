"""Dashboard settings pane (user state), id links, on both HTML files (node, no browser).

Test: scripts/test_dashboard_settings.py
"""
import json
import os
import re
import shutil
import subprocess
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"))
from test_dashboard_progress import PAGES  # noqa: E402


def block(page, name):
    src = open(page, encoding="utf-8").read()
    m = re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (name, name), src, re.S)
    assert m, name
    return m.group(1)


def run(page, names, body):
    code = "function txt(v){return v==null?'':typeof v==='string'?v:String(v)}\n" + "\n".join(block(page, n) for n in names) + "\n" + body
    r = subprocess.run(["node", "-e", code], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestUserStateLogic(unittest.TestCase):
    def test_payload_shape_and_default_active(self):
        body = """console.log(JSON.stringify([userStatePayload("remote","owner","2026-10-09T15:40:00Z"),
          userStateOf(null), userStateOf({}), userStateOf({user_state:"napping"}), userStateOf({user_state:"sleeping"}),
          USER_STATES.map((x) => x[0])]));"""
        for page in PAGES:
            got = run(page, ["userState"], body)
            self.assertEqual(got[0], {"user_state": "remote", "user_state_at": "2026-10-09T15:40:00Z", "user_state_by": "owner"})
            self.assertEqual(got[1:5], ["active", "active", "active", "sleeping"])
            self.assertEqual(got[5], ["active", "remote", "away", "sleeping"])


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestIdLinks(unittest.TestCase):
    def test_known_ids_link_unused_next_ids_stay_text(self):
        body = r"""
        const ids = new Set(["A36", "Da4", "Dac", "G0c"]), names = new Map([["tm-dogfood-01", "S02"]]);
        const re = new RegExp("(?<![\\w-])(?:(tm-dogfood-01)|([GPADTSR][0-9a-f]{2}))(?![\\w-])", "g");
        const t = "Next ids: A37 (200 free) · Da5; see A36, Da4. Dac and G0c/xA36 tm-dogfood-01";
        console.log(JSON.stringify(refMatches(t, re, names, ids).map((m) => [m.text, m.target])));"""
        for page in PAGES:
            got = run(page, ["refMatches"], body)
            self.assertEqual(got, [["A36", "A36"], ["Da4", "Da4"], ["Dac", "Dac"], ["G0c", "G0c"], ["tm-dogfood-01", "S02"]])

    def test_every_id_spot_goes_through_the_linker(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertIn('addText(el("span", "id"), id)', src)               # row titles
            self.assertIn('rich("span", "chip goal-chip"', src)               # session goal chip
            self.assertIn("addText(gsum,", src)                               # goals summary
            self.assertNotIn("meta.next_ids", src)                            # not a stored value


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestSleepHoursLogic(unittest.TestCase):
    def test_payload_validation(self):
        body = """console.log(JSON.stringify([sleepPayload("America/New_York","23:00"), sleepPayload("Nope/Zone","23:00").error,
          sleepPayload("UTC","").error, sleepPayload("UTC","7:00").error, sleepPayload("UTC","08:10").error]));"""
        got = run(PAGES[0], ["sleepHours"], body)
        self.assertEqual(got[0], {"payload": {"tz": "America/New_York", "sleep_start": "23:00"}})
        self.assertTrue(all(isinstance(e, str) and e for e in got[1:]))


class TestSettingsPane(unittest.TestCase):
    def test_gear_in_header_status_chip_is_a_menu_and_pane_holds_tz_sleep_and_beep(self):
        page = PAGES[0]
        src = open(page, encoding="utf-8").read()
        head = src[src.index("<header>"):src.index("</header>")]
        self.assertIn('id="settings-btn"', head)
        self.assertIn('aria-label="Settings"', head)
        self.assertNotIn("beep-toggle", head)
        self.assertRegex(head, r'<button[^>]*id="user-state-badge"[^>]*aria-haspopup="menu"')
        self.assertEqual(re.findall(r'data-state="(\w+)"', head), ["active", "remote", "away", "sleeping"])
        pane = src[src.index('<dialog id="settings-pane"'):src.index("</dialog>")]
        self.assertNotIn("user-state", pane)
        for needle in ('id="set-tz"', 'id="set-sleep-start"', 'type="time"', 'id="beep-toggle"', 'id="beep-test"'):
            self.assertIn(needle, pane)
        self.assertIn('db.doc("meta/settings")', src)
        self.assertIn("if_version", src[src.index("async function saveSettings"):src.index("async function chooseUserState")])


if __name__ == "__main__":
    unittest.main()
