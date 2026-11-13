"""#10960 (DOC-87 G-4, X10-X13): an explicit supersession link in the Intent Ledger, and a relay that refuses
an authentic quote of an owner turn the owner has since reversed.

The owner turns are synthetic: T1 "two helpers", T2 "four helpers", T3 "back to two", U an unrelated request.
The ledger is a scratch Architect IL (ARCHITECT_IL); relay.sh runs against the stub tmux of test_relay.
"""
import json
import os
import subprocess
import sys

import pytest

import datg
import datg_guards
from test_relay import RELAY, Stub, box, owner_turns, relay_log

DATG = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts", "datg.py")
T1 = {"at": "2026-11-07T10:00:05Z", "session": "tm-architect", "text": "Keep two helpers on the rules track."}
T2 = {"at": "2026-11-07T10:20:05Z", "session": "tm-architect", "text": "Use four helpers on the rules track now."}
U = {"at": "2026-11-07T10:30:05Z", "session": "tm-architect", "text": "Review PR 12 before lunch please."}
T3 = {"at": "2026-11-07T10:40:05Z", "session": "tm-architect", "text": "Go back to two helpers on the rules track."}


def cite(turn):
    return "chat " + turn["at"][:16] + "Z"


def owner(turn, **extra):
    return {"by": "owner", "cite": cite(turn), "quote": turn["text"], **extra}


@pytest.fixture
def il(tmp_path, monkeypatch):
    """A scratch Architect IL with four open decisions, and the owner turns recorded."""
    p = str(tmp_path / "il")
    os.makedirs(p)
    monkeypatch.setenv("ARCHITECT_IL", p)
    owner_turns(T1, T2, U, T3)
    datg.op_init(p, "ar")
    datg.write_ops(p, [{"op": "add", "kind": "G", "title": "ship the rules track"}] +
                   [{"op": "add", "kind": "D", "title": t, "goal": "ar-G01"}
                    for t in ("helpers", "helpers again", "helpers third", "PR 12 review")])
    return p


def item(p, iid):
    return next(i for i in datg.load_state(p)["items"] if i["id"] == iid)


def answer(p, iid, turn, **op):
    datg.write_ops(p, [{"op": "update", "id": iid, "answer": turn["text"], "source": owner(turn), **op}])


def relay(tmp_path, turn, name, slot="Ada", at=None):
    text = f'[Ada Architect 11:00Z] User ruling ({slot}): "{turn["text"]}" (owner turn: {at or cite(turn)})'
    stub = Stub(tmp_path / name, [box(), box(text)])
    return stub, stub.run(RELAY, "tm-api", text)


def sent(stub, out):
    assert out.returncode == 0 and out.stdout.startswith("tm-api sent "), out
    assert stub.sends()[-1] == "send-keys -t =tm-api: Enter"


def refused(stub, out, *why):
    assert out.returncode == 3 and "NO-OWNER-CITE" in out.stdout, out
    for w in why:
        assert w in out.stdout, out
    assert stub.calls() == []   # refused before anything is typed


# --- datg.py: the link -----------------------------------------------------------------

