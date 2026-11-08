"""Alert triage: which poller alerts need the Architect, which are only recorded.

Why: from 19:00-20:06Z on 2026-10-09 about 30 wakes were alerts the Architect
already knew (idle on a held session, ctx below the bar, token info, rc_restored).
Each wake cost an Opus turn and the context reached 300k (the owner 20:06Z: "make these
checks deterministic. And use Haiku to tell us if we need to investigate more").
What: `decide` is a pure rule function returning wake / log / ask_haiku from the data
file local/triage-rules.json (holds the Architect edits). `triage` batches every
ask_haiku alert into ONE injected Haiku call and fails open to wake on any problem.
`append_records` writes one inbox/triage.jsonl line per alert. Imported by fleet-poll.py.
Test: scripts/test_fleet_poll_triage.py
CLI: `alert_triage.py --replay N` runs the real gate over the last N alerts of
inbox/alerts.jsonl and prints the counts, latency and call cost (no files written).
"""
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
MODEL = "claude-haiku-5-5"
# The poller has no CLAUDE_CONFIG_DIR; ~/.claude's login expired, this dir's is live.
CLAUDE_CONFIG_DIR = os.path.expanduser("~/.trusty-tools/trusty-mpm/claude-config")
HAIKU_TIMEOUT = 90  # seconds one batched call may take before it fails open
PANE_LINES = 30
CTX_BAR_PCT = 25           # ruling b7 2026-10-07: refresh at 25% of a 1M window
CTX_BAR_TOKENS = 250_000   # the same bar in tokens, for token_watch ctx_main
RC_FAIL_WAKE = 3           # rc_failed alerts in a row on one session

WAKE_KINDS = {"permission_prompt", "dialog", "question", "exited", "session_exited", "self_ctx",
              "band", "status_request", "user_state", "effort", "pm_message",
              "il_cron", "ctxwatch", "ctxcycle"}
TOKEN_LOG = {"hit_low", "miss_idle", "miss_unexpected", "ctx_sub"}  # also feed the token optimizer


def load_rules(path):
    """Rules from `path`; a missing or unreadable file is no holds, never an error."""
    try:
        with open(path) as f:
            r = json.load(f)
        if not isinstance(r, dict) or not isinstance(r.get("holds", []), list):
            raise ValueError("bad shape")
    except (OSError, ValueError):
        return {"holds": []}
    r.setdefault("holds", [])
    return r


def _ts(text):
    return datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc).timestamp()


def hold_active(hold, now):
    """A hold with `until` lasts while now < until; one with only `until_event` lasts until removed."""
    until = hold.get("until")
    if not until:
        return bool(hold.get("until_event"))
    try:
        return now < _ts(until)
    except ValueError:
        return False  # an unparseable timestamp must not silence anything


def find_hold(rules, session):
    """The hold entry for `session` (with or without the tm- prefix), expired or not."""
    for h in rules.get("holds", []):
        name = h.get("session", "")
        if name and session in (name, "tm-" + name):
            return h
    return None


def hold_covers(hold, kind):
    """A hold applies to the alert kinds in its `kinds` list; no list means idle only."""
    kinds = hold.get("kinds")
    return kind in (kinds if isinstance(kinds, list) else ["idle"])


# --- urgent classes and the Architect-wide hold file (ar-T8a) --------------------

# Alert kinds that wake the Architect even while it is held. The poller (fleet_alert_dedupe.py) reads this one set.
URGENT_KINDS = {"permission_prompt", "dialog", "question", "exited", "session_exited", "self_ctx",
                "architect_dead", "architect_silent", "architect_frozen"}
URGENT_SEVERITY_KINDS = {"swap", "disk"}   # urgent only at severity "critical"
PM_WAKE_KINDS = {"decision", "blocker", "head", "done"}   # pm_intake.WAKE_MSG_KINDS
HOLD_FILE = "hold.json"


def is_urgent(alert):
    """True when `alert` must wake the Architect while a hold is active.

    What: a kind in URGENT_KINDS, any owner_* kind, a swap or disk alert at severity critical, or a
    pm_message whose msg_kind is decision/blocker/head/done or missing (pm_intake wakes an unknown kind).
    Test: test_urgent_classes_wake_while_held
    """
    kind = alert.get("kind", "")
    if kind in URGENT_KINDS or kind.startswith("owner_"):
        return True
    if kind in URGENT_SEVERITY_KINDS:
        return alert.get("severity") == "critical"
    if kind == "pm_message":
        return alert.get("msg_kind") in PM_WAKE_KINDS or not alert.get("msg_kind")
    return False


