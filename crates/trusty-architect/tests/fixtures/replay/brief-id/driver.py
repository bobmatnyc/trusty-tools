"""Replay driver for brief-id.py (#12028 P1e): the script's command line, with REPO at the working directory.

Why: brief_ids.py derives REPO, the registry, the briefs directory and the media store from its own
location, which is outside a recording sandbox. This file is copied beside the frozen modules at record
time; it points those paths (the module globals and the function defaults bound at import) at the working
directory (the sandbox root) and then runs brief-id.py as __main__, unchanged.
examples/replay_port.rs mirrors it as `replay_port brief-id`.
Usage: driver.py next | assign <path> [--project P] | find <id> | list | check
"""
import os
import runpy
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import brief_ids as b  # noqa: E402

b.REPO = os.getcwd()
b.BRIEFS = os.path.join(b.REPO, "local", "briefs")
b.REGISTRY = os.path.join(b.BRIEFS, "registry.jsonl")
b.MEDIA_DIR = os.path.join(b.REPO, "local", "owner-queue", "media")
b.read_registry.__defaults__ = (b.REGISTRY,)
b.media_ids.__defaults__ = (b.MEDIA_DIR,)
b.used.__defaults__ = (b.REGISTRY, b.MEDIA_DIR)
b.allocate.__defaults__ = (b.REGISTRY, b.MEDIA_DIR)
b.assign.__defaults__ = ("architect", b.REGISTRY, b.MEDIA_DIR)
b.local_briefs.__defaults__ = (b.BRIEFS,)
b.check.__defaults__ = (b.BRIEFS, b.REGISTRY)

script = os.path.join(HERE, "brief-id.py")
sys.argv[0] = script
runpy.run_path(script, run_name="__main__")
