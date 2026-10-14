#!/usr/bin/env python3
"""dashboard-serve.py: localhost server for the Architect dashboard (stdlib only).

Why: the owner (2026-10-08 17:34Z): use a localhost URL until the dashboard becomes a console
page. The page was written for the claude.ai runtime (window.claude.use("db"|"user")).
What: binds 127.0.0.1 only (default port 7890; refuses to start when the port is busy),
serves local/artifacts/owner-queue/index.html (else templates/dashboard/index.html) unchanged with a shim <script> injected
after <head>. The shim defines window.claude.use over JSON endpoints on this server and
emulates onSnapshot by polling every 2 s. Documents live in local/owner-queue via
dashboard_store (the same files scripts/dashboard-db.py writes).
API: GET /api/<coll>[?status=&order_by=&dir=&limit=], GET|PUT|PATCH|DELETE
/api/<coll>/<id> (body {data, if_version}), GET /files/local/briefs/<rel> (read-only,
text/plain, only real files whose realpath is under local/briefs; else 404), POST /api/<coll> (add), POST /api/batch
({ops}), GET /api/user. 409 on if_version mismatch. Requests with a Host other than
127.0.0.1/localhost (any port) or this host's MagicDNS name (Host name, name:<port>, name:443),
or a write whose Origin is not http://127.0.0.1|localhost:<port> or https://<name>[:<port>|:443],
get 403 (DNS-rebinding and CSRF guard). The MagicDNS name comes from `tailscale status --json`
(Self.DNSName) at startup plus any repeatable --allow-host NAME; if tailscale is missing or fails
the server logs one line and stays localhost-only. The bind stays 127.0.0.1; tailscale serve
(`tailscale serve --bg --https=7890 http://127.0.0.1:7890`) is the only tailnet path.
Discovery (trusty-tools #9474): after a successful bind the server writes 127.0.0.1:<port> to
<data dir>/trusty-architect/http_addr (data dir: $TRUSTY_DATA_DIR_OVERRIDE, else ~/Library/Application
Support on macOS, else ${XDG_DATA_HOME:-~/.local/share}) and removes it on shutdown if still its own.
Reload (the owner 2026-10-09): every response carries X-Page-Rev (sha1[:16] of the page file); the shim holds the rev it
was served with and reloads the tab once when it differs, after window.__stashDrafts().
Test: scripts/test_dashboard_store.py, scripts/test_dashboard_reload.py, scripts/test_dashboard_discovery.py, scripts/test_dashboard_tailnet.py
"""
import argparse
import hashlib
import json
import os
import signal
import shutil
import socket
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlsplit

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)
import dashboard_store as ds  # noqa: E402

