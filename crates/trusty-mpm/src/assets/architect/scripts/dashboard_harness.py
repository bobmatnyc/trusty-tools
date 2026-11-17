"""Live view collector: dashboard_harness.build and GET /api/harness (IL ar-Te8).

Why: the owner (2026-10-23 16:4xZ): show what each harness (tm-* PM session) and each of its subagents is doing, as it happens.
What: read-only snapshot merged from files the poller and the PMs already write; no tmux call, no write, bounded reads
(tails and per-file offsets, never a full scan). Cached ~4 s by the caller (HarnessCache).
Sources (each reported in `sources` as ok | empty | missing):
  live      inbox/dashboard-live.json   per session state + idle_min
  feed      inbox/dashboard-feed.json   per session ctx_k, out_tokens_1h, agents_active
  sessions  the `sessions` store docs   goal, summary
  items     the `items` store docs      open items with blocks_session -> state stop
  messages  inbox/pm-messages.jsonl     PM -> Architect  (dir "in"), last WINDOW_S
  relays    inbox/relays.jsonl          Architect -> PM  (dir "out"), written by scripts/relay.sh; empty until a relay is sent
  agents    <claude config>/projects/<cwd-encoded>/<session>/subagents/agent-*.{meta.json,jsonl}
  events    inbox/events.jsonl          tmux_session -> cwd, to join a transcript dir to a harness
State mapping (state is always one of ok|slow|stop|park):
  stop  an open item with blocks_session names the session, or its newest PM message in the window is a blocker
  park  session idle more than IDLE_PARK_MIN, or no live row, with no subagent in state ok/slow (those force ok/slow, ar-Tf9) and no
        message in or out within ACTIVITY_S (a messaged session is never park: it is slow, or ok when working; the owner 2026-10-25)
  slow  idle up to IDLE_PARK_MIN, or working with out_tokens_1h under SLOW_TOKENS_1H
  ok    working
Subagent state: ok (transcript written within 90 s), slow (within 5 min), park (older, up to 15 min); a transcript whose last
assistant turn ended (stop_reason end_turn) or whose meta carries finished=true is listed as "done" until it is AGENT_MAX_S old (the owner 2026-10-24:
show every PM subagent); since 2026-10-25 build() serves only ok/slow agents (done and park are dropped: Live shows active subagents only). An agent with no meta.json is listed too (type "agent"). Each agent also carries ctx_k (its last request's input +
cache tokens, null when unknown) and `events` (its last tool calls: epoch, tool NAME, file basename).
`architect` {state, ctx_k, approach_k, bar_k}: the Architect's own context band (dashboard_archctx.py; self-ctx.py measurement, il-schedule.json thresholds), k tokens or null.
`architect.state` is ok when its own transcript (main or subagent) was written, or a message went in or out, within ACTIVITY_S; else park when
user_state is "sleeping", else slow. SendMessage sends count as relays: the `relays` source is ok when any send is in the window.
Subagent-to-session join: the transcript session directory name is matched to the tmux session whose newest event carries that session_id; else
the directory's cwd owner (events cwd); a session with no event at all is matched to a transcript dir by name (tm-<stem>[-NN] -> dir ending -<stem>).
Never serves transcript content for agents: only agentType, description, tool NAMEs and file basenames, token counts and times.
Docs tab (ar-Tea): `docs` lists files written or edited (Write/Edit/MultiEdit tool_use) in the last DOC_WINDOW_S by fleet sessions and
the Architect, read incrementally from the session and subagent transcripts of the joined cwds plus the Architect's own; newest first, one row per
file and author {id, path, project, rel, view, session, agent, kind, tool, ts, epoch, n}. Code edits count too (the owner 2026-10-24): `kind` is "doc" for
.md, else "code"; `agent` is the subagent id (8 chars) that made the change, "" for the session itself. `view` is {project, rel} when a .md file sits under a /doc root, else null. The
diff of the latest change comes from GET /api/harness/diff?id= (doc_diff): difflib over the Edit old/new strings, or the whole file as additions
for a Write. Secret-named files are never listed; secret-looking values in a diff are redacted. SendMessage sends found in the Architect's own
transcripts are added to `messages` (dir "out", kind "sendmessage"). Read-only; bounded by READ_CAP per file and DOC_FILES files per dir.
Test: tests/test_dashboard_harness.py, tests/test_dashboard_concurrency.py
"""
import difflib
import fnmatch
import glob
import hashlib
import json
import os
import re
import subprocess
import threading
import time
from datetime import datetime, timezone

