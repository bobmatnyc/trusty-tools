"""Regenerate the #9801 slice 1 parity corpus and goldens from the prototype.

Why: #9801 slice 1 ports the prototype poller's usage alerts (usage.py and
fleet-poll.py's usage_alerts wrapper) and notify_macos. The goldens must come
from running that Python, never from the Rust.
What: loads usage.py and fleet-poll.py by path from the prototype directory
given as the one argument (read-only: -B -I, sys.dont_write_bytecode). Every
path either module could write (ledger, statusline cache, claude.json, inbox,
poll.log) points at a system tempdir before the import. It applies ONE
Architect-ruled patch to usage.burn_rate (see RULED below), builds the
corpus, and writes corpus_9801.json, one golden per surface, and
rulings.jsonl (what the unpatched prototype does where the port differs).
subprocess.run is replaced while notify_macos runs, so no notification is
sent and no osascript is started. The probe launcher is always a fake: no
usage-probe.sh runs and no pane is touched.
Test: tests/parity_usage.rs reads every file written here.

Regenerate (from the repository root):
    python3 -B -I crates/trusty-architect/tests/fixtures/9801/regen_9801.py <prototype-dir>
where <prototype-dir> is the Architect prototype's scripts/ directory.
"""
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timezone

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
if len(sys.argv) != 2:
    sys.exit("usage: regen_9801.py <prototype-dir>  (the Architect prototype's scripts/ directory)")
PROTOTYPE = sys.argv[1]
SCRATCH = tempfile.mkdtemp(prefix="regen-9801-")
os.environ["SUPERVISOR_INBOX_DIR"] = os.path.join(SCRATCH, "inbox")
os.environ["USAGE_LEDGER"] = os.path.join(SCRATCH, "ledger.jsonl")
os.environ["USAGE_STATUSLINE_CACHE"] = os.path.join(SCRATCH, "cache.json")
os.environ["USAGE_CLAUDE_JSON"] = os.path.join(SCRATCH, "claude.json")
os.makedirs(os.environ["SUPERVISOR_INBOX_DIR"])


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, os.path.join(PROTOTYPE, filename))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


u = _load("usage", "usage.py")
fc = _load("fleet_poll", "fleet-poll.py")
fc.LOG = os.path.join(SCRATCH, "poll.log")
UTC = timezone.utc
T = lambda s: datetime.fromisoformat(s.replace("Z", "+00:00"))

# --- RULED: the one patch (Architect ruling pending, #9801 slice 1) -----------
# The prototype's burn_rate indexes pts[-1] after filtering by window and span;
# when the account's newest sample is older than BURN_SPAN the list is empty and
# it raises IndexError, so usage_alerts raises (no alerts) whenever ANY
# account's newest reading is over 48 h old (runway) or the current account's
# is over 3 h old. Its docstring says "None when under two samples"; the port
# returns None. rulings.jsonl records the unpatched result for each case.
_orig_burn = u.burn_rate


def _ruled_burn(rows, account, now, kind):
    try:
        return _orig_burn(rows, account, now, kind)
    except IndexError:
        return None


def ruled(fn, *args):
    u.burn_rate = _ruled_burn
    try:
        return fn(*args)
    finally:
        u.burn_rate = _orig_burn


def outcome(fn, *args):
    try:
        return {"ok": fn(*args)}
    except Exception as e:  # noqa: BLE001 - the goldens record the exception class
        return {"raises": type(e).__name__}


# --- rows ---------------------------------------------------------------------

def row(ts, acct, p5, r5, pw, rw, src="manual", models=None):
    return {"ts": ts, "account": acct, "pct5h": p5, "reset5h": r5, "pctwk": pw, "resetwk": rw,
            "weekly_models": models or {}, "source": src}


