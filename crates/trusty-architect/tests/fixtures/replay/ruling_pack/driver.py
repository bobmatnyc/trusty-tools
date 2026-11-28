"""Replay driver for ruling_pack.py (#12028 P1i): runs its main() with ROOT at the working directory.

Why: the script takes ROOT from its own location (the parent of `scripts/`), so its default key map
`local/project-keys.json` and its pack directory `local/packs` sit outside the sandbox. This file is
copied beside the frozen script at record time as rp_driver.py. It loads the script unchanged with
`__file__` set to `<cwd>/scripts/ruling_pack.py`, so ROOT is the sandbox root and a pack lands there,
never in a live checkout. It runs `main()` with the program name `ruling_pack.py` and prints an
uncaught exception as its traceback's last line, as the port does. examples/replay_port.rs mirrors
it as `replay_port ruling_pack`.
Usage: driver.py ARGS...
"""
import os
import sys
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
_src = os.path.join(HERE, "ruling_pack.py")
rp = {"__name__": "ruling_pack", "__file__": os.path.join(os.getcwd(), "scripts", "ruling_pack.py")}
with open(_src, encoding="utf-8") as _f:
    exec(compile(_f.read(), _src, "exec"), rp)
sys.argv[0] = "ruling_pack.py"
try:
    rc = rp["main"](sys.argv[1:])
except Exception as e:  # noqa: BLE001 - the traceback's last line, as the port prints it
    sys.stdout.flush()
    sys.stderr.write("".join(traceback.format_exception_only(type(e), e)))
    rc = 1
sys.exit(rc)