import dashboard_archctx
from dashboard_fleet import (  # noqa: F401  (#10571: split out, re-exported for callers and tests)
    FACTORY_DONE, FACTORY_TASK, GONE_KEEP_S, PAUSED_DOC, POLLER, STATUSES, _iso_epoch, architect_state, collect_messages, dir_mtime, factory_sessions, fleet_status, map_state, resolve_target, session_status, session_tasks, tmux_sessions)
from dashboard_harness_base import (  # noqa: F401  (#10571: split out, re-exported)
    ACTIVITY_S, IDLE_PARK_MIN, SLOW_TOKENS_1H, TAIL_BYTES, TEXT_MAX, WINDOW_S, _SECRET, _iso, one_line, read_json, tail_json,
    tail_lines)

AGENT_OK_S, AGENT_SLOW_S, AGENT_MAX_S = 90, 300, 15 * 60
READ_CAP = 2 * 1024 * 1024        # per-file bytes read in one call; a bigger backlog is skipped to the tail
SESSION_DIRS = 4                  # newest session directories scanned per transcript project
CACHE_TTL_S = 4.0
DOC_WINDOW_S = 24 * 3600
DOC_FILES = 10                    # newest main transcripts per dir, and newest subagent transcripts per dir
DOC_KEEP = 800                    # change records kept in memory
DOC_ROWS = 300                    # rows served
AGENT_EVENTS = 12                 # tool events kept per agent
AGENT_RANK = {"ok": 0, "slow": 1, "park": 2, "done": 3}
DOC_TEXT_MAX = 20000              # per old/new string
DOC_DENY = ("*secret*", "*credential*", "*.env*", "*.pem", "*.key", "*id_rsa*", "*.p12", "*.pfx")
_TOKENS = re.compile(r"(?i)\b(?:ghp_|gho_|github_pat_|sk-|xox[bp]-|AKIA)[A-Za-z0-9_\-]{12,}")
_doc_state = {"off": {}, "changes": [], "sends": []}
# #10161: ThreadingHTTPServer serves requests concurrently; every read or write of _doc_state holds this lock.
_doc_lock = threading.Lock()
_agent_cache = {}                 # path -> {off, req{requestId: out_tokens}, first, tool, file, end}


def encode_cwd(cwd):
    """Claude's transcript directory name for a cwd (every non-alphanumeric byte becomes "-")."""
    return re.sub(r"[^A-Za-z0-9-]", "-", cwd or "")


def fold_worktree(dirname):
    """A worktree cwd's transcript dir folds into its repo's dir."""
    return dirname.split("--claude-worktrees-")[0]


def session_sids(rows):
    """{Claude session_id: tmux session} from the newest event per tmux session (the id names the transcript session directory)."""
    newest = {}
    for r in rows or []:
        s, i = r.get("tmux_session"), r.get("session_id")
        if isinstance(s, str) and isinstance(i, str):
            newest[s] = i
    return {i: s for s, i in newest.items()}


def name_dirs(projects_dir, names, taken):
    """{transcript dir: [session]} for sessions with no event cwd: tm-<stem>[-NN] matches the newest dir (not in `taken`) ending -<stem>.
    Why: a quiet session's last event can sit outside the events tail, and its store doc may name no project (tm-ai-power-rankings, 2026-10-24)."""
    try:
        dirs = [e for e in os.scandir(projects_dir) if e.is_dir() and "--claude-worktrees-" not in e.name]
    except OSError:
        return {}
    out, used = {}, set(taken)
    for n in names:
        stem = re.sub(r"-\d+$", "", re.sub(r"^tm-", "", n))
        hit = [e for e in dirs if e.name not in used and e.name.endswith("-" + stem)] if stem else []
        if hit:
            d = max(hit, key=lambda e: e.stat().st_mtime).name
            used.add(d)
            out.setdefault(d, []).append(n)
    return out


def session_cwds(events_path, projects, rows=None):
    """{encoded transcript dir: [tmux session, ...]} from the newest event per tmux session; `projects` = {session: cwd} fallback."""
    newest = {}
    for r in (rows if rows is not None else tail_json(events_path, 512 * 1024)) or []:
        s, c = r.get("tmux_session"), r.get("cwd")
        if isinstance(s, str) and isinstance(c, str):
            newest[s] = c
    for s, c in (projects or {}).items():
        newest.setdefault(s, c)
    out = {}
    for s, c in sorted(newest.items()):
        out.setdefault(fold_worktree(encode_cwd(c)), []).append(s)
    return out


