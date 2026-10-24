"""Media library for the dashboard's right pane: diagrams and images under each project's doc roots (the owner 2026-10-21).

Why: the right pane lists more than briefs; diagrams and images must be served with the same path refusals as /doc.
What: media_roots() names the folders searched; resolve_media() is the single gate for GET /media-file/<project>/<rel>
(extension allow-list, doc roots only, no dot / node_modules / target / secret names, realpath stays under the project
root, size cap); list_media() walks the roots and returns rows {type, project, path, title, date, size} for
type "diagram" (.mmd, .mermaid, .svg, and .md files with a mermaid block) and "image" (.png, .jpg, .jpeg, .svg).
`denied(segments, filename)` is passed in by dashboard-serve.py so the deny list stays in one place.
Ids (the owner 2026-10-22): each diagram or image file has a stable B id (prefix B, 2 hex digits, same rules as D/A/T) kept in
the store collection `media` (doc id = B id; fields project, path, status open|done). sync_ids(store, rows) allocates
ids for new files with dashboard_derive.next_id, writes each new doc with if_version 0 (a concurrent writer gets a
Conflict and the pass re-reads), marks a vanished file done (it keeps its id; retire_candidates lists it) and reopens it
if it returns (`accessed`, the ISO time the viewer last opened the file, is written by the page with PATCH /api/media/<id>
and kept by every rewrite here); when all 256 ids are used the oldest done media id is deleted and reused. media_rows() is the /media payload
with an `id` and, once the file was opened in the viewer, an `accessed` time on each row. SVG files listed under both types share one id. PRs, issues and briefs get no ids.
Test: tests/test_dashboard_media.py, tests/test_dashboard_media_ids.py
"""
import os
import re
import threading
from datetime import datetime, timezone

import dashboard_derive as derive
import dashboard_store as ds

MAX_MEDIA = 10 * 1024 * 1024
MAX_ROWS = 2000
MAX_MD = 1 << 20
MEDIA_TYPES = {".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".svg": "image/svg+xml",
               ".mmd": "text/plain; charset=utf-8", ".mermaid": "text/plain; charset=utf-8"}
DIAGRAM_EXT = (".mmd", ".mermaid", ".svg")
IMAGE_EXT = (".png", ".jpg", ".jpeg", ".svg")
_DATE = re.compile(r"^(\d{4}-\d{2}-\d{2})")
_MERMAID = re.compile(r"^```\s*mermaid\b", re.M)
SKIP_DIRS = ("node_modules", "target")


def media_roots(project):
    """Folders (relative to the project root) searched for media: docs, plus local/briefs for the architect."""
    return ("docs", "local/briefs") if project == "architect" else ("docs",)


def resolve_media(projects, project, segs, denied, md=False):
    """Real path of a servable media file (or, with md=True, a .md file) under a doc root, or None."""
    root = projects.get(project)
    if not root or not segs:
        return None
    for s in segs:
        if s in ("", ".", "..") or "/" in s or "\\" in s or "\x00" in s:
            return None
    ext = os.path.splitext(segs[-1])[1].lower()
    if (ext != ".md" if md else ext not in MEDIA_TYPES) or denied(segs, segs[-1]):
        return None
    root_real = os.path.realpath(root)
    real = os.path.realpath(os.path.join(root_real, *segs))
    if os.path.commonpath([root_real, real]) != root_real or not os.path.isfile(real):
        return None
    rsegs = os.path.relpath(real, root_real).split(os.sep)
    if denied(rsegs, rsegs[-1]) or os.path.splitext(rsegs[-1])[1].lower() != ext:
        return None
    rel = "/".join(rsegs)
    if not any(rel.startswith(r + "/") for r in media_roots(project)):
        return None
    if os.path.getsize(real) > (MAX_MD if md else MAX_MEDIA):
        return None
    return real


def _date(name, st):
    m = _DATE.match(name)
    return m.group(1) if m else datetime.fromtimestamp(st.st_mtime, timezone.utc).strftime("%Y-%m-%d")


