"""dashboard_routes.py: the dashboard routes added after the D3b split, one function per route (#10571).

Why: dashboard_http.py holds the request plumbing, the guard and the read-only views; the routes the prototype added
since (online doc edits, attachments, the Fleet and Projects views, the shared design tokens, the CLI Profiles view)
would take it past the 500-SLOC cap, so they live here and take the handler as their first argument.
What: `doc_put` (PUT /doc/<project>/<rel>, architect mode, same-origin, base_sha256 compare-and-swap, atomic replace,
one content-free line per save in <inbox>/doc-edits.jsonl); `attach` (POST /api/attach/<item id>?name=<file> stores an
Intent Ledger attachment, GET /api/attach/<item id>/<stored name> serves it sandboxed; dashboard_attach.py); `fleet`
(GET /api/fleet: count plus working|waiting|paused|inactive per managed session); `projects` (GET /api/projects;
dashboard_projects.py); `cli_profiles` (GET /api/cli-profiles, only when the optional cli_profiles module is present,
else 404); `static` (GET /static/<name>, the fixed STATIC_FILES names only). Every function returns the handler's
_send/_err result. Standard library only.
Test: tests/test_dashboard_doc.py, tests/test_dashboard_attach.py, tests/test_dashboard_routes.py
"""
import hashlib
import json
import os
import stat
import sys
import tempfile
import threading
from datetime import datetime, timezone
from urllib.parse import parse_qs, unquote

import dashboard_attach
import dashboard_docs as docs
import dashboard_harness
import dashboard_page as page
import dashboard_store as ds

try:
    import cli_profiles as cli_profiles_mod  # the CLI Profiles view; optional, not shipped yet (#10571)
except ImportError:
    cli_profiles_mod = None

_DOC_PUT_LOCK = threading.Lock()


def _architect_get(h):
    """None when the request may proceed (architect mode, GET/HEAD); else the 404/405 response."""
    if h.server.config.get("mode") != "architect":
        return h._err(404, "not_found", "no such path")
    if h.command not in ("GET", "HEAD"):
        return h._err(405, "method_not_allowed", "GET only")
    return None


def doc_put(h, raw_path, origin_allowed, digits):
    """PUT /doc/<project>/<rel> {text, base_sha256}: overwrite an existing .md the reader can open. Architect mode only
    (404 otherwise). Needs a same-origin Origin header, the same path rules as GET, text up to MAX_EDIT bytes, and
    base_sha256 equal to the file's current hash (409 with the current hash otherwise). Atomic: temp file in the same
    dir, mode kept, os.replace. No create, rename or delete. The edit only writes the file; the owning PM commits it."""
    if h.server.config.get("mode") != "architect":
        return h._err(404, "not_found", "no such path")
    origin = h.headers.get("Origin")
    if origin is None or not origin_allowed(origin, h.server.server_address[1], getattr(h.server, "tailnet_names", frozenset())):
        return h._err(403, "permission_denied", "same-origin Origin required")
    segs = [unquote(x) for x in raw_path.split("/")[2:]]
    real = docs.resolve_doc(h.server.projects(), segs[0], segs[1:]) if segs else None
    if real is None or os.sep + ".git" + os.sep in real + os.sep:
        return h._err(404, "not_found", "no such document")
    raw = (h.headers.get("Content-Length") or "").strip()
    if not digits.fullmatch(raw):
        return h._err(400, "bad_request", "bad Content-Length")
    if int(raw) > 4 * docs.MAX_EDIT:   # JSON escaping can inflate the text; the exact byte limit is checked below
        return h._err(413, "too_large", "request body too large")
    try:
        obj = json.loads(h.rfile.read(int(raw)) or b"{}")
    except ValueError:
        return h._err(400, "bad_request", "request body is not valid JSON")
    text, base = (obj.get("text"), obj.get("base_sha256")) if isinstance(obj, dict) else (None, None)
    if not isinstance(text, str) or not isinstance(base, str) or not base:
        return h._err(400, "bad_request", "body needs text and base_sha256 strings")
    data = text.encode("utf-8")
    if len(data) > docs.MAX_EDIT:
        return h._err(413, "too_large", "document over 1 MB")
    with _DOC_PUT_LOCK:
        with open(real, "rb") as f:
            cur = hashlib.sha256(f.read()).hexdigest()
        if cur != base:
            return h._send(409, {"error": "document changed on disk", "code": "conflict", "sha256": cur})
        _replace(real, data)
    new = hashlib.sha256(data).hexdigest()
    try:
        os.makedirs(h.server.inbox, exist_ok=True)
        with open(os.path.join(h.server.inbox, "doc-edits.jsonl"), "a", encoding="utf-8") as f:
            f.write(json.dumps({"ts": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"), "project": segs[0], "rel": "/".join(segs[1:]),
                                "old_sha256": cur, "new_sha256": new, "bytes": len(data)}) + "\n")
    except OSError as e:
        sys.stderr.write(f"doc-edits log write failed: {e!r}\n")
    return h._send(200, {"ok": True, "sha256": new, "bytes": len(data)})