def _hold_entries(data):
    """The hold entries of a parsed hold.json: {"holds": {source: entry}}, or one bare entry (the plan's shape)."""
    if not isinstance(data, dict):
        raise ValueError("hold file is not a JSON object")
    if isinstance(data.get("holds"), dict):
        return {k: v for k, v in data["holds"].items() if isinstance(v, dict)}
    if any(k in data for k in ("until", "until_event", "reason")):
        return {"file": data}
    raise ValueError("hold file has no holds")


def read_hold(path, now):
    """{"held", "reason", "allow", "error"} for the Architect-wide hold file at `path`.

    Why: ar-T8a, a paused Architect was woken 16 times by one unchanged alert; nothing told the poller it was held.
    What: absent file: not held. An entry is active per hold_active (until in the future, or until_event with no
    until). An unparseable file or timestamp is NOT held and `error` says why: a broken hold never silences.
    `allow` is the union of the active entries' extra kinds that still wake.
    Test: test_an_expired_or_unparseable_hold_is_not_held
    """
    out = {"held": False, "reason": "", "allow": set(), "error": None}
    try:
        with open(path, encoding="utf-8") as f:
            entries = _hold_entries(json.load(f))
    except FileNotFoundError:
        return out
    except (OSError, ValueError) as e:
        out["error"] = f"{type(e).__name__}: {e}"
        return out
    active = {}
    for src, h in entries.items():
        try:
            if hold_active(h, now):
                active[src] = h
        except (TypeError, AttributeError) as e:   # a non-string until: not held, said
            out["error"] = f"{src}: {type(e).__name__}: {e}"
    if active:
        out["held"] = True
        out["reason"] = "; ".join(f"{src}: {h.get('reason', '')}" for src, h in sorted(active.items()))
        out["allow"] = {k for h in active.values() if isinstance(h.get("allow"), list)
                        for k in h["allow"] if isinstance(k, str)}
    return out


def _edit_hold(path, source, entry):
    """Set (entry is a dict) or remove (None) one source's hold; the file goes when no source is left.
    An unparseable file is replaced: its hold was already not held. Serialised by flock on <path>.lock."""
    import fcntl
    if entry is None and not os.path.lexists(path):
        return False   # nothing to clear: create neither the file nor its lock
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    with open(path + ".lock", "a") as lk:
        fcntl.flock(lk, fcntl.LOCK_EX)
        try:
            with open(path, encoding="utf-8") as f:
                holds = _hold_entries(json.load(f))
        except FileNotFoundError:
            holds = {}
        except (OSError, ValueError):
            holds = {}
        if entry is None and source not in holds:
            return False
        if entry is not None and holds.get(source) == entry:
            return False
        if entry is None:
            holds.pop(source)
        else:
            holds[source] = entry
        if not holds:
            os.remove(path)
            return True
        tmp = path + ".tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump({"holds": holds}, f, indent=1, sort_keys=True)
        os.replace(tmp, path)
        return True


def set_hold(path, source, reason, until=None, until_event=None):
    """Record `source`'s hold; True when the file changed. Without `until` it lasts until clear_hold(source).
    Test: test_ledger_hold_writes_and_clears_the_hold_file, test_user_state_sleeping_holds_and_active_releases"""
    return _edit_hold(path, source, {"reason": str(reason)[:300], "until": until,
                                     "until_event": until_event or f"{source} cleared", "allow": []})


def clear_hold(path, source):
    """Remove `source`'s hold; True when the file changed. An absent file is left absent."""
    return _edit_hold(path, source, None)


def ledger_hold_path(project):
    """<inbox>/hold.json when `project` is an Architect's own ledger (<arch>/local/il), else None.

    Why: the Architect records a stop or pause as its ledger HOLD (`datg.py session set --hold`); a PM's ledger
    hold is that PM's business, not the Architect's alerting.
    What: <arch> holds scripts/fleet-poll.py; the inbox is ARCHITECT_INBOX_DIR, else <arch>/inbox, as the poller's.
    Test: test_ledger_hold_writes_and_clears_the_hold_file
    """
    il = os.path.realpath(project)
    arch = os.path.dirname(os.path.dirname(il))
    if os.path.basename(il) != "il" or os.path.basename(os.path.dirname(il)) != "local":
        return None
    if not os.path.isfile(os.path.join(arch, "scripts", "fleet-poll.py")):
        return None
    return os.path.join(os.environ.get("ARCHITECT_INBOX_DIR") or os.path.join(arch, "inbox"), HOLD_FILE)


def _res(decision, name, reason):
    return {"decision": decision, "path": f"rule:{name}", "reason": reason}


