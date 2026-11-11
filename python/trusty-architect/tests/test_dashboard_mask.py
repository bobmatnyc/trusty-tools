"""Pane-tail masker: dashboard_mask.mask_pane_text (#11065 phase 1).

Covers: one real-shaped fake secret per shape, with no window of the secret left in the output or the marker;
non-secret pane text unchanged; ANSI/OSC stripping; the line and length caps; a PEM block cut by the line cap or
by the capture window; regression cases the old _scrub rules leak.
Every fake secret is assembled at run time, so no literal in this file matches a secret scanner or the leak gate.
Run: python3 -m pytest tests/test_dashboard_mask.py
Test: tests/test_dashboard_mask.py
"""
import base64
import json
import os
import random
import string
import sys
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "scripts"))
import dashboard_harness as dh  # noqa: E402
import dashboard_mask as dm  # noqa: E402

_rng = random.Random(11065)
B64 = string.ascii_letters + string.digits + "+/"
ALNUM = string.ascii_letters + string.digits


def rand(n, alphabet=ALNUM):
    return "".join(_rng.choice(alphabet) for _ in range(n))


def jwt():
    seg = lambda obj: base64.urlsafe_b64encode(json.dumps(obj).encode()).decode().rstrip("=")  # noqa: E731
    return seg({"alg": "HS256", "typ": "JWT"}) + "." + seg({"sub": rand(10), "iat": 1792576800}) + "." + rand(43)


def pem_lines(kind="RSA PRIVATE KEY", body=8):
    """A PEM block as a list of lines: BEGIN, body base64 lines (64 wide, the last shorter), END."""
    rows = [rand(64, B64) for _ in range(body - 1)] + [rand(20, B64) + "=="]
    return ["-----BEGIN " + kind + "-----"] + rows + ["-----END " + kind + "-----"]


def windows(secret, n=6):
    """Every n-character window of a secret: none may survive into the output."""
    return {secret[i:i + n] for i in range(max(1, len(secret) - n + 1))}


GH = "gh" + "p_" + rand(36)
GHO = "gh" + "o_" + rand(36)
PAT = "github" + "_pat_" + rand(22) + "_" + rand(59)
SK = "s" + "k-ant-api03-" + rand(40, ALNUM + "-_")
SK_PROJ = "s" + "k-proj-" + rand(48)
XOXB = "xo" + "xb-" + rand(11, string.digits) + "-" + rand(13, string.digits) + "-" + rand(24)
XOXP = "xo" + "xp-" + rand(11, string.digits) + "-" + rand(24)
AKIA = "AK" + "IA" + rand(16, string.ascii_uppercase + string.digits)
ASIA = "AS" + "IA" + rand(16, string.ascii_uppercase + string.digits)
GOOG = "AI" + "za" + rand(35, ALNUM + "-_")
JWT = jwt()
PW = rand(18)
BEARER = rand(40, ALNUM + "-._")

# (shape, pane line holding the secret, the secret value, text that must survive around it)
FIXTURES = [
    ("github-token ghp_", "export GH=" + GH + " # set", GH, "# set"),
    ("github-token gho_", "token value " + GHO + " end", GHO, "end"),
    ("github-token github_pat_", "pat " + PAT + " ok", PAT, "ok"),
    ("api-key sk-ant-", "using " + SK + " now", SK, "using"),
    ("api-key sk-proj-", "key " + SK_PROJ + ".", SK_PROJ, "key "),
    ("slack xoxb-", "slack " + XOXB + " posted", XOXB, "posted"),
    ("slack xoxp-", "user " + XOXP + " posted", XOXP, "posted"),
    ("aws AKIA", "aws " + AKIA + " region", AKIA, "region"),
    ("aws ASIA", "sts " + ASIA + " region", ASIA, "region"),
    ("google AIza", "maps " + GOOG + " done", GOOG, "done"),
    ("jwt", "cookie session " + JWT + " expires", JWT, "expires"),
    ("bearer", "curl -H 'X: Bearer " + BEARER + "' https://api.example.com", BEARER, "https://api.example.com"),
    ("authorization header", "Authorization: Basic " + base64.b64encode(("u:" + PW).encode()).decode(),
     base64.b64encode(("u:" + PW).encode()).decode(), "Authorization: "),
    ("url credentials", "DATABASE=postgres://admin:" + PW + "@db.example.com:5432/app", PW, "@db.example.com:5432/app"),
    ("url credentials https", "git push https://bot:" + PW + "@git.example.com/repo.git", PW, "git push https://"),
    ("password=", "PASSWORD=" + PW + " ./run", PW, "PASSWORD="),
    ("password:", "password: " + PW, PW, "password: "),
    ("secret= quoted", 'CLIENT_SECRET="' + PW + ' x"', PW, "CLIENT_SECRET="),
    ("api key: with space", "API key: " + PW, PW, "API key: "),
    ("json api_key", '{"api_key": "' + PW + '", "n": 1}', PW, '"n": 1}'),
    ("token in query", "GET /cb?code=1&access_token=" + PW + "&state=2", PW, "GET /cb?code=1&"),
    ("unarmored base64 body", "  " + rand(64, B64), None, None),
]