def _scan_transcript(path, now):
    """Incremental read of one agent transcript: dedup'd output tokens, first timestamp, last tool name/file, ended flag."""
    try:
        size = os.stat(path).st_size
    except OSError:
        return None
    c = _agent_cache.get(path)
    if c is None or size < c["off"]:
        c = _agent_cache[path] = {"off": 0, "req": {}, "first": None, "tool": "", "file": "", "end": False, "approx": False, "ctx": None, "ev": []}
    if c["off"] == 0 and c["first"] is None:
        try:
            with open(path, "rb") as f:
                head = f.readline(65536)
            c["first"] = _iso(json.loads(head).get("timestamp"))
        except (OSError, ValueError):
            pass
    if size > c["off"]:
        start = c["off"]
        if size - start > READ_CAP:
            start, c["approx"] = size - READ_CAP, True
        with open(path, "rb") as f:
            f.seek(start)
            data = f.read(size - start)
        cut = data.rfind(b"\n")
        if cut >= 0:
            c["off"] = start + cut + 1
            for ln in data[: cut + 1].splitlines():
                try:
                    r = json.loads(ln)
                except ValueError:
                    continue
                m = r.get("message") if isinstance(r, dict) else None
                if not isinstance(m, dict) or r.get("type") != "assistant":
                    continue
                u = m.get("usage") or {}
                if isinstance(u.get("output_tokens"), int):
                    c["req"][r.get("requestId") or r.get("uuid")] = u["output_tokens"]
                    ctx = sum(u.get(k) for k in ("input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens") if isinstance(u.get(k), int))
                    c["ctx"] = ctx or c["ctx"]
                c["end"] = m.get("stop_reason") == "end_turn"
                for blk in (m.get("content") if isinstance(m.get("content"), list) else []):
                    if isinstance(blk, dict) and blk.get("type") == "tool_use":
                        c["tool"] = str(blk.get("name") or "")
                        inp = blk.get("input") or {}
                        fp = inp.get("file_path") or inp.get("path") or ""
                        c["file"] = os.path.basename(fp) if isinstance(fp, str) else ""
                        t = _iso(r.get("timestamp"))
                        ev = {"epoch": int(t) if t else None, "tool": c["tool"][:40], "file": one_line(c["file"], 60)}
                        if not c["ev"] or {k: c["ev"][-1][k] for k in ("tool", "file")} != {k: ev[k] for k in ("tool", "file")}:   # streamed rows repeat a call
                            c["ev"] = (c["ev"] + [ev])[-AGENT_EVENTS:]
    if len(_agent_cache) > 2000:
        for k in list(_agent_cache)[:1000]:
            _agent_cache.pop(k, None)
    return c


def collect_agents(projects_dir, dirs, now):
    """{encoded dir: [agent dict]} for subagents under the given transcript project dirs, live and recently ended (state done)."""
    out = {}
    for d in dirs:
        base = os.path.join(projects_dir, d)
        try:
            ents = [e for e in os.scandir(base) if e.is_dir()]
        except OSError:
            continue
        def stamp(e):
            try:
                return max(e.stat().st_mtime, os.stat(e.path + ".jsonl").st_mtime if os.path.exists(e.path + ".jsonl") else 0)
            except OSError:
                return 0
        ents.sort(key=stamp, reverse=True)
        for e in ents[:SESSION_DIRS]:
            sub = os.path.join(e.path, "subagents")
            for jp in glob.glob(os.path.join(sub, "agent-*.jsonl")):
                mp = jp[: -len(".jsonl")] + ".meta.json"
                try:
                    age = now - os.stat(jp).st_mtime
                except OSError:
                    continue
                if age > AGENT_MAX_S:
                    continue
                meta = read_json(mp) or {}   # an agent with no meta file is still an agent (type "agent")
                c = _scan_transcript(jp, now)
                if c is None:
                    continue
                aid = os.path.basename(jp)[len("agent-"):-len(".jsonl")]
                first = c["first"] or (os.stat(mp).st_ctime if os.path.exists(mp) else now - age)
                done = bool(meta.get("finished")) or c["end"]
                out.setdefault(d, []).append({
                    "id": aid[:8], "sid": e.name, "type": str(meta.get("agentType") or "agent"), "task": one_line(meta.get("description"), 100),
                    "state": "done" if done else "ok" if age <= AGENT_OK_S else "slow" if age <= AGENT_SLOW_S else "park",
                    "elapsed_s": max(0, int((now - age if done else now) - first)), "tokens": sum(c["req"].values()),
                    "ctx_k": round(c["ctx"] / 1000, 1) if c["ctx"] else None, "events": list(c["ev"]),
                    "act": one_line((c["tool"] + " " + c["file"]).strip(), 60), "worktree": bool(meta.get("spawnedWithWorktree")),
                    "branch": one_line(meta.get("worktreeBranch"), 60)})
    return out


