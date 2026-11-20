"""Regenerate the G0b IL auto-apply parity golden from the prototype `datg.py`.

Why: the Rust IL apply must leave the bytes `ilio.py apply` leaves through
`datg.write_ops`; the golden must come from the Python, never the Rust.
What: loads `datg.py` by path from the prototype directory given as the one
argument, pins its clock to 2026-10-21T10:00Z (as regen_writer.py does),
copies `../pm-small/` into a tempdir as `.trusty-mpm/datg/pm-small/` and runs
the ops ilio builds for the block in `block.txt` (session next, close, act).
Writes `state.json` (the final snapshot) and `log.tail` / `archive.tail` (the
bytes the write appended).
Test: `il_apply_matches_datg_bytes` in tests/il_packet.rs.

Regenerate (from the repository root):
    python3 -B -I crates/trusty-architect/tests/fixtures/ledger/il/regen_il.py <prototype-dir>
"""
import importlib.util
import os
import shutil
import sys
import tempfile
from datetime import datetime, timezone

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
START = datetime(2026, 10, 21, 10, 0, tzinfo=timezone.utc)
if len(sys.argv) != 2:
    sys.exit("usage: regen_il.py <prototype-dir>")
os.environ["DATG_NOTIFY_CMD"] = "true"
os.environ["DATG_SESSION"] = "pm-small"
spec = importlib.util.spec_from_file_location("datg", os.path.join(sys.argv[1], "datg.py"))
datg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(datg)


class FixedDatetime(datetime):
    @classmethod
    def now(cls, tz=None):
        return START


class FixedTime:
    @staticmethod
    def time():
        return START.timestamp()


def fixed_now_iso():
    return START.strftime("%Y-%m-%dT%H:%MZ")


for mod in [datg] + [sys.modules[m] for m in ("datg_store", "datg_ops", "datg_log", "datg_read") if m in sys.modules]:
    for name, fixed in (("datetime", FixedDatetime), ("time", FixedTime), ("now_iso", fixed_now_iso)):
        if hasattr(mod, name):
            setattr(mod, name, fixed)
    for name, bar in (("SIZE_WARN", 8 * 1024), ("SIZE_REFUSE", 16 * 1024)):
        if hasattr(mod, name):
            setattr(mod, name, bar)

# The ops ilio.apply_block builds for block.txt, less the `pulse` op (pct),
# which the Rust apply reports as skipped.
OPS = [
    {"op": "session", "next": "run the gates"},
    {"op": "close", "id": "xa-A01", "outcome": "sha abc1234"},
    {"op": "act", "who": "pm-small", "what": "merged the parser", "owner_words": "",
     "evidence": "PR #1", "goal": "xa-G01", "refs": "crates/trusty-architect"},
]

with tempfile.TemporaryDirectory() as tmp:
    store = os.path.join(tmp, ".trusty-mpm", "datg", "pm-small")
    shutil.copytree(os.path.join(HERE, "..", "pm-small"), store)
    read = lambda n: open(os.path.join(store, n), "rb").read() if os.path.exists(os.path.join(store, n)) else b""
    log0, arch0 = read("log.jsonl"), read("archive.jsonl")
    datg.write_ops(tmp, OPS)
    for name, data in (("state.json", read("state.json")), ("log.tail", read("log.jsonl")[len(log0):]),
                       ("archive.tail", read("archive.jsonl")[len(arch0):])):
        with open(os.path.join(HERE, name), "wb") as f:
            f.write(data)
print("wrote state.json, log.tail, archive.tail")
