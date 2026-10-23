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
  park  session idle more than IDLE_PARK_MIN, or no live row
  slow  idle up to IDLE_PARK_MIN, or working with out_tokens_1h under SLOW_TOKENS_1H
  ok    working
Subagent state: ok (transcript written within 90 s), slow (within 5 min), park (older, up to 15 min); a transcript whose last
assistant turn ended (stop_reason end_turn) or whose meta carries finished=true is not listed.
Never serves transcript content for agents: only agentType, description, the last tool NAME and a file basename, token counts and times.
Docs tab (ar-Tea): `docs` lists Markdown files written or edited (Write/Edit/MultiEdit tool_use) in the last DOC_WINDOW_S by fleet sessions and
the Architect, read incrementally from the session and subagent transcripts of the joined cwds plus the Architect's own; newest first, one row per
file {id, path, project, rel, view, session, tool, ts, epoch, n}. `view` is {project, rel} when the file sits under a /doc root, else null. The
diff of the latest change comes from GET /api/harness/diff?id= (doc_diff): difflib over the Edit old/new strings, or the whole file as additions
for a Write. Secret-named files are never listed; secret-looking values in a diff are redacted. SendMessage sends found in the Architect's own
transcripts are added to `messages` (dir "out", kind "sendmessage"). Read-only; bounded by READ_CAP per file and DOC_FILES files per dir.
Test: scripts/test_dashboard_harness.py
"""
import difflib
import fnmatch
import glob
import hashlib
import json
import os
import re
import time
from datetime import datetime, timezone

WINDOW_S = 30 * 60
IDLE_PARK_MIN = 15
SLOW_TOKENS_1H = 20000
AGENT_OK_S, AGENT_SLOW_S, AGENT_MAX_S = 90, 300, 15 * 60
TEXT_MAX = 160
TAIL_BYTES = 256 * 1024
READ_CAP = 2 * 1024 * 1024        # per-file bytes read in one call; a bigger backlog is skipped to the tail
SESSION_DIRS = 4                  # newest session directories scanned per transcript project
CACHE_TTL_S = 4.0
_SECRET = re.compile(r"(?i)(secret|token|password|passwd|api[_-]?key|bearer)\s*[:=]\s*\S+")
DOC_WINDOW_S = 24 * 3600
DOC_FILES = 6                     # newest main transcripts per dir, and newest subagent transcripts per dir
DOC_KEEP = 300                    # change records kept in memory
DOC_TEXT_MAX = 20000              # per old/new string
DOC_DENY = ("*secret*", "*credential*", "*.env*", "*.pem", "*.key", "*id_rsa*")
_TOKENS = re.compile(r"(?i)\b(?:ghp_|gho_|github_pat_|sk-|xox[bp]-|AKIA)[A-Za-z0-9_\-]{12,}")
_doc_state = {"off": {}, "changes": [], "sends": []}
_agent_cache = {}                 # path -> {off, req{requestId: out_tokens}, first, tool, file, end}


def _iso(ts):
    try:
        return datetime.strptime(ts[:19], "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc).timestamp()
    except (ValueError, TypeError):
        return None


def one_line(text, n=TEXT_MAX):
    s = _SECRET.sub(lambda m: m.group(1) + "=[redacted]", " ".join(str(text or "").split()))
    return s if len(s) <= n else s[: n - 1] + "…"


def tail_lines(path, nbytes=TAIL_BYTES):
    """Last nbytes of a file as complete lines; [] when absent. Bounded read."""
    try:
        with open(path, "rb") as f:
            f.seek(0, os.SEEK_END)
            size = f.tell()
            f.seek(max(0, size - nbytes))
            data = f.read()
    except OSError:
        return None
    lines = data.decode("utf-8", "replace").splitlines()
    return lines[1:] if size > nbytes else lines


def tail_json(path, nbytes=TAIL_BYTES):
    """Parsed JSON-object lines from the tail; None when the file is absent."""
    lines = tail_lines(path, nbytes)
    if lines is None:
        return None
    out = []
    for ln in lines:
        try:
            r = json.loads(ln)
        except ValueError:
            continue
        if isinstance(r, dict):
            out.append(r)
    return out


def read_json(path):
    try:
        with open(path, encoding="utf-8") as f:
            r = json.load(f)
        return r if isinstance(r, dict) else None
    except (OSError, ValueError):
        return None


def encode_cwd(cwd):
    """Claude's transcript directory name for a cwd (every non-alphanumeric byte becomes "-")."""
    return re.sub(r"[^A-Za-z0-9-]", "-", cwd or "")


def fold_worktree(dirname):
    """A worktree cwd's transcript dir folds into its repo's dir."""
    return dirname.split("--claude-worktrees-")[0]