def decide(alert, rules, now, rc_state):
    """Pure but for `rc_state` ({session: consecutive rc_failed count}), which it updates."""
    kind, sess, detail = alert["kind"], alert.get("session", ""), alert.get("detail", "")
    if kind.startswith("owner_") or kind.startswith("architect_") or kind in WAKE_KINDS:
        return _res("wake", "always_wake", f"{kind} needs a decision or action")
    if kind == "ctx":
        m = re.search(r"ctx (\d+)%", detail)
        pct = int(m.group(1)) if m else CTX_BAR_PCT
        if pct >= CTX_BAR_PCT:
            h = find_hold(rules, sess)
            if h and hold_active(h, now) and hold_covers(h, "ctx"):
                until = h.get("until") or h.get("until_event")
                return _res("log", "ctx_hold", f"hold: {h.get('reason', '')} (until {until})")
            return _res("wake", "ctx_at_bar", f"ctx {pct}% at or above the {CTX_BAR_PCT}% refresh bar")
        return _res("log", "ctx_below_bar", f"ctx {pct}% below the {CTX_BAR_PCT}% bar")
    if kind == "token":
        sub = alert.get("fingerprint", "").split(":")[0]
        if sub == "ctx_main":
            m = re.search(r"ctx (\d+)", detail)
            tokens = int(m.group(1)) if m else CTX_BAR_TOKENS
            if tokens >= CTX_BAR_TOKENS:
                return _res("wake", "token_ctx_main_at_bar", f"main ctx {tokens} at or above {CTX_BAR_TOKENS}")
            return _res("log", "token_ctx_main_below_bar", f"main ctx {tokens} below {CTX_BAR_TOKENS}")
        if sub in TOKEN_LOG:
            return _res("log", "token_info", f"token {sub} feeds the daily optimizer")
    if kind == "rc_restored":
        rc_state.pop(sess, None)
        return _res("log", "rc_restored", "Remote Control restored")
    if kind == "rc_failed":
        rc_state[sess] = rc_state.get(sess, 0) + 1
        if rc_state[sess] >= RC_FAIL_WAKE:
            return _res("wake", "rc_failed_repeat", f"rc_failed {rc_state[sess]} times in a row")
    if kind == "idle":
        h = find_hold(rules, sess)
        if h and hold_active(h, now) and hold_covers(h, "idle"):
            until = h.get("until") or h.get("until_event")
            return _res("log", "idle_hold", f"hold: {h.get('reason', '')} (until {until})")
    return {"decision": "ask_haiku", "path": "haiku", "reason": "no rule matched"}


# --- Haiku gate ------------------------------------------------------------------

_REDACT = [
    (re.compile(r"[\w.+-]+@[\w-]+(\.[\w-]+)+"), "<email>"),
    (re.compile(r"\b(?:gh[pousr]_|github_pat_|sk-|xox[abprs]-|AKIA)[A-Za-z0-9_-]{8,}"), "<secret>"),
    (re.compile(r"(?i)\b(bearer|token|password|secret|api[_-]?key)\b(\s*[:=]\s*)\S+"), r"\1\2<secret>"),
]
INSTRUCTIONS = (
    "You triage alerts from a fleet poller for a supervisor. For each alert decide whether the "
    "supervisor must investigate (investigate=true: a session is stuck, blocked on something "
    "unowned, erroring, or needs a decision) or the alert is known/benign (false: matches its "
    "hold, a session waiting on a named person, normal noise). When unsure, say true. Reply with "
    "ONLY a JSON array, one object per alert: "
    '[{"id": "<alert id>", "investigate": true|false, "reason": "<one line>"}]\n\n'
)


def redact(text):
    for pat, rep in _REDACT:
        text = pat.sub(rep, text)
    return text


def build_prompt(items, panes, holds):
    """`items` = [(id, alert)]; `panes` {session: tail}; `holds` {session: hold entry}."""
    payload = []
    for i, a in items:
        s = a.get("session", "")
        payload.append({"id": i, "kind": a["kind"], "session": s, "detail": a.get("detail", ""),
                        "hold": holds.get(s), "pane_tail": panes.get(s, "")})
    return redact(INSTRUCTIONS + json.dumps(payload, ensure_ascii=False, indent=1))


def parse_haiku(out):
    """{id: (investigate, reason)} from the first JSON array in `out`; ValueError otherwise."""
    m = re.search(r"\[.*\]", out or "", re.S)
    if not m:
        raise ValueError("no JSON array in the reply")
    data = json.loads(m.group(0))
    res = {}
    for d in data:
        if not (isinstance(d, dict) and isinstance(d.get("id"), str)
                and isinstance(d.get("investigate"), bool) and isinstance(d.get("reason"), str)):
            raise ValueError(f"bad verdict {d!r}")
        res[d["id"]] = (d["investigate"], d["reason"])
    return res


def run_haiku(prompt):
    """One `claude -p` call on the plan login (no API key). Raises on timeout or non-zero exit."""
    env = dict(os.environ)
    env.setdefault("CLAUDE_CONFIG_DIR", CLAUDE_CONFIG_DIR)
    r = subprocess.run(
        ["claude", "-p", "--model", MODEL, "--tools", "", "--disable-slash-commands",
         "--strict-mcp-config"],
        input=prompt, capture_output=True, text=True, timeout=HAIKU_TIMEOUT,
        cwd=tempfile.gettempdir(), env=env)
    if r.returncode != 0:
        # claude prints auth failures on stdout and leaves stderr empty
        why = r.stderr.strip() or r.stdout.strip()
        raise RuntimeError(f"claude exited {r.returncode}: {why[:120]}")
    return r.stdout