def _doc_denied(path):
    base = os.path.basename(path).lower()
    parts = path.split("/")
    return any(fnmatch.fnmatch(base, g) for g in DOC_DENY) or "node_modules" in parts or "target" in parts or ".git" in parts


def _scrub(text):
    text = _TOKENS.sub("[redacted]", str(text or "")[:DOC_TEXT_MAX])
    return "\n".join(_SECRET.sub(lambda m: m.group(1) + "=[redacted]", ln) for ln in text.split("\n"))


def _scan_docs(path, owner, scan_sends):
    """Incremental read of one transcript; appends Write/Edit/MultiEdit changes to .md files (and SendMessage sends) to _doc_state."""
    try:
        size = os.stat(path).st_size
    except OSError:
        return
    off = _doc_state["off"].get(path, 0)
    if size < off:
        off = 0
    if size <= off:
        return
    start = max(off, size - READ_CAP)
    with open(path, "rb") as f:
        f.seek(start)
        data = f.read(size - start)
    cut = data.rfind(b"\n")
    if cut < 0:
        return
    _doc_state["off"][path] = start + cut + 1
    base = os.path.basename(path)
    agent = base[len("agent-"):-len(".jsonl")][:8] if base.startswith("agent-") else ""
    for ln in data[: cut + 1].splitlines():
        if b'"tool_use"' not in ln:
            continue
        try:
            r = json.loads(ln)
        except ValueError:
            continue
        m = r.get("message") if isinstance(r, dict) else None
        t = _iso(r.get("timestamp")) if isinstance(r, dict) else None
        if not isinstance(m, dict) or t is None or not isinstance(m.get("content"), list):
            continue
        for blk in m["content"]:
            if not isinstance(blk, dict) or blk.get("type") != "tool_use":
                continue
            name, inp = blk.get("name"), blk.get("input") or {}
            if not isinstance(inp, dict):
                continue
            fp = inp.get("file_path")
            if name in ("Write", "Edit", "MultiEdit") and isinstance(fp, str) and fp.startswith("/") and not _doc_denied(fp):   # code edits are doc changes too
                if name == "Write":
                    hunks = [("", inp.get("content"))]
                elif name == "Edit":
                    hunks = [(inp.get("old_string"), inp.get("new_string"))]
                else:
                    hunks = [(e.get("old_string"), e.get("new_string")) for e in (inp.get("edits") or []) if isinstance(e, dict)]
                _doc_state["changes"].append({"path": fp, "session": owner, "agent": agent, "kind": "doc" if fp.endswith(".md") else "code", "tool": name, "epoch": int(t), "ts": r["timestamp"][:19] + "Z",
                                              "hunks": [(_scrub(a), _scrub(b)) for a, b in hunks][:20], "new_file": name == "Write"})
            elif scan_sends and name == "SendMessage" and isinstance(inp.get("to"), str):
                _doc_state["sends"].append({"ts": r["timestamp"][:19] + "Z", "epoch": int(t), "dir": "out", "session": inp["to"],
                                            "kind": "sendmessage", "text": one_line(inp.get("message") or inp.get("summary"))})
    for k in ("changes", "sends"):
        if len(_doc_state[k]) > DOC_KEEP:
            del _doc_state[k][: len(_doc_state[k]) - DOC_KEEP]
    if len(_doc_state["off"]) > 2000:
        for k in list(_doc_state["off"])[:1000]:
            _doc_state["off"].pop(k, None)


_title_cache = {}   # path -> (mtime, title); the Docs tab re-polls every 3 s


