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
Structured block (#11133, DOC-89 v1): one fenced `il` block of `key: value` lines; parse_block reads it and
  rejects on the first violation (never repairs, never masks a secret). parse_handback returns the block
  result when a block is present, else parse_pulse: the block wins, the legacy line stays accepted.
Mode: warn (default) or refuse (config.task_mode in a datg state, $TASK_TAG_MODE, or local/task-mode).
Misses are appended to inbox/task-compliance.jsonl. CLI: `relay-check` (for a relay wrapper; the shipped relay.sh does not call it yet), `has-tag TEXT`.
#11101: ported from the Architect prototype; the voice-relay marker matches any name, not one owner's.
Test: tests/test_task_tag.py, tests/test_il_block.py
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


# ---- structured IL block (DOC-89 v1, #11133 phase 1) ------------------------------------------------
# #11133: phase 1 parses the block beside the legacy line; no hook enforces it yet (DOC-89 section 12).

BLOCK_ID = r"[a-z][a-z0-9]{1,7}-[GDAT][0-9a-f]{2}"
BLOCK_ID_RE = re.compile(rf"^{BLOCK_ID}$")
BLOCK_FENCE_RE = re.compile(r"^(`{3,}|~{3,})[ \t]*il[ \t]*$")
BLOCK_LINE_RE = re.compile(r"^([a-z]+): (.+)$")
BLOCK_REQUIRED = ("goal", "task", "done", "pct", "next")
BLOCK_SINGLE = BLOCK_REQUIRED + ("est", "remaining", "open", "error", "feedback")
BLOCK_REPEAT = ("new", "evidence")
BLOCK_LINE_MAX = 240
BLOCK_DONE_MAX = 200
BLOCK_NEW_RE = re.compile(r"^(?P<type>[DA]) (?P<title>[^|]+?)(?: \| (?P<k>opts|decided): (?P<v>.+))?$")


def _fence_close(fence):
    """The closing-fence pattern for an opening fence: the same character, at least as many, nothing after."""
    return re.compile(rf"^{re.escape(fence[0])}{{{len(fence)},}}[ \t]*$")


def find_block(text):
    """(start, body_lines, error). start is None when text holds no `il` fence; error names an unclosed or
    second block. Pure. Lines keep no trailing whitespace."""
    lines = [ln.rstrip() for ln in (text or "").splitlines()]
    found, i = None, 0
    while i < len(lines):
        m = BLOCK_FENCE_RE.match(lines[i].strip())
        if not m:
            i += 1
            continue
        close = _fence_close(m.group(1))
        j = next((k for k in range(i + 1, len(lines)) if close.match(lines[k].strip())), None)
        if j is None:
            return (found or (i, None))[0], None, "il block has no closing fence"
        if found is not None:
            return found[0], None, "more than one il block"
        found = (i, lines[i + 1:j])
        i = j + 1
    return (found[0], found[1], None) if found else (None, None, None)


def _secret_shape(value):
    """The shape name of the first secret-shaped span in value, or None. Reuses dashboard_mask's rules as a
    detector: any span it would mask is a secret here, and the block is rejected, never masked (#11133)."""
    import dashboard_mask   # lazy: only a block with content pays the import
    # mask_pane_text over the one line, compared with the same line escape-stripped: the masker's public entry.
    masked = dashboard_mask.mask_pane_text(value, max_lines=1, max_line_len=10 * len(value) + 64)
    if masked == dashboard_mask.strip_escapes(value):
        return None
    m = re.search(r"\[masked:([a-z0-9\-]+)\]", masked)
    return m.group(1) if m else "secret"


def _check_id(value, letters, key):
    if not BLOCK_ID_RE.match(value) or value.split("-")[1][0] not in letters:
        return f"{key}: bad id {value!r} (want <code>-<{'|'.join(letters)}><hex2>, code [a-z][a-z0-9]{{1,7}})"
    return None


def _parse_new(value):
    """{type, title, opts: [{l, rec}], decided} for one `new:` value, or a reason string."""
    m = BLOCK_NEW_RE.match(value)
    if not m:
        return "new: want 'D|A <title> | opts: a*; b' or 'D <title> | decided: <option> @ <cite>'"
    item = {"type": m.group("type"), "title": m.group("title").strip(), "opts": [], "decided": None}
    if m.group("k") == "opts":
        opts = [o.strip() for o in m.group("v").split(";")]
        if any(not o or o == "*" for o in opts):
            return "new: an empty option in opts"
        item["opts"] = [{"l": o.rstrip("*").rstrip(), "rec": o.endswith("*")} for o in opts]
        if sum(o["rec"] for o in item["opts"]) > 1:
            return "new: more than one recommended option (*)"
    elif m.group("k") == "decided":
        item["decided"] = m.group("v").strip()
    return item


def parse_block(text):
    """('none', None) | ('reject', first violation) | ('ok', record) for the one fenced `il` block in text.

    Why: DOC-89 v1 (#11133): every PM turn and agent handback carries one `il` block that scripts parse
    directly; a block that breaks the grammar is rejected with its first violation, never repaired.
    What: finds the fence (``` or ~~~, info string `il`), then reads each non-blank body line as `key: value`
    (DOC-89 section 7). Rejects, in line order: a line outside the grammar or an unknown key, a repeated
    singleton, a line over 240 characters (`done` over 200), a bad id (goal G, task T with @MMDD, open D/A),
    pct not an integer 0-100, a malformed `new`, a secret-shaped span (dashboard_mask rules); then a missing
    required key. The record has parse_pulse's fields (kind "il", id, stamp, done, est, remaining, pct, line)
    plus source "block", goal, next, open (list), new (list of items), evidence (list), error and feedback.
    Unlike parse_pulse, pct is rejected out of range, never clamped.
    Test: tests/test_il_block.py
    """
    start, body, err = find_block(text)
    if start is None:
        return "none", None
    if err:
        return "reject", err
    seen = {}
    for n, line in enumerate(body, 1):
        if not line.strip():
            continue
        m = BLOCK_LINE_RE.match(line)
        if not m:
            return "reject", f"line {n}: not 'key: value' (key, colon, one space)"   # never echo: may hold a secret
        key, value = m.group(1), m.group(2).strip()
        if key not in BLOCK_SINGLE and key not in BLOCK_REPEAT:
            return "reject", f"line {n}: unknown key {key!r}"
        if key in BLOCK_SINGLE and key in seen:
            return "reject", f"line {n}: repeated key {key!r}"
        if len(line) > BLOCK_LINE_MAX:
            return "reject", f"line {n}: {key} is {len(line)} characters (max {BLOCK_LINE_MAX})"
        if not value:
            return "reject", f"line {n}: {key} is empty"
        shape = _secret_shape(value)
        if shape:
            return "reject", f"line {n}: {key} holds a secret-shaped span ({shape}); remove it, never mask it"
        bad = None
        if key == "done" and len(value) > BLOCK_DONE_MAX:
            bad = f"done is {len(value)} characters (max {BLOCK_DONE_MAX})"
        elif key == "goal":
            bad = _check_id(value, "G", "goal")
        elif key == "task":
            tid, at, stamp = value.partition("@")
            bad = _check_id(tid, "T", "task") or (None if at and re.fullmatch(r"\d{4}", stamp) else
                                                   f"task: want <id>@MMDD, got {value!r}")
        elif key == "pct":
            bad = None if re.fullmatch(r"\d{1,3}", value) and int(value) <= 100 else \
                f"pct must be an integer 0-100, got {value!r}"
        elif key == "open":
            bad = next((_check_id(i, "DA", "open") for i in value.split() if _check_id(i, "DA", "open")), None)
        elif key == "new":
            item = _parse_new(value)
            bad, value = (item, value) if isinstance(item, str) else (None, item)
        if bad:
            return "reject", f"line {n}: {bad}"
        if key in BLOCK_REPEAT:
            seen.setdefault(key, []).append(value)
        else:
            seen[key] = value
    missing = [k for k in BLOCK_REQUIRED if k not in seen]
    if missing:
        return "reject", f"missing required key {missing[0]!r}"
    tid, _, stamp = seen["task"].partition("@")
    return "ok", {"kind": "il", "id": tid, "stamp": stamp, "done": seen["done"], "est": seen.get("est"),
                  "remaining": seen.get("remaining"), "pct": int(seen["pct"]),
                  "line": "\n".join(ln for ln in body if ln.strip()), "source": "block", "goal": seen["goal"],
                  "next": seen["next"], "open": seen.get("open", "").split(), "new": seen.get("new", []),
                  "evidence": seen.get("evidence", []), "error": seen.get("error"), "feedback": seen.get("feedback")}


def parse_handback(text):
    """The block result when text holds a fenced `il` block, else parse_pulse(text) (#11133, DOC-89 section 9).

    The block wins over a legacy `Task:` last line in the same text; a text with only the legacy line keeps
    the legacy parse (no flag day). Same ('none'|'reject'|'ok', ...) pair as both parsers.
    Test: tests/test_il_block.py
    """
    if find_block(text)[0] is not None:
        return parse_block(text)
    return parse_pulse(text)


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
