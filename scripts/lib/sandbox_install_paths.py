#!/usr/bin/env python3
"""sandbox_install_paths.py — the install.toml a sandbox launcher writes or admits (#11821 P6a).

Why: a sandbox daemon gets a cleared env, so `<dir>/home/.trusty-tools/install.toml`
is the only rung that can move its projects root or config dir (the daemon's env
allowlist, daemon_sandbox.rs ENV_ALLOWLIST, admits neither env var). That same
file could also point a sandbox at the operator's real config dir, which
`HOME=<dir>/home` alone was meant to rule out.
What: `check --root DIR --home H [--projects-root P] [--config-dir C]`. With
values, each must be absolute, hold no `"`, `\\` or control character, and sit
under DIR after symlinks resolve; nothing is written. Without values, an existing
`H/.trusty-tools/install.toml` is read with install_paths.read_install_file (a
broken file refuses) and each value it names must pass the same test. Exit 0
admits, 1 refuses with one `refused:` line on stderr.
`write --home H [--projects-root P] [--config-dir C]` creates each named directory
and `H/.trusty-tools`, then writes install.toml through a temp file and a rename.
Test: scripts/sandbox_install_paths_selftest.sh.
"""
from __future__ import annotations

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from install_paths import (  # noqa: E402
    CONFIG_DIR, PROJECTS_ROOT, InstallPathsError, install_file_path, read_install_file)


def refuse_reason(key: str, value: str, root: str) -> str:
    """Empty when `value` is admissible under `root`; else why not."""
    if not isinstance(value, str) or not os.path.isabs(value):
        return f"{key} must be an absolute path, got {value!r}"
    if any(c in value for c in '"\\') or any(ord(c) < 32 for c in value):
        return f"{key} holds a quote, backslash or control character: {value!r}"
    real_root = os.path.realpath(root)
    real = os.path.realpath(value)
    if real != real_root and not real.startswith(real_root.rstrip("/") + "/"):
        return f"{key} {value!r} resolves to {real!r}, outside the sandbox dir {real_root!r}"
    return ""


def given(a: argparse.Namespace) -> dict[str, str]:
    return {k: v for k, v in ((PROJECTS_ROOT, a.projects_root), (CONFIG_DIR, a.config_dir)) if v}


def check(a: argparse.Namespace) -> int:
    values = given(a)
    if not values:
        try:
            values = read_install_file(a.home) or {}
        except InstallPathsError as e:
            print(f"refused: {e}", file=sys.stderr)
            return 1
    for key, value in values.items():
        why = refuse_reason(key, value, a.root)
        if why:
            print(f"refused: {why}", file=sys.stderr)
            return 1
    return 0


def write(a: argparse.Namespace) -> int:
    values = given(a)
    path = install_file_path(a.home)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    for value in values.values():
        os.makedirs(value, exist_ok=True)
    tmp = f"{path}.{os.getpid()}.tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        # JSON string escaping is valid TOML basic-string escaping for these values.
        f.write("".join(f"{k} = {json.dumps(v)}\n" for k, v in values.items()))
    os.replace(tmp, path)
    return 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("cmd", choices=("check", "write"))
    ap.add_argument("--root")
    ap.add_argument("--home", required=True)
    ap.add_argument("--projects-root")
    ap.add_argument("--config-dir")
    a = ap.parse_args(argv)
    if a.cmd == "check":
        if not a.root:
            ap.error("check needs --root")
        return check(a)
    return write(a)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
