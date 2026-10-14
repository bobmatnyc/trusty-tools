"""fleet-poll.py owner_answer alerts: once per answer, re-answer re-alerts, Relayed is quiet,
cancel alerts, a store read error never crashes the poll. Also pending-answers.py exit codes."""
import importlib.util
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(HERE, "fleet-poll.py"))
fp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fp)
_ps = importlib.util.spec_from_file_location("pending_answers", os.path.join(HERE, "pending-answers.py"))
pa = importlib.util.module_from_spec(_ps)
_ps.loader.exec_module(pa)


def put(root, item_id, **data):
    d = root / "items"
    d.mkdir(exist_ok=True)
    (d / f"{item_id}.json").write_text(json.dumps({"version": 1, "data": data}))


def poll(state, root, now=1000):
    new, _ = fp.dedup(state, fp.owner_answer_alerts(now, str(root)), now)
    return new


def test_new_answer_alerts_once_and_reanswer_alerts_again(tmp_path):
    put(tmp_path, "D4d", status="answered", answer="Restart now (after the sample)",
        answered_at="2026-10-09T00:17:41.078Z")
    st = {}
    new = poll(st, tmp_path)
    assert len(new) == 1 and new[0]["kind"] == "owner_answer"
    assert new[0]["display"] == "[owner] D4d answered 00:17Z: Restart now (after the sample)"
    assert poll(st, tmp_path, 1100) == []
    put(tmp_path, "D4d", status="answered", answer="Wait", answered_at="2026-10-09T02:00:00Z")
    assert len(poll(st, tmp_path, 1200)) == 1


def test_relayed_does_not_alert(tmp_path):
    put(tmp_path, "D4d", status="answered", answer="x", answered_at="2026-10-09T00:17:41Z",
        answer_note="Relayed 01:29Z (late).")
    put(tmp_path, "D4e", status="open")
    assert poll({}, tmp_path) == []


def test_cancelled_alerts_and_long_answer_truncated(tmp_path):
    put(tmp_path, "D50", status="cancelled", cancelled_at="2026-10-09T03:00:00Z")
    put(tmp_path, "D51", status="answered", answer="y" * 200, answered_at="2026-10-09T03:01:00Z")
    new = {a["display"].split()[1]: a for a in poll({}, tmp_path)}
    assert "cancelled 03:00Z" in new["D50"]["display"]
    assert len(new["D51"]["display"]) < 120


def test_store_read_error_does_not_crash(tmp_path):
    f = tmp_path / "items"
    f.write_text("not a directory")          # listdir raises NotADirectoryError
    assert fp.owner_answer_alerts(1000, str(tmp_path)) == []


def test_pending_answers_exit_codes(tmp_path, capsys):
    assert pa.main(["--root", str(tmp_path)]) == 0
    put(tmp_path, "D4d", status="answered", answer="Go", answered_at="2026-10-09T00:17:41Z")
    assert pa.main(["--root", str(tmp_path)]) == 1
    assert "D4d" in capsys.readouterr().out


def fb_alerts(state, root, now=1000):
    new, _ = fp.dedup(state, fp.owner_feedback_alerts(now, str(root)), now)
    return new


def test_feedback_alerts_once_per_feedback_and_applied_is_ignored(tmp_path):
    put(tmp_path, "A11", status="open", feedback={"to": "question", "note": "I approve this, not do it " + "x" * 100,
        "at": "2026-10-09T02:00:00Z", "status": "new"})
    put(tmp_path, "D60", status="open", feedback={"to": "task", "note": "", "at": "2026-10-09T02:01:00Z",
        "status": "applied", "applied_at": "2026-10-09T02:05:00Z"})
    st = {}
    new = fb_alerts(st, tmp_path)
    assert len(new) == 1 and new[0]["kind"] == "owner_feedback"
    assert new[0]["display"].startswith("[owner] feedback on A11: should be decision — I approve this")
    assert len(new[0]["display"]) < 140
    assert fb_alerts(st, tmp_path, 1100) == []
    put(tmp_path, "A11", status="open", feedback={"to": "task", "note": "again", "at": "2026-10-09T03:00:00Z", "status": "new"})
    assert len(fb_alerts(st, tmp_path, 1200)) == 1
    assert fp.owner_answer_alerts(1300, str(tmp_path)) == []     # feedback is not an answer alert