class MaskShapes(unittest.TestCase):
    def mask(self, text, lines=50, width=1000):
        return dm.mask_pane_text(text, max_lines=lines, max_line_len=width)

    def test_every_fixture_secret_is_masked_with_no_window_left(self):
        for shape, line, secret, keep in FIXTURES:
            with self.subTest(shape=shape):
                secret = secret or line.strip()
                out = self.mask("before\n" + line + "\nafter")
                for w in windows(secret):
                    self.assertNotIn(w, out, f"{shape}: a window of the secret survived: {out!r}")
                self.assertIn("[masked:", out)
                self.assertTrue(out.startswith("before\n") and out.endswith("\nafter"), out)
                if keep:
                    self.assertIn(keep, out, f"{shape}: the text around the secret was lost")

    def test_marker_carries_only_the_shape_name(self):
        out = self.mask("a " + GH + " b " + JWT + " c")
        self.assertEqual(out, "a [masked:gh-token] b [masked:jwt] c")

    def test_pem_block_is_one_marker_and_keeps_surrounding_lines(self):
        block = pem_lines()
        out = self.mask("$ cat id_rsa\n" + "\n".join(block) + "\n$ echo done")
        self.assertEqual(out, "$ cat id_rsa\n[masked:pem]\n$ echo done")

    def test_pgp_and_ec_armor_are_masked(self):
        for kind in ("EC PRIVATE KEY", "OPENSSH PRIVATE KEY", "PGP PRIVATE KEY BLOCK", "PRIVATE KEY"):
            with self.subTest(kind=kind):
                block = pem_lines(kind)
                out = self.mask("x\n" + "\n".join(block) + "\ny")
                self.assertEqual(out, "x\n[masked:pem]\ny")

    def test_a_secret_split_by_ansi_codes_is_still_masked(self):
        line = GH[:6] + "\x1b[1m" + GH[6:] + "\x1b[0m"
        out = self.mask(line)
        self.assertEqual(out, "[masked:gh-token]")


class MaskPem(unittest.TestCase):
    def test_line_cap_cutting_a_pem_block_leaks_no_body_line(self):
        block = pem_lines(body=20)
        text = "\n".join(["l1", "l2", "l3"] + block + ["tail1", "tail2"])
        out = dm.mask_pane_text(text, max_lines=6, max_line_len=200)
        for row in block[1:-1]:
            for w in windows(row, 8):
                self.assertNotIn(w, out)
        self.assertEqual(out, "l1\nl2\nl3\n[masked:pem]\ntail1\ntail2")
        # The raw last 6 lines are body rows; masking runs before the cap, so the cap never exposes one.
        out = dm.mask_pane_text(text, max_lines=3, max_line_len=200)
        self.assertEqual(out, "[masked:pem]\ntail1\ntail2")

    def test_capture_starting_mid_block_masks_up_to_end(self):
        block = pem_lines(body=12)
        text = "\n".join(block[5:] + ["$ ls"])
        out = dm.mask_pane_text(text, max_lines=50, max_line_len=200)
        self.assertEqual(out, "[masked:pem]\n$ ls")

    def test_capture_ending_mid_block_masks_to_the_end(self):
        block = pem_lines(body=12)
        text = "\n".join(["$ cat key.pem"] + block[:6])
        out = dm.mask_pane_text(text, max_lines=50, max_line_len=200)
        self.assertEqual(out, "$ cat key.pem\n[masked:pem]")

    def test_input_past_the_cap_drops_its_partial_first_line(self):
        text = "x " + GH + "\n" + "y" * (dm.INPUT_MAX - 10) + "\nlast"
        out = dm.mask_pane_text(text, max_lines=5, max_line_len=10)
        self.assertNotIn(GH[-4:], out)
        self.assertEqual(out.split("\n")[-1], "last")


