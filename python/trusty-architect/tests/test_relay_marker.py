"""Tests for scripts/relay-marker.py (#9997): the Architect marker, the
legacy marker form older Architects typed, both delivery forms (typed ❯ and
SendMessage ›), the input-box cut, the since filter, self-exclusion and the
refusal of a malformed `since`. Pure; no tmux involved."""
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
LEGACY = "supervisor"  # the marker role the shipped manual named before #9997

PANE = [
    f"❯ [Ops {LEGACY} 13:10Z] User ruling (Ada): take option A",
    "⏺ Acknowledged.",
    "› Message from @9-28-26: [Ada Architect 13:20Z] Architect answer: merge on green",
    "⏺ Done.",
    f"› Message from @9-28-26: [Lin {LEGACY} 13:2xZ] User ruling (Ada): hold",
    "⏺ Holding.",
    "❯ [Architect 13:40Z] Architect question: which branch?",
    "⏺ main.",
    RULE,
    "❯ [Ada Architect 13:50Z] typed in the box, not submitted",
    RULE,
]


@pytest.mark.parametrize("line,name,role,hm", [
    ("❯ [Ops Architect 13:18Z] x", "Ops", "Architect", "13:18"),
    ("› Message from @9-28-26: [Ada Architect 22:2xZ] x", "Ada", "Architect", "22:2x"),
    ("❯ [Architect 09:05Z] x", None, "Architect", "09:05"),
    (f"❯ [Ops {LEGACY} 13:18Z] x", "Ops", LEGACY, "13:18"),
])
def test_marker_accepts_the_architect_and_the_legacy_form_in_both_deliveries(line, name, role, hm):
    m = rm.relay_marker(line)
    assert m is not None
    assert (m.group("name"), m.group("role"), m.group("hm")) == (name, role, hm)


@pytest.mark.parametrize("line", [
    "❯ [Ada Architect] load is under 16",       # no time: a dim suggestion
    "[poll 13:18Z] 2 new alerts -> inbox/alerts.jsonl",
    "⏺ [Ada Architect 13:18Z] quoted by the PM, not a submitted turn",
    "❯ please ask the Architect 13:18Z",
])
def test_marker_rejects_non_relays(line):
    assert rm.relay_marker(line) is None


def test_find_relays_stops_at_input_box_and_applies_since():
    hits = rm.find_relays(PANE, "13:15Z")
    assert [h.split("]")[0].split("[")[1] for h in hits] == [
        "Ada Architect 13:20Z", f"Lin {LEGACY} 13:2xZ", "Architect 13:40Z"]


def test_find_relays_excludes_own_name_in_either_form():
    hits = rm.find_relays(PANE, "13:00Z", "Ada")
    assert all("[Ada " not in h for h in hits)
    assert len(hits) == 3  # Ops, Lin and the nameless marker still count
    hits = rm.find_relays(PANE, "13:00Z", "Ops")
    assert all("[Ops " not in h for h in hits)
    assert any("[Ada Architect" in h for h in hits)


@pytest.mark.parametrize("since,self_name,code,first", [
    ("13:00Z", "", 1, f"❯ [Ops {LEGACY} 13:10Z]"),
    ("13:45Z", "", 0, "NONE"),
])
def test_cli_exit_codes(since, self_name, code, first):
    run = subprocess.run([sys.executable, SCRIPT, since, self_name],
                         input="\n".join(PANE), capture_output=True, text=True)
    assert run.returncode == code
    assert run.stdout.startswith(first)


@pytest.mark.parametrize("since", ["1:5", "13:5", "now", "", "25:00Z", "13:60Z"])
def test_a_malformed_since_is_refused_never_none(since):
    """A since that cannot be compared would print NONE and let a double post
    through: it must exit 2 and print no NONE."""
    run = subprocess.run([sys.executable, SCRIPT, since],
                         input="\n".join(PANE), capture_output=True, text=True)
    assert run.returncode == 2, run
    assert "NONE" not in run.stdout