def test_owner_answers_returns_new_feedback_only_and_pending_answers_exits_1(tmp_path, capsys):
    import owner_answers
    put(tmp_path, "T20", status="open", feedback={"to": "action", "note": "n", "at": "2026-10-09T02:00:00Z", "status": "new"})
    put(tmp_path, "T21", status="open", feedback={"to": "action", "note": "n", "at": "x", "status": "applied"})
    got = owner_answers.pending(str(tmp_path))
    assert [(g["id"], g["kind"], g["to"]) for g in got] == [("T20", "feedback", "action")]
    assert pa.main(["--root", str(tmp_path)]) == 1
    assert "T20\tfeedback" in capsys.readouterr().out
    put(tmp_path, "T20", status="open", feedback={"to": "action", "note": "n", "at": "x", "status": "applied"})
    assert pa.main(["--root", str(tmp_path)]) == 0


def test_parked_item_flagged_only_when_due_and_alerts_once_per_due_date(tmp_path):
    import owner_answers
    from datetime import datetime, timezone
    pk = {"until": "2026-10-10T13:00:00Z", "choice": "tomorrow", "at": "2026-10-09T03:00:00Z", "by": "Owner"}
    put(tmp_path, "A12", status="open", parked=pk,
        updates=[{"at": "2026-10-09T05:00:00Z", "by": "Architect", "text": "x", "source": "pm"}])
    early, late = datetime(2026, 10, 10, 12, 0, tzinfo=timezone.utc), datetime(2026, 10, 10, 14, 0, tzinfo=timezone.utc)
    assert owner_answers.pending(str(tmp_path), early) == []
    got = owner_answers.pending(str(tmp_path), late)
    assert [(g["id"], g["kind"], g["updates"]) for g in got] == [("A12", "owner_unpark_due", 1)]
    st, t0 = {}, int(late.timestamp())
    new, _ = fp.dedup(st, fp.owner_unpark_alerts(t0, str(tmp_path)), t0)
    assert len(new) == 1 and new[0]["display"] == "[owner] parked A12 is due: 1 updates since parked"
    assert fp.dedup(st, fp.owner_unpark_alerts(t0 + 100, str(tmp_path)), t0 + 100)[0] == []
    assert fp.owner_unpark_alerts(int(early.timestamp()), str(tmp_path)) == []
    assert fp.owner_answer_alerts(1200, str(tmp_path)) == []
    put(tmp_path, "A12", status="open", parked=None)
    assert owner_answers.pending(str(tmp_path), late) == []


def put_session(root, sid, **data):
    d = root / "sessions"
    d.mkdir(exist_ok=True)
    (d / f"{sid}.json").write_text(json.dumps(data))


def test_session_command_new_alerts_once_per_command_and_pending_answers_lists_it(tmp_path, capsys):
    import owner_answers
    put_session(tmp_path, "S03", name="tm-memory", state="idle",
                command={"type": "activate", "text": "#9451", "at": "2026-10-09T02:00:00Z", "by": "Owner", "status": "new"})
    put_session(tmp_path, "S04", name="tm-search",
                command={"type": "prioritize", "goal": "G02", "note": "", "at": "2026-10-09T02:01:00Z", "status": "applied"})
    got = owner_answers.pending(str(tmp_path))
    assert [(g["id"], g["kind"], g["type"], g["text"]) for g in got] == [("S03", "owner_command", "activate", "#9451")]
    st = {}
    new, _ = fp.dedup(st, fp.owner_command_alerts(1000, str(tmp_path)), 1000)
    assert len(new) == 1 and new[0]["kind"] == "owner_command"
    assert new[0]["display"] == "[owner] activate tm-memory (S03): #9451"
    assert fp.dedup(st, fp.owner_command_alerts(1100, str(tmp_path)), 1100)[0] == []
    assert fp.owner_answer_alerts(1200, str(tmp_path)) == []          # not an answer alert
    assert pa.main(["--root", str(tmp_path)]) == 1
    assert "S03\towner_command" in capsys.readouterr().out
    put_session(tmp_path, "S03", name="tm-memory", command={"type": "prioritize", "goal": "G01", "note": "first",
                "at": "2026-10-09T03:00:00Z", "status": "new"})
    new, _ = fp.dedup(st, fp.owner_command_alerts(1300, str(tmp_path)), 1300)
    assert len(new) == 1 and new[0]["display"] == "[owner] prioritize G01 tm-memory (S03): first"
    put_session(tmp_path, "S03", name="tm-memory", command={"type": "prioritize", "goal": "G01", "at": "x", "status": "applied"})
    assert pa.main(["--root", str(tmp_path)]) == 0


