"""Regenerate the A2 parity corpus and its goldens from the live prototype.

Why: #9171 A2 holds the Rust ports to byte-identical JSON against the
Architect's live prototype. The goldens must come from running that Python,
never from the Rust. The classifier baseline is the copy inline in
fleet-poll.py, the script the live host runs; fleet-classify.py is stale.
What: loads fleet-poll.py, input-state.py, quiet-sessions.py and self-ctx.py
by path from the prototype directory given as the one argument (read-only: no
bytecode, nothing written there; importing fleet-poll.py writes nothing),
builds the input corpus from the prototype tests' inline strings and
parametrize cases plus the shipped scrubbed capture, and writes corpus.json
and one *.jsonl golden per function into this directory. Temporary files for
the quiet/self-ctx cases go to a system tempdir.
Test: tests/parity.rs reads every file written here.

Regenerate (from the repository root):
    python3 -B -I crates/trusty-architect/tests/fixtures/parity/regen.py <prototype-dir>
where <prototype-dir> is the Architect prototype's scripts/ directory.
"""
import importlib.util
import json
import os
import sys
import tempfile

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
if len(sys.argv) != 2:
    sys.exit("usage: regen.py <prototype-dir>  (the Architect prototype's scripts/ directory)")
PROTOTYPE = sys.argv[1]


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, os.path.join(PROTOTYPE, filename))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


fc = _load("fleet_poll", "fleet-poll.py")
ist = _load("input_state", "input-state.py")
qs = _load("quiet_sessions", "quiet-sessions.py")
sc = _load("self_ctx", "self-ctx.py")

# --- captures: the prototype tests' own shapes (tests/test_fleet_poll.py) ----

ESC, NBSP = chr(27), chr(0xA0)
RULE = "─" * 80
DIM, RESET = ESC + "[2m", ESC + "[0m"


def screen(body, box="", dim=False, agents=(), ctx=30, plan=(30, 40), status=None):
    box_line = "❯" + NBSP + (f"{DIM}{box}{RESET}" if dim else box)
    footer = [f"  TM 1.7.4 ● | org/proj ⎇ tm-x | @me | Opus 5.5 | ctx {ctx}% | $1.00 | "
              f"⏳{plan[0]}% 📅{plan[1]}% | 💸1%/1%",
              "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 9 agents",
              " " * 70 + "/rc", "", "  ⏺ main"]
    footer += [f"  ◯ {a}  Doing things…  3m 2s · ↓ 12.0k tokens" for a in agents]
    lines = list(body) + ([status] if status else []) + [RULE, box_line, RULE] + footer
    return "\n".join(lines) + "\n\n"


def wrapped(body, text):
    half = len(text) // 2
    return "\n".join(list(body) + [RULE, "❯" + NBSP + text[:half], text[half:], RULE]) + "\n"


PM_TURN = ["⏺ Merged #12 and the CI run is green.", "", "  Next: the release notes."]
PM_ASKS = ["⏺ Two options for the cache.", "",
           "  - A: keep it", "  - B: drop it", "", "  Which do you want, A or B?"]
PERMISSION = "\n".join([
    "⏺ Bash(rm -rf build/)", "", RULE, " Bash command · from the local-ops agent", "",
    "   rm -rf build/", "   Remove the build dir", "", " Do you want to proceed?",
    " ❯ 1. Yes", "   2. Yes, and don't ask again for rm commands in this project",
    "   3. No, and tell Claude what to do differently (esc)", "", " Esc to cancel",
]) + "\n"
ASK_USER = "\n".join([
    "⏺ I need a decision.", RULE, " ☐ Cache  ✔ Submit  →", "", "Keep the cache?", "",
    "❯ 1. Keep (Recommended)", "     Faster builds", "  2. Drop", "  3. Type something.",
    RULE, "  4. Chat about this", "", "Enter to select · ↑/↓ to navigate · Esc to cancel",
]) + "\n"
POINTER = "[poll 11:15Z] 4 new alerts -> /Users/x/inbox/alerts.jsonl"

with open(os.path.join(HERE, "..", "idle-pane-labeled-rule.txt"), encoding="utf-8") as f:
    LABELED = f.read()

# scripts/test_fleet_poll_idle.py and scripts/test_fleet_poll_rc.py shapes: the
# fleet-poll.py-only fields (agents anywhere with a timer, current_waiting,
# turn_active, account, rc_failed).
R40 = "─" * 40
ROWS = ("  ◯ version-control  Polling smoke-prod gate        6m 34s · ↓ 52.9k tokens\n"
        "  ◯ code-critic      Reading English bundle         45s · ↓ 124.7k tokens\n")
