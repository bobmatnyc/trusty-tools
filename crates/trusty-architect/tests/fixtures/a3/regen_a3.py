"""Regenerate the A3 parity corpus and goldens from the frozen prototype.

Why: #9171 A3a holds the poller's sensors and state helpers, and A3b-1 its
wake() send path, to the bytes the Architect's live prototype produces. The
goldens must come from running that Python (fleet-poll.py at 988152c), never
from the Rust.
What: loads fleet-poll.py by path from the prototype directory given as the one
argument (read-only: -B -I, sys.dont_write_bytecode), points every path it
could write (inbox, log, events) at a system tempdir, injects the clock and the
host probe values, scripts tmux for wake(), and writes corpus_a3.json plus one
golden per surface into this directory. It also writes
../../../src/decimal_table.rs, the table of Python `re` `\\d` code points the
Rust `is_digit` reads (Architect ruling Q6).
Test: tests/parity_a3.rs and tests/parity_wake.rs read every file written here.

Regenerate (from the repository root):
    python3 -B -I crates/trusty-architect/tests/fixtures/a3/regen_a3.py <prototype-dir>
where <prototype-dir> is the Architect prototype's scripts/ directory.
"""
import importlib.util
import json
import os
import re
import sys
import tempfile
import types
import unicodedata
from datetime import datetime

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
CRATE = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))
if len(sys.argv) != 2:
    sys.exit("usage: regen_a3.py <prototype-dir>  (the Architect prototype's scripts/ directory)")
PROTOTYPE = sys.argv[1]
SCRATCH = tempfile.mkdtemp(prefix="regen-a3-")
# Every path fleet-poll.py derives from the inbox lands in the scratch dir.
os.environ["SUPERVISOR_INBOX_DIR"] = os.path.join(SCRATCH, "inbox")
for var in ("LOAD_FACTOR", "SELF_CTX_WINDOW", "SELF_CTX_THRESHOLD", "PANE_CTX_WINDOW",
            "BAND_STOP_LOAD_FACTOR", "BAND_HOLD_LOAD_FACTOR", "BAND_OPEN_LOAD_FACTOR",
            "BAND_REHOLD_LOAD_FACTOR", "BAND_STOP_FREE_GIB", "BAND_HOLD_FREE_GIB",
            "BAND_STOP_MEM_FREE_PCT"):
    os.environ.pop(var, None)


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, os.path.join(PROTOTYPE, filename))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


fc = _load("fleet_poll", "fleet-poll.py")
os.makedirs(fc.INBOX, exist_ok=True)
fc.LOG = os.path.join(SCRATCH, "poll.log")
NOW = 1_760_000_000.5
fc.time = types.SimpleNamespace(time=lambda: NOW, sleep=lambda s: None)
DEAD_PID = 2_147_483_000  # above every pid_max: kill(pid, 0) is ESRCH

# --- list_panes ---------------------------------------------------------------

LIST_PANES = [
    ["two_panes", "tm-a\t%1\tclaude\t11\t/w/a\t0\ntm-b\t%22\t2.1.283\t01\t/w/b\t0\n"],
    ["short_rows", "tm-a\t%1\tzsh\t10\nx\t%2\n\n"],
    ["tab_in_path", "tm-a\t%3\tclaude\t11\t/w/a\tb\t0\n"],
    ["bad_pane_id", "tm-a\tmain\tclaude\t11\t/w\t0\ntm-b\t%00\tclaude\t11\t/w\t1\n"],
    ["crlf_and_unicode", "tm-é\t%4\tclaude\t11\t/w/ü\t0\r\ntm-z\t%5\t-zsh\t00\t/\t1"],
    ["empty", ""],
]

# --- builder band ----------------------------------------------------------------

BAND = [
    [0.0, 16, 300, 70, 0, None], [17.9, 16, 200, 20, 0, None], [18.0, 16, 300, 70, 0, None],
    [0.0, 16, 199, 70, 0, None], [30.0, 16, 300, 70, 0, None], [30.1, 16, 300, 70, 0, None],
    [0.0, 16, 99, 70, 0, None], [0.0, 16, 100, 70, 0, None], [0.0, 16, 300, 19, 0, None],
    [0.0, 16, 300, None, 0, None], [0.0, 16, 300, 70, 1, None], [0.0, 16, 300, 20, 0, None],
    [0.0, 16, 300, 70, None, None], [0.0, 16, 300, 70, 0.4, None],
    [15.5, 16, 300, 70, 0, "open"], [18.0, 16, 300, 70, 0, "open"], [20.0, 16, 300, 70, 0, "open"],
    [20.5, 16, 300, 70, 0, "open"], [18.0, 16, 300, 70, 0, "hold"], [16.0, 16, 300, 70, 0, "hold"],
    [15.9, 16, 300, 70, 0, "hold"], [15.9, 16, 300, 70, 0, "stop"], [18.0, 16, 300, 70, 0, "stop"],
    [30.5, 16, 300, 70, 0, "open"], [5.0, 16, 150, 70, 0, "open"], [5.0, 16, 150, 70, 0, "hold"],
    [5.0, 16, 300, 70, 0, "weird"], [4.0, 4, 250, 50, 0, None], [7.5, 4, 250, 50, 0, "open"],
]

