"""Why #11429: a Keychain prompt blocked `tm secrets set`, the dashboard said "Not stored", and the write landed once the owner approved the prompt. A time-out or an unknown outcome must not claim failure. Runs the page's own saveFailureText under node."""
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PAGE = os.path.join(ROOT, "templates/dashboard/index.html")


def texts(errors):
    src = open(PAGE, encoding="utf-8").read()
    m = re.search(r"/\* BEGIN saveFailure \*/(.*?)/\* END saveFailure \*/", src, re.S)
    assert m, "the page has no saveFailure block"
    body = "const errs = %s;\nconsole.log(JSON.stringify(errs.map(saveFailureText)));" % json.dumps(errors)
    r = subprocess.run(["node", "-e", m.group(1) + body], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestSecretSaveOutcome(unittest.TestCase):
    def test_a_timeout_or_unknown_outcome_never_says_not_stored(self):
        unknown = [
            {"message": "The Keychain did not answer", "status": 504, "code": "keychain_access_prompt"},
            {"message": "x", "status": 504, "code": "backend_timeout"},
            {"message": "tm secrets set timed out after 30 s", "status": 500},
            {"message": "Failed to fetch"},
        ]
        for text in texts(unknown):
            self.assertNotIn("Not stored", text)
            self.assertIn("tm secrets list", text)
            self.assertIn("unknown", text.lower())

    def test_a_real_refusal_keeps_its_own_wording(self):
        refusal = {"message": "The value was refused.", "status": 422, "code": "invalid_value"}
        self.assertEqual(texts([refusal]), ["Not stored: The value was refused."])


if __name__ == "__main__":
    unittest.main()
