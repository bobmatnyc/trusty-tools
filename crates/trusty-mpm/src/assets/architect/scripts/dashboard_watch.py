"""dashboard_watch.py: each registered project's watch state for the Projects view (GET /api/projects), read-only.

Why: #10896 (epic #10894 plan D6): the Projects view shows whether the Architect watches each project and queues Watch
and Unwatch requests (watch_changes docs). `tm fleet project add|remove`, run by scripts/watch-apply.py, is the only
writer of local/watch.json, so this module only reads it.
What: read_watch() reads <arch_dir>/local/watch.json directly. It never runs `tm fleet project list`, which creates the
file when it is absent. Absent -> None, shown as "watch list not created yet", never "Not watched"; unreadable,
malformed or another version -> WatchListError, never an empty set. state() decides one key: a `new` request -> pending;
else the newest `failed` request, when no `applied` one is newer -> failed (a dismissed request never counts); else
list membership -> watched | not_watched, or unknown while the list is absent. alias_for() is tm's registry join
(Registry::alias_for in tm's fleet project registry): exact alias, then the alias
equal ignoring ASCII case, then the alias row whose path is the project's managed checkout. flags() gives path_exists
and alias_mismatch the way `tm fleet project list --json` does. overlay() adds `watch` to every row of an answer; the
Projects view calls it on each request, outside its answer cache, so a just-written request shows at once.
Test: tests/test_dashboard_watch.py, tests/test_dashboard_watch_http.py
"""
import json
import os

WATCH = os.path.join("local", "watch.json")
COLL = "watch_changes"
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LABELS = {"watched": "Watched", "not_watched": "Not watched", "pending": "Watch pending", "failed": "Watch failed",
          "unknown": "watch list not created yet", "unreadable": "watch list unreadable"}
_ASCII_LOWER = str.maketrans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abcdefghijklmnopqrstuvwxyz")
REQUEST_FIELDS = ("key", "action", "status", "created", "by", "done_at", "message")


class WatchListError(ValueError):
    """local/watch.json exists but is not a readable version-1 watch list."""


def read_watch(arch_dir):
    """The watched keys of <arch_dir>/local/watch.json, or None when the file is absent. Never creates anything."""
    path = os.path.join(arch_dir, WATCH)
    try:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
    except FileNotFoundError:
        return None
    except (OSError, ValueError) as e:
        raise WatchListError("%s cannot be read (%s)" % (path, type(e).__name__)) from None
    if not isinstance(data, dict) or data.get("version") != 1 or not isinstance(data.get("projects"), list):
        raise WatchListError("%s is not a version 1 watch list" % path)
    return {p["key"] for p in data["projects"] if isinstance(p, dict) and isinstance(p.get("key"), str)}


def _trim(path):
    """The path with empty and "." components dropped, as Rust's `Path::components` collects it."""
    return ("/" if path.startswith("/") else "") + "/".join(x for x in path.split("/") if x not in ("", "."))


def same_path(a, b):
    """Rust same_path: equal after trimming, else both exist and resolve to one directory."""
    if _trim(a) == _trim(b):
        return True
    return os.path.exists(a) and os.path.exists(b) and os.path.realpath(a) == os.path.realpath(b)


def alias_for(key, checkout, rows):
    """The project-paths.json row ({alias, path}) of registry key `key`, or None.

    Why: the session alias and the project's folder come from project-paths.json, keyed by alias, and the page must name
    the same alias as `tm fleet project list` (#10896).
    What: tm's order: alias == key, else alias equal ignoring ASCII case, else the row whose path is `checkout` (the
    managed clone, or None). The first match in file order wins at each step.
    Test: tests/test_dashboard_watch.py::AliasJoin
    """
    entries = [r for r in rows if isinstance(r, dict) and isinstance(r.get("alias"), str) and isinstance(r.get("path"), str)]
    folded = key.translate(_ASCII_LOWER)
    hit = next((r for r in entries if r["alias"] == key), None)
    hit = hit or next((r for r in entries if r["alias"].translate(_ASCII_LOWER) == folded), None)
    if hit is None and checkout:
        hit = next((r for r in entries if same_path(r["path"], checkout)), None)
    return hit


def flags(key, path, alias):
    """{path_exists, alias_mismatch} as `tm fleet project list --json` sets them for one key."""
    return {"path_exists": bool(path) and os.path.exists(path), "alias_mismatch": alias is not None and alias != key}


def _request(doc):
    data = doc["data"]
    out = {k: data[k] for k in REQUEST_FIELDS if k in data}
    out.update(id=doc["id"], version=doc.get("version"))
    return out


def state(key, keys, docs, unreadable=False):
    """{state, label, request} for registry key `key`.

    keys = read_watch()'s set, or None when the list is absent; docs = the watch_changes docs ({id, version, data}).
    Precedence: newest `new` request (pending), then the newest `failed` request no `applied` one followed (failed), then
    the list. request = the deciding doc's fields plus id and version (the page pins if_version to it), else None.
    Test: tests/test_dashboard_watch.py::State
    """
    mine = sorted((d for d in docs if isinstance(d.get("data"), dict) and d["data"].get("key") == key),
                  key=lambda d: (str(d["data"].get("created", "")), d["id"]))
    new = [d for d in mine if d["data"].get("status") == "new"]
    settled = [d for d in mine if d["data"].get("status") in ("applied", "failed")]
    if new:
        name, doc = "pending", new[-1]
    elif settled and settled[-1]["data"]["status"] == "failed":
        name, doc = "failed", settled[-1]
    else:
        doc = None
        name = "unreadable" if unreadable else "unknown" if keys is None else "watched" if key in keys else "not_watched"
    return {"state": name, "label": LABELS[name], "request": _request(doc) if doc else None}


def overlay(answer, arch_dir, docs):
    """A copy of a /api/projects answer with `watch` on every project and `watch_list` {present, error} at the top."""
    err = None
    try:
        keys = read_watch(arch_dir)
    except WatchListError as e:
        keys, err = None, str(e)
    rows = [dict(p, watch=state(p["id"], keys, docs, unreadable=err is not None)) for p in answer.get("projects", [])]
    return dict(answer, projects=rows, watch_list={"present": keys is not None or err is not None, "error": err})