BAND_ALERTS = [
    {"case": "first_run_then_change", "state": {}, "steps": [
        [0.0, 1.0, 16, 300, 70, 0], [1.0, 1.0, 16, 300, 70, 0], [2.0, 13.2, 16, 180, 70, 0]]},
    {"case": "rounding_ties", "state": {"band": "hold"}, "steps": [
        [10.0, 0.25, 16, 2.5, 70.5, 0.5], [20.0, 0.35, 16, 300.5, None, None],
        [30.0, 31.25, 16, 300, 0.5, 0], [40.0, 1.0, 16, 300, 70, 2.5]]},
    {"case": "unknown_stored_band", "state": {"band": "weird"}, "steps": [[0.0, 1.0, 16, 300, 70, 0]]},
]

# --- host_alerts (injected probe values) -----------------------------------------


def probe(loads, cores=16, disk=(100, 1, 500 * 2**30), swap=0, mem=60.0):
    return {"loads": list(loads), "cores": cores, "disk": list(disk), "swap": swap, "mem": mem}


HOST = [
    {"case": "spike_1min_low_15", "probe": probe((29.7, 16.8, 10.0))},
    {"case": "sustained_15_over", "probe": probe((30.0, 25.0, 20.0))},
    {"case": "load15_over_factor", "probe": probe((30.0, 31.0, 24.5))},
    {"case": "load15_at_factor", "probe": probe((30.0, 31.0, 24.0))},
    {"case": "factor_env_2", "probe": probe((10.0, 17.0, 17.0), cores=8), "load_factor": "2.0"},
    {"case": "factor_env_3", "probe": probe((10.0, 17.0, 17.0), cores=8), "load_factor": "3"},
    {"case": "disk_full", "probe": probe((1.0, 1.0, 1.0), disk=(1000, 905, 95))},
    {"case": "disk_at_90", "probe": probe((1.0, 1.0, 1.0), disk=(1000, 900, 100))},
    {"case": "swap_in_use", "probe": probe((1.0, 1.0, 1.0), swap=512.4)},
    {"case": "swap_error", "probe": probe((1.0, 1.0, 1.0), swap="error")},
    {"case": "swap_unknown", "probe": probe((1.0, 1.0, 1.0), swap=None)},
    {"case": "calm_15_keeps_hold", "probe": probe((30.0, 31.0, 20.0)), "state": {"band": "hold"}},
    {"case": "high_15_stops", "probe": probe((30.0, 31.0, 31.0)), "state": {"band": "hold"}},
    {"case": "first_band", "probe": probe((2.0, 2.0, 2.0), mem=None), "state": {}},
    {"case": "every_alert", "probe": probe((50.0, 40.0, 33.3), disk=(2**40, 2**40 - 2**30, 2**30),
                                           swap=2048.0, mem=10.0), "state": {"band": "open"}},
]


class _Usage:
    def __init__(self, total, used, free):
        self.total, self.used, self.free = total, used, free


def run_host(case):
    p = case["probe"]
    fc.os.getloadavg = lambda: tuple(p["loads"])
    fc.os.cpu_count = lambda: p["cores"]
    fc.shutil.disk_usage = lambda path: _Usage(*p["disk"])
    fc.data_volume_path = lambda: "/data"

    def swap():
        if p["swap"] == "error":
            raise OSError("sysctl failed")
        return p["swap"]

    fc.swap_used_mb = swap
    fc.mem_free_pct = lambda: p["mem"]
    if "load_factor" in case:
        os.environ["LOAD_FACTOR"] = case["load_factor"]
    else:
        os.environ.pop("LOAD_FACTOR", None)
    state = json.loads(json.dumps(case["state"])) if "state" in case else None
    alerts = fc.host_alerts(NOW, state)
    os.environ.pop("LOAD_FACTOR", None)
    return {"case": case["case"], "alerts": alerts, "state": state}


# --- read_events ------------------------------------------------------------------


def ev(t, session="tm-api", pane="%3", message=None):
    row = {"tmux_session": session, "tmux_pane": pane, "type": t}
    if message is not None:
        row["message"] = message
    return json.dumps(row)


