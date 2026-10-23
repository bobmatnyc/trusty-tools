#!/usr/bin/env python3
"""dashboard-serve.py: localhost server for the Architect dashboard (stdlib only).

Why: the owner (2026-10-08 17:34Z): use a localhost URL until the dashboard becomes a console
page. The page was written for the claude.ai runtime (window.claude.use("db"|"user")).
What: binds 127.0.0.1 only (default port 7890; refuses to start when the port is busy),
serves templates/dashboard/index.html unchanged with a shim <script> injected
after <head>. The shim defines window.claude.use over JSON endpoints on this server and
emulates onSnapshot by polling every 2 s. Documents live in local/owner-queue via
dashboard_store (the same files scripts/dashboard-db.py writes).
API: GET /api/<coll>[?status=&order_by=&dir=&limit=], GET|PUT|PATCH|DELETE
/api/<coll>/<id> (body {data, if_version}), GET /files/local/briefs/<rel> (read-only,
text/plain, only real files whose realpath is under local/briefs; else 404), POST /api/<coll> (add), POST /api/batch
({ops}), GET /api/user, GET /api/il (architect mode only: every Intent Ledger, read-only; dashboard_il.py), GET /api/harness (architect mode only: read-only Live snapshot incl. modified docs, cached 4 s; dashboard_harness.py), GET /api/harness/diff?id= (the latest change of a Docs row), GET /live (the Live page, templates/dashboard/live.html), GET /api/viz[?days=1..14] (architect mode only: hourly tokens by goal, per-directory context-token estimate (ctx_hours) + closed IL tasks for the Visualize view, read-only; dashboard_viz.py). 409 on if_version mismatch. Requests with a Host other than
127.0.0.1/localhost (any port) or this host's MagicDNS name (Host name, name:<port>, name:443),
or a write whose Origin is not http://127.0.0.1|localhost:<port> or https://<name>[:<port>|:443],
get 403 (DNS-rebinding and CSRF guard). The MagicDNS name comes from `tailscale status --json`
(Self.DNSName) at startup plus any repeatable --allow-host NAME; if tailscale is missing or fails
the server logs one line and stays localhost-only. The bind stays 127.0.0.1; tailscale serve
(`tailscale serve --bg --https=7890 http://127.0.0.1:7890`) is the only tailnet path.
Discovery (trusty-tools #9474): after a successful bind the server writes 127.0.0.1:<port> to
<data dir>/trusty-architect/http_addr (data dir: $TRUSTY_DATA_DIR_OVERRIDE, else ~/Library/Application
Support on macOS, else ${XDG_DATA_HOME:-~/.local/share}) and removes it on shutdown if still its own.
Reader (2026-10-11, widened 2026-10-19): GET /doc/<project>/<rel> serves a Markdown file read-only as text/plain: <project> must be a
key of the server-side allowlist (this repo as "architect", plus the "- Root `<abs path>`" line of each local/projects/<name>.md,
plus optional local/projects/roots.json {name: abs path}); <rel> must be a real .md file anywhere under the project root (the owner 2026-10-19:
docs outside docs/briefs link to the viewer). 404 for `..`, empty or absolute segments, a realpath outside the root (symlink escape),
unknown projects, non-.md files, any segment starting with "." (.git, .env, .claude, .trusty-mpm), node_modules, target, and any
filename matching *secret*, *credential* or *.env* (case-insensitive; checked again on the resolved path). The page opens a doc from the
URL fragment #doc/<project>/<rel>. GET /doc-sync/<project>/<rel>[?refresh=1] (same checks) returns {share:false} or the Notion share state
and comments sidecar for that doc; refresh=1 starts `scripts/notion_sync.py pull` in the background, at most once per 5 minutes per doc
(the owner Dc2 2026-10-21; server holds no Notion key). GET /media?type=diagram,image lists diagrams and images under the doc roots (architect dash: each row carries a stable B id, store collection `media`; briefs, PRs and issues get none) and GET /media-file/<project>/<rel> serves them read-only (dashboard_media.py; same refusals as /doc; SVG under a sandbox CSP). GET /briefs lists the briefs the reader can reach ({briefs:[{project,path,title,date,size,words}]}, newest first; each file passes the /doc checks). GET /vendor/<name> serves the three pinned reader libraries (marked, DOMPurify, mermaid) from
templates/dashboard/vendor/ (marked, DOMPurify; mermaid.min.js is not shipped there and comes from this instance's local/artifacts/owner-queue/vendor/ when present), a fixed name list only. Both are GET/HEAD only and behind the same Host check.
Reload (the owner 2026-10-09): every response carries X-Page-Rev (sha1[:16] of the page file); the shim holds the rev it
was served with and reloads the tab once when it differs, after window.__stashDrafts().
Modes (2026-10-19): --mode architect (default) is the page above. --mode session --root STORE --title T --briefs-base URL serves the same page as one
PM session's Intent Ledger: GET /config.json {mode,title,briefs_base,storage_key} makes the page hide the Architect-only panes and open briefs at
briefs_base. Session mode refuses the default local/owner-queue root, writes only under its --root (the prune log goes to <root>/.archive/), serves no
/doc project and does not publish the console discovery file. /config.json is served in both modes; absent (older server, claude.ai) means architect.
/config.json also carries tailnet (https://<tailnet name>:7890/ from the guard names, "" when none) and, in architect mode,
briefs_abs (this checkout's local/briefs/), so the page holds no host name or checkout path (#10161).
Session links (nav) are served only in architect mode; session mode serves nav [] (ar-T44).
Test: scripts/test_dashboard_media.py, scripts/test_dashboard_media_ids.py, scripts/test_dashboard_mode.py, scripts/test_dashboard_qualified.py, scripts/test_dashboard_doc.py, scripts/test_notion_sync.py, scripts/test_dashboard_store.py, scripts/test_dashboard_reload.py, scripts/test_dashboard_discovery.py, scripts/test_dashboard_tailnet.py, scripts/test_dashboard_harness.py
"""
import argparse
import fnmatch
import hashlib
import json
import os
import re
import signal
import shutil
import socket
import subprocess
import sys
import time
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlsplit

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)
import dashboard_harness  # noqa: E402
import dashboard_il  # noqa: E402
import dashboard_media  # noqa: E402
import dashboard_viz  # noqa: E402
try:
    import notion_sync  # noqa: E402  (the Notion share sidecar; optional)
