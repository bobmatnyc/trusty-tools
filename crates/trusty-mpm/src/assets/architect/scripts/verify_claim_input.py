"""verify_claim_input.py - event parsing, the read-only guard and the repo map for verify-claim.py (#10161).

Why: verify-claim.py outgrew the 500-line cap; its input half is a module of
  its own, imported by name from the same directory.
What: `parse_event` / `load_event` validate one event (BadInput on a bad one),
  `guard_read_only` refuses every command outside the read-only set,
  `default_runner` runs one command with a timeout, and `repo_maps` turns
  `repos` in local/architect.json into the bare-name and owner maps the
  Verifier routes by. Standard library plus the sibling architect_config.
Test: tests/test_verify_claim.py
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from typing import Callable

KINDS = (
    "clearance_request", "merged", "published", "paused",
    "status", "improvement", "question", "fyi",
)
REST_PATH = re.compile(r"repos/[\w.-]+/[\w.-]+/(pulls|issues|commits)/[\w./-]+(\?per_page=\d+)?")
# The crates.io crate endpoint; a crate's URL is CRATES_API + "/" + name.
CRATES_API = "https://crates.io/api/v1/crates"
Runner = Callable[[list, "dict | None"], tuple]  # (argv, env) -> (rc, stdout, stderr)


class BadInput(Exception):
    """The event is not a usable event object."""


def default_runner(argv: list, env: dict | None = None) -> tuple:
    """Run one command with a timeout; never raises on a missing binary."""
    try:
        p = subprocess.run(argv, capture_output=True, text=True, env=env, timeout=60)
    except (OSError, subprocess.TimeoutExpired) as e:
        return 127, "", f"{type(e).__name__}: {e}"
    return p.returncode, p.stdout, p.stderr


def _git_sub(argv: list) -> str:
    i = 1
    while i < len(argv) and argv[i] == "-C":
        i += 2
    return argv[i] if i < len(argv) else ""


def tool_is_network(argv: list) -> bool:
    return argv[0] in ("gh", "curl")


def guard_read_only(argv: list) -> None:
    """Refuse any command outside the read-only set this script may run."""
    tool = argv[0]
    if tool == "gh":
        ok = tuple(argv[1:3]) in {("pr", "view"), ("pr", "checks"), ("run", "view")} or (
            len(argv) == 3 and argv[1] == "api" and bool(REST_PATH.fullmatch(argv[2])))
    elif tool == "git":
        ok = _git_sub(argv) in {"rev-parse", "merge-base", "rev-list", "diff"}
    elif tool == "curl":
        bad = {"-X", "-d", "--data", "-T", "-o", "-O", "--request", "--upload-file"}
        ok = argv[-1].startswith(CRATES_API + "/") and not bad & set(argv)
    else:
        ok = False
    if not ok:
        raise PermissionError(f"verify-claim refuses non-read-only command: {argv[:3]}")



def repo_maps(config) -> tuple:
    """`(repos, owners)` from an architect_config Config.

    repos:  bare name (lower case) -> (owner/name, checkout path or "").
    owners: GitHub owner (lower case) -> gh config dir, or None to keep the
            caller's default config. An owner listed under two names takes the
            first non-empty gh_config_dir.
    """
    repos, owners = {}, {}
    for name, entry in config.repos.items():
        repos[name.lower()] = (entry.slug, entry.checkout)
        owner = entry.slug.split("/")[0].lower()
        if not owners.get(owner):
            owners[owner] = entry.gh_config_dir or None
    return repos, owners


def parse_event(raw: object) -> dict:
    """Validate the event shape; returns a dict with every field present."""
    if not isinstance(raw, dict):
        raise BadInput("event must be a JSON object")
    ev = {k: raw.get(k) for k in (
        "ts", "session", "kind", "repo", "pr", "branch", "head", "base",
        "merge_sha", "crate", "version", "ci_run", "claims", "asks", "text_sha")}
    if ev["kind"] not in KINDS:
        raise BadInput(f"kind must be one of {', '.join(KINDS)}; got {ev['kind']!r}")
    for k in ("pr", "ci_run"):
        v = ev[k]
        if isinstance(v, bool) or not (v is None or isinstance(v, int)
                                       or (isinstance(v, str) and v.isdigit())):
            raise BadInput(f"{k} must be an integer or null; got {v!r}")
        ev[k] = None if v is None else int(v)
    for k in ("repo", "branch", "head", "base", "merge_sha", "crate", "version"):
        if ev[k] is not None and not isinstance(ev[k], str):
            raise BadInput(f"{k} must be a string or null; got {ev[k]!r}")
    claims = ev["claims"] if ev["claims"] is not None else {}
    if not isinstance(claims, dict):
        raise BadInput("claims must be an object or null")
    for k in ("files", "commits"):
        v = claims.get(k)
        if v is not None and (isinstance(v, bool) or not isinstance(v, int)):
            raise BadInput(f"claims.{k} must be an integer; got {v!r}")
    chk = claims.get("checks")
    if chk is not None:
        if not isinstance(chk, dict) or not all(
                k in ("pass", "fail", "pending") and isinstance(v, int) and not isinstance(v, bool)
                for k, v in chk.items()):
            raise BadInput("claims.checks must be {pass,fail,pending: integer}")
    issues = claims.get("issues")
    if issues is not None:
        if not isinstance(issues, list) or not all(
                isinstance(i, dict) and isinstance(i.get("n"), int) and not isinstance(i.get("n"), bool)
                and str(i.get("state")).lower() in ("open", "closed")
                and isinstance(i.get("reason", ""), str) for i in issues):
            raise BadInput("claims.issues must be a list of {n: integer, state: open|closed, reason?: string}")
    ev["claims"] = claims
    return ev


def load_event(arg: str, stdin=None) -> dict:
    """Read the event from a file path, "-" (stdin) or a literal JSON object."""
    if arg == "-":
        text = (stdin or sys.stdin).read()
    elif arg.lstrip().startswith("{"):
        text = arg
    elif os.path.isfile(arg):
        with open(arg) as f:
            text = f.read()
    else:
        raise BadInput(f"not a file, '-' or JSON object: {arg[:60]!r}")
    try:
        return parse_event(json.loads(text))
    except ValueError as e:
        raise BadInput(f"invalid JSON: {e}") from None
