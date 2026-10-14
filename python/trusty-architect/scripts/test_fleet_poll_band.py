"""Builder band and load alert follow the 15-minute load, not the 5-minute one."""
import importlib.util
import os
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(HERE, "fleet-poll.py"))
fp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fp)


def _host(loads, state):
    du = mock.Mock(used=1, total=100, free=500 * 2**30)
    with mock.patch.object(fp.os, "getloadavg", return_value=loads), \
            mock.patch.object(fp.os, "cpu_count", return_value=16), \
            mock.patch.object(fp.shutil, "disk_usage", return_value=du), \
            mock.patch.object(fp, "swap_used_mb", return_value=0), \
            mock.patch.object(fp, "mem_free_pct", return_value=60.0):
        return fp.host_alerts(0.0, state)


def test_five_minute_spike_with_calm_15m_keeps_hold():
    state = {"band": "hold"}
    out = _host((30.0, 31.0, 20.0), state)
    assert state["band"] == "hold"
    assert [a for a in out if a["kind"] in ("band", "load")] == []


def test_high_15m_load_stops_and_names_load15():
    state = {"band": "hold"}
    out = _host((30.0, 31.0, 31.0), state)
    band = [a for a in out if a["kind"] == "band"]
    assert state["band"] == "stop" and "load15" in band[0]["detail"]
