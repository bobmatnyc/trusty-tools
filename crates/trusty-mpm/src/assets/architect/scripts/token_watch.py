#!/usr/bin/env python3
"""token_watch.py: deterministic fleet token watcher (no LLM, stdlib only).

Why: the owner (see #10571): "only inference if action needed". The watcher decides
when token behaviour needs attention; the token-optimizer agent then suggests.
Spec: local/briefs/2026-10-07-token-watch-spec.md.
What: reads Claude Code transcripts incrementally (main and subagent files of
every project directory), de-duplicates turns by message.id, applies the rules
in DEFAULTS, returns alert dicts for fleet-poll.py and appends an hourly
rollup to local/usage/token-hourly.jsonl and never reads or stores message text:
only usage numbers, model, timestamps, ids and the Bash command shape of
polling turns. State: inbox/token-watch-state.json (offsets, counters, fired
fingerprints). Alert dedup here: an exact fingerprint fires once per 24 h, and
a rule's COOLDOWN keeps hourly-bucketed fingerprints from re-firing each hour.
CLI: `token_watch.py report --since 1h [--json]`, `token_watch.py scan --dry-run`.
Layout (#10571 S1, 500-SLOC cap): the rule engine and pure helpers live in
token_watch_rules.py and are re-exported here; this file keeps the paths, the
state file and its lock, transcript discovery and reads, the rollup file and the CLI.
Test: tests/test_token_watch.py, tests/test_token_watch_lock.py
"""
import argparse
import contextlib
import copy
import fcntl
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)
import architect_config  # noqa: E402
from token_watch_rules import (  # noqa: E402,F401  (#10571 S1: split for the 500-SLOC cap; re-exported)
    COOLDOWN, DEFAULTS, FIRED_TTL, LOOKBACK, POLL_RE, PRICE, SESSION_DIR_RE, UNPRICED, ScanRules,
    _load_ctx_bar, ctx_bar, effort_of, family, hour_fp, hour_of, new_state, parse_since, parse_ts,
    pct, prune, rollup_row, split_open_key, thinking_of, turn_units,
)

INBOX = os.environ.get("SUPERVISOR_INBOX_DIR") or os.path.join(ROOT, "inbox")
STATE_PATH = os.path.join(INBOX, "token-watch-state.json")
HOURLY_PATH = os.path.join(ROOT, "local", "usage", "token-hourly.jsonl")
THRESHOLDS_PATH = os.path.join(ROOT, "local", "usage", "token-thresholds.json")
# #11821 P6b: the default dir is the install's managed claude-config dir, read only when a variable is unset.
TRANSCRIPT_BASE = os.environ.get("TRANSCRIPT_BASE") or os.path.join(architect_config.managed_claude_config_dir(), "projects")
SESSIONS_DIR = os.environ.get("SESSIONS_DIR") or os.path.join(architect_config.managed_claude_config_dir(), "sessions")

BUDGET_S = 20.0            # hard cap per scan; resumes next cycle
READ_CAP = 16 * 1024 * 1024
FIRST_TAIL = 4 * 1024 * 1024
# #triage-gate: alert_triage.py runs one `claude -p` SDK call per poll; each is a
# main-session start by design. Marker: entrypoint sdk-cli + this first-user-message
# prefix (alert_triage.INSTRUCTIONS). The cwd (macOS temp dir) is not used: it varies per host.
TRIAGE_ENTRYPOINT = "sdk-cli"
TRIAGE_PREFIX = "You triage alerts from a fleet poller for a supervisor."


def thresholds(path=None):
    t = dict(DEFAULTS)
    try:
        with open(path or THRESHOLDS_PATH) as f:
            t.update(json.load(f))
    except (OSError, ValueError):
        pass
    return t


def load_state(path=None):
    try:
        with open(path or STATE_PATH) as f:
            st = json.load(f)
        for k, v in new_state().items():
            st.setdefault(k, v)
        return st
    except (OSError, ValueError):
        return new_state()


LOCK_WAIT_S = 30.0  # trusty-tools#11629: the Rust watcher's LOCK_WAIT