except ImportError:
    notion_sync = None
import dashboard_store as ds  # noqa: E402

DEFAULT_PORT = 7890
HOST = "127.0.0.1"
PAGE = os.path.join(ds.REPO, "templates", "dashboard", "index.html")
LIVE_PAGE = os.path.join(ds.REPO, "templates", "dashboard", "live.html")
INBOX = os.path.join(ds.REPO, "inbox")
CLAUDE_PROJECTS = os.path.join(os.environ.get("CLAUDE_CONFIG_DIR") or os.path.expanduser("~/.trusty-tools/trusty-mpm/claude-config"), "projects")
BRIEFS = os.path.join(ds.REPO, "local", "briefs")
LEDGER = os.path.join(ds.REPO, "local", "usage", "ledger.jsonl")
TOKENS = os.path.join(ds.REPO, "local", "usage", "token-hourly.jsonl")
USAGE_WINDOW_S = 24 * 3600
USAGE_FIELDS = ("ts", "pct5h", "reset5h", "pctwk", "resetwk")
MAX_BODY = 1 << 20
# D3: the shipped reader libraries live beside the page; the instance tree keeps mermaid (not shipped) and the old copies.
VENDOR = os.path.join(ds.REPO, "templates", "dashboard", "vendor")
VENDOR_INSTANCE = os.path.join(ds.REPO, "local", "artifacts", "owner-queue", "vendor")
VENDOR_FILES = ("marked.umd.js", "purify.min.js", "mermaid.min.js")   # fixed names; nothing else under vendor/ is served
ARCHITECT = "architect"
NAV_FILE = os.path.join(ds.REPO, "local", "il-dashboards.json")
IL_SCHEDULE = os.path.join(ds.REPO, "local", "il-schedule.json")
ARCHITECT_IL = os.path.join(ds.REPO, "local", "il")
ARCHITECT_CONFIG = {"mode": "architect", "title": "Architect Dashboard", "briefs_base": "", "storage_key": "architect-dashboard",
                    "briefs_abs": os.path.join(ds.REPO, "local", "briefs") + "/", "tailnet": ""}
DENY_DIRS = ("node_modules", "target")                                # path segments never served (plus any starting with ".")
DENY_NAMES = ("*secret*", "*credential*", "*.env*")                   # filename globs never served, case-insensitive
MAX_DOC = 4 << 20
ROOT_LINE = re.compile(r"^- Root:? +`(/[^`\n]+)`", re.M)


def load_projects(repo=None):
    """Project allowlist {name: realpath of root}: this repo as "architect", the first "- Root `/abs`" line of each
    local/projects/<name>.md, and optional local/projects/roots.json {name: "/abs"}. Server-side only; a name
    must be a plain file stem (no separator). Roots that are not directories are dropped."""
    repo = repo or ds.REPO
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
    import calendar
    import time
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


def _denied(segments, filename):
    """True when a path must never be served: a dot segment, node_modules, target, or a secret-like filename."""
    if any(x.startswith(".") or x in DENY_DIRS for x in segments):
        return True
    low = filename.lower()
    return any(fnmatch.fnmatch(low, g) for g in DENY_NAMES)


BRIEF_DIRS = {"architect": ("docs/briefs", "local/briefs")}
_DATE = re.compile(r"^(\d{4}-\d{2}-\d{2})")


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
                    m = _DATE.match(name)
                    st = os.stat(real)
                    out.append({"project": project, "path": "/".join(rel), "title": h1.group(1) if h1 else name,
                                "date": m.group(1) if m else datetime.fromtimestamp(st.st_mtime, timezone.utc).strftime("%Y-%m-%d"),
                                "size": st.st_size, "words": len(body.split())})
    out.sort(key=lambda b: (b["date"], b["path"]), reverse=True)
    return out


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
    if not rel_segments[-1].endswith(".md") or _denied(rel_segments, rel_segments[-1]):
        return None
    root_real = os.path.realpath(root)
    real = os.path.realpath(os.path.join(root_real, *rel_segments))
    if os.path.commonpath([root_real, real]) != root_real or real == root_real or not real.endswith(".md") or not os.path.isfile(real):
        return None
    rsegs = os.path.relpath(real, root_real).split(os.sep)
    if _denied(rsegs, rsegs[-1]):
        return None
    return real

