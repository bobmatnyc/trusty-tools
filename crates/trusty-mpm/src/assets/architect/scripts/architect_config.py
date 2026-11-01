"""The Architect's instance settings, read from `local/architect.json` (#10161).

Why: owner name, repo table, dashboard port, memory palace and caps differ per
installation. They must live in the instance's own file, never in a shipped
script, so the shipped scripts carry no operator value (#9171, D77).
`tailnet_url` is the dashboard's tailnet origin (e.g. https://<name>:7890/), the
second link of the reply footer; "" means localhost only. `projects_root` is the
directory holding <owner>/<repo> checkouts (default ~/trusty-mpm-projects;
`ARCHITECT_PROJECTS_ROOT` overrides) and `claude_config_dir` the Claude Code config
directory whose projects/ holds the transcripts (`CLAUDE_CONFIG_DIR` overrides; default
~/.trusty-tools/trusty-mpm/claude-config). Both are returned absolute (#10571).
What: `load()` reads `<root>/local/architect.json`, where `root` is the parent
of this `scripts/` directory, and returns a frozen `Config`. `ARCHITECT_CONFIG`
names another file, which must exist. Precedence is environment, then file,
then default; an empty variable counts as unset. A missing or blank file at the
default path yields the defaults and writes nothing. Malformed JSON, a wrong
type, an out-of-range or non-finite value, a missing `ARCHITECT_CONFIG` file or
an unreadable path raises `ConfigError`, which names the path and the key and
never the value. Unknown keys are kept out of the config and listed in
`Config.unknown`. Session names stay env-only (#8436). Standard library only,
Python 3.9+; import it as `architect_config` from a sibling script.
Test: tests/test_architect_config.py; tailnet_url in tests/test_dashboard_footer.py.
"""
# No `from __future__ import annotations`: a dataclass with string annotations
# fails when a sibling loads this file by path without registering it.
import json
import math
import os
import types
from dataclasses import dataclass, field
from typing import Any, Mapping, Optional, Tuple

DEFAULT_PORT = 7890
_SCRIPTS = os.path.dirname(os.path.abspath(__file__))
_KNOWN = ("owner_name", "memory_palace", "dashboard_port", "tailnet_url", "repos", "caps",
          "projects_root", "claude_config_dir")
DEFAULT_PROJECTS_ROOT = "~/trusty-mpm-projects"
DEFAULT_CLAUDE_CONFIG_DIR = "~/.trusty-tools/trusty-mpm/claude-config"
_REPO_FIELDS = ("slug", "gh_config_dir", "checkout")
_EMPTY: Mapping[str, Any] = types.MappingProxyType({})


class ConfigError(ValueError):
    """`local/architect.json` or an override variable is unusable."""


@dataclass(frozen=True)
class Repo:
    """One `repos` entry: the GitHub slug, its gh config dir and its checkout."""

    slug: str
    gh_config_dir: str = ""
    checkout: str = ""


@dataclass(frozen=True)
class Config:
    """The resolved settings. `repos` and `caps` are read-only mappings.

    `owner_name` "" means no owner is configured; callers say "the owner".
    `memory_palace` "" means the caller derives it from its session or project.
    `tailnet_url` "" means no tailnet origin: the footer shows localhost only.
    A cap absent from `caps` means the calling script's own constant applies.
    `projects_root` and `claude_config_dir` are absolute; see the module doc for defaults.
    """

    path: str
    owner_name: str = ""
    memory_palace: str = ""
    dashboard_port: int = DEFAULT_PORT
    tailnet_url: str = ""
    repos: Mapping[str, Repo] = field(default_factory=lambda: _EMPTY)
    caps: Mapping[str, Any] = field(default_factory=lambda: _EMPTY)
    unknown: Tuple[str, ...] = ()
    projects_root: str = ""
    claude_config_dir: str = ""


def default_claude_config_dir(environ: Optional[Mapping[str, str]] = None) -> str:
    """`CLAUDE_CONFIG_DIR`, else the trusty-mpm default; absolute. Reads no file (safe at import time)."""
    env = os.environ if environ is None else environ
    return os.path.abspath(os.path.expanduser(env.get("CLAUDE_CONFIG_DIR") or DEFAULT_CLAUDE_CONFIG_DIR))


def default_path(root: Optional[str] = None,
                 environ: Optional[Mapping[str, str]] = None) -> str:
    """The file `load` reads: `ARCHITECT_CONFIG`, else `<root>/local/architect.json`."""
    env = os.environ if environ is None else environ
    override = env.get("ARCHITECT_CONFIG")
    if override:
        return override
    return os.path.join(root or os.path.dirname(_SCRIPTS), "local", "architect.json")


