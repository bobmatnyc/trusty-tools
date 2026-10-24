#!/usr/bin/env python3
"""dashboard-serve.py: localhost server for the Architect dashboard (stdlib only).

Why: the dashboard is served on a localhost URL until it becomes a console page (See #10064). The page was written
for the claude.ai runtime (window.claude.use("db"|"user")); this server runs it unchanged.
What: binds 127.0.0.1 only (default port 7890; refuses to start when the port is busy) and serves
templates/dashboard/index.html with a shim <script> injected after <head> (dashboard_page.py). Documents live in
local/owner-queue via dashboard_store (the same files scripts/dashboard-db.py writes). The request handler and the
Host/Origin guard are in dashboard_http.py, the reader's path rules in dashboard_docs.py; this file keeps the server,
the tailnet guard names, the discovery file and the command line, and re-exports the names its callers import.
API: GET /api/<coll>[?status=&order_by=&dir=&limit=], GET|PUT|PATCH|DELETE /api/<coll>/<id> (body {data, if_version};
409 on if_version mismatch), POST /api/<coll> (add), POST /api/batch ({ops}), GET /api/user, GET /config.json
{mode,title,briefs_base,storage_key,nav,tailnet[,briefs_abs]}, GET /files/local/briefs/<rel> (read-only text/plain,
only real files under local/briefs), GET /usage/<account-slug>, GET /doc/<project>/<rel> and /doc-sync/<project>/<rel>
(reader; dashboard_docs.py), GET /briefs, GET /media?type=diagram,image and /media-file/<project>/<rel>
(dashboard_media.py; SVG under a sandbox CSP), GET /vendor/<name> (marked and DOMPurify from templates/dashboard/vendor/;
mermaid.min.js only from the instance tree when present). Architect mode only, read-only: GET /api/il (every Intent
Ledger; dashboard_il.py), GET /api/harness and /api/harness/diff?id= (the Live snapshot, cached 4 s;
dashboard_harness.py), GET /live (templates/dashboard/live.html), GET /api/viz[?days=1..14] (dashboard_viz.py).
Guard: a Host other than 127.0.0.1/localhost:<port> or a tailnet guard name, or a write from another Origin, gets 403.
The guard names are this host's MagicDNS name from `tailscale status --json` (Self.DNSName) at startup plus any
repeatable --allow-host NAME; if tailscale is missing or fails the server logs one line and stays localhost-only.
The bind stays 127.0.0.1; `tailscale serve` is the only tailnet path.
Config (#10161): local/architect.json via architect_config gives `tailnet_url` (served as /config.json tailnet, ""
when unset) and `owner_name` (the /api/user and meta/state seed name; "the owner" when unset). A config that fails to
load is named on stderr and the defaults apply. No owner, host or checkout path ships in this file.
Discovery (#9474): after a successful bind the server writes 127.0.0.1:<port> to <data dir>/trusty-architect/http_addr
(data dir: $TRUSTY_DATA_DIR_OVERRIDE, else ~/Library/Application Support on macOS, else
${XDG_DATA_HOME:-~/.local/share}) and removes it on shutdown if still its own.
Modes: --mode architect (default) is the page above. --mode session --root STORE --title T --briefs-base URL serves the
same page as one PM session's Intent Ledger: it refuses the default local/owner-queue root, writes only under its
--root (the prune log goes to <root>/.archive/), serves no /doc project, serves nav [] and publishes no discovery file.
Test: tests/test_dashboard_store.py, tests/test_dashboard_mode.py, tests/test_dashboard_tailnet.py,
tests/test_dashboard_discovery.py, tests/test_dashboard_reload.py, tests/test_dashboard_doc.py,
tests/test_dashboard_media.py, tests/test_dashboard_media_ids.py, tests/test_dashboard_qualified.py,
tests/test_dashboard_harness.py, tests/test_dashboard_header_cache.py
"""
import argparse
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
from http.server import ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)
import architect_config  # noqa: E402
import dashboard_docs  # noqa: E402
import dashboard_harness  # noqa: E402
import dashboard_store as ds  # noqa: E402
from dashboard_docs import ARCHITECT, list_briefs, resolve_doc, usage_samples  # noqa: E402,F401  (re-exported)
from dashboard_http import Handler, host_allowed, load_nav, origin_allowed  # noqa: E402,F401  (re-exported)
from dashboard_page import (  # noqa: E402,F401  (re-exported)
    LIVE_PAGE, PAGE, SHIM, VENDOR, VENDOR_FILES, VENDOR_INSTANCE, inject, page_rev, rev_of)

