"""Idle detection: a pane with running background agents is never idle."""
import importlib.util
import os

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(HERE, "fleet-poll.py"))
fp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fp)

R = "─" * 40
ROWS = (
    "  ◯ version-control  Polling smoke-prod gate        6m 34s · ↓ 52.9k tokens\n"
    "  ◯ code-critic      Reading English bundle         45s · ↓ 124.7k tokens\n"
)
# Box with no bottom rule: the footer slice is empty, so only a whole-capture scan sees the rows.
WITH_ROWS = f"⏺ Done.\n\n{R}\n❯ \n  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 2 agents\n  ⏺ main\n{ROWS}"
WITH_WAITING = f"⏺ x\n✻ Waiting for 3 background agents to finish\n\n{R}\n❯ \n  ⏵⏵ bypass\n"
WITHOUT = f"⏺ Done.\n\n{R}\n❯ \n  ⏵⏵ bypass permissions on (shift+tab to cycle)\n"


def _idle_alerts(capture, ps):
    info = fp.parse_pane(capture)
    return [a for a in fp.evaluate(info, ps, 10_000, "s", "p") if a["kind"] == "idle"]


def test_agent_rows_are_not_idle_and_reset_timer():
    ps = {"idle_since": 0}
    assert _idle_alerts(WITH_ROWS, ps) == []
    assert ps["idle_since"] is None


def test_waiting_line_is_not_idle():
    ps = {"idle_since": 0}
    assert _idle_alerts(WITH_WAITING, ps) == []
    assert ps["idle_since"] is None


def test_no_agents_still_idle():
    ps = {"idle_since": 0}
    alerts = _idle_alerts(WITHOUT, ps)
    assert len(alerts) == 1 and "no agents running" in alerts[0]["detail"]


STALE = "".join("✻ Waiting for 1 background agent to finish\n\n" for _ in range(3))
STALE_THEN_DONE = f"{STALE}✻ Brewed for 42s · done 1:13 AM\n\n{R}\n❯ \n{R}\n  ⏵⏵ bypass\n"
NEWEST_WAITING = f"✻ Brewed for 42s · done 1:13 AM\n\n✻ Waiting for 1 background agent to finish\n\n{R}\n❯ \n{R}\n  ⏵⏵ bypass\n"


def test_stale_waiting_lines_are_not_busy():
    info = fp.parse_pane(STALE_THEN_DONE)
    assert not info["busy"] and not info["turn_active"]


def test_newest_waiting_line_is_busy():
    info = fp.parse_pane(NEWEST_WAITING)
    assert info["busy"] and info["turn_active"]
