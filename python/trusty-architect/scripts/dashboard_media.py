"""Media library for the dashboard's right pane: diagrams and images under each project's doc roots (the owner 2026-10-21).

Why: the right pane lists more than briefs; diagrams and images must be served with the same path refusals as /doc.
What: media_roots() names the folders searched; resolve_media() is the single gate for GET /media-file/<project>/<rel>
(extension allow-list, doc roots only, no dot / node_modules / target / secret names, realpath stays under the project
root, size cap); list_media() walks the roots and returns rows {type, project, path, title, date, size} for
type "diagram" (.mmd, .mermaid, .svg, and .md files with a mermaid block), "image" (.png, .jpg, .jpeg, .svg) and
"file" (.json, served as application/json; charset=utf-8; the page shows it as escaped text, never rendered).
`denied(segments, filename)` is passed in by dashboard-serve.py so the deny list stays in one place.
Ids (the owner 2026-10-22; 4 hex digits since 2026-10-26): each diagram or image file has a stable id "B" + 4 hex digits (B0000-Bffff)
kept in the store collection `media` (doc id = media id; fields project, path, status open|done). sync_ids(store, rows)
first runs backfill_ids (one-time, idempotent: a legacy 2-hex doc B1f becomes B001f), then allocates ids for new files
(first free after the highest used), writes each new doc with if_version 0 (a concurrent writer gets a Conflict and the
pass re-reads), marks a vanished file done (it keeps its id) and reopens it if it returns (`accessed`, the ISO time the
viewer last opened the file, is written by the page with PATCH /api/media/<id> and kept by every rewrite here).
archive_media(store, projects, id, denied) (POST /api/media-archive/<id>) moves the file to <media root>/archive/ (timestamp
suffix on a name clash), deletes the media doc so the id is free, and the listing skips that archive folder; nothing is deleted from disk. media_rows() is the /media payload
with an `id` and, once the file was opened in the viewer, an `accessed` time on each row. SVG files listed under both types share one id; .json files list as type "file" and get ids too. Briefs get ids too via brief_rows() (the owner 2026-10-27); PRs and issues get none (the page shows their own ref).
Test: tests/test_dashboard_media.py, tests/test_dashboard_media_ids.py, tests/test_dashboard_media_archive.py
"""
import os
import re
import threading
from datetime import datetime, timezone

import brief_ids
import dashboard_derive as derive
import dashboard_store as ds

MAX_MEDIA = 10 * 1024 * 1024
MAX_ROWS = 2000
MAX_MD = 1 << 20
MEDIA_TYPES = {".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".svg": "image/svg+xml",
               ".mmd": "text/plain; charset=utf-8", ".mermaid": "text/plain; charset=utf-8",
               ".json": "application/json; charset=utf-8"}
DIAGRAM_EXT = (".mmd", ".mermaid", ".svg")
IMAGE_EXT = (".png", ".jpg", ".jpeg", ".svg")
FILE_EXT = (".json",)   # type "file": shared data files such as a Slack app manifest (the owner 2026-10-29); shown as escaped text
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
    """Rows for the requested types ("diagram", "image", "file"), newest first, capped at MAX_ROWS. SVG files appear under both."""
    out = []
    for project, root in projects.items():
        for sub in media_roots(project):
            base = os.path.join(root, *sub.split("/"))
            for dirpath, dirs, files in os.walk(base):
                dirs[:] = [d for d in dirs if not d.startswith(".") and d not in SKIP_DIRS
                           and not (dirpath == base and d == ARCHIVE_DIR)]   # archived media leave the list
                for name in files:
                    ext = os.path.splitext(name)[1].lower()
                    kinds = []
                    if "diagram" in types and ext in DIAGRAM_EXT:
                        kinds.append("diagram")
                    if "image" in types and ext in IMAGE_EXT:
                        kinds.append("image")
                    if "file" in types and ext in FILE_EXT:
                        kinds.append("file")
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
MEDIA_ID = re.compile(r"^B[0-9a-f]{4}$")
LEGACY_ID = re.compile(r"^B([0-9a-f]{2})$")
MEDIA_SPACE = 1 << 16
ARCHIVE_DIR = "archive"


def _alloc(used):
    """Next free media id: the first free number after the highest used, wrapping past ffff; None when all 65536 are used."""
    top = max(used) if used else -1
    for k in range(1, MEDIA_SPACE + 1):
        c = (top + k) % MEDIA_SPACE
        if c not in used:
            return f"B{c:04x}"
    return None


def _used(docs, registry=None):
    """Taken B numbers: live media docs plus the brief registry (durable brief ids, never reused; the owner 2026-10-28)."""
    got = {int(d["id"][1:], 16) for d in docs if MEDIA_ID.match(d["id"])}
    if registry:
        got |= {brief_ids.number(r["id"]) for r in brief_ids.read_registry(registry) if brief_ids.BRIEF_ID_RE.match(r["id"])}
    return got


def _registry(store):
    """Path of local/briefs/registry.jsonl beside the store (local/owner-queue); a store elsewhere finds no file, so no ids are reserved."""
    return os.path.join(os.path.dirname(store.root), "briefs", "registry.jsonl")