SHIM = r"""<script>
/* dashboard-serve.py shim: window.claude.use("db"|"user") over this server's /api, snapshots by polling. */
(function () {
  "use strict";
  var POLL_MS = 2000, FAIL_LIMIT = 5, listeners = new Set();
  var PAGE_REV = "__PAGE_REV__", reloading = false;
  /* The server sends the served page's hash on every API response. A different hash means index.html changed since this tab loaded it: stash unsent drafts, then reload once. */
  function checkRev(res) {
    var rev = res && res.headers && res.headers.get ? res.headers.get("X-Page-Rev") : null;
    if (!rev || !PAGE_REV || rev === PAGE_REV || reloading) return;
    reloading = true;
    try { if (window.__stashDrafts) window.__stashDrafts(); } catch (_) {}
    location.reload();
  }
  /* A tab left open with no API response (or a throttled background tab) never sees the new rev. Also check on a 60 s timer and when the tab becomes visible; skip while a text input or textarea holds text, and retry on the next tick. */
  function hasDraft() {
    return Array.prototype.some.call(document.querySelectorAll("input, textarea"), function (el) {
      var t = (el.type || "").toLowerCase();
      return (el.tagName === "TEXTAREA" || t === "" || t === "text" || t === "search" || t === "url" || t === "email") && String(el.value || "").trim() !== "";
    });
  }
  function revPoll() {
    if (reloading || hasDraft()) return;
    fetch("/config.json", { cache: "no-store" }).then(checkRev, function () {});
  }
  setInterval(revPoll, 60000);
  document.addEventListener("visibilitychange", function () { if (document.visibilityState === "visible") revPoll(); });
  function mkErr(status, body) {
    var e = new Error((body && body.error) || ("HTTP " + status));
    e.code = (body && body.code) || "unavailable"; e.status = status; return e;
  }
  async function api(method, path, body) {
    var res = await fetch("/api/" + path, { method: method, cache: "no-store",
      headers: body ? { "Content-Type": "application/json" } : {}, body: body ? JSON.stringify(body) : undefined });
    checkRev(res);
    var js = null; try { js = await res.json(); } catch (_) {}
    if (!res.ok) throw mkErr(res.status, js);
    return js;
  }
  /* After a write, re-fetch at once the subscriptions on the collections it touched and resolve when they have delivered. A poll already in flight started before the write and may carry the old data, and the old tick() dropped the post-write refresh while one was in flight, so the page kept the old state until the next 2 s tick (measured 2.6 s with a 0.6 s poll). The refresh now starts at once, in parallel. */
  function refreshColls(colls) {
    var jobs = [];
    listeners.forEach(function (h) { if (!colls || colls.indexOf(h.coll) >= 0) jobs.push(h.refresh()); });
    return Promise.all(jobs);
  }
  function collsOf(path, body) {
    if (path === "batch") return ((body && body.ops) || []).map(function (o) { return o.collection; });
    return [path.split("/")[0].split("?")[0]];
  }
  /* The page shows a "Processing..." overlay from these events: start, then settled (writes done and the re-fetch delivered) or error. The write's own promise still resolves as soon as the server answers. */
  function emit(phase, detail) { try { window.dispatchEvent(new CustomEvent("dash:write", { detail: Object.assign({ phase: phase }, detail || {}) })); } catch (_) {} }
  async function write(method, path, body) {
    var colls = collsOf(path, body);
    emit("start");
    try {
      var r = await api(method, path, body);
      refreshColls(colls).then(function () { emit("settled"); }, function () { emit("settled"); });
      return r;
    } catch (e) {
      refreshColls(colls).then(function () { emit("error", { status: e.status, code: e.code, message: e.message }); }, function () { emit("error", { status: e.status, code: e.code, message: e.message }); });   /* a 409 or any failure still reloads, so the page shows the current doc */
      throw e;
    }
  }
  function docSnap(id, d) { return { id: id, exists: !!d, version: d ? d.version : undefined, data: function () { return d ? d.data : undefined; } }; }
  function querySnap(res) {
    var docs = res.docs.map(function (d) { return docSnap(d.id, d); });
    return { docs: docs, size: docs.length, empty: docs.length === 0, forEach: function (f) { docs.forEach(f); } };
  }
  function subscribe(coll, load, cb, err) {
    var last = null, fails = 0, stopped = false, inflight = 0, started = 0, applied = 0;
    function tick() { return run(false); }
    /* refresh(): a fetch that starts now even when a poll is in flight (that poll began before the write and may carry old data). Results apply in start order: a slower, older fetch that lands after a newer one is dropped. */
    function refresh() { return run(true); }
    // The first load runs even in a hidden tab (else the page shows "Loading…" until shown); later polls run while hidden too (the browser throttles them), so a new decision can still beep.
    async function run(force) {
      if (stopped || (inflight && !force)) return;
      var mine = ++started; inflight++;
      try {
        var r = await load(); fails = 0;
        if (mine > applied) { applied = mine; if (r.rev !== last) { last = r.rev; if (!stopped) { try { cb(r.snap); } catch (e) { console.error(e); } } } }
      } catch (e) { fails++; if (fails === FAIL_LIMIT && err) { try { err(e); } catch (_) {} } }
      inflight--;
    }
    var h = { tick: tick, refresh: refresh, coll: coll }; listeners.add(h); run(true);
    var timer = setInterval(tick, POLL_MS);
    document.addEventListener("visibilitychange", tick);
    return function () { stopped = true; clearInterval(timer); listeners.delete(h); document.removeEventListener("visibilitychange", tick); };
  }
  function DocRef(coll, id) {
    var p = coll + "/" + encodeURIComponent(id);
    this.id = id;
    this.get = async function () { try { return docSnap(id, await api("GET", p)); } catch (e) { if (e.status === 404) return docSnap(id, null); throw e; } };
    this.set = function (data, opts) { return write("PUT", p, { data: data, if_version: opts && opts.if_version }); };
    this.update = function (patch, opts) { return write("PATCH", p, { data: patch, if_version: opts && opts.if_version }); };
    this.delete = function (opts) { return write("DELETE", p + (opts && opts.if_version != null ? "?if_version=" + opts.if_version : "")); };
    this.onSnapshot = function (cb, err) {
      return subscribe(coll, async function () {
        try { var d = await api("GET", p); return { rev: "v" + d.version, snap: docSnap(id, d) }; }
        catch (e) { if (e.status === 404) return { rev: "missing", snap: docSnap(id, null) }; throw e; }
      }, cb, err);
    };
  }
  function Query(coll, o) {
    this.doc = function (id) { return new DocRef(coll, id); };
    this.orderBy = function (f, dir) { return new Query(coll, Object.assign({}, o, { order_by: f, dir: dir === "desc" ? "desc" : "asc" })); };
    this.limit = function (n) { return new Query(coll, Object.assign({}, o, { limit: n })); };
    function qs() { var p = new URLSearchParams(o); var s = p.toString(); return coll + (s ? "?" + s : ""); }
    this.get = async function () { return querySnap(await api("GET", qs())); };
    this.add = async function (data) { var r = await write("POST", coll, { data: data }); return new DocRef(coll, r.id); };
    this.onSnapshot = function (cb, err) {
      return subscribe(coll, async function () { var r = await api("GET", qs()); return { rev: r.rev, snap: querySnap(r) }; }, cb, err);
    };
  }
  var db = {
    collection: function (name) { return new Query(name, {}); },
    doc: function (path) { var i = path.indexOf("/"); return new DocRef(path.slice(0, i), path.slice(i + 1)); },
    batch: async function (ops) { return (await write("POST", "batch", { ops: ops })).results; }   /* all-or-nothing; each op may carry if_version */
  };
  async function user() {
    var u = await api("GET", "user");
    return { id: u.id, name: u.name, can: async function () { return u.can_write !== false; } };
  }
  window.claude = { use: async function (cap) { return cap === "db" ? db : cap === "user" ? await user() : null; } };
})();
</script>
"""


