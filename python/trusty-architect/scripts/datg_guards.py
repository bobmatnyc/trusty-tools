"""datg_guards.py: the #10800 G1-G3 checks over an Intent Ledger (DOC-87, scope D0c).

Why: three gaps the provenance check (#10780) leaves open. G1: a policy file that CLAUDE.md imports is read
at every session start, and nothing compares its thresholds with the owner's live rulings, so a stale file
can restate a cap the owner changed. G2: a dispatch named a goal but no work card, and a card closed on an
outcome that cites no evidence, so "done" could not be checked against what was delivered. G3: checker and
repair work could take every running slot, or ready work could sit idle, and nothing reported it.
What:
- G1 `policy_flags`: reads <project>/CLAUDE.md (and .claude/CLAUDE.md, CLAUDE.local.md) and every file they
  import with `@path`, five hops deep as Claude Code does. A line stating `<number> <resource>` ("2 helpers")
  is a conflict when a current owner ruling (an answered item with source.by owner, not past `until`)
  quotes the same resource with other numbers. Relative counts ("two more agents") are not thresholds.
- G2 `card_check` (a dispatch must name `Card: <code>-Txx`, a not-done task under the named goal) and
  `evidence_gaps` (a task closed done needs evidence in its outcome: a URL, `#123`, a SHA, a file path,
  or a run/job/message id).
- G3 `delivery_flags`: idle ready work (open cards under active goals, none running, no agent recorded,
  no write for IDLE_MIN minutes, no hold), repair displacing delivery (every running card is checker or
  repair work while the top goal has open delivery cards) and a repair loop (REPAIR_LOOP or more
  checker/repair cards closed in 24 h and no delivery card).
Rollout (DOC-87 section 10): `config.guard_mode` is "shadow" (the default) or "enforce". In shadow G2 only
warns and appends to <datg>/guards.jsonl; in enforce the dispatch is denied and the close refused. G1 and G3
are flags in the resume digest and `datg.py guards`; they never block.
Test: tests/test_datg_guards.py
"""
import json
import os
import re
from datetime import datetime, timedelta, timezone

from datg_store import ID_RE, now_iso, parse_ts, paths, read_log

IDLE_MIN = 15
REPAIR_LOOP = 3
SHADOW = " (shadow: not enforced)"
IMPORT_DEPTH = 5
READ_MAX = 256 * 1024
NUM_WORDS = {w: i for i, w in enumerate(("zero one two three four five six seven eight nine ten eleven "
                                         "twelve").split())}
# `<number> <resource>`: no clock time, version, path or percentage on either side of the number.
PAIR = re.compile(r"(?<![\w:./-])(\d{1,3}|" + "|".join(NUM_WORDS) + r")(?![\w:./%-])\s+([a-z][a-z-]{2,})",
                  re.I)
NOT_RESOURCE = {"and", "the", "for", "with", "from", "that", "this", "are", "was", "were", "not", "but", "per",
                "each", "more", "less", "than", "into", "out", "other", "extra", "additional", "further"}
IMPORT = re.compile(r"(?:^|(?<=\s))@((?:~/|\.{1,2}/|/)?[\w.-][\w./-]*)")
CARD = r"Card:\s*({code}-T[0-9a-f]{{2}})\b"
EVIDENCE = re.compile(
    r"https?://\S+"                                     # a URL (PR, run, comment)
    r"|(?<![\w#])#\d{1,7}\b"                            # an issue or PR number
    r"|\b(?=[0-9a-f]*\d)(?=[0-9a-f]*[a-f])[0-9a-f]{7,40}\b"  # a commit SHA
    r"|\b[\w.-]+/[\w./-]*\.\w{1,8}\b"                   # a file path
    r"|\b(?:run|job|msg|message|id|pid)\s*[:#=]?\s*\d[\w-]*", re.I)  # a run, job or message id
REPAIR = re.compile(r"^\s*(?:repair|check|checker|fix-?up)\s*:"
                    r"|\b(?:repair\w*|checkers?|flak(?:e|es|y|iness)|re-?runs?|retr(?:y|ies)|tooling|lint|clippy"
                    r"|fix (?:the )?(?:gate|hook|checker|harness|ci|build))\b", re.I)


def guard_mode(st):
    """'enforce' when config.guard_mode says so, else 'shadow' (DOC-87 section 10: shadow first)."""
    return "enforce" if (st.get("config") or {}).get("guard_mode") == "enforce" else "shadow"


