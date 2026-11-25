"""trusty-tools#11629: concurrent token_watch writers lose no offsets or fired keys.

Why: the Rust port and this watcher share inbox/token-watch-state.json.
What: N processes each run a locked read-modify-write that adds its own
offset and fired key; every key survives. A held <state>.lock blocks
state_lock until the deadline, and save_state names a per-process temp file.
Test: scripts/test_token_watch_lock.py
"""
import multiprocessing
import os
import fcntl

import pytest

import importlib.util

HERE = os.path.dirname(os.path.abspath(__file__))
# #10571 S1: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
_spec = importlib.util.spec_from_file_location("token_watch", os.path.join(SCRIPTS, "token_watch.py"))
tw = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tw)

WRITERS, ROUNDS = 6, 15


def _writer(path, w):
    for r in range(ROUNDS):
        with tw.state_lock(path):
            st = tw.load_state(path)
            st["files"][f"w{w}r{r}"] = {"off": r + 1}
            st["fired"][f"w{w}r{r}|ctx_main:200k"] = 1.0
            tw.save_state(st, path)


def test_concurrent_writers_keep_every_offset_and_fired_key(tmp_path):
    path = str(tmp_path / "inbox" / "token-watch-state.json")
    ps = [multiprocessing.Process(target=_writer, args=(path, w)) for w in range(WRITERS)]
    for p in ps:
        p.start()
    for p in ps:
        p.join()
    assert all(p.exitcode == 0 for p in ps)
    st = tw.load_state(path)
    want = {f"w{w}r{r}" for w in range(WRITERS) for r in range(ROUNDS)}
    assert set(st["files"]) == want
    assert {k.split("|")[0] for k in st["fired"]} == want
    assert not [n for n in os.listdir(tmp_path / "inbox") if n.endswith(".tmp")]


def test_held_lock_refuses_the_write(tmp_path):
    path = str(tmp_path / "token-watch-state.json")
    with open(path + ".lock", "a") as held:
        fcntl.flock(held.fileno(), fcntl.LOCK_EX)
        with pytest.raises(TimeoutError):
            with tw.state_lock(path, wait=0.1):
                tw.save_state(tw.new_state(), path)
    assert not os.path.exists(path)
