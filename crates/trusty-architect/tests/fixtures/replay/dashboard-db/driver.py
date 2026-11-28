"""Replay driver for dashboard-db.py (#12029 P2g): the frozen script's own command line, with the checkout at the sandbox root.

Why: dashboard-db.py and the modules it imports read the checkout beside the script (local/owner-queue, schemas/dashboard,
local/architect.json, inbox/questions-rc.json, local/il), which is outside the sandbox the recorder allows. This file is
copied beside the frozen dashboard-db.py and its imports at record time as ddb_driver.py. It points each of those
defaults at the working directory (the sandbox root, which stands for the checkout), sets argparse's program name to
dashboard-db.py, and runs the script's main() on the given argv. It changes none of the script's text.
examples/replay_port.rs runs the port's command line as `replay_port dashboard-db`.
Usage: ddb_driver.py [dashboard-db.py ARGS...]
"""
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
CWD = os.getcwd()
import architect_config  # noqa: E402

architect_config._SCRIPTS = os.path.join(CWD, "scripts")
import dashboard_schema as dsch  # noqa: E402

dsch.SCHEMA_DIR = os.path.join(CWD, "schemas", "dashboard")
import dashboard_store as ds  # noqa: E402

ds.DEFAULT_ROOT = os.path.join(CWD, "local", "owner-queue")
import il_remind  # noqa: E402

il_remind.__file__ = os.path.join(CWD, "scripts", "il_remind.py")
spec = importlib.util.spec_from_file_location("dashboard_db", os.path.join(HERE, "dashboard-db.py"))
db = importlib.util.module_from_spec(spec)
sys.argv[0] = "dashboard-db.py"
spec.loader.exec_module(db)
db.HERE_DB = os.path.join(CWD, "scripts", "dashboard-db.py")
sys.exit(db.main())