def fleet(extra=()):
    """test_usage.py's fleet(): the 16:1xZ readings of 2026-10-07 plus masa's 13:09Z."""
    return [
        row("2026-10-07T13:09:00Z", "a3@example.com", 3, "2026-10-07T17:59:00Z", 1, "2026-10-13T10:59:00Z"),
        row("2026-10-07T16:15:00Z", "a3@example.com", 100, "2026-10-07T18:00:00Z", 27, "2026-10-13T11:00:00Z", models={"fable": 0}),
        row("2026-10-07T16:12:00Z", "a2@example.com", 9, "2026-10-07T20:50:00Z", 84, "2026-10-09T07:00:00Z"),
        row("2026-10-07T16:17:00Z", "a4@example.com", 0, None, 89, "2026-10-12T15:00:00Z"),
        row("2026-10-07T16:17:00Z", "a1@example.com", 0, None, 93, "2026-10-10T15:00:00Z"),
        row("2026-10-07T16:16:00Z", "a6@example.com", 0, None, 96, "2026-10-11T09:00:00Z"),
        row("2026-10-07T16:18:00Z", "a5@example.com", 0, None, 50, "2026-10-12T11:00:00Z"),
        *extra]


GM = "a2@example.com"
GM_6030 = row("2026-10-07T14:30:00Z", GM, 1, "2026-10-07T20:50:00Z", 60, "2026-10-09T07:00:00Z")

