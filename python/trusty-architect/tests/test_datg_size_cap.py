"""datg's state.json size bar is the instance cap caps.datg_size_refuse (#10571).

Covers: the shipped default (64 KB refuse, 32 KB warn; owner ruling D74) with no config, a configured bar,
a warn bar that follows a configured refuse bar, and an unusable config keeping the shipped default.
Test: tests/test_datg_size_cap.py
"""
import json
import os
import subprocess
import sys

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
PROBE = "import datg_store as s; print(s.SIZE_REFUSE, s.SIZE_WARN)"


def bars(tmp_path, config=None):
    env = {k: v for k, v in os.environ.items() if k != "ARCHITECT_CONFIG"}
    if config is not None:
        path = tmp_path / "architect.json"
        path.write_text(config if isinstance(config, str) else json.dumps(config), encoding="utf-8")
        env["ARCHITECT_CONFIG"] = str(path)
    else:
        env["ARCHITECT_CONFIG"] = ""
    out = subprocess.run([sys.executable, "-c", PROBE], cwd=SCRIPTS, env=env, capture_output=True, text=True, check=True)
    return tuple(int(x) for x in out.stdout.split())


def test_default_bar_is_64_kb(tmp_path):
    assert bars(tmp_path) == (64 * 1024, 32 * 1024)


def test_configured_bar_and_its_half_warn(tmp_path):
    assert bars(tmp_path, {"caps": {"datg_size_refuse": 16384}}) == (16384, 8192)


def test_explicit_warn_bar_wins(tmp_path):
    assert bars(tmp_path, {"caps": {"datg_size_refuse": 65536, "datg_size_warn": 4096}}) == (65536, 4096)


def test_unusable_config_keeps_the_shipped_bar(tmp_path):
    assert bars(tmp_path, "{not json") == (64 * 1024, 32 * 1024)
