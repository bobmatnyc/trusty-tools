"""dashboard_schema: the schemas in schemas/dashboard/ accept the migrated fixture store and reject one bad value per enum.

Why: owner (2026-10-11 04:12Z) wants every dashboard structure described by a schema; the vendored validator must agree
with the real `jsonschema` library on every case here (compared when that library is installed).
Test: scripts/test_dashboard_schema.py
"""
import copy
import glob
import json
import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_schema as sch  # noqa: E402

T = "2026-10-11T04:00:00Z"
GOAL = {"kind": "directed", "rank": 1, "title": "t", "lifecycle": "active", "updated": T}
ITEM = {"kind": "question", "status": "open", "title": "t", "session": "s", "goal": "G01", "rank": 1, "created": T, "updated": T}
SESSION = {"name": "tm-a", "state": "idle", "status": "x", "summary": "y", "updated": T}


def test_every_schema_file_loads_and_uses_supported_keywords():
    names = sorted(os.path.basename(p)[:-12] for p in glob.glob(os.path.join(sch.SCHEMA_DIR, "*.schema.json")))
    assert names == sorted(["items", "sessions", "goals", "goal_reorders", "goal_removals", "step_progress", "accounts",
                            "effort_tunes", "il_steers", "briefs", "questions", "media", "meta-state", "meta-settings", "meta-seen", "meta-gh_track"])
    for n in names:
        sch.load_schema(n)


def test_unsupported_keyword_is_refused(tmp_path):
    (tmp_path / "x.schema.json").write_text(json.dumps({"type": "object", "patternProperties": {}}))
    with pytest.raises(sch.SchemaError):
        sch.load_schema("x", str(tmp_path))


CASES = [  # (coll, id, doc, expected error count > 0?)
    ("items", "D01", ITEM, False),
    ("items", "D01", {**ITEM, "goal": None}, False),
    ("items", "D01", {**ITEM, "goal": "Goal 3 · Acme"}, True),
    ("items", "D01", {**ITEM, "status": "maybe"}, True),
    ("items", "D01", {**ITEM, "kind": "idea"}, True),
    ("items", "D01", {**ITEM, "rank": True}, True),
    ("items", "D01", {**ITEM, "due": ""}, True),
    ("items", "D01", {**ITEM, "due": "2026-10-15"}, False),
    ("items", "D01", {**ITEM, "id": "D01"}, True),
    ("items", "D01", {**ITEM, "parked": {"choice": "later", "at": T, "by": "owner"}}, True),
    ("items", "D01", {**ITEM, "parked": {"choice": "stale", "until": None, "at": T, "by": "owner"}}, False),
    ("items", "D01", {**ITEM, "options": [{"label": "a", "desc": "b", "markers": {"risk": 4}}]}, True),
    ("items", "D01", {**ITEM, "steer_requests": [{"text": "x", "at": T}]}, True),
    ("items", "D01", {**ITEM, "resume_request": None, "status_request": {"at": T}}, False),
    ("sessions", "S01", SESSION, False),
    ("sessions", "S01", {**SESSION, "state": "asleep"}, True),
    ("sessions", "S01", {**SESSION, "order": "0"}, True),
    ("sessions", "S01", {**SESSION, "command": {"type": "reboot", "status": "new", "at": T}}, True),
    ("sessions", "S01", {**SESSION, "command": {"type": "restart", "status": "new", "at": T}}, False),
    ("goals", "G01", GOAL, False),
    ("goals", "G01", {**GOAL, "lifecycle": "gone"}, True),
    ("goals", "G01", {**GOAL, "kind": "factory"}, True),
    ("goals", "G01", {**GOAL, "kind": "factory", "board": {"label": "b", "url": "u"}, "repo": "o/r"}, False),
    ("goals", "G01", {**GOAL, "repo": "not a repo"}, True),
    ("goals", "G01", {**GOAL, "lifecycle": "parked"}, True),
    ("goals", "G01", {**GOAL, "lifecycle": "parked", "parked": {"until": None, "choice": "owner", "at": T, "by": "B"}, "park_ack": None}, False),
    ("goals", "G01", {**GOAL, "status": "free text"}, True),
    ("meta", "settings", {"user_state": "remote", "sleep_start": "20:00", "tz": "Europe/Rome"}, False),
    ("meta", "settings", {"user_state": "dozing"}, True),
    ("meta", "settings", {"sleep_start": "8pm"}, True),
    ("meta", "state", {"owner_name": "owner", "updated": T, "next_ids": {"A": "01"}}, True),
    ("meta", "seen", {"map": {"D01": "sig", "D02": ["a", "b"]}}, False),
    ("meta", "other", {}, True),
    ("briefs", "2026-10-11-x", {"question": "q", "answer": "a", "project": "architect", "status": "open", "created": T, "updated": T}, False),
    ("briefs", "2026-10-11-x", {"question": "q", "answer": "a", "project": "architect", "status": "open", "created": T, "updated": T,
                                "doc": "docs/briefs/2026-10-11-x.md", "goal": "G01", "session": "S01"}, False),
    ("briefs", "2026-10-11-x", {"question": "q", "answer": "a", "project": "architect", "status": "open", "created": T, "updated": T,
                                "doc": "../secret.txt"}, True),
    ("briefs", "2026-10-11-x", {"question": "q", "answer": "a", "project": "architect", "status": "open", "created": T, "updated": T,
                                "doc": "local/briefs/x.md"}, True),
    ("accounts", "a", {"account": "a", "pct5h": 101, "pctwk": 1, "ts": T, "source": "manual"}, True),
    ("goal_reorders", "x", {"at": T, "kind": "directed", "order": ["G01", "x"], "status": "new"}, True),
]


@pytest.mark.parametrize("coll,doc_id,doc,bad", CASES)
def test_cases(coll, doc_id, doc, bad):
    errs = sch.validate(coll, doc_id, doc)
    assert bool(errs) == bad, errs
    if errs:
        assert all(e.startswith(f"{coll}/{doc_id}: ") for e in errs)


@pytest.mark.parametrize("coll,doc_id,doc,bad", [c for c in CASES if c[0] != "meta" or c[1] != "other"])
def test_agrees_with_jsonschema_library(coll, doc_id, doc, bad):
    jsonschema = pytest.importorskip("jsonschema")
    schema = sch.schema_for(coll, doc_id)
    lib_bad = not jsonschema.Draft202012Validator(schema).is_valid(doc)
    mine = bool(sch.check_schema(schema, doc))
    code_only = bad and not mine and not lib_bad          # invariant checks that JSON Schema cannot express
    assert mine == lib_bad or code_only


def test_cross_references():
    snap = {"goals": [{"id": "G01", "data": {**GOAL}}],
            "items": [{"id": "D01", "data": {**ITEM, "goal": "G09"}}],
            "step_progress": [{"id": "X", "data": {"item": "D99", "updated": T, "steps": {}}}]}
    errs = sch.validate_store(snap)
    assert any("G09 is not a goal id" in e for e in errs) and any("D99 is not an item id" in e for e in errs)


def test_mode_env():
    assert sch.mode({}) is None and sch.mode({"DASHBOARD_SCHEMA": "enforce"}) == "enforce"
    with pytest.raises(sch.SchemaError):
        sch.mode({"DASHBOARD_SCHEMA": "loud"})