def wd_alerts(state, root, now=1000):
    new, _ = fp.dedup(state, fp.owner_wont_do_alerts(now, str(root)), now)
    return new


def test_wont_do_alerts_once_acknowledged_is_quiet_and_empty_reason_says_so(tmp_path, capsys):
    import owner_answers
    put(tmp_path, "A30", status="wont_do", wont_do={"note": "Not worth it, drop it", "at": "2026-10-09T03:00:00Z", "by": "Owner", "attempts": 1})
    put(tmp_path, "A31", status="wont_do", wont_do={"note": "", "at": "2026-10-09T03:01:00Z", "reason_missing": True})
    put(tmp_path, "A32", status="wont_do", answer_note="Acknowledged 03:10Z: dropped.", wont_do={"note": "x", "at": "2026-10-09T03:02:00Z"})
    put(tmp_path, "A33", status="wont_do", answer_note="Relayed 03:10Z", wont_do={"note": "x", "at": "2026-10-09T03:02:00Z"})
    got = {g["id"]: g for g in owner_answers.pending(str(tmp_path)) if g["kind"] == "wont_do"}
    assert sorted(got) == ["A30", "A31"]
    assert got["A30"]["text"] == "Not worth it, drop it" and got["A31"]["text"] == "no reason given"
    st = {}
    new = wd_alerts(st, tmp_path)
    assert sorted(a["kind"] for a in new) == ["owner_wont_do", "owner_wont_do"]
    assert any(a["display"] == "[owner] A30 won't do: Not worth it, drop it" for a in new)
    assert wd_alerts(st, tmp_path, 1100) == []
    assert fp.owner_answer_alerts(1200, str(tmp_path)) == []     # not an answer alert
    assert pa.main(["--root", str(tmp_path)]) == 1
    assert "A30\twont_do" in capsys.readouterr().out


def test_park_and_restart_commands_reach_pending_answers_and_alert_plainly(tmp_path, capsys):
    import owner_answers
    put_session(tmp_path, "S03", name="tm-memory-01", state="exited",
                command={"type": "restart", "note": "after the build", "at": "2026-10-09T04:00:00Z", "by": "Owner", "status": "new"})
    put_session(tmp_path, "S04", name="tm-search-01", state="idle",
                command={"type": "park", "at": "2026-10-09T04:01:00Z", "by": "Owner", "status": "new"})
    got = {g["id"]: g for g in owner_answers.pending(str(tmp_path))}
    assert (got["S03"]["type"], got["S03"]["text"], got["S04"]["type"]) == ("restart", "after the build", "park")
    displays = sorted(a["display"] for a in fp.owner_command_alerts(1000, str(tmp_path)))
    assert displays == ["[owner] park tm-search-01", "[owner] restart tm-memory-01: after the build"]
    assert pa.main(["--root", str(tmp_path)]) == 1
    assert "S03\towner_command" in capsys.readouterr().out


def put_tune(root, doc_id, **data):
    d = root / "effort_tunes"
    d.mkdir(exist_ok=True)
    (d / f"{doc_id}.json").write_text(json.dumps({"version": 1, "data": data}))


def test_effort_tune_alerts_once_applied_quiet_and_listed_by_pending_answers(tmp_path, capsys):
    put_tune(tmp_path, "e1", goal="G01", direction="more", bias_after=2, at="2026-10-09T03:52:10Z", by="Owner", status="new")
    put_tune(tmp_path, "e2", goal="G02", direction="less", bias_after=-1, at="2026-10-09T03:53:00Z", status="applied")
    st = {}
    new, _ = fp.dedup(st, fp.owner_effort_alerts(1000, str(tmp_path)), 1000)
    assert [(a["kind"], a["display"]) for a in new] == [("owner_effort", "[owner] G01 effort more (bias +2) 03:52Z")]
    again, _ = fp.dedup(st, fp.owner_effort_alerts(1100, str(tmp_path)), 1100)
    assert again == []
    assert fp.owner_answer_alerts(1200, str(tmp_path)) == []     # not an answer alert
    assert pa.main(["--root", str(tmp_path)]) == 1
    out = capsys.readouterr().out
    assert "e1\teffort" in out and "e2" not in out


def test_effort_store_error_is_tolerated(tmp_path, capsys):
    (tmp_path / "effort_tunes").write_text("not a directory")
    assert fp.owner_effort_alerts(1000, str(tmp_path)) == []