DEFAULT_PORT = 7890
HOST = "127.0.0.1"
# The installation's own page when it has one, else the shipped template (#8436).
_LOCAL_PAGE = os.path.join(ds.REPO, "local", "artifacts", "owner-queue", "index.html")
PAGE = _LOCAL_PAGE if os.path.exists(_LOCAL_PAGE) else os.path.join(ds.REPO, "templates", "dashboard", "index.html")
BRIEFS = os.path.join(ds.REPO, "local", "briefs")
MAX_BODY = 1 << 20

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
  function refreshAll() { listeners.forEach(function (h) { h.tick(); }); }
  async function write(method, path, body) { try { return await api(method, path, body); } finally { refreshAll(); } }
  function docSnap(id, d) { return { id: id, exists: !!d, version: d ? d.version : undefined, data: function () { return d ? d.data : undefined; } }; }
  function querySnap(res) {
    var docs = res.docs.map(function (d) { return docSnap(d.id, d); });
    return { docs: docs, size: docs.length, empty: docs.length === 0, forEach: function (f) { docs.forEach(f); } };
  }
  function subscribe(load, cb, err) {
    var last = null, fails = 0, stopped = false, busy = false;
    function tick() { return run(false); }
    // The first load runs even in a hidden tab (else the page shows "Loading…" until shown); later polls run while hidden too (the browser throttles them), so a new decision can still beep.
    async function run(force) {
      if (stopped || busy) return;
      busy = true;
      try {
        var r = await load(); fails = 0;
        if (r.rev !== last) { last = r.rev; if (!stopped) { try { cb(r.snap); } catch (e) { console.error(e); } } }
      } catch (e) { fails++; if (fails === FAIL_LIMIT && err) { try { err(e); } catch (_) {} } }
      busy = false;
    }
    var h = { tick: tick }; listeners.add(h); run(true);
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
      return subscribe(async function () {
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
      return subscribe(async function () { var r = await api("GET", qs()); return { rev: r.rev, snap: querySnap(r) }; }, cb, err);
    };
  }
  var db = {
    collection: function (name) { return new Query(name, {}); },
    doc: function (path) { var i = path.indexOf("/"); return new DocRef(path.slice(0, i), path.slice(i + 1)); }
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

    def _send(self, status, payload, ctype="application/json; charset=utf-8"):
        body = payload if isinstance(payload, bytes) else json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
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
            if parts[0] == "files":
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                return self._file(unquote(url.path))
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

    def _api(self, parts, q):
        store, m = self.server.store, self.command
        if parts == ["user"] and m == "GET":
            try:
                name = store.get("meta", "state")["data"].get("owner_name") or "Owner"
            except ds.NotFound:
                name = "Owner"
            return self._send(200, {"id": "local", "name": name, "can_write": True})
        if parts == ["batch"] and m == "POST":
            return self._send(200, {"results": store.batch(self._body().get("ops"))})
        if len(parts) == 1:
            coll = parts[0]
            if m == "GET":
                docs = store.list_docs(
                    coll, status=(q.get("status") or [None])[0], order_by=(q.get("order_by") or [None])[0],
                    desc=(q.get("dir") or ["asc"])[0] == "desc",
                    limit=int(q["limit"][0]) if q.get("limit") and q["limit"][0].isdigit() else None)
                return self._send(200, {"docs": docs, "rev": rev_of(docs)})
            if m == "POST":
                return self._send(201, store.add(coll, self._body().get("data")))
        if len(parts) == 2:
            coll, doc_id = parts
            if m == "GET":
                return self._send(200, store.get(coll, doc_id))
            if m in ("PUT", "PATCH"):
                b = self._body()
                fn = store.set if m == "PUT" else store.update
                return self._send(200, fn(coll, doc_id, b.get("data"), b.get("if_version")))
            if m == "DELETE":
                iv = (q.get("if_version") or [None])[0]
                return self._send(200, store.delete(coll, doc_id, int(iv) if iv is not None and iv.isdigit() else None))
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


def allowed_names(extra=(), lookup=tailnet_name):
    """Tailnet names admitted by the guard: the looked-up MagicDNS name plus --allow-host values."""
    names = {n.strip().rstrip(".").lower() for n in extra if n and n.strip()}
    found = lookup()
    if found:
        names.add(found)
    return frozenset(names)


class DashboardServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, port, store, page=PAGE, briefs=BRIEFS, tailnet_names=()):
        self.store, self.page, self.briefs = store, page, briefs
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


def _raise_interrupt(signum, frame):
    raise KeyboardInterrupt


def main(argv=None):
    ap = argparse.ArgumentParser(description="Serve the Architect dashboard on 127.0.0.1.")
    ap.add_argument("--port", type=int, default=DEFAULT_PORT)
    ap.add_argument("--root", default=ds.DEFAULT_ROOT, help="document store directory (default local/owner-queue)")
    ap.add_argument("--page", default=PAGE)
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
    store = ds.Store(args.root)
    try:
        srv = DashboardServer(args.port, store, args.page, tailnet_names=allowed_names(args.allow_host))
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
    write_addr_file(addr)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        remove_addr_file(addr)
        srv.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
