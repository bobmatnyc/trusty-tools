"""Runs datg.py or ilio.py with the clock frozen at $FROZEN_EPOCH (parity tests only).

Usage: frozen_py.py <scripts dir> <datg|ilio> <args...>

Why: `prog Nh ago`, `STALE`, `session.written_at` and log timestamps read the wall
clock, so a byte comparison against the Rust port needs both sides on one instant.
What: patches `time.time`, `time.gmtime` and the `datetime` class that datg, il_remind
and ilio imported, then calls the script's `main(argv)` and exits with its code.
"""
import datetime as _dt
import os
import sys
import time

scripts, prog, argv = sys.argv[1], sys.argv[2], sys.argv[3:]
epoch = float(os.environ["FROZEN_EPOCH"])
sys.path.insert(0, scripts)


class Frozen(_dt.datetime):
    @classmethod
    def now(cls, tz=None):
        d = _dt.datetime.fromtimestamp(epoch, _dt.timezone.utc)
        return d if tz else d.replace(tzinfo=None)


_gmtime = time.gmtime
time.time = lambda: epoch
time.gmtime = lambda secs=None: _gmtime(epoch if secs is None else secs)

import datg  # noqa: E402
import il_remind  # noqa: E402

datg.datetime = Frozen
il_remind.datetime = Frozen
if prog == "ilio":
    import ilio  # noqa: E402

    ilio.datetime = Frozen
    sys.exit(ilio.main(argv))
sys.exit(datg.main(argv))
