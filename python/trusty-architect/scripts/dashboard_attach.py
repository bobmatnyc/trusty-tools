"""Owner file uploads on dashboard asks, stored as Intent Ledger attachments (the owner 2026-10-25).

Why: the owner answers an ask (D decision, A action) with a file (screenshot, log, document); it must land beside the owning
project's Intent Ledger and be recorded on the item, so the Architect finds it when it reads the answer.
What: attach_dir() maps an item's session to `<IL dir>/attachments` (il-schedule.json name -> project; `.trusty-mpm/il`
when the project has one, the project itself when it is the Architect's IL dir, else `.trusty-mpm/datg/<session>`;
an unmapped session falls back to the Architect's local/il and the record says `il: "architect-fallback"`).
clean_name() refuses traversal (`/`, `\\`, `..`, NUL, control chars, absolute paths) and replaces other odd characters.
save() streams the body to a hidden temp file (O_NOFOLLOW, size cap enforced while reading, sha256 computed on the
way), then os.link()s it to `<dir>/<item id>/<name>` (never overwrites; a taken name gets `-2`, `-3`), refusing any
symlinked path component. record() appends {name, stored_name, size, sha256, uploaded_at, path, il} to the item's
`attachments` list, pinned with the version it just read (retried on conflict); on a failed write the file is removed.
Limits: MAX_BYTES (default 25 MiB, env DASH_ATTACH_MAX_MB), MAX_PER_ITEM 20.
The record also carries `type`, a content type derived from the stored extension (never from the client). serve_file()
answers GET /api/attach/<item id>/<stored_name>: the name is matched against the item's own `attachments` list, never
joined from the URL; raster images (png/jpeg/gif/webp) are served inline, everything else as an octet-stream download.
Test: scripts/test_dashboard_attach.py
"""
import hashlib
import os
import re
import tempfile
import unicodedata
from datetime import datetime, timezone

import dashboard_il as dil
import dashboard_store as ds

MAX_PER_ITEM = 20
MAX_NAME = 100
CHUNK = 1 << 16
ITEM_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$")
_BAD = re.compile(r"[^A-Za-z0-9._ ()+,=@-]")


