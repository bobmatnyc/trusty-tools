"""Dashboard settings pane (user state), id links and next ids, on both HTML files (node, no browser).

Test: scripts/test_dashboard_settings.py
"""
import json
import os
import re
import shutil
import subprocess
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
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
        body = """console.log(JSON.stringify([userStatePayload("remote","Owner","2026-10-09T15:40:00Z"),
          userStateOf(null), userStateOf({}), userStateOf({user_state:"napping"}), userStateOf({user_state:"sleeping"}),
          USER_STATES.map((x) => x[0])]));"""
        for page in PAGES:
            got = run(page, ["userState"], body)
            self.assertEqual(got[0], {"user_state": "remote", "user_state_at": "2026-10-09T15:40:00Z", "user_state_by": "Owner"})
            self.assertEqual(got[1:5], ["active", "active", "active", "sleeping"])
            self.assertEqual(got[5], ["active", "remote", "away", "sleeping"])


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestNextIds(unittest.TestCase):
    def test_next_is_computed_from_the_store_with_free_counts_and_wrap(self):
        body = """
        const d = (...ids) => ids.map((id) => ({ id }));
        const full = []; for (let n = 3; n < 256; n++) full.push({ id: "D" + (n < 16 ? "0" : "") + n.toString(16) });
        console.log(JSON.stringify([
          nextIds([d("A36", "Da4", "D9c", "A1", "04"), d("G0c"), [], d("S17")], ["G","P","A","D","T","S"]),
          nextIds([full], ["D"]), nextIds([full.concat(d("D00","D01","D02"))], ["D"])]));"""
        for page in PAGES:
            a, wrap, none = run(page, ["nextIds"], body)
            self.assertEqual({k: v["next"] for k, v in a.items()}, {"G": "G0d", "P": "P00", "A": "A37", "D": "Da5", "T": "T00", "S": "S18"})
            self.assertEqual(a["D"]["free"], 254 - 0)   # Da4 and D9c used
            self.assertEqual(wrap["D"], {"next": "D00", "free": 3})
            self.assertEqual(none["D"], {"next": None, "free": 0})


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
            self.assertIn("addText(note, \"Next ids: \"", src)                # next ids line
            self.assertNotIn("meta.next_ids", src)                            # not a stored value


class TestSettingsPane(unittest.TestCase):
    def test_gear_replaces_beep_in_header_and_pane_holds_state_and_beep(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            head = src[src.index("<header>"):src.index("</header>")]
            self.assertIn('id="settings-btn"', head)
            self.assertIn('aria-label="Settings"', head)
            self.assertIn('id="user-state-chip"', head)
            self.assertNotIn("beep-toggle", head)
            pane = src[src.index('<dialog id="settings-pane"'):src.index("</dialog>")]
            self.assertEqual(re.findall(r'name="user-state" value="(\w+)"', pane), ["active", "remote", "away", "sleeping"])
            for needle in ("on computer", "on device", "not engaged", "sleeping or not available", 'id="beep-toggle"', 'id="beep-test"'):
                self.assertIn(needle, pane)
            self.assertIn('db.doc("meta/settings")', src)
            self.assertIn("if_version", src[src.index("async function chooseUserState"):src.index("(function wireSettings")])


if __name__ == "__main__":
    unittest.main()
