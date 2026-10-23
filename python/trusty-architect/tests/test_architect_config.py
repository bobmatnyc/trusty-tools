"""Tests for scripts/architect_config.py, the loader of local/architect.json
(#10161). Pure: tmp_path files and an explicit environ dict, no tmux."""
import ast
import dataclasses
import importlib.util
import json
import os
import sys

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
ROOT = os.path.dirname(SCRIPTS)
SEED = os.path.join(ROOT, "templates", "local", "architect.json")


def _module():
    spec = importlib.util.spec_from_file_location(
        "architect_config_t", os.path.join(SCRIPTS, "architect_config.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


ac = _module()


def _write(root, text):
    local = root / "local"
    local.mkdir(exist_ok=True)
    path = local / "architect.json"
    path.write_text(text, encoding="utf-8")
    return path


def _error(root, text, environ=None):
    _write(root, text)
    with pytest.raises(ac.ConfigError) as info:
        ac.load(root=str(root), environ=environ or {})
    return str(info.value)


def test_no_file_yields_the_documented_defaults(tmp_path):
    cfg = ac.load(root=str(tmp_path), environ={})
    assert cfg.owner_name == ""
    assert cfg.memory_palace == ""
    assert cfg.dashboard_port == 7890
    assert dict(cfg.repos) == {}
    assert dict(cfg.caps) == {}
    assert cfg.unknown == ()
    assert cfg.path == str(tmp_path / "local" / "architect.json")
    assert not (tmp_path / "local").exists(), "load wrote a file"


def test_a_blank_file_yields_the_defaults(tmp_path):
    _write(tmp_path, "  \n")
    assert ac.load(root=str(tmp_path), environ={}).dashboard_port == 7890


def test_the_default_root_is_the_parent_of_scripts():
    cfg = ac.load(environ={})
    assert cfg.path == os.path.join(ROOT, "local", "architect.json")


def test_file_values_are_read(tmp_path):
    _write(tmp_path, json.dumps({
        "owner_name": "alice", "memory_palace": "arch-alice", "dashboard_port": 8123,
        "repos": {"tools": {"slug": "example-org/tools", "gh_config_dir": "/cfg/gh",
                            "checkout": "/src/tools"},
                  "web": {"slug": "example-org/web"}},
        "caps": {"builder_slots": 2, "load_factor": 1.25}}))
    cfg = ac.load(root=str(tmp_path), environ={})
    assert (cfg.owner_name, cfg.memory_palace, cfg.dashboard_port) == ("alice", "arch-alice", 8123)
    assert cfg.repos["tools"] == ac.Repo("example-org/tools", "/cfg/gh", "/src/tools")
    assert cfg.repos["web"] == ac.Repo("example-org/web", "", "")
    assert dict(cfg.caps) == {"builder_slots": 2, "load_factor": 1.25}


def test_the_environment_beats_the_file(tmp_path):
    _write(tmp_path, json.dumps({"owner_name": "alice", "memory_palace": "p1",
                                 "dashboard_port": 8123, "caps": {"load_factor": 1.25}}))
    env = {"ARCHITECT_OWNER_NAME": "carol", "ARCHITECT_MEMORY_PALACE": "p2",
           "DASHBOARD_PORT": "9001", "LOAD_FACTOR": "2"}
    cfg = ac.load(root=str(tmp_path), environ=env)
    assert (cfg.owner_name, cfg.memory_palace, cfg.dashboard_port) == ("carol", "p2", 9001)
    assert cfg.caps["load_factor"] == 2.0


def test_an_empty_variable_counts_as_unset(tmp_path):
    _write(tmp_path, json.dumps({"owner_name": "alice"}))
    cfg = ac.load(root=str(tmp_path), environ={"ARCHITECT_OWNER_NAME": "", "DASHBOARD_PORT": ""})
    assert (cfg.owner_name, cfg.dashboard_port) == ("alice", 7890)


def test_architect_config_names_the_file(tmp_path):
    other = tmp_path / "elsewhere.json"
    other.write_text(json.dumps({"owner_name": "alice"}), encoding="utf-8")
    cfg = ac.load(root=str(tmp_path / "unused"), environ={"ARCHITECT_CONFIG": str(other)})
    assert (cfg.owner_name, cfg.path) == ("alice", str(other))


def test_a_missing_file_named_by_architect_config_fails(tmp_path):
    # See #10161: a mistyped override must not silently yield the defaults.
    missing = str(tmp_path / "typo.json")
    with pytest.raises(ac.ConfigError) as info:
        ac.load(root=str(tmp_path), environ={"ARCHITECT_CONFIG": missing})
    assert missing in str(info.value) and "ARCHITECT_CONFIG" in str(info.value)


def test_load_never_mutates_the_environment(tmp_path):
    env = {"ARCHITECT_OWNER_NAME": "carol"}
    ac.load(root=str(tmp_path), environ=env)
    assert env == {"ARCHITECT_OWNER_NAME": "carol"}


def test_malformed_json_fails_and_names_the_path(tmp_path):
    text = _error(tmp_path, '{"owner_name": "SECRET-alice",')
    assert str(tmp_path / "local" / "architect.json") in text
    assert "not valid JSON" in text
    assert "SECRET-alice" not in text


def test_a_top_level_that_is_not_an_object_fails(tmp_path):
    assert "JSON object" in _error(tmp_path, '["alice"]')


@pytest.mark.parametrize("key,value", [
    ("owner_name", 7), ("memory_palace", ["x"]), ("owner_name", None),
    ("dashboard_port", "7890"), ("dashboard_port", True), ("dashboard_port", 7890.5),
    ("repos", []), ("caps", "x"),
])
def test_a_wrong_type_fails_and_names_the_key(tmp_path, key, value):
    text = _error(tmp_path, json.dumps({key: value}))
    assert key in text


@pytest.mark.parametrize("port", [0, 65536, -1])
def test_a_port_out_of_range_fails_without_echoing_it(tmp_path, port):
    text = _error(tmp_path, json.dumps({"dashboard_port": port}))
    assert "dashboard_port" in text and "1..65535" in text


@pytest.mark.parametrize("repos,key", [
    ({"tools": "example-org/tools"}, "repos.tools"),
    ({"tools": {}}, "repos.tools.slug"),
    ({"tools": {"slug": ""}}, "repos.tools.slug"),
    ({"tools": {"slug": "example-org/tools", "checkout": 3}}, "repos.tools.checkout"),
])
def test_a_bad_repo_entry_fails_and_names_the_key(tmp_path, repos, key):
    assert key in _error(tmp_path, json.dumps({"repos": repos}))


@pytest.mark.parametrize("caps,key", [
    ({"builder_slots": 0}, "caps.builder_slots"),
    ({"builder_slots": 1.5}, "caps.builder_slots"),
    ({"load_factor": 0}, "caps.load_factor"),
    # See #10161: json.loads accepts the NaN and Infinity literals.
    ({"load_factor": float("nan")}, "caps.load_factor"),
    ({"load_factor": float("inf")}, "caps.load_factor"),
    ({"load_factor": float("-inf")}, "caps.load_factor"),
    ({"other": float("nan")}, "caps.other"),
    ({"other": "x"}, "caps.other"),
    ({"other": False}, "caps.other"),
])
def test_a_bad_cap_fails_and_names_the_key(tmp_path, caps, key):
    assert key in _error(tmp_path, json.dumps({"caps": caps}))


@pytest.mark.parametrize("env,name", [
    ({"DASHBOARD_PORT": "http"}, "DASHBOARD_PORT"),
    ({"DASHBOARD_PORT": "70000"}, "DASHBOARD_PORT"),
    ({"LOAD_FACTOR": "fast"}, "LOAD_FACTOR"),
    ({"LOAD_FACTOR": "-1"}, "LOAD_FACTOR"),
    ({"LOAD_FACTOR": "nan"}, "LOAD_FACTOR"),
    ({"LOAD_FACTOR": "inf"}, "LOAD_FACTOR"),
    ({"LOAD_FACTOR": "-inf"}, "LOAD_FACTOR"),
])
def test_a_bad_variable_fails_and_names_it(tmp_path, env, name):
    with pytest.raises(ac.ConfigError) as info:
        ac.load(root=str(tmp_path), environ=env)
    assert name in str(info.value)
    assert env[name] not in str(info.value)


def test_tailnet_url_is_read_from_the_file_and_defaults_to_empty(tmp_path):
    # Ported from the prototype's dashboard footer tests: the footer's second link.
    assert ac.load(root=str(tmp_path / "none"), environ={}).tailnet_url == ""
    _write(tmp_path, json.dumps({"tailnet_url": "https://tailnet.example:7890/"}))
    assert ac.load(root=str(tmp_path), environ={}).tailnet_url == "https://tailnet.example:7890/"


def test_dashboard_tailnet_url_beats_the_file(tmp_path):
    _write(tmp_path, json.dumps({"tailnet_url": "https://tailnet.example:7890/"}))
    env = {"DASHBOARD_TAILNET_URL": "https://override.tailnet.example:7890/"}
    assert ac.load(root=str(tmp_path), environ=env).tailnet_url == env["DASHBOARD_TAILNET_URL"]
    blank = ac.load(root=str(tmp_path), environ={"DASHBOARD_TAILNET_URL": ""})
    assert blank.tailnet_url == "https://tailnet.example:7890/", "an empty variable counts as unset"


@pytest.mark.parametrize("value", [7890, "host.example:7890", "ftp://host.example/"])
def test_a_bad_tailnet_url_in_the_file_fails_and_names_the_key(tmp_path, value):
    text = _error(tmp_path, json.dumps({"tailnet_url": value}))
    assert "tailnet_url" in text and "host.example" not in text


def test_a_bad_dashboard_tailnet_url_fails_and_names_the_variable(tmp_path):
    with pytest.raises(ac.ConfigError) as info:
        ac.load(root=str(tmp_path), environ={"DASHBOARD_TAILNET_URL": "host.example:7890"})
    assert "DASHBOARD_TAILNET_URL" in str(info.value) and "host.example" not in str(info.value)


def test_an_unreadable_path_fails(tmp_path):
    (tmp_path / "local" / "architect.json").mkdir(parents=True)
    with pytest.raises(ac.ConfigError):
        ac.load(root=str(tmp_path), environ={})


def test_unknown_keys_are_listed_not_fatal(tmp_path):
    _write(tmp_path, json.dumps({"owner_nmae": "alice", "zeta": 1,
                                 "repos": {"tools": {"slug": "example-org/tools", "branch": "x"}},
                                 "caps": {"builder_slots": 2, "night_cap": 3}}))
    cfg = ac.load(root=str(tmp_path), environ={})
    assert cfg.unknown == ("owner_nmae", "repos.tools.branch", "zeta")
    assert cfg.owner_name == ""
    assert cfg.caps["night_cap"] == 3, "an extra cap is kept, as the plan's caps table is open"


def test_the_config_is_read_only(tmp_path):
    _write(tmp_path, json.dumps({"repos": {"t": {"slug": "example-org/t"}}, "caps": {"x": 1}}))
    cfg = ac.load(root=str(tmp_path), environ={})
    with pytest.raises(Exception):
        cfg.owner_name = "carol"
    with pytest.raises(TypeError):
        cfg.repos["u"] = ac.Repo("example-org/u", "", "")
    with pytest.raises(TypeError):
        cfg.caps["y"] = 2


def test_the_seed_parses_holds_no_owner_values_and_equals_the_defaults(tmp_path):
    with open(SEED, encoding="utf-8") as f:
        seed = json.load(f)
    assert seed["owner_name"] == "" and seed["memory_palace"] == ""
    assert seed["repos"] == {} and seed["caps"] == {}
    local = tmp_path / "local"
    local.mkdir()
    (local / "architect.json").write_bytes(open(SEED, "rb").read())
    seeded = ac.load(root=str(tmp_path), environ={})
    default = ac.load(root=str(tmp_path / "none"), environ={})
    assert dataclasses.replace(default, path=seeded.path) == seeded


def test_the_module_imports_only_the_standard_library():
    with open(os.path.join(SCRIPTS, "architect_config.py"), encoding="utf-8") as f:
        tree = ast.parse(f.read())
    names = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(a.name.split(".")[0] for a in node.names)
        elif isinstance(node, ast.ImportFrom) and node.level == 0:
            names.add(node.module.split(".")[0])
    stdlib = getattr(sys, "stdlib_module_names", None)
    allowed = stdlib or {"__future__", "dataclasses", "json", "os", "types", "typing"}
    assert names and names <= set(allowed), names
