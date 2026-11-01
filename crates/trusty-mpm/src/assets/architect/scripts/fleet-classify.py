"""fleet-classify.py — pure pane parsing and alert evaluation for fleet-poll.py.

Why: fleet-poll.py grew past the 500-SLOC production cap (#8891). Its pure half
splits off cleanly: nothing here calls tmux, reads a file, or sends keys.
What: parse_pane() turns one pane capture into a dict of facts (input box via
scripts/input-state.py `classify`, dialog, question, ctx, plan, limit,
errors, and `turn_active`: a running spinner or a current-turn WAITING line,
never an agent row; turn-active.py reads it, #10579). evaluate() and fleet_plan_alerts() turn those facts into alert
dicts; dedup() splits alerts into new and suppressed. context_bar() reads the
one context bar (#9997): CONTEXT_BAR_TOKENS, default 250000, against
CONTEXT_WINDOW_TOKENS, default 1000000, for panes that show only a percent. fleet-poll.py loads this
file by path and re-exports these names, so its callers see no change.
Test: tests/test_fleet_poll.py (through fleet-poll.py's re-exports),
tests/test_fleet_poll_idle.py, tests/test_turn_active.py
"""
import hashlib
import importlib.util
import math
import os
import re
import time
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))

IDLE_AFTER = 600          # seconds an idle pane waits before it alerts
REEMIT = {"idle": 1800, "ctx": 1800, "load": 1800, "disk": 1800, "swap": 1800,
          "self_ctx": 1800, "architect_dead": 1800, "architect_silent": 1800,
          "architect_frozen": 1800,  # #10571: the watchdog's alerts re-emit as the prototype's do
          "owner_answer": 1800}  # #10670: until owner_answers.pending stops listing it
