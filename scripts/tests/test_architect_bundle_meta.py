"""Tests for scripts/architect_bundle_meta.py, the Architect bundle provenance and seed history (#10571).

The checked-in files must be current (the --check modes pass on the tree), and each --check mode must fail when its
input moves. The failure cases run against a copy of the inputs in a temp tree, never the checkout.
Run: python3 -m pytest scripts/tests/test_architect_bundle_meta.py -q -p no:cacheprovider
"""

import argparse
import importlib.util
import json
import shutil
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
ROOT = SCRIPTS.parent
_spec = importlib.util.spec_from_file_location("architect_bundle_meta", SCRIPTS / "architect_bundle_meta.py")
meta = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(meta)


def args(**kw):
    base = {"commit": "", "check": True, "from_git": False}
    return argparse.Namespace(**{**base, **kw})


def test_checked_in_provenance_and_history_are_current():
    assert meta.provenance(args()) == 0
    assert meta.history(args()) == 0


def test_history_check_fails_when_a_seed_changes(tmp_path):
    rows = meta.seed_rows(ROOT)
    fleet = tmp_path / meta.FLEET.relative_to(ROOT)
    fleet.mkdir(parents=True)
    shutil.copy(meta.TABLE, fleet / "seed_table.rs")
    shutil.copy(meta.HISTORY, fleet / "seed_history.tsv")
    for _, asset in rows:
        dst = tmp_path / asset
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(ROOT / asset, dst)
    assert meta.history(args(), root=tmp_path) == 0
    (tmp_path / rows[0][1]).write_bytes(b"changed\n")
    assert meta.history(args(), root=tmp_path) == 1
    assert meta.history(args(check=False), root=tmp_path) == 0   # the refresh records the new bytes
    assert meta.history(args(), root=tmp_path) == 0


def test_provenance_check_fails_when_a_pair_source_changes(monkeypatch):
    good = json.loads(meta.PROVENANCE.read_text(encoding="utf-8"))
    rel = sorted(good["files"])[0]
    stale = dict(good, files=dict(good["files"], **{rel: "0" * 64}))
    real_read = Path.read_text

    def read(self, *a, **k):
        return json.dumps(stale) if self == meta.PROVENANCE else real_read(self, *a, **k)

    monkeypatch.setattr(Path, "read_text", read)
    assert meta.provenance(args()) == 1


def test_provenance_refuses_a_malformed_commit():
    assert meta.main(["provenance", "--commit", "not-a-sha", "--check"]) == 2
