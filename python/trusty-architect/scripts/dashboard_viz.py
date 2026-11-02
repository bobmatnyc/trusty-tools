"""dashboard_viz.py: read-only data for the dashboard's Visualize view (GET /api/viz).

Why: the owner 2026-10-21 14:56Z asked for a Visualize view: tokens and work over time, broken out by goal. The page already
holds items, goals and sessions; it does not hold the hourly token file or the closed IL tasks (those live in archive.jsonl).
What: build() returns {generated_at, since, tokens, projects, ctx_hours, il_closed}.
  tokens     [{hour, goal, out}]: out_tokens from local/usage/token-hourly.jsonl (all agents, main and sub), summed per
             UTC hour and goal key, for the last `days` days.
  projects   [{name, dir, goal, sessions, basis, out}]: how each Claude project directory was mapped to a goal.
             INFERRED, not recorded: directory -> project (allowlist roots, then the tm registry, then the path under
             ~/trusty-mpm-projects/<owner>/<repo>; scratchpad dirs follow their parent project; OS temp dirs are named
             "temp: ...") -> sessions -> the session's goal. name is never empty (the owner 2026-10-27: no unnamed rows).
  ctx_hours  [{hour, dir, turns, ctx, out}]: per UTC hour and Claude project directory (worktrees folded in), for the
             Treemap view: out = out_tokens (the Graph's measure, exact), and ctx = sum over rows of turns * ctx_p50. The file stores per-turn context (input +
             cache_read + cache_creation tokens, token_watch.py) only as p50/p90 per row, so ctx ESTIMATES the true sum;
             it is never exact. Granularity is the directory: the file carries no tm session name or IL task id, so the page
             attributes a directory to a session only when the project has exactly one.
  il_closed  [{at, session, id, title, goal}]: IL tasks closed in the window, from each ledger's archive.jsonl. `at` and
             `session` are recorded; `goal` is inferred from the ledger's session.
Goal key: a goal id ("G01") when the session's goal field starts with one, else that field's text when it has no id, else
the goals collection's `sessions` list names the session, else "" (unattributed). Nothing is written; every file is opened read-only.
Test: tests/test_dashboard_viz.py
"""
import json
import os
import re
from datetime import datetime, timedelta, timezone

import architect_config
import dashboard_il
import dashboard_projects

GOAL_ID = re.compile(r"^(G[0-9a-f]{2})\b")
WORKTREE = re.compile(r"--claude-worktrees-.*$")
MAX_ARCHIVE = 8 << 20
MAX_TITLE = 140
PROJECTS_ROOT = os.path.expanduser(architect_config.DEFAULT_PROJECTS_ROOT)   # #10571: build() reads local/architect.json projects_root
SCRATCH = re.compile(r"^-private-tmp-claude-\d+-(-.+?)-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}-scratchpad")
TEMP_PREFIXES = (("-private-var-folders-", "var/folders/"), ("-var-folders-", "var/folders/"), ("-private-tmp-", ""), ("-tmp-", ""))


def enc(path):
    """Claude Code's project directory name for a path: every character but A-Z, a-z and 0-9 becomes "-".
    #10571: "_" too, as decode() reads it; a temp path holding "_" no longer reads as "checkout not on disk"."""
    return re.sub(r"[^A-Za-z0-9]", "-", os.path.realpath(path))


def _bodies(store, coll):
    try:
        return [dict(d["data"], id=d["id"]) for d in store.list_docs(coll)]
    except Exception:  # noqa: BLE001 - a missing or unreadable collection gives an empty view, not a 500
        return []


def _stem(name):
    return re.sub(r"-\d\d$", "", re.sub(r"^tm-", "", name or ""))


