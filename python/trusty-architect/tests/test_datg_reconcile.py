"""#10960 (DOC-87 G-3, X7-X9): the digest's RECONCILE block checks the handoff and local/standing-rules.md
against the Intent Ledger on every SessionStart (startup, resume, clear, compact) and in `datg.py digest`.

All data is synthetic. The ledger sits at <tmp>/local/il, the Architect layout, so the standing-rules file is
found at <tmp>/local/standing-rules.md the way `tm hook --standing-rules` reads it. The block only reports.
"""
import json
import os
import subprocess
import sys

import pytest

import datg
import datg_read
from test_relay import owner_turns

DATG = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts", "datg.py")
T1 = {"at": "2026-11-07T10:00:05Z", "session": "tm-architect", "text": "Keep two helpers on the rules track."}
T2 = {"at": "2026-11-07T10:20:05Z", "session": "tm-architect", "text": "Use four helpers on the rules track now."}
T3 = {"at": "2026-11-07T10:40:05Z", "session": "tm-architect", "text": "Go back to two helpers on the rules track."}
HOLD = {"at": "2026-11-07T09:00:05Z", "session": "tm-architect", "text": "Do not merge PR 12 until the review lands."}
OLD = {"at": "2026-11-07T08:00:05Z", "session": "tm-architect", "text": "Pause the docs track until tomorrow 09:00."}


def cite(turn):
    return "chat " + turn["at"][:16] + "Z"


def line(turn):
    return f'"{turn["text"]}" (owner turn: {cite(turn)})'


@pytest.fixture
def il(tmp_path, monkeypatch):
    """A scratch Architect ledger at <tmp>/local/il with five open decisions and the owner turns recorded."""
    p = str(tmp_path / "local" / "il")
    os.makedirs(p)
    monkeypatch.setenv("ARCHITECT_IL", p)
    monkeypatch.delenv("ARCHITECT_STANDING_RULES", raising=False)
    owner_turns(T1, T2, T3, HOLD, OLD)
    datg.op_init(p, "ar")
    datg.write_ops(p, [{"op": "add", "kind": "G", "title": "ship the rules track"}] +
                   [{"op": "add", "kind": "D", "title": t, "goal": "ar-G01"}
                    for t in ("helpers", "helpers again", "PR 12 hold", "helpers third", "docs pause")])
    return p


def answer(p, iid, turn, until=None, **op):
    src = {"by": "owner", "cite": cite(turn), "quote": turn["text"], **({"until": until} if until else {})}
    datg.write_ops(p, [{"op": "update", "id": iid, "answer": turn["text"], "source": src, **op}])


def sources(p, handoff=None, rules=None):
    """Write the handoff (recorded as the session's handoff ref) and the standing rules; None leaves one out."""
    if handoff is not None:
        with open(os.path.join(p, "state.md"), "w", encoding="utf-8") as f:
            f.write("\n".join(handoff) + "\n")
        datg.write_ops(p, [{"op": "session", "task": "pace the rules track", "next": "keep going",
                            "refs": [{"key": "handoff", "value": "state.md"}]}])
    if rules is not None:
        with open(os.path.join(os.path.dirname(p), "standing-rules.md"), "w", encoding="utf-8") as f:
            f.write("\n".join(rules) + "\n")


def block(p):
    """The RECONCILE block of the digest, as lines."""
    out = datg.build_digest(p, 3000).splitlines()
    start = out.index(next(ln for ln in out if ln.startswith("RECONCILE")))
    rest = out[start + 1:]
    end = next((n for n, ln in enumerate(rest) if not ln.startswith("  ")), len(rest))
    return [out[start]] + rest[:end]


def flags(p):
    return [ln for ln in block(p) if ln.startswith("  FLAG reconcile")]


def hook(p, source):
    r = subprocess.run([sys.executable, DATG, "hook"], capture_output=True, text=True,
                       input=json.dumps({"hook_event_name": "SessionStart", "source": source}),
                       env=dict(os.environ, CLAUDE_PROJECT_DIR=p))
    return r


# --- the three reports (X7, X8, X9) ---------------------------------------------------------

def test_handoff_citing_an_expired_ruling_is_marked_expired_not_applied_x7_10960(il):
    answer(il, "ar-D05", OLD, until="2026-11-08T09:00Z")
    sources(il, handoff=["- docs: " + line(OLD)], rules=[])
    fl = flags(il)
    assert fl == ["  FLAG reconcile: state.md:1 cites ar-D05, EXPIRED at 2026-11-08T09:00Z, not applied"], fl


def test_handoff_citing_a_superseded_ruling_shows_the_successor_x8_10960(il):
    answer(il, "ar-D01", T1)
    answer(il, "ar-D02", T2, supersedes="ar-D01")
    sources(il, handoff=["- helpers: " + line(T1)], rules=["- helpers: " + line(T2)])
    fl = flags(il)
    assert fl == ["  FLAG reconcile: state.md:1 cites ar-D01, SUPERSEDED, not applied; ledger applies "
                  f'ar-D02 "{T2["text"]}" (owner turn: {cite(T2)})',
                  "  FLAG reconcile: owner ruling active in the ledger, missing from handoff; "
                  f'restored: ar-D02 "{T2["text"]}" (owner turn: {cite(T2)})'], fl