ALERTS = [
    {"name": "week_thresholds_and_cutover", "rows": fleet([GM_6030]), "now": "2026-10-07T16:20:00Z", "current": GM},
    {"name": "five_hour_exhaustion", "now": "2026-10-07T16:21:00Z", "current": GM, "rows": fleet([
        row("2026-10-07T15:40:00Z", GM, 30, "2026-10-07T20:50:00Z", 84, "2026-10-09T07:00:00Z"),
        row("2026-10-07T16:20:00Z", GM, 70, "2026-10-07T20:50:00Z", 84, "2026-10-09T07:00:00Z")])},
    {"name": "five_hour_far", "now": "2026-10-07T16:21:00Z", "current": GM, "rows": fleet([
        row("2026-10-07T14:00:00Z", GM, 1, "2026-10-07T20:50:00Z", 84, "2026-10-09T07:00:00Z"),
        row("2026-10-07T16:20:00Z", GM, 10, "2026-10-07T20:50:00Z", 84, "2026-10-09T07:00:00Z")])},
    {"name": "limit_reached_means_now", "rows": fleet(), "now": "2026-10-07T16:20:00Z", "current": "a3@example.com"},
    {"name": "healthy_no_cutover", "rows": [r for r in fleet() if r["account"] != GM],
     "now": "2026-10-07T16:20:00Z", "current": "a5@example.com"},
    {"name": "float_percents", "now": "2026-10-07T16:20:00Z", "current": "f@x", "rows": fleet([
        row("2026-10-07T15:00:00Z", "f@x", 12.5, "2026-10-07T19:00:00Z", 90.5, "2026-10-12T00:00:00Z"),
        row("2026-10-07T16:10:00Z", "f@x", 20.25, "2026-10-07T19:00:00Z", 91.75, "2026-10-12T00:00:00Z")])},
    {"name": "float_limit_reached", "now": "2026-10-07T16:20:00Z", "current": "f@x", "rows": fleet([
        row("2026-10-07T16:10:00Z", "f@x", 100.0, "2026-10-07T19:00:00Z", 40.0, "2026-10-12T00:00:00Z")])},
    {"name": "no_eligible_account", "now": "2026-10-07T16:20:00Z", "current": "a@x", "rows": [
        row("2026-10-07T16:00:00Z", "a@x", 100, "2026-10-07T18:00:00Z", 50, "2026-10-12T00:00:00Z"),
        row("2026-10-07T16:01:00Z", "b@x", 0, None, 100, "2026-10-12T00:00:00Z"),
        row("2026-10-07T16:02:00Z", "c@x", 0, None, None, None),
        row("2026-10-07T16:03:00Z", "d@x", 97, "2026-10-07T18:00:00Z", 10, "2026-10-12T00:00:00Z")]},
    {"name": "current_not_in_ledger", "rows": fleet(), "now": "2026-10-07T16:20:00Z", "current": "nobody@x"},
    {"name": "other_account_over_48h_old", "now": "2026-10-07T16:20:00Z", "current": GM, "rows": fleet([
        GM_6030, row("2026-10-04T10:00:00Z", "old@x", 0, None, 5, "2026-10-14T00:00:00Z")])},
    {"name": "current_5h_reading_over_3h_old", "now": "2026-10-07T20:20:00Z", "current": "a@x", "rows": [
        row("2026-10-07T16:00:00Z", "a@x", 40, "2026-10-07T21:00:00Z", 85, "2026-10-12T00:00:00Z")]},
    {"name": "winter_est_times", "now": "2027-01-15T14:20:00Z", "current": "w@x", "rows": [
        row("2027-01-15T13:00:00Z", "w@x", 20, "2027-01-15T17:00:00Z", 60, "2027-01-18T05:00:00Z"),
        row("2027-01-15T14:10:00Z", "w@x", 66, "2027-01-15T17:00:00Z", 88, "2027-01-18T05:00:00Z"),
        row("2027-01-15T14:00:00Z", "v@x", 0, None, 10, "2027-01-20T05:00:00Z")]},
    {"name": "dst_start_boundary", "now": "2027-03-14T06:30:00Z", "current": "w@x", "rows": [
        row("2027-03-14T05:30:00Z", "w@x", 10, "2027-03-14T09:00:00Z", 50, "2027-03-14T07:00:00Z"),
        row("2027-03-14T06:20:00Z", "w@x", 60, "2027-03-14T09:00:00Z", 79, "2027-03-14T07:00:00Z"),
        row("2027-03-14T06:00:00Z", "v@x", 0, None, 10, "2027-03-14T06:59:59Z")]},
    {"name": "dst_end_boundary", "now": "2026-11-01T05:30:00Z", "current": "w@x", "rows": [
        row("2026-11-01T04:40:00Z", "w@x", 10, "2026-11-01T09:00:00Z", 81, "2026-11-01T06:00:00Z"),
        row("2026-11-01T05:25:00Z", "w@x", 55, "2026-11-01T09:00:00Z", 83, "2026-11-01T06:00:00Z"),
        row("2026-11-01T05:00:00Z", "v@x", 0, None, 10, "2026-11-01T06:30:00Z")]},
    {"name": "null_reset_exhaustion", "now": "2026-10-07T16:21:00Z", "current": "n@x", "rows": [
        row("2026-10-07T15:40:00Z", "n@x", 30, None, 20, "2026-10-12T00:00:00Z"),
        row("2026-10-07T16:20:00Z", "n@x", 70, None, 20, "2026-10-12T00:00:00Z")]},
    {"name": "inferred_windows", "now": "2026-10-12T12:00:00Z", "current": "d@x", "rows": [
        row("2026-10-12T05:00:00Z", "d@x", 91, None, 95, "2026-10-12T11:00:00Z"),
        row("2026-10-12T04:00:00Z", "e@x", 99, "2026-10-12T06:00:00Z", 20, "2026-10-19T00:00:00Z")]},
    {"name": "bad_timestamp", "now": "2026-10-07T16:20:00Z", "current": GM, "rows": fleet([
        row("garbage", GM, 1, None, 2, None)])},
    {"name": "non_numeric_percent", "now": "2026-10-07T16:20:00Z", "current": "s@x", "rows": [
        row("2026-10-07T16:00:00Z", "s@x", "lots", None, 20, "2026-10-12T00:00:00Z")]},
    {"name": "tiny_rate_overflows", "now": "2026-10-07T16:20:00Z", "current": "t@x", "rows": [
        row("2026-10-07T15:00:00Z", "t@x", 10, "2026-10-07T19:00:00Z", 50, "2026-10-12T00:00:00Z"),
        row("2026-10-07T16:00:00Z", "t@x", 10.000000000001, "2026-10-07T19:00:00Z", 50, "2026-10-12T00:00:00Z")]},
    {"name": "offset_timestamps", "now": "2026-10-07T16:20:00Z", "current": "o@x", "rows": [
        row("2026-10-07T11:00:00-04:00", "o@x", 20, "2026-10-07T19:00:00.500000+00:00", 81, "2026-10-12T00:00:00Z"),
        row("2026-10-07T12:10:00.250-04:00", "o@x", 60, "2026-10-07T19:00:00.500000+00:00", 82, "2026-10-12T00:00:00Z")]},
]

