#!/usr/bin/env python3
"""ids36.py: base-34 helpers for the 2-digit id suffix of dashboard and Intent Ledger ids.

Why: the owner (see #10571): "Let's also switch to base 39 prefix letter + 2 digits to avoid duplication risks. 0-1a-z".
The Architect read that as base 36 (0-9, a-z); the owner ruled "agreed, drop" for the letters o and l (they
read as 0 and 1), so the digits are base 34: 0-9, a-k, m, n, p-z, lowercase. Two digits give 34 * 34 = 1,156 values per
prefix, 1,155 issuable (00 is never issued), instead of 256 hex values. Every old hex id is a valid base-34 id (hex has no
o or l) and keeps its string; only its numeric value changes, so sort and "next" arithmetic must go through this module,
never int(x, 16).
What: DIGITS, BASE (34), SPACE (1156), DIGIT_RE (the regex fragment for the two digits), enc(n) -> 2 digits, dec(s) -> int
(ValueError on anything but two lowercase base-34 digits; uppercase, o and l are rejected). Brief ids (B + 4 hex) are a
different scheme and do not use it. The module name is kept (ids36) from the first reading.
Test: tests/test_ids36.py
"""
import re

DIGITS = "0123456789abcdefghijkmnpqrstuvwxyz"   # base 34: 0-9 and a-z without l and o
BASE = len(DIGITS)
SPACE = BASE * BASE
DIGIT_RE = "[0-9a-km-np-z]{2}"
_VALID = re.compile(r"^[0-9a-km-np-z]{2}$")


def enc(n):
    """Two base-34 digits for 0 <= n < SPACE."""
    if not isinstance(n, int) or isinstance(n, bool) or not 0 <= n < SPACE:
        raise ValueError(f"id number out of range: {n!r}")
    return DIGITS[n // BASE] + DIGITS[n % BASE]


def dec(s):
    """Numeric value of two lowercase base-34 digits; ValueError otherwise (uppercase and wrong length are rejected)."""
    if not isinstance(s, str) or not _VALID.match(s):
        raise ValueError(f"not two base-34 digits: {s!r}")
    return DIGITS.index(s[0]) * BASE + DIGITS.index(s[1])


def next_digits(last):
    """The digits after `last` in base-34 order, skipping 00 and wrapping zz -> 01."""
    return enc(dec(last) % (SPACE - 1) + 1)


if __name__ == "__main__":
    import sys
    if len(sys.argv) == 3 and sys.argv[1] == "next":
        try:
            print(next_digits(sys.argv[2]))
        except ValueError as e:
            sys.exit(f"ids36: {e}")
    else:
        sys.exit("usage: ids36.py next <two digits>   (0-9 and a-z without o and l; e.g. 0f -> 0g, 0z -> 10)")
