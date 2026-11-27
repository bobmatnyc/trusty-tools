"""Replay driver for ids36.py (#12029 P2a): its command line and its library functions.

Why: the script's command line reaches only `next`; `enc` bounds and `dec` refusals are library
behaviour the dashboard and ledger ports depend on. This file is copied beside the frozen script at
record time as i36_driver.py. `cli ARGS...` runs the script's `__main__` block unchanged with those
arguments; `table` prints the constants; `enc N...`, `dec S...` and `next S...` call the function on
each argument and print its result or its ValueError. examples/replay_port.rs mirrors it as
`replay_port ids36`.
Usage: driver.py cli ARGS... | table | enc N... | dec S... | next S...
"""
import os
import runpy
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import ids36  # noqa: E402

FUNCS = {"enc": lambda a: ids36.enc(int(a)), "dec": ids36.dec, "next": ids36.next_digits}


def main(argv):
    if argv[:1] == ["cli"]:
        sys.argv = ["ids36.py"] + argv[1:]
        runpy.run_path(os.path.join(HERE, "ids36.py"), run_name="__main__")
        return 0
    if argv == ["table"]:
        print(f"DIGITS {ids36.DIGITS}\nBASE {ids36.BASE}\nSPACE {ids36.SPACE}\nDIGIT_RE {ids36.DIGIT_RE}")
        return 0
    if len(argv) > 1 and argv[0] in FUNCS:
        for a in argv[1:]:
            try:
                print(f"{argv[0]} {a!r} -> {FUNCS[argv[0]](a)!r}")
            except ValueError as e:
                print(f"{argv[0]} {a!r} ValueError: {e}")
        return 0
    sys.stderr.write("usage: driver.py cli ARGS... | table | enc N... | dec S... | next S...\n")
    return 2


sys.exit(main(sys.argv[1:]))