EVENTS = [
    {"case": "python_test_rows", "steps": [
        {"mode": "w", "text": ""},
        {"mode": "w", "text": "".join(r + "\n" for r in [
            ev("permission_prompt", message="Claude needs your permission to use Bash"),
            ev("idle_prompt", message="Claude is waiting for your input"),
            ev("agent_needs_input", message="Agent needs input"),
            json.dumps({"tmux_session": "tm-supervisor", "type": "permission_prompt", "message": "x"})])},
        {"mode": "a", "text": ""}]},
    {"case": "first_run_skips_backlog", "steps": [
        {"mode": "w", "text": ev("permission_prompt") + "\n"},
        {"mode": "a", "text": ev("agent_needs_input", "tm-b", "%9", "") + "\n"}]},
    {"case": "rotation_rereads_from_zero", "offset": 0, "steps": [
        {"mode": "w", "text": ev("permission_prompt", message="one") + "\n" + ev("permission_prompt", message="two") + "\n"},
        {"mode": "w", "text": ev("agent_needs_input", message="after rotation") + "\n"}]},
    {"case": "skip_sessions_and_bad_lines", "offset": 0, "steps": [
        {"mode": "w", "text": "".join(r + "\n" for r in [
            ev("permission_prompt", "tm-architect"), ev("permission_prompt", "tm-architect-poll"),
            ev("permission_prompt", "tm-supervisor-collector"), "{not json", "", "[]x",
            json.dumps({"type": "permission_prompt"}), ev("permission_prompt", "", "", "no session"),
            ev("permission_prompt", "tm-é", "%1", "café — API_KEY=abc123 ok")])}]},
    {"case": "truncated_last_line_then_completed", "offset": 0, "steps": [
        {"mode": "w", "text": ev("permission_prompt", message="whole") + "\n" + ev("agent_needs_input")[:20]},
        {"mode": "a", "text": ev("agent_needs_input")[20:] + "\n" + ev("agent_needs_input", message="next") + "\n"}]},
    {"case": "missing_file", "steps": [{"mode": "delete"}, {"mode": "delete"}]},
    {"case": "missing_file_with_offset", "offset": 40, "steps": [{"mode": "delete"}]},
]


def run_events(case):
    path = os.path.join(SCRATCH, "events.jsonl")
    fc.EVENTS = path
    if os.path.exists(path):
        os.remove(path)
    state = {} if "offset" not in case else {"events_offset": case["offset"]}
    rows = []
    for step, s in enumerate(case["steps"]):
        if s["mode"] == "delete":
            if os.path.exists(path):
                os.remove(path)
        else:
            with open(path, s["mode"], encoding="utf-8", newline="") as out:
                out.write(s["text"])
        alerts = fc.read_events(state)
        rows.append(json.dumps({"case": case["case"], "step": step, "alerts": alerts,
                                "events_offset": state.get("events_offset")}, ensure_ascii=False))
    return rows


# --- self_ctx_alerts -------------------------------------------------------------


def usage_line(usage, msg_type="assistant", entrypoint="cli"):
    return json.dumps({"type": msg_type, "message": {"usage": usage}, "entrypoint": entrypoint})


def f(name, mtime, *lines):
    return {"name": name, "mtime": mtime, "lines": list(lines)}


SELF_CTX_ALERTS = [
    {"case": "over_threshold", "files": [f("over.jsonl", 1000, usage_line({"input_tokens": 1}, "user"),
                                           usage_line({"input_tokens": 100_000, "cache_read_input_tokens": 300_000,
                                                       "cache_creation_input_tokens": 5_000}))]},
    {"case": "newer_under_threshold", "files": [
        f("over.jsonl", 1000, usage_line({"input_tokens": 405_000})),
        f("under.jsonl", 2000, usage_line({"input_tokens": 1_000, "cache_read_input_tokens": 2_000}))]},
    {"case": "newer_sdk_cli_ignored", "files": [
        f("supervisor.jsonl", 1000, usage_line({"input_tokens": 900_000})),
        f("collector.jsonl", 9000, usage_line({"input_tokens": 1}, entrypoint="sdk-cli"))]},
    {"case": "missing_dir", "missing": True, "files": []},
    {"case": "empty_dir", "files": []},
    {"case": "env_window_threshold", "window": "1000", "threshold": "5",
     "files": [f("t.jsonl", 1000, usage_line({"input_tokens": 50, "cache_read_input_tokens": 50}))]},
    {"case": "env_threshold_50", "window": "1000", "threshold": "50",
     "files": [f("t.jsonl", 1000, usage_line({"input_tokens": 50, "cache_read_input_tokens": 50}))]},
    {"case": "exactly_at_threshold", "files": [f("t.jsonl", 1000, usage_line({"input_tokens": 250_000}))]},
    {"case": "just_under_threshold", "files": [f("t.jsonl", 1000, usage_line({"input_tokens": 249_999}))]},
    {"case": "rounding_tie", "files": [f("t.jsonl", 1000, usage_line({"input_tokens": 625_000}))]},
    {"case": "window_zero", "window": "0", "files": [f("t.jsonl", 1000, usage_line({"input_tokens": 5}))]},
    {"case": "bad_window", "window": "lots", "files": [f("t.jsonl", 1000, usage_line({"input_tokens": 5}))]},
    {"case": "bad_usage_field", "files": [f("t.jsonl", 1000, usage_line({"input_tokens": "many"}))]},
]