def doc_title(path):
    """First markdown H1 of a doc (first 8 KB, fenced code skipped), else the basename without extension made readable."""
    try:
        mt = os.stat(path).st_mtime
        hit = _title_cache.get(path)
        if hit and hit[0] == mt:
            return hit[1]
        with open(path, "rb") as f:
            head = f.read(8192).decode("utf-8", "replace")
    except OSError:
        mt, head = None, ""
    title, fence = None, False
    for ln in head.split("\n"):
        if ln.lstrip().startswith(("```", "~~~")):
            fence = not fence
        elif not fence and ln.startswith("# ") and ln[2:].strip():
            title = ln[2:].strip().rstrip("#").strip()[:120]
            break
    if not title:
        base = os.path.splitext(os.path.basename(path))[0]
        title = re.sub(r"[-_.]+", " ", base).strip() or base
    if mt is not None:
        if len(_title_cache) > 500:
            _title_cache.clear()
        _title_cache[path] = (mt, title)
    return title


def doc_id(path, epoch):
    return hashlib.sha1(("%s|%d" % (path, epoch)).encode()).hexdigest()[:12]


def collect_docs(projects_dir, dir_owner, roots, now, architect_dir=None):
    """(docs rows, sends). dir_owner = {encoded dir: session name}; roots = {project: root path} (for view/project). Newest change per file wins.
    Holds _doc_lock: one scan of _doc_state at a time (#10161)."""
    with _doc_lock:
        return _collect_docs(projects_dir, dir_owner, roots, now, architect_dir)


def _collect_docs(projects_dir, dir_owner, roots, now, architect_dir):
    for d, owner in dir_owner.items():
        base = os.path.join(projects_dir, d)
        try:
            mains = [(e.stat().st_mtime, e) for e in os.scandir(base) if e.is_file() and e.name.endswith(".jsonl")]
        except OSError:
            continue
        mains = sorted(((m, e) for m, e in mains if now - m <= DOC_WINDOW_S), key=lambda me: me[0], reverse=True)
        subs = []     # (mtime, path): one stat per transcript, from the scandir entry (was two stats per file plus a glob per session dir)
        for e in os.scandir(base):
            if not e.is_dir():
                continue
            try:
                for a in os.scandir(os.path.join(e.path, "subagents")):
                    if a.name.startswith("agent-") and a.name.endswith(".jsonl"):
                        m = a.stat().st_mtime
                        if now - m <= DOC_WINDOW_S:
                            subs.append((m, a.path))
            except OSError:
                continue
        subs.sort(reverse=True)
        for _, e in mains[:DOC_FILES]:
            _scan_docs(e.path, owner, d == architect_dir)
        for _, p in subs[:DOC_FILES]:
            _scan_docs(p, owner, False)
    real_roots = {k: os.path.realpath(v) for k, v in (roots or {}).items()}
    best = {}      # (path, session, agent) -> newest change: a node's own list shows what that node changed
    count = {}
    for c in _doc_state["changes"]:
        if now - c["epoch"] > DOC_WINDOW_S:
            continue
        k = (c["path"], c["session"], c.get("agent", ""))
        count[k] = count.get(k, 0) + 1
        if k not in best or c["epoch"] >= best[k]["epoch"]:
            best[k] = c
    rows = []
    for k, c in best.items():
        path = k[0]
        project, rel, view, under = os.path.basename(os.path.dirname(path)) or "?", os.path.basename(path), None, False
        for rk, rr in sorted(real_roots.items(), key=lambda kv: -len(kv[1])):
            if path.startswith(rr + os.sep):
                project, rel, under = rk, path[len(rr) + 1:], True
                if c["kind"] == "doc" and not any(x.startswith(".") or x in ("node_modules", "target") for x in rel.split("/")):
                    view = {"project": rk, "rel": rel}
                break
        rows.append({"id": doc_id(path, c["epoch"]), "title": doc_title(path) if c["kind"] == "doc" else os.path.basename(path), "full": path,
                     "path": rel if under else path, "project": project, "rel": rel, "view": view, "kind": c["kind"], "loc": file_loc(path),
                     "session": c["session"], "agent": c.get("agent", ""), "tool": c["tool"], "ts": c["ts"], "epoch": c["epoch"], "n": count[k]})
    rows.sort(key=lambda r: -r["epoch"])
    sends = [s for s in _doc_state["sends"] if now - s["epoch"] <= WINDOW_S]
    return rows[:DOC_ROWS], sends