EFFECTIVE = [
    ["reset_passed_5h", row("2026-10-06T18:58:00Z", "d", 91, "2026-10-06T21:00:00Z", 25, "2026-10-12T11:00:00Z"), "2026-10-07T16:00:00Z"],
    ["reset_passed_week", row("2026-10-06T18:58:00Z", "d", 91, "2026-10-06T21:00:00Z", 25, "2026-10-12T11:00:00Z"), "2026-10-12T12:00:00Z"],
    ["window_not_started", row("2026-10-07T16:17:00Z", "r", 0, None, 89, "2026-10-12T15:00:00Z"), "2026-10-07T16:30:00Z"],
    ["no_reset_over_5h", row("2026-10-07T10:00:00Z", "r", 40, None, 89, None), "2026-10-07T16:30:00Z"],
    ["null_pct_over_5h", row("2026-10-07T10:00:00Z", "r", None, None, None, None), "2026-10-07T16:30:00Z"],
    ["stale_day_old", row("2026-10-05T10:00:00Z", "r", 12.5, "2026-10-10T00:00:00Z", 30.0, "2026-10-12T00:00:00Z"), "2026-10-07T16:30:00Z"],
]

PROJECT = [
    ["masa_pair_week", [r for r in fleet() if r["account"] == "a3@example.com"], "a3@example.com", "2026-10-07T16:20:00Z", "wk"],
    ["one_sample", [row("2026-10-07T16:12:00Z", "a", 9, None, 84, "2026-10-09T07:00:00Z")], "a", "2026-10-08T00:00:00Z", "wk"],
    ["gap_breaks_run", [row("2026-10-07T16:12:00Z", "a", 9, None, 84, "2026-10-09T07:00:00Z"),
                        row("2026-10-07T23:50:00Z", "a", 9, None, 90, "2026-10-09T07:00:00Z")], "a", "2026-10-08T00:00:00Z", "wk"],
    ["other_window", [row("2026-10-07T16:12:00Z", "a", 9, None, 84, "2026-10-09T07:00:00Z"),
                      row("2026-10-07T16:40:00Z", "a", 9, None, 90, "2026-10-16T07:00:00Z")], "a", "2026-10-08T00:00:00Z", "wk"],
    ["falling", [row("2026-10-07T15:00:00Z", "a", 50, "2026-10-07T19:00:00Z", 10, None),
                 row("2026-10-07T16:00:00Z", "a", 40, "2026-10-07T19:00:00Z", 10, None)], "a", "2026-10-07T16:10:00Z", "5h"],
    ["under_min_span", [row("2026-10-07T16:00:00Z", "a", 10, None, 10, None),
                        row("2026-10-07T16:05:00Z", "a", 20, None, 10, None)], "a", "2026-10-07T16:10:00Z", "5h"],
    ["after_reset_not_reached", [row("2026-10-07T15:00:00Z", "a", 10, "2026-10-07T16:30:00Z", 10, None),
                                 row("2026-10-07T16:00:00Z", "a", 20, "2026-10-07T16:30:00Z", 10, None)], "a", "2026-10-07T16:10:00Z", "5h"],
    ["reset_jitter_under_300s", [row("2026-10-07T15:00:00Z", "a", 10, "2026-10-07T19:00:00Z", 10, None),
                                 row("2026-10-07T16:00:00Z", "a", 30, "2026-10-07T19:04:59Z", 10, None)], "a", "2026-10-07T16:10:00Z", "5h"],
    ["newest_older_than_span", [row("2026-10-07T10:00:00Z", "a", 10, None, 10, None),
                                row("2026-10-07T11:00:00Z", "a", 20, None, 10, None)], "a", "2026-10-07T16:10:00Z", "5h"],
]