def record_guard(project, guard, text, ref=""):
    """Append one shadow record to <datg>/guards.jsonl; an unwritable file never stops the caller."""
    rec = {"ts": now_iso(), "session": os.environ.get("DATG_SESSION", ""), "guard": guard, "ref": ref,
           "text": text}
    try:
        with open(os.path.join(paths(project)[0], "guards.jsonl"), "a", encoding="utf-8") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
    except OSError:
        pass


# ---- G1: policy files the project instructions import --------------------------------

def _pairs(text):
    """{resource: {numbers}} for every `<number> <resource>` threshold in `text`."""
    out = {}
    for m in PAIR.finditer(str(text or "")):
        word = m.group(2).lower()
        if word in NOT_RESOURCE:
            continue
        res = word[:-1] if word.endswith("s") and len(word) > 3 else word
        n = m.group(1).lower()
        out.setdefault(res, set()).add(NUM_WORDS[n] if n in NUM_WORDS else int(n))
    return out


def _read(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return f.read(READ_MAX)
    except OSError:
        return None


def _imports(path, text):
    """Paths `text` imports with `@path`, outside code spans and fences, resolved from `path`'s directory."""
    out, fence = [], False
    for line in text.splitlines():
        if line.lstrip().startswith("```"):
            fence = not fence
            continue
        if fence:
            continue
        for m in IMPORT.finditer(re.sub(r"`[^`]*`", "", line)):
            p = m.group(1).rstrip(".,;:)")
            p = os.path.expanduser(p) if p.startswith("~/") else p
            out.append(p if os.path.isabs(p) else os.path.join(os.path.dirname(path), p))
    return out


def policy_files(project):
    """[(path, text)] of the project instruction files and every file they import, roots first."""
    queue = [(os.path.join(project, n), 0) for n in ("CLAUDE.md", os.path.join(".claude", "CLAUDE.md"),
                                                      "CLAUDE.local.md")]
    seen, out = set(), []
    while queue:
        path, depth = queue.pop(0)
        real = os.path.realpath(path)
        if real in seen or not os.path.isfile(real):
            continue
        seen.add(real)
        text = _read(real)
        if text is None:
            continue
        out.append((path, text))
        if depth < IMPORT_DEPTH:
            queue += [(p, depth + 1) for p in _imports(path, text)]
    return out


def owner_rulings(st, now):
    """[(item, source)] of the current owner rulings: answered, source.by owner, not past `until`."""
    out = []
    for i in st.get("items") or []:
        src = i.get("source") if isinstance(i.get("source"), dict) else {}
        if i.get("status") != "answered" or src.get("by") != "owner":
            continue
        until = parse_ts(src.get("until")) if src.get("until") else None
        if until is None or until > now:
            out.append((i, src))
    return out


def policy_flags(project, st, now=None):
    """G1: one FLAG line per policy-file line whose threshold differs from a current owner ruling."""
    now = now or datetime.now(timezone.utc)
    rulings = [(i, src, _pairs(src.get("quote"))) for i, src in owner_rulings(st, now)]
    rulings = [r for r in rulings if r[2]]
    if not rulings:
        return []
    flags = []
    for path, text in policy_files(project):
        rel = os.path.relpath(path, project)
        rel = path if rel.startswith("..") else rel
        for n, line in enumerate(text.splitlines(), 1):
            for res, nums in _pairs(line).items():
                for i, src, owner in rulings:
                    if res in owner and not nums & owner[res]:
                        flags.append(f"  FLAG policy conflict: {rel}:{n} says \"{' '.join(line.split())[:100]}\"; "
                                     f"owner ruling {i['id']} says \"{src.get('quote')}\" (owner turn: "
                                     f"{src.get('cite')}). The ruling holds; raise a D for the owner before "
                                     f"following the file.")
    return flags[:8]


# ---- G2: work card and outcome evidence ------------------------------------------------

def card_check(st, prompt, goal):
    """'' when `prompt` names `Card: <code>-Txx`, a not-done task under `goal`; else what is wrong."""
    m = re.search(CARD.format(code=re.escape(st["code"])), prompt or "")
    add = f"add one with `datg.py add T --goal {goal} --title ...`"
    if not m:
        return f"DATG: dispatch names no work card. Add 'Card: {st['code']}-Txx', an open task under {goal}; {add}."
    task = next((t for t in st.get("tasks") or [] if t.get("id") == m.group(1)), None)
    if task is None or task.get("status") == "done":
        return f"DATG: card {m.group(1)} is not an open task; {add}."
    if task.get("goal") != goal:
        return f"DATG: card {m.group(1)} is under {task.get('goal')}, not {goal}."
    return ""


def evidence_gaps(closed):
    """[(id, message)] for each task in `closed` [(kind, obj)] whose outcome cites no evidence."""
    return [(o["id"], f"DATG: {o['id']} closed with no evidence in its outcome ({o.get('outcome', '')[:60]!r}); "
             "cite a PR or issue (#123), a SHA, a URL, a file path or a run id.")
            for kind, o in closed if kind == "task" and not EVIDENCE.search(o.get("outcome") or "")]


def dispatch_card(project, st, prompt):
    """#10800 G2: (decision, message) for the work card of a dispatch whose goal check allowed it.

    What: 'allow' when no goal is active (no ledger in use) or the card is an open task under the named goal;
    else 'block' under config.guard_mode enforce, or 'warn' in shadow with a guards.jsonl record.
    Test: tests/test_datg_guards.py CardCase
    """
    goal = re.search(rf"Goal:\s*({re.escape(st['code'])}-G[0-9a-f]{{2}})\b", prompt or "")
    msg = card_check(st, prompt, goal.group(1)) if goal else ""
    if not msg:
        return "allow", ""
    if guard_mode(st) == "enforce":
        return "block", msg
    record_guard(project, "card", msg, goal.group(1))
    return "warn", msg + SHADOW


def guard_lines(project, st):
    """#10800 G1 and G3 flag lines for the digest and `datg.py guards`; a failed check is one visible line."""
    try:
        return policy_flags(project, st) + delivery_flags(st, read_log(project, 262144))
    except Exception as e:  # noqa: BLE001 - a guard never breaks the resume digest
        return [f"  FLAG guards not checked ({type(e).__name__}: {e})"]


# ---- G3: delivery displacement and idle ready work ----------------------------------------

def _is_task(item_id):
    m = ID_RE.match(str(item_id or ""))
    return bool(m) and m.group(2) == "T"


def _repair(title):
    return bool(REPAIR.search(str(title or "")))


def delivery_flags(st, records, now=None):
    """G3: FLAG lines for idle ready work, repair displacing delivery and a repair loop. Pure.

    records: the log as (datetime|None, kind, record) tuples, as read_log returns them.
    """
    now = now or datetime.now(timezone.utc)
    ses = st.get("session") or {}
    active = sorted((g for g in st.get("goals") or [] if g.get("status") == "active"), key=lambda g: g["rank"])
    ids = {g["id"] for g in active}
    tasks = [t for t in st.get("tasks") or [] if t.get("goal") in ids]
    ready = [t for t in tasks if t.get("status") == "open"]
    running = [t for t in tasks if t.get("status") == "running"]
    flags = []
    written = ses.get("written_epoch")
    idle = isinstance(written, int) and now.timestamp() - written >= IDLE_MIN * 60
    if ready and not running and not ses.get("agents") and not ses.get("hold") and idle:
        flags.append(f"  FLAG idle ready work: {', '.join(t['id'] for t in ready[:6])} ready, no card running "
                     f"and no agent recorded for {int((now.timestamp() - written) // 60)} min. Dispatch the top "
                     "card or record what it waits on.")
    waiting = [t for t in ready if active and t.get("goal") == active[0]["id"] and not _repair(t.get("title"))]
    if running and waiting and all(_repair(t.get("title")) for t in running):
        flags.append(f"  FLAG repair displacing delivery: running {', '.join(t['id'] for t in running[:6])} "
                     f"are checker/repair work; delivery cards {', '.join(t['id'] for t in waiting[:6])} under "
                     f"{active[0]['id']} wait. Run one delivery card or record why it cannot run.")
    since = now - timedelta(hours=24)
    done = [r for t, k, r in records if k == "done" and t and t >= since and _is_task(r.get("id"))]
    fixes = [r for r in done if _repair(r.get("note"))]
    if len(fixes) >= REPAIR_LOOP and len(fixes) == len(done):
        flags.append(f"  FLAG repair loop: {len(fixes)} checker/repair cards closed in 24 h "
                     f"({', '.join(r['id'] for r in fixes[-4:])}) and no delivery card. Pick the next "
                     "delivery card before another repair.")
    return flags
