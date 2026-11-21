"""Regenerate the df-T7i P1b parity goldens from the live prototype.

Why: P1b ports the sensors and wake helpers fleet-poll.py gained after 988152c.
The goldens must come from running that Python, never from the Rust.
What: loads fleet-poll.py by path from the prototype directory given as the one
argument (read-only: -B -I, sys.dont_write_bytecode), points every path it could
write at a system tempdir, injects the clock and the zone readers, and writes
corpus_p1b.json plus one golden per surface into this directory. Captures for
the completion cases come from ../parity/corpus.json. It never touches the
live inbox, poll-state.json or alerts.jsonl.
Test: tests/parity_p1b.rs reads every file written here.

Regenerate (from the repository root):
    python3 -B -I crates/trusty-architect/tests/fixtures/p1b/regen_p1b.py <prototype-dir>
where <prototype-dir> is the Architect prototype's scripts/ directory, with
usage.py and input-state.py pinned at 988152c (see ../a3/README.md).
"""
import importlib.util
import json
import os
import sys
import tempfile
import types

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
if len(sys.argv) != 2:
    sys.exit("usage: regen_p1b.py <prototype-dir>")
PROTOTYPE = sys.argv[1]
SCRATCH = tempfile.mkdtemp(prefix="regen-p1b-")
os.environ["SUPERVISOR_INBOX_DIR"] = os.path.join(SCRATCH, "inbox")
os.environ["IL_SCHEDULE"] = os.path.join(SCRATCH, "no-schedule.json")
for var in ("ARCHITECT_INBOX_DIR", "SELF_CTX_WINDOW", "SELF_CTX_THRESHOLD"):
    os.environ.pop(var, None)

spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(PROTOTYPE, "fleet-poll.py"))
fc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fc)
os.makedirs(fc.INBOX, exist_ok=True)
fc.LOG = os.path.join(SCRATCH, "poll.log")
NOW = 1_760_000_000.5
fc.time = types.SimpleNamespace(time=lambda: NOW, sleep=lambda s: None)
with open(os.path.join(HERE, "..", "parity", "corpus.json"), encoding="utf-8") as f:
    CAPTURES = dict(json.load(f)["captures"])


def write(name, rows):
    with open(os.path.join(HERE, name), "w", encoding="utf-8", newline="\n") as out:
        out.write("".join(r + "\n" for r in rows))


# --- ctx_bar --------------------------------------------------------------------
CTX_BAR = [
    {"case": "no_schedule_pm", "session": "tm-a", "window": 1_000_000, "schedule": None},
    {"case": "no_schedule_architect", "session": "tm-architect", "window": 1_000_000, "schedule": None},
    {"case": "architect_bar_450k", "session": "tm-architect", "window": 1_000_000,
     "schedule": {"thresholds": {"architect_bar_ctx": 450_000}}},
    {"case": "architect_bar_pm_unchanged", "session": "tm-a", "window": 1_000_000,
     "schedule": {"thresholds": {"architect_bar_ctx": 450_000}}},
    {"case": "bar_bool_ignored", "session": "tm-architect", "window": 1_000_000,
     "schedule": {"thresholds": {"architect_bar_ctx": True}}},
    {"case": "bar_zero_ignored", "session": "tm-architect", "window": 1_000_000,
     "schedule": {"thresholds": {"architect_bar_ctx": 0}}},
    {"case": "bar_float_ignored", "session": "tm-architect", "window": 1_000_000,
     "schedule": {"thresholds": {"architect_bar_ctx": 400_000.0}}},
    {"case": "thresholds_not_a_dict", "session": "tm-architect", "window": 1_000_000,
     "schedule": {"thresholds": None}},
    {"case": "small_window", "session": "tm-architect", "window": 700_000,
     "schedule": {"thresholds": {"architect_bar_ctx": 450_000}}},
    {"case": "bad_json", "session": "tm-architect", "window": 1_000_000, "schedule": "not json"},
]


def ctx_bar_rows():
    rows = []
    for c in CTX_BAR:
        path = os.path.join(SCRATCH, f"sched-{c['case']}.json")
        if c["schedule"] is not None:
            with open(path, "w") as f:
                f.write(c["schedule"] if isinstance(c["schedule"], str) else json.dumps(c["schedule"]))
        rows.append(json.dumps({"case": c["case"],
                                "bar_pct": fc.ctx_bar.bar_pct(c["session"], c["window"], path)}))
    return rows


# --- cap_wake -------------------------------------------------------------------
HEAD = "[poll 07:00Z] 3 alerts need you (1 logged) -> /x/inbox/alerts.jsonl"
CAP_WAKE = [
    ["empty_digest", HEAD, ""],
    ["fits", HEAD, "tm-a idle: idle 12 min"],
    ["cut_with_ellipsis", HEAD, "item " * 300],
    ["whitespace_collapsed", HEAD, "a\n b\t\tc   d"],
    ["no_room", "h" * 390, "digest text here"],
    ["room_exactly_20", "h" * 377, "x" * 30],
    ["room_19", "h" * 378, "x" * 30],
    ["unicode_digest", HEAD, "é" * 400],
]

