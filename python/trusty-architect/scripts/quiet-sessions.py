#!/usr/bin/env python3
"""quiet-sessions.py — sessions the fleet poller should not pester about idling.

Why: once fleet-poll.py's wake() could actually reach the Architect (the
labeled-top-rule fix), every pane's "idle" alert started landing too —
including sessions the owner has deliberately parked or paused, which then
wake the Architect every 30 minutes for nothing (2026-09-27: a parked
session, idle 1998 min).
What: `read_quiet_sessions(path)` returns the plain-text session-name set
from `path` — one name per line, a `#` starts a comment, blank lines
ignored — re-read fresh every call; a missing file is an empty set, never
a crash. `is_test_fixture_session(name)` matches the tm-xtest-*, tm-qa*-*
and tm-r[0-9]* glob shapes throwaway test fixtures use, which must never
raise an "exited" alert regardless of the quiet-sessions list.
Test: tests/test_quiet_sessions.py
"""
import fnmatch
import re

TEST_FIXTURE_GLOBS = ("tm-xtest-*", "tm-qa*-*", "tm-r[0-9]*")
TEST_FIXTURE_SESSION = re.compile("|".join(fnmatch.translate(g) for g in TEST_FIXTURE_GLOBS))


def read_quiet_sessions(path):
    """Session names from `path`, one per line ('#' comments, blanks
    ignored). A missing file returns an empty set, not an error."""
    try:
        with open(path) as f:
            lines = f.read().splitlines()
    except OSError:
        return set()
    out = set()
    for line in lines:
        line = line.split("#", 1)[0].strip()
        if line:
            out.add(line)
    return out


def is_test_fixture_session(session):
    """True for a throwaway test-fixture session name (tm-xtest-*, tm-qa*-*,
    tm-r[0-9]*): these never raise an "exited" alert."""
    return bool(TEST_FIXTURE_SESSION.match(session))
