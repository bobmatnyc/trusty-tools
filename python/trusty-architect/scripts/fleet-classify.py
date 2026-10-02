"""fleet-classify.py — pure pane parsing and alert evaluation for fleet-poll.py.

Why: fleet-poll.py grew past the 500-SLOC production cap (#8891). Its pure half
splits off cleanly: nothing here calls tmux, reads a file, or sends keys.
What: parse_pane() turns one pane capture into a dict of facts (input box via
scripts/input-state.py `classify`, dialog, question, ctx, plan, limit,
errors). evaluate() and fleet_plan_alerts() turn those facts into alert
dicts; dedup() splits alerts into new and suppressed. fleet-poll.py loads this
file by path and re-exports these names, so its callers see no change.
Test: tests/test_fleet_poll.py (through fleet-poll.py's re-exports)
"""
import hashlib
import importlib.util
import os
import re
import time
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))

IDLE_AFTER = 600          # seconds an idle pane waits before it alerts
REEMIT = {"idle": 1800, "ctx": 1800, "load": 1800, "disk": 1800, "swap": 1800,
          "self_ctx": 1800}
CTX_WARN, CTX_HIGH = 45, 50
PLAN_HIGH = 90
DETAIL_MAX = 200


def _load_by_path(name, filename):
    # A hyphenated script name isn't importable, so load it by path.
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, filename))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


input_state = _load_by_path("input_state", "input-state.py")

# --- pane parsing (pure) ----------------------------------------------------

ANSI = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][0-9A-Za-z]")
RULE = re.compile(r"^\s*[─━]{10,}\s*$")
# The box's own TOP rule only: Claude Code now draws a short label (e.g. a
# date) before the trailing rule run on days the input hasn't changed
# ("──…── 9-26/2026 ─"); the bottom rule and every dialog rule stay plain.
BOX_TOP_RULE = re.compile(r"^\s*[─━]{10,}(?:\s.{1,30}[─━]+)?\s*$")
OPTION = re.compile(r"^\s*❯\s*\d+\.")              # dialog cursor on a numbered option
SPINNER = re.compile(r"^[·✢✳✶✻✽*∗] ")               # status line glyph, column 0
ACTIVE = re.compile(r"…|esc to interrupt")          # a running turn, not "✻ Worked for 3m"
WAITING = re.compile(r"Waiting for \d+ background agent")
AGENT_ROW = re.compile(r"^\s*◯ ")
DIALOG_MARKERS = ("Do you want to proceed?", "Esc to cancel", "Enter to select")
FOOTER_NOISE = re.compile(r"^\s*TM \d|bypass permissions|/rc\s*$|Update installed|^\s*[◯⏺] ")
LIMIT = re.compile(r"(?i)usage limit reached|limit reached|hit your (?:usage )?limit|"
                   r"resets at|limit will reset")
ERROR = re.compile(r"Traceback|API Error|Error: Exit code")
TIMERS = re.compile(r"\d+m \d+s|\b\d+s\b|↓ [\d.]+k tokens|\d+:\d\d(?: [AP]M)?")
SECRET = re.compile(r"(?i)\b(\w*(?:key|token|secret|passw(?:or)?d|auth)\w*)\s*[=:]\s*\S+")
LONG_TOKEN = re.compile(r"\b(?=\w*\d)\w{32,}\b")    # API keys, hex digests


def fp(text):
    """Short stable fingerprint of a string."""
    return hashlib.sha1(text.encode()).hexdigest()[:12]


def clean(text):
    """One-line detail: whitespace collapsed, secret-looking values redacted, capped."""
    text = " ".join(text.split())
    text = SECRET.sub(r"\1=<redacted>", text)
    text = LONG_TOKEN.sub("<redacted>", text)
    return text[:DETAIL_MAX]


