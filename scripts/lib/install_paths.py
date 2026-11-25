#!/usr/bin/env python3
"""install_paths.py — the Python twin of trusty-common's InstallPaths resolver (#11821 P6a).

Why: the projects root (`~/trusty-mpm-projects`) and the config dir (the
directory holding `.trusty-tools` and `.trusty-mpm`) are configurable per
install. Python tooling that derived them from `$HOME` would write where the
operator did not choose. One reader, with the Rust precedence, keeps both
languages on the same answer.
What: the rules of crates/trusty-common/src/install_paths/mod.rs, restated:
  - per key, the first of override > env > install file > default (mod.rs:9,
    `resolve_with` at mod.rs:275);
  - the install file is always `<home>/.trusty-tools/install.toml`
    (file.rs:51), read at most once, and only when an override or env does not
    decide the key, so a broken file never blocks a valid env value
    (mod.rs:280-288);
  - override and env values are trimmed, an empty env value means unset, and
    a leading `~` is expanded (mod.rs:320-335; `expand_tilde`,
    workspace_layout.rs:150); file values are taken literally (mod.rs:344);
  - every winning value must be non-empty and absolute (mod.rs:375); a file
    with bad TOML, a non-string value or an unknown key is an error
    (file.rs:28-30, 62-80); a missing file means "no file rung";
  - defaults: projects_root `<home>/trusty-mpm-projects`, config_dir `<home>`,
    claude_config_dir and codex_home none (mod.rs:233-239);
  - env names: TRUSTY_MPM_WORKSPACE_ROOT, TRUSTY_MPM_CONFIG_DIR,
    CLAUDE_CONFIG_DIR, CODEX_HOME (mod.rs:206-213).
  Every error raises InstallPathsError. There is no tolerant variant: a
  caller that cannot resolve a path does not fall back to `$HOME`.
  `managed_claude_config_dir` is `<config_dir>/.trusty-tools/trusty-mpm/
  claude-config`, as trusty_tools_config.rs:780 derives it through
  crate_config.rs:276 (one key, like `resolve_process_key`, mod.rs:392).
  Standard library only. Below Python 3.11 (no `tomllib`; macOS ships 3.9)
  only the flat `key = "string"` lines the Rust writer emits are read, and any
  other line fails closed.
Test: scripts/tests/test_install_paths.py.
Usage: python3 scripts/lib/install_paths.py [--json]
  Prints each key, its path and its source. Exit 0; 1 on InstallPathsError.
"""
from __future__ import annotations

import json
import os
import re
import sys
from dataclasses import dataclass
from typing import Callable, Mapping, Optional

PROJECTS_ROOT = "projects_root"
CONFIG_DIR = "config_dir"
CLAUDE_CONFIG_DIR = "claude_config_dir"
CODEX_HOME = "codex_home"
KEYS = (PROJECTS_ROOT, CONFIG_DIR, CLAUDE_CONFIG_DIR, CODEX_HOME)

ENV_VARS = {
    PROJECTS_ROOT: "TRUSTY_MPM_WORKSPACE_ROOT",
    CONFIG_DIR: "TRUSTY_MPM_CONFIG_DIR",
    CLAUDE_CONFIG_DIR: "CLAUDE_CONFIG_DIR",
    CODEX_HOME: "CODEX_HOME",
}
TOOLS_DIR_NAME = ".trusty-tools"
MPM_DIR_NAME = ".trusty-mpm"
DEFAULT_WORKSPACE_DIR = "trusty-mpm-projects"
INSTALL_FILE_NAME = "install.toml"

FLAG, ENV, INSTALL_FILE, DEFAULT = "flag", "env", "install-file", "default"

EnvLookup = Callable[[str], Optional[str]]


class InstallPathsError(Exception):
    """A path could not be resolved; the message names the key and source."""


