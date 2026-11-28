"""Replay driver for architect_config.py (#12028): loads the config under ROOT and prints every field.

Why: architect_config.py is a library with no command line, so a recording needs a caller. This file is copied
beside the frozen module at record time; examples/replay_port.rs mirrors it as `replay_port architect_config`.
Usage: driver.py ROOT   (the environment is os.environ, as every caller passes it)
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import architect_config as ac  # noqa: E402


def main(argv):
    if len(argv) != 1:
        print("usage: driver.py ROOT", file=sys.stderr)
        return 2
    print(f"default_path: {ac.default_path(argv[0])}")
    print(f"default_claude_config_dir: {ac.default_claude_config_dir()}")
    try:
        c = ac.load(argv[0])
    except ac.ConfigError as e:
        print(f"ConfigError: {e}", file=sys.stderr)
        return 1
    print(f"path: {c.path}")
    print(f"owner_name: {c.owner_name}")
    print(f"memory_palace: {c.memory_palace}")
    print(f"dashboard_port: {c.dashboard_port}")
    print(f"tailnet_url: {c.tailnet_url}")
    for name, r in c.repos.items():
        print(f"repo {name}: slug={r.slug} gh_config_dir={r.gh_config_dir} checkout={r.checkout}")
    for name, v in c.caps.items():
        print(f"cap {name}: {v!r}")
    print(f"unknown: {' '.join(c.unknown)}")
    print(f"projects_root: {c.projects_root}")
    print(f"claude_config_dir: {c.claude_config_dir}")
    print(f"poll_interval_s: {c.poll_interval_s}")
    return 0


sys.exit(main(sys.argv[1:]))
