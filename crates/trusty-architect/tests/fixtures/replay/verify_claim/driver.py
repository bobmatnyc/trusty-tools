"""Replay driver for verify-claim.py (#12028 P1g): runs its main() with the config root at the working directory.

Why: the script reads `local/architect.json` under its own checkout (the parent of `scripts/`), outside the
sandbox. This file is copied beside the frozen modules at record time as vc_driver.py; it points
`architect_config.load()` at the working directory (the sandbox root), runs `main()` unchanged, and prints an
uncaught exception as its traceback's last line, as the port does. examples/replay_port.rs mirrors it as
`replay_port verify_claim`.
Usage: driver.py EVENT
"""
import importlib.util
import os
import sys
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import architect_config  # noqa: E402

_load = architect_config.load


def _load_here(root=None, environ=None):
    return _load(os.getcwd() if root is None else root, environ)


architect_config.load = _load_here
_spec = importlib.util.spec_from_file_location("verify_claim", os.path.join(HERE, "verify-claim.py"))
vc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(vc)
try:
    rc = vc.main(sys.argv[1:])
except Exception as e:  # noqa: BLE001 - the traceback's last line, as the port prints it
    sys.stdout.flush()
    sys.stderr.write("".join(traceback.format_exception_only(type(e), e)))
    rc = 1
sys.exit(rc)
