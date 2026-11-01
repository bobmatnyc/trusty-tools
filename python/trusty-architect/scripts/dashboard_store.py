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
batch() stages every operation in memory first and restores the previous bytes if a write
fails midway, so it is all-or-nothing. Reads take no lock (replace is atomic).
Format: DASHBOARD_FORMAT=dir (default, the layout above) or jsonl (one canonical <coll>.jsonl per collection plus
changes.jsonl, see dashboard_jsonl.py). A jsonl write renders the whole collection file and validates every touched
document against schemas/dashboard/ (DASHBOARD_SCHEMA=off|warn|enforce; default warn for jsonl, off for dir).
A store of the other format is refused loudly, never read as empty.
No network, no secrets.
Test: tests/test_dashboard_store.py, tests/test_dashboard_jsonl.py
"""
import copy
import fcntl
import json
import os
import re
import sys
import uuid
from contextlib import contextmanager
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)
import dashboard_jsonl as dj  # noqa: E402
import dashboard_schema as dsch  # noqa: E402

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


class SchemaInvalid(StoreError):
    """A document fails its schema (DASHBOARD_SCHEMA=enforce). `errors` lists every problem."""
    status = 400
    code = "schema_invalid"

    def __init__(self, errors):
        super().__init__("; ".join(errors[:5]) + (f" (+{len(errors) - 5} more)" if len(errors) > 5 else ""))
        self.errors = list(errors)


FORMATS = ("dir", "jsonl")


def default_format(env=None):
    value = (env if env is not None else os.environ).get("DASHBOARD_FORMAT") or "dir"
    if value not in FORMATS:
        raise StoreError(f"DASHBOARD_FORMAT={value!r}: use dir or jsonl")
    return value


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


_MINUTE_TS = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}Z$")
_TS_KEYS = {"created", "updated", "due", "at", "until", "eta", "eta_low", "eta_high"}


def normalize_timestamps(value, key=None):
    """Return value with every minute-precision timestamp (2026-10-12T03:46Z) in a timestamp field given seconds.
    Why: the schemas want HH:MM:SS; the Architect's hand-typed CLI writes used HH:MMZ and made validate fail (2026-10-12).
    What: recurse dicts and lists; rewrite a string only under created/updated/due/at/until/eta* or a key ending _at,
    never free text (title, detail, answer). Test: tests/test_dashboard_store_guards.py"""
    if isinstance(value, dict):
        return {k: normalize_timestamps(v, k) for k, v in value.items()}
    if isinstance(value, list):
        return [normalize_timestamps(v, key) for v in value]
    if isinstance(value, str) and key and (key in _TS_KEYS or key.endswith("_at")) and _MINUTE_TS.match(value):
        return value[:-1] + ":00Z"
    return value


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


def _refuse_live_write_under_test(root):
    """A test run must never write the live store, whatever conftest did or did not set.
    Why: tests that ran without scripts/conftest.py wrote "exited" over every session doc (2026-10-09).
    What: raise when pytest is loaded and root is the real local/owner-queue.
    Test: tests/test_dashboard_store_guards.py"""
    if "pytest" in sys.modules and os.path.realpath(root) == os.path.realpath(DEFAULT_ROOT):
        raise StoreError(f"refusing to write the live store {root} under pytest")


def _restore_on_brief(data):
    """An item awaiting a brief (status brief_pending, the owner 2026-10-26) returns to open when a write links the brief."""
    b = data.get("brief")
    if data.get("status") == "brief_pending" and isinstance(b, str) and b.strip().endswith(".md") and "/" in b:
        return {**data, "status": "open"}
    return data


class Store:
    def __init__(self, root=DEFAULT_ROOT, fmt=None, actor=None, schema_mode=None, clock=now_iso, prune_log=None):
        self.root = os.path.abspath(root)
        self._prune_log = prune_log   # None: <root>/../owner-queue-archive/pruned.jsonl; a session store passes a path under its own root
        self.fmt = fmt or default_format()
        if self.fmt not in FORMATS:
            raise StoreError(f"unknown store format {self.fmt!r}: use dir or jsonl")
        self.actor = actor or os.environ.get("DASHBOARD_ACTOR") or "unknown"
        self.schema_mode = schema_mode or dsch.mode() or ("warn" if self.fmt == "jsonl" else "off")
        self.clock = clock
        self._cache = {}     # jsonl: coll -> ((mtime_ns, size, inode), {id: (version, data)})

    # ---- paths and reads -------------------------------------------------
    def _path(self, coll, doc_id):
        return os.path.join(self.root, _check_id("collection", coll), _check_id("doc id", doc_id) + ".json")

    def _guard(self, coll):
        """Refuse a store laid out in the other format instead of reading it as empty."""
        _check_id("collection", coll)
        has_jsonl = os.path.exists(dj.coll_path(self.root, coll))
        cdir = os.path.join(self.root, coll)
        if self.fmt == "dir" and has_jsonl:
            raise StoreError(f"{self.root} holds {coll}.jsonl: set DASHBOARD_FORMAT=jsonl")
        if self.fmt == "jsonl":
            if dj.RESERVED.match(coll):
                raise StoreError(f"collection name {coll!r} is reserved for the change log")
            if not has_jsonl and os.path.isdir(cdir) and any(n.endswith(".json") for n in os.listdir(cdir)):
                raise StoreError(f"{cdir} holds per-document files and no {coll}.jsonl: run scripts/dashboard-migrate.py")

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

    def _jcoll(self, coll):
        """jsonl: {id: (version, data)} for a collection through an mtime/size/inode parse cache. Do not mutate."""
        self._guard(coll)
        path = dj.coll_path(self.root, coll)
        try:
            st = os.stat(path)
        except FileNotFoundError:
            return {}
        sig = (st.st_mtime_ns, st.st_size, st.st_ino)
        hit = self._cache.get(coll)
        if hit and hit[0] == sig:
            return hit[1]
        try:
            docs = dj.read_coll(self.root, coll)
        except dj.JsonlError as e:
            raise StoreError(str(e))
        self._cache[coll] = (sig, docs)
        return docs

    def get(self, coll, doc_id):
        """Return {"id","version","data"}; raise NotFound when absent."""
        if self.fmt == "jsonl":
            _check_id("doc id", doc_id)
            found = self._jcoll(coll).get(doc_id)
            found = (found[0], copy.deepcopy(found[1])) if found else None
        else:
            self._guard(coll)
            found = self._load(self._path(coll, doc_id))
        if found is None:
            raise NotFound(f"{coll}/{doc_id} not found")
        return {"id": doc_id, "version": found[0], "data": found[1]}

    def collections(self):
        """Names of the collections that exist (jsonl: *.jsonl files; dir: directories holding *.json documents)."""
        if self.fmt == "jsonl":
            return dj.collections(self.root)
        try:
            names = sorted(os.listdir(self.root))
        except FileNotFoundError:
            return []
        return [n for n in names if not n.startswith(".") and not n.endswith(".json.d") and os.path.isdir(os.path.join(self.root, n))
                and any(f.endswith(".json") and not f.startswith(".") for f in os.listdir(os.path.join(self.root, n)))]

    def _dir_docs(self, coll):
        cdir = os.path.join(self.root, coll)
        try:
            names = sorted(n for n in os.listdir(cdir) if n.endswith(".json") and not n.startswith("."))
        except FileNotFoundError:
            return []
        out = []
        for n in names:
            found = self._load(os.path.join(cdir, n))
            if found is not None:
                out.append({"id": n[:-5], "version": found[0], "data": found[1]})
        return out

    def list_docs(self, coll, status=None, order_by=None, desc=False, limit=None):
        """Documents of a collection (empty when it is absent), id order by default."""
        self._guard(coll)
        if self.fmt == "jsonl":
            cur = self._jcoll(coll)
            docs = [{"id": i, "version": cur[i][0], "data": copy.deepcopy(cur[i][1])} for i in dj.sort_ids(cur)]
        else:
            docs = self._dir_docs(coll)
        if status is not None:
            docs = [d for d in docs if d["data"].get("status") == status]
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

    def _current(self, coll, doc_id, fresh):
        """(version, data) or None as of now, read under the lock (jsonl: once per batch per collection)."""
        self._guard(coll)
        if self.fmt == "dir":
            return self._load(self._path(coll, doc_id))
        _check_id("doc id", doc_id)
        if coll not in fresh:
            try:
                fresh[coll] = dj.read_coll(self.root, coll)
            except dj.JsonlError as e:
                raise StoreError(str(e))
        return fresh[coll].get(doc_id)

    def _stage(self, staged, op, fresh):
        kind = op.get("op")
        if kind not in ("set", "update", "delete"):
            raise StoreError(f"unknown op {kind!r}: use set, update or delete")
        coll, doc_id = op.get("collection"), op.get("id")
        key = (coll, doc_id)
        path = self._path(coll, doc_id)
        if key not in staged:
            found = self._current(coll, doc_id, fresh)
            staged[key] = {"path": path, "exists": found is not None,
                           "version": found[0] if found else 0,
                           "data": found[1] if found else None, "dirty": False, "old": found}
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
        data = normalize_timestamps(_check_body(op.get("data")))
        if kind == "update":
            if not cur["exists"]:
                raise NotFound(f"{coll}/{doc_id} not found")
            data = {**cur["data"], **data}
        if coll == "items" and cur["exists"] and (cur["data"] or {}).get("status") == "brief_pending":
            data = _restore_on_brief(data)
        cur.update(exists=True, data=dict(data), dirty=True, version=cur["version"] + 1)
        return {"collection": coll, "id": doc_id, "version": cur["version"]}

    def _commit(self, staged):
        plan = [(st["path"], _render(st["version"], st["data"]) if st["exists"] else None)
                for st in staged.values() if st["dirty"]]
        self._apply([(path, payload) for path, payload in plan])

    @staticmethod
    def _apply(plan, fsync_dirs=False):
        """Write (path, payload|None) pairs in order; on any failure restore every earlier path's old bytes."""
        backups = []
        try:
            for path, payload in plan:
                old = None
                if os.path.exists(path):
                    with open(path, "rb") as f:
                        old = f.read()
                backups.append((path, old))
                if payload is None:
                    if old is not None:
                        os.unlink(path)
                else:
                    os.makedirs(os.path.dirname(path), exist_ok=True)
                    _atomic_write(path, payload)
                if fsync_dirs:
                    dj.fsync_dir(os.path.dirname(path))
        except BaseException:
            for path, old in reversed(backups):
                if old is None:
                    if os.path.exists(path):
                        os.unlink(path)
                else:
                    _atomic_write(path, old)
            raise

    # ---- jsonl commit ----------------------------------------------------
    def _check_schema(self, staged):
        """Validate each touched, existing document. Returns the error list; raises in enforce mode."""
        if self.schema_mode == "off":
            return []
        errs = []
        for (coll, doc_id), st in sorted(staged.items()):
            if st["dirty"] and st["exists"]:
                errs.extend(dsch.validate(coll, doc_id, st["data"]))
        if errs and self.schema_mode == "enforce":
            raise SchemaInvalid(errs)
        for e in errs:
            sys.stderr.write(f"dashboard-store: schema warning: {e}\n")
        return errs

    def _commit_jsonl(self, staged, fresh, actor, op_of):
        warnings = self._check_schema(staged)
        by_coll = {}
        for (coll, doc_id), st in staged.items():
            if st["dirty"]:
                by_coll.setdefault(coll, {})[doc_id] = st
        plan, entries = [], []
        for coll in sorted(by_coll):
            docs = dict(fresh.get(coll, {}))
            for doc_id, st in by_coll[coll].items():
                if st["exists"]:
                    docs[doc_id] = (st["version"], st["data"])
                else:
                    docs.pop(doc_id, None)
            try:
                payload = dj.render_file(docs) or None
            except dj.JsonlError as e:
                raise StoreError(f"{coll}: {e}")
            plan.append((dj.coll_path(self.root, coll), payload))
            for doc_id in dj.sort_ids(by_coll[coll]):
                st = by_coll[coll][doc_id]
                old = st["old"][1] if st["old"] else None
                if st["exists"]:
                    line = dj.render_line(doc_id, st["version"], st["data"])
                    entries.append({"actor": actor, "coll": coll, "id": doc_id, "keys": dj.changed_keys(old, st["data"]),
                                    "op": op_of[(coll, doc_id)], "sha256": dj.sha256(line), "version": st["version"]})
                else:
                    entries.append({"actor": actor, "coll": coll, "id": doc_id, "keys": [], "op": "delete",
                                    "sha256": None, "version": st["old"][0] if st["old"] else 0})
        self._apply(plan, fsync_dirs=True)
        if warnings:
            for e in entries:
                mine = [w for w in warnings if w.startswith(f"{e['coll']}/{e['id']}:")]
                if mine:
                    e["warnings"] = mine
        try:
            dj.append_changes(self.root, entries, self.clock())
        except (OSError, ValueError) as e:
            sys.stderr.write(f"dashboard-store: change log not written: {e}\n")

    def prune_log_path(self):
        """Append-only prune log, outside the served root: <root>/../owner-queue-archive/pruned.jsonl."""
        return self._prune_log or os.path.join(os.path.dirname(self.root), "owner-queue-archive", "pruned.jsonl")

    def _log_pruned(self, staged, actor):
        """Append one line per deleted existing document (full pre-delete data), flush + fsync. Raises StoreError on failure."""
        recs = []
        for (coll, doc_id), st in staged.items():
            if st["dirty"] and not st["exists"] and st["old"] is not None:
                recs.append(json.dumps({"pruned_at": self.clock(), "actor": actor, "collection": coll, "id": doc_id,
                                        "version": st["old"][0], "data": st["old"][1]},
                                       sort_keys=True, ensure_ascii=False) + "\n")
        if not recs:
            return
        path = self.prune_log_path()
        try:
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "a", encoding="utf-8") as f:
                f.write("".join(recs))
                f.flush()
                os.fsync(f.fileno())
        except OSError as e:
            raise StoreError(f"prune log not written ({path}): {e}; delete refused")

    def batch(self, ops, actor=None):
        """Apply ops (list of dicts: op, collection, id, data, if_version) all-or-nothing."""
        if not isinstance(ops, list) or not ops:
            raise StoreError("batch needs a non-empty list of ops")
        _refuse_live_write_under_test(self.root)
        with self._lock():
            staged, results, fresh, op_of = {}, [], {}, {}
            for op in ops:
                if not isinstance(op, dict):
                    raise StoreError("each batch op must be an object")
                results.append(self._stage(staged, op, fresh))
                op_of[(op.get("collection"), op.get("id"))] = op.get("op")
            self._log_pruned(staged, actor or self.actor)   # fail closed: no log line, no delete
            if self.fmt == "jsonl":
                self._commit_jsonl(staged, fresh, actor or self.actor, op_of)
            else:
                self._commit(staged)
        return results

    def set(self, coll, doc_id, data, if_version=None, actor=None):
        return self.batch([{"op": "set", "collection": coll, "id": doc_id, "data": data, "if_version": if_version}], actor)[0]

    def update(self, coll, doc_id, data, if_version=None, actor=None):
        return self.batch([{"op": "update", "collection": coll, "id": doc_id, "data": data, "if_version": if_version}], actor)[0]

    def delete(self, coll, doc_id, if_version=None, actor=None):
        return self.batch([{"op": "delete", "collection": coll, "id": doc_id, "if_version": if_version}], actor)[0]

    def add(self, coll, data, actor=None):
        """Create a document under a generated id (the page's collection.add)."""
        doc_id = uuid.uuid4().hex[:20]
        return self.set(coll, doc_id, data, if_version=0, actor=actor)

    # ---- seed ------------------------------------------------------------
    def seed_meta(self, state_md=DEFAULT_STATE_MD, owner_name="the owner"):
        """Create meta/state when absent (owner_name only; next ids are computed from the store). True if created."""
        # state_md is kept for callers; the NEXT IDS line is no longer read (scripts/dashboard_ids.py, the page).
        body = {"owner_name": owner_name, "updated": now_iso()}
        try:
            self.set("meta", "state", body, if_version=0)
            return True
        except Conflict:
            return False


# #10570: last in the module: pm-guard reads this value, then any later backtick, as a run-time 1Password call.
BATCH_VERB_KEY = "op"   # a batch entry's verb key; dashboard-db.py also accepts "action" (#10571)