def materialize(directory, files):
    os.makedirs(directory, exist_ok=True)
    for spec in files:
        path = os.path.join(directory, spec["name"])
        with open(path, "w", encoding="utf-8", newline="\n") as out:
            out.write("".join(line + "\n" for line in spec["lines"]))
        os.utime(path, (spec["mtime"], spec["mtime"]))


def run_self_ctx(case):
    with tempfile.TemporaryDirectory() as tmp:
        directory = os.path.join(tmp, "missing") if case.get("missing") else tmp
        if not case.get("missing"):
            materialize(tmp, case["files"])
        fc.SELF_CTX_DIR = directory
        for var, key in (("SELF_CTX_WINDOW", "window"), ("SELF_CTX_THRESHOLD", "threshold")):
            if key in case:
                os.environ[var] = case[key]
            else:
                os.environ.pop(var, None)
        size = os.path.getsize(fc.LOG) if os.path.exists(fc.LOG) else 0
        alerts = fc.self_ctx_alerts(NOW)
        logged = (os.path.getsize(fc.LOG) if os.path.exists(fc.LOG) else 0) > size
        os.environ.pop("SELF_CTX_WINDOW", None)
        os.environ.pop("SELF_CTX_THRESHOLD", None)
    return json.dumps({"case": case["case"], "alerts": alerts, "logged": logged}, ensure_ascii=False)


# --- pane_session_id / transcript_ctx_pct ------------------------------------------


def session(name, pid, sid, tmux, updated=1, raw=None):
    return {"name": name, "pid": pid, "sid": sid, "tmux": tmux, "updatedAt": updated, "raw": raw}


def transcript(folder, sid, tokens, mtime):
    return {"dir": folder, "sid": sid, "tokens": tokens, "mtime": mtime}


CWD = "-Users-x-trusty-tools"
TRANSCRIPT = [
    {"case": "same_cwd_own_ctx", "sessions": [session("a.json", "self", "sid-a", "tm-x:@1.%1"),
                                              session("b.json", "dead", "sid-b", "tm-x:@1.%2")],
     "transcripts": [transcript(CWD, "sid-a", 450_000, 2000), transcript(CWD, "sid-b", 180_000, 1000)],
     "queries": ["%1", "%2", "%9", "", "%"]},
    {"case": "pid_one_is_alive", "sessions": [session("b.json", 1, "sid-b", "tm-x:@1.%2")],
     "transcripts": [transcript(CWD, "sid-b", 180_000, 1000)], "queries": ["%2", "%22"]},
    {"case": "newest_updated_wins", "sessions": [
        session("a.json", "self", "sid-old", "tm-x:@1.%3", 5), session("b.json", "self", "sid-new", "tm-x:@1.%3", 9),
        session("c.json", "dead", "sid-dead", "tm-x:@1.%3", 99)],
     "transcripts": [transcript(CWD, "sid-old", 100_000, 1000), transcript(CWD, "sid-new", 300_000, 1000),
                     transcript(CWD, "sid-dead", 900_000, 1000)], "queries": ["%3"]},
    {"case": "newest_transcript_folder_wins", "sessions": [session("a.json", "self", "sid-a", "s:@1.%4")],
     "transcripts": [transcript("-w-one", "sid-a", 100_000, 1000), transcript("-w-two", "sid-a", 230_000, 3000),
                     transcript(".hidden", "sid-a", 990_000, 9000)], "queries": ["%4"]},
    {"case": "window_env", "window": "500000", "sessions": [session("a.json", "self", "sid-a", "s:@1.%5")],
     "transcripts": [transcript(CWD, "sid-a", 123_456, 1000)], "queries": ["%5"]},
    {"case": "window_zero", "window": "0", "sessions": [session("a.json", "self", "sid-a", "s:@1.%5")],
     "transcripts": [transcript(CWD, "sid-a", 123_456, 1000)], "queries": ["%5"]},
    {"case": "no_transcript_and_bad_entries", "sessions": [
        session("a.json", "self", "sid-a", "s:@1.%6"), session("note.txt", "self", "sid-x", "s:@1.%7"),
        session("bad.json", None, None, None, raw="{not json"),
        session("nopid.json", None, "sid-y", "s:@1.%8", raw=json.dumps({"sessionId": "sid-y", "tmux": "s:@1.%8"})),
        session("strpid.json", "self-str", "sid-z", "s:@1.%10")],
     "transcripts": [transcript(CWD, "sid-z", 470_000, 1000)], "queries": ["%6", "%7", "%8", "%10"]},
    {"case": "no_sessions_dir", "missing_sessions": True, "sessions": [], "transcripts": [], "queries": ["%1"]},
]


