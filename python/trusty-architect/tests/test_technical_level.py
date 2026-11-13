"""Technical level setting, per-user and per-item levels, the hook line and the warn-only level lint (#11064).

Ported from the prototype's scripts/test_technical_level.py (commit ab5a222), lint cases included.
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
import level_lint as ll  # noqa: E402
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


@pytest.mark.parametrize("prompt", ["[SECONDARY USER Ana Lopez via slack] status?",
                                    "[SECONDARY USER ] status?", "[SECONDARY USER ana] status?"])
def test_secondary_marker_with_an_unparsed_name_never_takes_the_owner_level(tmp_path, monkeypatch, prompt):
    hook = _hook(monkeypatch, tmp_path, owner_level="cto")   # #11064 ruling (d): pm, never the owner's cto
    out = ctx(hook({"hook_event_name": "UserPromptSubmit", "prompt": prompt}, 1001.0))
    levels = [x for x in out if x.startswith("TECH LEVEL: ")]
    assert levels and all(x.startswith("TECH LEVEL: pm (") for x in levels), levels
    assert tl.speaker(prompt) is not None


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


# ---- level lint (#11064 TL-11 to TL-13): flags at pm and below, warns, never blocks ----

LINT_CASES = [
    ("path", "see scripts/foo.py:12-14 for it", "scripts/foo.py:12-14"),
    ("path", "edit config.py today", "config.py"),
    ("sha", "fixed in 1a2b3c4d", "1a2b3c4d"),
    ("flag", "pass --force to it", "--force"),
    ("envvar", "set TRUSTY_MPM_FLAG first", "TRUSTY_MPM_FLAG"),
    ("job", "the job 'build-linux' failed", "job 'build-linux'"),
    ("tool", "run cargo now", "cargo"),
    ("identifier", "the `slot_role` value", "`slot_role`"),
]


@pytest.mark.parametrize("rule,text,match", LINT_CASES)
def test_each_lint_rule_flags_below_engineer_and_is_silent_from_engineer_up(rule, text, match):
    for level in ("non-technical", "vibe-coder", "pm"):
        assert {"rule": rule, "match": match, "line": 1} in ll.lint(text, level)
    for level in ("engineer", "senior-engineer", "cto"):
        assert ll.lint(text, level) == []


def test_lint_allows_issue_refs_urls_and_clean_text_at_pm():
    ok = ("Decision on #123 and example-org/example-repo#45, see https://example.com/a/b/pull/9 and https://x.io/a/b.py. "
          "The decade facade is fine.")
    assert ll.lint(ok, "pm") == []
    assert ll.lint("", "pm") == [] and ll.lint(None, "pm") == []


def test_lint_unknown_level_is_pm_and_line_numbers_count_newlines():
    assert ll.lint("fine\nbut --force here", "wizard") == [{"rule": "flag", "match": "--force", "line": 2}]
    assert ll.lint("x --force", None) != []


def test_lint_cli_warns_with_exit_zero_and_strict_exits_one(tmp_path, capsys):
    assert ll.main(["--level", "pm", "see", "a/b.py:3"]) == 0
    assert "path `a/b.py:3`" in capsys.readouterr().out
    assert ll.main(["--level", "pm", "--strict", "see", "a/b.py:3"]) == 1
    assert ll.main(["--level", "senior-engineer", "--strict", "a/b.py:3"]) == 0
    assert ll.main(["--level", "pm", "--strict", "clean words only"]) == 0
    assert ll.main(["--level", "bogus", "--strict", "--", "--force"]) == 1   # unknown level: pm rules apply
    assert "unknown level" in capsys.readouterr().err
    f = tmp_path / "t.txt"
    f.write_text("line one\nsha 1a2b3c4d\n")
    assert ll.main(["--level", "pm", "--file", str(f)]) == 0
    assert "sha `1a2b3c4d` (line 2)" in capsys.readouterr().out


# The spec's worked example (section 3), at pm: decision, cost, risk, reversibility; no paths, SHAs or flags.
PM_ITEM = {
    "kind": "question", "status": "open", "task": "T01", "created": "2026-11-11T00:00:00Z",
    "title": "What should a server copy do when it cannot tell whether it is the live one?",
    "detail": ("Today it assumes live and keeps syncing to Notion, so a typo or blank setting can cause two copies to "
               "write at once. Recommended: B now, C as a follow-up."),
    "options": [{"label": "A", "desc": "Keep today's behaviour."},
                {"label": "B (Recommended)", "desc": "Only an explicit \"active\" counts as live; low effort, no deploy change."},
                "C: also treat a missing setting as idle in production; needs a deploy check first."],
}
EVIDENCE = " Evidence: config.py:1994-2045."


def test_pm_fixture_item_is_clean_and_file_line_evidence_is_flagged_only_below_engineer():
    texts = [t for _, t in ll.item_texts(PM_ITEM)]
    assert len(texts) == 7 and all(ll.lint(t, "pm") == [] for t in texts)
    detail = PM_ITEM["detail"] + EVIDENCE
    assert ll.lint(detail, "pm") == [{"rule": "path", "match": "config.py:1994-2045", "line": 1}]
    assert ll.lint(detail, "senior-engineer") == [] and ll.lint(detail, "cto") == []


def _create(root, item):
    assert db.main(["--root", str(root), "create", "items", "D", "--json", json.dumps(item)]) == 0
    return max(p.stem for p in (root / "items").glob("*.json"))


def test_item_writes_with_findings_still_commit_and_warn_on_every_path(tmp_path, capsys):
    put_settings(tmp_path, technical_level="pm")
    doc_id = _create(tmp_path, dict(PM_ITEM, detail=PM_ITEM["detail"] + EVIDENCE))
    err = capsys.readouterr().err
    assert f"warning: level_lint [pm]: item {doc_id} detail: path `config.py:1994-2045`" in err
    assert "config.py:1994-2045" in ds.Store(str(tmp_path)).get("items", doc_id)["data"]["detail"]   # committed
    assert db.main(["--root", str(tmp_path), "update", "items", doc_id, "--json", '{"title": "Fix b/c.rs:2?"}']) == 0
    assert "title: path `b/c.rs:2`" in capsys.readouterr().err
    ops = tmp_path / "ops.json"
    ops.write_text(json.dumps([{"action": "update", "collection": "items", "id": doc_id,
                                "data": {"options": [{"label": "Yes", "desc": "pass --force"}]}}]))
    assert db.main(["--root", str(tmp_path), "batch", str(ops)]) == 0
    assert "options[0].desc: flag `--force`" in capsys.readouterr().err
    data = ds.Store(str(tmp_path)).get("items", doc_id)["data"]
    assert data["title"] == "Fix b/c.rs:2?" and data["options"][0]["desc"] == "pass --force"


def test_pm_fixture_item_write_is_quiet(tmp_path, capsys):
    put_settings(tmp_path, technical_level="pm")
    _create(tmp_path, PM_ITEM)
    assert "level_lint" not in capsys.readouterr().err


def test_item_write_is_quiet_at_senior_engineer_and_under_a_senior_override(tmp_path, capsys):
    put_settings(tmp_path, technical_level="senior-engineer")
    _create(tmp_path, dict(PM_ITEM, detail=PM_ITEM["detail"] + EVIDENCE))
    assert "level_lint" not in capsys.readouterr().err
    put_settings(tmp_path, technical_level="pm")
    doc_id = _create(tmp_path, dict(PM_ITEM, level_override="senior-engineer"))
    capsys.readouterr()
    assert db.main(["--root", str(tmp_path), "update", "items", doc_id, "--json", json.dumps({"detail": EVIDENCE})]) == 0
    assert "level_lint" not in capsys.readouterr().err   # the stored override sets the level, not the owner's pm


def test_a_lint_exception_never_aborts_the_write(tmp_path, capsys, monkeypatch):
    put_settings(tmp_path, technical_level="pm")

    def boom(*a, **k):
        raise RuntimeError("lint broke")
    monkeypatch.setattr(ll, "lint", boom)
    doc_id = _create(tmp_path, dict(PM_ITEM, detail=PM_ITEM["detail"] + EVIDENCE))
    assert "level lint skipped: RuntimeError('lint broke')" in capsys.readouterr().err
    assert ds.Store(str(tmp_path)).get("items", doc_id)["data"]["detail"].endswith(EVIDENCE.strip())
    monkeypatch.setattr(ll, "item_level", boom)
    assert db.main(["--root", str(tmp_path), "update", "items", doc_id, "--json", '{"title": "still saved"}']) == 0
    assert ds.Store(str(tmp_path)).get("items", doc_id)["data"]["title"] == "still saved"
