"""session-relaunch.sh --il / --il-session against a fake tmux and a no-op sleep (ar-T13)."""
import json
import os
import re
import stat
import subprocess

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10571 S2: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
SCRIPT = os.path.join(SCRIPTS, "session-relaunch.sh")

# Fake pane: claude -> (/exit) -> zsh -> (tm) -> claude. Every submitted line is logged.
FAKE = r'''#!/usr/bin/env python3
import json, sys
SF = __SF__
st = json.load(open(SF))
a = sys.argv[1:]
if a[:1] == ["-S"]:
    a = a[2:]
if a[0] == "display":
    print(st["cmd"])
elif a[0] == "capture-pane":
    print("\n".join(["done."] + ["-" * 20, "❯ " + st["input"], "-" * 20, "  /rc"]))
elif a[0] == "send-keys":
    k = a[3:]
    if k[0] == "-l":
        st["input"] += k[1]; st.setdefault("typed", []).append(k[1])
    elif k[0] == "C-u":
        st["input"] = ""
    elif k[0] == "Enter":
        line = st["input"]; st["sent"].append(line); st["input"] = ""
        if line == "/exit":
            st["cmd"] = "zsh"
        elif line == "tm" and st["cmd"] == "zsh":
            st["cmd"] = "claude"
json.dump(st, open(SF, "w"))
'''


def run(tmp_path, args, il_ok=True, dbatg=(), datg_sessions=(), script=SCRIPT):
    b = tmp_path / "bin"
    b.mkdir()
    sf = tmp_path / "t.json"
    sf.write_text(json.dumps({"cmd": "claude", "input": "", "sent": []}))
    for n, body in (("tmux", FAKE.replace("__SF__", repr(str(sf)))), ("sleep", "#!/bin/sh\nexit 0\n")):
        (b / n).write_text(body)
        (b / n).chmod(0o755 | stat.S_IEXEC)
    il = tmp_path / "proj"
    if dbatg:
        for u, files in dbatg:
            d = il / ".trusty-mpm" / "dbatg" / u
            d.mkdir(parents=True)
            for f in files:
                (d / f).write_text("{}")
    elif il_ok:
        (il / ".trusty-mpm" / "datg").mkdir(parents=True)
        (il / ".trusty-mpm" / "datg" / "state.json").write_text("{}")
    else:
        il.mkdir()
    for name in datg_sessions:
        d = il / ".trusty-mpm" / "datg" / name
        d.mkdir(parents=True)
        (d / "state.json").write_text("{}")
    args = [a.replace("IL", str(il)) for a in args]
    # #10571 S2: the marker name comes from the config; "Ann" stands in for local/architect.json owner_name.
    env = dict(os.environ, PATH=f"{b}:{os.environ['PATH']}", RELAUNCH_BRIEF_DIR=str(tmp_path / "briefs"),
               ARCHITECT_OWNER_NAME="Ann")
    env.pop("TMUX_SOCKET", None)
    log = tmp_path / "relaunch.log"
    r = subprocess.run([script, *args, "tm-x-01", str(log)], env=env, capture_output=True, text=True, timeout=60)
    return r, json.loads(sf.read_text()), log.read_text() if log.exists() else ""


def sent_text(st):
    """The resume text the PM received: the last submitted line after the relay marker."""
    last = st["sent"][-1]
    assert re.match(r"^\[Ann Architect \d\d:\d\dZ\] Architect answer: ", last), last
    return last.split("] Architect answer: ", 1)[1]


def test_syntax():
    assert subprocess.run(["bash", "-n", SCRIPT]).returncode == 0


def test_il_missing_ledger_exits_before_exit_and_sends_nothing(tmp_path):
    r, st, log = run(tmp_path, ["--il", "IL"], il_ok=False)
    assert r.returncode == 1 and "IL-MISSING" in log
    assert st["sent"] == [] and st["input"] == ""


@pytest.mark.parametrize("args", [["--stop-tasks", "--il", "IL"], ["--il", "IL", "--stop-tasks"], ["--il", "IL"]])
def test_flag_order_parses_and_session_is_last_arg(tmp_path, args):
    r, st, log = run(tmp_path, args)
    assert r.returncode == 0, r.stdout + r.stderr + log
    assert st["sent"][:2] == ["/exit", "tm"] and "tm-x-01 sent IL resume" in log


def test_without_il_last_command_is_session_resume(tmp_path):
    r, st, log = run(tmp_path, ["--stop-tasks"])
    assert r.returncode == 0, r.stdout + r.stderr + log
    assert st["sent"][-1] == "/tm-session-resume"


def test_with_il_sends_resume_text_and_never_session_resume(tmp_path):
    r, st, log = run(tmp_path, ["--il", "IL"])
    assert r.returncode == 0, r.stdout + r.stderr + log
    last = st["sent"][-1]
    assert last.startswith("[Ann Architect ") and "Z] Architect answer: You were relaunched" in last
    assert not any(c.startswith("/tm-session-resume") for c in st["sent"])
    assert [p.name for p in (tmp_path / "briefs").iterdir()] == ["relaunch-resume.md"]
    body = sent_text(st)
    assert f"datg.py --project {tmp_path / 'proj'} digest" in body and "do not run /tm-session-resume" in body


