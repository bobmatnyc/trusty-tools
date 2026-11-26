"""Tests for the install-path rungs of scripts/architect_config.py (#11821 P6b).

The chain is trusty-common's: env, then <home>/.trusty-tools/install.toml, then
the default; a broken install.toml fails closed with ConfigError. Every test
passes an explicit environ with HOME under tmp_path, so none reads the
operator's install file.
"""
import importlib.util
import os
import sys

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
if SCRIPTS not in sys.path:
    sys.path.insert(0, SCRIPTS)
import architect_config as ac  # noqa: E402
import dashboard_projects as dp  # noqa: E402


def _self_ctx():
    spec = importlib.util.spec_from_file_location("self_ctx_t", os.path.join(SCRIPTS, "self-ctx.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _install(home, text):
    tools = home / ".trusty-tools"  # path-literal: allow (a tmp_path fixture home)
    tools.mkdir(parents=True, exist_ok=True)
    (tools / "install.toml").write_text(text, encoding="utf-8")


BROKEN = "config_dir = [not a string\n"


def test_defaults_without_an_install_file(tmp_path):
    env = {"HOME": str(tmp_path)}
    assert ac.install_config_dir(env) == str(tmp_path)
    assert ac.install_projects_root(env) == str(tmp_path / "trusty-mpm-projects")
    assert ac.managed_claude_config_dir(env) == str(tmp_path / ".trusty-tools" / "trusty-mpm" / "claude-config")


def test_install_file_beats_default_and_env_beats_file(tmp_path):
    _install(tmp_path, 'config_dir = "/srv/cfg"\nprojects_root = "/srv/projects"\n')
    env = {"HOME": str(tmp_path)}
    assert ac.install_config_dir(env) == "/srv/cfg"
    assert ac.install_projects_root(env) == "/srv/projects"
    env.update(TRUSTY_MPM_CONFIG_DIR=" ~/c ", TRUSTY_MPM_WORKSPACE_ROOT="/env/p")
    assert ac.install_config_dir(env) == str(tmp_path / "c")
    assert ac.install_projects_root(env) == "/env/p"


@pytest.mark.parametrize("text", [BROKEN, 'colour = "/x"\n', "config_dir = 3\n", 'config_dir = "rel/dir"\n'])
def test_a_broken_install_file_fails_closed(tmp_path, text):
    _install(tmp_path, text)
    with pytest.raises(ac.ConfigError):
        ac.install_config_dir({"HOME": str(tmp_path)})


def test_an_env_value_wins_over_a_broken_file_and_a_relative_env_value_is_refused(tmp_path):
    _install(tmp_path, BROKEN)
    assert ac.install_config_dir({"HOME": str(tmp_path), "TRUSTY_MPM_CONFIG_DIR": "/e"}) == "/e"
    with pytest.raises(ac.ConfigError):
        ac.install_config_dir({"HOME": str(tmp_path), "TRUSTY_MPM_CONFIG_DIR": "rel"})


def test_load_defaults_follow_the_install_file(tmp_path):
    home = tmp_path / "home"
    _install(home, 'config_dir = "/srv/cfg"\nprojects_root = "/srv/projects"\n')
    cfg = ac.load(root=str(tmp_path), environ={"HOME": str(home)})
    assert cfg.projects_root == "/srv/projects"
    assert cfg.claude_config_dir == "/srv/cfg/.trusty-tools/trusty-mpm/claude-config"
    assert ac.default_claude_config_dir({"HOME": str(home)}) == cfg.claude_config_dir


def test_load_fails_closed_on_a_broken_install_file_unless_both_paths_are_explicit(tmp_path):
    home = tmp_path / "home"
    _install(home, BROKEN)
    with pytest.raises(ac.ConfigError):
        ac.load(root=str(tmp_path), environ={"HOME": str(home)})
    explicit = {"HOME": str(home), "ARCHITECT_PROJECTS_ROOT": "/p", "CLAUDE_CONFIG_DIR": "/c"}
    cfg = ac.load(root=str(tmp_path), environ=explicit)
    assert (cfg.projects_root, cfg.claude_config_dir) == ("/p", "/c")


def test_self_ctx_default_dir_follows_the_install_paths(tmp_path):
    _install(tmp_path, 'config_dir = "/srv/cfg"\nprojects_root = "/srv/projects"\n')
    got = _self_ctx().default_dir({"HOME": str(tmp_path)})
    assert got == "/srv/cfg/.trusty-tools/trusty-mpm/claude-config/projects/-srv-projects-architect"
    _install(tmp_path, BROKEN)
    with pytest.raises(ac.ConfigError):
        _self_ctx().default_dir({"HOME": str(tmp_path)})


def test_mpm_root_follows_the_config_dir_and_fails_closed(tmp_path):
    _install(tmp_path, 'config_dir = "/srv/cfg"\n')
    assert dp.mpm_root({"HOME": str(tmp_path)}) == "/srv/cfg/.trusty-mpm"
    _install(tmp_path, BROKEN)
    with pytest.raises(dp.RegistryError):
        dp.mpm_root({"HOME": str(tmp_path)})


# #11821 P6b r1: install.toml syntax parity with trusty-common's reader (the `toml` crate).
# Rows named as a SYNTAX_FIXTURES row mirror it from `architect_reader_matches_trusty_common_resolver`
# (trusty-crate-contracts test target architect_install_paths_parity_11821, #11916); those
# fixtures are Rust consts, so they are mirrored here, not read. "two BOMs" follows toml_parser's
# lexer, which skips exactly one leading BOM; the "BOM, CRLF ..." row combines three mirrored rows.
# want "" means Rust refuses with a parse error; None means Rust accepts and the key stays at its default.
_SYNTAX_PARITY = [
    ("BOM", "﻿config_dir = \"/bom\"\n", "/bom"),
    ("BOM, CRLF line ends and comments", "﻿# c\r\nconfig_dir = \"/bom\" # here\r\n", "/bom"),
    ("BOM after first line", "# c\n﻿config_dir = \"/bom\"\n", ""),
    ("two BOMs", "﻿﻿config_dir = \"/bom\"\n", ""),
    ("CRLF line ends", "# c\r\nconfig_dir = \"/crlf\"\r\n", "/crlf"),
    ("only comments", "# one\n\n   # two\n", None),
    ("comments and a trailing comment", "# moved\n\nconfig_dir = '/lit/path' # here\n", "/lit/path"),
]


@pytest.fixture(params=["tomllib", "fallback"])
def toml_reader(request, monkeypatch):
    """Run each parity row through tomllib and through the <3.11 flat-line fallback."""
    if request.param == "fallback":
        monkeypatch.setitem(sys.modules, "tomllib", None)  # `import tomllib` raises ImportError
    elif sys.version_info < (3, 11):
        pytest.skip("tomllib needs Python 3.11")
    return request.param


@pytest.mark.parametrize("name,body,want", _SYNTAX_PARITY, ids=[r[0] for r in _SYNTAX_PARITY])
def test_install_file_syntax_matches_the_rust_reader(tmp_path, toml_reader, name, body, want):
    tools = tmp_path / ".trusty-tools"  # path-literal: allow (a tmp_path fixture home)
    tools.mkdir()
    (tools / "install.toml").write_bytes(body.encode("utf-8"))
    env = {"HOME": str(tmp_path)}
    if want == "":
        with pytest.raises(ac.ConfigError):
            ac.install_config_dir(env)
    else:
        assert ac.install_config_dir(env) == (want or str(tmp_path)), name