class TestLinkCase:
    def test_supersedes_stores_superseded_by_on_the_old_ruling_only_10960(self, il):
        answer(il, "ar-D01", T1)
        r = subprocess.run([sys.executable, DATG, "--project", il, "update", "ar-D02", "--answer", T2["text"],
                            "--source", "owner", "--cite", cite(T2), "--quote", T2["text"], "--supersedes", "ar-D01"],
                           capture_output=True, text=True)
        assert r.returncode == 0, r.stderr
        assert item(il, "ar-D01")["superseded_by"] == "ar-D02"
        assert "superseded_by" not in item(il, "ar-D02")

    @pytest.mark.parametrize("setup,op,why", [
        ([("ar-D01", T1)], {"id": "ar-D02", "answer": "four", "supersedes": "ar-D01",
                            "source": {"by": "architect", "basis": "pace"}}, "sourced to the owner"),
        ([("ar-D02", T2)], {"id": "ar-D02", "supersedes": "ar-D01"}, "not an owner ruling"),
        ([("ar-D01", T1)], {"id": "ar-D01", "supersedes": "ar-D01"}, "another decision"),
        ([("ar-D01", T1), ("ar-D02", T2)], {"id": "ar-D02", "supersedes": "ar-G01"}, "another decision"),
    ])
    def test_a_bad_link_is_refused_and_leaves_the_file_10960(self, il, setup, op, why):
        for iid, turn in setup:
            answer(il, iid, turn)
        before = open(datg.paths(il)[1]).read()
        with pytest.raises(datg.DatgError, match=why):
            datg.write_ops(il, [{"op": "update", **op}])
        assert open(datg.paths(il)[1]).read() == before

    def test_links_never_fork_and_a_superseded_ruling_takes_no_new_answer_10960(self, il):
        answer(il, "ar-D01", T1)
        answer(il, "ar-D02", T2, supersedes="ar-D01")
        answer(il, "ar-D03", T3)
        with pytest.raises(datg.DatgError, match="already superseded by ar-D02"):
            datg.write_ops(il, [{"op": "update", "id": "ar-D03", "supersedes": "ar-D01"}])
        with pytest.raises(datg.DatgError, match="is superseded by ar-D02"):
            answer(il, "ar-D01", T3)
        datg.write_ops(il, [{"op": "update", "id": "ar-D02", "supersedes": "ar-D01"}])   # the same link: no-op
        assert item(il, "ar-D01")["superseded_by"] == "ar-D02"

    def test_a_superseded_ruling_is_not_current_and_a_broken_link_is_flagged_10960(self, il):
        answer(il, "ar-D01", T1)
        answer(il, "ar-D02", T2, supersedes="ar-D01")
        st = datg.load_state(il)
        from datetime import datetime, timezone
        assert [i["id"] for i, _ in datg_guards.owner_rulings(st, datetime.now(timezone.utc))] == ["ar-D02"]
        assert datg_guards.supersession_flags(st) == []
        digest = datg.build_digest(il, 3000)
        assert "superseded by ar-D02" in digest.split("OWNER RULINGS EXPIRED")[1]
        st["items"] = [i for i in st["items"] if i["id"] != "ar-D02"]   # the successor gone from the ledger
        assert "ar-D01 is superseded by 'ar-D02'" in "".join(datg_guards.supersession_flags(st))


# --- relay.sh: the refusal ---------------------------------------------------------------

@pytest.mark.parametrize("at", [None, "chat 2026-11-07T10:01Z"])   # the same turn cited a minute later
def test_relay_refuses_a_cite_of_a_superseded_owner_ruling_10960(tmp_path, il, at):
    """X10: T1 two, T2 four with --supersedes T1; a relay citing T1 is refused and names T2."""
    answer(il, "ar-D01", T1)
    answer(il, "ar-D02", T2, supersedes="ar-D01")
    stub, out = relay(tmp_path, T1, "t1", at=at)
    refused(stub, out, "superseded by ar-D02", T2["text"])
    assert [r["outcome"] for r in relay_log()] == ["refused"]


