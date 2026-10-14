#!/usr/bin/env python3
"""dashboard_store.py: file-backed document store for the Architect dashboard (stdlib only).

Why: the dashboard left the claude.ai artifact runtime (the owner, 2026-10-08 17:34Z: use a
localhost URL). The page and the Architect both need the same documents, and the
Architect must be able to write them while the web server is down.
What: one JSON file per document at <root>/<collection>/<doc_id>.json (root defaults to
local/owner-queue, the ArtifactData mirror). A file holds the document body plus an
integer "version"; a file with no "version" (the old mirror shape) counts as version 1.
Every write takes an exclusive flock on <root>/.lock, checks the optional if_version
(0 = must not exist), bumps the version and replaces files with temp file + os.replace.
batch() stages every op in memory first and restores the previous bytes if a write
fails midway, so it is all-or-nothing. Reads take no lock (replace is atomic).
No network, no secrets.
Test: scripts/test_dashboard_store.py
"""
import fcntl
import json
import os
import re
import uuid
from contextlib import contextmanager
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
DEFAULT_ROOT = os.path.join(REPO, "local", "owner-queue")
DEFAULT_STATE_MD = os.path.join(REPO, "local", "state.md")

ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,79}$")


class StoreError(Exception):
    """Invalid request (bad id, bad body)."""
    status = 400
    code = "invalid_argument"


class NotFound(StoreError):
    status = 404
    code = "not_found"


class Conflict(StoreError):
    status = 409
    code = "conflict"


def now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _check_id(kind, value):
    if not isinstance(value, str) or not ID_RE.match(value):
        raise StoreError(f"invalid {kind} {value!r}: use letters, digits, '_', '-', '.', max 80, no leading '.'")
    return value


def _check_body(data):
    if not isinstance(data, dict):
        raise StoreError("document body must be a JSON object")
    if "version" in data:
        raise StoreError("'version' is reserved for the store")
    try:
        json.dumps(data)
    except (TypeError, ValueError) as e:
        raise StoreError(f"document body is not JSON-serializable: {e}")
    return data


