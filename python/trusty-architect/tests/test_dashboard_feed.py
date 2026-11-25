"""dashboard_feed.py: the accounts path (newest ledger line per account, written once, stale docs dropped) and the
clear error while token_watch.py is absent.

#10571 S2: the pin's build_feed cases (active/parked edge, session-id mapping, token windows, live state, fleet totals)
need token_watch.py, which ships with the usage ledger (S1); they are ported with it.
Test: python3 -m pytest -q tests/test_dashboard_feed.py
"""
import importlib.util
import json
import os

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10571 S2: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
_spec = importlib.util.spec_from_file_location("dashboard_feed", os.path.join(SCRIPTS, "dashboard_feed.py"))
df = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(df)


def test_accounts_latest_line_per_account_written_once(tmp_path):
    ledger = tmp_path / "ledger.jsonl"
    lines = [
        {"ts": "2026-10-09T01:00:00Z", "account": "a@example.com", "pct5h": 10, "reset5h": "r", "pctwk": 50, "resetwk": "w", "source": "s", "weekly_models": {}},
        {"ts": "2026-10-09T02:00:00Z", "account": "a@example.com", "pct5h": 20, "reset5h": "r", "pctwk": 51, "resetwk": "w", "source": "s"},
        {"ts": "2026-10-09T01:30:00Z", "account": "B.c@example.org", "pct5h": 0, "reset5h": None, "pctwk": 90, "resetwk": "w2", "source": "manual"},
    ]
    ledger.write_text("\n".join(json.dumps(r) for r in lines) + "\nnot json\n" + json.dumps({"ts": "t"}) + "\n")
    acc = df.latest_accounts(str(ledger))
    assert sorted(acc) == ["a-example-com", "b-c-example-org"]                 # one doc per account, count follows the data
    assert acc["a-example-com"]["pct5h"] == 20 and acc["b-c-example-org"]["reset5h"] is None
    root = tmp_path / "store"
    assert sorted(df.write_accounts(str(root), acc)) == ["a-example-com", "b-c-example-org"]
    assert df.write_accounts(str(root), acc) == []                # unchanged: no rewrite, no version bump
    assert json.loads((root / "accounts" / "a-example-com.json").read_text())["pct5h"] == 20
    assert df.latest_accounts(str(tmp_path / "missing.jsonl")) == {}


def test_non_email_ledger_line_skipped_and_stale_accounts_doc_removed(tmp_path, capsys):
    """#10571: "acct6" must not reach the store, and a doc no longer in the ledger is dropped via the prune log."""
    ledger = tmp_path / "ledger.jsonl"
    base = {"pct5h": 1, "reset5h": None, "pctwk": 2, "resetwk": "w", "source": "s"}
    ledger.write_text("\n".join(json.dumps(r) for r in [
        {"ts": "2026-11-06T22:29:30Z", "account": "acct6", **base},
        {"ts": "2026-11-06T22:00:00Z", "account": "a@example.com", **base}]) + "\n")
    acc = df.latest_accounts(str(ledger))
    assert sorted(acc) == ["a-example-com"]                              # non-email label skipped
    root = tmp_path / "store"
    st = df.ds.Store(str(root), actor="test")
    st.set("accounts", "acct6", {"account": "acct6"})              # stale doc already in the store
    assert df.write_accounts(str(root), acc) == ["a-example-com"]
    assert not (root / "accounts" / "acct6.json").exists() and (root / "accounts" / "a-example-com.json").exists()
    assert "stale accounts/acct6" in capsys.readouterr().err
    pruned = (tmp_path / "owner-queue-archive" / "pruned.jsonl").read_text()
    assert '"id": "acct6"' in pruned
    assert df.write_accounts(str(root), {}) == [] and (root / "accounts" / "a-example-com.json").exists()  # empty ledger drops nothing


def test_build_feed_without_token_watch_names_the_missing_file(monkeypatch, tmp_path, capsys):
    # #10571 S2: the module loads without token_watch.py; the feed refuses with the path, never a partial feed.
    monkeypatch.setattr(df, "tw", None)
    with pytest.raises(df.TokenWatchMissing, match="usage watcher module"):
        df.build_feed(0.0, names={}, live={})
    out = tmp_path / "feed.json"
    assert df.main(["--out", str(out), "--ledger", str(tmp_path / "none.jsonl"),
                    "--store-root", str(tmp_path / "store")]) == 2
    assert "usage watcher module" in capsys.readouterr().err and not out.exists()