def cache_payload(account, five=12.5, week=84, r5=1791400000, rw=1791500000):
    return account + "\n" + json.dumps({"rate_limits": {"five_hour": {"used_percentage": five, "resets_at": r5},
                                                       "seven_day": {"used_percentage": week, "resets_at": rw}}}) + "\n"


CACHE_NOW = 1_791_380_000.0
CACHE = [
    {"name": "epoch_seconds", "text": cache_payload(GM), "age": 60},
    {"name": "epoch_millis", "text": cache_payload(GM, r5=1791400000123, rw=1791500000999), "age": 60},
    {"name": "iso_strings", "text": cache_payload(GM, r5="2026-10-07T20:50:00Z", rw="2026-10-09T07:00:00.900+00:00"), "age": 60},
    {"name": "fractional_epoch", "text": cache_payload(GM, r5=1791400000.9999996, rw=1791500000.5), "age": 60},
    {"name": "stale", "text": cache_payload(GM), "age": 1801},
    {"name": "edge_of_max_age", "text": cache_payload(GM), "age": 1800},
    {"name": "no_rate_limits", "text": GM + "\n{\"x\": 1}\n", "age": 60},
    {"name": "both_percent_null", "text": cache_payload(GM, five=None, week=None), "age": 60},
    {"name": "one_window_only", "text": GM + "\n" + json.dumps({"rate_limits": {"seven_day": {"used_percentage": 7}}}), "age": 60},
    {"name": "bad_json", "text": GM + "\n{nope", "age": 60},
    {"name": "not_an_object", "text": GM + "\n[1, 2]", "age": 60},
    {"name": "empty_account_line", "text": "  \n" + json.dumps({"rate_limits": {"five_hour": {"used_percentage": 3}}}), "age": 60},
    {"name": "five_hour_is_a_list", "text": GM + "\n" + json.dumps({"rate_limits": {"five_hour": ["x"], "seven_day": {"used_percentage": 1}}}), "age": 60},
    {"name": "reset_garbage", "text": cache_payload(GM, r5="soon"), "age": 60},
    {"name": "reset_empty_string", "text": cache_payload(GM, r5=""), "age": 60},
    {"name": "missing_file", "text": None, "age": 0},
]

CLAUDE_GM = json.dumps({"oauthAccount": {"emailAddress": GM}})
NOW0 = 1_791_380_000.0
IDLE = [["tm-x", "%1"]]

