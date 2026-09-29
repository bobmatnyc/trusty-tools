#!/usr/bin/env python3
"""self-ctx.py — measure the Architect's own context from its Claude Code transcript.

Why: the Architect's own tmux pane carries no `ctx N%` status-line field (its
sessions are skipped from fleet-poll.py's per-pane capture, itself included),
so the standing self-refresh rule (pause/clear/resume over 40% context) has
nothing in the pane to read. This module tails the newest INTERACTIVE
transcript *.jsonl Claude Code writes for this project instead. "Interactive"
matters: the same directory also fills with short-lived `claude -p --agent`
transcripts from any `claude -p` helper (the deferred question collector ran
one every few minutes), and a newest-by-mtime-alone pick can land on one of
those instead of the Architect's own multi-hour session, under-reporting its
context and never
alerting (trusty-tools review, 2026-09-27). Claude Code tags every line with
"entrypoint": "cli" for an interactive session and "sdk-cli" for `claude -p`;
this module filters on it.
What: `self_ctx_tokens(directory)` finds the newest *.jsonl (by mtime, among
only the CANDIDATE_LIMIT newest files so an sdk-cli-heavy directory stays
cheap to scan) whose tail carries entrypoint "cli", reads it from the tail
(growing the read window only if needed, never parsing the whole file up
front), and sums input_tokens + cache_read_input_tokens +
cache_creation_input_tokens from its last assistant `message.usage` object.
Returns None when the directory is missing or empty, no candidate is an
interactive ("cli") transcript, or the chosen one carries no assistant usage
line — this module never raises for that case; the caller (fleet-poll.py)
decides what a None means. `default_dir()` resolves the transcript directory
from CLAUDE_CONFIG_DIR and ARCHITECT_PROJECT_DIR, each with a documented
default, so no operator path is baked in.
Test: tests/test_self_ctx.py
"""
import json
import os
import re

# `tm fleet init`'s default Architect project (#8436 ruling Q4).
DEFAULT_PROJECT_DIR = "~/trusty-mpm-projects/architect"
# The Claude Code config dir trusty-mpm launches sessions under.
DEFAULT_CLAUDE_CONFIG_DIR = "~/.trusty-tools/trusty-mpm/claude-config"
USAGE_TAIL_START = 65536       # first tail read for usage; grows if not found in it
ENTRYPOINT_TAIL_START = 4096   # entrypoint is on nearly every line; a small tail suffices
CANDIDATE_LIMIT = 20           # only this many newest-by-mtime files are considered


def project_slug(path):
    """Claude Code's per-project transcript directory name for `path`: every
    character that is not an ASCII letter or digit becomes "-"."""
    return re.sub(r"[^A-Za-z0-9]", "-", path)


def default_dir(environ=None):
    """<CLAUDE_CONFIG_DIR>/projects/<slug of ARCHITECT_PROJECT_DIR>, with
    DEFAULT_CLAUDE_CONFIG_DIR and DEFAULT_PROJECT_DIR standing in for an
    unset variable. `~` expands; the project path is made absolute first."""
    env = os.environ if environ is None else environ
    config = os.path.expanduser(env.get("CLAUDE_CONFIG_DIR") or DEFAULT_CLAUDE_CONFIG_DIR)
    project = os.path.abspath(os.path.expanduser(
        env.get("ARCHITECT_PROJECT_DIR") or DEFAULT_PROJECT_DIR))
    return os.path.join(config, "projects", project_slug(project))


def _scan_tail(path, want, start):
    """Grow the tail read (start, x8, x64, ...) until `want(obj)` returns a
    truthy result for some parsed JSON line, scanning newest-line-first, or
    until the whole file has been read without a match. Returns `want`'s
    result, or None. Malformed lines are skipped, not raised."""
    size = os.path.getsize(path)
    if size == 0:
        return None
    window = min(start, size)
    while True:
        with open(path, "rb") as f:
            if window < size:
                f.seek(-window, os.SEEK_END)
            data = f.read()
        lines = data.decode("utf-8", "replace").splitlines()
        if window < size:
            lines = lines[1:]  # first line of a tail read may be a partial line
        for line in reversed(lines):
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except ValueError:
                continue
            result = want(obj)
            if result:
                return result
        if window >= size:
            return None
        window = min(window * 8, size)


def _entrypoint(path):
    """The entrypoint ("cli" for interactive, "sdk-cli" for `claude -p`)
    carried by the newest line in `path`, or None."""
    return _scan_tail(path, lambda o: o.get("entrypoint"), ENTRYPOINT_TAIL_START)


def newest_transcript(directory, limit=CANDIDATE_LIMIT):
    """Absolute path of the newest *.jsonl in `directory` whose entrypoint is
    "cli" (an interactive session), or None. Only the `limit` newest files by
    mtime are examined — cheapest (newest) first — so a directory shared with
    a high-churn `claude -p` caller (entrypoint "sdk-cli") stays bounded."""
    try:
        names = [n for n in os.listdir(directory) if n.endswith(".jsonl")]
    except OSError:
        return None
    if not names:
        return None
    paths = sorted((os.path.join(directory, n) for n in names),
                   key=os.path.getmtime, reverse=True)
    for path in paths[:limit]:
        try:
            if _entrypoint(path) == "cli":
                return path
        except OSError:
            continue
    return None


def tail_last_assistant_usage(path, start=USAGE_TAIL_START):
    """The last assistant `message.usage` dict in `path`, read from the tail.

    Grows the read window instead of parsing the whole file, so a 100+ MB
    transcript costs one small read in the common case where a recent line
    carries usage.
    """
    def want(obj):
        if obj.get("type") != "assistant":
            return None
        return (obj.get("message") or {}).get("usage")
    return _scan_tail(path, want, start)


def self_ctx_tokens(directory=None):
    """input + cache_read + cache_creation tokens from the newest interactive
    transcript's last assistant usage, or None if there is nothing to
    measure."""
    directory = default_dir() if directory is None else directory
    path = newest_transcript(directory)
    if not path:
        return None
    usage = tail_last_assistant_usage(path)
    if not usage:
        return None
    return (usage.get("input_tokens") or 0) + (usage.get("cache_read_input_tokens") or 0) \
        + (usage.get("cache_creation_input_tokens") or 0)
