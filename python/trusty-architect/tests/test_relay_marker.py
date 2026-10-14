"""Tests for scripts/relay-marker.py: both relay-marker forms (supervisor and
Architect), both delivery forms (typed ❯ and SendMessage ›), the input-box
cut, the since filter and self-exclusion. Pure; no tmux involved."""
import importlib.util
import os
import subprocess
import sys

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
SCRIPT = os.path.join(SCRIPTS, "relay-marker.py")

spec = importlib.util.spec_from_file_location("relay_marker", SCRIPT)
rm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rm)

RULE = "─" * 40

PANE = [
    "❯ [Global supervisor 13:10Z] User ruling (the owner): take option A",
    "⏺ Acknowledged.",
    "› Message from @9-28-26: [Owner Architect 13:20Z] Architect answer: merge on green",
    "⏺ Done.",
    "› Message from @9-28-26: [CTO supervisor 13:2xZ] User ruling (the owner): hold",
    "⏺ Holding.",
    "❯ [supervisor 13:40Z] Supervisor question: which branch?",
    "⏺ main.",
    RULE,
    "❯ [Owner Architect 13:50Z] typed in the box, not submitted",
    RULE,
]


@pytest.mark.parametrize("line,name,role,hm", [
    ("❯ [Global supervisor 13:18Z] x", "Global", "supervisor", "13:18"),
    ("❯ [Owner Architect 13:18Z] x", "Owner", "Architect", "13:18"),
    ("› Message from @9-28-26: [Owner supervisor 22:2xZ] x", "Owner", "supervisor", "22:2x"),
    ("❯ [Architect 09:05Z] x", None, "Architect", "09:05"),
    ("❯ [Owner Architect 09:05Z] x", "Owner", "Architect", "09:05"),
])
def test_marker_accepts_both_forms_and_both_deliveries(line, name, role, hm):
    m = rm.relay_marker(line)
    assert m is not None
    assert (m.group("name"), m.group("role"), m.group("hm")) == (name, role, hm)


@pytest.mark.parametrize("line", [
    "❯ [Owner supervisor] load is under 16",       # no time: a dim suggestion seen 2026-09-28
    "[poll 13:18Z] 2 new alerts -> inbox/alerts.jsonl",
    "⏺ [Owner Architect 13:18Z] quoted by the PM, not a submitted turn",
    "❯ please ask the Architect 13:18Z",
])
def test_marker_rejects_non_relays(line):
    assert rm.relay_marker(line) is None


def test_find_relays_stops_at_input_box_and_applies_since():
    hits = rm.find_relays(PANE, "13:15Z")
    assert [h.split("]")[0].split("[")[1] for h in hits] == [
        "Owner Architect 13:20Z", "CTO supervisor 13:2xZ", "supervisor 13:40Z"]


def test_find_relays_excludes_own_name_in_either_form():
    hits = rm.find_relays(PANE, "13:00Z", "Owner")
    assert all("[Owner " not in h for h in hits)
    assert len(hits) == 3  # Global, CTO and the nameless marker still count
    hits = rm.find_relays(PANE, "13:00Z", "Global")
    assert all("[Global " not in h for h in hits)
    assert any("[Owner Architect" in h for h in hits)


@pytest.mark.parametrize("since,self_name,code,first", [
    ("13:00Z", "", 1, "❯ [Global supervisor 13:10Z]"),
    ("13:45Z", "", 0, "NONE"),
])
def test_cli_exit_codes(since, self_name, code, first):
    run = subprocess.run([sys.executable, SCRIPT, since, self_name],
                         input="\n".join(PANE), capture_output=True, text=True)
    assert run.returncode == code
    assert run.stdout.startswith(first)