class MaskText(unittest.TestCase):
    def test_non_secret_pane_text_survives_unchanged(self):
        text = "\n".join([
            "⏺ Bash(cargo test -p trusty-mpm --no-fail-fast)",
            "  ⎿  test result: ok. 68 passed; 0 failed; 0 ignored",
            "commit 3f9a1c2b7d4e5f60718293a4b5c6d7e8f9012345 (HEAD -> feat/x)",
            "sha256 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "/tmp/work/src/main.rs:42:7: warning: unused variable `key`",
            "✻ Thinking… (12s · ↓ 1.2k tokens · esc to interrupt)",
            "risk-assessment task-runner keyboard=us",
            "\tindented with a tab; trailing spaces   ",
            "see https://docs.example.com/guide#token-lifetime for details",
            "",
            "> ",
        ])
        self.assertEqual(dm.mask_pane_text(text, max_lines=50, max_line_len=500), text)

    def test_ansi_and_osc_are_stripped(self):
        text = ("\x1b[31mred\x1b[0m \x1b]8;;https://example.com/p\x07link\x1b]8;;\x07 "
                "\x1b]0;title\x1b\\\x1b[?25l\x1b(Bplain\x1b7\x07\rdone\r\n")
        out = dm.mask_pane_text(text, max_lines=10, max_line_len=200)
        self.assertEqual(out, "red link plain\ndone\n")
        self.assertNotIn("\x1b", out)

    def test_line_cap_keeps_the_last_lines(self):
        text = "\n".join(f"line {i}" for i in range(100))
        out = dm.mask_pane_text(text, max_lines=10, max_line_len=200)
        self.assertEqual(out.split("\n"), [f"line {i}" for i in range(90, 100)])
        self.assertEqual(dm.mask_pane_text(text, max_lines=0, max_line_len=10), "")

    def test_length_cap_cuts_with_a_visible_marker(self):
        out = dm.mask_pane_text("a" * 300 + "\nshort", max_lines=5, max_line_len=80)
        first, second = out.split("\n")
        self.assertEqual(first, "a" * 80 + dm.TRUNC)
        self.assertEqual(second, "short")

    def test_length_cap_inside_a_marker_drops_the_fragment(self):
        out = dm.mask_pane_text("abc " + GH + " tail", max_lines=5, max_line_len=10)
        self.assertEqual(out, "abc " + dm.TRUNC)

    def test_bad_caps_are_rejected(self):
        with self.assertRaises(ValueError):
            dm.mask_pane_text("x", max_lines=-1, max_line_len=10)
        with self.assertRaises(ValueError):
            dm.mask_pane_text("x", max_lines=1, max_line_len=0)
        with self.assertRaises(TypeError):
            dm.mask_pane_text("x", max_lines="5", max_line_len=10)

    def test_a_long_dashed_run_masks_in_linear_time(self):
        text = "a-" * (dm.INPUT_MAX // 2)
        t0 = time.monotonic()
        dm.mask_pane_text(text, max_lines=5, max_line_len=50)
        self.assertLess(time.monotonic() - t0, 5.0)


class CriticRegression(unittest.TestCase):
    """code-critic repros on b2453c580a (#11065): each leaked there."""

    def mask(self, text):
        return dm.mask_pane_text(text, max_lines=50, max_line_len=1000)

    def test_mid_key_line_starting_with_slash_is_masked(self):
        row = "/" + rand(63, B64)
        out = self.mask("MIIEvQIBADANBgkq\n" + row + "\n" + rand(64, B64))
        for w in windows(row, 8):
            self.assertNotIn(w, out)
        self.assertIn("[masked:base64]", out)

    def test_word_segment_paths_and_identifiers_stay_readable(self):
        paths = [
            "/" + "Users" + "/" + "ma" + "sa" + "/trusty-mpm-projects/" + "bo" + "bmatnyc" + "/trusty-tools/cr" + "ates/x.rs",
            "/opt/WorkSpace/ProjectAlpha/srcFiles/MainModule/Handler2024.rs",
            "src/Components/Dashboard/DecisionPanel/LastOutputView/index.ts",
            "TestShouldCalculateTotalWhenItemsAddedAndTaxApplied2024",
        ]
        for p in paths:
            with self.subTest(path=p):
                self.assertEqual(self.mask("open " + p + " ok"), "open " + p + " ok")

    def test_quoted_value_with_escaped_quotes_is_masked_to_its_close(self):
        out = self.mask('{"password": "a\\"b c", "n": 1}')
        self.assertEqual(out, '{"password": [masked:secret], "n": 1}')

    def test_unquoted_multi_word_value_is_masked_to_end_of_line(self):
        for key, value in (("password: ", "hunter two words"), ("SECRET=", "correct horse battery"),
                           ("token = ", "abc def")):
            with self.subTest(key=key):
                self.assertEqual(self.mask(key + value + "\nnext"), key + "[masked:secret]\nnext")


class OldScrubRegression(unittest.TestCase):
    """Shapes dashboard_harness._scrub (the _TOKENS + _SECRET rules) lets through and mask_pane_text masks."""

    PEM = pem_lines()
    CASES = [
        ("pem body", "\n".join(PEM), PEM[1]),
        ("jwt", "cookie " + JWT, JWT),
        ("url credentials", "postgres://admin:" + PW + "@db.example.com/app", PW),
        ("authorization basic", "Authorization: Basic " + PW, PW),
        ("aws sts", "sts " + ASIA, ASIA),
        ("google key", "maps " + GOOG, GOOG),
    ]

    def test_old_scrub_leaks_and_the_masker_does_not(self):
        for name, text, s in self.CASES:
            with self.subTest(case=name):
                self.assertIn(s, dh._scrub(text), f"{name}: the old rules already mask it; not a regression case")
                self.assertNotIn(s, dm.mask_pane_text(text, max_lines=50, max_line_len=1000))


if __name__ == "__main__":
    unittest.main()