# --- ctx_cycle_alerts ---------------------------------------------------------------
CTX_CYCLE = [
    {"case": "missing_file", "steps": [{"content": None, "dry_run": False}]},
    {"case": "two_lines_then_one_more", "steps": [
        {"content": '{"session": "tm-a", "fingerprint": "f1", "detail": "cycle failed"}\nnot json\n'
                    '{"fingerprint": "f2"}\n', "dry_run": False},
        {"append": '{"session": "tm-b", "fingerprint": "f3", "detail": "x"}\n', "dry_run": False},
        {"append": "", "dry_run": False}]},
    {"case": "dry_run_keeps_offset", "steps": [
        {"content": '{"session": "tm-a", "fingerprint": "f1", "detail": "d"}\n', "dry_run": True},
        {"append": "", "dry_run": True}]},
    {"case": "partial_last_line", "steps": [
        {"content": '{"session": "tm-a", "fingerprint": "f1", "detail": "d"}\n{"session": "tm-b"', "dry_run": False},
        {"append": ', "fingerprint": "f2"}\n', "dry_run": False}]},
]


def ctx_cycle_rows():
    rows = []
    for c in CTX_CYCLE:
        path = os.path.join(SCRATCH, f"cc-{c['case']}.jsonl")
        state = {}
        for i, st in enumerate(c["steps"]):
            if st.get("content") is not None:
                with open(path, "w", encoding="utf-8", newline="") as f:
                    f.write(st["content"])
            if st.get("append"):
                with open(path, "a", encoding="utf-8", newline="") as f:
                    f.write(st["append"])
            alerts = fc.ctx_cycle_alerts(state, NOW, st["dry_run"], path)
            rows.append(json.dumps({"case": c["case"], "step": i, "alerts": alerts, "state": state},
                                   ensure_ascii=False))
    return rows


# --- selftest_alerts ----------------------------------------------------------------
SELFTEST = [
    ["missing", None],
    ["clean", {"failures": [], "lines": []}],
    ["failing_with_lines", {"failures": ["ledger_spec", "routines"], "lines": ["ledger_spec: 2 rules", "routines: il-cron stale"]}],
    ["failing_without_lines", {"failures": ["factory"]}],
    ["not_json", "{"],
]


def selftest_rows():
    rows = []
    for name, content in SELFTEST:
        inbox = os.path.join(SCRATCH, f"st-{name}")
        os.makedirs(inbox, exist_ok=True)
        if content is not None:
            with open(os.path.join(inbox, "selftest-state.json"), "w") as f:
                f.write(content if isinstance(content, str) else json.dumps(content))
        rows.append(json.dumps({"case": name, "alerts": fc.selftest_alerts(NOW, inbox)}, ensure_ascii=False))
    return rows


# --- zone_alerts ---------------------------------------------------------------------
BOOT = 1_759_000_000
ZONE = [
    {"case": "hourly_and_dedup_per_boot", "steps": [
        [NOW, 7_999_999, BOOT], [NOW + 60, 9_000_000, BOOT], [NOW + 3600, 9_000_000, BOOT],
        [NOW + 7200, 12_500_000, BOOT], [NOW + 10800, 12_500_000, BOOT + 5]]},
    {"case": "uptime_over_14_days", "steps": [[NOW, 1_000, BOOT - 15 * 86400], [NOW + 3600, 1_000, BOOT - 15 * 86400]]},
    {"case": "no_readings", "steps": [[NOW, None, None]]},
    {"case": "no_boot_time", "steps": [[NOW, 8_500_000, None], [NOW + 3600, 8_500_000, None]]},
]


def zone_rows():
    rows = []
    for c in ZONE:
        state, path = {}, os.path.join(SCRATCH, f"zone-{c['case']}.jsonl")
        for i, (now, el, bt) in enumerate(c["steps"]):
            try:
                alerts = fc.zone_alerts(state, now, reader=lambda e=el: e, boot=lambda b=bt: b, path=path)
            except KeyError as e:  # df-T7i P1b ruling: the port starts an empty fired list
                alerts = {"raises": f"KeyError: {e}"}
            log = open(path).read() if os.path.exists(path) else ""
            rows.append(json.dumps({"case": c["case"], "step": i, "alerts": alerts, "state": state, "log": log},
                                   ensure_ascii=False))
    return rows


# --- completion_alerts ------------------------------------------------------------------
COMPLETION = [
    {"case": "fires_once_then_holds", "steps": [
        ["box_empty", 20, True], ["box_empty", 20, True], ["box_typed", 20, True], ["box_empty", 20, True]]},
    {"case": "below_floor_then_over", "steps": [["idle_suggestion", 14, True], ["idle_suggestion", 15, True]]},
    {"case": "il_open", "steps": [["box_empty", 40, False]]},
    {"case": "busy_and_dialog", "steps": [["busy_agents_waiting", 40, True], ["permission", 40, True]]},
    {"case": "limit_notice", "steps": [["plan_limit_text", 40, True]]},
    {"case": "no_ctx", "steps": [["box_empty", None, True]]},
]


