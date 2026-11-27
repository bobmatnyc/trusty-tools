"""Replay driver for il_timeline.py (#12028): prints a Timeline and its answers for argv.

Why: il_timeline.py is a library with no command line, so a recording needs a caller. This file is copied
beside the frozen module at record time; examples/replay_port.rs mirrors it as `replay_port il_timeline`.
What: load({PROJECT: DIR, ...}), then every incarnation (sorted by key), every newest key (sorted by id),
and at(project, epoch) for each project (sorted) and the unknown project "-", for each EPOCH.
An exception from load() prints its class only (exit 1).
Usage: driver.py PROJECT=DIR... [-- EPOCH...]
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import il_timeline as it  # noqa: E402


def key(k):
    return "None" if k is None else f"{k[0]} {k[1]!r}"


def main(argv):
    cut = argv.index("--") if "--" in argv else len(argv)
    pairs = [a.split("=", 1) for a in argv[:cut]]
    if not pairs or any(len(p) != 2 for p in pairs):
        print("usage: driver.py PROJECT=DIR... [-- EPOCH...]", file=sys.stderr)
        return 2
    dirs = dict(pairs)
    epochs = [float(a) for a in argv[cut + 1:]]
    try:
        tl = it.load(dirs)
    except Exception as e:  # noqa: BLE001 - the class is the observable
        print(f"load raised {type(e).__name__}", file=sys.stderr)
        return 1
    for k in sorted(tl.inc):
        v = tl.inc[k]
        print(f"inc {key(k)} close={v['close']!r} title={v['title']!r}")
    for i in sorted(tl.newest):
        print(f"newest {i} {key(tl.newest[i])}")
    for p in sorted(dirs) + ["-"]:
        for e in epochs:
            print(f"at {p} {e!r} {key(tl.at(p, e))}")
    return 0


sys.exit(main(sys.argv[1:]))