def _tail(text):
    return "\n".join(text.splitlines()[-PANE_LINES:])


def _gate(items, rules, now, runner, pane_fn):
    """{id: result} for the ask_haiku alerts; every failure is a wake (fail open)."""
    def fail(why):
        return {i: {"decision": "wake", "path": "haiku", "reason": f"haiku fail-open: {why}"}
                for i, _ in items}
    if runner is None and os.environ.get("TRIAGE_HAIKU", "").lower() == "off":
        return fail("gate disabled (TRIAGE_HAIKU=off)")
    panes, holds = {}, {}
    for _, a in items:
        s = a.get("session", "")
        if not s or s in panes:
            continue
        try:
            panes[s] = _tail(pane_fn(s)) if pane_fn else ""
        except Exception:  # a dead pane must not stop the gate; the model sees an empty tail
            panes[s] = ""
        h = find_hold(rules, s)
        if h:
            holds[s] = dict(h, active=hold_active(h, now))
    try:
        verdicts = parse_haiku((runner or run_haiku)(build_prompt(items, panes, holds)))
    except Exception as e:  # timeout, non-zero exit, bad JSON: never silent
        return fail(f"{type(e).__name__}: {str(e)[:100]}")
    out = {}
    for i, _ in items:
        if i in verdicts:
            inv, why = verdicts[i]
            out[i] = {"decision": "wake" if inv else "log", "path": "haiku", "reason": why}
        else:
            out[i] = {"decision": "wake", "path": "haiku", "reason": "haiku fail-open: no verdict returned"}
    return out


def alert_id(a):
    key = "|".join((a.get("session", ""), a.get("kind", ""), a.get("fingerprint", ""), a.get("ts", "")))
    return hashlib.sha1(key.encode()).hexdigest()[:10]


def triage(alerts, rules, now, rc_state, runner=None, pane_fn=None):
    """One record per alert: ts, id, kind, session, decision (wake|log), path, reason."""
    results = [decide(a, rules, now, rc_state) for a in alerts]
    items = [(f"a{n}", a) for n, (a, r) in
             enumerate(((a, r) for a, r in zip(alerts, results) if r["decision"] == "ask_haiku"), 1)]
    verdicts = _gate(items, rules, now, runner, pane_fn) if items else {}
    by_alert = {id(a): verdicts[i] for i, a in items}
    ts = datetime.fromtimestamp(now, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    recs = []
    for a, r in zip(alerts, results):
        r = by_alert.get(id(a), r)
        recs.append({"ts": ts, "id": alert_id(a), "kind": a["kind"], "session": a.get("session", ""),
                     "decision": r["decision"], "path": r["path"], "reason": r["reason"]})
    return recs


def append_records(path, recs):
    with open(path, "a") as f:
        for r in recs:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")


def _replay(n):
    """Dry run of the real gate over the last n alerts; prints counts, latency and cost."""
    alerts = []
    with open(os.path.join(ROOT, "inbox", "alerts.jsonl")) as f:
        for line in f.readlines()[-n:]:
            try:
                alerts.append(json.loads(line))
            except ValueError:
                pass
    rules = load_rules(os.path.join(ROOT, "local", "triage-rules.json"))
    now = time.time()
    rc = {}
    asked = [a for a in alerts if decide(a, rules, now, {})["decision"] == "ask_haiku"]
    t0 = time.time()
    recs = triage(alerts, rules, now, rc, pane_fn=lambda s: _live_pane(s))
    dt = time.time() - t0
    wake = [r for r in recs if r["decision"] == "wake"]
    print(f"alerts={len(recs)} rule_wake={sum(r['path'].startswith('rule:') for r in wake)} "
          f"rule_log={sum(r['path'].startswith('rule:') and r['decision'] == 'log' for r in recs)} "
          f"asked_haiku={len(asked)} haiku_wake={sum(r['path'] == 'haiku' for r in wake)} "
          f"total_wake={len(wake)} seconds={dt:.1f}")
    for r in wake:
        print(f"  WAKE {r['kind']} {r['session']} {r['path']}: {r['reason'][:100]}")


def _live_pane(session):
    return subprocess.run(["tmux", "capture-pane", "-t", f"={session}:", "-p", "-S", f"-{PANE_LINES}"],
                          capture_output=True, text=True, timeout=10).stdout


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--replay":
        _replay(int(sys.argv[2]))
    else:
        sys.exit("usage: alert_triage.py --replay N")
