"""Base-34 id digits (the owner: "0-1a-z", then "agreed, drop" for o and l; see #10571): the alphabet, the
f->g, k->m, n->p and z->10 boundaries, the validator and the sort order of mixed old and new ids.

#10571 S2: the pin's cases for dashboard_derive, dashboard_ids, datg and task_tag ship when those modules move to
base 34 (the S4 re-merge); this slice ships ids36.py alone.
Test: python3 -m pytest -q tests/test_ids36.py
"""
import pytest

import ids36


def test_alphabet_is_base_34_without_o_and_l():
    assert ids36.DIGITS == "0123456789abcdefghijkmnpqrstuvwxyz" and ids36.SPACE == 1156
    assert "o" not in ids36.DIGITS and "l" not in ids36.DIGITS


@pytest.mark.parametrize("a,b", [("0f", "0g"), ("0k", "0m"), ("0n", "0p"), ("0z", "10"), ("zy", "zz")])
def test_enc_dec_boundaries_are_consecutive(a, b):
    assert ids36.dec(b) == ids36.dec(a) + 1 and ids36.enc(ids36.dec(a)) == a and ids36.enc(ids36.dec(b)) == b


@pytest.mark.parametrize("bad", ["G7", "0O", "o1", "1l", "00x", "7", "", "A1", "ZZ", None])
def test_dec_rejects(bad):
    with pytest.raises(ValueError):
        ids36.dec(bad)


def test_sort_of_mixed_old_and_new_ids_is_base_34_and_matches_string_order():
    ids = ["zz", "10", "0g", "ff", "0f", "a9", "2f", "g7", "0m", "0k"]
    by_num = sorted(ids, key=ids36.dec)
    assert by_num == sorted(ids)   # fixed-width lowercase digits sort the same as strings
    assert by_num == ["0f", "0g", "0k", "0m", "10", "2f", "a9", "ff", "g7", "zz"]
    # int(x, 16) would fail on 'g7' and order 'ff' above 'a9' only by luck; the base-34 value of 'ff' is 525
    assert ids36.dec("ff") == 15 * 34 + 15


@pytest.mark.parametrize("last,want", [("0f", "0g"), ("0z", "10"), ("zz", "01")])
def test_next_digits_skips_00_and_wraps(last, want):
    # #10571 S2: next_digits had no pin case; it is the `ids36.py next` CLI.
    assert ids36.next_digits(last) == want