def test_relay_accepts_the_reversal_and_refuses_what_it_reversed_10960(tmp_path, il):
    """X12, owner reversals keep working: T3 "back to two" supersedes T2. T3 is accepted; T2 and T1 are refused."""
    answer(il, "ar-D01", T1)
    answer(il, "ar-D02", T2, supersedes="ar-D01")
    r = subprocess.run([sys.executable, DATG, "--project", il, "update", "ar-D03", "--answer", "two helpers",
                        "--source", "owner", "--cite", cite(T3), "--quote", T3["text"], "--supersedes", "ar-D02"],
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    sent(*relay(tmp_path, T3, "t3"))
    refused(*relay(tmp_path, T2, "t2"), "superseded by ar-D03")
    refused(*relay(tmp_path, T1, "t1"), "superseded by ar-D02", "the current ruling is ar-D03")


def test_relay_sends_a_cite_no_link_supersedes_10960(tmp_path, il):
    """X13 and no new gate: later owner turns recorded without --supersedes leave T1 citable, logged as before."""
    answer(il, "ar-D01", T1)
    answer(il, "ar-D04", U)
    answer(il, "ar-D02", T2)   # a later contradicting turn, but unlinked: nothing is inferred
    stub, out = relay(tmp_path, T1, "t1")
    sent(stub, out)
    assert out.stderr == ""
    assert [(r["outcome"], r["cite"], "flag" in r) for r in relay_log()] == [("verified", cite(T1), False)]


def test_relay_flags_a_cite_superseded_by_a_bounded_ruling_10960(tmp_path, il):
    """X11: T2 holds only until a time; the T1 cite is ambiguous, so it is sent with a FLAG, not refused."""
    answer(il, "ar-D01", T1)
    bounded(il, "ar-D02", T2, "ar-D01", FUTURE)
    stub, out = relay(tmp_path, T1, "t1")
    sent(stub, out)
    assert "FLAG superseded-bounded" in out.stderr and "ar-D02" in out.stderr, out
    assert f"until {FUTURE}" in relay_log()[0]["flag"]


FUTURE, PAST = "2099-01-01T00:00Z", "2026-11-09T09:00Z"


def bounded(p, iid, turn, old, until):
    datg.write_ops(p, [{"op": "update", "id": iid, "answer": turn["text"], "supersedes": old,
                        "source": owner(turn, until=until)}])


def test_relay_refuses_past_a_bounded_successor_to_an_unbounded_one_10960(tmp_path, il):
    """critic HIGH: T1 -> T2 (until ahead) -> T3 (unbounded). The chain ends unbounded, so a T1 cite refuses."""
    answer(il, "ar-D01", T1)
    bounded(il, "ar-D02", T2, "ar-D01", FUTURE)
    answer(il, "ar-D03", T3, supersedes="ar-D02")
    refused(*relay(tmp_path, T1, "t1"), "superseded by ar-D02", "the current ruling is ar-D03")


def test_relay_refuses_a_cite_whose_bounded_successor_has_expired_10960(tmp_path, il):
    """critic HIGH: a successor whose `until` has passed no longer makes the cite ambiguous; it refuses."""
    answer(il, "ar-D01", T1)
    bounded(il, "ar-D02", T2, "ar-D01", PAST)
    stub, out = relay(tmp_path, T1, "t1")
    refused(stub, out, "superseded by ar-D02", f"its until {PAST} has passed")


def test_relay_sends_a_superseded_ruling_labelled_as_history_10960(tmp_path, il):
    answer(il, "ar-D01", T1)
    answer(il, "ar-D02", T2, supersedes="ar-D01")
    sent(*relay(tmp_path, T1, "t1", slot="Ada, superseded"))


@pytest.mark.parametrize("archived", ["ar-D02", "ar-D01"])
def test_relay_reads_the_archive_for_a_link_or_its_successor_10960(tmp_path, il, archived):
    answer(il, "ar-D01", T1)
    answer(il, "ar-D02", T2, supersedes="ar-D01")
    datg.write_ops(il, [{"op": "close", "id": archived, "outcome": "closed at review, see #10960"}])
    refused(*relay(tmp_path, T1, "t1"), "superseded by ar-D02")


# --- damaged ledgers (Di9: refuse only a cite whose chain touches the damage) ----------------

def _state(p):
    return datg.paths(p)[1]


def _archive(p):
    return os.path.join(datg.paths(p)[0], "archive.jsonl")


def _append(path, text):
    with open(path, "a") as f:
        f.write(text)


def _corrupt_tail(p, tmp_path):   # the cite's own record is still in the bytes
    _append(_state(p), "}garbage")


def _unreadable(p, tmp_path):     # no bytes, and no sound record locates the cite
    if os.geteuid() == 0:
        pytest.skip("root reads a mode-000 file")
    os.chmod(_state(p), 0)


def _torn_archive_on_chain(p, tmp_path):
    _append(_archive(p), '{"item": {"id": "ar-D05", "source": {"cite": "%s"}, "superseded_by": "ar-D0\n' % cite(T1))


def _bad_schedule(p, tmp_path):   # the schedule's sessions go unread
    path = tmp_path / "schedule.json"
    path.write_text("{not json")
    os.environ["IL_SCHEDULE"] = str(path)   # conftest's monkeypatch restores it


def _dangling(p, tmp_path):
    st = json.load(open(_state(p)))
    next(i for i in st["items"] if i["id"] == "ar-D01")["superseded_by"] = "ar-D7f"
    with open(_state(p), "w") as f:
        json.dump(st, f)


@pytest.mark.parametrize("damage,recorded,why", [
    (_corrupt_tail, True, "is corrupt"),
    (_unreadable, True, "cannot be read"),
    (_torn_archive_on_chain, True, "is a corrupt archive line"),
    (_bad_schedule, False, "schedule.json cannot be read"),     # nothing sound locates the cite: cannot tell, refuse
    (_dangling, True, "which no ledger holds"),
])
def test_relay_refuses_a_damaged_record_on_the_cites_chain_10960(tmp_path, il, damage, recorded, why):
    """Fail closed where it matters: the damaged record may hold this cite's link, so the cite is refused."""
    if recorded:
        answer(il, "ar-D01", T1)
    damage(il, tmp_path)
    try:
        refused(*relay(tmp_path, T1, "t1"), why)
    finally:
        os.chmod(_state(il), 0o644)


def _other_project(tmp_path, monkeypatch, state):
    """A second scheduled PM ledger whose state.json holds `state` (bytes), or is mode 000 when `state` is None."""
    proj = tmp_path / "pm-other"
    d = proj / ".trusty-mpm" / "datg"
    d.mkdir(parents=True)
    (d / "state.json").write_bytes(state or b"{}")
    if state is None:
        if os.geteuid() == 0:
            pytest.skip("root reads a mode-000 file")
        os.chmod(d / "state.json", 0)
    sched = tmp_path / "il-schedule.json"
    sched.write_text(json.dumps({"sessions": [{"name": "pm-other", "project": str(proj), "code": "po"}]}))
    monkeypatch.setenv("IL_SCHEDULE", str(sched))
    return str(d / "state.json")


@pytest.mark.parametrize("where", ["other-corrupt", "other-unreadable", "own-archive-line", "own-state-emptied",
                                   "schedule"])
def test_relay_flags_a_damaged_ledger_off_the_cites_chain_and_sends_10960(tmp_path, il, monkeypatch, where):
    """Di9, no blanket gate: damage that cannot hold this cite's link is a loud FLAG and a finding row; the
    relay is sent."""
    answer(il, "ar-D01", T1)
    if where == "other-corrupt":
        path = _other_project(tmp_path, monkeypatch, b'{"code": "po", "items": [{"id": "po-D01"')
    elif where == "other-unreadable":
        path = _other_project(tmp_path, monkeypatch, None)
    elif where == "own-archive-line":   # a torn line that names nothing on the chain
        path = _archive(il) + ":1"
        _append(_archive(il), '{"item": {"id": "ar-D05", "title": "cut sh\n')
    elif where == "own-state-emptied":  # the bytes that could hold a link are gone, as with a missing ledger
        path = _state(il)
        with open(path, "w") as f:
            f.write("[]")
    else:
        path = str(tmp_path / "schedule.json")
        _bad_schedule(il, tmp_path)
    try:
        stub, out = relay(tmp_path, T1, "t1")
    finally:
        os.chmod(path.rsplit(":", 1)[0], 0o644)
    sent(stub, out)
    assert "FLAG DAMAGED LEDGER" in out.stderr and path in out.stderr, out
    rows = relay_log()
    assert [r["damaged"] for r in rows if r["outcome"] == "finding"] == [path], rows
    assert [r["outcome"] for r in rows].count("verified") == 1, rows
