"""The S2 shell scripts with no live test must at least parse (bash -n).

#10571 S2: rc-restore.sh and rc-disconnect.sh drive real panes and fleet-restart.sh kills the tmux server, so
they get no live run here. rc-lib.sh, which they source, ships with S1 together with its own test.
Test: python3 -m pytest -q tests/test_shell_parse.py
"""
import os
import subprocess

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")


@pytest.mark.parametrize("name", ["rc-restore.sh", "rc-disconnect.sh", "fleet-restart.sh",
                                  "architect-start.sh", "poll-supervise.sh"])
def test_shell_scripts_parse(name):
    assert subprocess.run(["bash", "-n", os.path.join(SCRIPTS, name)]).returncode == 0
