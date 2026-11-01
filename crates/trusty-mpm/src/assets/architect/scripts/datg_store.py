"""datg_store.py: the datg state files: constants, paths, load, atomic commit, lock.

Why: datg.py passed the 500-SLOC cap; #10161 P3 split it by section, bodies moved verbatim.
What: constants and RULES, DatgError, the per-PM directory (`_SESSION` lives here and
datg.py main() sets it from --session), load_state, commit (with its one automatic prune),
reconcile_archive and Locked, plus the log readers and prune policy commit() falls back on.
datg.py re-exports every name.
Test: tests/test_datg.py
"""
import fcntl
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone

DEFAULT_BLOCK_AFTER = "2026-10-12T13:30Z"
PREFIXES = ("G", "D", "A", "T")
STATUS = {
    "G": ("active", "done", "parked"),
    "D": ("open", "answered", "done"),
    "A": ("open", "answered", "done"),
    "T": ("open", "running", "done"),
}
EVENTS = ("open", "escalated", "answered", "done", "drift", "reprio", "update")
CODE_RE = re.compile(r"^[a-z][a-z0-9]{1,7}$")
ID_RE = re.compile(r"^([a-z][a-z0-9]{1,7})-([GDAT])([0-9a-f]{2})$")
TITLE_MAX = 120


def _size_cap(name, default):
    """The `caps.<name>` byte bar from local/architect.json, else `default` (#10571).

    Why: the state.json size bar is an owner decision (D74: 64 KB everywhere), so
    it is an instance cap, not a shipped constant. What: reads architect_config beside this
    file; a missing loader, an unusable config or a non-positive value keeps `default`.
    Test: tests/test_datg_size_cap.py
    """
    try:
        import architect_config  # sibling module; absent in a bare checkout of datg alone
        value = architect_config.load().caps.get(name)
    except Exception as e:  # an unusable config never stops the ledger; the shipped bar applies
        sys.stderr.write(f"datg: caps.{name} not read ({type(e).__name__}); using {default} B\n")
        return default
    if value is None:
        return default
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value <= 0:
        sys.stderr.write(f"datg: caps.{name} must be a positive number of bytes; using {default} B\n")
        return default
    return int(value)