STALE = "".join("✻ Waiting for 1 background agent to finish\n\n" for _ in range(3))
RC_BASE = f"⏺ Done.\n\n{R40}\n❯ \n{R40}\n  ⏵⏵ bypass permissions on (shift+tab to cycle)"

CAPTURES = {
    "box_empty": screen(PM_TURN),
    "box_suggestion": screen(PM_TURN, box="merge PR #7 once CI is green", dim=True),
    "box_typed": screen(PM_TURN, box="half-typed reply"),
    "permission": PERMISSION,
    "permission_moved": PERMISSION.replace(" ❯ 1. Yes", "   1. Yes").replace("   2. Yes,", " ❯ 2. Yes,"),
    "ask_user": ASK_USER,
    "dialog_words_above_box": screen(["⏺ The prompt said:", "  Do you want to proceed?", "  ❯ 1. Yes"],
                                     agents=["engineer"]),
    "labeled_rule_fixture": LABELED,
    "idle_worked": screen(PM_TURN, status="✻ Worked for 3m 2s"),
    "idle_suggestion": screen(PM_TURN, box="do the next thing", dim=True),
    "busy_agents_waiting": screen(PM_ASKS, agents=["python-engineer", "research"],
                                  status="✻ Waiting for 2 background agents to finish"),
    "busy_undulating": screen(PM_ASKS, status="✳ Undulating… (5m 23s · ↓ 19.1k tokens)"),
    "busy_waiting_one": screen(PM_ASKS, status="✻ Waiting for 1 background agent to finish"),
    "waiting_line_anywhere": screen(["✻ Waiting for 1 background agent to finish"] + PM_TURN),
    "question_cooked": screen(PM_ASKS, status="✻ Cooked for 23s · done 7:36 AM"),
    "user_turn": screen(PM_TURN + ["", "❯ can you check the logs?"]),
    "ctx_47": screen(PM_TURN, agents=["x"], ctx=47),
    "ctx_52": screen(PM_TURN, agents=["x"], ctx=52),
    "ctx_44": screen(PM_TURN, agents=["x"], ctx=44),
    "ctx_compacted": screen(PM_TURN, agents=["x"]).replace("ctx 30% |", "⤵ 500k→77k (92%) |"),
    "ctx_over_200k": screen(PM_TURN, agents=["x"]).replace("ctx 30% |", "ctx>200k |"),
    "plan_limit_text": screen(PM_TURN + ["  ⎿  Claude usage limit reached. Your limit will reset at 5pm."],
                              agents=["x"]),
    "plan_prose": screen(["⏺ Both agents died at the account's usage limit."], agents=["x"]),
    "plan_91": screen(PM_TURN, plan=(91, 50)),
    "plan_88": screen(PM_TURN, plan=(88, 50)),
    "plan_week_95": screen(PM_TURN, plan=(10, 95)),
    "errors_old": screen(PM_TURN + ["  ⎿  Error: Exit code 1"], agents=["x"]),
    "errors_new": screen(PM_TURN + ["  ⎿  Error: Exit code 1", "⏺ API Error: 529 overloaded"], agents=["x"]),
    "stale_prompt_busy": "❯\n\n  ✻ Doing work… (2m 14s · esc to interrupt)\n",
    "wrapped_draft": wrapped(PM_TURN, "a long typed reply that wraps across two screen rows"),
    "wrapped_pointer": wrapped(PM_TURN, POINTER),
    "draft_starting_with_poll": screen(PM_TURN, box="poll the team about the release date"),
    "empty_capture": "",
    "no_prompt": "plain shell output\n$ ls\n",
    "crlf_box_empty": screen(PM_TURN).replace("\n", "\r\n"),
    "secret_question": screen(["⏺ Set it up.", "", "  Should I export API_KEY=sk123abc and auth_token: xyz?"]),
    "long_token_error": screen(PM_TURN + ["⏺ API Error: key " + "a1b2" * 10 + " rejected"], agents=["x"]),
    "box_labeled_synthetic": screen(PM_TURN).replace(RULE + "\n❯", "─" * 40 + " 10-12/2026 ──\n❯", 1),
    "unicode_question": screen(["⏺ Résumé prêt — on publie maintenant ? 🚀", "", "  Ça te va ?"]),
    "dim_suggestion_wrapped": "\n".join(PM_TURN + [RULE, "❯" + NBSP + DIM + "first half of a" + RESET,
                                                   DIM + "long suggestion" + RESET, RULE]) + "\n",
    "box_empty_text_below": "\n".join(PM_TURN + [RULE, "❯" + NBSP, "stray draft row", RULE]) + "\n",
    "option_below_box": screen(PM_TURN) + " ❯ 1. Yes\n Esc to cancel\n",
    "osc_and_charset": ESC + "]0;title" + chr(7) + ESC + "(B" + screen(PM_TURN),
    "ctx_21": screen(PM_TURN, agents=["x"], ctx=21),
    "ctx_22": screen(PM_TURN, agents=["x"], ctx=22),
    "ctx_24": screen(PM_TURN, agents=["x"], ctx=24),
    "ctx_25": screen(PM_TURN, agents=["x"], ctx=25),
    "rows_no_bottom_rule": f"⏺ Done.\n\n{R40}\n❯ \n  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 2 agents\n"
                           f"  ⏺ main\n{ROWS}",
    "waiting_no_bottom_rule": f"⏺ x\n✻ Waiting for 3 background agents to finish\n\n{R40}\n❯ \n  ⏵⏵ bypass\n",
    "no_rows_no_bottom_rule": f"⏺ Done.\n\n{R40}\n❯ \n  ⏵⏵ bypass permissions on (shift+tab to cycle)\n",
    "stale_waiting_then_done": f"{STALE}✻ Brewed for 42s · done 1:13 AM\n\n{R40}\n❯ \n{R40}\n  ⏵⏵ bypass\n",
    "newest_waiting": f"✻ Brewed for 42s · done 1:13 AM\n\n✻ Waiting for 1 background agent to finish\n\n"
                      f"{R40}\n❯ \n{R40}\n  ⏵⏵ bypass\n",
    "waiting_then_worked_for": f"{STALE}✻ Worked for 3m 2s\n\n{R40}\n❯ \n{R40}\n",
    "waiting_then_for_hours": f"{STALE}✻ Churned for 5 h\n\n{R40}\n❯ \n{R40}\n",
    "waiting_then_user_turn": f"{STALE}❯ status?\n\n⏺ Still on it.\n\n{R40}\n❯ \n{R40}\n",
    "waiting_then_forgot": f"{STALE}✻ forgot 5s · nope\n\n{R40}\n❯ \n{R40}\n",
    "waiting_no_box": STALE + "⏺ still going\n",
    "agent_row_no_timer": screen(PM_TURN) + "  ◯ research  queued\n",
    "agent_row_ms_timer": screen(PM_TURN) + "  ◯ research  took 12ms\n",
    "agent_row_digit_name": screen(PM_TURN) + "  ◯ 5s\n",
    "agent_row_above_box": screen(PM_TURN + ["  ◯ research  Reading files  45s · ↓ 1.0k tokens"]),
    "agent_row_minutes": screen(PM_TURN) + "  ◯ x  3m 2s\n",
    "rc_failed": f"{RC_BASE} · /rc failed\n",
    "rc_ok": f"{RC_BASE} · /rc\n",
    "rc_failed_scrolled_off": f"{RC_BASE} · /rc failed\n" + "".join(f"row {i}\n" for i in range(12)),
    "account": f"{RC_BASE} · TM 1.7.12 ● | architect | @x | ✻a@x.com | Opus 5.5 | ctx 3%\n",
    "account_truncated": f"{RC_BASE} · TM 1 ● | p | ✻a@x.co…\n",
    "account_across_rows": f"{RC_BASE}\n  TM 1.7.12 ● | arch | ✻me@x.io\n| tail\n",
    "account_pipe_in_name": f"{RC_BASE} · | ✻a| |\n",
    # Typed text next to a dim span: input-state.py reads "suggestion"; the
    # Rust send guard must not (#9171 A2 critic round).
    "slash_command_dim_hint": screen(PM_TURN, box=f"/tm-fleet-check {DIM}[pr-number]{RESET}"),
    "typed_then_dim_tail": "\n".join(["⏺ done", RULE, "❯" + NBSP + f"my draft reply{DIM} …{RESET}…", RULE]) + "\n",
    "dim_row_then_typed_wrapped": "\n".join(PM_TURN + [RULE, "❯" + NBSP + f"{DIM}a dim start{RESET}",
                                                       f"typed {DIM}tail{RESET}", RULE]) + "\n",
    "empty_dim_pair_then_typed": "\n".join(["⏺ done", RULE, "❯" + NBSP + f"{DIM}{RESET}typed reply", RULE]) + "\n",
}