_REV_CACHE = {}


def page_rev(path):
    """First 16 hex of the sha1 of the page file; re-read only when its (mtime, size) changes."""
    st = os.stat(path)
    key = (st.st_mtime_ns, st.st_size)
    hit = _REV_CACHE.get(path)
    if hit and hit[0] == key:
        return hit[1]
    with open(path, "rb") as f:
        rev = hashlib.sha1(f.read()).hexdigest()[:16]
    _REV_CACHE[path] = (key, rev)
    return rev


def inject(html, rev=""):
    """Insert the shim right after <head> (before any page script); prepend when there is no head."""
    shim = SHIM.replace("__PAGE_REV__", rev)
    low = html.lower()
    i = low.find("<head")
    if i >= 0:
        j = html.find(">", i)
        if j >= 0:
            return html[: j + 1] + "\n" + shim + html[j + 1:]
    return shim + html


def rev_of(docs):
    h = hashlib.sha1()
    for d in docs:
        h.update(f"{d['id']}:{d['version']};".encode())
    return h.hexdigest()


class Handler(BaseHTTPRequestHandler):
    server_version = "dashboard-serve"
    # HTTP/1.0 default: one request per connection, so an unread body never poisons the next request.

    # ---- plumbing ----
    def log_request(self, code="-", size="-"):
        # Quiet the 2 s polling: log writes and errors only.
        if self.command != "GET" or (str(code).isdigit() and int(code) >= 400):
            super().log_request(code, size)

    def _send(self, status, payload, ctype="application/json; charset=utf-8", cache="no-store"):
        body = payload if isinstance(payload, bytes) else json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", cache)
        self.send_header("X-Content-Type-Options", "nosniff")
        if getattr(self, "_csp", None):
            self.send_header("Content-Security-Policy", self._csp)
            self._csp = None
        try:
            self.send_header("X-Page-Rev", page_rev(self.server.page))
        except OSError:
            pass
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def _err(self, status, code, message):
        self._send(status, {"error": message, "code": code})

    def _guard(self):
        port = self.server.server_address[1]
        names = getattr(self.server, "tailnet_names", frozenset())
        host = self.headers.get("Host", "")
        if not host_allowed(host, port, names):
            self._err(403, "permission_denied", "host not allowed")
            return False
        origin = self.headers.get("Origin")
        if self.command not in ("GET", "HEAD") and origin is not None:
            if not origin_allowed(origin, port, names):
                self._err(403, "permission_denied", "cross-origin write refused")
                return False
        return True

    def _body(self):
        try:
            n = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            raise ds.StoreError("bad Content-Length")
        if n > MAX_BODY:
            raise ds.StoreError("request body too large")
        try:
            obj = json.loads(self.rfile.read(n) or b"{}")
        except ValueError:
            raise ds.StoreError("request body is not valid JSON")
        if not isinstance(obj, dict):
            raise ds.StoreError("request body must be a JSON object")
        return obj

    # ---- routing ----
    def _dispatch(self):
        if not self._guard():
            return
        url = urlsplit(self.path)
        parts = [p for p in url.path.split("/") if p]
        try:
            if not parts or parts == ["index.html"]:
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                rev = page_rev(self.server.page)
                with open(self.server.page, "r", encoding="utf-8") as f:
                    return self._send(200, inject(f.read(), rev).encode("utf-8"), "text/html; charset=utf-8")
            if parts == ["config.json"]:
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                # ar-T44: session links only on the Architect dashboard; a session dash serves an empty nav
                nav = load_nav(self.server.nav_file) if self.server.config.get("mode") == "architect" else []
                return self._send(200, dict(self.server.config, nav=nav, tailnet=tailnet_origin(self.server.tailnet_names)))
            if parts == ["api", "il"]:
                # Df7: consolidated Intent Ledger view; architect mode only, read-only, GET only
                if self.server.config.get("mode") != "architect":
                    return self._err(404, "not_found", "no such path")
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                return self._send(200, dashboard_il.aggregate(self.server.projects(), self.server.il_schedule, self.server.architect_il))
            if parts == ["live"]:
                # ar-Te8: the Live view page; architect mode only, GET only, static file
                if self.server.config.get("mode") != "architect":
                    return self._err(404, "not_found", "no such path")
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                with open(self.server.live_page, "rb") as f:
                    return self._send(200, f.read(), "text/html; charset=utf-8")
            if parts == ["api", "harness"]:
                # ar-Te8: read-only Live snapshot (sessions, subagents, recent in/out messages); architect mode only, cached ~4 s
                if self.server.config.get("mode") != "architect":
                    return self._err(404, "not_found", "no such path")
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                return self._send(200, self.server.harness_cache.get(self._harness))
            if parts == ["api", "harness", "diff"]:
                # ar-Tea: unified diff of the latest change behind a Docs row; same checks as /api/harness
                if self.server.config.get("mode") != "architect":
                    return self._err(404, "not_found", "no such path")
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                self.server.harness_cache.get(self._harness)
                d = dashboard_harness.doc_diff((parse_qs(url.query).get("id") or [""])[0])
                return self._send(200, d) if d else self._err(404, "not_found", "no such change")
            if parts == ["api", "viz"]:
                # Visualize view: hourly tokens by goal and closed IL tasks; architect mode only, read-only, GET only
                if self.server.config.get("mode") != "architect":
                    return self._err(404, "not_found", "no such path")
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                try:
                    days = int(parse_qs(url.query).get("days", ["7"])[0])
                except ValueError:
                    return self._err(400, "bad_request", "days must be an integer")
                return self._send(200, dashboard_viz.build(self.server.store, self.server.tokens, self.server.projects(), self.server.il_schedule, self.server.architect_il, time.time(), days))
            if parts[0] == "files":
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                return self._file(unquote(url.path))
            if parts[0] == "usage":
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                return self._usage(parts[1:])
            if parts[0] in ("media", "media-file"):
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                if parts[0] == "media":
                    return self._send(200, {"media": self._media(parse_qs(url.query))}) if len(parts) == 1 else self._err(404, "not_found", "no such path")
                return self._media_file(url.path)
            if parts[0] in ("doc", "vendor", "doc-sync", "briefs"):
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                if parts[0] == "briefs":
                    return self._send(200, {"briefs": list_briefs(self.server.projects())}) if len(parts) == 1 else self._err(404, "not_found", "no such path")
                if parts[0] == "doc-sync":
                    return self._doc_sync(url.path, parse_qs(url.query))
                return self._doc(url.path) if parts[0] == "doc" else self._vendor(parts[1:])
            if parts[0] != "api":
                return self._err(404, "not_found", "no such path")
            return self._api(parts[1:], parse_qs(url.query))
        except ds.StoreError as e:
            return self._err(e.status, e.code, str(e))
        except FileNotFoundError as e:
            return self._err(404, "not_found", f"missing file: {e.filename}")
        except Exception as e:  # noqa: BLE001 - last-resort 500 with the message in the log
            sys.stderr.write(f"internal error on {self.command} {self.path}: {e!r}\n")
            return self._err(500, "internal", "internal error")

    def _harness(self):
        sv = self.server
        sdocs = [d["data"] for d in sv.store.list_docs("sessions")]
        idocs = [d["data"] for d in sv.store.list_docs("items")]
        roots = sv.projects()
        cwds = {x.get("name"): roots[x.get("project")] for x in sdocs if x.get("name") and x.get("project") in roots}
        return dashboard_harness.build(sv.inbox, sv.claude_projects, sdocs, idocs, project_cwds=cwds, roots=roots)

    def _file(self, path):
        """Serve /files/local/briefs/<rel>. Anything that does not resolve to a regular file
        inside the briefs dir (dot-dot, absolute, symlink escape, directory) is a 404."""
        prefix = "/files/local/briefs/"
        if not path.startswith(prefix) or "\x00" in path:
            return self._err(404, "not_found", "no such file")
        root = os.path.realpath(self.server.briefs)
        real = os.path.realpath(os.path.join(root, path[len(prefix):]))
        if os.path.commonpath([root, real]) != root or real == root or not os.path.isfile(real):
            return self._err(404, "not_found", "no such file")
        with open(real, "rb") as f:
            data = f.read()
        return self._send(200, data, "text/plain; charset=utf-8")

    def _usage(self, parts):
        """GET /usage/<account-slug>: the account's last-24h ledger samples. The slug must name a doc in the
        accounts collection (that doc's `account` label selects the ledger lines); anything else is a 404."""
        try:
            label = self.server.store.get("accounts", parts[0])["data"].get("account") if len(parts) == 1 else None
        except ds.NotFound:
            label = None
        if not label:
            return self._err(404, "not_found", "no such account")
        return self._send(200, {"samples": usage_samples(self.server.ledger, label)})

    def _doc(self, raw_path):
        """Serve /doc/<project>/<rel>: a brief file, read-only. Segments are unquoted one by one, so an encoded
        slash or dot-dot cannot slip past; every failure is the same 404."""
        segs = [unquote(x) for x in raw_path.split("/")[2:]]
        real = resolve_doc(self.server.projects(), segs[0], segs[1:]) if segs else None
        if real is None or os.path.getsize(real) > MAX_DOC:
            return self._err(404, "not_found", "no such document")
        with open(real, "rb") as f:
            data = f.read()
        return self._send(200, data, "text/plain; charset=utf-8")

    def _doc_sync(self, raw_path, query):
        """GET /doc-sync/<project>/<rel>[?refresh=1]: Notion share state for a doc the reader opened. The doc must pass
        the same /doc checks. No share record: {"share": false}. refresh=1 starts a background `notion_sync.py pull`
        (at most once per 5 minutes per doc). The server holds no Notion credentials; the child fetches the key."""
        segs = [unquote(x) for x in raw_path.split("/")[2:]]
        real = resolve_doc(self.server.projects(), segs[0], segs[1:]) if segs else None
        if real is None:
            return self._err(404, "not_found", "no such document")
        doc_id = notion_sync.find_doc_id(real) if notion_sync else None
        if not doc_id:
            return self._send(200, {"share": False})
        if query.get("refresh") == ["1"]:
            notion_sync.maybe_refresh(doc_id)
        return self._send(200, notion_sync.snapshot(doc_id))

    def _media(self, q):
        """GET /media?type=diagram,image (default both): diagrams and images under the doc roots. Briefs stay on /briefs."""
        want = {t for v in q.get("type", ["diagram,image"]) for t in v.split(",")} & {"diagram", "image"}
        if not want:
            return []
        store = self.server.store if self.server.config.get("mode") == "architect" else None   # B ids: architect dash only
        return dashboard_media.media_rows(store, self.server.projects(), want, _denied)

    def _media_file(self, raw_path):
        """GET /media-file/<project>/<rel>: a diagram or image file, read-only. Same 404-for-everything rule as /doc.
        SVG is sandboxed by CSP so a script inside it cannot run even if opened directly."""
        segs = [unquote(x) for x in raw_path.split("/")[2:]]
        real = dashboard_media.resolve_media(self.server.projects(), segs[0], segs[1:], _denied) if segs else None
        if real is None:
            return self._err(404, "not_found", "no such file")
        with open(real, "rb") as f:
            data = f.read()
        self._csp = "sandbox; default-src 'none'; style-src 'unsafe-inline'"
        return self._send(200, data, dashboard_media.MEDIA_TYPES[os.path.splitext(real)[1].lower()])

    def _vendor(self, parts):
        if len(parts) != 1 or parts[0] not in VENDOR_FILES:
            return self._err(404, "not_found", "no such file")
        path = os.path.join(self.server.vendor, parts[0])
        if not os.path.isfile(path) and self.server.vendor == VENDOR:
            path = os.path.join(VENDOR_INSTANCE, parts[0])
        if not os.path.isfile(path):
            return self._err(404, "not_found", "no such file")
        with open(path, "rb") as f:
            data = f.read()
        return self._send(200, data, "application/javascript; charset=utf-8", cache="public, max-age=3600")

    def _actor(self):
        """Change-log actor: the X-Actor header, else "page" (every HTTP write comes from the page)."""
        return (self.headers.get("X-Actor") or "page")[:40]

    def _api(self, parts, q):
        store, m = self.server.store, self.command
        if parts == ["user"] and m == "GET":
            try:
                name = store.get("meta", "state")["data"].get("owner_name") or "the owner"
            except ds.NotFound:
                name = "the owner"
            return self._send(200, {"id": "local", "name": name, "can_write": True})
        if parts == ["batch"] and m == "POST":
            return self._send(200, {"results": store.batch(self._body().get("ops"), actor=self._actor())})
        if len(parts) == 1:
            coll = parts[0]
            if m == "GET":
                docs = store.list_docs(
                    coll, status=(q.get("status") or [None])[0], order_by=(q.get("order_by") or [None])[0],
                    desc=(q.get("dir") or ["asc"])[0] == "desc",
                    limit=int(q["limit"][0]) if q.get("limit") and q["limit"][0].isdigit() else None)
                return self._send(200, {"docs": docs, "rev": rev_of(docs)})
            if m == "POST":
                return self._send(201, store.add(coll, self._body().get("data"), actor=self._actor()))
        if len(parts) == 2:
            coll, doc_id = parts
            if m == "GET":
                return self._send(200, store.get(coll, doc_id))
            if m in ("PUT", "PATCH"):
                b = self._body()
                fn = store.set if m == "PUT" else store.update
                return self._send(200, fn(coll, doc_id, b.get("data"), b.get("if_version"), actor=self._actor()))
            if m == "DELETE":
                iv = (q.get("if_version") or [None])[0]
                return self._send(200, store.delete(coll, doc_id, int(iv) if iv is not None and iv.isdigit() else None, actor=self._actor()))
        return self._err(405, "method_not_allowed", f"{m} {'/'.join(parts)} not supported")

    do_GET = do_HEAD = do_PUT = do_PATCH = do_POST = do_DELETE = _dispatch