SIZE_REFUSE = _size_cap("datg_size_refuse", 64 * 1024)   # owner ruling D74
SIZE_WARN = _size_cap("datg_size_warn", SIZE_REFUSE // 2)  # 32 KB by default
SESSION_MAX = 2048
SESSION_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
WHO = ("owner", "architect", "ci", "agent", "other")
VAGUE = {"latest", "main", "master", "head", "current", "tbd", "unknown", "?", "n/a", "none", "-"}
_SESSION = None  # set by --session; forces the per-PM directory

RULES = (
    "DATG rules (state: .trusty-mpm/datg/state.json). You are the only writer: one "
    "`python3 {py} <verb>` call at the end of EVERY turn. "
    "1) Every Agent dispatch prompt starts 'Goal: {code}-Gxx'; no goal id, no dispatch. "
    "2) Handbacks end with: Goals (rank order), Open D/A (ids), Next. "
    "3) Owner questions are D, owner actions are A (options, recommendation first); cite ids, never repeat. "
    "4) Before dispatch: does it serve the top active goal? If none, record a D 'drift:'. "
    "5) Unsettled D/A: set esc=architect and SendMessage tm-architect. "
    "6) Relayed answer: write it in, act, close with a verified outcome. "
    "Never change goal ranks. No secret values."
)


class DatgError(Exception):
    """Invalid input or unwritable state; the CLI maps it to exit 2."""


def now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%MZ")


def parse_iso(s):
    return datetime.strptime(s, "%Y-%m-%dT%H:%MZ").replace(tzinfo=timezone.utc)


def _tmux_session():
    pane = os.environ.get("TMUX_PANE")
    if not pane:
        return None
    try:
        r = subprocess.run(["tmux", "display-message", "-p", "-t", pane, "#{session_name}"],
                           capture_output=True, text=True, timeout=5)
        return r.stdout.strip() if r.returncode == 0 and r.stdout.strip() else None
    except (OSError, subprocess.SubprocessError):
        return None


def per_pm_session(project):
    """The per-PM directory name in use, or None for the legacy single directory."""
    base = os.path.join(project, ".trusty-mpm", "datg")
    if _SESSION:
        if not SESSION_RE.match(_SESSION):
            raise DatgError(f"bad session name {_SESSION!r}")
        return _SESSION
    env = os.environ.get("DATG_SESSION")
    if env and SESSION_RE.match(env) and os.path.isfile(os.path.join(base, env, "state.json")):
        return env
    try:
        has_subs = any(os.path.isfile(os.path.join(base, e, "state.json")) for e in os.listdir(base)
                       if os.path.isdir(os.path.join(base, e)))
    except OSError:
        return None
    if has_subs:
        t = _tmux_session()
        if t and SESSION_RE.match(t) and os.path.isfile(os.path.join(base, t, "state.json")):
            return t
    return None


def datg_dir(project):
    base = os.path.join(project, ".trusty-mpm", "datg")
    s = per_pm_session(project)
    return os.path.join(base, s) if s else base


def paths(project):
    d = datg_dir(project)
    return d, os.path.join(d, "state.json"), os.path.join(d, "log.jsonl")


def load_state(project):
    """Return the parsed state; raise DatgError when missing or malformed."""
    _, sp, _ = paths(project)
    try:
        with open(sp, encoding="utf-8") as f:
            st = json.load(f)
    except FileNotFoundError:
        raise DatgError("state.json missing; run init")
    except (OSError, ValueError) as e:
        raise DatgError(f"state.json malformed ({e}); refusing to write")
    ok = (isinstance(st, dict) and CODE_RE.match(str(st.get("code", "")))
          and isinstance(st.get("v"), int) and isinstance(st.get("next"), dict)
          and all(isinstance(st.get(k), list) for k in ("goals", "items", "tasks"))
          and isinstance(st.get("session", {}), dict))
    if not ok:
        raise DatgError("state.json malformed (bad shape); refusing to write")
    return st


def _atomic_write(path, text):
    tmp = f"{path}.tmp.{os.getpid()}"
    try:
        with open(tmp, "w", encoding="utf-8") as f:
            f.write(text)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    except BaseException:
        if os.path.exists(tmp):
            os.unlink(tmp)
        raise


def _dump(st):
    return json.dumps(st, separators=(",", ":"), ensure_ascii=False) + "\n"


def _jl(rec):
    return json.dumps(rec, separators=(",", ":"), ensure_ascii=False) + "\n"


def _size(path):
    try:
        return os.path.getsize(path)
    except OSError:
        return 0


def _largest(st, n=3):
    objs = [(len(json.dumps(o, ensure_ascii=False).encode()), o.get("id", "?"))
            for k in ("goals", "items", "tasks") for o in st[k]]
    return ", ".join(f"{i} ({b} B)" for b, i in sorted(objs, reverse=True)[:n])


def commit(project, st, events, archived=()):
    """Bump v, stamp session.written_*, archive closed items, replace state.json, append log lines.

    archived: [(kind, item)] already removed from st. Order: archive line(s) first, then the
    state replace (a crash between leaves an orphan archive line, cv > state v, dropped later).
    Refuses (DatgError, nothing written) when the result is > SIZE_REFUSE and bigger than before.
    """
    d, sp, lp = paths(project)
    old = _size(sp)
    archived = list(archived)
    st["v"] += 1
    ses = st.setdefault("session", {})
    ses["written_at"] = now_iso()
    ses["written_epoch"] = int(time.time())
    text = _dump(st)
    if len(text.encode()) > SIZE_REFUSE and len(text.encode()) > old:
        archived += prune_state(st, answered_times(project))
        text = _dump(st)
        if len(text.encode()) > SIZE_REFUSE and len(text.encode()) > old:
            raise DatgError(f"state.json would be {len(text.encode())} B (> {SIZE_REFUSE} B bar); "
                            f"largest: {_largest(st)}; close or shorten items")
    if archived:
        t = now_iso()
        buf = "".join(_jl({"archived_at": t, "closed_at": t, "kind": k, "item": o, "cv": st["v"]})
                      for k, o in archived)
        fd = os.open(os.path.join(d, "archive.jsonl"), os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
        try:
            os.write(fd, buf.encode())
            os.fsync(fd)
        finally:
            os.close(fd)
    _atomic_write(sp, text)
    with open(lp, "a", encoding="utf-8") as f:
        t = now_iso()
        for ev in events:
            rec = {"t": t, "id": ev["id"], "ev": ev["ev"]}
            if ev.get("note"):
                rec["note"] = ev["note"]
            f.write(_jl(rec))
        if len(text.encode()) > SIZE_WARN >= old:
            f.write(_jl({"ts": t, "session": os.environ.get("DATG_SESSION", ""), "kind": "size_warning",
                         "text": f"state.json {len(text.encode())} B > {SIZE_WARN} B bar; largest: {_largest(st)}"}))
        f.flush()
        os.fsync(f.fileno())


def reconcile_archive(project):
    """Drop archive lines whose commit never landed (cv > state v). Caller holds the lock."""
    d, sp, _ = paths(project)
    ap = os.path.join(d, "archive.jsonl")
    try:
        with open(sp, encoding="utf-8") as f:
            v = json.load(f)["v"]
        with open(ap, encoding="utf-8") as f:
            lines = f.readlines()
    except (OSError, ValueError, KeyError, TypeError):
        return
    keep = []
    for ln in lines:
        try:
            cv = json.loads(ln).get("cv")
        except ValueError:
            cv = None
        if not (isinstance(cv, int) and cv > v):
            keep.append(ln)
    if len(keep) != len(lines):
        _atomic_write(ap, "".join(keep))


class Locked:
    """flock on <datg>/.lock for the read-modify-write span; reconciles the archive on entry."""

    def __init__(self, project):
        self.project = project

    def __enter__(self):
        self.d = datg_dir(self.project)
        os.makedirs(self.d, exist_ok=True)
        self.f = open(os.path.join(self.d, ".lock"), "w")
        fcntl.flock(self.f, fcntl.LOCK_EX)
        reconcile_archive(self.project)
        return self

    # #10570: pm-guard read the one-letter star argument as a secret-file glob.
    def __exit__(self, *exc):
        self.f.close()




# ---- log readers and the prune policy commit() falls back on --------------------

def parse_ts(s):
    """Tolerant timestamp parse (minute or second precision, Z suffix); None when unreadable."""
    for fmt in ("%Y-%m-%dT%H:%MZ", "%Y-%m-%dT%H:%M:%SZ"):
        try:
            return datetime.strptime(str(s), fmt).replace(tzinfo=timezone.utc)
        except ValueError:
            pass
    return None


def log_rec(line):
    """Normalise both log shapes (t/ev and ts/kind): (datetime|None, kind, record) or None."""
    try:
        r = json.loads(line)
    except ValueError:
        return None
    if not isinstance(r, dict):
        return None
    return parse_ts(r.get("t") or r.get("ts")), r.get("ev") or r.get("kind") or "", r


def read_log(project, tail_bytes=None):
    _, _, lp = paths(project)
    try:
        with open(lp, "rb") as f:
            if tail_bytes and os.fstat(f.fileno()).st_size > tail_bytes:
                f.seek(-tail_bytes, 2)
                f.readline()
            data = f.read().decode("utf-8", "replace")
    except OSError:
        return []
    return [x for x in map(log_rec, data.splitlines()) if x]


def answered_times(project):
    """{id: datetime of the last answered event}; ids missing here are treated as old."""
    out = {}
    for t, kind, r in read_log(project):
        if kind == "answered" and t and r.get("id"):
            out[r["id"]] = t
    return out


def prune_state(st, times, hours=24, require_outcome=False, now=None):
    """Remove done items/tasks and answered items older than `hours`; return [(kind, obj)]."""
    now = now or datetime.now(timezone.utc)
    out, keep = [], []
    for o in st["items"]:
        t = times.get(o.get("id"))
        old = t is None or (now - t).total_seconds() >= hours * 3600
        if o.get("status") == "done" or (o.get("status") == "answered" and old
                                         and (o.get("outcome") or not require_outcome)):
            out.append(("item", o))
        else:
            keep.append(o)
    st["items"] = keep
    out += [("task", o) for o in st["tasks"] if o.get("status") == "done"]
    st["tasks"] = [o for o in st["tasks"] if o.get("status") != "done"]
    return out


# #10570: last in the module: pm-guard reads this value, then any later backtick, as a run-time 1Password call.
VERB_KEY = "op"  # a batch entry's verb key (the stdin batch protocol); datg.py names it only through this (#10571)