def test_dbatg_layout_brief_uses_dbatg_tool_and_cron_prompts(tmp_path):
    r, st, log = run(tmp_path, ["--il", "IL"], dbatg=[("u1", ["state.json", "log.jsonl", "cron-prompts.md"])])
    assert r.returncode == 0, r.stdout + r.stderr + log
    assert st["sent"][:2] == ["/exit", "tm"] and "sent IL resume" in log
    assert not any(c.startswith("/tm-session-resume") for c in st["sent"])
    body = sent_text(st)
    ledger = tmp_path / "proj" / ".trusty-mpm" / "dbatg" / "u1"
    assert "dbatg MCP tool" in body and "datg.py --project" not in body
    assert "do not run /tm-session-resume" in body and "session_context_catchup" in body
    assert f"{ledger}/cron-prompts.md" in body and "re-create each cron" in body and "continue the work in next" in body


def test_dbatg_layout_without_cron_prompts_file_omits_path(tmp_path):
    r, st, log = run(tmp_path, ["--il", "IL"], dbatg=[("u1", ["state.json"])])
    assert r.returncode == 0, r.stdout + r.stderr + log
    assert "cron-prompts.md" not in sent_text(st)


def test_dbatg_ambiguous_refuses_before_exit(tmp_path):
    r, st, log = run(tmp_path, ["--il", "IL"], dbatg=[("u1", ["state.json"]), ("u2", ["state.json"])])
    assert r.returncode == 1 and "IL-AMBIGUOUS" in log
    assert st["sent"] == [] and not (tmp_path / "briefs").exists()


def test_dbatg_dir_without_state_json_is_missing(tmp_path):
    r, st, log = run(tmp_path, ["--il", "IL"], dbatg=[("u1", ["log.jsonl"])])
    assert r.returncode == 1 and "IL-MISSING" in log and st["sent"] == []


def test_il_session_datg_found_brief_names_session_digest(tmp_path):
    # Project-level datg ledger also present: the per-session one must win.
    r, st, log = run(tmp_path, ["--il", "IL", "--il-session", "tm-peer"], datg_sessions=["tm-peer"])
    assert r.returncode == 0, r.stdout + r.stderr + log
    assert st["sent"][:2] == ["/exit", "tm"] and "sent IL resume" in log
    body = sent_text(st)
    assert f"datg.py --project {tmp_path / 'proj'} --session tm-peer digest" in body
    assert "dbatg MCP tool" not in body and "do not run /tm-session-resume" in body


def test_il_session_dbatg_found_brief_names_that_folder(tmp_path):
    # Two dbatg ledgers: no IL-AMBIGUOUS, the named one is used.
    r, st, log = run(tmp_path, ["--il-session", "tm-peer", "--il", "IL"],
                     dbatg=[("tm-peer", ["state.json", "cron-prompts.md"]), ("other", ["state.json"])])
    assert r.returncode == 0, r.stdout + r.stderr + log
    assert st["sent"][:2] == ["/exit", "tm"] and "IL-AMBIGUOUS" not in log
    body = sent_text(st)
    ledger = tmp_path / "proj" / ".trusty-mpm" / "dbatg" / "tm-peer"
    assert f"ledger folder {ledger})" in body and "datg.py --project" not in body
    assert f"{ledger}/cron-prompts.md" in body and "/other" not in body


def test_il_session_datg_wins_over_dbatg_of_same_name(tmp_path):
    r, st, log = run(tmp_path, ["--il", "IL", "--il-session", "tm-peer"],
                     il_ok=False, dbatg=[("tm-peer", ["state.json"])], datg_sessions=["tm-peer"])
    assert r.returncode == 0, r.stdout + r.stderr + log
    assert "--session tm-peer digest" in sent_text(st)


def test_il_session_missing_refuses_before_exit(tmp_path):
    # A project-level ledger and another session's ledger exist, but not the named one.
    r, st, log = run(tmp_path, ["--il", "IL", "--il-session", "tm-peer"], datg_sessions=["tm-other"])
    assert r.returncode == 1 and "IL-MISSING" in log and "tm-peer" in log
    assert st["sent"] == [] and st["input"] == "" and not (tmp_path / "briefs").exists()


def test_il_session_without_il_is_usage_error(tmp_path):
    r, st, log = run(tmp_path, ["--il-session", "tm-peer"])
    assert r.returncode == 2 and st["sent"] == []


def test_il_session_name_with_slash_is_usage_error(tmp_path):
    r, st, log = run(tmp_path, ["--il", "IL", "--il-session", "../x"])
    assert r.returncode == 2 and st["sent"] == []


def test_no_flag_ignores_per_session_ledgers_and_brief_has_no_session_arg(tmp_path):
    r, st, log = run(tmp_path, ["--il", "IL"], datg_sessions=["tm-peer"])
    assert r.returncode == 0, r.stdout + r.stderr + log
    body = sent_text(st)
    assert f"datg.py --project {tmp_path / 'proj'} digest" in body and "--session" not in body