DEFAULT_PORT = 7890
HOST = "127.0.0.1"
INBOX = os.path.join(ds.REPO, "inbox")
CLAUDE_PROJECTS = os.path.join(os.environ.get("CLAUDE_CONFIG_DIR") or os.path.expanduser("~/.trusty-tools/trusty-mpm/claude-config"), "projects")
BRIEFS = os.path.join(ds.REPO, "local", "briefs")
LEDGER = os.path.join(ds.REPO, "local", "usage", "ledger.jsonl")
TOKENS = os.path.join(ds.REPO, "local", "usage", "token-hourly.jsonl")
NAV_FILE = os.path.join(ds.REPO, "local", "il-dashboards.json")
IL_SCHEDULE = os.path.join(ds.REPO, "local", "il-schedule.json")
ARCHITECT_IL = os.path.join(ds.REPO, "local", "il")
ARCHITECT_CONFIG = {"mode": "architect", "title": "Architect Dashboard", "briefs_base": "", "storage_key": "architect-dashboard",
                    "briefs_abs": os.path.join(ds.REPO, "local", "briefs") + "/", "tailnet": ""}
TAILSCALE_FALLBACK = "/Applications/Tailscale.app/Contents/MacOS/Tailscale"


def load_projects(repo=None):
    """The reader's project allowlist for `repo` (default this checkout); see dashboard_docs.load_projects."""
    return dashboard_docs.load_projects(repo or ds.REPO)


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


def instance_settings():
    """(tailnet_url, owner_name) from local/architect.json; ("", "") and one stderr line when it cannot be loaded."""
    try:
        cfg = architect_config.load()
    except architect_config.ConfigError as e:
        sys.stderr.write(f"dashboard-serve: local/architect.json not used: {e}\n")
        return "", ""
    return cfg.tailnet_url, cfg.owner_name


class DashboardServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, port, store, page=PAGE, briefs=BRIEFS, tailnet_names=(), projects=None, vendor=VENDOR, ledger=LEDGER,
                 config=None, nav_file=NAV_FILE, il_schedule=IL_SCHEDULE, architect_il=ARCHITECT_IL, tokens=TOKENS,
                 live_page=LIVE_PAGE, inbox=INBOX, claude_projects=CLAUDE_PROJECTS, tailnet_url="", owner_name=""):
        self.tokens = tokens
        self.live_page, self.inbox, self.claude_projects = live_page, inbox, claude_projects
        self.harness_cache = dashboard_harness.HarnessCache()
        self.store, self.page, self.briefs, self.vendor, self.ledger = store, page, briefs, vendor, ledger
        self.config = dict(config or ARCHITECT_CONFIG)
        self.nav_file = nav_file
        self.il_schedule, self.architect_il = il_schedule, architect_il
        # #10161: instance settings come from the caller (main reads local/architect.json); "" means unset.
        self.tailnet_url, self.owner_name = tailnet_url, owner_name
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
    tailnet_url, owner_name = instance_settings()
    try:
        srv = DashboardServer(args.port, store, args.page, tailnet_names=allowed_names(args.allow_host), nav_file=args.nav_file,
                              tailnet_url=tailnet_url, owner_name=owner_name, **extra)
    except OSError as e:
        sys.stderr.write(f"dashboard-serve: cannot bind {HOST}:{args.port}: {e}\n")
        return 1
    if store.seed_meta(**({"owner_name": owner_name} if owner_name else {})):
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