POLL = [
    {"name": "account_change_probe", "state": {"usage": {"account": "a3@example.com", "since": 0}},
     "claude": CLAUDE_GM, "ledger": None, "cache": None, "steps": [
        {"dt": 0, "idle": IDLE}, {"dt": 600, "idle": IDLE}, {"dt": 700, "idle": []}]},
    {"name": "hourly_not_more", "state": {}, "claude": CLAUDE_GM, "ledger": None, "cache": None, "steps": [
        {"dt": 0, "idle": IDLE}, {"dt": 600, "idle": IDLE}, {"dt": 1200, "idle": IDLE}, {"dt": 3700, "idle": IDLE}]},
    {"name": "pending_retries_capped_at_three", "state": {"usage": {"account": "other@x", "since": 0}},
     "claude": CLAUDE_GM, "ledger": None, "cache": None, "steps": [
        {"dt": 0, "idle": IDLE}, {"dt": 600, "idle": IDLE}, {"dt": 1200, "idle": IDLE},
        {"dt": 1800, "idle": IDLE}, {"dt": 4900, "idle": IDLE}]},
    {"name": "cache_sample_recorded_once", "state": {}, "claude": CLAUDE_GM, "ledger": None,
     "cache": {"text": cache_payload(GM), "age": 60}, "steps": [
        {"dt": 0, "idle": IDLE}, {"dt": 60, "idle": IDLE}]},
    {"name": "cache_of_another_account", "state": {}, "claude": CLAUDE_GM, "ledger": None,
     "cache": {"text": cache_payload("a1@example.com"), "age": 60}, "steps": [{"dt": 0, "idle": IDLE}]},
    {"name": "carry_over_after_login", "state": {}, "claude": CLAUDE_GM,
     "ledger": json.dumps(row("2026-10-07T23:00:00Z", "a3@example.com", 5, "2026-10-07T21:46:40Z", 27, "2026-10-09T01:33:20Z")) + "\n",
     "cache": {"text": cache_payload(GM), "age": 60}, "steps": [{"dt": 0, "idle": IDLE}]},
    {"name": "dry_run_samples_and_probes_nothing", "state": {}, "claude": CLAUDE_GM, "ledger": None,
     "cache": {"text": cache_payload(GM), "age": 60}, "steps": [{"dt": 0, "idle": IDLE, "dry_run": True}]},
    {"name": "alerts_from_the_ledger", "state": {}, "claude": CLAUDE_GM,
     "ledger": "".join(json.dumps(r) + "\n" for r in fleet([GM_6030])) + "not json\n\n[1]\n{\"account\": \"\", \"ts\": \"x\"}\n",
     "cache": None, "steps": [{"now": T("2026-10-07T16:20:00Z").timestamp(), "idle": []}]},
    {"name": "no_account", "state": {"other": 1}, "claude": None, "ledger": None, "cache": None,
     "steps": [{"dt": 0, "idle": IDLE}]},
    {"name": "account_not_a_string", "state": {}, "claude": json.dumps({"oauthAccount": "x"}), "ledger": None,
     "cache": None, "steps": [{"dt": 0, "idle": IDLE}]},
    {"name": "crlf_ledger_lines", "state": {}, "claude": CLAUDE_GM,
     "ledger": json.dumps(row("2026-10-07T16:12:00Z", GM, 9, "2026-10-07T20:50:00Z", 84, "2026-10-09T07:00:00Z", src="usage-cmd"))
     + "\r\n" + json.dumps(row("2026-10-07T16:13:00Z", GM, 9, "2026-10-07T20:50:00Z", 85, "2026-10-09T07:00:00Z", src="usage-cmd")) + "\r",
     "cache": None, "steps": [{"now": T("2026-10-07T16:20:00Z").timestamp(), "idle": IDLE}]},
]

NOTIFY = [
    ["Architect watchdog", "[tm-architect]: architect_dead: no pane for tm-architect"],
    ["Architect watchdog", 'say "hi" then C:\\temp\\x and \\"both\\"'],
    ["T\"i\\tle", "café ❯ 📅 done"],
    ["", ""],
]


# --- runners --------------------------------------------------------------------

def iso_or_none(dt):
    return u.iso(dt) if dt else None


def plan_summary(p):
    rw = p["runway"]
    return {
        "need": p["need"], "switch_at": iso_or_none(p["switch_at"]),
        "triggers": [[u.iso(t), text] for t, text in p["triggers"]],
        "next": p["next"][0] if p["next"] else None,
        "alternatives": [a for a, _ in p["alternatives"]],
        "skipped": [list(s) for s in p["skipped"]], "note": p.get("note"),
        "runway": None if rw is None else {
            "left": rw["left"], "rate": rw["rate"], "rate_account": rw["rate_account"], "missing": rw["missing"],
            "first_reset": None if rw["first_reset"] is None else [u.iso(rw["first_reset"][0]), rw["first_reset"][1]],
            "runway_h": rw["runway_h"], "gap_h": rw["gap_h"], "covers_gap": rw["covers_gap"]},
    }


def effective_summary(e):
    return {"pct5h": e["pct5h"], "pctwk": e["pctwk"], "reset5h": iso_or_none(e["reset5h"]),
            "resetwk": iso_or_none(e["resetwk"]), "inferred5h": e["inferred5h"], "inferredwk": e["inferredwk"],
            "stale": e["stale"], "age": e["age"]}


def project_summary(p):
    return None if p is None else {"rate": p["rate"], "at": u.iso(p["at"]), "reset": iso_or_none(p["reset"]),
                                   "before_reset": p["before_reset"], "pct": p["pct"]}


def sorted_rows(rows):
    return sorted(rows, key=lambda r: r["ts"])  # read_ledger's order


