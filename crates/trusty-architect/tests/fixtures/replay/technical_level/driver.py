"""Replay driver for technical_level.py (#12028): prints the library's answers for argv.

Why: technical_level.py is a library with no command line, so a recording needs a caller. This file is copied
beside the frozen module at record time; examples/replay_port.rs mirrors it as `replay_port technical_level`.
Usage: driver.py table | value V... | read ROOT
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import dashboard_store as ds  # noqa: E402
import technical_level as tl  # noqa: E402


def main(argv):
    cmd, rest = (argv[0], argv[1:]) if argv else ("", [])
    if cmd == "table" and not rest:
        print("LEVELS", " ".join(tl.LEVELS))
        print("DEFAULT", tl.DEFAULT)
        for level in tl.LEVELS:
            print(tl.line(level))
        return 0
    if cmd == "value" and rest:
        for v in rest:
            print(f"{v!r} normalize={tl.normalize(v)} below_engineer={tl.below_engineer(v)}")
            print(tl.line(v))
        return 0
    if cmd == "read" and len(rest) == 1:
        try:
            level = tl.read(rest[0])
        except ds.StoreError as e:
            print(f"read raised {type(e).__name__}", file=sys.stderr)
            return 1
        print(f"read {level}")
        print(tl.line(level))
        return 0
    print("usage: driver.py table | value V... | read ROOT", file=sys.stderr)
    return 2


sys.exit(main(sys.argv[1:]))
