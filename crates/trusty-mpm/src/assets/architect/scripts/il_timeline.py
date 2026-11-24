"""il_timeline.py: which task a project was working on at a given time, from its Intent Ledger log (ar-T5h).

Why: the Tasks view (dashboard_tasks.py) attributed a message only through a literal "Task: <id>" line, so most messages were
unattributed. Each project's ledger log records when a task was opened, updated and done; that is enough to say what it was on.
What: load(datg_dirs) -> Timeline. datg_dirs = {claude project dir name: <repo>/.trusty-mpm/datg}.
  The log (log.jsonl or log-*.jsonl, events {t, id, ev}) is swept in time order. A task id (<code>-T<2-3 chars>) is ACTIVE from its
  'open' event until its 'done' event; an 'update' touches an active task (open also touches). A re-opened id (ids cycle) is a new
  incarnation, keyed (id, open epoch). At each event time the current task is the active one touched most recently; ties (the log
  has minute resolution) go to a task named in the state.json session task/next text, then to the higher id.
  Tasks whose state.json status is parked are never the current task. Events carry no start/focus record: "current" means last touched.
  Timeline.at(project, epoch) -> incarnation key (id, open_epoch) or None (nothing active then). Timeline.inc[key] = {id, open, close|None,
  title}. Timeline.newest[id] = key of the id's latest incarnation (a "Task: <id>" line resolves through it, also for a closed task).
Test: tests/test_il_timeline.py
"""
import bisect
import glob
import json
import os
import re
from datetime import datetime, timezone

TASK_ID = re.compile(r"^[a-z]{2}-T[0-9a-km-np-z]{2,3}$")
MENTION = re.compile(r"\b([a-z]{2}-T[0-9a-km-np-z]{2,3})\b")


def _epoch(s):
    for fmt in ("%Y-%m-%dT%H:%M:%S.%fZ", "%Y-%m-%dT%H:%M:%SZ", "%Y-%m-%dT%H:%MZ"):
        try:
            return datetime.strptime(s, fmt).replace(tzinfo=timezone.utc).timestamp()
        except (TypeError, ValueError):
            pass
    return None


def _events(datg):
    out = []
    for p in sorted(glob.glob(os.path.join(datg, "log*.jsonl"))):
        try:
            with open(p, encoding="utf-8", errors="replace") as f:
                for line in f:
                    if '"ev"' not in line or "-T" not in line:
                        continue
                    try:
                        d = json.loads(line)
                    except ValueError:
                        continue
                    t = _epoch(d.get("t"))
                    if t is not None and isinstance(d.get("id"), str) and TASK_ID.match(d["id"]):
                        out.append((t, d["id"], d.get("ev"), d.get("note") or ""))
        except OSError:
            continue
    out.sort(key=lambda e: e[0])   # stable: file order within one minute
    return out


class Timeline:
    def __init__(self):
        self.inc = {}
        self.newest = {}
        self._pts = {}   # project -> ([epoch], [incarnation key | None])

    def at(self, project, ts):
        pts = self._pts.get(project)
        if not pts:
            return None
        i = bisect.bisect_right(pts[0], ts) - 1
        return pts[1][i] if i >= 0 else None

    def add(self, project, datg):
        try:
            with open(os.path.join(datg, "state.json"), encoding="utf-8") as f:
                st = json.load(f)
        except (OSError, ValueError):
            st = {}
        tasks = {t["id"]: t for t in (st.get("tasks") or []) if isinstance(t, dict) and t.get("id")}
        ses = st.get("session") if isinstance(st.get("session"), dict) else {}
        named = set(MENTION.findall("%s %s" % (ses.get("task") or "", ses.get("next") or "")))
        active, ts_l, best_l = {}, [], []   # active: id -> [last touch, incarnation key]
        for t, tid, ev, note in _events(datg):
            if ev == "open":
                if tid not in active:
                    key = (tid, t)
                    title = (tasks.get(tid) or {}).get("title") or note
                    self.inc[key] = {"id": tid, "open": t, "close": None, "title": title}
                    active[tid] = [t, key]
                    self.newest[tid] = key
                else:
                    active[tid][0] = t
            elif ev == "update" and tid in active:
                active[tid][0] = t
            elif ev == "done" and tid in active:
                self.inc[active.pop(tid)[1]]["close"] = t
            else:
                continue
            cand = [(a[0], tid2 in named, tid2, a[1]) for tid2, a in active.items() if (tasks.get(tid2) or {}).get("status") != "parked"]
            ts_l.append(t)
            best_l.append(max(cand)[3] if cand else None)
        self._pts[project] = (ts_l, best_l)


def load(datg_dirs):
    tl = Timeline()
    for project, datg in sorted(datg_dirs.items()):
        tl.add(project, datg)
    return tl


def repo_datg_dirs(root, architect_dir, architect_datg):
    """{claude project dir name: datg dir} for every <root>/*/.trusty-mpm/datg and <root>/*/*/.trusty-mpm/datg, plus the Architect's own ledger."""
    out = {}
    for pat in ("*", os.path.join("*", "*")):
        for d in glob.glob(os.path.join(root, pat, ".trusty-mpm", "datg")):
            out[re.sub(r"[/.]", "-", os.path.realpath(os.path.dirname(os.path.dirname(d))))] = d
    out[architect_dir] = architect_datg   # the Architect's ledger lives in local/il, not in its repo's own .trusty-mpm
    return out
