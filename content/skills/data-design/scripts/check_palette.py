#!/usr/bin/env python3
"""Offline palette checker for the data-design skill (#10401).

Why: chart palettes must stay distinct under colour-vision deficiency, sit in
one lightness band, and contrast with the real surface in both themes. This
checker runs locally with the Python standard library only: no network, no
third-party packages, no dependency on any other tool's validator.

What: for each colour it computes OKLCH (Ottosson's OKLab), simulates protan,
deutan and tritan vision (Machado, Oliveira, Fernandes 2009, severity 1.0, in
linear sRGB), and measures WCAG 2.x contrast against `--surface`.

  categorical (default): every colour's OKLCH L inside `--band`; every pair
    at least `--min-normal` apart (OKLab distance x100) with normal vision and
    `--min-cvd` apart under each simulation; every colour at least
    `--min-contrast` against the surface (WCAG 1.4.11, 3:1).
  sequential: OKLCH L strictly monotonic in the given order, and each
    adjacent pair at least `--min-step` apart under normal vision and every
    simulation.

Exit 0: every check passed. Exit 1: at least one FAIL line. Exit 2: bad input.

Test: scripts/tests/test_data_design_palette.py
Usage: check_palette.py [--surface HEX] [--mode categorical|sequential] HEX...
"""

import argparse
import itertools
import json
import math
import re
import sys

HEX_RE = re.compile(r"^#?([0-9a-fA-F]{6}|[0-9a-fA-F]{3})$")

# Machado, Oliveira, Fernandes (2009), severity 1.0, applied to linear sRGB.
CVD_MATRICES = {
    "protan": (
        (0.152286, 1.052583, -0.204868),
        (0.114503, 0.786281, 0.099216),
        (-0.003882, -0.048116, 1.051998),
    ),
    "deutan": (
        (0.367322, 0.860646, -0.227968),
        (0.280085, 0.672501, 0.047413),
        (-0.011820, 0.042940, 0.968881),
    ),
    "tritan": (
        (1.255528, -0.076749, -0.178779),
        (-0.078411, 0.930809, 0.147602),
        (0.004733, 0.691367, 0.303900),
    ),
}


def parse_hex(text):
    """Return (r, g, b) in 0..1 sRGB for `#rrggbb` or `#rgb`; raise ValueError."""
    match = HEX_RE.match(text.strip())
    if not match:
        raise ValueError(f"not a hex colour: {text!r}")
    digits = match.group(1)
    if len(digits) == 3:
        digits = "".join(c * 2 for c in digits)
    return tuple(int(digits[i : i + 2], 16) / 255.0 for i in (0, 2, 4))


def to_linear(c):
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def linear_rgb(rgb):
    return tuple(to_linear(c) for c in rgb)


def oklab_from_linear(lin):
    """Ottosson's OKLab from linear sRGB."""
    r, g, b = lin
    l_ = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b
    m_ = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b
    s_ = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b
    l_, m_, s_ = (math.copysign(abs(v) ** (1 / 3), v) for v in (l_, m_, s_))
    return (
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    )


def oklch(rgb):
    """(L, C, h-degrees) for an sRGB colour."""
    lab_l, a, b = oklab_from_linear(linear_rgb(rgb))
    return lab_l, math.hypot(a, b), math.degrees(math.atan2(b, a)) % 360


def simulate(lin, kind):
    """Linear sRGB as seen with full-severity `kind` deficiency, clamped."""
    m = CVD_MATRICES[kind]
    return tuple(min(1.0, max(0.0, sum(m[i][j] * lin[j] for j in range(3)))) for i in range(3))


def delta_e(lin_a, lin_b):
    """OKLab Euclidean distance, scaled x100."""
    return 100 * math.dist(oklab_from_linear(lin_a), oklab_from_linear(lin_b))


def luminance(rgb):
    r, g, b = linear_rgb(rgb)
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def contrast(rgb_a, rgb_b):
    """WCAG 2.x contrast ratio, 1..21."""
    hi, lo = sorted((luminance(rgb_a), luminance(rgb_b)), reverse=True)
    return (hi + 0.05) / (lo + 0.05)