_loc_cache = {}   # path -> ((mtime_ns, size), loc)


def file_loc(path):
    """Physical line count of the file's current contents (blank and comment lines included), cached by mtime+size.
    Empty file -> 0; missing, unreadable or binary (NUL byte) -> None (the UI shows an em dash, never a fake 0)."""
    try:
        st = os.stat(path)
        if not os.path.isfile(path):
            return None
        key = (st.st_mtime_ns, st.st_size)
        hit = _loc_cache.get(path)
        if hit and hit[0] == key:
            return hit[1]
        n, last = 0, b"\n"
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                if b"\0" in chunk:
                    n = None
                    break
                n += chunk.count(b"\n")
                last = chunk[-1:]
        if n is not None and last != b"\n":
            n += 1
    except OSError:
        return None
    if len(_loc_cache) > 4096:
        _loc_cache.clear()
    _loc_cache[path] = (key, n)
    return n


def doc_diff(doc_id_, now=None):
    """Unified diff text of the latest change behind a docs row id, or None for an unknown id."""
    with _doc_lock:   # #10161: never iterate _doc_state while a scan appends to it
        changes = list(_doc_state["changes"])
    for c in reversed(changes):
        if doc_id(c["path"], c["epoch"]) == doc_id_:
            out = []
            for a, b in c["hunks"]:
                out.extend(difflib.unified_diff(a.splitlines(), b.splitlines(), "before", "after", lineterm="", n=2))
            return {"id": doc_id_, "path": c["path"], "tool": c["tool"], "ts": c["ts"], "new_file": c["new_file"], "diff": "\n".join(out)}
    return None


# ---- session status: the four labels every surface shows (the owner 2026-10-25 12:02Z) ----


