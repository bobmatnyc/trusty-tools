#!/usr/bin/env python3
"""task_tag.py: the Task-ID tag grammar shared by datg.py, pm_intake.py and task_pulse.py (stdlib only).

Why: the owner 2026-11-10: every Architect, PM and subagent prompt carries a Task ID, and hand-backs report
which task was worked on. Design review MB1047 and D09
(the owner: warn-only first, refuse after 14 days at 90%+ compliance, earliest 2026-11-24).
What: two shapes, one module.
  Loose token (prompts, relays; anywhere in the text): `Task: <id>` where <id> is an IL id `ar-T0b`
    (optionally `@MMDD`, the task's creation day) or a dashboard id `T93`. Tasks live at Architect/PM level
    only (the owner 2026-11-10): a subagent dispatch carries its parent's task id, a hand-back never creates a task,
    and unplanned work stacks on an existing task or waits.
  Strict pulse tag (hand-backs; the LAST non-empty line, fixed key order, single spaces around pipes):
    `Task: ar-T0b@1110 | done: <text> | est: <text> | remaining: <text> | pct: <n>`
    done, est, remaining and pct are each optional but keep that order; an IL id needs @MMDD there;
    a bare dashboard id (`T93`) does not. pct is 0-100 with an optional %.
Mode: warn (default) or refuse (config.task_mode in a datg state, $TASK_TAG_MODE, or local/task-mode).
Misses are appended to inbox/task-compliance.jsonl. CLI: `relay-check` (for a relay wrapper; the shipped relay.sh does not call it yet), `has-tag TEXT`.
#11101: ported from the Architect prototype; the voice-relay marker matches any name, not one owner's.
Test: tests/test_task_tag.py
"""
import argparse
import json
import os
import re
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

IL_ID = r"[a-z][a-z0-9]{1,7}-T[0-9a-f]{2}"
DASH_ID = r"T[0-9a-f]{2,4}"
LOOSE_RE = re.compile(
    rf"(?<![\w-])Task:[ \t]*(?:(?P<il>{IL_ID})(?:@(?P<stamp>\d{{4}}))?|(?P<dash>{DASH_ID}))(?![\w@-])")
STRICT_RE = re.compile(
    rf"^Task: (?:(?P<il>{IL_ID})@(?P<stamp>\d{{4}})|(?P<dash>{DASH_ID}))"
    r"(?: \| done: (?P<done>[^|\n]{1,200}?))?"
    r"(?: \| est: (?P<est>[^|\n]{1,60}?))?"
    r"(?: \| remaining: (?P<remaining>[^|\n]{1,200}?))?"
    r"(?: \| pct: (?P<pct>\d{1,3})%?)?$")
VOICE_RE = re.compile(r"\[[A-Za-z][A-Za-z .-]{0,39} via Codex")   # #11101: `[<owner name> via Codex ...]`
GRAMMAR = "Task: <id>@<MMDD> | done: <text> | est: <text> | remaining: <text> | pct: <n>"
MODES = ("warn", "refuse")
REFUSE_EXIT = 3   # relay-check exits 3 only for an explicit refusal; any other failure (usage error, crash) lets the relay send


def inbox():
    return os.environ.get("ARCHITECT_INBOX_DIR") or os.environ.get("SUPERVISOR_INBOX_DIR") or os.path.join(ROOT, "inbox")


def _spec(m):
    if m.group("il"):
        return {"kind": "il", "id": m.group("il"), "stamp": m.group("stamp")}
    return {"kind": "dash", "id": m.group("dash"), "stamp": None}


def find_tasks(text):
    """Every loose `Task: <id>` token in text, as [{kind: il|dash, id, stamp}]."""
    return [_spec(m) for m in LOOSE_RE.finditer(text or "")]


def has_task(text):
    return bool(LOOSE_RE.search(text or ""))


