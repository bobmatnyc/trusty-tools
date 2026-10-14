"""Tests for scripts/question-collector.py: capture cleanup, extract retry, log sizes.

subprocess.run is replaced by fakes, so no tmux server or claude CLI runs, and
LOG points at a temp file so the real inbox/collector.log is untouched.
"""
import contextlib
import importlib.util
import json
import os
import subprocess

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
spec = importlib.util.spec_from_file_location("question_collector",
                                              os.path.join(SCRIPTS, "question-collector.py"))
qc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qc)

EMPTY = '{"resolved":[],"questions":[]}'
PANE = "\n".join(f"line {i} ⏺ status text" for i in range(1, 121))  # 120 lines
TAIL = "\n".join(PANE.splitlines()[-60:])


@pytest.fixture
def logfile(tmp_path, monkeypatch):
    path = tmp_path / "collector.log"
    monkeypatch.setattr(qc, "LOG", str(path))
    monkeypatch.setattr(qc, "ledger", lambda session: "(none)")
    return path


def log_lines(path):
    return path.read_text().splitlines() if path.exists() else []


class FakeRun:
    """Stands in for subprocess.run: each call pops the next outcome, either an
    exception to raise or a (returncode, stdout) pair to return."""

    def __init__(self, *outcomes):
        self.outcomes = list(outcomes)
        self.calls = []

    def __call__(self, args, **kw):
        self.calls.append(kw)
        out = self.outcomes.pop(0)
        if isinstance(out, BaseException):
            raise out
        return subprocess.CompletedProcess(args, out[0], stdout=out[1], stderr="")


def test_capture_strips_trailing_whitespace(monkeypatch):
    raw = [f"row {i} text".ljust(200) for i in range(10)]
    fake = FakeRun((0, "\n".join(raw) + "\n"))
    monkeypatch.setattr(qc.subprocess, "run", fake)
    lines = qc.capture("tm-x").splitlines()
    assert lines == [f"row {i} text" for i in range(10)], lines


def test_no_retry_on_success(logfile, monkeypatch):
    fake = FakeRun((0, EMPTY))
    monkeypatch.setattr(qc.subprocess, "run", fake)
    res = qc.extract("tm-x", PANE)
    assert res == ([], [], f"bytes={len(PANE.encode())} lines=120")
    assert [c["timeout"] for c in fake.calls] == [120]
    assert log_lines(logfile) == []


def test_retry_once_on_timeout(logfile, monkeypatch):
    fake = FakeRun(subprocess.TimeoutExpired("claude", 120), (0, EMPTY))
    monkeypatch.setattr(qc.subprocess, "run", fake)
    res = qc.extract("tm-x", PANE)
    assert [c["timeout"] for c in fake.calls] == [120, 180]
    assert fake.calls[0]["input"].endswith(PANE + "\n")
    assert fake.calls[1]["input"].endswith("\nPANE:\n" + TAIL + "\n")
    assert res == ([], [], f"bytes={len(TAIL.encode())} lines=60 attempt=2")
    (line,) = log_lines(logfile)
    assert line.endswith(f"tm-x extract TIMEOUT attempt=1 bytes={len(PANE.encode())} lines=120")


def test_retry_only_once_when_both_time_out(logfile, monkeypatch):
    fake = FakeRun(subprocess.TimeoutExpired("claude", 120),
                   subprocess.TimeoutExpired("claude", 180))
    monkeypatch.setattr(qc.subprocess, "run", fake)
    assert qc.extract("tm-x", PANE) is None
    assert len(fake.calls) == 2
    lines = log_lines(logfile)
    assert [l.split(" ", 1)[1] for l in lines] == [
        f"tm-x extract TIMEOUT attempt=1 bytes={len(PANE.encode())} lines=120",
        f"tm-x extract TIMEOUT attempt=2 bytes={len(TAIL.encode())} lines=60",
    ]


@pytest.mark.parametrize("stdout,tag", [("no json here", "NOJSON"), ("{not json}", "BADJSON")])
def test_failure_log_lines_carry_size(logfile, monkeypatch, stdout, tag):
    monkeypatch.setattr(qc.subprocess, "run", FakeRun((0, stdout)))
    assert qc.extract("tm-x", PANE) is None
    (line,) = log_lines(logfile)
    assert f"tm-x extract {tag} " in line
    assert f" bytes={len(PANE.encode())} lines=120" in line


def test_success_log_line_carries_size(logfile, monkeypatch):
    q = {"question": "Will you merge PR #1?", "options": [], "recommendation": "",
         "urgent": False, "dialog": False}
    monkeypatch.setattr(qc, "capture", lambda session: PANE)
    monkeypatch.setattr(qc.qe, "fold", lambda: {})
    monkeypatch.setattr(qc.qe, "lock", contextlib.nullcontext)
    appended = []
    monkeypatch.setattr(qc.qe, "append", lambda event, **kw: appended.append(event))
    monkeypatch.setattr(qc.subprocess, "run",
                        FakeRun((0, json.dumps({"resolved": [], "questions": [q]}))))
    qc.process("tm-x", {"hashes": {}}, "idle_prompt")
    lines = [l.split(" ", 1)[1] for l in log_lines(logfile)]
    assert appended == ["question.opened"]
    assert lines[0].startswith("NEW ") and lines[0].endswith("[tm-x] Will you merge PR #1?")
    assert lines[1] == (f"tm-x idle_prompt questions=1 new=1 retired=0 "
                        f"bytes={len(PANE.encode())} lines=120")


def test_collector_is_retired_and_refuses_to_run():
    r = subprocess.run(["python3", os.path.join(SCRIPTS, "question-collector.py"), "--once"],
                       capture_output=True, text=True)
    assert r.returncode == 2
    assert "retired 2026-10-04 (f4)" in r.stderr
