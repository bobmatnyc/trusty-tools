"""Technical level setting, per-user and per-item levels, and the hook line (#11064).

Ported from the prototype's scripts/test_technical_level.py; the level lint cases ship with level_lint.py in a later PR.
Test: tests/test_technical_level.py
"""
import importlib.util
import json
import os
import sys

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
if SCRIPTS not in sys.path:
    sys.path.insert(0, SCRIPTS)
import dashboard_store as ds  # noqa: E402
import pending_hook_dedupe as ph  # noqa: E402
import technical_level as tl  # noqa: E402

spec = importlib.util.spec_from_file_location("dashboard_db", os.path.join(SCRIPTS, "dashboard-db.py"))
db = importlib.util.module_from_spec(spec)
spec.loader.exec_module(db)
SCHEMAS = os.path.join(os.path.dirname(SCRIPTS), "schemas", "dashboard")


def put_settings(root, **data):
    d = root / "meta"
    d.mkdir(exist_ok=True)
    (d / "settings.json").write_text(json.dumps(dict(data, version=1)))


def settings(root):
    return json.loads((root / "meta" / "settings.json").read_text())


# ---- setting: table, schema, read, refuse ----

def test_rules_table_covers_exactly_the_levels_and_schema_enums_match():
    assert set(tl.RULES) == set(tl.LEVELS) and tl.DEFAULT == "pm"
    meta = json.load(open(os.path.join(SCHEMAS, "meta-settings.schema.json")))
    items = json.load(open(os.path.join(SCHEMAS, "items.schema.json")))
    assert tuple(meta["properties"]["technical_level"]["enum"]) == tl.LEVELS
    assert tuple(items["properties"]["level_override"]["enum"]) == tl.LEVELS


@pytest.mark.parametrize("value", [None, "wizard", "PM", "", 3, ["pm"]])
def test_read_unset_or_unknown_value_falls_back_to_pm(tmp_path, value):
    assert tl.read(str(tmp_path)) == "pm"   # no settings doc
    data = {"user_state": "active"} if value is None else {"technical_level": value}
    put_settings(tmp_path, **data)
    assert tl.read(str(tmp_path)) == "pm"


def test_read_returns_each_set_level(tmp_path):
    for level in tl.LEVELS:
        put_settings(tmp_path, technical_level=level)
        assert tl.read(str(tmp_path)) == level


def test_settings_write_accepts_a_level_and_refuses_an_invalid_one(tmp_path, capsys):
    put_settings(tmp_path, user_state="active")
    assert db.main(["--root", str(tmp_path), "update", "meta", "settings", "--json", '{"technical_level":"engineer"}']) == 0
    assert tl.read(str(tmp_path)) == "engineer"
    capsys.readouterr()
    assert db.main(["--root", str(tmp_path), "update", "meta", "settings", "--json", '{"technical_level":"wizard"}']) == 2
    assert "technical_level" in capsys.readouterr().err
    assert tl.read(str(tmp_path)) == "engineer"   # the refused write changed nothing
    ops = tmp_path / "ops.json"
    ops.write_text(json.dumps([{"action": "update", "collection": "meta", "id": "settings", "data": {"technical_level": "x"}}]))
    assert db.main(["--root", str(tmp_path), "batch", str(ops)]) == 2
    assert tl.read(str(tmp_path)) == "engineer"


def test_store_refuses_an_invalid_level_on_every_write_path(tmp_path):
    store = ds.Store(str(tmp_path), fmt="dir", schema_mode="off")   # the page's PUT/PATCH/batch reach the store directly
    store.set("meta", "settings", {"user_state": "active"}, 0)
    for write in (lambda: store.update("meta", "settings", {"technical_level": "guru"}),
                  lambda: store.set("meta", "settings", {"technical_level": None}, 1),
                  lambda: store.batch([{"op": "update", "collection": "meta", "id": "settings", "data": {"technical_level": "CTO"}}])):
        with pytest.raises(ds.StoreError, match="technical_level"):
            write()
    assert store.get("meta", "settings")["data"] == {"user_state": "active"}
    store.update("meta", "settings", {"user_state": "away"})   # a write without the key is never refused on it
    store.update("meta", "settings", {"technical_level": "cto"})
    assert tl.read(str(tmp_path)) == "cto"