@dataclass(frozen=True)
class ResolvedPath:
    """An absolute path and the precedence rung that supplied it."""

    path: str
    source: str


@dataclass(frozen=True)
class InstallPaths:
    """The resolved host paths of one install (mod.rs:176)."""

    projects_root: ResolvedPath
    config_dir: ResolvedPath
    claude_config_dir: Optional[ResolvedPath]
    codex_home: Optional[ResolvedPath]

    def tools_dir(self) -> str:
        """`<config_dir>/.trusty-tools`."""
        return os.path.join(self.config_dir.path, TOOLS_DIR_NAME)

    def mpm_dir(self) -> str:
        """`<config_dir>/.trusty-mpm`."""
        return os.path.join(self.config_dir.path, MPM_DIR_NAME)


def home_dir(env: Optional[Mapping[str, str]] = None) -> str:
    """`$HOME` when set and non-empty, else the password database (as `dirs::home_dir`)."""
    env = os.environ if env is None else env
    home = env.get("HOME") or ""
    if home:
        return home
    try:
        import pwd

        return pwd.getpwuid(os.getuid()).pw_dir
    except (ImportError, KeyError, OSError) as e:
        raise InstallPathsError("the home directory could not be determined") from e


def expand_tilde(raw: str, home: str) -> str:
    """Replace a leading `~/` or a bare `~` with `home`; anything else as-is."""
    if raw.startswith("~/"):
        return os.path.join(home, raw[2:])
    if raw == "~":
        return home
    return raw


def install_file_path(home: str) -> str:
    """`<home>/.trusty-tools/install.toml`, anchored at home, never at the config dir."""
    return os.path.join(home, TOOLS_DIR_NAME, INSTALL_FILE_NAME)


def read_install_file(home: str) -> Optional[dict]:
    """The parsed install file; None when absent; InstallPathsError when broken."""
    path = install_file_path(home)
    try:
        with open(path, "rb") as f:
            raw = f.read()
    except FileNotFoundError:
        return None
    except OSError as e:
        raise InstallPathsError(f"cannot read {path!r}: {e}") from e
    try:
        text = raw.decode("utf-8")
        try:
            import tomllib
        except ImportError:  # Python < 3.11 (macOS /usr/bin/python3 is 3.9)
            data = _flat_toml(text)
        else:
            data = tomllib.loads(text)
    except ValueError as e:  # UnicodeDecodeError and TOMLDecodeError are ValueErrors
        raise InstallPathsError(f"cannot parse {path!r}: {e}") from e
    for key, value in data.items():
        if key not in KEYS:
            raise InstallPathsError(f"cannot parse {path!r}: unknown field `{key}`, expected one of {list(KEYS)}")
        if not isinstance(value, str):
            raise InstallPathsError(f"cannot parse {path!r}: `{key}` must be a string path")
    return data


_FLAT_LINE = re.compile(r'^\s*([A-Za-z0-9_-]+)\s*=\s*("(?:[^"\\]|\\.)*"|\'[^\']*\')\s*(?:#.*)?$')