def test_ledger_hold_absent_from_handoff_is_restored_x9_10960(il):
    answer(il, "ar-D03", HOLD)
    sources(il, handoff=["- next: review PR 12"], rules=["- PR 12: " + line(HOLD)])
    assert flags(il) == ["  FLAG reconcile: owner ruling active in the ledger, missing from handoff; "
                         f'restored: ar-D03 "{HOLD["text"]}" (owner turn: {cite(HOLD)})']


def test_a_ruling_missing_from_standing_rules_is_restored_10960(il):
    answer(il, "ar-D03", HOLD)
    sources(il, handoff=["- hold ar-D03 stays"], rules=["# Standing rules", "", "---"])
    assert flags(il) == ["  FLAG reconcile: owner ruling active in the ledger, missing from standing-rules.md; "
                         f'restored: ar-D03 "{HOLD["text"]}" (owner turn: {cite(HOLD)})']


def test_lines_with_no_ledger_source_are_flagged_10960(il):
    sources(il, handoff=["- User ruling (Ada): three helpers", "- pace 1.75%/h", "- " + line(T1)],
            rules=["# Standing rules", "- Ship docs on Fridays."])
    fl = flags(il)
    assert [f.split(" has no ledger source")[0] for f in fl] == [
        "  FLAG reconcile: state.md:1", "  FLAG reconcile: state.md:3", "  FLAG reconcile: standing-rules.md:2"], fl


# --- reversals and the clean case -----------------------------------------------------------

def test_owner_reversal_old_line_superseded_new_applied_and_never_missing_10960(il):
    answer(il, "ar-D01", T1)
    answer(il, "ar-D02", T2, supersedes="ar-D01")
    sources(il, handoff=["- helpers per ar-D02"], rules=["- " + line(T1), "- " + line(T2)])
    fl = flags(il)
    assert fl == ["  FLAG reconcile: standing-rules.md:1 cites ar-D01, SUPERSEDED, not applied; ledger applies "
                  f'ar-D02 "{T2["text"]}" (owner turn: {cite(T2)})'], fl
    assert not any("missing from" in f for f in fl)   # the reversed T1 is history, not a missing ruling
    # the owner goes back to two (T3 supersedes T2): the old lines point at the chain's end, T3 is restored
    answer(il, "ar-D04", T3, supersedes="ar-D02")
    fl = flags(il)
    assert sum("ledger applies ar-D04" in f for f in fl) == 3, fl   # handoff:1, rules:1, rules:2
    assert [f for f in fl if "missing from" in f] == [
        "  FLAG reconcile: owner ruling active in the ledger, missing from handoff and standing-rules.md; "
        f'restored: ar-D04 "{T3["text"]}" (owner turn: {cite(T3)})']


def test_a_handoff_superseded_line_never_hides_a_successor_missing_from_standing_rules_10960(il):
    answer(il, "ar-D01", T1)
    answer(il, "ar-D02", T2, supersedes="ar-D01")
    sources(il, handoff=["- helpers: " + line(T1), "- helpers now ar-D02"], rules=["# Standing rules"])
    fl = flags(il)
    assert fl == ["  FLAG reconcile: state.md:1 cites ar-D01, SUPERSEDED, not applied; ledger applies "
                  f'ar-D02 "{T2["text"]}" (owner turn: {cite(T2)})',
                  "  FLAG reconcile: owner ruling active in the ledger, missing from standing-rules.md; "
                  f'restored: ar-D02 "{T2["text"]}" (owner turn: {cite(T2)})'], fl


def test_the_cap_never_drops_stale_or_missing_lines_for_no_source_lines_10960(il):
    answer(il, "ar-D01", T1)
    answer(il, "ar-D02", T2, supersedes="ar-D01")
    answer(il, "ar-D03", HOLD)
    sources(il, handoff=["- helpers: " + line(T1)],
            rules=["# Standing rules"] + [f"- uncited prose rule number {n}." for n in range(1, 17)])
    b = block(il)
    fl = [ln for ln in b if ln.startswith("  FLAG reconcile")]
    assert len(fl) == datg_read.RECONCILE_MAX, b   # bounded
    assert fl[0].startswith("  FLAG reconcile: state.md:1 cites ar-D01, SUPERSEDED"), fl
    for n, iid in ((1, "ar-D02"), (2, "ar-D03")):   # both restored rulings sit ahead of every no-source line
        assert fl[n].startswith("  FLAG reconcile: owner ruling active in the ledger, missing from handoff and "
                                f"standing-rules.md; restored: {iid}"), fl
    assert all("has no ledger source" in f for f in fl[3:]), fl
    assert b[-1] == f"  +{16 - (datg_read.RECONCILE_MAX - 3)} more reconcile flags not shown", b