def list_media(projects, types, denied):
    """Rows for the requested types ("diagram", "image"), newest first, capped at MAX_ROWS. SVG files appear under both."""
    out = []
    for project, root in projects.items():
        for sub in media_roots(project):
            base = os.path.join(root, *sub.split("/"))
            for dirpath, dirs, files in os.walk(base):
                dirs[:] = [d for d in dirs if not d.startswith(".") and d not in SKIP_DIRS]
                for name in files:
                    ext = os.path.splitext(name)[1].lower()
                    kinds = []
                    if "diagram" in types and ext in DIAGRAM_EXT:
                        kinds.append("diagram")
                    if "image" in types and ext in IMAGE_EXT:
                        kinds.append("image")
                    if "diagram" in types and ext == ".md":
                        kinds.append("diagram-md")
                    if not kinds:
                        continue
                    rel = os.path.relpath(os.path.join(dirpath, name), root).split(os.sep)
                    real = resolve_media(projects, project, rel, denied, md=(ext == ".md"))
                    if real is None:
                        continue
                    if ext == ".md":
                        with open(real, encoding="utf-8", errors="replace") as f:
                            text = f.read()
                        if not _MERMAID.search(text):
                            continue
                        h1 = re.search(r"^#\s+(.+?)\s*$", re.sub(r"\A---\n.*?\n---\n", "", text, flags=re.S), re.M)
                        title = h1.group(1) if h1 else name
                    else:
                        title = name
                    st = os.stat(real)
                    for k in kinds:
                        out.append({"type": "diagram" if k == "diagram-md" else k, "project": project, "path": "/".join(rel),
                                    "title": title, "date": _date(name, st), "size": st.st_size})
    out.sort(key=lambda r: (r["date"], r["path"], r["type"]), reverse=True)
    return out[:MAX_ROWS]


_ID_LOCK = threading.Lock()


def sync_ids(store, rows):
    """Ensure every (project, path) in rows has a media doc; return {(project, path): id} (id None only if no id can be freed).

    Marks docs whose file is absent from rows as done and reopens returning ones. rows must be the full listing
    (both types); a partial listing would mark the other type's files gone.
    """
    keys = sorted({(r["project"], r["path"]) for r in rows}, key=lambda k: (k[0], k[1]))
    with _ID_LOCK:
        for _ in range(12):
            snap = derive.snapshot(store, derive.ID_COLLECTIONS)
            have = {(d["data"].get("project"), d["data"].get("path")): d for d in snap["media"]}
            now, ops, planned = store.clock(), [], dict(snap)
            planned["media"] = list(snap["media"])
            full = False
            for key in keys:
                if key in have:
                    continue
                nid = derive.next_id(planned, "B")
                if nid is None:
                    gone = sorted((str(d["data"].get("done_at") or d["data"].get("updated") or ""), d["id"])
                                  for k, d in have.items() if k not in set(keys))[:1]   # absent files only, oldest first
                    if not gone:
                        full = True
                        break
                    store.delete("media", gone[0][1])   # logged to pruned.jsonl by the store
                    ops = None
                    break
                ops.append({"op": "set", "collection": "media", "id": nid, "if_version": 0,
                            "data": {"project": key[0], "path": key[1], "status": "open", "created": now, "updated": now}})
                planned["media"].append({"id": nid, "data": {}})
            if ops is None:
                continue
            present = set(keys)
            for key, d in have.items():
                is_done = d["data"].get("status") == "done"
                if key in present and is_done:
                    data = {k: v for k, v in d["data"].items() if k != "done_at"}
                    ops.append({"op": "set", "collection": "media", "id": d["id"], "if_version": d["version"],
                                "data": dict(data, status="open", updated=now)})
                elif key not in present and not is_done:
                    ops.append({"op": "set", "collection": "media", "id": d["id"], "if_version": d["version"],
                                "data": dict(d["data"], status="done", done_at=now, updated=now)})
            if ops:
                try:
                    store.batch(ops)
                except ds.Conflict:
                    continue   # another writer took an id or touched a doc: re-read and plan again
            ids = {k: d["id"] for k, d in have.items()}
            if ops:
                for op in ops:
                    if op["if_version"] == 0:
                        ids[(op["data"]["project"], op["data"]["path"])] = op["id"]
            if full:
                for k in keys:
                    ids.setdefault(k, None)
            return ids
    raise ds.Conflict("media ids: could not settle after 12 attempts")


def media_rows(store, projects, types, denied):
    """The /media payload: rows for `types` with a stable `id`. store None (session dash) leaves the rows without ids."""
    rows = list_media(projects, ("diagram", "image"), denied)
    if store is not None:
        ids = sync_ids(store, rows)
        seen = {(d["data"].get("project"), d["data"].get("path")): d["data"].get("accessed") for d in store.list_docs("media")}
        for r in rows:
            r["id"] = ids.get((r["project"], r["path"]))
            if seen.get((r["project"], r["path"])):
                r["accessed"] = seen[(r["project"], r["path"])]
    return [r for r in rows if r["type"] in types]
