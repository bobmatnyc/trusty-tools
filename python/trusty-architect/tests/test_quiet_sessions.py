"""Tests for scripts/quiet-sessions.py: the parked-session list and the
test-fixture-session glob match, both pure (no tmux involved)."""
import importlib.util
import os

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")


def _load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(SCRIPTS, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


qs = _load("quiet_sessions", "quiet-sessions.py")


# --- read_quiet_sessions ------------------------------------------------------

def test_read_quiet_sessions_one_per_line_with_comments_and_blanks(tmp_path):
    f = tmp_path / "quiet-sessions.txt"
    f.write_text(
        "# parked by the owner\n"
        "tm-parked-01\n"
        "\n"
        "tm-paused-02  # paused pending a review\n"
        "   \n"
    )
    assert qs.read_quiet_sessions(str(f)) == {
        "tm-parked-01", "tm-paused-02"}


def test_read_quiet_sessions_missing_file_is_an_empty_set(tmp_path):
    assert qs.read_quiet_sessions(str(tmp_path / "does-not-exist.txt")) == set()


def test_read_quiet_sessions_all_comments_or_blank_is_an_empty_set(tmp_path):
    f = tmp_path / "quiet-sessions.txt"
    f.write_text("# nothing listed yet\n\n")
    assert qs.read_quiet_sessions(str(f)) == set()


def test_read_quiet_sessions_reflects_the_current_file_each_call(tmp_path):
    """Re-read every cycle: an edit between two calls must be seen."""
    f = tmp_path / "quiet-sessions.txt"
    f.write_text("tm-a\n")
    assert qs.read_quiet_sessions(str(f)) == {"tm-a"}
    f.write_text("tm-a\ntm-b\n")
    assert qs.read_quiet_sessions(str(f)) == {"tm-a", "tm-b"}


# --- is_test_fixture_session --------------------------------------------------

def test_test_fixture_globs_match():
    for name in ("tm-xtest-1", "tm-xtest-anything", "tm-qa1-run", "tm-qa-x",
                 "tm-r0", "tm-r42-worker"):
        assert qs.is_test_fixture_session(name) is True, name


def test_test_fixture_globs_do_not_match_real_sessions():
    for name in ("tm-architect", "tm-parked-01", "tm-api",
                 "tm-qa", "tm-r", "tm-rfoo", "tm-xtest"):
        assert qs.is_test_fixture_session(name) is False, name