def wrapper_rows(case):
    now = T(case["now"])
    rows = sorted_rows(case["rows"])
    step = lambda st, now_ts, idle, dry: ruled(u.usage_alerts, rows, now, case["current"])
    open(fc.LOG, "w").close()
    got = fc.usage_alerts({}, now.timestamp(), [], step=step)
    logged = [line.split(" ", 1)[1].rstrip("\n") for line in open(fc.LOG)]
    return {"alerts": got, "log": logged}


def set_files(sc):
    for name, key in ((u.CLAUDE_JSON, "claude"), (u.LEDGER, "ledger")):
        if os.path.exists(name):
            os.remove(name)
        if sc.get(key) is not None:
            with open(name, "w", newline="") as f:
                f.write(sc[key])
    if os.path.exists(u.CACHE):
        os.remove(u.CACHE)
    if sc.get("cache"):
        with open(u.CACHE, "w") as f:
            f.write(sc["cache"]["text"])
        when = NOW0 - sc["cache"]["age"]
        os.utime(u.CACHE, (when, when))


def run_poll(sc):
    set_files(sc)
    state = json.loads(json.dumps(sc["state"]))
    out = []
    for i, st in enumerate(sc["steps"]):
        launched = []
        now_ts = st["now"] if "now" in st else NOW0 + st["dt"]
        got = outcome(ruled, u.poll_step, state, now_ts, [tuple(p) for p in st["idle"]], st.get("dry_run", False),
                      lambda *a: launched.append(list(a)))
        ledger = open(u.LEDGER).read() if os.path.exists(u.LEDGER) else None
        res = {"step": i, "now": now_ts, "state": json.loads(json.dumps(state.get("usage"))),
               "has_usage": "usage" in state,
               "launched": launched, "ledger": ledger}
        res.update({"alerts": [list(a) for a in got["ok"]]} if "ok" in got else got)
        out.append([sc["name"], res])
    return out


def run_cache(case):
    if os.path.exists(u.CACHE):
        os.remove(u.CACHE)
    if case["text"] is not None:
        with open(u.CACHE, "w") as f:
            f.write(case["text"])
        when = CACHE_NOW - case["age"]
        os.utime(u.CACHE, (when, when))
    got = outcome(u.read_cache, datetime.fromtimestamp(CACHE_NOW, UTC))
    if "ok" in got and got["ok"] is not None:
        acct, mtime, data = got["ok"]
        got = {"ok": [acct, mtime.timestamp(), data]}
    return [case["name"], got]


def run_notify(title, message):
    seen = []

    def fake_run(argv, **kw):
        seen.append([argv, sorted(kw.items())])
        return subprocess.CompletedProcess(argv, 0, b"", b"")

    real = fc.subprocess.run
    fc.subprocess.run = fake_run
    try:
        fc.notify_macos(title, message)
    finally:
        fc.subprocess.run = real
    return [title, message, seen[0][0], seen[0][1]]


def notify_failure(exc):
    """What the prototype logs when osascript cannot run or times out."""
    def fake_run(argv, **kw):
        if exc == "nonzero":
            return subprocess.CompletedProcess(argv, 1, b"", b"execution error")
        raise exc

    real = fc.subprocess.run
    fc.subprocess.run = fake_run
    open(fc.LOG, "w").close()
    try:
        fc.notify_macos("Architect watchdog", "x")
    finally:
        fc.subprocess.run = real
    return [line.split(" ", 1)[1].rstrip("\n") for line in open(fc.LOG)]