def _flat_toml(text: str) -> dict:
    """Below Python 3.11: the flat `key = "string"` subset the Rust writer emits; anything else is a ValueError."""
    data = {}
    for n, line in enumerate(text.splitlines(), 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        m = _FLAT_LINE.match(line)
        if not m:
            raise ValueError(f"line {n}: only `key = \"string\"` lines are read below Python 3.11")
        key, val = m.groups()
        if key in data:
            raise ValueError(f"line {n}: duplicate key `{key}`")
        data[key] = json.loads(val) if val.startswith('"') else val[1:-1]
    return data


def _absolute(key: str, origin: str, path: str) -> ResolvedPath:
    if not path:
        raise InstallPathsError(f"{key} from {origin} is empty; give an absolute path")
    if not os.path.isabs(path):
        raise InstallPathsError(f"{key} from {origin} must be an absolute path, got {path!r}")
    return ResolvedPath(path, origin)


def _early(key: str, env: EnvLookup, home: str, overrides: Mapping[str, str]) -> Optional[ResolvedPath]:
    """The override and env rungs; None when neither decides `key`."""
    if overrides.get(key) is not None:
        return _absolute(key, FLAG, expand_tilde(overrides[key].strip(), home))
    raw = env(ENV_VARS[key])
    if raw is not None and raw.strip():
        return _absolute(key, ENV, expand_tilde(raw.strip(), home))
    return None


def _late(key: str, file: Optional[dict], home: str) -> Optional[ResolvedPath]:
    """The file and default rungs; None only for a key with no default."""
    if file is not None and key in file:
        return _absolute(key, INSTALL_FILE, file[key])
    if key == PROJECTS_ROOT:
        return ResolvedPath(os.path.join(home, DEFAULT_WORKSPACE_DIR), DEFAULT)
    if key == CONFIG_DIR:
        return ResolvedPath(home, DEFAULT)
    return None


def _lookup(env: Optional[Mapping[str, str]]) -> EnvLookup:
    source = os.environ if env is None else env
    return source.get


def resolve_with(env: EnvLookup, home: str, overrides: Optional[Mapping[str, str]] = None) -> InstallPaths:
    """All four keys against an injected env lookup and home (mod.rs:275)."""
    overrides = overrides or {}
    cache: list = []
    out = {}
    for key in KEYS:
        hit = _early(key, env, home, overrides)
        if hit is None:
            if not cache:
                cache.append(read_install_file(home))
            hit = _late(key, cache[0], home)
        out[key] = hit
    return InstallPaths(**out)


def resolve(overrides: Optional[Mapping[str, str]] = None, env: Optional[Mapping[str, str]] = None) -> InstallPaths:
    """All four keys from the process env (or `env`) and its home."""
    return resolve_with(_lookup(env), home_dir(env), overrides)


def resolve_key(key: str, env: Optional[Mapping[str, str]] = None) -> Optional[ResolvedPath]:
    """One key, so a bad value under another key cannot change it (mod.rs:392)."""
    if key not in KEYS:
        raise ValueError(f"unknown install-paths key: {key}")
    home = home_dir(env)
    lookup = _lookup(env)
    hit = _early(key, lookup, home, {})
    return hit if hit is not None else _late(key, read_install_file(home), home)


def projects_root(env: Optional[Mapping[str, str]] = None) -> str:
    """The projects root; raises InstallPathsError rather than default on a broken file."""
    return resolve_key(PROJECTS_ROOT, env).path  # type: ignore[union-attr]  # always has a default


def config_dir(env: Optional[Mapping[str, str]] = None) -> str:
    """The config dir; raises InstallPathsError rather than default on a broken file."""
    return resolve_key(CONFIG_DIR, env).path  # type: ignore[union-attr]  # always has a default


def tools_dir(env: Optional[Mapping[str, str]] = None) -> str:
    """`<config_dir>/.trusty-tools`."""
    return os.path.join(config_dir(env), TOOLS_DIR_NAME)


def managed_claude_config_dir(env: Optional[Mapping[str, str]] = None) -> str:
    """`<config_dir>/.trusty-tools/trusty-mpm/claude-config`, tm's managed CLAUDE_CONFIG_DIR."""
    return os.path.join(tools_dir(env), "trusty-mpm", "claude-config")


def main(argv: list) -> int:
    try:
        paths = resolve()
    except InstallPathsError as e:
        print(f"install_paths: {e}", file=sys.stderr)
        return 1
    rows = {k: (None if getattr(paths, k) is None else getattr(paths, k).__dict__) for k in KEYS}
    if "--json" in argv:
        print(json.dumps(rows, indent=2))
    else:
        for k, v in rows.items():
            print(f"{k}\t{'-' if v is None else v['path']}\t{'-' if v is None else v['source']}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