def run_transcript(case):
    with tempfile.TemporaryDirectory() as tmp:
        base, sdir = os.path.join(tmp, "projects"), os.path.join(tmp, "sessions")
        if not case.get("missing_sessions"):
            os.makedirs(sdir)
        for s in case["sessions"]:
            pid = {"self": os.getpid(), "self-str": str(os.getpid()), "dead": DEAD_PID}.get(s["pid"], s["pid"])
            text = s["raw"] if s["raw"] is not None else json.dumps(
                {"pid": pid, "sessionId": s["sid"], "tmux": s["tmux"], "updatedAt": s["updatedAt"]})
            with open(os.path.join(sdir, s["name"]), "w", encoding="utf-8") as out:
                out.write(text)
        for t in case["transcripts"]:
            d = os.path.join(base, t["dir"])
            os.makedirs(d, exist_ok=True)
            p = os.path.join(d, t["sid"] + ".jsonl")
            with open(p, "w", encoding="utf-8") as out:
                out.write(json.dumps({"type": "assistant", "message": {"usage": {"input_tokens": t["tokens"]}}}) + "\n")
            os.utime(p, (t["mtime"], t["mtime"]))
        fc.TRANSCRIPT_BASE, fc.SESSIONS_DIR = base, sdir
        fc._transcript_cache.clear()
        if "window" in case:
            os.environ["PANE_CTX_WINDOW"] = case["window"]
        else:
            os.environ.pop("PANE_CTX_WINDOW", None)
        out = [[q, fc.transcript_ctx_pct(q)] for q in case["queries"]]
        os.environ.pop("PANE_CTX_WINDOW", None)
    return json.dumps({"case": case["case"], "results": out})


# --- alerts.jsonl line bytes --------------------------------------------------------

ALERT_ROWS = [
    ["idle", "since-1000", "idle 10 min, no agents running", "tm-x", "%1", 1600.0],
    ["permission_prompt", "0123456789ab", "café — ❯ \U0001f4c5 \"quoted\" \\ tab\there",
     "tm-é", "%2", NOW],
    ["load", "load", "load 30.0/25.0/20.0 (15m > 1.5x16 cores)", "", "", 0.0],
    ["self_ctx", "25", "self ctx 41% (405000/1000000 tokens)", "tm-architect", "", 1759000000.25],
    ["dialog", "abc", "ctrl \x01 and \x7f del", "tm-y", "%3", 86399.9999999],
]

# --- Unicode \d ------------------------------------------------------------------------


def digit_ranges():
    pat = re.compile(r"\d")
    ranges, start, prev = [], None, None
    for c in range(0x110000):
        if 0xD800 <= c <= 0xDFFF:
            continue
        if pat.match(chr(c)):
            if start is None or c != prev + 1:
                if start is not None:
                    ranges.append([start, prev])
                start = c
            prev = c
    ranges.append([start, prev])
    return ranges


def digit_zeros(ranges):
    zeros = []
    for lo, hi in ranges:
        assert (hi - lo + 1) % 10 == 0, (lo, hi)
        for z in range(lo, hi + 1, 10):
            assert all(unicodedata.decimal(chr(z + k)) == k for k in range(10)), z
            zeros.append(z)
    return zeros


def write_digit_table(zeros):
    body = ",\n".join("    " + ", ".join(f"0x{z:05X}" for z in zeros[i:i + 6]) for i in range(0, len(zeros), 6))
    text = (
        "//! Python `re` `\\d` on `str`: the first code point (digit zero) of every\n"
        "//! run of ten decimal digits, as the prototype's Python reports them.\n"
        "//!\n"
        "//! Generated by `tests/fixtures/a3/regen_a3.py` (#9171 A3a, Architect ruling\n"
        f"//! Q6) from Python {sys.version.split()[0]}, Unicode {unicodedata.unidata_version}. Do not edit by hand.\n\n"
        "/// Digit-zero code points, ascending; each starts a run of ten digits.\n"
        "#[rustfmt::skip]\n"
        f"pub(crate) const DIGIT_ZEROS: [u32; {len(zeros)}] = [\n{body},\n];\n")
    with open(os.path.join(CRATE, "src", "decimal_table.rs"), "w", encoding="ascii", newline="\n") as out:
        out.write(text)