def backfill_ids(store):
    """One-time, idempotent: move each legacy 2-hex media doc (B1f) to a 4-hex id (B001f) so every media item has one.

    Returns the number of docs renamed (0 once nothing legacy is left). A doc keeps all its fields; the old id is deleted
    in the same batch (logged to pruned.jsonl by the store). A taken target is skipped by allocating the next free id.
    """
    with _ID_LOCK:
        for _ in range(12):
            docs = store.list_docs("media")
            legacy = [d for d in docs if LEGACY_ID.match(d["id"])]
            if not legacy:
                return 0
            used, ops = _used(docs, _registry(store)), []
            for d in legacy:
                want = int(d["id"][1:], 16)
                nid = f"B{want:04x}" if want not in used else _alloc(used)
                if nid is None:
                    return 0
                used.add(int(nid[1:], 16))
                ops.append({"op": "set", "collection": "media", "id": nid, "if_version": 0, "data": dict(d["data"])})
                ops.append({"op": "delete", "collection": "media", "id": d["id"], "if_version": d["version"]})
            try:
                store.batch(ops)
            except ds.Conflict:
                continue
            return len(legacy)
    raise ds.Conflict("media ids: backfill could not settle after 12 attempts")


def sync_ids(store, rows):
    """Ensure every (project, path) in rows has a media doc; return {(project, path): id} (id None only if all 65536 are used).

    Ids are B + 4 hex digits, unique among the live media docs. Runs backfill_ids first. Marks docs whose file is absent
    from rows as done (they keep their id) and reopens returning ones. rows must be the full listing (both types); a
    partial listing would mark the other type's files gone.
    """
    keys = sorted({(r["project"], r["path"]) for r in rows}, key=lambda k: (k[0], k[1]))
    backfill_ids(store)
    with _ID_LOCK:
        for _ in range(12):
            docs = store.list_docs("media")
            have = {(d["data"].get("project"), d["data"].get("path")): d for d in docs}
            now, ops, used, new = store.clock(), [], _used(docs, _registry(store)), {}
            for key in keys:
                if key in have:
                    continue
                nid = _alloc(used)
                if nid is None:
                    break
                used.add(int(nid[1:], 16))
                new[key] = nid
                ops.append({"op": "set", "collection": "media", "id": nid, "if_version": 0,
                            "data": {"project": key[0], "path": key[1], "status": "open", "created": now, "updated": now}})
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
            ids.update(new)
            for k in keys:
                ids.setdefault(k, None)
            return ids
    raise ds.Conflict("media ids: could not settle after 12 attempts")


def _stamp():
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def archive_media(store, projects, media_id, denied):
    """Archive one media item: move its file to <media root>/archive/ (original name; a timestamp suffix on a clash), delete
    its media doc so the id is free again, and return {id, project, path, archived_to}. Nothing is deleted from disk.

    Raises ds.NotFound for an unknown id or a file that no longer resolves. If the doc delete fails the file is moved back.
    """
    doc = store.get("media", media_id)
    project, path = doc["data"].get("project"), str(doc["data"].get("path") or "")
    segs = path.split("/")
    real = resolve_media(projects, project, segs, denied, md=segs[-1].lower().endswith(".md"))
    if real is None:
        raise ds.NotFound(f"media file for {media_id} not found")
    root_real = os.path.realpath(projects[project])
    sub = next(r for r in media_roots(project) if path.startswith(r + "/"))
    adir = os.path.join(root_real, *sub.split("/"), ARCHIVE_DIR)
    os.makedirs(adir, exist_ok=True)
    dest = os.path.join(adir, os.path.basename(real))
    if os.path.exists(dest):
        stem, ext = os.path.splitext(os.path.basename(real))
        dest = os.path.join(adir, f"{stem}-{_stamp()}{ext}")
        n = 1
        while os.path.exists(dest):
            n += 1
            dest = os.path.join(adir, f"{stem}-{_stamp()}-{n}{ext}")
    with _ID_LOCK:
        os.replace(real, dest)
        try:
            store.delete("media", media_id, if_version=doc["version"])
        except Exception:
            os.replace(dest, real)
            raise
    return {"id": media_id, "project": project, "path": path,
            "archived_to": os.path.relpath(dest, root_real).replace(os.sep, "/")}


def media_rows(store, projects, types, denied, briefs=()):
    """The /media payload: rows for `types` with a stable `id`. store None (session dash) leaves the rows without ids.

    `briefs` ({project, path} rows from /briefs) join the id sync so a brief's media doc is not marked gone by a media-only pass.
    """
    rows = list_media(projects, ("diagram", "image", "file"), denied)
    if store is not None:
        ids = sync_ids(store, rows + [{"project": b["project"], "path": b["path"]} for b in briefs])
        seen = {(d["data"].get("project"), d["data"].get("path")): d["data"].get("accessed") for d in store.list_docs("media")}
        for r in rows:
            r["id"] = ids.get((r["project"], r["path"]))
            if seen.get((r["project"], r["path"])):
                r["accessed"] = seen[(r["project"], r["path"])]
    return [r for r in rows if r["type"] in types]


def brief_rows(store, projects, briefs, denied):
    """Add a stable B id to each brief row ({project, path, ...}) from the same `media` collection as diagrams (the owner 2026-10-27).

    A brief that is also a mermaid diagram shares one id. store None (session dash) leaves the rows without ids.
    """
    if store is None:
        return briefs
    ids = sync_ids(store, list_media(projects, ("diagram", "image", "file"), denied) + [{"project": b["project"], "path": b["path"]} for b in briefs])
    for b in briefs:
        b["id"] = b.get("brief_id") or ids.get((b["project"], b["path"]))   # a marked brief shows its durable id (the owner 2026-10-28)
    return briefs