# ---- per-item override ----

def test_item_level_override_leaves_the_setting_unchanged(tmp_path):
    put_settings(tmp_path, technical_level="pm")
    before = settings(tmp_path)
    item = {"kind": "question", "status": "open", "title": "Pick one", "task": "T01", "created": "2026-11-11T00:00:00Z"}
    assert db.main(["--root", str(tmp_path), "create", "items", "D", "--json", json.dumps(item)]) == 0
    doc_id = next(p.stem for p in (tmp_path / "items").glob("*.json"))
    assert db.main(["--root", str(tmp_path), "update", "items", doc_id, "--json", '{"level_override": "engineer"}']) == 0
    stored = ds.Store(str(tmp_path)).get("items", doc_id)["data"]
    assert tl.for_item(stored, str(tmp_path)) == "engineer"
    assert settings(tmp_path) == before and tl.read(str(tmp_path)) == "pm"
    assert db.main(["--root", str(tmp_path), "update", "items", doc_id, "--json", '{"level_override": "wizard"}']) == 2
    assert ds.Store(str(tmp_path)).get("items", doc_id)["data"]["level_override"] == "engineer"
    assert tl.for_item({"title": "no override"}, str(tmp_path)) == "pm"


# ---- secondary users ----

def test_secondary_user_level_from_registry_then_profile_never_the_owner(tmp_path):
    owner = tmp_path / "owner-queue"
    owner.mkdir()
    put_settings(owner, technical_level="cto")
    users = tmp_path / "users"
    users.mkdir()
    (users / "registry.json").write_text(json.dumps({"enabled": True, "users": [
        {"name": "ana", "technical_level": "vibe-coder"}, {"name": "bo", "technical_level": "guru"}]}))
    (users / "cy.md").write_text("# The user: cy\n\n## Identity\n\n- Technical level: senior-engineer\n")
    (users / "dee.md").write_text("- Technical level: <non-technical | vibe-coder | pm>\n")
    assert tl.read_user("ana", str(users)) == "vibe-coder"
    assert tl.read_user("bo", str(users)) == "pm"   # an invalid registry value fails to pm, not to the owner's cto
    assert tl.read_user("cy", str(users)) == "senior-engineer"
    assert tl.read_user("dee", str(users)) == "pm"   # the template placeholder is unset
    assert tl.read_user("nobody", str(users)) == "pm"
    assert tl.read_user("../owner-queue/meta/settings", str(users)) == "pm"
    assert tl.read(str(owner)) == "cto"


def test_speaker_names_only_the_secondary_marker():
    assert tl.speaker("[SECONDARY USER ana via slack req r1] hi") == "ana"
    assert tl.speaker("hello") is None and tl.speaker(None) is None
    assert not ph.owner_prompt({"hook_event_name": "UserPromptSubmit", "prompt": "[SECONDARY USER ana via slack req r1] hi"})


# ---- hook line ----

def test_level_line_set_unset_and_throttle(tmp_path):
    st = str(tmp_path / "ph.json")
    assert ph.level_line(st, "s", "engineer", now=1000.0) == tl.line("engineer")
    assert tl.line("engineer").startswith("TECH LEVEL: engineer (")
    assert ph.level_line(st, "s", "engineer", now=1100.0) is None   # same level inside the window
    assert ph.level_line(st, "s", "cto", now=1200.0) == tl.line("cto")   # a change speaks at once
    assert ph.level_line(st, "s", "cto", now=1200.0 + ph.REMIND_SECS - 1) is None
    assert ph.level_line(st, "s", "cto", now=1200.0 + ph.REMIND_SECS) == tl.line("cto")   # reminder after the window
    assert ph.level_line(st, "s2", None, now=1.0) == "TECH LEVEL: pm (" + tl.RULES["pm"] + ")"   # unset: pm


def test_level_line_has_the_user_state_timing(tmp_path):
    st, lv = str(tmp_path / "us.json"), str(tmp_path / "tl.json")
    for t, state, level in [(0, "away", "pm"), (10, "away", "pm"), (20, "active", "cto"), (919, "active", "cto"),
                            (920, "active", "cto"), (921, "active", "cto")]:
        said_state = ph.decide([], st, "s", now=float(t), user_state=state) is not None
        said_level = ph.level_line(lv, "s", level, now=float(t)) is not None
        assert said_state == said_level, t