LOCAL_NAMES = ("127.0.0.1", "localhost")
TAILSCALE_FALLBACK = "/Applications/Tailscale.app/Contents/MacOS/Tailscale"


def host_allowed(host, port, names):
    """Host header check: local names need :port; tailnet names also pass bare or with :443."""
    if host in {f"{h}:{port}" for h in LOCAL_NAMES}:
        return True
    return any(host in (n, f"{n}:{port}", f"{n}:443") for n in names)


def origin_allowed(origin, port, names):
    """Write Origin check: http://local:port, or https://<tailnet name>[:port|:443]."""
    if origin in {f"http://{h}:{port}" for h in LOCAL_NAMES}:
        return True
    return any(origin in (f"https://{n}", f"https://{n}:{port}", f"https://{n}:443") for n in names)


def tailnet_name():
    """This host's MagicDNS name from `tailscale status --json` (Self.DNSName), or None (fail closed)."""
    exe = shutil.which("tailscale") or (TAILSCALE_FALLBACK if os.access(TAILSCALE_FALLBACK, os.X_OK) else None)
    if not exe:
        sys.stderr.write("dashboard-serve: tailscale not found; localhost-only\n")
        return None
    try:
        out = subprocess.run([exe, "status", "--json"], capture_output=True, text=True, timeout=10, check=True)
        name = json.loads(out.stdout)["Self"]["DNSName"].rstrip(".").lower()
    except Exception as e:  # noqa: BLE001 - fail closed on any tailscale problem
        sys.stderr.write(f"dashboard-serve: tailscale name lookup failed ({type(e).__name__}); localhost-only\n")
        return None
    if not name:
        sys.stderr.write("dashboard-serve: tailscale reported no DNSName; localhost-only\n")
        return None
    return name