# --- scenarios ----------------------------------------------------------------

EVALUATE = [{"case": f"single:{name}", "steps": [[name, 10000.0]]} for name in CAPTURES] + [
    {"case": "idle_after_ten_minutes", "steps": [["idle_worked", 1000.0], ["idle_worked", 1599.0],
                                                 ["idle_worked", 1600.0]]},
    {"case": "suggestion_idle", "steps": [["idle_suggestion", 0.0], ["idle_suggestion", 600.0]]},
    {"case": "idle_resets_when_busy", "steps": [["box_empty", 0.0], ["busy_undulating", 300.0],
                                                ["box_empty", 700.0], ["box_empty", 1300.5]]},
    {"case": "errors_new_since_last_cycle", "steps": [["errors_old", 10000.0], ["errors_new", 10060.0],
                                                      ["errors_new", 10120.0]]},
    {"case": "labeled_rule_idle", "steps": [["labeled_rule_fixture", 1759000000.25],
                                            ["labeled_rule_fixture", 1759003600.75]]},
    {"case": "stale_waiting_goes_idle", "steps": [["stale_waiting_then_done", 0.0],
                                                  ["stale_waiting_then_done", 600.0]]},
    {"case": "newest_waiting_never_idle", "steps": [["newest_waiting", 0.0], ["newest_waiting", 600.0]]},
    {"case": "rows_without_bottom_rule_never_idle", "steps": [["rows_no_bottom_rule", 0.0],
                                                              ["rows_no_bottom_rule", 600.0]]},
]