def rulings():
    out = []
    for case in ALERTS:
        rows, now = sorted_rows(case["rows"]), T(case["now"])
        proto = outcome(u.usage_alerts, rows, now, case["current"])
        port = outcome(ruled, u.usage_alerts, rows, now, case["current"])
        if proto != port:
            out.append({"case": "usage_alerts:" + case["name"], "ruling": "burn_rate_empty_run_is_no_rate",
                        "prototype": proto, "port": port})
    for name, rows, acct, now, kind in PROJECT:
        args = (sorted_rows(rows), acct, T(now), kind)
        proto, port = outcome(u.project, *args), outcome(ruled, u.project, *args)
        if proto != port:
            out.append({"case": "project:" + name, "ruling": "burn_rate_empty_run_is_no_rate",
                        "prototype": proto, "port": {"ok": project_summary(port["ok"])}})
    # A failed probe launch: the prototype advances last_probe and tries first,
    # then the launcher raises out of poll_step and the cycle gets no usage alerts.
    sc = {"claude": CLAUDE_GM, "ledger": "".join(json.dumps(r) + "\n" for r in fleet([GM_6030])), "cache": None}
    set_files(sc)
    state = {}

    def boom(*a):
        raise OSError(2, "No such file or directory", "usage-probe.sh")
    now_ts = T("2026-10-07T16:20:00Z").timestamp()
    got = outcome(u.poll_step, state, now_ts, [("tm-x", "%1")], False, boom)
    open(fc.LOG, "w").close()
    wrapped = fc.usage_alerts({}, now_ts, [("tm-x", "%1")], step=lambda *a: u.poll_step(*a, launcher=boom))
    logged = [line.split(" ", 1)[1].rstrip("\n") for line in open(fc.LOG)]
    out.append({"case": "poll_step:probe_launch_fails", "ruling": "failed_launch_is_not_a_probe",
                "prototype": {"result": got, "state": state, "wrapper_alerts": wrapped, "log": logged}})
    for name, exc in (("spawn_fails", FileNotFoundError(2, "No such file or directory", "osascript")),
                      ("timeout", subprocess.TimeoutExpired(["osascript"], 10)), ("nonzero_exit", "nonzero")):
        out.append({"case": "notify_macos:" + name, "ruling": "notify_failure_is_logged",
                    "prototype": {"log": notify_failure(exc)}})
    return out


def write(name, rows, **kw):
    with open(os.path.join(HERE, name), "w", encoding="utf-8", newline="\n") as f:
        for r in rows:
            f.write(json.dumps(r, **kw) + "\n")


def main():
    corpus = {"alerts": ALERTS, "effective": EFFECTIVE, "project": PROJECT, "cache": CACHE,
              "cache_now": CACHE_NOW, "poll": POLL, "poll_now": NOW0, "notify": NOTIFY}
    with open(os.path.join(HERE, "corpus_9801.json"), "w", encoding="ascii", newline="\n") as f:
        json.dump(corpus, f, indent=1)
        f.write("\n")
    alerts, plans, wrapped = [], [], []
    for case in ALERTS:
        rows, now = sorted_rows(case["rows"]), T(case["now"])
        got = outcome(ruled, u.usage_alerts, rows, now, case["current"])
        alerts.append([case["name"], {"ok": [list(a) for a in got["ok"]]} if "ok" in got else got])
        p = outcome(ruled, u.plan, rows, now, case["current"])
        plans.append([case["name"], {"ok": plan_summary(p["ok"])} if "ok" in p else p])
        wrapped.append([case["name"], wrapper_rows(case)])
    write("usage_alerts.jsonl", alerts, ensure_ascii=False)
    write("plan.jsonl", plans, ensure_ascii=False)
    write("usage_rows.jsonl", wrapped, ensure_ascii=False)
    eff = []
    for name, rec, now in EFFECTIVE:
        eff.append([name, effective_summary(u.effective(rec, T(now)))])
    write("effective.jsonl", eff)
    proj = []
    for name, rows, acct, now, kind in PROJECT:
        got = outcome(ruled, u.project, sorted_rows(rows), acct, T(now), kind)
        proj.append([name, {"ok": project_summary(got["ok"])} if "ok" in got else got])
    write("project.jsonl", proj)
    write("cache.jsonl", [run_cache(c) for c in CACHE], ensure_ascii=False)
    poll = []
    for sc in POLL:
        poll += run_poll(sc)
    write("poll_step.jsonl", poll, ensure_ascii=False, sort_keys=True)
    write("notify.jsonl", [run_notify(t, m) for t, m in NOTIFY], ensure_ascii=False)
    write("rulings.jsonl", rulings(), ensure_ascii=False, sort_keys=True)
    shutil.rmtree(SCRATCH)


if __name__ == "__main__":
    main()