def find_box(lines):
    """(top_rule, bottom_rule) indexes of Claude Code's input box, or None.

    The box is the last ❯ line that sits directly under a horizontal rule and
    is not a dialog's numbered-option cursor.
    """
    for i in range(len(lines) - 1, 0, -1):
        if (lines[i].lstrip().startswith("❯") and not OPTION.match(lines[i])
                and BOX_TOP_RULE.match(lines[i - 1])):
            bottom = next((j for j in range(i + 1, len(lines)) if RULE.match(lines[j])),
                          len(lines) - 1)
            return i - 1, bottom
    return None


def find_dialog(lines, bottom):
    """(fingerprint_text, detail) of an open dialog below any input box, or None."""
    start = max(0, len(lines) - 25)
    if bottom is not None:
        start = max(start, bottom + 1)
    region = lines[start:]
    hits = [k for k, l in enumerate(region)
            if OPTION.match(l) or any(m in l for m in DIALOG_MARKERS)]
    if not hits:
        return None
    m = hits[-1]
    window = region[max(0, m - 20):m + 1]
    body = [l.strip() for l in window
            if l.strip() and not FOOTER_NOISE.search(l) and not RULE.match(l)]
    # The cursor position and timers change while the dialog itself does not.
    norm = "\n".join(TIMERS.sub("", l.replace("❯", " ")).strip() for l in body)
    # Header: first line under the dialog's top rule ("Bash command", "☐ Topic").
    rule = next((k for k, l in enumerate(window) if RULE.match(l)), None)
    header = next((l.strip() for l in window[rule + 1:] if l.strip()), None) \
        if rule is not None else None
    asks = [l for l in body if l.endswith("?")]
    parts = [p for p in (header, asks[-1] if asks else None) if p]
    return norm, " | ".join(dict.fromkeys(parts)) or body[0]


def current_status(lines, top):
    """The spinner/status line of the latest turn (e.g. '✻ Worked for 3m'), or ''."""
    for i in range(top - 1, -1, -1):
        l = lines[i]
        if not l.strip() or l[0].isspace():
            continue
        return l if SPINNER.match(l) else ""
    return ""


def last_question(lines, top):
    """The last line of the latest PM turn when it ends in '?', else None."""
    last, seen_status = None, False
    for i in range(top - 1, -1, -1):
        l = lines[i]
        if not l.strip():
            continue
        if l[0].isspace():
            if last is None:
                last = l.strip()
            continue
        if SPINNER.match(l) and not seen_status:
            seen_status, last = True, None
            continue
        if l.startswith("⏺"):
            if last is None:
                last = l[1:].strip()
            return last if last.endswith("?") else None
        return None  # the latest turn is the user's (❯) or unrecognised
    return None


def _pct(pattern, text):
    m = re.search(pattern, text)
    return int(m.group(1)) if m else None


def _screen_lines(capture):
    """Escape-stripped, right-trimmed screen rows with trailing blanks dropped.
    Shared by parse_pane and wake()'s own post-send confirmation."""
    lines = [ANSI.sub("", l).rstrip() for l in capture.splitlines()]
    while lines and not lines[-1].strip():
        lines.pop()
    return lines


def parse_pane(capture):
    """Classify one capture taken with escape codes. Pure; no tmux calls."""
    lines = _screen_lines(capture)
    box_state, box_text = input_state.classify(capture)
    box = find_box(lines)
    top, bottom = box if box else (None, None)
    footer = lines[bottom + 1:] if box else lines[-10:]
    dialog = find_dialog(lines, bottom)
    status = current_status(lines, top) if box else ""
    agents = sum(1 for l in footer if AGENT_ROW.match(l))
    waiting = bool(WAITING.search(status))
    spinning = bool(status) and bool(ACTIVE.search(status))
    busy = agents > 0 or waiting or spinning
    ftext = "\n".join(footer)
    tail = lines[-30:]
    return {
        "box": box_state if box else ("dialog" if dialog else box_state),
        "box_text": box_text,
        "has_box": box is not None,
        "dialog": dialog,
        "status": status,
        "agents": agents,
        "busy": busy,
        "ctx": _pct(r"\bctx (\d+)%", ftext),
        "plan5h": _pct(r"⏳\s*(\d+)%", ftext),
        "planwk": _pct(r"📅\s*(\d+)%", ftext),
        "question": None if (busy or dialog or not box) else last_question(lines, top),
        "limit": next((l.strip() for l in reversed(tail) if LIMIT.search(l)), None),
        "errors": sorted({l.strip() for l in tail if ERROR.search(l)}),
    }