def cand(kind, f="f1", session="tm-x", detail="d", pane="%1"):
    return [kind, f, detail, session, pane, 0.0]


DEDUP = [
    {"case": "suppress_until_cleared", "steps": [
        [[cand("dialog")], 0.0], [[cand("dialog")], 60.0], [[], 120.0],
        [[cand("dialog")], 180.0], [[cand("dialog", "f2")], 240.0]]},
    {"case": "reemit_idle_not_dialog", "steps": [
        [[cand("idle"), cand("dialog")], 0.0], [[cand("idle"), cand("dialog")], 1799.0],
        [[cand("idle"), cand("dialog")], 1800.0]]},
    {"case": "duplicates_in_one_cycle", "steps": [
        [[cand("dialog"), cand("dialog"), cand("ctx", "45"), cand("ctx", "45", "tm-y")], 5.0]]},
    {"case": "self_ctx_reemit", "steps": [
        [[cand("self_ctx", "x", "tm-architect", "self ctx 50%", "")], 0.0],
        [[cand("self_ctx", "x", "tm-architect", "self ctx 50%", "")], 1799.0],
        [[cand("self_ctx", "x", "tm-architect", "self ctx 50%", "")], 1800.0]]},
    {"case": "token_reemits_hourly", "steps": [
        [[cand("token", "t")], 0.0], [[cand("token", "t")], 3599.0], [[cand("token", "t")], 3600.0]]},
    {"case": "architect_kinds_reemit", "steps": [
        [[cand(k) for k in ("architect_dead", "architect_silent", "architect_frozen", "band")], 0.0],
        [[cand(k) for k in ("architect_dead", "architect_silent", "architect_frozen", "band")], 1800.0]]},
]

PLAN = [
    {"case": "five_hour_91", "panes": [["tm-a", "plan_91"], ["tm-b", "plan_88"]], "now": 0.0},
    {"case": "weekly_95", "panes": [["tm-a", "box_empty"], ["tm-z", "plan_week_95"]], "now": 1759000000.0},
    {"case": "below_threshold", "panes": [["tm-a", "plan_88"]], "now": 0.0},
    {"case": "no_panes", "panes": [], "now": 0.0},
]

CLEAN = [
    "export API_KEY=abc123 then run",
    "  many   spaces\tand\nnewlines  ",
    "password: hunter2 and PASSWD=x and Auth-Token = y",
    "sha " + "0123456789abcdef" * 3 + " done; nodigits" + "x" * 40,
    "secret_value=a" + "b" * 250,
    "Résumé — ✻ ⏳ 📅 naïve SECRET:é",
    "tokenless key",
    "monkey=banana keyboard:qwerty",
]