def session_goal(name, sessions, goals):
    """Goal key of a session by name: its doc's goal id or label, else the first live goal listing it, else ""."""
    for s in sessions:
        if s.get("name") == name:
            text = str(s.get("goal") or "").strip()
            m = GOAL_ID.match(text)
            if m:
                return m.group(1)
            if text:
                return text   # a standing label with no goal id, e.g. "Standing · fleet supervision"
    for g in sorted(goals, key=lambda g: g.get("rank") if isinstance(g.get("rank"), int) else 99):
        if g.get("lifecycle") != "removed" and name in (g.get("sessions") or []):
            return g["id"]
    return ""


# #10570: the argument is "word": pm-guard read the old name's method call as a token file.
def _token_in(text, word):
    return re.search(r"(?<![\w-])" + re.escape(word.lower()) + r"(?![\w-])", (text or "").lower()) is not None


def project_sessions(project, sessions):
    """Sessions that belong to a project name: name-stem equal to it, else its name in the session's project text."""
    by_stem = [s["name"] for s in sessions if s.get("name") and _stem(s["name"]) == project.lower()]
    if by_stem:
        return by_stem, "session name"
    return [s["name"] for s in sessions if s.get("name") and _token_in(str(s.get("project") or ""), project)], "session project text"


def attribute(projects, sessions, goals):
    """{encoded dir: {name, goal, sessions, basis}} for each allowlisted project. Several goals: the most sessions win."""
    out = {}
    for name, root in projects.items():
        names, basis = project_sessions(name, sessions)
        votes = {}
        for n in names:
            g = session_goal(n, sessions, goals)
            if g:
                votes[g] = votes.get(g, 0) + 1
        if name == "architect" and not votes:
            names, basis = [dashboard_il.architect_session()], "this repo is the Architect's"   # #10161: ARCHITECT_SESSION
        goal = max(sorted(votes), key=lambda g: votes[g]) if votes else ""
        out[enc(root)] = {"name": name, "goal": goal, "sessions": sorted(names), "basis": basis if names else "no session found"}
    return out


def _enc_name(name):
    return re.sub(r"[^A-Za-z0-9]", "-", name)


def decode(d, base="/"):
    """Real path whose enc() is the directory name `d`, or None. "/", "." and "-" all encode to "-", so the name is matched
    against the real directory listing at each level (backtracking), never replaced naively."""
    def walk(path, rest):
        if not rest:
            return path
        try:
            names = os.listdir(path)
        except OSError:
            return None
        for n in sorted(names, key=len, reverse=True):
            e = _enc_name(n)
            if rest == e or rest.startswith(e + "-"):
                got = walk(os.path.join(path, n), rest[len(e) + 1:] if rest != e else "")
                if got:
                    return got
        return None
    rest = d[1:] if d.startswith("-") else d
    return walk(base, rest) if rest else None


def _inside(root, path):
    return path == root or path.startswith(root.rstrip("/") + "/")


def _owner_sessions(owner, repo, alias, sessions):
    """Sessions of a project by repo/alias, minus those whose project text names the repo under a different owner."""
    names, basis = project_sessions(repo, sessions)
    if alias and alias.lower() != repo.lower():
        more, b2 = project_sessions(alias, sessions)
        if more and not names:
            names, basis = more, b2
    keep = []
    for n in names:
        text = next((str(s.get("project") or "") for s in sessions if s.get("name") == n), "")
        m = re.search(r"([\w.-]+)/" + re.escape(repo) + r"(?![\w-])", text, re.I)
        if m and owner and m.group(1).lower() != owner.lower():
            continue
        keep.append(n)
    return keep, basis


def _goal_of(names, sessions, goals):
    votes = {}
    for n in names:
        g = session_goal(n, sessions, goals)
        if g:
            votes[g] = votes.get(g, 0) + 1
    return max(sorted(votes), key=lambda g: votes[g]) if votes else ""


def _row(name, owner, repo, alias, basis, sessions, goals):
    names, _ = _owner_sessions(owner, repo, alias, sessions) if repo else ([], "")
    return {"name": name, "goal": _goal_of(names, sessions, goals), "sessions": sorted(names),
            "basis": basis if names else basis + "; no session found"}


