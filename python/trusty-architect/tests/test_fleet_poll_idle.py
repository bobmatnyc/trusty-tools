"""Idle detection and turn_active (#10579), ported from the prototype's test_fleet_poll_idle.py.

Covers: a current WAITING line keeps a pane from idling; a plain prompt still idles; WAITING lines in scrollback
that a finished turn follows are idle; the newest WAITING line is busy and a running turn; agent rows make a pane
busy but never a running turn. Loads fleet-poll.py, so the poller's re-export of parse_pane is what is tested.
Test: tests/test_fleet_poll_idle.py
"""
import importlib.util
import os

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
_spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(SCRIPTS, "fleet-poll.py"))
fp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fp)

R = "─" * 40
WITH_WAITING = f"⏺ x\n✻ Waiting for 3 background agents to finish\n\n{R}\n❯ \n  ⏵⏵ bypass\n"
WITHOUT = f"⏺ Done.\n\n{R}\n❯ \n  ⏵⏵ bypass permissions on (shift+tab to cycle)\n"
STALE = "".join("✻ Waiting for 1 background agent to finish\n\n" for _ in range(3))
STALE_THEN_DONE = f"{STALE}✻ Brewed for 42s · done 1:13 AM\n\n{R}\n❯ \n{R}\n  ⏵⏵ bypass\n"
NEWEST_WAITING = f"✻ Brewed for 42s · done 1:13 AM\n\n✻ Waiting for 1 background agent to finish\n\n{R}\n❯ \n{R}\n  ⏵⏵ bypass\n"
AGENT_ROWS = (f"⏺ Done.\n\n{R}\n❯ \n{R}\n  ⏵⏵ bypass permissions on\n"
              "  ◯ code-critic      Reading English bundle         45s · ↓ 124.7k tokens\n")


def _idle_alerts(capture, ps):
    info = fp.parse_pane(capture)
    return [a for a in fp.evaluate(info, ps, 10_000, "s", "p") if a["kind"] == "idle"]


def test_waiting_line_is_not_idle():
    ps = {"idle_since": 0}
    assert _idle_alerts(WITH_WAITING, ps) == []
    assert ps["idle_since"] is None


def test_no_agents_still_idle():
    ps = {"idle_since": 0}
    alerts = _idle_alerts(WITHOUT, ps)
    assert len(alerts) == 1 and "no agents running" in alerts[0]["detail"]
    assert fp.parse_pane(WITHOUT)["turn_active"] is False


def test_stale_waiting_lines_are_not_busy():
    info = fp.parse_pane(STALE_THEN_DONE)
    assert not info["busy"] and not info["turn_active"]


def test_newest_waiting_line_is_busy():
    info = fp.parse_pane(NEWEST_WAITING)
    assert info["busy"] and info["turn_active"]


def test_agent_rows_are_busy_but_never_a_running_turn():
    info = fp.parse_pane(AGENT_ROWS)
    assert info["agents"] == 1 and info["busy"]
    assert info["turn_active"] is False