QUIET = [
    {"case": "one_per_line", "content": "# parked by the owner\ntm-hotstats-reporting-01\n\n"
                                        "tm-ai-power-rankings-01  # paused pending #5678\n   \n"},
    {"case": "missing", "content": None},
    {"case": "all_comments", "content": "# nothing listed yet\n\n"},
    {"case": "crlf_and_tabs", "content": "tm-a\r\n\ttm-b\t# x\r\ntm-c#d\rtm-a\n"},
]
FIXTURE_NAMES = ["tm-xtest-1", "tm-xtest-anything", "tm-qa1-run", "tm-qa-x", "tm-r0", "tm-r42-worker",
                 "tm-supervisor", "tm-hotstats-reporting-01", "tm-api", "tm-qa", "tm-r", "tm-rfoo",
                 "tm-xtest", "tm-qa-", "tm-r9", "TM-R1", "tm-xtest-", "xtm-r1"]


def usage_line(usage, msg_type="assistant", entrypoint="cli"):
    return json.dumps({"type": msg_type, "message": {"usage": usage}, "entrypoint": entrypoint})


def f(name, mtime, *lines):
    return {"name": name, "mtime": mtime, "lines": [list(l) for l in lines]}


FILLER = json.dumps({"type": "user", "message": {"content": "x" * 200}})
SELF_CTX = [
    {"case": "highest_mtime_cli", "files": [f("old.jsonl", 1000, (usage_line({"input_tokens": 1}), 1)),
                                           f("new.jsonl", 2000, (usage_line({"input_tokens": 2}), 1))]},
    {"case": "no_files", "files": []},
    {"case": "wrong_extension", "files": [f("not-a-transcript.txt", 1000, ("x", 1))]},
    {"case": "sdk_cli_newer", "files": [
        f("supervisor.jsonl", 1000, (usage_line({"input_tokens": 999000}), 1)),
        f("collector.jsonl", 9000, (usage_line({"input_tokens": 1}, entrypoint="sdk-cli"), 1))]},
    {"case": "limit_3", "limit": 3, "files": [f("cli.jsonl", 1000, (usage_line({"input_tokens": 1}), 1))] + [
        f(f"sdk-{i}.jsonl", 2000 + i, (usage_line({"input_tokens": 1}, entrypoint="sdk-cli"), 1))
        for i in range(5)]},
    {"case": "limit_10", "limit": 10, "files": [f("cli.jsonl", 1000, (usage_line({"input_tokens": 1}), 1))] + [
        f(f"sdk-{i}.jsonl", 2000 + i, (usage_line({"input_tokens": 1}, entrypoint="sdk-cli"), 1))
        for i in range(5)]},
    {"case": "skips_non_assistant", "usage_of": "t.jsonl", "files": [f(
        "t.jsonl", 1000, (usage_line({"input_tokens": 1}, "user"), 1),
        (usage_line({"input_tokens": 10, "cache_read_input_tokens": 20}), 1))]},
    {"case": "empty_file", "usage_of": "empty.jsonl", "files": [f("empty.jsonl", 1000)]},
    {"case": "grows_window", "usage_of": "big.jsonl", "start": 1024, "files": [f(
        "big.jsonl", 1000, (usage_line({"input_tokens": 900000, "cache_read_input_tokens": 0}), 1),
        (FILLER, 2000))]},
    {"case": "last_of_several", "usage_of": "t.jsonl", "files": [f(
        "t.jsonl", 1000, (usage_line({"input_tokens": 1}), 1), (usage_line({"input_tokens": 2}), 1),
        (usage_line({"input_tokens": 3}), 1))]},
    {"case": "sums_three", "files": [f("t.jsonl", 1000, (usage_line(
        {"input_tokens": 100, "cache_read_input_tokens": 200, "cache_creation_input_tokens": 300}), 1))]},
    {"case": "missing_fields_zero", "files": [f("t.jsonl", 1000, (usage_line({"input_tokens": 5}), 1))]},
    {"case": "no_assistant_usage", "files": [f("t.jsonl", 1000, (usage_line({"input_tokens": 1}, "user"), 1))]},
    {"case": "only_sdk_cli", "files": [f("c.jsonl", 1000, (usage_line({"input_tokens": 999000},
                                                                     entrypoint="sdk-cli"), 1))]},
    {"case": "malformed_and_empty_usage", "usage_of": "t.jsonl", "files": [f(
        "t.jsonl", 1000, (usage_line({"input_tokens": 7, "cache_creation_input_tokens": None}), 1),
        ("{not json", 1), (usage_line({}), 1), ("", 1))]},
    {"case": "newest_line_lacks_entrypoint", "files": [f(
        "t.jsonl", 1000, (usage_line({"input_tokens": 4}), 1),
        (json.dumps({"type": "summary", "entrypoint": ""}), 1))]},
]