def _temp(d, real):
    for pre, label in TEMP_PREFIXES:
        if d.startswith(pre):
            rest = d[len(pre):]
            if label:
                rest = os.path.basename(real) if real else rest.rsplit("-", 1)[-1]
            return "temp: " + label + (rest or d.strip("-"))
    return "temp: " + d.strip("-")


def resolve_dir(d, registry, sessions, goals, projects_root=PROJECTS_ROOT, _top=True):
    """{name, goal, sessions, basis} for a Claude project directory not in the allowlist. name is never "".
    Order: scratchpad -> its parent project "(scratch)"; the tm registry by path; ~/trusty-mpm-projects/<owner>/<repo>
    ("path, not registered"); OS temp dirs ("temp: ..."); else the directory text, flagged unresolved."""
    m = SCRATCH.match(d) if _top else None
    if m:
        par = resolve_dir(m.group(1), registry, sessions, goals, projects_root, _top=False)
        if not par["basis"].startswith("OS temp dir") and not par["basis"].startswith("unresolved"):
            return dict(par, name=par["name"] + " (scratch)", basis="Claude scratchpad of " + par["name"] + " (" + par["basis"].split(";")[0] + ")")
    real = decode(d)
    best = None
    for r in registry:
        path = r.get("path")
        if not path:
            continue
        rp = os.path.realpath(path)
        if (real and _inside(rp, real)) or enc(path) == d:
            if best is None or len(rp) > len(best[0]):
                best = (rp, r)
    if best:
        r = best[1]
        owner, repo = r.get("owner"), r.get("repo")
        return _row("%s/%s" % (owner, repo) if owner and repo else r["name"], owner, repo or r["name"], r["name"], "tm registry (path match)", sessions, goals)
    if real:
        root = os.path.realpath(projects_root)
        if _inside(root, real) and real != root:
            parts = os.path.relpath(real, root).split(os.sep)
            if len(parts) >= 2:
                return _row("%s/%s" % (parts[0], parts[1]), parts[0], parts[1], "", "path, not registered", sessions, goals)
            return {"name": parts[0], "goal": "", "sessions": [], "basis": "path, not registered; no session found"}
    if real is None:   # checkout gone from disk: split owner/repo from the text under the projects root
        pre = enc(projects_root) + "-"
        if d.startswith(pre):
            rest = d[len(pre):]
            try:
                owners = sorted(os.listdir(projects_root), key=len, reverse=True)
            except OSError:
                owners = []
            for o in owners:
                if rest.startswith(_enc_name(o) + "-") and len(rest) > len(o) + 1:
                    repo = rest[len(o) + 1:]
                    return _row("%s/%s" % (o, repo), o, repo, "", "path, not registered; checkout not on disk", sessions, goals)
    if real is not None and not (_inside("/private/tmp", real) or _inside("/private/var", real)):
        return {"name": os.path.basename(real) or real, "goal": "", "sessions": [], "basis": "path, not registered; outside the projects root"}
    if real is not None or any(d.startswith(p) for p, _ in TEMP_PREFIXES):
        return {"name": _temp(d, real), "goal": "", "sessions": [], "basis": "OS temp dir"}
    return {"name": "unresolved: " + (d.strip("-")[-40:] or d), "goal": "", "sessions": [], "basis": "unresolved: directory not found on disk"}


def _dirs(token_file):
    """Distinct worktree-folded directory names in the token file."""
    out = set()
    try:
        with open(token_file, encoding="utf-8") as f:
            for line in f:
                try:
                    out.add(WORKTREE.sub("", json.loads(line)["project"]))
                except (ValueError, KeyError, TypeError):
                    continue
    except OSError:
        pass
    return out