@contextlib.contextmanager
def state_lock(path=None, wait=LOCK_WAIT_S):
    """trusty-tools#11629: the exclusive flock both watchers hold from load to
    save, on <state>.lock (the Rust port's lock::lock_path). A timeout or an
    open failure raises; the caller never writes unlocked."""
    lp = (path or STATE_PATH) + ".lock"
    os.makedirs(os.path.dirname(lp), exist_ok=True)
    with open(lp, "a") as f:
        deadline = time.monotonic() + wait
        while True:
            try:
                fcntl.flock(f.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise TimeoutError(f"{lp} held by another writer")
                time.sleep(0.025)
        try:
            yield
        finally:
            fcntl.flock(f.fileno(), fcntl.LOCK_UN)


def save_state(st, path=None):
    path = path or STATE_PATH
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = f"{path}.{os.getpid()}.tmp"  # trusty-tools#11629: per-process
    try:
        with open(tmp, "w") as f:
            json.dump(st, f)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    except BaseException:
        with contextlib.suppress(OSError):
            os.remove(tmp)
        raise


# --- discovery ---------------------------------------------------------------

def discover(base, now):
    """(path, kind, project, sid, agent_id) for transcript files modified in the
    lookback. scandir only: a project directory holds thousands of files."""
    cutoff = now - LOOKBACK
    out = []
    try:
        projects = list(os.scandir(base))
    except OSError:
        return out
    for pd in projects:
        if not pd.is_dir():
            continue
        try:
            entries = list(os.scandir(pd.path))
        except OSError:
            continue
        for e in entries:
            try:
                if e.is_file() and e.name.endswith(".jsonl"):
                    if e.stat().st_mtime >= cutoff:
                        out.append((e.path, "main", pd.name, e.name[:-6], ""))
                elif e.is_dir():
                    sub = os.path.join(e.path, "subagents")
                    if os.stat(sub).st_mtime < now - 6 * 3600:
                        continue
                    for a in os.scandir(sub):
                        if a.name.startswith("agent-") and a.name.endswith(".jsonl") \
                                and a.stat().st_mtime >= cutoff:
                            out.append((a.path, "sub", pd.name, e.name, a.name[6:-6]))
            except OSError:
                continue
    return out


def agent_type(path):
    try:
        with open(path[:-len(".jsonl")] + ".meta.json") as f:
            return str(json.load(f).get("agentType") or "subagent")
    except (OSError, ValueError):
        return "subagent"


def session_names(sessions_dir=None):
    """sessionId -> tmux session name from the live-session registry."""
    names = {}
    try:
        for n in os.listdir(sessions_dir or SESSIONS_DIR):
            if not n.endswith(".json"):
                continue
            try:
                with open(os.path.join(sessions_dir or SESSIONS_DIR, n)) as f:
                    d = json.load(f)
                tmux = str(d.get("tmux", ""))
                if d.get("sessionId") and ":" in tmux:
                    names[d["sessionId"]] = tmux.split(":", 1)[0]
            except (OSError, ValueError):
                continue
    except OSError:
        pass
    return names


# --- engine (the rules live in ScanRules, from token_watch_rules) -------------

class Scan(ScanRules):
    """One scan cycle: ScanRules plus the incremental transcript reads."""

    def read_file(self, path, f):
        try:
            stt = os.stat(path)
        except OSError:
            return
        f["seen_at"] = self.now
        first = "ino" not in f
        self.min_ts = self.now - (LOOKBACK if first else 3 * LOOKBACK)
        if not first and (f["ino"] != stt.st_ino or stt.st_size < f.get("off", 0)):
            f["off"] = 0
        off = f.get("off", 0)
        if first and stt.st_size > FIRST_TAIL:
            off = stt.st_size - FIRST_TAIL
        if off >= stt.st_size:
            f["ino"], f["size"] = stt.st_ino, stt.st_size
            return
        f.setdefault("off0", off == 0)
        with open(path, "rb") as fh:
            fh.seek(off)
            data = fh.read(min(stt.st_size - off, READ_CAP))
        end = data.rfind(b"\n")
        if end < 0:                      # only a partial line so far
            f["ino"], f["size"] = stt.st_ino, stt.st_size
            return
        chunk = data[:end + 1]
        if off > 0 and first:            # tail start: drop the cut first line
            chunk = chunk[chunk.find(b"\n") + 1:]
        for raw in chunk.splitlines():
            if not raw.strip():
                continue
            self.lines += 1
            try:
                d = json.loads(raw)
            except ValueError:
                self.errors += 1
                continue
            if isinstance(d, dict) and d.get("type") == "user" and not f.get("triage_gate") \
                    and d.get("entrypoint") == TRIAGE_ENTRYPOINT:
                c = (d.get("message") or {}).get("content")
                if isinstance(c, list):
                    c = "".join(b.get("text", "") for b in c if isinstance(b, dict))
                if isinstance(c, str) and c.startswith(TRIAGE_PREFIX):
                    f["triage_gate"] = True
            if isinstance(d, dict) and f.get("kind") == "sub":
                # #fanout: a subagent is done once its transcript ends on an
                # assistant turn with stop_reason end_turn; any later user or
                # assistant line (a resume) makes it live again.
                m = d.get("message") or {}
                if d.get("type") == "assistant":
                    f["done"] = m.get("stop_reason") in ("end_turn", "stop_sequence")
                elif d.get("type") == "user":
                    f["done"] = False
            if isinstance(d, dict) and (d.get("type") == "assistant"
                                        or (d.get("message") or {}).get("role") == "assistant"):
                self.turn(path, f, d)
        f.update(ino=stt.st_ino, size=stt.st_size, off=off + end + 1)


def flush_hours(st, now, hourly_path):
    """Append completed-hour rows; a key already in the file is skipped."""
    cur = hour_of(now)
    due = [k for k in st["open"] if k.split("|", 1)[0] < cur]
    if not due:
        return 0
    have = set()
    try:
        with open(hourly_path) as f:
            for line in f:
                try:
                    r = json.loads(line)
                except ValueError:
                    continue
                have.add("|".join((r["hour"], r["project"], r["session"], r["agent_type"], r["model"], r.get("effort") or "unknown")))
    except OSError:
        pass
    os.makedirs(os.path.dirname(hourly_path), exist_ok=True)
    n = 0
    with open(hourly_path, "a") as f:
        for k in sorted(due):
            if k not in have:
                f.write(json.dumps(rollup_row(k, st["open"][k])) + "\n")
                n += 1
            del st["open"][k]
    return n


def step(now=None, dry_run=False, base=None, state_path=None, hourly_path=None,
         thresholds_path=None, names=None):
    """One scan cycle. Returns [{"rule","session","fingerprint","detail"}].
    dry_run works on a copy and writes nothing."""
    now = time.time() if now is None else now
    if dry_run:
        return _step(now, True, base, state_path, hourly_path, thresholds_path, names)
    with state_lock(state_path):  # trusty-tools#11629: load to save, locked
        return _step(now, False, base, state_path, hourly_path, thresholds_path, names)


def _step(now, dry_run, base, state_path, hourly_path, thresholds_path, names):
    base = base or TRANSCRIPT_BASE
    st = load_state(state_path)
    if dry_run:
        st = copy.deepcopy(st)
    sc = Scan(st, thresholds(thresholds_path), now, base, names if names is not None else session_names())
    t0 = time.monotonic()
    for path, kind, project, sid, aid in sorted(discover(base, now)):
        if time.monotonic() - t0 > BUDGET_S:
            print("token_watch: time budget reached, resuming next cycle", file=sys.stderr)
            break
        f = st["files"].setdefault(path, {"kind": kind, "project": project, "session": sid,
                                          "agent_id": aid, "last_ts": None, "recent": [], "polls": []})
        if kind == "sub" and "agent_type" not in f:
            f["agent_type"] = agent_type(path)
        sc.read_file(path, f)
    sc.degraded()
    sc.fleet_rules()
    st["lines"], st["parse_errors"] = sc.lines, sc.errors
    if not dry_run:
        flush_hours(st, now, hourly_path or HOURLY_PATH)
    prune(st, now)
    if not dry_run:
        save_state(st, state_path)
    return sc.alerts


# --- report ------------------------------------------------------------------

def report(since_s, hourly_path=None, state_path=None, now=None):
    now = time.time() if now is None else now
    cut = hour_of(now - since_s)
    rows = []
    try:
        with open(hourly_path or HOURLY_PATH) as f:
            for line in f:
                try:
                    r = json.loads(line)
                except ValueError:
                    continue
                if r.get("hour", "") >= cut:
                    rows.append(r)
    except OSError:
        pass
    st = load_state(state_path)
    rows += [rollup_row(k, o) for k, o in st["open"].items() if k.split("|", 1)[0] >= cut]
    return rows


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("report")
    r.add_argument("--since", default="1h")
    r.add_argument("--json", action="store_true")
    s = sub.add_parser("scan")
    s.add_argument("--dry-run", action="store_true")
    a = ap.parse_args(argv)
    if a.cmd == "scan":
        for al in step(dry_run=a.dry_run):
            print(f"{'would alert' if a.dry_run else 'alert'}: [{al['session']}] token {al['detail']}")
        return 0
    rows = report(parse_since(a.since))
    if a.json:
        print(json.dumps(rows))
    else:
        print("hour project session agent model turns ctx_p50 ctx_p90 hit units  "
              "(units: price x (in+5out+w_read*read+1.25*c5m+2*c1h)/1e6, w_read opus 0.05 other 0.1)")
        for x in rows:
            print(x["hour"], x["project"][-24:], x["session"][:8], x["agent_type"], x["model"], x["turns"],
                  x["ctx_p50"], x["ctx_p90"], x["hit_ratio"], x["units"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
