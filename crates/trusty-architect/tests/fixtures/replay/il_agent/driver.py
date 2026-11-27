"""Replay driver for il_agent.py (#12028 P1f): the script's command line, with HERE at the working directory.

Why: `dispatch` prints `python3 <HERE>/il_agent.py`, where HERE is the script's own directory, outside a
recording sandbox. This file is copied beside the frozen modules at record time; it imports il_agent.py,
points HERE at the working directory (the sandbox root) and runs its main() unchanged.
examples/replay_port.rs mirrors it as `replay_port il_agent`.
Usage: driver.py [--project P] {brief,dispatch,handback} ...
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import il_agent  # noqa: E402

il_agent.HERE = os.getcwd()
sys.exit(il_agent.main())