# --- wake() and dispatch_wake() (#9171 A3b-1) ---------------------------------------------
#
# Each wake scenario scripts tmux per verb (a FIFO of stdout strings, or
# {"err": text}; a verb with an empty FIFO answers its fallback, else raises)
# and records every argv the prototype passes to tmux plus the return phrase.
# The captures are the shapes tests/test_fleet_poll.py builds (screen(),
# _wrapped_box_screen(), PERMISSION). "{msg}" in a capture is replaced by the
# pointer the prototype itself would type, read off a dry-run wake().
# A scenario with a "ruling" diverges from the prototype by an Architect
# ruling: wake.jsonl records the ruled argv and phrase, and
# wake_rulings.jsonl records what the prototype does instead.

WAKE_ALERTS = "/w/ïnbox/alerts.jsonl"  # non-ASCII: BSpace counts characters, not bytes
TEST_RULE = "─" * 80
DIM, RESET = "\x1b[2m", "\x1b[0m"
PM_TURN = ["⏺ Merged #12 and the CI run is green.", "", "  Next: the release notes."]


def screen(body, box="", dim=False):
    """tests/test_fleet_poll.py screen(): transcript, input box, footer."""
    box_line = "❯\xa0" + (f"{DIM}{box}{RESET}" if dim else box)
    footer = ["  TM 1.7.4 ● | org/proj ⎇ tm-x | @me | Opus 5.5 | ctx 30% | $1.00 | "
              "⏳30% 📅40% | 💸1%/1%",
              "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 9 agents",
              " " * 70 + "/rc", "", "  ⏺ main"]
    return "\n".join(list(body) + [TEST_RULE, box_line, TEST_RULE] + footer) + "\n\n"


def wrapped(text):
    """tests/test_fleet_poll.py _wrapped_box_screen(PM_TURN, text)."""
    half = len(text) // 2
    return "\n".join(PM_TURN + [TEST_RULE, f"❯\xa0{text[:half]}", text[half:], TEST_RULE]) + "\n"


PERMISSION = "\n".join(["⏺ Bash(rm -rf build/)", "", TEST_RULE, " Bash command · from the local-ops agent",
                        "", "   rm -rf build/", "   Remove the build dir", "", " Do you want to proceed?",
                        " ❯ 1. Yes", "   2. Yes, and don't ask again for rm commands in this project",
                        "   3. No, and tell Claude what to do differently (esc)", "", " Esc to cancel"]) + "\n"
EMPTY, NO_BOX = screen(PM_TURN), "❯\n\n  ✻ Doing work… (2m 14s · esc to interrupt)\n"
STALE = "[poll 11:15Z] 4 new alerts -> /Users/x/inbox/alerts.jsonl"
SUP = ["tm-architect", "%1", "11"]
OWNER = {"display-message": "tm-architect\n", "send-keys": ""}
BOX_CHANGED = "failed: text not confirmed in the input box; box changed, nothing erased"
DM = ["display-message", "-p", "-t", "%1", "#{session_name}"]
CAP = ["capture-pane", "-e", "-p", "-S", "-80", "-t", "%1"]


def sc(case, captures, panes=(SUP,), n=3, logged=None, dry_run=False, fallback=OWNER, script=None, ruling=None):
    return {"case": case, "panes": [list(p) for p in panes], "n": n, "logged": logged, "dry_run": dry_run,
            "script": dict(script or {}, **{"capture-pane": captures}), "fallback": dict(fallback),
            "ruling": ruling}