def _replace(real, data):
    """Write `data` over `real` atomically: a temp file in the same dir, the file's mode kept, os.replace."""
    mode = stat.S_IMODE(os.stat(real).st_mode)
    fd, tmp = tempfile.mkstemp(prefix=".doc-edit-", dir=os.path.dirname(real))
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        os.chmod(tmp, mode)
        os.replace(tmp, real)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def attach(h, rest, q, digits):
    """POST /api/attach/<item id>?name=<file>: raw body, stored as an IL attachment and recorded on the item;
    GET /api/attach/<item id>/<stored name>: a recorded file, read-only, sandboxed (dashboard_attach.py)."""
    if h.command in ("GET", "HEAD") and len(rest) == 2:
        data, ctype, inline = dashboard_attach.serve_file(h.server.store, rest[0], unquote(rest[1]))
        h._csp = "default-src 'none'; sandbox"
        h._extra = None if inline else {"Content-Disposition": 'attachment; filename="%s"' % unquote(rest[1])}
        return h._send(200, data, ctype if inline else "application/octet-stream", cache="private, max-age=60")
    if h.command != "POST" or len(rest) != 1:
        return h._err(405, "method_not_allowed", "POST /api/attach/<item id>?name=<file>")
    raw = (h.headers.get("Content-Length") or "").strip()
    if not digits.fullmatch(raw):
        raise ds.StoreError("bad Content-Length")
    n = int(raw)
    if n > dashboard_attach.max_bytes():   # refuse before reading; the connection closes with the response
        return h._err(413, "too_large", f"file is {n} bytes; the limit is {dashboard_attach.max_bytes()}")
    name = (q.get("name") or [""])[0]
    root = h.server.store.root if h.server.config.get("mode") == "session" else None
    return h._send(201, dashboard_attach.handle_upload(h.server.store, rest[0], name, h.rfile, n, h.server.il_schedule,
                                                       h.server.architect_il, root, actor=h._actor()))


def fleet(h):
    """GET /api/fleet: {sessions: {name: {status, alive, managed}}, count, inactive}; architect mode only, cached ~4 s."""
    refused = _architect_get(h)
    if refused is not None:
        return refused
    sv = h.server

    def build():
        live = dashboard_harness.read_json(os.path.join(sv.inbox, "dashboard-live.json"))
        return dashboard_harness.fleet_status([d["data"] for d in sv.store.list_docs("sessions")],
                                              [d["data"] for d in sv.store.list_docs("goals")],
                                              dashboard_harness.tmux_sessions(), live)
    return h._send(200, sv.fleet_cache.get(build))


def projects(h):
    """GET /api/projects: registry projects with sessions, stack, config and custom agents; architect mode only."""
    refused = _architect_get(h)
    if refused is not None:
        return refused
    sess_ids = {d["data"].get("name"): d["id"] for d in h.server.store.list_docs("sessions")
                if isinstance(d["data"], dict) and d["data"].get("name")}   # the row id is the doc id, not a data field
    return h._send(200, h.server.projects_view.get(sess_ids))


def cli_profiles(h, url):
    """GET /api/cli-profiles: per-profile auth status (names only) and mappings; 404 while cli_profiles is not shipped."""
    if cli_profiles_mod is None:
        return h._err(404, "not_found", "no such path")
    refused = _architect_get(h)
    if refused is not None:
        return refused
    try:
        return h._send(200, cli_profiles_mod.snapshot(h.server.cli_profiles or cli_profiles_mod.DEFAULT_FILE,
                                                      runner=h.server.cli_runner, refresh="refresh" in parse_qs(url.query)))
    except cli_profiles_mod.ProfileError as e:
        return h._err(500, "cli_profiles_unreadable", str(e))


def static(h, parts):
    """GET /static/<name>: the shared design tokens (both modes). Fixed name list; any other name is a 404."""
    if len(parts) != 1 or parts[0] not in page.STATIC_FILES:
        return h._err(404, "not_found", "no such file")
    path = os.path.join(h.server.static, parts[0])
    if not os.path.isfile(path):
        return h._err(404, "not_found", "no such file")
    with open(path, "rb") as f:
        data = f.read()
    return h._send(200, data, "text/css; charset=utf-8")
