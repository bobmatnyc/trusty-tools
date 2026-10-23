"""dashboard-db footer: same count as tally, parked excluded from N, deterministic bytes, unreadable store fails.

Test: scripts/test_dashboard_footer.py
"""
import importlib.util
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
spec = importlib.util.spec_from_file_location("dashboard_db", os.path.join(SCRIPTS, "dashboard-db.py"))
db = importlib.util.module_from_spec(spec)
spec.loader.exec_module(db)


def put(root, doc_id, **data):
    d = root / "items"
    d.mkdir(exist_ok=True)
    (d / f"{doc_id}.json").write_text(json.dumps(dict(data, version=1)))


def mixed(root):
    put(root, "D01", kind="question", status="open")
    put(root, "D02", kind="question", status="open", parked={"until": "2999-01-01"})
    put(root, "A01", kind="action", status="open")
    put(root, "T01", kind="task", status="open", run_start="2000-01-01", run_end="2999-01-01")
    put(root, "D03", kind="question", status="answered")
    put(root, "D04", kind="question", status="done")


def run(root, capsys, *args):
    rc = db.main(["--root", str(root), *args])
    cap = capsys.readouterr()
    return rc, cap.out, cap.err


def test_footer_count_equals_tally_and_parked_excluded(tmp_path, capsys, monkeypatch):
    monkeypatch.setattr(db, "TAILNET_URL", "https://host.tailnet.example.com:7890/")
    mixed(tmp_path)
    _, tally_out, _ = run(tmp_path, capsys, "tally")
    rc, out, _ = run(tmp_path, capsys, "footer")
    assert rc == 0
    assert out == (f"Dashboard: [localhost]({db.LOCAL_URL}) · [tailnet]({db.TAILNET_URL}) — "
                   + tally_out)
    assert tally_out.startswith("3 open dashboard items:")   # D02 parked is not in N
    assert "(1 parked)" in out and "(1 running)" in out
    assert out.count("\n") == 1


def test_footer_json_and_determinism(tmp_path, capsys):
    mixed(tmp_path)
    _, a, _ = run(tmp_path, capsys, "footer")
    _, b, _ = run(tmp_path, capsys, "footer")
    assert a == b
    rc, j, _ = run(tmp_path, capsys, "footer", "--json")
    d = json.loads(j)
    assert rc == 0 and d["footer"] + "\n" == a and d["tally"]["open"] == 3 and d["tally"]["parked"] == 1


def test_footer_unreadable_store_fails_without_count(tmp_path, capsys):
    rc, out, err = run(tmp_path / "missing", capsys, "footer")
    assert rc != 0 and out == "" and "not readable" in err
    put(tmp_path, "D01", kind="question", status="open")
    (tmp_path / "items" / "D01.json").write_text("{not json")
    rc, out, err = run(tmp_path, capsys, "footer")
    assert rc != 0 and out == "" and err


LOCAL = "http://127.0.0.1:7890/"   # the footer's localhost link


def fresh_db(tmp_path, monkeypatch, config, env_url=None):
    """A newly loaded dashboard-db whose config is `config` (dict) and whose DASHBOARD_TAILNET_URL is `env_url`."""
    cfg = tmp_path / "architect.json"
    cfg.write_text(json.dumps(config))
    monkeypatch.setenv("ARCHITECT_CONFIG", str(cfg))
    if env_url is None:
        monkeypatch.delenv("DASHBOARD_TAILNET_URL", raising=False)
    else:
        monkeypatch.setenv("DASHBOARD_TAILNET_URL", env_url)
    s = importlib.util.spec_from_file_location("dashboard_db_fresh", os.path.join(SCRIPTS, "dashboard-db.py"))
    m = importlib.util.module_from_spec(s)
    s.loader.exec_module(m)
    return m


def footer_out(m, root, capsys):
    root.mkdir()
    mixed(root)
    rc = m.main(["--root", str(root), "footer"])
    cap = capsys.readouterr()
    assert rc == 0, cap.err
    return cap.out


def test_footer_takes_tailnet_from_architect_json(tmp_path, capsys, monkeypatch):
    """#10161: only local/architect.json carries the tailnet origin; the footer prints both links."""
    m = fresh_db(tmp_path, monkeypatch, {"tailnet_url": "https://tailnet.example:7890/"})
    out = footer_out(m, tmp_path / "store", capsys)
    assert out.startswith(f"Dashboard: [localhost]({LOCAL}) · [tailnet](https://tailnet.example:7890/) — ")


def test_footer_env_tailnet_overrides_architect_json(tmp_path, capsys, monkeypatch):
    m = fresh_db(tmp_path, monkeypatch, {"tailnet_url": "https://tailnet.example:7890/"},
                 env_url="https://override.tailnet.example:7890/")
    out = footer_out(m, tmp_path / "store", capsys)
    assert "[tailnet](https://override.tailnet.example:7890/) — " in out
    assert "https://tailnet.example:" not in out


def test_footer_without_any_tailnet_origin_prints_localhost_only(tmp_path, capsys, monkeypatch):
    m = fresh_db(tmp_path, monkeypatch, {})
    out = footer_out(m, tmp_path / "store", capsys)
    assert out.startswith(f"Dashboard: [localhost]({LOCAL}) — ") and "tailnet" not in out


def test_footer_survives_unusable_architect_json(tmp_path, capsys, monkeypatch):
    """A config that fails to load costs the tailnet link (named on stderr), never the footer; env still wins."""
    m = fresh_db(tmp_path, monkeypatch, {"tailnet_url": 7890})
    assert "tailnet_url" in capsys.readouterr().err
    out = footer_out(m, tmp_path / "store", capsys)
    assert out.startswith(f"Dashboard: [localhost]({LOCAL}) — ")
    m = fresh_db(tmp_path, monkeypatch, {"tailnet_url": 7890}, env_url="https://override.tailnet.example:7890/")
    capsys.readouterr()
    assert "[tailnet](https://override.tailnet.example:7890/)" in footer_out(m, tmp_path / "store2", capsys)
