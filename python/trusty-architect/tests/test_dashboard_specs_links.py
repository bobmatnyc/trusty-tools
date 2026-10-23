"""dashboard-db.py normalizes item `specs` to one localhost reader-form entry per local doc (the owner 2026-10-21).

Covers: legacy doc/ and files/local/briefs/ forms, label suffix strip, localhost+tailnet pair collapse, spec equal to
`brief` dropped, other urls untouched, detail URL moved to specs, update and batch paths. Test: scripts/test_dashboard_specs_links.py
"""
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
L, T = "http://127.0.0.1:7890/", "https://host.tailnet.example.com:7890/"


def cli(root, *args):
    p = subprocess.run([sys.executable, os.path.join(SCRIPTS, "dashboard-db.py"), "--root", str(root), *args], capture_output=True, text=True, env={**os.environ, "DASHBOARD_TAILNET_URL": T})
    return p.returncode, p.stdout, p.stderr


def get(root, i):
    return json.loads(cli(root, "get", "items", i)[1])["data"]


def test_single_entry_legacy_forms_pair_collapse_and_untouched(tmp_path):
    specs = [{"label": "Brief", "url": L + "files/local/briefs/x.md"},
             {"label": "Doc (localhost)", "url": L + "doc/mcp/docs/a.md"},
             {"label": "Doc (tailnet)", "url": T + "#doc/mcp/docs/a.md"},
             {"label": "Spec", "url": "https://example.com/s"}]
    assert cli(tmp_path, "set", "items", "D01", "--json", json.dumps({"specs": specs}))[0] == 0
    assert get(tmp_path, "D01")["specs"] == [
        {"label": "Brief", "url": L + "#doc/architect/local/briefs/x.md"},
        {"label": "Doc", "url": L + "#doc/mcp/docs/a.md"},
        {"label": "Spec", "url": "https://example.com/s"}]


def test_spec_equal_to_brief_dropped_and_idempotent(tmp_path):
    d = {"brief": "p/docs/briefs/b.md", "specs": [{"label": "B", "url": L + "#doc/p/docs/briefs/b.md"}, {"label": "C", "url": L + "#doc/p/c.md"}]}
    cli(tmp_path, "set", "items", "D01", "--json", json.dumps(d))
    assert get(tmp_path, "D01")["specs"] == [{"label": "C", "url": L + "#doc/p/c.md"}]


def test_detail_url_moved_to_specs_on_update_and_batch(tmp_path):
    cli(tmp_path, "set", "items", "D01", "--json", "{}")
    d = {"detail": "Read " + L + "#doc/p/a.md now"}
    assert cli(tmp_path, "update", "items", "D01", "--json", json.dumps(d))[0] == 0
    g = get(tmp_path, "D01")
    assert g["detail"] == "Read see doc links now" and g["specs"] == [{"label": "a.md", "url": L + "#doc/p/a.md"}]
    ops = [{"op": "set", "collection": "items", "id": "D02", "data": {"specs": [{"label": "B", "url": L + "doc/p/b.md"}]}}]
    assert cli(tmp_path, "batch", json.dumps(ops))[0] == 0
    assert get(tmp_path, "D02")["specs"] == [{"label": "B", "url": L + "#doc/p/b.md"}]
