"""Tests for state-archive.py: old handoff sections move, parsed lines stay.

Test: scripts/test_state_archive.py
"""
import importlib.util
import os

# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
spec = importlib.util.spec_from_file_location("sa", os.path.join(SCRIPTS, "state-archive.py"))
sa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sa)

STATE = """# Current supervisor state
## Architect handoff UPDATE 4
NEXT IDS A 33 D 96
## Architect handoff UPDATE 3
## Architect UPDATE 2
## Architect handoff UPDATE 1
old one
## Watch set (updated)
- x
## Supervisor handoff (self-refresh)
old two
## Heartbeat
- poll_count: 24
- last_consumed_ts: 5
"""


def test_moves_old_keeps_rest_and_nothing_lost(tmp_path):
    st, ar = tmp_path / "state.md", tmp_path / "archive.md"
    st.write_text(STATE)
    moved = sa.run(str(st), str(ar), keep=3, dry_run=False)
    assert moved == 2
    kept, arch = st.read_text(), ar.read_text()
    assert "old one" not in kept and "old two" not in kept
    assert "NEXT IDS A 33" in kept and "poll_count: 24" in kept and "## Watch set" in kept
    assert "old one" in arch and "old two" in arch
    assert sorted((kept + arch).splitlines()).count("old one") == 1
    assert sa.run(str(st), str(ar), keep=3, dry_run=False) == 0  # idempotent


def test_dry_run_writes_nothing(tmp_path):
    st, ar = tmp_path / "state.md", tmp_path / "archive.md"
    st.write_text(STATE)
    assert sa.run(str(st), str(ar), keep=3, dry_run=True) == 2
    assert st.read_text() == STATE and not ar.exists()


def test_refuses_when_next_ids_would_be_lost(tmp_path):
    st, ar = tmp_path / "state.md", tmp_path / "archive.md"
    st.write_text(STATE.replace("NEXT IDS A 33 D 96", "x") + "## Architect handoff UPDATE 0\nNEXT IDS A 01\n")
    try:
        sa.run(str(st), str(ar), keep=3, dry_run=False)
    except SystemExit:
        pass
    else:
        raise AssertionError("expected refusal")
    assert st.read_text().count("NEXT IDS") == 1