def test_level_line_keeps_user_state_entry(tmp_path):
    st = str(tmp_path / "ph.json")
    assert ph.decide([], st, "s", now=1000.0, user_state="away") == "USER STATE: away"
    ph.level_line(st, "s", "pm", now=1001.0)
    assert ph.decide([], st, "s", now=1002.0, user_state="away") is None   # us/us_last survived the level write


def _hook(monkeypatch, tmp_path, owner_level="senior-engineer"):
    import owner_answers
    import pm_cursor
    qroot = tmp_path / "local" / "owner-queue"
    qroot.mkdir(parents=True)
    put_settings(qroot, technical_level=owner_level)
    monkeypatch.setattr(ph, "ROOT", str(tmp_path))
    monkeypatch.setattr(ph, "STATE", str(tmp_path / "ph.json"))
    monkeypatch.setattr(tl, "USERS_DIR", str(tmp_path / "local" / "users"))
    monkeypatch.setenv("ARCHITECT_OWNER_TURNS", str(tmp_path / "turns.jsonl"))
    monkeypatch.setattr(owner_answers, "pending", lambda *a, **k: [])
    monkeypatch.setattr(pm_cursor, "inject_block", lambda *a, **k: "")
    return lambda d, now: ph.build_output(dict(d, session_id="s"), now=now)


def ctx(out):
    return out["hookSpecificOutput"]["additionalContext"].splitlines() if out else []


def test_build_output_prints_the_owner_level_once_then_on_change(tmp_path, monkeypatch):
    hook = _hook(monkeypatch, tmp_path)
    assert tl.line("senior-engineer") in ctx(hook({"hook_event_name": "PostToolUse"}, 1000.0))
    assert ctx(hook({"hook_event_name": "PostToolUse"}, 1001.0)) == []   # deduped
    put_settings(tmp_path / "local" / "owner-queue", technical_level="cto")
    assert tl.line("cto") in ctx(hook({"hook_event_name": "UserPromptSubmit", "prompt": "hi"}, 1002.0))


def test_build_output_uses_the_secondary_users_own_level_for_the_turn(tmp_path, monkeypatch):
    hook = _hook(monkeypatch, tmp_path)
    users = tmp_path / "local" / "users"
    users.mkdir()
    (users / "ana.md").write_text("- Technical level: non-technical\n")
    hook({"hook_event_name": "PostToolUse"}, 1000.0)
    out = ctx(hook({"hook_event_name": "UserPromptSubmit", "prompt": "[SECONDARY USER ana via slack req r1] status?"}, 1001.0))
    assert tl.line("non-technical", "ana") in out
    assert ctx(hook({"hook_event_name": "PostToolUse"}, 1002.0)) == []   # same reader for the turn's tool calls
    assert not (tmp_path / "turns.jsonl").exists()   # a secondary user's message is not an owner turn
    assert tl.line("senior-engineer") in ctx(hook({"hook_event_name": "UserPromptSubmit", "prompt": "owner again"}, 1003.0))


def test_build_output_survives_a_level_read_error(tmp_path, monkeypatch, capsys):
    hook = _hook(monkeypatch, tmp_path)

    def boom(root=None):
        raise ValueError("bad store")
    monkeypatch.setattr(tl, "read", boom)
    assert hook({"hook_event_name": "PostToolUse"}, 1000.0) is None
    assert "technical level line failed" in capsys.readouterr().err


def test_cli_prints_the_owner_and_a_user_level(tmp_path, monkeypatch, capsys):
    put_settings(tmp_path, technical_level="engineer")
    monkeypatch.setattr(tl, "USERS_DIR", str(tmp_path))
    (tmp_path / "ana.md").write_text("- Technical level: cto\n")
    assert tl.main(["--root", str(tmp_path)]) == 0 and capsys.readouterr().out == "engineer\n"
    assert tl.main(["--user", "ana"]) == 0 and capsys.readouterr().out == "cto\n"
