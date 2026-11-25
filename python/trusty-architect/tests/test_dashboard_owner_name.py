"""/api/user owner name after `tm fleet init` seeds meta/state before local/architect.json exists (#10760).

Covers: a seeded "the owner" (and a stored "") is unset, so an architect.json owner_name written after the
server started is served with no restart; an owner name stored explicitly in meta/state still wins.
"""
import importlib.util
import json
import os
import sys
import threading
import urllib.request

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")


def _serve_module():
    if SCRIPTS not in sys.path:
        sys.path.insert(0, SCRIPTS)
    spec = importlib.util.spec_from_file_location("dashboard_serve_owner", os.path.join(SCRIPTS, "dashboard-serve.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


@pytest.fixture
def started(tmp_path, monkeypatch):
    """A server started the way `tm fleet init` starts it: no architect.json yet, so no configured owner."""
    cfg = tmp_path / "architect.json"
    monkeypatch.setenv("ARCHITECT_CONFIG", str(cfg))   # absent until a test writes it
    monkeypatch.delenv("ARCHITECT_OWNER_NAME", raising=False)
    mod = _serve_module()
    page = tmp_path / "index.html"
    page.write_text("<html><head></head><body>x</body></html>")
    store = mod.ds.Store(str(tmp_path / "store"))
    srv = mod.DashboardServer(0, store, str(page), owner_name="")
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield store, cfg, srv.server_address[1]
    srv.shutdown()
    srv.server_close()


def _user(port):
    req = urllib.request.Request(f"http://127.0.0.1:{port}/api/user", headers={"Host": f"127.0.0.1:{port}"})
    with urllib.request.urlopen(req, timeout=5) as r:
        return json.loads(r.read())["name"]


@pytest.mark.parametrize("seeded", ["the owner", ""])
def test_seeded_placeholder_yields_to_a_later_architect_json_owner(started, seeded):
    store, cfg, port = started
    if seeded == "the owner":
        assert store.seed_meta()   # the fleet-init seed with no configured owner
    else:
        store.set("meta", "state", {"owner_name": "", "updated": "2026-11-25T00:00:00Z"}, if_version=0)
    assert store.get("meta", "state")["data"]["owner_name"] == seeded
    assert _user(port) == "the owner"
    cfg.write_text(json.dumps({"owner_name": "Robin"}))
    assert _user(port) == "Robin"   # same server process: no restart


def test_an_explicitly_stored_owner_name_still_wins(started):
    store, cfg, port = started
    store.seed_meta(owner_name="Pat")
    cfg.write_text(json.dumps({"owner_name": "Robin"}))
    assert _user(port) == "Pat"
