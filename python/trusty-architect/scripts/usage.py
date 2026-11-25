#!/usr/bin/env python3
"""usage.py — Claude account usage ledger, summary, cutover plan and poller hooks.

Why: the owner (see #10571): "let's keep track of usage so we can intelligently plan
cutovers." A cutover is the owner's /login to another Claude account for the whole
fleet. Each account has a weekly limit with its own reset time (the owner:
"no 5 hour limits"; the 5h fields are optional, kept for old rows, and never gate
anything); planning a switch needs the latest reading per account and a burn rate.
What: one ledger, local/usage/ledger.jsonl, one JSON line per reading:
  ts, account, pct5h, reset5h (optional, legacy; absent or null is normal),
  pctwk, resetwk, weekly_models ({"fable": 0}), source, note.
source is statusline-cache (inbox/statusline-usage.json, written by
scripts/statusline-tee.sh from Claude Code's statusLine stdin, which carries
rate_limits), usage-cmd (a /usage capture, scripts/usage-probe.sh) or manual.
CLI: `usage.py summary` (per-account table, burn rate, projection),
`usage.py plan` (cutover recommendation), `usage.py ingest` (stdin capture),
`usage.py sample` (one statusline-cache sample). The poller calls poll_step.
Layout (#10571 S1, 500-SLOC cap): the pure functions live in usage_calc.py and are
re-exported here, so every name stays importable as usage.<name>; this file keeps
the paths, the ledger, cache and account I/O, the probe launch and the CLI.
Test: tests/test_usage.py
Usage: scripts/usage.py summary | plan | sample | ingest --source usage-cmd
"""
import argparse
import json
import os
import sys
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
LEDGER = os.environ.get("USAGE_LEDGER") or os.path.join(ROOT, "local", "usage", "ledger.jsonl")
INBOX = os.environ.get("SUPERVISOR_INBOX_DIR") or os.path.join(ROOT, "inbox")
CACHE = os.environ.get("USAGE_STATUSLINE_CACHE") or os.path.join(INBOX, "statusline-usage.json")
PROBE_SCRIPT = os.path.join(HERE, "usage-probe.sh")
if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)
import architect_config  # noqa: E402

# #11821: the config dir comes from the shared resolver (CLAUDE_CONFIG_DIR, else the default).
CLAUDE_JSON = os.environ.get("USAGE_CLAUDE_JSON") or os.path.join(
    architect_config.default_claude_config_dir(), ".claude.json")
from usage_calc import (  # noqa: E402,F401  (#10571 S1: split for the 500-SLOC cap; re-exported)
    BURN_MAX_GAP, BURN_MIN_RISE, BURN_MIN_SPAN, BURN_RELIABLE_SPAN, BURN_SPAN, CACHE_MAX_AGE,
    EMAIL_RE, H5, NY, PROBE_EVERY, STALE_AFTER, UTC, WEEK_WARN, WRITE_EVERY, _clock, _dur,
    _epoch_or_iso, _hm, _in, _MONTHS, _PCT, _reset_text, _RESETS, _TIME, _widen_if_short,
    burn_rate, changed, choose_probe_pane, effective, format_plan, format_runway, format_summary,
    iso, latest_by_account, make_record, now_utc, parse_iso, parse_reset, parse_usage_capture,
    plan, project, rank_candidates, runway, same_windows_as_other_account, usage_alerts,
)


# --- ledger (I/O) ---------------------------------------------------------------

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


# --- account and sources (I/O) ------------------------------------------------

def detect_account():
    """The logged-in Claude account (what the status line shows as ✻<account>)."""
    try:
        with open(CLAUDE_JSON) as f:
            return json.load(f).get("oauthAccount", {}).get("emailAddress") or None
    except (OSError, ValueError, AttributeError):
        return None


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


# --- the poller hook ------------------------------------------------------------

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
        if not EMAIL_RE.match(acct):
            print(f"ingest: --account {acct!r} is not an email address; label readings by email, never a row number",
                  file=sys.stderr)
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