def session_cwds(events_path, projects):
    """{encoded transcript dir: [tmux session, ...]} from the newest event per tmux session; `projects` = {session: cwd} fallback."""
    newest = {}
    for r in tail_json(events_path, 512 * 1024) or []:
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
        c = _agent_cache[path] = {"off": 0, "req": {}, "first": None, "tool": "", "file": "", "end": False, "approx": False}
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
                c["end"] = m.get("stop_reason") == "end_turn"
                for blk in (m.get("content") if isinstance(m.get("content"), list) else []):
                    if isinstance(blk, dict) and blk.get("type") == "tool_use":
                        c["tool"] = str(blk.get("name") or "")
                        inp = blk.get("input") or {}
                        fp = inp.get("file_path") or inp.get("path") or ""
                        c["file"] = os.path.basename(fp) if isinstance(fp, str) else ""
    if len(_agent_cache) > 2000:
        for k in list(_agent_cache)[:1000]:
            _agent_cache.pop(k, None)
    return c


def collect_agents(projects_dir, dirs, now):
    """{encoded dir: [agent dict]} for live subagents under the given transcript project dirs."""
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
            for mp in glob.glob(os.path.join(sub, "agent-*.meta.json")):
                jp = mp[: -len(".meta.json")] + ".jsonl"
                try:
                    age = now - os.stat(jp).st_mtime
                except OSError:
                    continue
                if age > AGENT_MAX_S:
                    continue
                meta = read_json(mp) or {}
                if meta.get("finished"):
                    continue
                c = _scan_transcript(jp, now)
                if c is None or c["end"]:
                    continue
                aid = os.path.basename(jp)[len("agent-"):-len(".jsonl")]
                first = c["first"] or os.stat(mp).st_ctime
                out.setdefault(d, []).append({
                    "id": aid[:8], "type": str(meta.get("agentType") or "agent"), "task": one_line(meta.get("description"), 100),
                    "state": "ok" if age <= AGENT_OK_S else "slow" if age <= AGENT_SLOW_S else "park",
                    "elapsed_s": max(0, int(now - first)), "tokens": sum(c["req"].values()),
                    "act": one_line((c["tool"] + " " + c["file"]).strip(), 60), "worktree": bool(meta.get("spawnedWithWorktree")),
                    "branch": one_line(meta.get("worktreeBranch"), 60)})
    return out