def _render(version, data):
    body = dict(data)
    body["version"] = version
    return (json.dumps(body, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode("utf-8")


def _atomic_write(path, payload):
    """Write payload to path through a temp file in the same directory."""
    tmp = os.path.join(os.path.dirname(path), f".{os.path.basename(path)}.{os.getpid()}.{uuid.uuid4().hex[:8]}.tmp")
    try:
        with open(tmp, "wb") as f:
            f.write(payload)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


def _sort_key(value):
    if isinstance(value, bool) or value is None:
        return (3, str(value))
    if isinstance(value, (int, float)):
        return (0, value)
    if isinstance(value, str):
        return (1, value)
    return (2, json.dumps(value, sort_keys=True))


class Store:
    def __init__(self, root=DEFAULT_ROOT):
        self.root = os.path.abspath(root)

    # ---- paths and reads -------------------------------------------------
    def _path(self, coll, doc_id):
        return os.path.join(self.root, _check_id("collection", coll), _check_id("doc id", doc_id) + ".json")

    def _load(self, path):
        """Return (version, data) or None when the file is absent."""
        try:
            with open(path, "r", encoding="utf-8") as f:
                raw = json.load(f)
        except FileNotFoundError:
            return None
        except (OSError, ValueError) as e:
            raise StoreError(f"unreadable document {path}: {e}")
        if not isinstance(raw, dict):
            raise StoreError(f"document {path} is not a JSON object")
        version = raw.pop("version", 1)
        if not isinstance(version, int) or isinstance(version, bool) or version < 1:
            version = 1
        return version, raw

    def get(self, coll, doc_id):
        """Return {"id","version","data"}; raise NotFound when absent."""
        found = self._load(self._path(coll, doc_id))
        if found is None:
            raise NotFound(f"{coll}/{doc_id} not found")
        return {"id": doc_id, "version": found[0], "data": found[1]}

    def list_docs(self, coll, status=None, order_by=None, desc=False, limit=None):
        """Documents of a collection (empty when the directory is absent), id order by default."""
        _check_id("collection", coll)
        cdir = os.path.join(self.root, coll)
        try:
            names = sorted(n for n in os.listdir(cdir) if n.endswith(".json") and not n.startswith("."))
        except FileNotFoundError:
            return []
        docs = []
        for n in names:
            found = self._load(os.path.join(cdir, n))
            if found is None:
                continue
            if status is not None and found[1].get("status") != status:
                continue
            docs.append({"id": n[:-5], "version": found[0], "data": found[1]})
        if order_by:
            present = [d for d in docs if order_by in d["data"]]
            absent = [d for d in docs if order_by not in d["data"]]
            present.sort(key=lambda d: _sort_key(d["data"][order_by]), reverse=bool(desc))
            docs = present + absent
        if limit is not None:
            docs = docs[: max(0, int(limit))]
        return docs

    # ---- writes ----------------------------------------------------------
    @contextmanager
    def _lock(self):
        os.makedirs(self.root, exist_ok=True)
        with open(os.path.join(self.root, ".lock"), "a") as lf:
            fcntl.flock(lf, fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lf, fcntl.LOCK_UN)

    def _stage(self, staged, op):
        kind = op.get("op")
        if kind not in ("set", "update", "delete"):
            raise StoreError(f"unknown op {kind!r}: use set, update or delete")
        coll, doc_id = op.get("collection"), op.get("id")
        key = (coll, doc_id)
        path = self._path(coll, doc_id)
        if key not in staged:
            found = self._load(path)
            staged[key] = {"path": path, "exists": found is not None,
                           "version": found[0] if found else 0,
                           "data": found[1] if found else None, "dirty": False}
        cur = staged[key]
        expected = op.get("if_version")
        if expected is not None:
            if isinstance(expected, bool) or not isinstance(expected, int):
                raise StoreError("if_version must be an integer")
            have = cur["version"] if cur["exists"] else 0
            if expected != have:
                raise Conflict(f"{coll}/{doc_id}: if_version {expected} but current version is {have}")
        if kind == "delete":
            if not cur["exists"]:
                raise NotFound(f"{coll}/{doc_id} not found")
            cur.update(exists=False, data=None, dirty=True)
            return {"collection": coll, "id": doc_id, "deleted": True}
        data = _check_body(op.get("data"))
        if kind == "update":
            if not cur["exists"]:
                raise NotFound(f"{coll}/{doc_id} not found")
            data = {**cur["data"], **data}
        cur.update(exists=True, data=dict(data), dirty=True, version=cur["version"] + 1)
        return {"collection": coll, "id": doc_id, "version": cur["version"]}

    def _commit(self, staged):
        plan = [(st["path"], _render(st["version"], st["data"]) if st["exists"] else None)
                for st in staged.values() if st["dirty"]]
        backups = []
        try:
            for path, payload in plan:
                old = None
                if os.path.exists(path):
                    with open(path, "rb") as f:
                        old = f.read()
                backups.append((path, old))
                if payload is None:
                    os.unlink(path)
                else:
                    os.makedirs(os.path.dirname(path), exist_ok=True)
                    _atomic_write(path, payload)
        except BaseException:
            for path, old in reversed(backups):
                if old is None:
                    if os.path.exists(path):
                        os.unlink(path)
                else:
                    _atomic_write(path, old)
            raise

    def batch(self, ops):
        """Apply ops (list of dicts: op, collection, id, data, if_version) all-or-nothing."""
        if not isinstance(ops, list) or not ops:
            raise StoreError("batch needs a non-empty list of ops")
        with self._lock():
            staged, results = {}, []
            for op in ops:
                if not isinstance(op, dict):
                    raise StoreError("each batch op must be an object")
                results.append(self._stage(staged, op))
            self._commit(staged)
        return results

    def set(self, coll, doc_id, data, if_version=None):
        return self.batch([{"op": "set", "collection": coll, "id": doc_id, "data": data, "if_version": if_version}])[0]

    def update(self, coll, doc_id, data, if_version=None):
        return self.batch([{"op": "update", "collection": coll, "id": doc_id, "data": data, "if_version": if_version}])[0]

    def delete(self, coll, doc_id, if_version=None):
        return self.batch([{"op": "delete", "collection": coll, "id": doc_id, "if_version": if_version}])[0]

    def add(self, coll, data):
        """Create a document under a generated id (the page's collection.add)."""
        doc_id = uuid.uuid4().hex[:20]
        return self.set(coll, doc_id, data, if_version=0)

    # ---- seed ------------------------------------------------------------
    def seed_meta(self, state_md=DEFAULT_STATE_MD, owner_name="Owner"):
        """Create meta/state when absent (owner_name only; next ids are computed from the store). True if created."""
        # state_md is kept for callers; the NEXT IDS line is no longer read (scripts/dashboard_ids.py, the page).
        body = {"owner_name": owner_name, "updated": now_iso()}
        try:
            self.set("meta", "state", body, if_version=0)
            return True
        except Conflict:
            return False