Q1 = "A3b Q1: an empty box after typing sends nothing"
Q3 = "A3 Q3 / rule 4: Enter only on equality; BSpace only when the box ends with our own text"
WAKE = [
    sc("no_tm_architect_pane", [], panes=(["tm-x", "%4", "11"],)),
    sc("no_input_box_found", [NO_BOX]),
    sc("typed_draft", [screen(PM_TURN, box="half-typed reply")]),
    sc("dialog_open", [PERMISSION]),
    sc("own_stale_pointer_wrapped", [wrapped(STALE)]),
    sc("human_draft_starting_with_poll", [screen(PM_TURN, box="poll the team about the release date")]),
    sc("dry_run", [EMPTY], dry_run=True),
    sc("wrapped_confirm_then_enter", [EMPTY, "{wrapped}", EMPTY]),
    sc("suggestion_box_sends", [screen(PM_TURN, box="try /tm-fleet-check", dim=True), "{wrapped}", EMPTY]),
    sc("need_you_form_one_alert", [EMPTY, "{wrapped}", EMPTY], n=1, logged=0),
    sc("need_you_form_many", [EMPTY, "{wrapped}", EMPTY], n=4, logged=7),
    sc("still_in_box_after_enter", [EMPTY, "{wrapped}", "{boxed}"]),
    sc("active_pane_is_chosen", [EMPTY, "{wrapped}", EMPTY],
       panes=(["tm-architect", "%1", "10"], ["tm-architect", "%2", "11"], ["tm-x", "%3", "11"])),
    sc("wrong_session_refused", [EMPTY], fallback={"display-message": "tm-api\n", "send-keys": ""}),
    sc("empty_box_after_typing", [EMPTY], fallback=dict(OWNER, **{"capture-pane": EMPTY}),
       ruling={"name": Q1, "keep": 4, "erase": False, "result": BOX_CHANGED}),
    sc("message_plus_extra_text", [EMPTY, "{msg_extra}", EMPTY],
       ruling={"name": Q3, "keep": 4, "erase": False, "result": BOX_CHANGED}),
    sc("draft_then_message_left_draft", [EMPTY, "{draft_msg}", screen(PM_TURN, box="draft")],
       ruling={"name": Q3, "keep": 4, "erase": True,
               "result": "failed: text not confirmed in the input box; cleanup left box non-empty"}),
    sc("draft_then_message_box_emptied", [EMPTY, "{draft_msg}", EMPTY],
       ruling={"name": Q3, "keep": 4, "erase": True,
               "result": "failed: text not confirmed in the input box; cleaned up, box empty"}),
]


def _pane_dicts(panes):
    return [{"session": s, "pane": p, "cmd": "claude", "active": a, "path": "", "dead": "0"} for s, p, a in panes]


def _resolve(text, msg):
    return {"{wrapped}": wrapped(msg) if msg else text,
            "{boxed}": screen(PM_TURN, box=msg) if msg else text,
            "{msg_extra}": screen(PM_TURN, box=f"{msg} and more") if msg else text,
            "{draft_msg}": screen(PM_TURN, box=f"draft {msg}") if msg else text}.get(text, text)


def run_wake(case):
    calls = []
    fifo = {verb: list(outs) for verb, outs in case["script"].items()}

    def tmux(*args):
        calls.append(list(args))
        queue = fifo.get(args[0])
        if queue:
            out = queue.pop(0)
        elif args[0] in case["fallback"]:
            out = case["fallback"][args[0]]
        else:
            raise fc.TmuxError(f"{args[0]}: unscripted")
        if isinstance(out, dict):
            raise fc.TmuxError(f"{args[0]}: {out['err']}")
        return out

    fc.tmux = tmux
    panes = _pane_dicts(case["panes"])
    try:
        return calls, fc.wake(case["n"], panes, case["dry_run"], case["logged"]), None
    except fc.TmuxError as e:
        return calls, None, str(e)


def pointer_for(case):
    """The pointer the prototype types for this scenario: a dry-run wake()
    over an empty box on the scenario's panes."""
    probe_case = dict(case, dry_run=True, script={"capture-pane": [EMPTY]}, fallback=OWNER)
    _, result, _ = run_wake(probe_case)
    return result.split(": ", 2)[2] if result and result.startswith("dry-run:") else None


class _FixedDateTime(datetime):
    @classmethod
    def now(cls, tz=None):
        return datetime.fromtimestamp(NOW, tz)


def wake_rows():
    saved = fc.ALERTS, fc.datetime, fc.tmux
    fc.ALERTS, fc.datetime = WAKE_ALERTS, _FixedDateTime
    rows, rulings = [], []
    try:
        for case in WAKE:
            msg = pointer_for(case)
            case["script"]["capture-pane"] = [_resolve(c, msg) for c in case["script"]["capture-pane"]]
            calls, result, raises = run_wake(case)
            got = {"argv": calls, "result": result, "raises": raises}
            if case["ruling"]:
                r = case["ruling"]
                argv = calls[:r["keep"]]
                if r["erase"]:
                    argv += [DM, ["send-keys", "-t", "%1"] + ["BSpace"] * len(msg), CAP]
                rulings.append(json.dumps({"case": case["case"], "ruling": r["name"], "python": got},
                                          ensure_ascii=False))
                got = {"argv": argv, "result": r["result"], "raises": None}
            rows.append(json.dumps(dict({"case": case["case"]}, **got), ensure_ascii=False))
    finally:
        fc.ALERTS, fc.datetime, fc.tmux = saved
    return rows, rulings


