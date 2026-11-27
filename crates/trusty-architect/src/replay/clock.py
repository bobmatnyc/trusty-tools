"""Freezes the wall clock for a replay recording (#12028).

Why: a script that prints or writes the time cannot replay without a diff
unless the recorder and the port read one instant. The script itself is never
edited; the replay probe calls freeze() before it runs the script.
What: freeze(t) pins, to the epoch second t: time.time, time.time_ns, the
default argument of time.localtime, gmtime, ctime, asctime and strftime, and
datetime.datetime.now and utcnow. date.today and datetime.today read
time.time, so they follow. Monotonic clocks and sleep are left alone. The
time zone is pinned by the recorder (TZ in the recorded env), not here.
Test: `a_clocked_script_replays_clean_against_a_stand_in_that_reads_the_seam`.
"""
import datetime as _dt
import time as _time


def freeze(t):
    t = int(t)
    f = float(t)
    lt, gt, ct = _time.localtime, _time.gmtime, _time.ctime
    at, st = _time.asctime, _time.strftime
    _time.time = lambda: f
    _time.time_ns = lambda: t * 1_000_000_000
    _time.localtime = lambda secs=None: lt(f if secs is None else secs)
    _time.gmtime = lambda secs=None: gt(f if secs is None else secs)
    _time.ctime = lambda secs=None: ct(f if secs is None else secs)
    _time.asctime = lambda tt=None: at(lt(f) if tt is None else tt)
    _time.strftime = lambda fmt, tt=None: st(fmt, lt(f) if tt is None else tt)

    class datetime(_dt.datetime):
        @classmethod
        def now(cls, tz=None):
            return cls.fromtimestamp(f, tz)

        @classmethod
        def utcnow(cls):
            return cls.fromtimestamp(f, _dt.timezone.utc).replace(tzinfo=None)

    # A heap type's repr uses __name__, so this keeps `datetime.datetime(...)`.
    datetime.__name__ = "datetime.datetime"
    datetime.__qualname__ = "datetime"
    datetime.__module__ = "datetime"
    _dt.datetime = datetime
