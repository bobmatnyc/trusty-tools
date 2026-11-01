"""dashboard_docs.py: the read-only file rules behind the dashboard reader (split from dashboard-serve.py, #10161).

Why: the reader serves Markdown, briefs and usage samples from the Architect's projects. Every path rule lives in one
pure module so /doc, /doc-sync, /briefs and /media apply the same refusals.
What: `load_projects` builds the server-side project allowlist {name: realpath of root}: this repo as "architect", the
"- Root `<abs path>`" line of each local/projects/<name>.md, plus optional local/projects/roots.json {name: abs path}.
`resolve_doc` returns the real path of a readable .md file under a project root, or None: `..`, empty or absolute
segments, a realpath outside the root (symlink escape), non-.md files, any segment starting with "." (.git, .env,
.claude), node_modules, target, and any filename matching *secret*, *credential* or *.env*
(case-insensitive; checked again on the resolved path) are refused. One dot exception (`doc_denied`): a leading
`.trusty-mpm` segment is readable (PM plans) unless the next segment is `sessions`. `list_briefs` lists the briefs the
reader can reach, newest first, each with its durable brief id (brief_ids); each file passes `resolve_doc`.
`usage_samples` reads one account's last 24 h of ledger samples. `MAX_EDIT` bounds a PUT /doc text.
Standard library only. The Rust successor is the console route (See #10064).
Test: tests/test_dashboard_doc.py, tests/test_dashboard_item_brief.py, tests/test_dashboard_files.py
"""
import calendar
import fnmatch
import json
import os
import re
import time
from datetime import datetime, timezone

import brief_ids

ARCHITECT = "architect"
DENY_DIRS = ("node_modules", "target")                                # path segments never served (plus any starting with ".")
DENY_NAMES = ("*secret*", "*credential*", "*.env*")                   # filename globs never served, case-insensitive
MAX_DOC = 4 << 20
MAX_EDIT = 1 << 20                                                    # PUT /doc text limit, bytes
ROOT_LINE = re.compile(r"^- Root:? +`(/[^`\n]+)`", re.M)
BRIEF_DIRS = {ARCHITECT: ("docs/briefs", "local/briefs")}
USAGE_WINDOW_S = 24 * 3600
USAGE_FIELDS = ("ts", "pct5h", "reset5h", "pctwk", "resetwk")
_DATE = re.compile(r"^(\d{4}-\d{2}-\d{2})")


def load_projects(repo):
    """Project allowlist {name: realpath of root}: `repo` as "architect", the first "- Root `/abs`" line of each
    local/projects/<name>.md, and optional local/projects/roots.json {name: "/abs"}. Server-side only; a name
    must be a plain file stem (no separator). Roots that are not directories are dropped."""
    out = {ARCHITECT: repo}
    pdir = os.path.join(repo, "local", "projects")
    try:
        names = sorted(os.listdir(pdir))
    except OSError:
        names = []
    for fn in names:
        if fn.endswith(".md"):
            try:
                with open(os.path.join(pdir, fn), encoding="utf-8") as f:
                    m = ROOT_LINE.search(f.read(20000))
            except OSError:
                continue
            if m:
                out.setdefault(fn[:-3], m.group(1))
    try:
        with open(os.path.join(pdir, "roots.json"), encoding="utf-8") as f:
            extra = json.load(f)
        if isinstance(extra, dict):
            out.update({k: v for k, v in extra.items() if isinstance(k, str) and isinstance(v, str)})
    except (OSError, ValueError):
        pass
    return {k: os.path.realpath(v) for k, v in out.items() if k and "/" not in k and "\\" not in k and os.path.isdir(v)}


def usage_samples(ledger, account, now=None):
    """Last 24 h of ledger samples for one account label: numbers and times only (USAGE_FIELDS), oldest first.
    Unreadable lines and other accounts are skipped; an absent ledger gives []."""
    cut = (time.time() if now is None else now) - USAGE_WINDOW_S
    out = []
    try:
        with open(ledger, encoding="utf-8") as f:
            for line in f:
                try:
                    r = json.loads(line)
                    t = calendar.timegm(time.strptime(r["ts"], "%Y-%m-%dT%H:%M:%SZ"))
                except (ValueError, KeyError, TypeError):
                    continue
                if isinstance(r, dict) and r.get("account") == account and t >= cut:
                    out.append({k: r.get(k) for k in USAGE_FIELDS})
    except OSError:
        return []
    return sorted(out, key=lambda r: r["ts"])


