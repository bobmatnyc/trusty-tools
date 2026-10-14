#!/usr/bin/env python3
"""usage.py — Claude account usage ledger, summary, cutover plan and poller hooks.

Why: the owner (2026-10-07): "let's keep track of usage so we can intelligently plan
cutovers." A cutover is the owner's /login to another Claude account for the whole
fleet. Each account has a 5-hour window and a weekly limit with its own reset
times; planning a switch needs the latest reading per account and a burn rate.
What: one ledger, local/usage/ledger.jsonl, one JSON line per reading:
  ts, account, pct5h, reset5h (UTC ISO or null: window not started),
  pctwk, resetwk, weekly_models ({"fable": 0}), source, note.
source is statusline-cache (inbox/statusline-usage.json, written by
scripts/statusline-tee.sh from Claude Code's statusLine stdin, which carries
rate_limits), usage-cmd (a /usage capture, scripts/usage-probe.sh) or manual.
CLI: `usage.py summary` (per-account table, burn rate, projection),
`usage.py plan` (cutover recommendation), `usage.py ingest` (stdin capture),
`usage.py sample` (one statusline-cache sample). The poller calls poll_step.
Test: scripts/test_usage.py
Usage: scripts/usage.py summary | plan | sample | ingest --source usage-cmd
"""
import argparse
import json
import os
import re
import sys
import time
from datetime import datetime, timedelta, timezone
from zoneinfo import ZoneInfo

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
LEDGER = os.environ.get("USAGE_LEDGER") or os.path.join(ROOT, "local", "usage", "ledger.jsonl")
INBOX = os.environ.get("SUPERVISOR_INBOX_DIR") or os.path.join(ROOT, "inbox")
CACHE = os.environ.get("USAGE_STATUSLINE_CACHE") or os.path.join(INBOX, "statusline-usage.json")
CLAUDE_JSON = os.environ.get("USAGE_CLAUDE_JSON") or os.path.join(
    os.environ.get("CLAUDE_CONFIG_DIR") or os.path.expanduser("~/.trusty-tools/trusty-mpm/claude-config"),
    ".claude.json")
PROBE_SCRIPT = os.path.join(HERE, "usage-probe.sh")
NY = ZoneInfo("America/New_York")
UTC = timezone.utc

CACHE_MAX_AGE = 30 * 60      # a statusline cache older than this is not a reading
PROBE_EVERY = 60 * 60        # /usage fallback: at most once per hour
WRITE_EVERY = 30 * 60        # an unchanged reading is re-recorded after this
WARN_5H_MIN = 45             # alert when the 5h window is projected to run out within this
WEEK_WARN = (80, 90)         # weekly thresholds that alert
H5 = 5 * 3600
ELIGIBLE_5H = 95             # a next account needs its 5h window below this
STALE_AFTER = 24 * 3600      # a reading this old (reset not passed) is flagged stale
BURN_SPAN = {"5h": 3 * 3600, "wk": 48 * 3600}  # samples older than this are ignored
BURN_MAX_GAP = {"5h": 5400, "wk": 6 * 3600}  # a longer gap between samples ends the run
BURN_MIN_SPAN = 10 * 60      # two samples closer than this give no rate


def now_utc():
    return datetime.now(UTC)


def iso(dt):
    return dt.astimezone(UTC).strftime("%Y-%m-%dT%H:%M:%SZ") if dt else None


def parse_iso(s):
    if not s:
        return None
    return datetime.fromisoformat(s.replace("Z", "+00:00")).astimezone(UTC)


# --- /usage capture parsing (pure) --------------------------------------------