INLINE_TYPES = {".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".gif": "image/gif", ".webp": "image/webp"}
OTHER_TYPES = {".txt": "text/plain", ".md": "text/markdown", ".json": "application/json", ".pdf": "application/pdf", ".csv": "text/csv", ".log": "text/plain"}


def content_type(name):
    """Content type for a stored name, by extension only; unknown = application/octet-stream."""
    ext = os.path.splitext(name)[1].lower()
    return INLINE_TYPES.get(ext) or OTHER_TYPES.get(ext) or "application/octet-stream"


class TooLarge(ds.StoreError):
    status = 413
    code = "too_large"


def max_bytes():
    """Upload cap in bytes: DASH_ATTACH_MAX_MB (positive number) else 25 MiB. Read per call."""
    try:
        mb = float(os.environ.get("DASH_ATTACH_MAX_MB", "25"))
        return int(mb * 1024 * 1024) if mb > 0 else 25 << 20
    except ValueError:
        return 25 << 20


def clean_name(raw):
    """A safe single-component file name, or raise StoreError. Names with path parts are refused, not trimmed."""
    if not isinstance(raw, str) or not raw.strip():
        raise ds.StoreError("file name required")
    name = unicodedata.normalize("NFC", raw).strip()
    if "/" in name or "\\" in name or "\x00" in name or ".." in name or any(ord(c) < 32 or ord(c) == 127 for c in name):
        raise ds.StoreError("file name must not contain a path, '..' or control characters")
    if re.match(r"^[A-Za-z]:", name):
        raise ds.StoreError("file name must not be an absolute path")
    name = _BAD.sub("_", name).lstrip(". ").rstrip(". ")
    if not name:
        raise ds.StoreError("file name has no usable characters")
    if len(name) > MAX_NAME:
        stem, ext = os.path.splitext(name)
        name = stem[: MAX_NAME - len(ext[:16])] + ext[:16]
    return name


def attach_dir(session, schedule_path, architect_il, store_root=None):
    """(attachments dir, il label). store_root set = session-mode server: the dir beside that PM's store."""
    if store_root:
        return os.path.join(os.path.dirname(os.path.abspath(store_root)), "attachments"), "session"
    for e in dil._read_schedule(schedule_path):
        if e["name"] == session:
            proj = os.path.abspath(e["project"])
            if os.path.isdir(os.path.join(proj, ".trusty-mpm", "il")):
                return os.path.join(proj, ".trusty-mpm", "il", "attachments"), "project"
            if proj == os.path.abspath(architect_il):
                return os.path.join(proj, "attachments"), "project"
            if dil.SESSION_RE.match(session):
                return os.path.join(proj, ".trusty-mpm", "datg", session, "attachments"), "project"
    return os.path.join(os.path.abspath(architect_il), "attachments"), "architect-fallback"


def _safe_dir(base, item_id):
    """Create base/<item_id> without following a symlink at any component below an existing ancestor."""
    target = os.path.join(base, item_id)
    anc = base
    while not os.path.lexists(anc):
        anc = os.path.dirname(anc)
    for p in (anc, base, target):   # an existing ancestor must be a real directory, not a link
        if os.path.lexists(p) and (os.path.islink(p) or not os.path.isdir(p)):
            raise ds.StoreError("attachment path is not a plain directory")
    os.makedirs(target, mode=0o755, exist_ok=True)
    probe = anc
    for part in os.path.relpath(target, anc).split(os.sep):
        if part in ("", "."):
            continue
        probe = os.path.join(probe, part)
        if os.path.islink(probe):
            raise ds.StoreError("attachment path is not a plain directory")
    if os.path.realpath(target) != os.path.join(os.path.realpath(anc), os.path.relpath(target, anc)):
        raise ds.StoreError("attachment path resolves outside its directory")
    return target


def save(rfile, length, name, directory, item_id, limit):
    """Stream `length` bytes from rfile into directory/<item_id>/<name>; returns (stored_name, path, size, sha256)."""
    if length <= 0:
        raise ds.StoreError("empty upload")
    if length > limit:
        raise TooLarge(f"file is {length} bytes; the limit is {limit}")
    target = _safe_dir(directory, item_id)
    fd, tmp = tempfile.mkstemp(prefix=".upload-", dir=target)
    h, got = hashlib.sha256(), 0
    try:
        with os.fdopen(fd, "wb") as out:
            while got < length:
                buf = rfile.read(min(CHUNK, length - got))
                if not buf:
                    raise ds.StoreError("upload ended before Content-Length bytes")
                got += len(buf)
                h.update(buf)
                out.write(buf)
            out.flush()
            os.fsync(out.fileno())
        stem, ext = os.path.splitext(name)
        stored, n = name, 1
        while True:
            try:
                os.link(tmp, os.path.join(target, stored))   # atomic, refuses to overwrite
                break
            except FileExistsError:
                n += 1
                stored = f"{stem}-{n}{ext}"
                if n > 999:
                    raise ds.StoreError("too many files with this name")
        return stored, os.path.join(target, stored), got, h.hexdigest()
    finally:
        try:
            os.unlink(tmp)
        except OSError:
            pass


def record(store, item_id, entry, actor=None):
    """Append entry to items/<id>.attachments, pinned to the version just read; retry on conflict. Returns the doc."""
    for _ in range(5):
        doc = store.get("items", item_id)
        cur = doc["data"].get("attachments")
        cur = list(cur) if isinstance(cur, list) else []
        if len(cur) >= MAX_PER_ITEM:
            raise ds.StoreError(f"at most {MAX_PER_ITEM} attachments per item")
        try:
            return store.update("items", item_id, {"attachments": cur + [entry]}, if_version=doc["version"], actor=actor)
        except ds.Conflict:
            continue
    raise ds.Conflict("item changed repeatedly; retry the upload")


def now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def handle_upload(store, item_id, raw_name, rfile, length, schedule_path, architect_il, store_root=None, actor=None):
    """Whole upload for POST /api/attach/<item id>?name=: checks, save, record. Returns the response dict."""
    if not ITEM_ID.match(item_id or ""):
        raise ds.StoreError("bad item id")
    name = clean_name(raw_name)
    item = store.get("items", item_id)["data"]
    if item.get("status") not in ("open", "answered"):
        raise ds.StoreError("uploads are accepted on open or answered items only")
    if item.get("kind") not in ("question", "action"):
        raise ds.StoreError("uploads are accepted on decisions and actions only")
    if len(item.get("attachments") or []) >= MAX_PER_ITEM:
        raise ds.StoreError(f"at most {MAX_PER_ITEM} attachments per item")
    directory, il = attach_dir(item.get("session") or "", schedule_path, architect_il, store_root)
    stored, path, size, sha = save(rfile, length, name, directory, item_id, max_bytes())
    entry = {"name": name, "stored_name": stored, "size": size, "sha256": sha, "uploaded_at": now_iso(), "path": path, "il": il, "type": content_type(stored)}
    try:
        doc = record(store, item_id, entry, actor)
    except Exception:
        try:
            os.unlink(path)
        except OSError:
            pass
        raise
    return {"attachment": entry, "version": doc["version"]}


def serve_file(store, item_id, stored_name):
    """(bytes, content type, inline?) for one recorded attachment, or raise ds.NotFound. The name must equal a
    stored_name on the item; the recorded path must be a regular, unlinked file in <...>/attachments/<item id>/."""
    if not ITEM_ID.match(item_id or ""):
        raise ds.NotFound("no such attachment")
    for a in store.get("items", item_id)["data"].get("attachments") or []:
        if isinstance(a, dict) and a.get("stored_name") == stored_name and isinstance(a.get("path"), str):
            path = a["path"]
            parent = os.path.dirname(path)
            if (os.path.basename(path) != stored_name or os.path.basename(parent) != item_id
                    or os.path.basename(os.path.dirname(parent)) != "attachments"
                    or os.path.islink(path) or not os.path.isfile(path)
                    or os.path.realpath(path) != os.path.join(os.path.realpath(parent), stored_name)):
                break
            with open(path, "rb") as f:
                data = f.read(max_bytes() + 1)
            if len(data) > max_bytes():
                break
            ctype = content_type(stored_name)
            return data, ctype, os.path.splitext(stored_name)[1].lower() in INLINE_TYPES
    raise ds.NotFound("no such attachment")