def build(inbox, projects_dir, session_docs, item_docs, now=None, project_cwds=None, roots=None, goal_docs=None, user_state=None, tmux=None):
    """Snapshot dict. session_docs/item_docs/goal_docs are the store docs' `data` dicts; project_cwds = {session: cwd} fallback."""
    now = time.time() if now is None else now
    live = read_json(os.path.join(inbox, "dashboard-live.json"))
    feed = read_json(os.path.join(inbox, "dashboard-feed.json"))
    sources = {"live": "ok" if live else "missing", "feed": "ok" if feed and feed.get("sessions") else "missing",
               "sessions": "ok" if session_docs else "empty", "items": "ok" if item_docs else "empty"}
    fsess = (feed or {}).get("sessions") or {}
    docs = {d.get("name"): d for d in session_docs if isinstance(d, dict)}
    blocked_by = {i.get("session") for i in item_docs if isinstance(i, dict) and i.get("status") == "open" and i.get("blocks_session")}
    msgs, msrc = collect_messages(inbox, now)
    sources.update(msrc)
    last_kind = {}
    for m in msgs:
        if m["dir"] == "in":
            last_kind[m["session"]] = m["kind"]
    ev_rows = tail_json(os.path.join(inbox, "events.jsonl"), 512 * 1024)
    cwds = session_cwds(None, project_cwds, ev_rows or [])
    sids = session_sids(ev_rows)
    sources["events"] = "ok" if cwds else "missing"
    names = sorted((live or {}).keys() | ({n for n in fsess} if not live else set()))
    placed = {s for ss in cwds.values() for s in ss}
    for d, ss in name_dirs(projects_dir, [n for n in names if n not in placed], cwds).items():   # no event for it: join by name
        cwds.setdefault(d, []).extend(ss)
    agents_by_dir = collect_agents(projects_dir, list(cwds), now)
    sources["agents"] = "ok" if os.path.isdir(projects_dir) else "missing"
    owner = {}        # transcript dir -> the session that gets its agents (a working one first)
    for d, ss in cwds.items():
        ss = [s for s in ss if s in names] or ss
        owner[d] = sorted(ss, key=lambda s: ((live or {}).get(s, {}).get("state") != "working", s))[0]
    dir_owner = dict(owner)
    arch_dir = None
    if roots and roots.get("architect"):
        arch_dir = fold_worktree(encode_cwd(roots["architect"]))
        dir_owner.setdefault(arch_dir, "tm-architect")
    doc_rows, sends = collect_docs(projects_dir, dir_owner, roots, now, arch_dir) if os.path.isdir(projects_dir) else ([], [])
    sources["docs"] = "ok" if doc_rows else "empty" if os.path.isdir(projects_dir) else "missing"
    for sd in sends:
        sd["session"] = resolve_target(sd["session"], names)
    if sends and sources.get("relays") != "ok":
        sources["relays"] = "ok"      # SendMessage relays do not pass relay.sh
    msgs = sorted(msgs + sends, key=lambda m: m["epoch"])
    msg_at = {}
    for m in msgs:
        msg_at[m["session"]] = max(msg_at.get(m["session"], 0), m["epoch"])
    agents_all, agent_owner = [], {}
    for d, lst in agents_by_dir.items():
        for a in lst:   # exact join first: the transcript session dir is the session_id of the tmux session that spawned it
            o = sids.get(a.pop("sid"))
            agents_all.append(a)
            agent_owner[id(a)] = o if o in names else owner.get(d)
    sessions = []
    fac = factory_sessions(goal_docs)
    for n in names:
        lv = (live or {}).get(n) or {}
        fd = fsess.get(n) or {}
        doc = docs.get(n) or {}
        raw = lv.get("state") or fd.get("live_state")
        idle = lv.get("idle_min", fd.get("idle_min"))
        blocked = n in blocked_by or last_kind.get(n) == "blocker"
        goal = str(doc.get("goal") or "")
        gid, _, gtitle = goal.partition(" · ")
        ags = [a for a in agents_all if agent_owner[id(a)] == n and a["state"] in ("ok", "slow")]   # the owner 2026-10-25: Live lists only active subagents
        ags.sort(key=lambda a: (AGENT_RANK[a["state"]], -a["tokens"]))
        sessions.append({"name": n, "goal": gid, "goal_title": gtitle, "state": map_state(raw, idle, fd.get("out_tokens_1h"), blocked, ags, messaged=msg_at.get(n, 0) >= now - ACTIVITY_S),
                         "status": session_status(doc.get("state"), (n in tmux) if tmux is not None else n in (live or {}), raw, any(a["state"] in ("ok", "slow") for a in ags)),
                         "raw_state": raw, "idle_min": idle, "ctx_k": fd.get("ctx_k"), "tokens_1h": fd.get("out_tokens_1h"),
                         "summary": one_line(doc.get("summary"), 200), "agents": ags, "tasks": session_tasks(n, item_docs, fac)})
    arch = dashboard_archctx.architect_ctx(projects_dir, (roots or {}).get("architect"))
    arch["state"] = architect_state(dir_mtime(projects_dir, arch_dir) if arch_dir else 0, max(m["epoch"] for m in msgs) if msgs else 0, user_state, now)
    return {"generated": datetime.fromtimestamp(now, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"), "epoch": int(now),
            "window_s": WINDOW_S, "architect": arch, "sessions": sessions, "messages": msgs[-200:], "docs": doc_rows, "sources": sources}


class HarnessCache:
    """Serve one build() result for CACHE_TTL_S so concurrent pages and polls share one scan.

    swr=True (stale-while-revalidate): once a value exists, an expired get() returns it at once and rebuilds on one background
    thread, so a slow build under host load never holds a request (Live "timed out", the owner 2026-10-30). The first get() still builds inline."""

    def __init__(self, ttl=CACHE_TTL_S, clock=time.time, swr=False):
        self.ttl, self.clock, self.swr, self._at, self._val = ttl, clock, swr, 0.0, None
        self._lock = threading.Lock()   # #10161: concurrent requests share one build() instead of racing two
        self._refreshing = False

    def _refresh(self, fn):
        try:
            val = fn()
            with self._lock:
                self._val, self._at = val, self.clock()
        except Exception as e:  # noqa: BLE001 - keep serving the stale value; the next expired get() retries
            import sys
            sys.stderr.write(f"harness refresh failed: {e!r}\n")
        finally:
            with self._lock:
                self._refreshing = False

    def get(self, fn):
        with self._lock:
            t = self.clock()
            if self.swr and self._val is not None:
                if t - self._at >= self.ttl and not self._refreshing:
                    self._refreshing = True
                    threading.Thread(target=self._refresh, args=(fn,), daemon=True).start()
                return self._val
            if self._val is None or t - self._at >= self.ttl:
                self._val, self._at = fn(), t
            return self._val