def completion_rows():
    rows = []
    for c in COMPLETION:
        state = {}
        for i, (cap, ctx, il) in enumerate(c["steps"]):
            info = dict(fc.parse_pane(CAPTURES[cap]), ctx=ctx)
            alerts = fc.completion_alerts(state, "tm-x", "%1", info, NOW, il_check=lambda s, v=il: v)
            rows.append(json.dumps({"case": c["case"], "step": i, "alerts": alerts, "state": state},
                                   ensure_ascii=False))
    return rows


# --- il_complete_for ----------------------------------------------------------------------
IL = [
    {"case": "clean", "state": {"session": {}, "tasks": [{"status": "done"}]}},
    {"case": "open_task", "state": {"session": {}, "tasks": [{"status": "open"}]}},
    {"case": "session_task", "state": {"session": {"task": "ab-T12"}, "tasks": []}},
    {"case": "owed", "state": {"session": {"owed": ["x"]}}},
    {"case": "agents", "state": {"session": {"agents": 1}}},
    {"case": "no_tasks_key", "state": {"session": {}}},
    {"case": "task_not_dict", "state": {"tasks": ["done"]}},
    {"case": "state_not_dict", "state": ["x"]},
    {"case": "no_state_file", "state": None},
    {"case": "not_scheduled", "state": {"session": {}}, "name": "tm-other"},
    {"case": "per_session_dir", "state": {"session": {}, "tasks": []}, "per_session": True},
]


def il_rows():
    rows = []
    for c in IL:
        project = os.path.join(SCRATCH, f"proj-{c['case']}")
        base = os.path.join(project, ".trusty-mpm", "datg")
        sdir = os.path.join(base, "tm-x") if c.get("per_session") else base
        os.makedirs(sdir, exist_ok=True)
        if c["state"] is not None:
            with open(os.path.join(sdir, "state.json"), "w") as f:
                json.dump(c["state"], f)
        sched = os.path.join(SCRATCH, f"il-{c['case']}.json")
        with open(sched, "w") as f:
            json.dump({"sessions": [{"name": c.get("name", "tm-x"), "project": project}]}, f)
        os.environ["IL_SCHEDULE"] = sched
        rows.append(json.dumps({"case": c["case"], "complete": fc.il_complete_for("tm-x")}))
    os.environ["IL_SCHEDULE"] = os.path.join(SCRATCH, "no-schedule.json")
    return rows


# --- evaluate: the idle-exempt session; parse_pane: rc_active ----------------------------
RC_LINES = [
    "  TM 1.7.4 | org/proj | ✻me | Opus | /rc",
    "  TM 1.7.4 | org/proj | ✻me | Opus | /rc   ",
    "  TM 1.7.4 | org/proj | ✻me | Opus |/rc",
    "  TM 1.7.4 · /rc",
    "/rc",
    "  TM 1.7.4 | /rc failed",
    "  TM 1.7.4 | x/rc",
    "  TM 1.7.4 | /rc extra",
]


def rc_rows():
    return [json.dumps([line, fc.parse_pane("⏺ done\n" + line + "\n")["rc_active"]], ensure_ascii=False)
            for line in RC_LINES]


def exempt_rows():
    rows = []
    for session in ("tm-architect-questions", "tm-a"):
        ps = {}
        info = dict(fc.parse_pane(CAPTURES["box_empty"]), ctx=40)
        fc.evaluate(info, ps, 0.0, session, "%1")
        alerts = fc.evaluate(info, ps, 700.0, session, "%1")
        rows.append(json.dumps({"session": session, "kinds": [a["kind"] for a in alerts]}))
    return rows


def main():
    corpus = {"now": NOW, "ctx_bar": CTX_BAR, "cap_wake": CAP_WAKE, "ctx_cycle": CTX_CYCLE,
              "selftest": SELFTEST, "zone": ZONE, "completion": COMPLETION, "il": IL, "rc_lines": RC_LINES}
    with open(os.path.join(HERE, "corpus_p1b.json"), "w", encoding="ascii", newline="\n") as out:
        json.dump(corpus, out, indent=1)
        out.write("\n")
    write("ctx_bar.jsonl", ctx_bar_rows())
    write("cap_wake.jsonl", [json.dumps([n, fc.cap_wake(h, d)], ensure_ascii=False) for n, h, d in CAP_WAKE])
    write("ctx_cycle.jsonl", ctx_cycle_rows())
    write("selftest.jsonl", selftest_rows())
    write("zone.jsonl", zone_rows())
    write("completion.jsonl", completion_rows())
    write("il_complete.jsonl", il_rows())
    write("rc_active.jsonl", rc_rows())
    write("idle_exempt.jsonl", exempt_rows())


main()
