"""dashboard_http.py: the dashboard's request handler and its Host/Origin guard (split from dashboard-serve.py, #10161).

Why: one handler serves the page, the JSON store API and the read-only views, and every route must sit behind the same
DNS-rebinding and CSRF guard.
What: `Handler` routes GET / (the page with the shim), /config.json, /api/<coll>[/<id>] (store reads and versioned
writes; 409 on if_version mismatch), /api/batch, /api/user, /api/il, /api/harness[/diff], /api/viz, /live, /files,
/usage, /media, /media-file, /doc, /doc-sync, /briefs and /vendor (see dashboard-serve.py for the API list), plus the
dashboard_routes.py routes (PUT /doc, /api/attach, /api/fleet, /api/projects, /api/cli-profiles, /static). A request
whose Host is not 127.0.0.1/localhost:<port> or a guard name (bare, :<port>, :443), or a write whose Origin is not
http://127.0.0.1|localhost:<port> or https://<name>[:<port>|:443], gets 403 (`host_allowed`, `origin_allowed`).
/config.json carries the tailnet origin the server was given (local/architect.json `tailnet_url`, "" when unset) and,
in architect mode only, the session nav from `load_nav`. /api/user names meta/state.owner_name, else the configured
owner_name, else "the owner". The Notion share sidecar is optional: without notion_sync every /doc-sync answer is
{"share": false}. The Secrets and project-lifecycle routes are optional too (#10571): while dashboard_secrets or
dashboard_lifecycle is not shipped, /api/secrets/* and /api/projects/<verb>/<id> fall through to the store API, which
refuses them (404 or 405), and nothing runs. A Content-Length that is not
plain digits is a 400 before any body read, and each connection times out after REQUEST_TIMEOUT_S (#10161). A gzip copy
of /api/harness goes to a client that accepts it. Standard library only. The Rust successor is the console route
(See #10064).
Test: tests/test_dashboard_store.py, tests/test_dashboard_tailnet.py, tests/test_dashboard_mode.py, tests/test_dashboard_doc.py,
tests/test_dashboard_media.py, tests/test_dashboard_harness.py, tests/test_dashboard_il.py, tests/test_dashboard_viz.py,
tests/test_dashboard_concurrency.py
"""
import hashlib
import json
import os
import re
import sys
import time
from http.server import BaseHTTPRequestHandler
from urllib.parse import parse_qs, unquote, urlsplit

import dashboard_docs as docs
import dashboard_harness
import dashboard_il
import dashboard_media
import dashboard_page as page
import dashboard_routes as routes
import dashboard_store as ds
import dashboard_viz

try:
    import notion_sync  # the Notion share sidecar; optional, not shipped yet
except ImportError:
    notion_sync = None
try:  # #10571: the Secrets nav and project lifecycle need the trusty-secrets bridge; optional, not shipped yet
    # #10570: bound as sec_nav; pm-guard read the module name's attribute calls as a secret-file name.
    import dashboard_secrets as sec_nav
except ImportError:
    sec_nav = None
try:
    import dashboard_lifecycle
except ImportError:
    dashboard_lifecycle = None

MAX_BODY = 1 << 20
REQUEST_TIMEOUT_S = 30   # #10161: a stalled client releases its handler thread after this
_DIGITS = re.compile(r"[0-9]+")
LOCAL_NAMES = ("127.0.0.1", "localhost")


