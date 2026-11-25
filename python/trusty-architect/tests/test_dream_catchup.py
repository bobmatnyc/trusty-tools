"""dream_catchup: transcript filters, answer classification and uptake, credential scrub, the 06:30 clock and the
transcript directory read from local/architect.json.

#10571 S2: the pin's alert case went through fleet-poll.py's dream_alerts (S3); here it drives due() directly.
Test: tests/test_dream_catchup.py
Rome is UTC+1 on 2026-11-02 (CET).
"""
import importlib.util
import json
import os
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
# #10571 S2: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
_spec = importlib.util.spec_from_file_location("dream_catchup", os.path.join(SCRIPTS, "dream_catchup.py"))
dc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(dc)
SINCE = "2026-10-29T10:00:00Z"


def utc(s):
    return datetime.fromisoformat(s).replace(tzinfo=timezone.utc).timestamp()


def turn(text, ts, **kw):
    r = {"type": "user", "timestamp": ts, "origin": {"kind": "human"}, "message": {"content": text}}
    r.update(kw)
    return json.dumps(r)


def make_transcripts(tmp):
    d = tmp / "t"
    d.mkdir()
    lines = [
        turn("keep me", "2026-10-29T11:00:00Z"),
        turn("too old", "2026-10-29T09:00:00Z"),
        turn("[poll 11:05Z] 2 new alerts", "2026-10-29T11:05:00Z"),
        turn("<system-reminder>x</system-reminder>", "2026-10-29T11:06:00Z"),
        turn("meta text", "2026-10-29T11:07:00Z", isMeta=True),
        turn("agent text", "2026-10-29T11:08:00Z", isSidechain=True),
        turn("from a task", "2026-10-29T11:09:00Z", origin={"kind": "task-notification"}),
        turn("Resume from the Architect handoff block", "2026-10-29T11:10:00Z"),
        json.dumps({"type": "user", "timestamp": "2026-10-29T11:11:00Z", "origin": {"kind": "human"},
                    "message": {"content": [{"type": "tool_result", "content": "x"}]}}),
        turn("use ghp_" + "a" * 30 + " for it", "2026-10-29T11:12:00Z"),
        turn("ab " * 140, "2026-10-29T11:13:00Z"),
        "not json",
    ]
    (d / "s.jsonl").write_text("\n".join(lines) + "\n")
    return str(d)


def item(answer, rec_label="A", answered="2026-10-29T12:00:00Z"):
    return {"answer": answer, "answered_at": answered, "title": "t", "kind": "question",
            "options": [{"label": "A", "recommended": rec_label == "A"}, {"label": "B", "recommended": rec_label == "B"}]}


def make_store(tmp):
    root = tmp / "owner-queue"
    (root / "items").mkdir(parents=True)
    docs = {"D01": item("A"), "D02": item("A"), "D03": item("B"), "D04": item("my own words"),
            "D05": item("", answered=None), "D06": item("A", answered="2026-10-01T00:00:00Z")}
    for k, v in docs.items():
        (root / "items" / f"{k}.json").write_text(json.dumps({"version": 1, **v}))
    arch = tmp / "owner-queue-archive"
    arch.mkdir()
    (arch / "pruned.jsonl").write_text(json.dumps({"collection": "items", "id": "D07", "data": item("B", rec_label="B")}) + "\n")
    return str(root)


def run(tmp, capsys):
    out = tmp / "out"
    log = tmp / "dream-log.md"
    log.write_text("2026-10-29T10:0xZ pass\n")
    rc = dc.main(["--out", str(out), "--root", make_store(tmp), "--transcripts", make_transcripts(tmp), "--log", str(log)])
    assert rc == 0
    capsys.readouterr()
    return next(out.glob("*-digest.md")).read_text()


def test_prompts_filtered_scrubbed_and_truncated(tmp_path, capsys):
    text = run(tmp_path, capsys)
    assert "Count: 2 (credential-like prompts dropped: 1)" in text
    assert "keep me" in text and "ghp_" not in text and "too old" not in text
    for skipped in ("poll", "system-reminder", "meta text", "agent text", "from a task", "Resume from"):
        assert skipped not in text
    long = next(ln for ln in text.splitlines() if "ab ab" in ln)
    assert long.endswith("...") and long.count("ab") <= 101


def test_answers_uptake_and_overrides(tmp_path, capsys):
    text = run(tmp_path, capsys)
    assert "Count: 5. took-rec 3, other option 1, free-text 1." in text      # D07 comes from pruned.jsonl
    assert "Uptake (took-rec / all answers): 60%. Of option picks only: 75%." in text
    assert "D03 [other]" in text and "D04 [free]" in text and "D01 [" not in text and "D07 [" not in text


def test_dream_due_fires_once_at_0630_local(tmp_path):
    (tmp_path / "meta").mkdir()
    (tmp_path / "meta" / "settings.json").write_text(json.dumps({"version": 1, "tz": "Europe/Rome"}))
    state = {}
    due = lambda when: dc.due(state, utc(when), str(tmp_path))  # noqa: E731
    assert due("2026-11-02T05:29:59") is None                               # 06:29:59 Rome
    assert due("2026-11-02T05:30:00") == {"date": "2026-11-02", "time": "06:30", "tz": "Europe/Rome"}
    assert due("2026-11-02T09:00:00") is None                               # same day: not again
    assert json.loads(json.dumps(state))["dream"]["last"] == "2026-11-02"   # survives a restart
    assert due("2026-11-03T05:30:00")["date"] == "2026-11-03"


def test_default_transcripts_come_from_the_config(tmp_path):
    # #10571 S2: <claude_config_dir>/projects/<slug of the checkout>; no home path in the script.
    got = dc.default_transcripts(environ={"CLAUDE_CONFIG_DIR": str(tmp_path / "cfg")})
    assert got == os.path.join(str(tmp_path / "cfg"), "projects", "".join(
        c if c.isascii() and c.isalnum() else "-" for c in dc.ROOT))
