"""#10861: the poller falls back to the legacy <root>/quiet-sessions.txt, with a warning."""
import importlib.util
import os

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")


def _load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(SCRIPTS, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


cfg = _load("fleet_poll_cfg_legacy", "fleet_poll_cfg.py")
qs = _load("quiet_sessions_legacy", "quiet-sessions.py")


def _resolve(root):
    return cfg.resolve_quiet_sessions_file(str(root), env={})


def test_legacy_only_reads_the_legacy_list_and_warns(tmp_path, capsys):
    (tmp_path / "quiet-sessions.txt").write_text("tm-old\n")
    path = _resolve(tmp_path)
    assert qs.read_quiet_sessions(path) == {"tm-old"}
    err = capsys.readouterr().err
    assert str(tmp_path / "quiet-sessions.txt") in err
    assert str(tmp_path / "local" / "quiet-sessions.txt") in err
    assert "local/" in err.replace(os.sep, "/")


def test_new_path_wins_when_both_exist(tmp_path, capsys):
    (tmp_path / "local").mkdir()
    (tmp_path / "local" / "quiet-sessions.txt").write_text("tm-new\n")
    (tmp_path / "quiet-sessions.txt").write_text("tm-old\n")
    assert qs.read_quiet_sessions(_resolve(tmp_path)) == {"tm-new"}
    assert capsys.readouterr().err == ""


def test_neither_file_is_the_empty_set(tmp_path, capsys):
    assert qs.read_quiet_sessions(_resolve(tmp_path)) == set()
    assert capsys.readouterr().err == ""