def parse_pulse(text):
    """('none', None) | ('reject', reason) | ('ok', tag). Strict: the tag is the last non-empty line.

    Why: models drop pipes, quote values or put the tag mid-text (MB1047 C5); a malformed tag must be
    counted and kept in a rejects file, never silently dropped.
    """
    lines = [ln.rstrip() for ln in (text or "").splitlines() if ln.strip()]
    if not lines:
        return "none", None
    last = lines[-1]
    if not last.startswith("Task:"):
        if any(ln.lstrip().startswith("Task:") and "|" in ln for ln in lines):
            return "reject", "tag is not the last line"
        return "none", None
    m = STRICT_RE.match(last)
    if not m:
        return "reject", "malformed tag (grammar: " + GRAMMAR + ")"
    spec = _spec(m)
    pct = m.group("pct")
    spec.update(done=m.group("done"), est=m.group("est"), remaining=m.group("remaining"),
                pct=min(100, int(pct)) if pct is not None else None, line=last)
    return "ok", spec


TAGLINE_RE = re.compile(
    rf"^Task:[ \t]*(?:{IL_ID}(?:@\d{{4}})?|{DASH_ID})(?:[ \t]*\|[ \t]*[a-z]+:[^|\n]*)*[ \t]*$")


def strip_tags(text):
    """Text without its pure tag lines (`Task: <id>` followed only by `| key: value` fields), so a tag never trips
    the pm_intake ruling test (`?`, which, choose). A `Task:` line carrying other prose is kept: it may be an ask."""
    return "\n".join(ln for ln in (text or "").splitlines() if not TAGLINE_RE.match(ln.strip()))


def mode(override=None):
    """warn (default) or refuse: override, $TASK_TAG_MODE, then the one word in local/task-mode."""
    if override in MODES:
        return override
    env = os.environ.get("TASK_TAG_MODE")
    if env in MODES:
        return env
    try:
        with open(os.path.join(ROOT, "local", "task-mode"), encoding="utf-8") as f:
            word = f.read().strip()
        if word in MODES:
            return word
    except OSError:
        pass
    return "warn"


def log_miss(kind, reason, who=""):
    """Append one compliance line (kind dispatch|relay); never raises."""
    try:
        os.makedirs(inbox(), exist_ok=True)
        rec = {"ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "kind": kind, "who": who, "reason": reason}
        with open(os.path.join(inbox(), "task-compliance.jsonl"), "a", encoding="utf-8") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
    except OSError:
        pass


def relay_check(text, kind="", urgent=False, no_task=False, session="", mode_override=None):
    """(exit code, message) for a relay. Refuses only a task-assign relay with no Task token in refuse mode.

    Urgent relays, voice-relay turns and every kind but task-assign never refuse and are not logged.
    """
    if urgent or VOICE_RE.search(text or "") or (text or "").lstrip().startswith("URGENT") or kind != "task-assign":
        return 0, ""
    if no_task or has_task(text):
        return 0, ""
    log_miss("relay", "task-assign relay without a Task token", session)
    msg = "relay has no 'Task: <id>' token (task-assign relays carry one; --no-task to override)"
    if mode(mode_override) == "refuse":
        return 1, "REFUSED: " + msg
    return 0, "WARN: " + msg


def main(argv=None):
    ap = argparse.ArgumentParser(prog="task_tag.py")
    sub = ap.add_subparsers(dest="verb", required=True)
    rc = sub.add_parser("relay-check")
    rc.add_argument("--kind", default="")
    rc.add_argument("--urgent", action="store_true")
    rc.add_argument("--no-task", action="store_true")
    rc.add_argument("--session", default="")
    rc.add_argument("text")
    ht = sub.add_parser("has-tag")
    ht.add_argument("text")
    a = ap.parse_args(argv)
    if a.verb == "has-tag":
        return 0 if has_task(a.text) else 1
    code, msg = relay_check(a.text, a.kind, a.urgent, a.no_task, a.session)
    if msg:
        print(msg, file=sys.stderr)
    return REFUSE_EXIT if code == 1 else code


if __name__ == "__main__":
    sys.exit(main())