def tokens(token_file, attr, since_hour):
    """(rows, projects, ctx): out_tokens per (UTC hour, goal), per-directory out totals, and per-(hour, directory)
    {turns, ctx, out} (ctx = turns * ctx_p50, an estimate), hours >= since_hour."""
    agg, tot, ctx = {}, {}, {}
    try:
        f = open(token_file, encoding="utf-8")
    except OSError:
        return [], {}, []
    with f:
        for line in f:
            try:
                r = json.loads(line)
                hour, d, n = r["hour"], WORKTREE.sub("", r["project"]), int(r.get("out_tokens") or 0)
                turns = int(r.get("turns") or 0)
                c = turns * int(r.get("ctx_p50") or 0)
            except (ValueError, KeyError, TypeError):
                continue
            if hour < since_hour:
                continue
            goal = attr.get(d, {}).get("goal", "")
            agg[(hour, goal)] = agg.get((hour, goal), 0) + n
            tot[d] = tot.get(d, 0) + n
            e = ctx.setdefault((hour, d), [0, 0, 0])
            e[0] += turns
            e[1] += c
            e[2] += n
    rows = [{"hour": h, "goal": g, "out": n} for (h, g), n in sorted(agg.items())]
    return rows, tot, [{"hour": h, "dir": d, "turns": e[0], "ctx": e[1], "out": e[2]} for (h, d), e in sorted(ctx.items()) if e[1] or e[2]]


def il_closed(metas, since_iso, sessions, goals):
    """Tasks closed since since_iso across the discovered ledgers' archive.jsonl (read-only, size-capped)."""
    out = []
    for m in metas:
        path = os.path.join(os.path.dirname(m["state_path"]), "archive.jsonl")
        try:
            if os.path.getsize(path) > MAX_ARCHIVE:
                continue
            f = open(path, encoding="utf-8")
        except OSError:
            continue
        goal = session_goal(m["session"], sessions, goals)
        with f:
            for line in f:
                try:
                    r = json.loads(line)
                    it = r["item"]
                    at = r.get("closed_at") or r.get("archived_at") or ""
                except (ValueError, KeyError, TypeError):
                    continue
                if r.get("kind") != "task" or not isinstance(it, dict) or at[:16] < since_iso[:16]:
                    continue
                out.append({"at": at, "session": m["session"], "id": str(it.get("id") or ""),
                            "title": str(it.get("title") or "")[:MAX_TITLE], "goal": goal})
    return sorted(out, key=lambda x: x["at"])


def build(store, token_file, projects, il_schedule, architect_il, now, days=7, registry=None, projects_root=None):
    """The whole payload. `now` is epoch seconds; `days` is clamped to 1..14. `registry` = dashboard_projects.registered_projects()
    rows; None reads the tm registry (an unreadable one gives [])."""
    if projects_root is None:
        try:
            projects_root = architect_config.load().projects_root
        except architect_config.ConfigError:
            projects_root = PROJECTS_ROOT
    if registry is None:
        try:
            registry = dashboard_projects.registered_projects()
        except Exception:  # noqa: BLE001 - no registry: unmapped dirs fall back to the path rules
            registry = []
    days = max(1, min(14, int(days)))
    sessions, goals = _bodies(store, "sessions"), _bodies(store, "goals")
    start = datetime.fromtimestamp(now, timezone.utc) - timedelta(days=days)
    attr = attribute(projects, sessions, goals)
    fixed = {d: resolve_dir(d, registry, sessions, goals, projects_root) for d in _dirs(token_file) if d not in attr}
    attr.update(fixed)
    rows, tot, ctx_hours = tokens(token_file, attr, start.strftime("%Y-%m-%dT%H"))
    plist = [dict(a, dir=d, out=tot.get(d, 0)) for d, a in attr.items()]
    plist.sort(key=lambda p: -p["out"])
    closed = il_closed(dashboard_il.discover(projects, il_schedule, architect_il), start.strftime("%Y-%m-%dT%H:%M"), sessions, goals)
    return {"generated_at": datetime.fromtimestamp(now, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "since": start.strftime("%Y-%m-%dT%H:%M:%SZ"), "tokens": rows, "projects": [p for p in plist if p["out"]], "ctx_hours": ctx_hours, "il_closed": closed}
