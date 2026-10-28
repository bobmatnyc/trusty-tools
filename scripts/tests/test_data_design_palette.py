"""Tests for content/skills/data-design/scripts/check_palette.py (#10401).

Stdlib unittest, so it runs with or without pytest:
  python3 -m unittest scripts/tests/test_data_design_palette.py
  python3 -m pytest scripts/tests/test_data_design_palette.py -q -p no:cacheprovider
"""

import ast
import importlib.util
import subprocess
import sys
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
TOOL = REPO / "content/skills/data-design/scripts/check_palette.py"

_spec = importlib.util.spec_from_file_location("check_palette", TOOL)
cp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cp)


def run(*args):
    return subprocess.run([sys.executable, "-I", str(TOOL), *args],
                          capture_output=True, text=True, check=False)


def failed_checks(results):
    return {r["check"] for r in results if not r["ok"]}


class ColourMath(unittest.TestCase):
    def test_parse_hex_accepts_long_and_short_forms(self):
        self.assertEqual(cp.parse_hex("#ffffff"), (1.0, 1.0, 1.0))
        self.assertEqual(cp.parse_hex("000"), (0.0, 0.0, 0.0))
        with self.assertRaises(ValueError):
            cp.parse_hex("#12345")

    def test_oklch_white_and_black_endpoints(self):
        self.assertAlmostEqual(cp.oklch(cp.parse_hex("#ffffff"))[0], 1.0, places=4)
        self.assertAlmostEqual(cp.oklch(cp.parse_hex("#000000"))[0], 0.0, places=4)
        self.assertLess(cp.oklch(cp.parse_hex("#808080"))[1], 1e-4)  # grey has no chroma

    def test_wcag_contrast_reference_values(self):
        white, black = cp.parse_hex("#fff"), cp.parse_hex("#000")
        self.assertAlmostEqual(cp.contrast(white, black), 21.0, places=2)
        # WebAIM reference: #777777 on white is 4.48:1.
        self.assertAlmostEqual(cp.contrast(cp.parse_hex("#777777"), white), 4.48, places=2)

    def test_cvd_simulation_collapses_red_green_but_not_blue_yellow(self):
        red, green = (cp.linear_rgb(cp.parse_hex(h)) for h in ("#ff0000", "#00aa00"))
        self.assertGreater(cp.delta_e(red, green), 15)
        self.assertLess(cp.delta_e(cp.simulate(red, "deutan"), cp.simulate(green, "deutan")), 8)
        blue, yellow = (cp.linear_rgb(cp.parse_hex(h)) for h in ("#0000ff", "#ffff00"))
        self.assertGreater(cp.delta_e(cp.simulate(blue, "deutan"), cp.simulate(yellow, "deutan")), 8)

    def test_simulation_keeps_greys_grey(self):
        grey = cp.linear_rgb(cp.parse_hex("#808080"))
        for kind in cp.CVD_MATRICES:
            sim = cp.simulate(grey, kind)
            self.assertAlmostEqual(max(sim) - min(sim), 0.0, places=2)


class Checks(unittest.TestCase):
    def test_dark2_subset_passes_categorical_on_light_surface(self):
        results = cp.check(["#1b9e77", "#d95f02", "#7570b3"], surface="#fcfcfb")
        self.assertEqual(failed_checks(results), set())

    def test_red_green_pair_fails_cvd_distance(self):
        self.assertIn("distance-deutan", failed_checks(cp.check(["#ff0000", "#00aa00"])))

    def test_pale_colour_fails_band_and_contrast(self):
        failed = failed_checks(cp.check(["#deebf7", "#1b9e77"], surface="#ffffff"))
        self.assertTrue({"lightness-band", "surface-contrast"} <= failed)

    def test_sequential_blues_pass_and_shuffled_fail(self):
        blues = ["#deebf7", "#9ecae1", "#3182bd"]
        self.assertEqual(failed_checks(cp.check(blues, mode="sequential")), set())
        shuffled = ["#9ecae1", "#deebf7", "#3182bd"]
        self.assertIn("lightness-monotonic", failed_checks(cp.check(shuffled, mode="sequential")))

    def test_check_rejects_a_single_colour(self):
        with self.assertRaises(ValueError):
            cp.check(["#000000"])


class Cli(unittest.TestCase):
    def test_exit_codes(self):
        self.assertEqual(run("--surface", "#fcfcfb", "#1b9e77", "#d95f02").returncode, 0)
        failing = run("#ff0000", "#00aa00")
        self.assertEqual(failing.returncode, 1)
        self.assertIn("FAIL distance-deutan", failing.stdout)
        self.assertEqual(run("nothex", "#000000").returncode, 2)

    def test_json_output(self):
        out = run("--json", "#1b9e77", "#d95f02")
        self.assertIn('"ok"', out.stdout)


class StdlibOnly(unittest.TestCase):
    def test_imports_only_stdlib_and_no_network_modules(self):
        tree = ast.parse(TOOL.read_text())
        names = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                names.update(a.name.split(".")[0] for a in node.names)
            elif isinstance(node, ast.ImportFrom):
                names.add(node.module.split(".")[0])
        self.assertTrue(names <= set(sys.stdlib_module_names), names - set(sys.stdlib_module_names))
        self.assertFalse(names & {"socket", "urllib", "http", "ssl", "subprocess", "ftplib"}, names)


if __name__ == "__main__":
    unittest.main()