def test_a_current_ruling_on_a_line_never_hides_a_superseded_cite_beside_it_10960(il):
    answer(il, "ar-D01", T1)
    answer(il, "ar-D02", T2, supersedes="ar-D01")
    answer(il, "ar-D03", HOLD)
    sources(il, handoff=["- helpers: " + line(T1) + "; hold ar-D03", "- helpers ar-D01 now ar-D02"],
            rules=["- ar-D02 ar-D03"])
    fl = flags(il)   # line 2 names the successor beside the old ruling: an update, not a stale cite
    assert fl == ["  FLAG reconcile: state.md:1 cites ar-D01, SUPERSEDED, not applied; ledger applies "
                  f'ar-D02 "{T2["text"]}" (owner turn: {cite(T2)})'], fl


def test_a_current_ruling_on_a_line_never_hides_an_expired_cite_beside_it_10960(il):
    answer(il, "ar-D05", OLD, until="2026-11-08T09:00Z")
    answer(il, "ar-D03", HOLD)
    sources(il, handoff=["- docs paused ar-D05, hold ar-D03"], rules=["- ar-D03"])
    b = block(il)
    assert b[1:] == ["  FLAG reconcile: state.md:1 cites ar-D05, EXPIRED at 2026-11-08T09:00Z, not applied"], b


def test_all_reconciled_when_every_source_matches_10960(il):
    answer(il, "ar-D03", HOLD)
    sources(il, handoff=["- hold: " + line(HOLD)], rules=["# Rules", "- ar-D03 " + line(HOLD)])
    assert block(il)[1:] == ["  all reconciled: 1 active owner rulings vs state.md, standing-rules.md"]


# --- Fail-Open Check: an unreadable source or ledger is said, never "all reconciled" -----------

@pytest.mark.parametrize("breakage,want", [
    ("handoff_missing", "FLAG reconcile: handoff state.md UNREADABLE (not found)"),
    ("handoff_dir", "FLAG reconcile: handoff state.md UNREADABLE"),
    ("rules_not_utf8", "FLAG reconcile: standing-rules.md standing-rules.md UNREADABLE"),
    ("rules_dir", "FLAG reconcile: standing-rules.md standing-rules.md UNREADABLE"),
])
def test_an_unreadable_source_is_flagged_never_reconciled_10960(il, breakage, want):
    answer(il, "ar-D03", HOLD)
    sources(il, handoff=["- hold: " + line(HOLD)], rules=["- " + line(HOLD)])
    hp, rp = os.path.join(il, "state.md"), os.path.join(os.path.dirname(il), "standing-rules.md")
    if breakage == "handoff_missing":
        os.unlink(hp)
    elif breakage == "handoff_dir":
        os.unlink(hp)
        os.mkdir(hp)
    elif breakage == "rules_not_utf8":
        with open(rp, "wb") as f:
            f.write(b"- \xff\xfe two helpers\n")
    else:
        os.unlink(rp)
        os.mkdir(rp)
    b = block(il)
    assert any(want in ln for ln in b), b
    assert not any("all reconciled" in ln for ln in b), b


def test_an_unreadable_ledger_is_flagged_and_the_exit_code_is_unchanged_10960(il):
    with open(datg.paths(il)[1], "w", encoding="utf-8") as f:
        f.write("{garbage")
    r = subprocess.run([sys.executable, DATG, "--project", il, "digest", "--no-log"], capture_output=True, text=True)
    assert r.returncode == 2, r   # the DatgError exit code the digest had before G-3
    assert "FLAG reconcile: ledger unreadable" in r.stdout and "NOTHING RECONCILED" in r.stdout, r.stdout
    h = hook(il, "clear")
    assert h.returncode == 0 and "NOTHING RECONCILED" in h.stdout and "permissionDecision" not in h.stdout, h
    # a ledger that parses but whose items cannot be read is said the same way
    assert datg.reconcile_lines(il, {"items": [{"status": "answered", "source": {"by": "owner"}}]})[1].startswith(
        "  FLAG reconcile: ledger unreadable")


# --- No blanket approval gates: flags only, no decision, no new exit code ---------------------

@pytest.mark.parametrize("source", ["startup", "resume", "clear", "compact"])
def test_reconcile_reports_on_every_session_start_and_blocks_nothing_10960(il, source):
    answer(il, "ar-D01", T1)
    answer(il, "ar-D02", T2, supersedes="ar-D01")
    sources(il, handoff=["- User ruling (Ada): " + line(T1)], rules=["- Ship docs on Fridays."])
    h = hook(il, source)
    assert h.returncode == 0, h
    out = json.loads(h.stdout)["hookSpecificOutput"]
    assert set(out) == {"hookEventName", "additionalContext"}, out   # context only, never a permissionDecision
    assert "RECONCILE" in out["additionalContext"] and "SUPERSEDED, not applied" in out["additionalContext"]
    r = subprocess.run([sys.executable, DATG, "--project", il, "digest", "--no-log"], capture_output=True, text=True)
    assert r.returncode == 0 and "FLAG reconcile" in r.stdout, r
    st = datg.load_state(il)
    assert datg.dispatch_decision(il, st, "Goal: ar-G01 Task: none (probe)")[0] == datg.dispatch_decision(
        il, dict(st, session={}), "Goal: ar-G01 Task: none (probe)")[0]