# dispatch_wake(): the fake wake takes the live four arguments (the frozen
# tests' three-argument fake raises TypeError against the live call).
DISPATCH = [
    {"case": "skip_then_send_carries_total", "steps": [
        [3, None, False, "skipped: no input box found"], [2, None, False, "sent to %1"]]},
    {"case": "failed_confirm_keeps_pending", "steps": [
        [4, None, False, "failed: text not confirmed in the input box; box changed, nothing erased"],
        [0, None, False, "failed: text not confirmed in the input box; box changed, nothing erased"]]},
    {"case": "nothing_pending", "steps": [[0, None, False, None], [0, 2, False, None]]},
    {"case": "logged_carried_then_cleared", "steps": [
        [2, 3, False, "skipped: input typed"], [0, 1, False, "sent to %1 (still in box after Enter)"],
        [0, 0, False, None]]},
    {"case": "dry_run_sets_no_sent_at", "steps": [
        [5, None, True, "dry-run: would send to %1: x"], [0, None, True, "sent to %1"]]},
    {"case": "raise_is_failed", "steps": [
        [1, None, False, {"raise": "refusing to send to %1 (session 'tm-api')"}]]},
]


def dispatch_rows():
    saved = fc.wake, fc.time
    rows = []
    try:
        fc.time = types.SimpleNamespace(time=lambda: NOW, sleep=lambda s: None)
        for case in DISPATCH:
            state = {}
            for step, (new, logged, dry_run, result) in enumerate(case["steps"]):
                calls = []

                def fake_wake(n, panes, dry, lg, _r=result, _calls=calls):
                    _calls.append([n, lg])
                    if isinstance(_r, dict):
                        raise fc.TmuxError(_r["raise"])
                    return _r

                fc.wake = fake_wake
                got = fc.dispatch_wake(state, [], new, dry_run, logged)
                rows.append(json.dumps({"case": case["case"], "step": step, "result": got, "calls": calls,
                                        "state": state}, sort_keys=True))
    finally:
        fc.wake, fc.time = saved
    return rows


# --- write ------------------------------------------------------------------------------


def write(name, rows):
    with open(os.path.join(HERE, name), "w", encoding="utf-8", newline="\n") as out:
        out.write("".join(r + "\n" for r in rows))


def list_panes_rows():
    rows = []
    for case, out in LIST_PANES:
        fc.tmux = lambda *args, _out=out: _out
        rows.append(json.dumps([case, fc.list_panes()]))
    return rows


def main():
    wake, rulings = wake_rows()  # resolves the "{msg}" captures in WAKE first
    corpus = {"list_panes": LIST_PANES, "band": BAND, "band_alerts": BAND_ALERTS, "host": HOST,
              "events": EVENTS, "self_ctx_alerts": SELF_CTX_ALERTS, "transcript": TRANSCRIPT,
              "alert_rows": ALERT_ROWS, "now": NOW, "dead_pid": DEAD_PID,
              "wake": WAKE, "wake_alerts": WAKE_ALERTS, "dispatch": DISPATCH}
    with open(os.path.join(HERE, "corpus_a3.json"), "w", encoding="ascii", newline="\n") as out:
        json.dump(corpus, out, indent=1)
        out.write("\n")
    write("list_panes.jsonl", list_panes_rows())
    write("band.jsonl", [json.dumps([row, fc.builder_band(*row[:5], prev=row[5])]) for row in BAND])
    rows = []
    for sc in BAND_ALERTS:
        state = dict(sc["state"])
        for step, args in enumerate(sc["steps"]):
            rows.append(json.dumps({"case": sc["case"], "step": step, "alerts": fc.band_alerts(state, *args),
                                    "state": state}, ensure_ascii=False))
    write("band_alerts.jsonl", rows)
    write("host.jsonl", [json.dumps(run_host(c), ensure_ascii=False) for c in HOST])
    write("events.jsonl", [r for c in EVENTS for r in run_events(c)])
    write("self_ctx_alerts.jsonl", [run_self_ctx(c) for c in SELF_CTX_ALERTS])
    write("transcript_ctx.jsonl", [run_transcript(c) for c in TRANSCRIPT])
    # The append in cycle(): one json.dumps(a, ensure_ascii=False) + "\n" per alert.
    with open(os.path.join(HERE, "alerts_lines.jsonl"), "w", encoding="utf-8", newline="\n") as f_:
        for a in [fc.alert(*row) for row in ALERT_ROWS]:
            f_.write(json.dumps(a, ensure_ascii=False) + "\n")
    write("wake.jsonl", wake)
    write("wake_rulings.jsonl", rulings)
    write("dispatch_wake.jsonl", dispatch_rows())
    ranges = digit_ranges()
    write("digits.jsonl", [json.dumps(r) for r in ranges])
    write_digit_table(digit_zeros(ranges))


if __name__ == "__main__":
    main()