def tailnet_origin(names, port=DEFAULT_PORT):
    """The page's tailnet link origin, https://<name>:<port>/ for the first guard name (normally the one MagicDNS
    name), or "" with no names (#10161)."""
    if not names:
        return ""
    return f"https://{min(names)}:{port}/"


def allowed_names(extra=(), lookup=tailnet_name):
    """Tailnet names admitted by the guard: the looked-up MagicDNS name plus --allow-host values."""
    names = {n.strip().rstrip(".").lower() for n in extra if n and n.strip()}
    found = lookup()
    if found:
        names.add(found)
    return frozenset(names)


class DashboardServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, port, store, page=PAGE, briefs=BRIEFS, tailnet_names=(), projects=None, vendor=VENDOR, ledger=LEDGER, config=None, nav_file=NAV_FILE, il_schedule=IL_SCHEDULE, architect_il=ARCHITECT_IL, tokens=TOKENS, live_page=LIVE_PAGE, inbox=INBOX, claude_projects=CLAUDE_PROJECTS):
        self.tokens = tokens
        self.live_page, self.inbox, self.claude_projects = live_page, inbox, claude_projects
        self.harness_cache = dashboard_harness.HarnessCache()
        self.store, self.page, self.briefs, self.vendor, self.ledger = store, page, briefs, vendor, ledger
        self.config = dict(config or ARCHITECT_CONFIG)
        self.nav_file = nav_file
        self.il_schedule, self.architect_il = il_schedule, architect_il
        # projects: {name: root} or a callable returning it; default re-reads the server-side allowlist per request
        self.projects = projects if callable(projects) else (load_projects if projects is None else (lambda: projects))
        self.tailnet_names = frozenset(tailnet_names)
        super().__init__((HOST, port), Handler)   # never 0.0.0.0