def load(root: Optional[str] = None, environ: Optional[Mapping[str, str]] = None) -> Config:
    """Resolve the config; see the module doc for precedence and failures."""
    env = os.environ if environ is None else environ
    path = default_path(root, env)
    data = _read(path, required=bool(env.get("ARCHITECT_CONFIG")))
    unknown = sorted(k for k in data if k not in _KNOWN)
    owner = _string(path, data, "owner_name")
    palace = _string(path, data, "memory_palace")
    port = _port(path, "dashboard_port", data.get("dashboard_port", DEFAULT_PORT))
    tailnet = _url(path, "tailnet_url", _string(path, data, "tailnet_url"))
    repos = _repos(path, data.get("repos", {}), unknown)
    caps = _caps(path, data.get("caps", {}))
    projects_root = _string(path, data, "projects_root") or DEFAULT_PROJECTS_ROOT
    claude_dir = _string(path, data, "claude_config_dir") or DEFAULT_CLAUDE_CONFIG_DIR
    # See #10161: the environment wins over the file, one variable per key.
    owner = env.get("ARCHITECT_OWNER_NAME") or owner
    palace = env.get("ARCHITECT_MEMORY_PALACE") or palace
    if env.get("DASHBOARD_TAILNET_URL"):
        tailnet = _url("environment", "DASHBOARD_TAILNET_URL", env["DASHBOARD_TAILNET_URL"])
    if env.get("DASHBOARD_PORT"):
        port = _port("environment", "DASHBOARD_PORT", _env_number(env, "DASHBOARD_PORT", int))
    projects_root = env.get("ARCHITECT_PROJECTS_ROOT") or projects_root
    claude_dir = env.get("CLAUDE_CONFIG_DIR") or claude_dir
    if env.get("LOAD_FACTOR"):
        caps["load_factor"] = _positive(
            "environment", "LOAD_FACTOR", _env_number(env, "LOAD_FACTOR", float))
    return Config(path=path, owner_name=owner, memory_palace=palace, dashboard_port=port,
                  tailnet_url=tailnet, repos=types.MappingProxyType(repos), caps=types.MappingProxyType(caps),
                  unknown=tuple(sorted(unknown)), projects_root=_abs(projects_root),
                  claude_config_dir=_abs(claude_dir))


def _abs(value: str) -> str:
    return os.path.abspath(os.path.expanduser(value))


def _read(path: str, required: bool) -> dict:
    """The file's top-level object; `{}` when blank, or absent and not `required`."""
    try:
        with open(path, encoding="utf-8") as f:
            text = f.read()
    except FileNotFoundError:
        if required:
            # See #10161: a mistyped override must not silently yield the defaults.
            raise ConfigError(f"{path}: named by ARCHITECT_CONFIG, but does not exist") from None
        return {}
    except (OSError, UnicodeDecodeError) as err:
        # See #10161: an unreadable file fails; it never falls back to defaults.
        raise ConfigError(f"{path}: cannot read: {type(err).__name__}") from None
    if not text.strip():
        return {}
    try:
        data = json.loads(text)
    except json.JSONDecodeError as err:
        # See #10161: the decoder's message carries no file content, only a position.
        raise ConfigError(
            f"{path}: not valid JSON ({err.msg} at line {err.lineno} column {err.colno})"
        ) from None
    if not isinstance(data, dict):
        raise ConfigError(f"{path}: the top level must be a JSON object")
    return data


def _fail(path: str, key: str, want: str) -> ConfigError:
    return ConfigError(f"{path}: {key}: must be {want}")


def _string(path: str, data: Mapping[str, Any], key: str) -> str:
    value = data.get(key, "")
    if not isinstance(value, str):
        raise _fail(path, key, "a string")
    return value


def _url(path: str, key: str, value: str) -> str:
    """"" or an http(s) origin; the error names the key, never the value."""
    if value and not value.startswith(("https://", "http://")):
        raise _fail(path, key, "an http:// or https:// URL")
    return value


def _is_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _is_number(value: Any) -> bool:
    # See #10161: NaN and Infinity (env text or JSON literals) are not numbers here.
    return (isinstance(value, (int, float)) and not isinstance(value, bool)
            and math.isfinite(value))


def _port(path: str, key: str, value: Any) -> int:
    if not _is_int(value) or not 1 <= value <= 65535:
        raise _fail(path, key, "an integer in 1..65535")
    return value


def _positive(path: str, key: str, value: Any) -> Any:
    if not _is_number(value) or value <= 0:
        raise _fail(path, key, "a number above 0")
    return value


def _env_number(env: Mapping[str, str], name: str, kind: type) -> Any:
    try:
        return kind(env[name])
    except ValueError:
        raise _fail("environment", name, f"a{'n integer' if kind is int else ' number'}") from None


def _repos(path: str, value: Any, unknown: list) -> dict:
    """`{name: Repo}`; `slug` is required, the other two fields default to ""."""
    if not isinstance(value, dict):
        raise _fail(path, "repos", "an object of name -> {slug, gh_config_dir, checkout}")
    repos = {}
    for name, entry in value.items():
        key = f"repos.{name}"
        if not isinstance(entry, dict):
            raise _fail(path, key, "an object")
        fields = {}
        for f in _REPO_FIELDS:
            item = entry.get(f, "")
            if not isinstance(item, str):
                raise _fail(path, f"{key}.{f}", "a string")
            fields[f] = item
        if not fields["slug"]:
            raise _fail(path, f"{key}.slug", "a non-empty string")
        unknown.extend(f"{key}.{k}" for k in entry if k not in _REPO_FIELDS)
        repos[name] = Repo(**fields)
    return repos


def _caps(path: str, value: Any) -> dict:
    """Numeric caps; `builder_slots` a positive integer, `load_factor` above 0."""
    if not isinstance(value, dict):
        raise _fail(path, "caps", "an object of name -> number")
    caps = {}
    for name, item in value.items():
        key = f"caps.{name}"
        if not _is_number(item):
            raise _fail(path, key, "a number")
        if name == "builder_slots" and (not _is_int(item) or item < 1):
            raise _fail(path, key, "an integer of 1 or more")
        if name == "load_factor":
            _positive(path, key, item)
        caps[name] = item
    return caps
