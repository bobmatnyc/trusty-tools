"""Dashboard ids recycle: 2-digit hex per prefix, next free after the highest used, wrap past ff,
free count, and the oldest done items to retire so at least 64 ids stay free.

Test: scripts/test_dashboard_ids.py
"""
import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"))
import dashboard_ids as di  # noqa: E402


def put(root, coll, doc_id, **data):
    d = root / coll
    d.mkdir(exist_ok=True)
    (d / f"{doc_id}.json").write_text(json.dumps(dict(data, version=1)))


def test_next_is_max_plus_one_per_prefix_across_collections(tmp_path):
    put(tmp_path, "items", "A36", status="open")
    put(tmp_path, "items", "Da4", status="done")
    put(tmp_path, "items", "D9c", status="done")
    put(tmp_path, "goals", "G0c")
    put(tmp_path, "sessions", "S17")
    r = str(tmp_path)
    assert [di.next_id(p, r) for p in "ADGTSP"] == ["A37", "Da5", "G0d", "T00", "S18", "P00"]


def test_next_wraps_past_ff_to_lowest_free(tmp_path):
    for n in range(0x03, 0x100):
        put(tmp_path, "items", f"D{n:02x}", status="done")
    assert di.next_id("D", str(tmp_path)) == "D00"
    put(tmp_path, "items", "D00", status="done")
    assert di.next_id("D", str(tmp_path)) == "D01"
    put(tmp_path, "items", "D01", status="done"); put(tmp_path, "items", "D02", status="done")
    assert di.next_id("D", str(tmp_path)) is None


def test_free_count_ignores_other_prefixes_and_odd_ids(tmp_path):
    put(tmp_path, "items", "A01"); put(tmp_path, "items", "D01"); put(tmp_path, "items", "A1"); put(tmp_path, "items", "04")
    assert di.free_count("A", str(tmp_path)) == 255


def test_retire_candidates_oldest_done_only_until_64_free(tmp_path):
    for n in range(0x00, 0xE0):                     # 224 used, 32 free
        put(tmp_path, "items", f"A{n:02x}", status="done", done_at=f"2026-10-{1 + n // 10:02d}T00:00:{n % 60:02d}Z")
    put(tmp_path, "items", "Aff", status="open")
    put(tmp_path, "items", "Afe", status="answered")  # not done: never a candidate
    r = str(tmp_path)
    assert di.free_count("A", r) == 256 - 226
    got = di.retire_candidates("A", r, keep_free=64)
    assert len(got) == 34 and got[0] == "A00" and "Aff" not in got and "Afe" not in got
    assert got == sorted(got, key=lambda i: (int(i[1:], 16)))[:34]   # oldest done_at first (fixture is monotone)
    assert di.retire_candidates("A", r, keep_free=10) == []