def port_in_use(port):
    """True when something already accepts connections on 127.0.0.1:port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.settimeout(0.5)
        return s.connect_ex((HOST, port)) == 0


def data_dir():
    """Platform data dir for trusty tools; TRUSTY_DATA_DIR_OVERRIDE wins when non-empty."""
    override = os.environ.get("TRUSTY_DATA_DIR_OVERRIDE")
    if override:
        return override
    if sys.platform == "darwin":
        return os.path.expanduser("~/Library/Application Support")
    return os.environ.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share")


def addr_file_path():
    return os.path.join(data_dir(), "trusty-architect", "http_addr")


def write_addr_file(addr):
    """Atomically publish addr for console discovery; a failure logs one line and never raises."""
    try:
        path = addr_file_path()
        d = os.path.dirname(path)
        os.makedirs(d, mode=0o700, exist_ok=True)
        tmp = os.path.join(d, f".http_addr.{os.getpid()}.tmp")
        try:
            with open(tmp, "w", encoding="utf-8") as f:
                f.write(addr)
            os.replace(tmp, path)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise
    except Exception as e:  # noqa: BLE001 - discovery is best-effort
        sys.stderr.write(f"dashboard-serve: cannot write discovery file: {e}\n")


def remove_addr_file(addr):
    """Remove the discovery file only if it still holds addr; best-effort, one stderr line on failure."""
    try:
        path = addr_file_path()
        try:
            with open(path, "r", encoding="utf-8") as f:
                current = f.read().strip()
        except FileNotFoundError:
            return
        if current == addr:
            os.unlink(path)
    except Exception as e:  # noqa: BLE001
        sys.stderr.write(f"dashboard-serve: cannot remove discovery file: {e}\n")


def load_nav(path):
    """Read the IL dashboard registry per request: a list of {name, port}. A missing or bad file, or bad entries, give fewer entries (at worst [])."""
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError):
        return []
    if not isinstance(data, list):
        return []
    return [{"name": e["name"], "port": e["port"]} for e in data
            if isinstance(e, dict) and isinstance(e.get("name"), str) and e["name"]
            and isinstance(e.get("port"), int) and not isinstance(e["port"], bool)]


def session_config(title, briefs_base, session=""):
    """Page config for session mode. Title: explicit title, else "<session>: IL Dashboard", else "IL Dashboard". storage_key (localStorage prefix) comes from the session when given, so a title change does not reset viewers; otherwise from the title."""
    title = title or (f"{session}: IL Dashboard" if session else "IL Dashboard")
    slug = re.sub(r"[^a-z0-9]+", "-", (f"{session} il dashboard" if session else title).lower()).strip("-") or "ledger"
    return {"mode": "session", "title": title, "briefs_base": briefs_base, "storage_key": slug}


def _raise_interrupt(signum, frame):
    raise KeyboardInterrupt


def main(argv=None):
    ap = argparse.ArgumentParser(description="Serve the Architect dashboard on 127.0.0.1.")
    ap.add_argument("--port", type=int, default=DEFAULT_PORT)
    ap.add_argument("--root", default=ds.DEFAULT_ROOT, help="document store directory (default local/owner-queue)")
    ap.add_argument("--page", default=PAGE)
    ap.add_argument("--mode", choices=("architect", "session"), default="architect",
                    help="architect (default) or session: one PM session's Intent Ledger over its own --root")
    ap.add_argument("--title", default="", help="session mode: page title and heading")
    ap.add_argument("--session", default="", help="session mode: PM session name; default title is '<session>: IL Dashboard' and storage_key derives from it")
    ap.add_argument("--nav-file", default=NAV_FILE, help="IL dashboard registry JSON [{name, port}], read per request, served as /config.json nav")
    ap.add_argument("--briefs-base", default="", help="session mode: URL prefix that docs/briefs/<file>.md links resolve against")
    ap.add_argument("--allow-host", action="append", default=[], metavar="NAME",
                    help="extra Host/Origin name to accept (repeatable); the MagicDNS name is added automatically")
    args = ap.parse_args(argv)
    if not os.path.isfile(args.page):
        sys.stderr.write(f"dashboard-serve: page not found: {args.page}\n")
        return 2
    if port_in_use(args.port):
        sys.stderr.write(f"dashboard-serve: port {args.port} on {HOST} is already in use; "
                         f"stop the other server or pass --port N\n")
        return 1
    session = args.mode == "session"
    if session and os.path.realpath(args.root) == os.path.realpath(ds.DEFAULT_ROOT):
        sys.stderr.write("dashboard-serve: session mode needs its own --root, not the Architect store\n")
        return 2
    store = ds.Store(args.root, prune_log=os.path.join(os.path.abspath(args.root), ".archive", "pruned.jsonl") if session else None)
    extra = {"config": session_config(args.title, args.briefs_base, args.session), "projects": {},
             "briefs": os.path.join(os.path.abspath(args.root), ".no-briefs")} if session else {}
    try:
        srv = DashboardServer(args.port, store, args.page, tailnet_names=allowed_names(args.allow_host), nav_file=args.nav_file, **extra)
    except OSError as e:
        sys.stderr.write(f"dashboard-serve: cannot bind {HOST}:{args.port}: {e}\n")
        return 1
    if store.seed_meta():
        sys.stderr.write("dashboard-serve: seeded meta/state\n")
    sys.stderr.write(f"dashboard-serve: pid {os.getpid()} serving http://{HOST}:{srv.server_address[1]}/ "
                     f"store={store.root} extra_hosts={sorted(srv.tailnet_names)}\n")
    sys.stderr.flush()
    addr = f"{HOST}:{srv.server_address[1]}"
    signal.signal(signal.SIGTERM, _raise_interrupt)
    if not session:   # the discovery file belongs to the Architect's server
        write_addr_file(addr)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        if not session:
            remove_addr_file(addr)
        srv.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