def pair_distances(lins, pairs):
    """Yield (i, j, vision, dE) for each pair under normal and CVD vision."""
    for i, j in pairs:
        yield i, j, "normal", delta_e(lins[i], lins[j])
        for kind in CVD_MATRICES:
            yield i, j, kind, delta_e(simulate(lins[i], kind), simulate(lins[j], kind))


def check(colours, surface="#ffffff", mode="categorical", band=(0.48, 0.67),
          min_normal=15.0, min_cvd=8.0, min_contrast=3.0, min_step=5.0):
    """Return a list of result dicts: {ok, check, detail}. Raises ValueError on bad hex."""
    if len(colours) < 2:
        raise ValueError("need at least two colours")
    rgbs = [parse_hex(c) for c in colours]
    surf = parse_hex(surface)
    lins = [linear_rgb(c) for c in rgbs]
    results = []

    def add(ok, name, detail):
        results.append({"ok": ok, "check": name, "detail": detail})

    if mode == "categorical":
        for name, rgb in zip(colours, rgbs):
            lightness = oklch(rgb)[0]
            add(band[0] <= lightness <= band[1], "lightness-band",
                f"{name} L={lightness:.3f} band={band[0]:.2f}-{band[1]:.2f}")
            ratio = contrast(rgb, surf)
            add(ratio >= min_contrast, "surface-contrast",
                f"{name} vs {surface} {ratio:.2f}:1 (min {min_contrast}:1)")
        for i, j, vision, de in pair_distances(lins, itertools.combinations(range(len(rgbs)), 2)):
            floor = min_normal if vision == "normal" else min_cvd
            add(de >= floor, f"distance-{vision}",
                f"{colours[i]} / {colours[j]} dE={de:.1f} (min {floor})")
    elif mode == "sequential":
        ls = [oklch(rgb)[0] for rgb in rgbs]
        diffs = [b - a for a, b in zip(ls, ls[1:])]
        monotonic = all(d > 0 for d in diffs) or all(d < 0 for d in diffs)
        add(monotonic, "lightness-monotonic", "L=" + " ".join(f"{v:.3f}" for v in ls))
        adjacent = [(k, k + 1) for k in range(len(rgbs) - 1)]
        for i, j, vision, de in pair_distances(lins, adjacent):
            add(de >= min_step, f"step-{vision}",
                f"{colours[i]} -> {colours[j]} dE={de:.1f} (min {min_step})")
    else:
        raise ValueError(f"unknown mode: {mode}")
    return results


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("colours", nargs="+", help="hex colours in display order")
    parser.add_argument("--surface", default="#ffffff", help="background hex (default #ffffff)")
    parser.add_argument("--mode", choices=("categorical", "sequential"), default="categorical")
    parser.add_argument("--band", nargs=2, type=float, default=(0.48, 0.67), metavar=("LO", "HI"))
    parser.add_argument("--min-normal", type=float, default=15.0)
    parser.add_argument("--min-cvd", type=float, default=8.0)
    parser.add_argument("--min-contrast", type=float, default=3.0)
    parser.add_argument("--min-step", type=float, default=5.0)
    parser.add_argument("--json", action="store_true", help="print results as JSON")
    args = parser.parse_args(argv)
    try:
        results = check(args.colours, args.surface, args.mode, tuple(args.band),
                        args.min_normal, args.min_cvd, args.min_contrast, args.min_step)
    except ValueError as err:
        print(f"error: {err}", file=sys.stderr)
        return 2
    failed = [r for r in results if not r["ok"]]
    if args.json:
        print(json.dumps({"ok": not failed, "results": results}, indent=2))
    else:
        for r in results:
            print(f"{'PASS' if r['ok'] else 'FAIL'} {r['check']}: {r['detail']}")
        print(f"{len(results) - len(failed)} passed, {len(failed)} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
