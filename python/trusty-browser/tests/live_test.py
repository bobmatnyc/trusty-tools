"""Run the two-account site used for existing Chrome profile acceptance, or probe a live UDS."""

from __future__ import annotations

import argparse
import json
import os
import secrets
import socket
import struct
import subprocess
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass, field
from http.cookies import SimpleCookie
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

ACCOUNTS = ("personal-fixture", "travel-fixture")


@dataclass
class Site:
    origin: str
    base: str
    login: dict[str, str]
    switch: dict[str, str]
    journal: dict[str, list[str]] = field(
        default_factory=lambda: {a: [] for a in ACCOUNTS}
    )
    loads: dict[str, int] = field(default_factory=lambda: {a: 0 for a in ACCOUNTS})


@contextmanager
def qa_site():
    prefix = "/qa-" + secrets.token_urlsafe(16)
    cookie_name = "tb_" + secrets.token_hex(6)
    tickets = {secrets.token_urlsafe(32): a for a in ACCOUNTS}
    switches = {secrets.token_urlsafe(32): a for a in ACCOUNTS}
    sessions = {a: secrets.token_urlsafe(32) for a in ACCOUNTS}
    lock = threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def send(self, status, body="", **headers):
            self.send_response(status)
            self.send_header("Cache-Control", "no-store")
            self.send_header("Referrer-Policy", "no-referrer")
            self.send_header("Content-Type", "text/html; charset=utf-8")
            for key, val in headers.items():
                self.send_header(key, val)
            self.end_headers()
            self.wfile.write(body.encode())

        def account(self):
            cookies = SimpleCookie(self.headers.get("Cookie", ""))
            cookie = cookies.get(cookie_name)
            return next(
                (
                    a
                    for a, value in sessions.items()
                    if cookie and secrets.compare_digest(cookie.value, value)
                ),
                None,
            )

        def do_GET(self):
            path = urlsplit(self.path).path
            if path.startswith(prefix + "/login/"):
                with lock:
                    account = tickets.pop(path.rsplit("/", 1)[1], None)
                if not account:
                    return self.send(403)
                return self.send(
                    303,
                    Location=prefix + "/app",
                    **{
                        "Set-Cookie": f"{cookie_name}={sessions[account]}; Path={prefix}/; HttpOnly; SameSite=Strict; Max-Age=7200"
                    },
                )
            if path.startswith(prefix + "/switch/"):
                account = switches.get(path.rsplit("/", 1)[1])
                if not account:
                    return self.send(403)
                return self.send(
                    200,
                    "QA session changed",
                    **{
                        "Set-Cookie": f"{cookie_name}={sessions[account]}; Path={prefix}/; HttpOnly; SameSite=Strict; Max-Age=7200"
                    },
                )
            if path == prefix + "/logout":
                return self.send(
                    200,
                    "Fixture cookie cleared",
                    **{
                        "Set-Cookie": f"{cookie_name}=; Path={prefix}/; HttpOnly; SameSite=Strict; Max-Age=0"
                    },
                )
            account = self.account()
            if account and path == prefix + "/identity":
                return self.send(200, json.dumps({"account": account}))
            if not account or path != prefix + "/app":
                return self.send(403)
            with lock:
                site.loads[account] += 1
                count = len(site.journal[account])
            self.send(
                200,
                f"""<title>Trusty Browser QA — {account}</title><p id=account>{account}</p>
              <input id=field><button id=apply>Apply</button><p id=count>{count}</p>
              <script>apply.onclick=async()=>{{const expected=apply.dataset.expectedAccount||'';delete apply.dataset.expectedAccount;
              const r=await fetch('apply',{{method:'POST',body:field.value,headers:{{'X-Expected-Account':expected}}}});
              if(r.ok){{count.textContent=await r.text();document.dispatchEvent(new Event('fixture-applied'));}}
              else document.dispatchEvent(new Event('fixture-refused'));}};</script>""",
            )

        def do_POST(self):
            if (
                self.path != prefix + "/apply"
                or self.headers.get("Origin") != site.origin
            ):
                return self.send(403)
            account = self.account()
            if not account or self.headers.get("X-Expected-Account") != account:
                return self.send(403)
            size = int(self.headers.get("Content-Length", "0"))
            if not 0 < size <= 1024:
                return self.send(400)
            marker = self.rfile.read(size).decode()
            with lock:
                site.journal[account].append(marker)
                count = len(site.journal[account])
            self.send(200, str(count))

    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        origin = f"http://127.0.0.1:{server.server_port}"
        base = origin + prefix
        site = Site(
            origin,
            base,
            {a: f"{base}/login/{t}" for t, a in tickets.items()},
            {a: f"{base}/switch/{t}" for t, a in switches.items()},
        )
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield site
        finally:
            server.shutdown()
            thread.join()