# --- goldens ------------------------------------------------------------------


def write(name, rows):
    with open(os.path.join(HERE, name), "w", encoding="utf-8", newline="\n") as out:
        out.write("".join(r + "\n" for r in rows))


def materialize(directory, files):
    for spec in files:
        path = os.path.join(directory, spec["name"])
        with open(path, "w", encoding="utf-8", newline="\n") as out:
            out.write("".join((text + "\n") * count for text, count in spec["lines"]))
        os.utime(path, (spec["mtime"], spec["mtime"]))


def main():
    corpus = {"captures": [[n, c] for n, c in CAPTURES.items()], "evaluate": EVALUATE, "dedup": DEDUP, "plan": PLAN,
              "clean": CLEAN, "quiet": QUIET, "fixture_names": FIXTURE_NAMES, "self_ctx": SELF_CTX}
    with open(os.path.join(HERE, "corpus.json"), "w", encoding="ascii", newline="\n") as out:
        json.dump(corpus, out, indent=1)
        out.write("\n")
    infos = {name: fc.parse_pane(cap) for name, cap in CAPTURES.items()}
    write("input_state.jsonl", [json.dumps([n, list(ist.classify(c))]) for n, c in CAPTURES.items()])
    write("parse_pane.jsonl", [json.dumps([n, infos[n]]) for n in CAPTURES])
    rows = []
    for sc_ in EVALUATE:
        ps = {}
        for step, (name, now) in enumerate(sc_["steps"]):
            alerts = fc.evaluate(fc.parse_pane(CAPTURES[name]), ps, now, "tm-x", "%1")
            rows.append(json.dumps({"case": sc_["case"], "step": step, "alerts": alerts, "ps": ps},
                                   ensure_ascii=False))
    write("evaluate.jsonl", rows)
    rows = []
    for sc_ in DEDUP:
        state = {}
        for step, (cands, now) in enumerate(sc_["steps"]):
            alerts = [fc.alert(k, fp_, d, s, p, t) for k, fp_, d, s, p, t in cands]
            new, sup = fc.dedup(state, alerts, now)
            rows.append(json.dumps({"case": sc_["case"], "step": step, "new": new, "suppressed": sup,
                                    "state": state}))
    write("dedup.jsonl", rows)
    write("plan.jsonl", [json.dumps({"case": p["case"], "alerts": fc.fleet_plan_alerts(
        [(s, infos[c]) for s, c in p["panes"]], p["now"])}, ensure_ascii=False) for p in PLAN])
    write("clean.jsonl", [json.dumps([t, fc.clean(t), fc.fp(t)], ensure_ascii=False) for t in CLEAN])
    rows = []
    with tempfile.TemporaryDirectory() as tmp:
        for q in QUIET:
            path = os.path.join(tmp, q["case"] + ".txt")
            if q["content"] is not None:
                with open(path, "w", encoding="utf-8", newline="") as out:
                    out.write(q["content"])
            rows.append(json.dumps({"case": q["case"], "sessions": sorted(qs.read_quiet_sessions(path))}))
    rows += [json.dumps([n, qs.is_test_fixture_session(n)]) for n in FIXTURE_NAMES]
    write("quiet.jsonl", rows)
    rows = []
    for s in SELF_CTX:
        with tempfile.TemporaryDirectory() as tmp:
            materialize(tmp, s["files"])
            newest = sc.newest_transcript(tmp, limit=s.get("limit", sc.CANDIDATE_LIMIT))
            target = s.get("usage_of") or (newest and os.path.basename(newest))
            usage = sc.tail_last_assistant_usage(os.path.join(tmp, target), start=s.get(
                "start", sc.USAGE_TAIL_START)) if target else None
            rows.append(json.dumps({"case": s["case"], "newest": newest and os.path.basename(newest),
                                    "usage": usage, "tokens": sc.self_ctx_tokens(tmp)}, sort_keys=True))
    write("self_ctx.jsonl", rows)


if __name__ == "__main__":
    main()