def denied(segments, filename):
    """True when a path must never be served: a dot segment, node_modules, target, or a secret-like filename."""
    if any(x.startswith(".") or x in DENY_DIRS for x in segments):
        return True
    low = filename.lower()
    return any(fnmatch.fnmatch(low, g) for g in DENY_NAMES)


def list_briefs(projects):
    """Briefs the reader can open: every .md under each project's docs/briefs (the architect also local/briefs).
    Each file goes through resolve_doc, so the /doc refusals apply unchanged. Newest first by date (file-name date, else mtime)."""
    out = []
    for project, root in projects.items():
        for sub in BRIEF_DIRS.get(project, ("docs/briefs",)):
            base = os.path.join(root, *sub.split("/"))
            for dirpath, dirs, files in os.walk(base):
                dirs[:] = [d for d in dirs if not d.startswith(".")]
                for name in files:
                    rel = os.path.relpath(os.path.join(dirpath, name), root).split(os.sep)
                    real = resolve_doc(projects, project, rel) if name.endswith(".md") else None
                    if real is None or os.path.getsize(real) > MAX_DOC:
                        continue
                    with open(real, encoding="utf-8", errors="replace") as f:
                        text = f.read()
                    body = re.sub(r"\A---\n.*?\n---\n", "", text, flags=re.S)
                    h1 = re.search(r"^#\s+(.+?)\s*$", body, re.M)
                    h1_id = brief_ids.H1_ID_RE.match(h1.group(1)) if h1 else None
                    bid = brief_ids.marker_id(text) or (h1_id.group(1) if h1_id else None)   # durable brief id; the H1 "Bxxxx: " prefix leaves the title
                    m = _DATE.match(name)
                    st = os.stat(real)
                    out.append({"project": project, "path": "/".join(rel), "title": brief_ids.H1_ID_RE.sub("", h1.group(1), count=1) if h1 else name, "brief_id": bid,
                                "date": m.group(1) if m else datetime.fromtimestamp(st.st_mtime, timezone.utc).strftime("%Y-%m-%d"),
                                "size": st.st_size, "words": len(body.split())})
    out.sort(key=lambda b: (b["date"], b["path"]), reverse=True)
    return out


def doc_denied(segments):
    """`denied` for the doc reader/editor, with one exception: a leading `.trusty-mpm` segment is allowed (PM plans live in
    .trusty-mpm/plans/ and .trusty-mpm/*.md) unless the next segment is `sessions` (snapshots). Every other dot segment stays denied."""
    segs = list(segments)
    if len(segs) > 1 and segs[0] == ".trusty-mpm":
        if segs[1] == "sessions":
            return True
        segs = segs[1:]
    return denied(segs, segments[-1])


def resolve_doc(projects, project, rel_segments):
    """Real path of a readable .md file under the project root, or None. Pure: no request state. rel_segments are
    already unquoted. Denied: dot segments, node_modules, target, *secret*, *credential*, *.env* names (checked on the
    requested path and on the symlink-resolved path); realpath must stay under the root."""
    root = projects.get(project)
    if not root or not rel_segments:
        return None
    for seg in rel_segments:
        if seg in ("", ".", "..") or "/" in seg or "\\" in seg or "\x00" in seg:
            return None
    if not rel_segments[-1].endswith(".md") or doc_denied(rel_segments):
        return None
    root_real = os.path.realpath(root)
    real = os.path.realpath(os.path.join(root_real, *rel_segments))
    if os.path.commonpath([root_real, real]) != root_real or real == root_real or not real.endswith(".md") or not os.path.isfile(real):
        return None
    rsegs = os.path.relpath(real, root_real).split(os.sep)
    if doc_denied(rsegs):
        return None
    return real
