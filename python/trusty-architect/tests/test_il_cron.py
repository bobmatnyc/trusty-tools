"""il-cron.py: overdue jobs run at the next cycle, failures retry 3 times then alert, once-jobs run once."""
import importlib.util
import json
import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
_spec = importlib.util.spec_from_file_location("il_cron", os.path.join(SCRIPTS, "il-cron.py"))
ic = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ic)

DAY = ic.parse_iso("2026-10-17T00:00:00Z")
FAKE = """import os, sys
open(os.environ["FAKE_LOG"], "a").write(" ".join(sys.argv[1:]) + "\\n")
sys.exit(1 if os.path.exists(os.environ["FAKE_LOG"] + ".fail") else 0)
"""


@pytest.fixture
def env(tmp_path, monkeypatch):
    fake = tmp_path / "fake.py"
    fake.write_text(FAKE)
    log = tmp_path / "calls.log"
    monkeypatch.setenv("FAKE_LOG", str(log))
    monkeypatch.setenv("DATG_CMD", f"{sys.executable} {fake}")
    monkeypatch.setenv("IL_ARCHIVE_CMD", f"{sys.executable} {fake}")
    monkeypatch.setenv("IL_CRON_STATE", str(tmp_path / "state.json"))
    monkeypatch.setenv("IL_BRIEFS_DIR", str(tmp_path / "briefs"))
    proj = tmp_path / "proj"
    d = proj / ".trusty-mpm" / "datg"
    d.mkdir(parents=True)
    (d / "state.json").write_text("{}")
    (d / "log.jsonl").write_text('{"t":"2026-10-16T01:00Z","id":"x-D01","ev":"escalated"}\n'
                                 '{"ts":"2026-10-16T02:00Z","session":"s","kind":"drift"}\n')
    sched = {"sessions": [{"name": "tm-x", "project": str(proj)}], "jobs": []}
    return sched, log, tmp_path, d


def calls(log):
    return log.read_text().splitlines() if log.exists() else []


def test_overdue_job_runs_next_cycle_then_waits_for_its_period(env):
    sched, log, _, _ = env
    sched["jobs"] = [{"name": "rotate", "due": "2026-10-16", "every": "1h", "command": "builtin:rotate"}]
    st = {}
    assert ic.cycle(sched, st, DAY) == []
    assert len(calls(log)) == 1 and "rotate" in calls(log)[0]
    ic.cycle(sched, st, DAY + 600)
    assert len(calls(log)) == 1  # not due again within the hour
    ic.cycle(sched, st, DAY + 3600)
    assert len(calls(log)) == 2


def test_job_not_yet_due_does_not_run(env):
    sched, log, _, _ = env
    sched["jobs"] = [{"name": "rotate", "due": "2026-10-18", "command": "builtin:rotate"}]
    ic.cycle(sched, {}, DAY)
    assert calls(log) == []


def test_failing_job_retries_three_times_then_alerts_once_and_stops(env):
    sched, log, tmp, _ = env
    (tmp / "calls.log.fail").write_text("")
    sched["jobs"] = [{"name": "rotate", "due": "2026-10-16", "every": "1h", "command": "builtin:rotate"}]
    st, alerts = {}, []
    for i in range(3):
        alerts += ic.cycle(sched, st, DAY + i * 3600)
    assert len(calls(log)) == 3
    assert len(alerts) == 1 and "failed 3 times" in alerts[0][1]
    assert ic.cycle(sched, st, DAY + 2 * 3600 + 600) == []
    assert len(calls(log)) == 3  # given up until the next regular period


def test_pilot_rescore_runs_once_and_writes_its_brief(env):
    sched, log, tmp, _ = env
    sched["jobs"] = [{"name": "pilot-rescore", "due": "2026-10-17", "command": "builtin:pilot-rescore"}]
    st = {}
    alerts = ic.cycle(sched, st, DAY + 60)
    brief = tmp / "briefs" / "2026-10-17-t08-pilot-rescore.md"
    body = brief.read_text()
    assert "| tm-x | 1 | 1 |" in body  # both log shapes counted
    assert alerts and str(brief) in alerts[0][1]
    brief.unlink()
    assert ic.cycle(sched, st, DAY + 7200) == [] and not brief.exists()  # once only


def test_size_check_alerts_over_8kb(env):
    # #10571: the bar is datg's own warn bar (caps.datg_size_warn, 32 KB by default since D74)
    sched, _, _, d = env
    size = ic.SIZE_WARN + 1000
    (d / "state.json").write_text("x" * size)
    sched["jobs"] = [{"name": "size-check", "due": "2026-10-16", "every": "1h", "command": "builtin:size-check"}]
    alerts = ic.cycle(sched, {}, DAY)
    assert alerts and f"tm-x {size}B" in alerts[0][1]
    assert f"over {ic.SIZE_WARN // 1024} KB" in alerts[0][1]


def test_step_runs_at_most_once_per_hour_and_persists_state(env):
    sched, log, tmp, _ = env
    sched["jobs"] = [{"name": "rotate", "due": "2026-10-16", "every": "10m", "command": "builtin:rotate"}]
    ic.step(now=DAY, sched=sched)
    ic.step(now=DAY + 1200, sched=sched)  # inside the hourly gate
    assert len(calls(log)) == 1
    ic.step(now=DAY + 3600, sched=sched)
    assert len(calls(log)) == 2
    assert json.loads((tmp / "state.json").read_text())["jobs"]["rotate"]["last_result"].startswith("ok")


def test_dry_run_runs_nothing_and_writes_no_state(env, capsys):
    sched, log, tmp, _ = env
    sched["jobs"] = [{"name": "rotate", "due": "2026-10-16", "command": "builtin:rotate"}]
    ic.step(now=DAY, dry_run=True, sched=sched)
    assert calls(log) == [] and not (tmp / "state.json").exists()
    assert "would run rotate" in capsys.readouterr().out


@pytest.mark.parametrize("job", ["prune", "rotate", "size-check"])
def test_uninitialized_project_is_skipped_not_failed(env, job, tmp_path, monkeypatch):
    sched, log, _, _ = env
    new = tmp_path / "noil"
    new.mkdir()   # no .trusty-mpm/datg/state.json: B11 has not set this project up
    bad = tmp_path / "datg_fails.py"
    bad.write_text("import sys; sys.stderr.write('datg: state.json missing; run init'); sys.exit(1)")
    monkeypatch.setenv("DATG_CMD", f"{sys.executable} {bad}")   # any datg call on it would fail the job
    sched["sessions"] = [{"name": "tm-new", "project": str(new)}]
    sched["jobs"] = [{"name": job, "due": "2026-10-16", "every": "1h", "command": f"builtin:{job}"}]
    st = {}
    assert ic.cycle(sched, st, DAY) == []
    assert st["jobs"][job]["last_result"].startswith("ok")
    if job != "size-check":
        assert "skipped tm-new: not initialized" in st["jobs"][job]["last_result"]
