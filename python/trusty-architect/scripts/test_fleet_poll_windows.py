"""Window renaming in fleet-poll.py: tm-* windows are named after their session."""
import importlib.util
import os

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(HERE, "fleet-poll.py"))
fp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fp)


def test_mismatch_is_planned_and_match_is_left_alone():
    listing = "tm-a\t0\t2.1.287\ntm-b\t0\ttm-b\ntm-a\t1\ttm-a\n"
    assert fp.plan_window_renames(listing) == [("tm-a", "0", "2.1.287")]


def test_non_tm_sessions_are_ignored():
    assert fp.plan_window_renames("work\t0\tzsh\nmain\t1\t2.1.287\n") == []


def test_runner_renames_with_exact_target_and_no_send_keys():
    calls = []

    def fake(*args):
        calls.append(args)
        return "tm-architect\t0\t2.1.287\nother\t0\tzsh\n" if args[0] == "list-windows" else ""

    assert fp.rename_windows(runner=fake) == 1
    assert calls[1] == ("rename-window", "-t", "=tm-architect:0", "tm-architect")
    assert all(c[0] in ("list-windows", "rename-window") for c in calls)