_MONTHS = {m: i for i, m in enumerate(
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"], 1)}
_TIME = r"(\d{1,2})(?::(\d{2}))?\s*([ap]m)"
_PCT = re.compile(r"(\d+(?:\.\d+)?)\s*%\s*used")
_RESETS = re.compile(r"Resets\s+(.*?)(?:\s*\(([A-Za-z_]+/[A-Za-z_]+)\))?\s*$")


def _clock(h, m, ap):
    h = int(h) % 12 + (12 if ap.lower() == "pm" else 0)
    return h, int(m or 0)


def parse_reset(text, captured):
    """Resolve a /usage "Resets ..." phrase to a UTC datetime, or None.

    Forms: "4:50pm (America/New_York)" (next occurrence after `captured`) and
    "Oct 9 at 3am (America/New_York)" (a date; the year is the next one that
    puts it at or after `captured` minus a day).
    """
    m = _RESETS.search(text.strip())
    if not m:
        return None
    body, tzname = m.group(1).strip(), m.group(2)
    tz = ZoneInfo(tzname) if tzname else NY
    cap = captured.astimezone(tz)
    d = re.match(r"([A-Za-z]{3})[a-z]*\s+(\d{1,2})(?:\s+at)?\s+" + _TIME, body)
    if d:
        h, mi = _clock(*d.group(3, 4, 5))
        month, day = _MONTHS[d.group(1).lower()], int(d.group(2))
        for year in (cap.year, cap.year + 1):
            cand = datetime(year, month, day, h, mi, tzinfo=tz)
            if cand >= cap - timedelta(days=1):
                return cand.astimezone(UTC)
        return None
    t = re.match(_TIME + r"$", body)
    if t:
        h, mi = _clock(*t.group(1, 2, 3))
        cand = cap.replace(hour=h, minute=mi, second=0, microsecond=0)
        if cand <= cap:
            cand += timedelta(days=1)
        return cand.astimezone(UTC)
    return None


def parse_usage_capture(text, captured):
    """Parse a /usage capture. Returns a dict (pct5h, reset5h, pctwk, resetwk,
    weekly_models) or None when neither the session nor the week block is found.
    A per-model week block ("Current week (Fable)") goes to weekly_models.
    """
    out = {"pct5h": None, "reset5h": None, "pctwk": None, "resetwk": None, "weekly_models": {}}
    block = None
    for line in text.splitlines():
        s = line.strip()
        low = s.lower()
        if low.startswith("current session"):
            block = "5h"
        elif low.startswith("current week"):
            model = re.search(r"\(([^)]*)\)", s)
            name = model.group(1).strip().lower() if model else "all models"
            block = "wk" if name in ("all models", "all") else "model:" + name
        elif block and (pm := _PCT.search(s)):
            v = float(pm.group(1))
            v = int(v) if v == int(v) else v
            if block == "5h":
                out["pct5h"] = v
            elif block == "wk":
                out["pctwk"] = v
            else:
                out["weekly_models"][block[6:]] = v
        elif block and s.startswith("Resets"):
            dt = iso(parse_reset(s, captured))
            if block == "5h":
                out["reset5h"] = dt
            elif block == "wk":
                out["resetwk"] = dt
    if out["pct5h"] is None and out["pctwk"] is None:
        return None
    return out


# --- ledger and effective state (pure except read/append) ---------------------

def read_ledger(path=None):
    rows = []
    try:
        with open(path or LEDGER) as f:
            for line in f:
                try:
                    r = json.loads(line)
                except ValueError:
                    continue
                if isinstance(r, dict) and r.get("account") and r.get("ts"):
                    rows.append(r)
    except OSError:
        pass
    rows.sort(key=lambda r: r["ts"])
    return rows


def append_ledger(rec, path=None):
    path = path or LEDGER
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "a") as f:
        f.write(json.dumps(rec, ensure_ascii=False) + "\n")


def make_record(account, source, when, data, note=None):
    rec = {"ts": iso(when), "account": account,
           "pct5h": data.get("pct5h"), "reset5h": data.get("reset5h"),
           "pctwk": data.get("pctwk"), "resetwk": data.get("resetwk"),
           "weekly_models": data.get("weekly_models") or {}, "source": source}
    if note:
        rec["note"] = note
    return rec


def latest_by_account(rows):
    out = {}
    for r in rows:
        out[r["account"]] = r
    return out


def effective(rec, now):
    """Reading -> usable figures at `now`. A window whose reset has passed counts
    as 0% (flag inferred). A 5h window with no reset time was not started: 0%.
    A 5h reading older than 5 h with no reset on file is inferred 0%.
    Returns dict: pct5h, pctwk, reset5h, resetwk, inferred5h, inferredwk, stale, age.
    """
    ts = parse_iso(rec["ts"])
    age = (now - ts).total_seconds()
    r5, rw = parse_iso(rec.get("reset5h")), parse_iso(rec.get("resetwk"))
    p5, pw = rec.get("pct5h"), rec.get("pctwk")
    inf5 = infw = False
    if r5 is not None and r5 <= now:
        p5, inf5 = 0, True
    elif r5 is None and p5 != 0 and age > H5:
        p5, inf5 = 0, True  # no reset on file and the reading is over 5 h old
    if rw is not None and rw <= now:
        pw, infw = 0, True
    stale = age > STALE_AFTER and not infw
    return {"pct5h": p5, "pctwk": pw, "reset5h": None if inf5 else r5,
            "resetwk": None if infw else rw, "inferred5h": inf5, "inferredwk": infw,
            "stale": stale, "age": age, "source": rec.get("source"), "ts": ts,
            "weekly_models": rec.get("weekly_models") or {}}


# --- burn rate and projection (pure) ------------------------------------------

def burn_rate(rows, account, now, kind):
    """(rate %/h, first, last) over this account's samples in the current window.

    kind "5h" or "wk". Samples count only when they share the latest sample's
    reset time (same window; a null 5h reset matches null) and are within
    BURN_SPAN and linked by gaps under BURN_MAX_GAP. None when under two samples, under BURN_MIN_SPAN apart, or not rising.
    """
    pk, rk = ("pct5h", "reset5h") if kind == "5h" else ("pctwk", "resetwk")
    mine = [r for r in rows if r["account"] == account and r.get(pk) is not None]
    if not mine:
        return None
    last = mine[-1]
    lr = parse_iso(last.get(rk))
    pts = []
    for r in mine:
        t = parse_iso(r["ts"])
        rr = parse_iso(r.get(rk))
        same = (rr is None and lr is None) or (rr and lr and abs((rr - lr).total_seconds()) < 300)
        if same and (now - t).total_seconds() <= BURN_SPAN[kind]:
            pts.append((t, float(r[pk])))
    run = [pts[-1]]  # the latest run of samples with no gap longer than BURN_MAX_GAP
    for pt in reversed(pts[:-1]):
        if (run[0][0] - pt[0]).total_seconds() > BURN_MAX_GAP[kind]:
            break
        run.insert(0, pt)
    pts = run
    if len(pts) < 2:
        return None
    (t0, p0), (t1, p1) = pts[0], pts[-1]
    dt = (t1 - t0).total_seconds()
    if dt < BURN_MIN_SPAN or p1 <= p0:
        return None
    return (p1 - p0) / (dt / 3600.0), pts[0], pts[-1]


def project(rows, account, now, kind):
    """Projected exhaustion for one window of one account.

    Returns {"rate", "at", "reset", "before_reset", "pct"} or None without a
    rate. `at` is when the window reaches 100% at the measured rate, counted
    from the last sample; `before_reset` says that happens before the reset.
    """
    br = burn_rate(rows, account, now, kind)
    last = latest_by_account(rows).get(account)
    if br is None or last is None:
        return None
    rate, _, (t1, p1) = br
    at = t1 + timedelta(hours=(100.0 - p1) / rate)
    reset = parse_iso(last.get("reset5h" if kind == "5h" else "resetwk"))
    return {"rate": rate, "at": at, "reset": reset, "pct": p1,
            "before_reset": reset is None or at < reset}


# --- cutover plan (pure) ------------------------------------------------------

def _hm(delta_s):
    h = max(delta_s, 0) / 3600.0
    return f"{h:.1f} h"


def _fmt(dt):
    if dt is None:
        return "-"
    return f"{iso(dt)} / {dt.astimezone(NY).strftime('%a %m-%d %H:%M')} NY"


def rank_candidates(rows, now, current):
    """Other accounts usable for a cutover, best first, with reasons.

    Eligible: a reading exists, effective 5h below ELIGIBLE_5H, weekly below 100.
    Order: lowest effective weekly %, then (within 5 points) the later weekly
    reset, since a later reset spreads that allowance over more days.
    """
    out, skipped = [], []
    for acct, rec in latest_by_account(rows).items():
        if acct == current:
            continue
        e = effective(rec, now)
        p5, pw = e["pct5h"], e["pctwk"]
        if pw is None:
            skipped.append((acct, "no weekly reading"))
        elif pw >= 100:
            skipped.append((acct, "weekly limit reached"))
        elif p5 is not None and p5 >= ELIGIBLE_5H:
            skipped.append((acct, f"5h window at {p5}%"))
        else:
            out.append((acct, e))
    out.sort(key=lambda x: (round(x[1]["pctwk"] / 5), -(x[1]["resetwk"].timestamp() if x[1]["resetwk"] else 0), x[1]["pctwk"]))
    return out, skipped


def plan(rows, now, current=None):
    """Cutover recommendation. Returns a dict; format_plan renders it.

    A cutover is needed when the current account's 5h window is exhausted or is
    projected to be before its reset, or its weekly limit is projected to be
    reached before the weekly reset. switch_at is the earliest such time.
    """
    last = latest_by_account(rows)
    current = current or (rows[-1]["account"] if rows else None)
    res = {"now": now, "current": current, "need": False, "triggers": [], "switch_at": None,
           "next": None, "alternatives": [], "skipped": [], "runway": None}
    if current not in last:
        res["note"] = "no reading for the current account"
        return res
    cur = effective(last[current], now)
    trig = []
    for kind, label in (("5h", "5-hour"), ("wk", "weekly")):
        pct = cur["pct5h" if kind == "5h" else "pctwk"]
        reset = cur["reset5h" if kind == "5h" else "resetwk"]
        pr = project(rows, current, now, kind)
        if pct is not None and pct >= 100 and (reset is None or reset > now):
            trig.append((now, f"{label} limit reached ({pct}%)"
                         + (f"; resets {_fmt(reset)}, in {_hm((reset - now).total_seconds())}" if reset else "")))
        elif pr and pr["before_reset"] and not (cur["inferred5h"] if kind == "5h" else cur["inferredwk"]):
            at = max(pr["at"], now)
            trig.append((at, f"{label} projected to reach 100% at {_fmt(at)} "
                         f"(burn {pr['rate']:.1f} %/h from {pr['pct']:g}%)"
                         + (f", before its reset {_fmt(pr['reset'])}" if pr["reset"] else "")))
    res["triggers"] = trig
    res["need"] = bool(trig)
    if trig:
        res["switch_at"] = min(t for t, _ in trig)
    cands, skipped = rank_candidates(rows, now, current)
    res["skipped"] = skipped
    if cands:
        res["next"], res["alternatives"] = cands[0], cands[1:]
    res["runway"] = runway(rows, now, current)
    return res


def runway(rows, now, current):
    """Weekly capacity left across accounts, hours of runway at the measured
    weekly burn rate of the current account, and the gap to the first weekly
    reset. Accounts without a weekly reading are listed, not counted.
    """
    left, missing, resets = 0.0, [], []
    for acct, rec in latest_by_account(rows).items():
        e = effective(rec, now)
        if e["pctwk"] is None:
            missing.append(acct)
            continue
        left += max(0.0, 100.0 - e["pctwk"])
        if e["resetwk"] and e["resetwk"] > now:
            resets.append((e["resetwk"], acct))
    # Burn rate: the highest weekly rate measured on any account (full-fleet use is
    # the planning case; an account's average over idle hours understates it).
    pr, rate_acct = None, None
    for acct in latest_by_account(rows):
        cand = project(rows, acct, now, "wk")
        if cand and (pr is None or cand["rate"] > pr["rate"]):
            pr, rate_acct = cand, acct
    rate = pr["rate"] if pr else None
    first = min(resets) if resets else None
    out = {"left": left, "rate": rate, "rate_account": rate_acct, "missing": missing, "first_reset": first,
           "runway_h": left / rate if rate else None,
           "gap_h": (first[0] - now).total_seconds() / 3600.0 if first else None}
    out["covers_gap"] = (out["runway_h"] >= out["gap_h"]) if out["runway_h"] and out["gap_h"] is not None else None
    return out


def format_runway(rw):
    lines = [f"Weekly capacity left across accounts: {rw['left']:g} points (100 = one account's week)"]
    if rw["rate"]:
        lines.append(f"Runway at the measured weekly burn ({rw['rate']:.1f} points/h, measured on {rw['rate_account']}): {rw['runway_h']:.1f} h")
    else:
        lines.append("Runway: no weekly burn rate yet (needs two readings 10+ min apart in one window)")
    if rw["first_reset"]:
        lines.append(f"First weekly reset: {rw['first_reset'][1]} at {_fmt(rw['first_reset'][0])}, "
                     f"{rw['gap_h']:.1f} h away")
        if rw["covers_gap"] is not None:
            lines.append("Runway " + ("covers" if rw["covers_gap"] else "does NOT cover") + " the gap to that reset")
    if rw["missing"]:
        lines.append("No weekly reading: " + ", ".join(rw["missing"]))
    return lines


def format_plan(p):
    now = p["now"]
    out = [f"Cutover plan at {iso(now)} (current account: {p['current']})"]
    if p.get("note"):
        out.append(p["note"])
    if not p["need"]:
        out.append("No cutover needed: no limit is reached or projected before its reset.")
    else:
        out.append(f"CUTOVER RECOMMENDED. Switch by {_fmt(p['switch_at'])}"
                   + (" (now)" if p["switch_at"] <= now else f" (in {_hm((p['switch_at'] - now).total_seconds())})"))
        out += ["  - " + r for _, r in p["triggers"]]
        if p["next"]:
            acct, e = p["next"]
            out.append(f"Next account: {acct}")
            out.append(f"  - weekly {e['pctwk']:g}% used, resets {_fmt(e['resetwk'])}"
                       + (" (inferred: reset passed)" if e["inferredwk"] else ""))
            p5 = e["pct5h"]
            out.append("  - 5h window " + ("not started or reset (0%)" if not p5 else f"{p5:g}% used")
                       + (f", resets {_fmt(e['reset5h'])}" if e["reset5h"] else "")
                       + (" (inferred)" if e["inferred5h"] else ""))
            out.append(f"  - reading {_hm(e['age'])} old, source {e['source']}" + (" (STALE)" if e["stale"] else ""))
            for acct2, e2 in p["alternatives"]:
                out.append(f"Alternative: {acct2} (weekly {e2['pctwk']:g}%, resets {_fmt(e2['resetwk'])})")
        else:
            out.append("Next account: none eligible")
        for acct, why in p["skipped"]:
            out.append(f"Not eligible: {acct} ({why})")
    if p["runway"]:
        out += format_runway(p["runway"])
    return "\n".join(out)


# --- summary ------------------------------------------------------------------

def format_summary(rows, now, current=None):
    last = latest_by_account(rows)
    current = current or (rows[-1]["account"] if rows else None)
    out = [f"Usage at {iso(now)}; current account: {current}", ""]
    for acct in sorted(last, key=lambda a: (a != current, a)):
        rec = last[acct]
        e = effective(rec, now)
        def pct(v, inf):
            return "?" if v is None else f"{v:g}%" + ("*" if inf else "")
        flags = (" STALE" if e["stale"] else "")
        out.append(f"{'>' if acct == current else ' '} {acct}")
        out.append(f"    5h  {pct(e['pct5h'], e['inferred5h']):>6}  reset {_fmt(parse_iso(rec.get('reset5h')))}"
                   + ("  (window not started)" if rec.get("reset5h") is None and rec.get("pct5h") == 0 else ""))
        out.append(f"    wk  {pct(e['pctwk'], e['inferredwk']):>6}  reset {_fmt(parse_iso(rec.get('resetwk')))}"
                   + (f"  models {e['weekly_models']}" if e["weekly_models"] else ""))
        out.append(f"    read {rec['ts']}  age {_hm(e['age'])}  source {rec.get('source')}{flags}")
    out.append("")
    out.append("* inferred: the reading's reset time has passed, so the window counts as 0%.")
    if current in last:
        for kind, label in (("5h", "5h"), ("wk", "weekly")):
            pr = project(rows, current, now, kind)
            if pr is None:
                out.append(f"Burn {label}: no rate yet (needs two readings 10+ min apart in one window)")
            else:
                at = f"{iso(pr['at'])}" + ("" if pr["before_reset"] else " (after reset: not reached)")
                out.append(f"Burn {label}: {pr['rate']:.1f} %/h; projected 100% at {at}")
    return "\n".join(out)


# --- account and sources (I/O) ------------------------------------------------

def detect_account():
    """The logged-in Claude account (what the status line shows as ✻<account>)."""
    try:
        with open(CLAUDE_JSON) as f:
            return json.load(f).get("oauthAccount", {}).get("emailAddress") or None
    except (OSError, ValueError, AttributeError):
        return None


def _epoch_or_iso(v):
    if v is None:
        return None
    if isinstance(v, (int, float)):
        return datetime.fromtimestamp(v / 1000.0 if v > 1e11 else v, UTC)
    return parse_iso(str(v))


def read_cache(now, path=None, max_age=CACHE_MAX_AGE):
    """Parse the statusline cache: line 1 the account, the rest Claude Code's
    statusLine JSON. Returns (account, captured_at, data) or None when missing,
    stale or without rate_limits.
    """
    path = path or CACHE
    try:
        mtime = datetime.fromtimestamp(os.path.getmtime(path), UTC)
        with open(path) as f:
            first, _, rest = f.read().partition("\n")
        rl = json.loads(rest).get("rate_limits")
    except (OSError, ValueError, AttributeError):
        return None
    if not isinstance(rl, dict) or (now - mtime).total_seconds() > max_age:
        return None
    five, week = rl.get("five_hour") or {}, rl.get("seven_day") or {}
    if five.get("used_percentage") is None and week.get("used_percentage") is None:
        return None
    data = {"pct5h": five.get("used_percentage"), "reset5h": iso(_epoch_or_iso(five.get("resets_at"))),
            "pctwk": week.get("used_percentage"), "resetwk": iso(_epoch_or_iso(week.get("resets_at"))),
            "weekly_models": {}}
    return first.strip() or None, mtime, data


def same_windows_as_other_account(rows, account, data):
    """True when the newest reading of a DIFFERENT account has the same reset
    times: right after a /login the cache can still carry the old account's figures."""
    other = [r for r in rows if r["account"] != account]
    return bool(other) and other[-1].get("reset5h") == data["reset5h"] \
        and other[-1].get("resetwk") == data["resetwk"] and data["resetwk"] is not None


def changed(prev, data):
    return prev is None or any(prev.get(k) != data.get(k) for k in ("pct5h", "pctwk", "reset5h", "resetwk"))


def record_reading(account, source, when, data, rows=None, note=None, force=False):
    """Append a reading unless it repeats the account's last one within WRITE_EVERY."""
    rows = read_ledger() if rows is None else rows
    prev = latest_by_account(rows).get(account)
    if not force and prev and not changed(prev, data) \
            and (when - parse_iso(prev["ts"])).total_seconds() < WRITE_EVERY:
        return None
    rec = make_record(account, source, when, data, note)
    append_ledger(rec)
    return rec


def sample_cache(now, rows=None):
    """One no-typing sample from the statusline cache. Returns the record or None."""
    acct_now = detect_account()
    got = read_cache(now)
    if not got or not acct_now or got[0] != acct_now:
        return None
    rows = read_ledger() if rows is None else rows
    if same_windows_as_other_account(rows, acct_now, got[2]):
        return None
    return record_reading(acct_now, "statusline-cache", now, got[2], rows)


# --- alerts and the poller hook -----------------------------------------------

def usage_alerts(rows, now, current):
    """[(fingerprint, detail)] for the current account. Deduped by the poller on
    the fingerprint: the reset time is part of it, so each window alerts once.
    """
    out = []
    last = latest_by_account(rows)
    if current not in last:
        return out
    cur = effective(last[current], now)
    p = plan(rows, now, current)
    nxt = ""
    if p["next"]:
        sw = p["switch_at"] or now
        nxt = f" Next account: {p['next'][0]}, switch by {iso(max(sw, now))}."
    pr5 = project(rows, current, now, "5h")
    r5 = cur["reset5h"]
    if pr5 and pr5["before_reset"] and not cur["inferred5h"] and (pr5["at"] - now).total_seconds() <= WARN_5H_MIN * 60:
        out.append((f"5h-exhaust:{current}:{iso(r5)}",
                    f"{current} 5h window projected to hit 100% at {iso(max(pr5['at'], now))}"
                    f" ({pr5['rate']:.0f} %/h, now {pr5['pct']:g}%).{nxt}"))
    wk = cur["pctwk"]
    if wk is not None and not cur["inferredwk"]:
        for th in WEEK_WARN:
            if wk >= th:
                out.append((f"wk>={th}:{current}:{iso(cur['resetwk'])}",
                            f"{current} weekly usage {wk:g}% (resets {iso(cur['resetwk'])}).{nxt}"))
    if p["need"]:
        tgt = p["next"][0] if p["next"] else "no eligible account"
        out.append((f"cutover:{current}:{p['next'][0] if p['next'] else 'none'}",
                    f"Cutover recommended: {current} -> {tgt} by {iso(max(p['switch_at'], now))}. "
                    + "; ".join(r for _, r in p["triggers"])))
    return out


def choose_probe_pane(idle_panes):
    """First (session, pane) the poller found idle with an empty/suggestion box."""
    return idle_panes[0] if idle_panes else None


def poll_step(state, now_ts, idle_panes=(), dry_run=False, launcher=None):
    """One poller cycle: sample, maybe launch a /usage probe, return alert tuples.

    Never raises into the caller's loop beyond what the caller catches; the
    poller wraps this in try/except. Probe: on an account change until a
    usage-cmd/statusline reading exists for the new account since the change
    (3 tries), else when no fresh reading exists and PROBE_EVERY has passed.
    """
    now = datetime.fromtimestamp(now_ts, UTC)
    st = state.setdefault("usage", {})
    acct = detect_account()
    if not acct:
        return []
    if st.get("account") != acct:
        st.update(account=acct, since=now_ts, tries=0, pending=st.get("account") is not None)
    rows = read_ledger()
    if not dry_run:
        sample_cache(now, rows)
        rows = read_ledger()
    last = latest_by_account(rows).get(acct)
    fresh = bool(last and last.get("source") != "manual"
                 and parse_iso(last["ts"]).timestamp() >= st.get("since", 0) - 60
                 and (now - parse_iso(last["ts"])).total_seconds() < PROBE_EVERY)
    if st.get("pending") and fresh:
        st["pending"] = False
    due = st.get("pending") and st.get("tries", 0) < 3 or \
        (not fresh and now_ts - st.get("last_probe", 0) >= PROBE_EVERY)
    pane = choose_probe_pane(idle_panes)
    if due and pane and not dry_run:
        st["last_probe"], st["tries"] = now_ts, st.get("tries", 0) + 1
        (launcher or launch_probe)(pane[0], pane[1], acct)
    return usage_alerts(rows, now, acct)


def launch_probe(session, pane, account):
    """Start usage-probe.sh detached; its result lands in the ledger by itself."""
    import subprocess
    log = open(os.path.join(INBOX, "usage-probe.log"), "a")
    subprocess.Popen([PROBE_SCRIPT, session, pane, account], stdout=log, stderr=log,
                     stdin=subprocess.DEVNULL, start_new_session=True)


# --- CLI ----------------------------------------------------------------------

def main(argv=None):
    ap = argparse.ArgumentParser(description="Claude account usage ledger.")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("summary", help="per-account table, burn rate, projection")
    sub.add_parser("plan", help="cutover recommendation and weekly runway")
    sub.add_parser("sample", help="record one sample from the statusline cache")
    ing = sub.add_parser("ingest", help="parse a /usage capture on stdin into the ledger")
    ing.add_argument("--source", default="usage-cmd")
    ing.add_argument("--account", default=None)
    args = ap.parse_args(argv)
    now = now_utc()
    if args.cmd == "sample":
        rec = sample_cache(now)
        print(json.dumps(rec) if rec else "no sample (no fresh statusline cache for this account)")
        return 0 if rec else 1
    if args.cmd == "ingest":
        acct = args.account or detect_account()
        data = parse_usage_capture(sys.stdin.read(), now)
        if not acct or data is None:
            print("ingest: no account or no /usage figures found", file=sys.stderr)
            return 1
        rec = record_reading(acct, args.source, now, data, force=True)
        print(json.dumps(rec))
        return 0
    rows = read_ledger()
    cur = detect_account() or (rows[-1]["account"] if rows else None)
    print(format_summary(rows, now, cur) if args.cmd == "summary" else format_plan(plan(rows, now, cur)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