# --- alert evaluation (pure) ------------------------------------------------

def iso(t=None):
    t = time.time() if t is None else t
    return datetime.fromtimestamp(t, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def alert(kind, fingerprint, detail, session="", pane="", now=None):
    detail = clean(detail)
    return {
        "ts": iso(now), "session": session, "pane": pane, "kind": kind,
        "fingerprint": fingerprint, "detail": detail,
        "display": (f"[{session}]: " if session else "") + f"{kind}: {detail}",
    }


def evaluate(info, ps, now, session, pane):
    """Alerts for one parsed pane. `ps` is this pane's mutable state (idle, errors)."""
    out = []

    def add(kind, f, detail):
        out.append(alert(kind, f, detail, session, pane, now))

    if info["dialog"]:
        norm, detail = info["dialog"]
        add("dialog", fp(norm), detail)
    if info["question"]:
        add("question", fp(info["question"]), info["question"])
    idle = (info["has_box"] and info["box"] in ("empty", "suggestion")
            and not info["dialog"] and not info["busy"])
    if idle:
        since = ps.get("idle_since")
        if since is None:
            since = ps["idle_since"] = now
        if now - since >= IDLE_AFTER:
            add("idle", f"since-{int(since)}", f"idle {int((now - since) // 60)} min, no agents running")
    else:
        ps["idle_since"] = None
    ctx = info["ctx"]
    if ctx is not None and ctx >= CTX_WARN:
        add("ctx", str(CTX_HIGH if ctx >= CTX_HIGH else CTX_WARN), f"ctx {ctx}%")
    if info["limit"]:
        add("plan-limit", fp(info["limit"]), info["limit"])
    prev = ps.get("errors")
    ps["errors"] = info["errors"]
    if prev is not None:  # first sight of a pane seeds the set; no alert flood at start
        new = [e for e in info["errors"] if e not in prev]
        if new:
            add("error", fp("\n".join(new)), f"{len(new)} new: {new[0]}")
    return out


def fleet_plan_alerts(infos, now):
    """Plan usage is account-wide: one alert for the fleet, not one per pane."""
    out = []
    for key, label in (("plan5h", "5-hour"), ("planwk", "weekly")):
        vals = [(i[key], s) for s, i in infos if i.get(key) is not None]
        if vals:
            v, s = max(vals)
            if v >= PLAN_HIGH:
                out.append(alert("plan-limit", f"{key}>={PLAN_HIGH}",
                                 f"{label} plan usage {v}% (status line of {s})", now=now))
    return out


def dedup(state, candidates, now):
    """Split candidates into (new, suppressed_count) and update state['alerts'].

    Key = session|pane|kind|fingerprint. A key absent this cycle is cleared, so
    its next appearance is new. A kind in REEMIT re-emits after its interval.
    """
    seen = state.get("alerts", {})
    active, new, suppressed = {}, [], 0
    for a in candidates:
        k = "|".join((a["session"], a["pane"], a["kind"], a["fingerprint"]))
        if k in active:
            continue
        prev = seen.get(k)
        every = REEMIT.get(a["kind"])
        if prev is None or (every and now - prev["last"] >= every):
            new.append(a)
            active[k] = {"first": prev["first"] if prev else now, "last": now}
        else:
            suppressed += 1
            active[k] = prev
    state["alerts"] = active
    return new, suppressed