class Handler(BaseHTTPRequestHandler):
    server_version = "dashboard-serve"
    timeout = REQUEST_TIMEOUT_S   # socket timeout per connection (StreamRequestHandler)
    # HTTP/1.0 default: one request per connection, so an unread body never poisons the next request.

    # ---- plumbing ----
    def log_request(self, code="-", size="-"):
        # Quiet the 2 s polling: log writes and errors only.
        if self.command != "GET" or (str(code).isdigit() and int(code) >= 400):
            super().log_request(code, size)

    def _send(self, status, payload, ctype="application/json; charset=utf-8", cache="no-store", gz=None):
        body = payload if isinstance(payload, bytes) else json.dumps(payload).encode("utf-8")
        # gz: pre-compressed copy of body; sent only when the client accepts gzip (a phone over the tailnet)
        gzip_ok = gz is not None and "gzip" in (self.headers.get("Accept-Encoding") or "").lower()
        if gzip_ok:
            body = gz
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        if gz is not None:
            self.send_header("Vary", "Accept-Encoding")
        if gzip_ok:
            self.send_header("Content-Encoding", "gzip")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", cache)
        self.send_header("X-Content-Type-Options", "nosniff")
        if getattr(self, "_csp", None):
            self.send_header("Content-Security-Policy", self._csp)
            self._csp = None
        for k, v in (getattr(self, "_extra", None) or {}).items():
            self.send_header(k, v)
        self._extra = None
        try:
            self.send_header("X-Page-Rev", self.server.rev())
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
        raw = (self.headers.get("Content-Length") or "0").strip()
        # #10161: only plain ASCII digits; a negative length would make rfile.read(-1) wait for EOF
        if not _DIGITS.fullmatch(raw):
            raise ds.StoreError("bad Content-Length")
        n = int(raw)
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
                rev = self.server.rev()
                with open(self.server.page, "r", encoding="utf-8") as f:
                    return self._send(200, page.inject(f.read(), rev).encode("utf-8"), "text/html; charset=utf-8")
            if parts == ["config.json"]:
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                # ar-T44: session links only on the Architect dashboard; a session dash serves an empty nav
                nav = load_nav(self.server.nav_file) if self.server.config.get("mode") == "architect" else []
                return self._send(200, dict(self.server.config, nav=nav, tailnet=self.server.tailnet_url, console=self.server.console_url,
                                            features=features()))   # See #10161; #10571: the page hides a tab whose backend is absent
            if parts == ["api", "il"]:
                # Df7: consolidated Intent Ledger view; architect mode only, read-only, GET only
                if self.server.config.get("mode") != "architect":
                    return self._err(404, "not_found", "no such path")
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                return self._send(200, dashboard_il.aggregate(self.server.projects(), self.server.il_schedule, self.server.architect_il))
            if parts == ["live"] or parts == ["live", "kiosk"]:
                # ar-Te8: the Live view page (also /live/kiosk, the same page in kiosk mode); architect mode only, GET only
                if self.server.config.get("mode") != "architect":
                    return self._err(404, "not_found", "no such path")
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                with open(self.server.live_page, "r", encoding="utf-8") as f:
                    return self._send(200, page.inject(f.read(), self.server.rev(), shim_on=False).encode("utf-8"), "text/html; charset=utf-8")
            if parts == ["api", "harness"]:
                # ar-Te8: read-only Live snapshot (sessions, subagents, recent in/out messages); architect mode only, cached ~4 s
                if self.server.config.get("mode") != "architect":
                    return self._err(404, "not_found", "no such path")
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                body, gz = self.server.harness_body(self.server.harness_cache.get(self._harness))
                return self._send(200, body, gz=gz)
            if parts == ["api", "fleet"]:
                return routes.fleet(self)
            if parts == ["api", "projects"]:
                return routes.projects(self)
            if parts == ["api", "cli-profiles"]:
                return routes.cli_profiles(self, url)
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
            if parts[0] == "doc" and self.command == "PUT":
                return routes.doc_put(self, url.path, origin_allowed, _DIGITS)
            if parts[0] in ("doc", "vendor", "doc-sync", "briefs", "static"):
                if self.command not in ("GET", "HEAD"):
                    return self._err(405, "method_not_allowed", "GET only")
                if parts[0] == "static":
                    return routes.static(self, parts[1:])
                if parts[0] == "briefs":
                    return self._send(200, {"briefs": self._brief_rows()}) if len(parts) == 1 else self._err(404, "not_found", "no such path")
                if parts[0] == "doc-sync":
                    return self._doc_sync(url.path, parse_qs(url.query))
                return self._doc(url.path) if parts[0] == "doc" else self._vendor(parts[1:])
            if parts[:2] == ["api", "attach"]:
                return routes.attach(self, parts[2:], parse_qs(url.query), _DIGITS)
            if sec_nav is not None and tuple(parts[:2]) == sec_nav.PREFIX:
                # Secrets nav: POST-only, Origin + CSRF guarded; never returns a value
                return sec_nav.handle(self, parts[2:], host_allowed, origin_allowed)
            if dashboard_lifecycle is not None and tuple(parts[:2]) == dashboard_lifecycle.PREFIX and len(parts) == 3:
                # Projects archive/decommission/restore: POST-only, CSRF guarded
                return dashboard_lifecycle.handle(self, parts[2:], host_allowed, origin_allowed)
            if parts[0] != "api":
                return self._err(404, "not_found", "no such path")
            return self._api(parts[1:], parse_qs(url.query))
        except ds.StoreError as e:
            return self._err(e.status, e.code, str(e))
        except FileNotFoundError as e:
            return self._err(404, "not_found", f"missing file: {e.filename}")
        except (BrokenPipeError, ConnectionResetError, TimeoutError):
            self.close_connection = True   # the client went away mid-response: quiet, no 500, no traceback
            return
        except Exception as e:  # noqa: BLE001 - last-resort 500 with the message in the log
            sys.stderr.write(f"internal error on {self.command} {self.path}: {e!r}\n")
            return self._err(500, "internal", "internal error")

    def _harness(self):
        sv = self.server
        sdocs = [d["data"] for d in sv.store.list_docs("sessions")]
        idocs = [d["data"] for d in sv.store.list_docs("items")]
        gdocs = [d["data"] for d in sv.store.list_docs("goals")]
        roots = sv.projects()
        cwds = {x.get("name"): roots[x.get("project")] for x in sdocs if x.get("name") and x.get("project") in roots}
        try:
            ustate = sv.store.get("meta", "settings")["data"].get("user_state")
        except Exception:  # noqa: BLE001 - settings absent: state unknown, never sleeping
            ustate = None
        return dashboard_harness.build(sv.inbox, sv.claude_projects, sdocs, idocs, project_cwds=cwds, roots=roots, goal_docs=gdocs,
                                       user_state=ustate, tmux=dashboard_harness.tmux_sessions())

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
        return self._send(200, {"samples": docs.usage_samples(self.server.ledger, label)})

    def _doc(self, raw_path):
        """Serve /doc/<project>/<rel>: a brief file, read-only. Segments are unquoted one by one, so an encoded
        slash or dot-dot cannot slip past; every failure is the same 404."""
        segs = [unquote(x) for x in raw_path.split("/")[2:]]
        real = docs.resolve_doc(self.server.projects(), segs[0], segs[1:]) if segs else None
        if real is None or os.path.getsize(real) > docs.MAX_DOC:
            return self._err(404, "not_found", "no such document")
        with open(real, "rb") as f:
            data = f.read()
        self._extra = {"X-Doc-Sha256": hashlib.sha256(data).hexdigest()}   # the base_sha256 a PUT /doc must name
        return self._send(200, data, "text/plain; charset=utf-8")

    def _doc_sync(self, raw_path, query):
        """GET /doc-sync/<project>/<rel>[?refresh=1]: Notion share state for a doc the reader opened. The doc must pass
        the same /doc checks. No share record: {"share": false}. refresh=1 starts a background `notion_sync.py pull`
        (at most once per 5 minutes per doc). The server holds no Notion credentials; the child fetches the key."""
        segs = [unquote(x) for x in raw_path.split("/")[2:]]
        real = docs.resolve_doc(self.server.projects(), segs[0], segs[1:]) if segs else None
        if real is None:
            return self._err(404, "not_found", "no such document")
        doc_id = notion_sync.find_doc_id(real) if notion_sync else None
        if not doc_id:
            return self._send(200, {"share": False})
        if query.get("refresh") == ["1"]:
            notion_sync.maybe_refresh(doc_id)
        return self._send(200, notion_sync.snapshot(doc_id))

    def _brief_rows(self):
        """GET /briefs rows; the architect dash adds a stable B id to each (same `media` collection as diagrams)."""
        store = self.server.store if self.server.config.get("mode") == "architect" else None
        return dashboard_media.brief_rows(store, self.server.projects(), docs.list_briefs(self.server.projects()), docs.denied)

    def _media(self, q):
        """GET /media?type=diagram,image,file (default diagram,image): diagrams, images and .json files under the doc roots."""
        want = {t for v in q.get("type", ["diagram,image"]) for t in v.split(",")} & {"diagram", "image", "file"}
        if not want:
            return []
        store = self.server.store if self.server.config.get("mode") == "architect" else None   # B ids: architect dash only
        return dashboard_media.media_rows(store, self.server.projects(), want, docs.denied, docs.list_briefs(self.server.projects()))

    def _media_file(self, raw_path):
        """GET /media-file/<project>/<rel>: a diagram, image or .json file, read-only. Same 404-for-everything rule as /doc.
        SVG is sandboxed by CSP so a script inside it cannot run even if opened directly."""
        segs = [unquote(x) for x in raw_path.split("/")[2:]]
        real = dashboard_media.resolve_media(self.server.projects(), segs[0], segs[1:], docs.denied) if segs else None
        if real is None:
            return self._err(404, "not_found", "no such file")
        with open(real, "rb") as f:
            data = f.read()
        self._csp = "sandbox; default-src 'none'; style-src 'unsafe-inline'"
        return self._send(200, data, dashboard_media.MEDIA_TYPES[os.path.splitext(real)[1].lower()])

    def _vendor(self, parts):
        path = page.vendor_file(self.server.vendor, parts[0]) if len(parts) == 1 else None
        if path is None:
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
                name = store.get("meta", "state")["data"].get("owner_name")
            except ds.NotFound:
                name = ""
            # #10161: no owner name ships; meta/state, else local/architect.json, else a neutral default.
            name = name or self.server.owner_name or "the owner"
            return self._send(200, {"id": "local", "name": name, "can_write": True})
        if len(parts) == 2 and parts[0] == "media-archive" and m == "POST":   # archive a media item (architect dash only)
            if self.server.config.get("mode") != "architect":
                return self._err(404, "not_found", "no such path")
            return self._send(200, dashboard_media.archive_media(store, self.server.projects(), parts[1], docs.denied))
        if parts == ["batch"] and m == "POST":
            return self._send(200, {"results": store.batch(self._body().get("ops"), actor=self._actor())})
        if len(parts) == 1:
            coll = parts[0]
            if m == "GET":
                rows = store.list_docs(
                    coll, status=(q.get("status") or [None])[0], order_by=(q.get("order_by") or [None])[0],
                    desc=(q.get("dir") or ["asc"])[0] == "desc",
                    limit=int(q["limit"][0]) if q.get("limit") and q["limit"][0].isdigit() else None)
                return self._send(200, {"docs": rows, "rev": page.rev_of(rows)})
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


def features():
    """Which optional backends this install has (#10571): {"secrets", "cli_profiles", "project_lifecycle"} -> bool.
    The page drops the Secrets and CLI Profiles tabs and the archive buttons when their flag is false."""
    return {"secrets": sec_nav is not None, "cli_profiles": routes.cli_profiles_mod is not None,
            "project_lifecycle": dashboard_lifecycle is not None}


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