def _doc_denied(path):
    base = os.path.basename(path).lower()
    parts = path.split("/")
    return any(fnmatch.fnmatch(base, g) for g in DOC_DENY) or "node_modules" in parts or "target" in parts


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
            if name in ("Write", "Edit", "MultiEdit") and isinstance(fp, str) and fp.endswith(".md") and not _doc_denied(fp):
                if name == "Write":
                    hunks = [("", inp.get("content"))]
                elif name == "Edit":
                    hunks = [(inp.get("old_string"), inp.get("new_string"))]
                else:
                    hunks = [(e.get("old_string"), e.get("new_string")) for e in (inp.get("edits") or []) if isinstance(e, dict)]
                _doc_state["changes"].append({"path": fp, "session": owner, "tool": name, "epoch": int(t), "ts": r["timestamp"][:19] + "Z",
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
    """(docs rows, sends). dir_owner = {encoded dir: session name}; roots = {project: root path} (for view/project). Newest change per file wins."""
    for d, owner in dir_owner.items():
        base = os.path.join(projects_dir, d)
        try:
            mains = [e for e in os.scandir(base) if e.is_file() and e.name.endswith(".jsonl") and now - e.stat().st_mtime <= DOC_WINDOW_S]
        except OSError:
            continue
        mains.sort(key=lambda e: e.stat().st_mtime, reverse=True)
        subs = [p for e in os.scandir(base) if e.is_dir() for p in glob.glob(os.path.join(e.path, "subagents", "agent-*.jsonl"))]
        subs = sorted((p for p in subs if now - os.stat(p).st_mtime <= DOC_WINDOW_S), key=lambda p: os.stat(p).st_mtime, reverse=True)
        for e in mains[:DOC_FILES]:
            _scan_docs(e.path, owner, d == architect_dir)
        for p in subs[:DOC_FILES]:
            _scan_docs(p, owner, False)
    real_roots = {k: os.path.realpath(v) for k, v in (roots or {}).items()}
    best = {}
    count = {}
    for c in _doc_state["changes"]:
        if now - c["epoch"] > DOC_WINDOW_S:
            continue
        count[c["path"]] = count.get(c["path"], 0) + 1
        if c["path"] not in best or c["epoch"] >= best[c["path"]]["epoch"]:
            best[c["path"]] = c
    rows = []
    for path, c in best.items():
        project, rel, view = os.path.basename(os.path.dirname(path)) or "?", os.path.basename(path), None
        for k, rr in sorted(real_roots.items(), key=lambda kv: -len(kv[1])):
            if path.startswith(rr + os.sep):
                project, rel = k, path[len(rr) + 1:]
                if not any(x.startswith(".") or x in ("node_modules", "target") for x in rel.split("/")):
                    view = {"project": k, "rel": rel}
                break
        rows.append({"id": doc_id(path, c["epoch"]), "title": doc_title(path), "full": path, "path": path if view is None else rel, "project": project, "rel": rel, "view": view,
                     "session": c["session"], "tool": c["tool"], "ts": c["ts"], "epoch": c["epoch"], "n": count[path]})
    rows.sort(key=lambda r: -r["epoch"])
    sends = [s for s in _doc_state["sends"] if now - s["epoch"] <= WINDOW_S]
    return rows[:100], sends


def doc_diff(doc_id_, now=None):
    """Unified diff text of the latest change behind a docs row id, or None for an unknown id."""
    for c in reversed(_doc_state["changes"]):
        if doc_id(c["path"], c["epoch"]) == doc_id_:
            out = []
            for a, b in c["hunks"]:
                out.extend(difflib.unified_diff(a.splitlines(), b.splitlines(), "before", "after", lineterm="", n=2))
            return {"id": doc_id_, "path": c["path"], "tool": c["tool"], "ts": c["ts"], "new_file": c["new_file"], "diff": "\n".join(out)}
    return None


def collect_messages(inbox, now):
    """(messages, sources) from pm-messages (in) and relays (out), newest last, within WINDOW_S."""
    msgs, src = [], {}
    for name, fname, direction in (("messages", "pm-messages.jsonl", "in"), ("relays", "relays.jsonl", "out")):
        rows = tail_json(os.path.join(inbox, fname))
        if rows is None:
            src[name] = "missing"
            continue
        n = 0
        for r in rows:
            t = _iso(r.get("ts"))
            if t is None or now - t > WINDOW_S or not r.get("session"):
                continue
            n += 1
            msgs.append({"ts": r["ts"], "epoch": int(t), "dir": direction, "session": str(r["session"]),
                         "kind": str(r.get("kind") or ("relay" if direction == "out" else "msg")), "text": one_line(r.get("text"))})
        src[name] = "ok" if n else "empty"
    msgs.sort(key=lambda m: m["epoch"])
    return msgs, src


def map_state(raw, idle_min, tokens_1h, blocked):
    if blocked:
        return "stop"
    if raw is None:
        return "park"
    if raw == "working":
        return "slow" if tokens_1h is not None and tokens_1h < SLOW_TOKENS_1H else "ok"
    return "slow" if (idle_min or 0) <= IDLE_PARK_MIN else "park"


def build(inbox, projects_dir, session_docs, item_docs, now=None, project_cwds=None, roots=None):
    """Snapshot dict. session_docs/item_docs are the store docs' `data` dicts; project_cwds = {session: cwd} fallback."""
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
    cwds = session_cwds(os.path.join(inbox, "events.jsonl"), project_cwds)
    sources["events"] = "ok" if cwds else "missing"
    agents_by_dir = collect_agents(projects_dir, list(cwds), now)
    sources["agents"] = "ok" if os.path.isdir(projects_dir) else "missing"
    names = sorted((live or {}).keys() | ({n for n in fsess} if not live else set()))
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
    msgs = sorted(msgs + sends, key=lambda m: m["epoch"])
    sessions = []
    for n in names:
        lv = (live or {}).get(n) or {}
        fd = fsess.get(n) or {}
        doc = docs.get(n) or {}
        raw = lv.get("state") or fd.get("live_state")
        idle = lv.get("idle_min", fd.get("idle_min"))
        blocked = n in blocked_by or last_kind.get(n) == "blocker"
        goal = str(doc.get("goal") or "")
        gid, _, gtitle = goal.partition(" · ")
        ags = [a for d, lst in agents_by_dir.items() if owner.get(d) == n for a in lst]
        ags.sort(key=lambda a: (a["state"] != "ok", -a["tokens"]))
        sessions.append({"name": n, "goal": gid, "goal_title": gtitle, "state": map_state(raw, idle, fd.get("out_tokens_1h"), blocked),
                         "raw_state": raw, "idle_min": idle, "ctx_k": fd.get("ctx_k"), "tokens_1h": fd.get("out_tokens_1h"),
                         "summary": one_line(doc.get("summary"), 200), "agents": ags})
    return {"generated": datetime.fromtimestamp(now, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"), "epoch": int(now),
            "window_s": WINDOW_S, "sessions": sessions, "messages": msgs[-200:], "docs": doc_rows, "sources": sources}


class HarnessCache:
    """Serve one build() result for CACHE_TTL_S so concurrent pages and polls share one scan."""

    def __init__(self, ttl=CACHE_TTL_S, clock=time.time):
        self.ttl, self.clock, self._at, self._val = ttl, clock, 0.0, None

    def get(self, fn):
        t = self.clock()
        if self._val is None or t - self._at >= self.ttl:
            self._val, self._at = fn(), t
        return self._val
