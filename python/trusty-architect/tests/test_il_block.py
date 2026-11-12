"""#11133 phase 1 (DOC-89 v1): task_tag.parse_block, the structured `il` block parser, and parse_handback.

Fixtures: tests/fixtures/il_block/. Each reject class has its own fixture and asserts the violation it names.
Run: python3 -m pytest tests/test_il_block.py
"""
import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "scripts"))
import task_tag  # noqa: E402

FIX = os.path.join(HERE, "fixtures", "il_block")
SHARED = {"id", "stamp", "done", "est", "remaining", "pct"}
# Built at run time, never stored: an `sk-` key shape dashboard_mask names "api-key".
SECRET = "sk-" + "a1B2c3D4" * 3


def fixture(name):
    with open(os.path.join(FIX, name), encoding="utf-8") as f:
        return f.read().replace("{SECRET}", SECRET)


def test_valid_block_parses_every_field():
    kind, rec = task_tag.parse_block(fixture("valid.md"))
    assert kind == "ok", rec
    assert (rec["id"], rec["stamp"], rec["pct"], rec["goal"]) == ("tt-T2f", "1112", 60, "tt-G01")
    assert (rec["done"], rec["est"], rec["remaining"]) == ("wrote the v1 grammar into DOC-89", "20m",
                                                           "validators, fixtures")
    assert rec["next"] == "run check_sld.sh"
    assert rec["open"] == ["tt-D4e", "tt-A1c"]
    assert rec["new"] == [{"type": "D", "title": "Which session is exempt?", "decided": None,
                           "opts": [{"l": "Architect", "rec": True}, {"l": "none", "rec": False}]}]
    assert rec["evidence"] == ["head=docs/11133-il-block-spec@b59075b7b4", "check_sld.sh => 0 errors; EXIT=0"]
    assert (rec["source"], rec["error"], rec["feedback"]) == ("block", None, None)


@pytest.mark.parametrize("name, want", [
    ("reject_unknown_key.md", "unknown key 'status'"),
    ("reject_duplicate_singleton.md", "repeated key 'pct'"),
    ("reject_missing_required.md", "missing required key 'next'"),
    ("reject_bad_id.md", "goal: bad id 'TT-G01'"),
    ("reject_pct_out_of_range.md", "pct must be an integer 0-100, got '140'"),
    ("reject_secret.md", "secret-shaped span (api-key)"),
])
def test_each_reject_class_names_its_first_violation(name, want):
    kind, reason = task_tag.parse_block(fixture(name))
    assert kind == "reject", reason
    assert want in reason, reason


def test_a_secret_is_rejected_and_never_echoed():
    kind, reason = task_tag.parse_handback(fixture("reject_secret.md"))
    assert kind == "reject"
    assert SECRET not in reason and SECRET[3:11] not in reason


@pytest.mark.parametrize("body, want", [
    ("goal: tt-G01\ntask: tt-T2f\ndone: x\npct: 5\nnext: none", "task: want <id>@MMDD"),
    ("goal: tt-G01\ntask: tt-D2f@1112\ndone: x\npct: 5\nnext: none", "task: bad id 'tt-D2f'"),
    ("goal: tt-G01\ntask: tt-T2f@1112\ndone: x\npct: 5\nnext: none\nopen: tt-T01", "open: bad id 'tt-T01'"),
    ("goal: tt-G01\ntask: tt-T2f@1112\ndone: x\npct: 40%\nnext: none", "pct must be an integer"),
    ("goal: tt-G01\ntask: tt-T2f@1112\ndone: " + "d" * 201 + "\npct: 5\nnext: none", "done is 201 characters"),
    ("goal: tt-G01\ntask: tt-T2f@1112\ndone: x\npct: 5\nnext: none\nfree prose here", "not 'key: value'"),
    ("goal: tt-G01\ntask: tt-T2f@1112\ndone: x\npct: 5\nnext: none\nnew: Q what? | opts: a", "new: want"),
])
def test_value_rules_reject(body, want):
    kind, reason = task_tag.parse_block(f"```il\n{body}\n```")
    assert kind == "reject" and want in reason, reason


def test_an_unclosed_or_second_block_is_rejected():
    assert task_tag.parse_block("```il\ngoal: tt-G01\n")[1] == "il block has no closing fence"
    one = fixture("valid.md")
    assert task_tag.parse_block(one + "\n" + one)[1] == "more than one il block"


def test_block_wins_over_a_legacy_line():
    kind, rec = task_tag.parse_handback(fixture("block_and_legacy.md"))
    assert (kind, rec["source"], rec["id"], rec["pct"]) == ("ok", "block", "tt-T2f", 60)


def test_legacy_line_only_falls_back_to_parse_pulse():
    text = fixture("legacy_only.md")
    assert task_tag.parse_block(text) == ("none", None)
    kind, rec = task_tag.parse_handback(text)
    assert (kind, rec) == task_tag.parse_pulse(text)
    assert (kind, rec["id"], rec["pct"]) == ("ok", "ar-T0b", 40)


def test_no_il_at_all_is_none():
    text = fixture("no_il.md")
    assert task_tag.parse_block(text) == ("none", None)
    assert task_tag.parse_handback(text) == ("none", None)


def test_both_parsers_share_one_record_shape():
    _, block = task_tag.parse_block(fixture("valid.md"))
    _, legacy = task_tag.parse_pulse(fixture("legacy_only.md"))
    assert SHARED <= set(block) and SHARED <= set(legacy)
    assert block["kind"] == legacy["kind"] == "il"


def test_pct_is_clamped_by_parse_pulse_and_rejected_by_parse_block():
    assert task_tag.parse_pulse("Task: T93 | pct: 140")[1]["pct"] == 100
    assert task_tag.parse_block(fixture("reject_pct_out_of_range.md"))[0] == "reject"