# #9997: one context bar in tokens; a pane's `ctx N%` is converted against the
# window. "approaching" fires at 90% of the bar, "bar" at the bar.
CONTEXT_BAR_DEFAULT = 250_000
CONTEXT_WINDOW_DEFAULT = 1_000_000
APPROACH = 0.9
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
# ("──…── 1-01/2026 ─"); the bottom rule and every dialog rule stay plain.
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
    is not a dialog's numbered-option cursor. The box ends at the first plain
    rule below it. With no plain bottom rule (#10670) the box runs to the
    end of the screen, so every row under the ❯ counts as box text, footer rows included.
    A draft on the next row is then never mistaken for an empty box. wake()
    reads the end-of-screen bottom as "no bottom rule" and sends no keys: on
    such a pane every wake skips, a stall rather than a typed-over draft.
    """
    for i in range(len(lines) - 1, 0, -1):
        if (lines[i].lstrip().startswith("❯") and not OPTION.match(lines[i])
                and BOX_TOP_RULE.match(lines[i - 1])):
            bottom = next((j for j in range(i + 1, len(lines)) if RULE.match(lines[j])),
                          len(lines))  # #10670: no rule -> every row below the ❯ is box text
            return i - 1, bottom
    return None


SGR_SEQ = re.compile(r"\x1b\[([0-9;]*)m")
_ANSI_SPLIT = re.compile(f"({ANSI.pattern})")


def _apply_sgr(params, dim, inverse):
    """(dim, inverse) after one SGR sequence's parameters; colour arguments are skipped."""
    codes, i = (params.split(";") if params else ["0"]), 0
    while i < len(codes):
        c = int(codes[i] or 0)
        if c in (38, 48, 58):  # 38;5;n or 38;2;r;g;b: the arguments are not codes
            i += 3 if codes[i + 1:i + 2] == ["5"] else 5
            continue
        if c == 0:
            dim = inverse = False
        dim = True if c == 2 else False if c == 22 else dim
        inverse = True if c == 7 else False if c == 27 else inverse
        i += 1
    return dim, inverse


def box_is_suggestion(capture):
    """True when every non-blank cell of the input box is dim (SGR 2) or inverse (SGR 7),
    and at least one is dim.

    Why: classify() calls a ❯ row a suggestion on any literal SGR 2, so a typed `/review`
    with a dim hint after it, or a dim row over an undimmed draft, read as a suggestion,
    and a dim row whose SGR 2 shares a sequence with another code did not (#10670).
    What: walks the capture from its first row, carrying SGR state across rows as
    `capture-pane -e` does (a wrapped dim row repeats no escape), and checks each cell
    between find_box()'s rules, skipping blanks and the ❯ glyph. An inverse cell is the
    cursor. False with no box, or with no dim cell.
    Test: test_a_box_is_a_suggestion_only_when_every_cell_is_dim_or_inverse,
    test_a_typed_cell_anywhere_in_the_box_is_not_a_suggestion,
    test_a_suggestion_wakes_only_when_the_whole_box_is_dim
    """
    rows = capture.splitlines()
    box = find_box(_screen_lines(capture))
    if not box:
        return False
    top, bottom = box
    dim = inverse = seen = False
    for n, row in enumerate(rows[:bottom]):
        glyph = n == top + 1  # the ❯ row's first ❯ is the prompt, not text
        for piece in _ANSI_SPLIT.split(row):
            if (sgr := SGR_SEQ.fullmatch(piece)):
                dim, inverse = _apply_sgr(sgr.group(1), dim, inverse)
                continue
            if n <= top or ANSI.fullmatch(piece):
                continue
            for ch in piece:
                if glyph and ch == "❯":
                    glyph = False
                elif not ch.isspace():
                    if not (dim or inverse):
                        return False
                    seen = seen or dim
    return seen


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


# A finished turn's summary line: "✻ Brewed for 42s · done 1:13 AM" (#10579).
COMPLETED = re.compile(r"^[·✢✳✶✻✽*∗] .*\bfor \d+\s*[smh]\b|· done ")


def current_waiting(lines, top):
    """True when the newest "Waiting for N background agent" line is of the current turn.

    Why: the capture carries scrollback, so stale WAITING lines made idle panes read busy.
    What: a WAITING line counts only if no completed-turn line and no submitted
    "❯ " turn (above the input box) follows it.
    Test: test_stale_waiting_lines_are_not_busy, test_newest_waiting_line_is_busy
    """
    end = top if top is not None else len(lines)
    w = max((i for i in range(end) if WAITING.search(lines[i])), default=None)
    if w is None:
        return False
    return not any(COMPLETED.search(l) or l.lstrip().startswith("❯")
                   for l in lines[w + 1:end])


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


def _footer_tokens(text):
    """Tokens a footer states outright, or None. tm's status line prints
    `ctx>200k` when Claude Code sends no window payload: a lower bound."""
    m = re.search(r"\bctx>(\d+)k\b", text)
    return int(m.group(1)) * 1000 if m else None


def _positive_int(env, name, default, warnings):
    raw = env.get(name)
    if raw is None or not raw.strip():
        return default
    try:
        value = int(raw.strip())
    except ValueError:
        value = 0
    if value <= 0:
        warnings.append(f"{name}={raw!r} is not a positive integer; using {default}")
        return default
    return value


def context_bar(env=None):
    """(bar_tokens, window_tokens, warnings) from the environment (#9997).

    An unset name takes its default. A value that is not a positive integer
    takes the default and adds a warning: the alert is never switched off.
    A bar at or above the window adds a warning too; ctx_lines() then stops
    the percent line at the window and token counts still compare in tokens.
    """
    env = os.environ if env is None else env
    warnings = []
    bar = _positive_int(env, "CONTEXT_BAR_TOKENS", CONTEXT_BAR_DEFAULT, warnings)
    window = _positive_int(env, "CONTEXT_WINDOW_TOKENS", CONTEXT_WINDOW_DEFAULT, warnings)
    if bar >= window:
        warnings.append(
            f"CONTEXT_BAR_TOKENS={bar} is not below CONTEXT_WINDOW_TOKENS={window}: "
            "a pane's ctx % alerts at 90% and 100% of its window; a token count "
            "in the footer alerts at the bar")
    return bar, window, warnings


def ctx_lines(bar, window):
    """(approaching_pct, bar_pct) for a pane's integer `ctx N%`."""
    bar_pct = min(bar, window) * 100 / window
    return math.floor(bar_pct * APPROACH), bar_pct


def ctx_level(info, bar, window):
    """"bar", "approaching" or None for one parsed pane."""
    tokens = info.get("ctx_tokens")
    if tokens is not None:
        if tokens >= bar:
            return "bar"
        return "approaching" if tokens >= bar * APPROACH else None
    ctx = info.get("ctx")
    if ctx is None:
        return None
    approaching, at_bar = ctx_lines(bar, window)
    if ctx >= at_bar:
        return "bar"
    return "approaching" if ctx >= approaching else None


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
    waiting = bool(WAITING.search(status)) or current_waiting(lines, top)
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
        "turn_active": spinning or waiting,  # #10579: agent rows never make a turn running
        "ctx": _pct(r"\bctx (\d+)%", ftext),
        "ctx_tokens": _footer_tokens(ftext),
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


def evaluate(info, ps, now, session, pane, bar=None):
    """Alerts for one parsed pane. `ps` is this pane's mutable state (idle, errors).
    `bar` is (bar_tokens, window_tokens); None reads context_bar()."""
    out = []
    bar_tokens, window = context_bar()[:2] if bar is None else bar

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
    level = ctx_level(info, bar_tokens, window)
    if level:
        seen = (f"ctx {info['ctx']}%" if info.get("ctx_tokens") is None
                else f"ctx>{info['ctx_tokens'] // 1000}k")
        add("ctx", level, f"{seen} ({level}; bar {bar_tokens} tokens)")
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
