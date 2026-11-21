"""Two-HTTPS-origin UAT for cross-origin iframe reads (#11301).

Why: the unit tests mock chrome.scripting; this proves page_read and page_find
against real Chromium frames, real host permissions and real documentIds.
What: one HTTPS server with a self-signed certificate answers a.test (the top
page, embedding an iframe) and b.test (the child, labelled CHILD-LABEL-7);
Chromium maps both names to 127.0.0.1 with --host-resolver-rules and loads a
copy of the shipped extension in a persistent context. The copy differs only in
its manifest: `host_permissions` lists the origins the case grants, the one way
to hold a host permission without a user's prompt click. The commands run in
the extension's service worker through TrustyPages.commands, as the native
port would drive them.
Run: python3 -m pytest crates/trusty-browser/tests/extension/test_frames_uat.py
Test: `test_granted_child_is_read_and_found_with_attribution`,
`test_child_without_permission_is_denied`, `test_child_ref_is_refused_after_the_frame_navigates`,
`test_include_text_reads_top_and_child_text_on_a_link_heavy_page`.
"""

import json
import shutil
import ssl
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

sync_api = pytest.importorskip("playwright.sync_api")

EXTENSION = Path(__file__).resolve().parents[2] / "extension"
LABEL = "CHILD-LABEL-7"


def pages(port):
    child = f"https://b.test:{port}/child"
    return {
        ("a.test", "/"): f'<title>Top</title><button>Top button</button><iframe src="{child}"></iframe>',
        # #11284: a link-heavy page whose elements alone fill a read's budget.
        ("a.test", "/links"): '<title>Links</title><p>TOP-TEXT body</p>' + "".join(
            f'<a href="/l{i}">Link number {i}</a> ' for i in range(1500)) + f'<iframe src="{child}"></iframe>',
        ("b.test", "/child"): f'<title>Child</title><form><button>{LABEL}</button><a href="/next">Next</a></form>',
        ("b.test", "/next"): "<title>Next</title><button>After navigation</button>",
    }


@pytest.fixture(scope="module")
def site(tmp_path_factory):
    """Serve a.test and b.test over HTTPS on one port; yields the port."""
    tmp = tmp_path_factory.mktemp("tls")
    key, cert = tmp / "key.pem", tmp / "cert.pem"
    subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                    "-subj", "/CN=a.test", "-addext", "subjectAltName=DNS:a.test,DNS:b.test",
                    "-keyout", str(key), "-out", str(cert)], check=True, capture_output=True)
    table = {}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            body = table.get((self.headers.get("Host", "").split(":")[0], self.path))
            self.send_response(200 if body else 404)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write((body or "missing").encode())

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(cert, key)
    server.socket = tls.wrap_socket(server.socket, server_side=True)
    table.update(pages(server.server_port))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    yield server.server_port
    server.shutdown()


SETUP = """async () => {
  globalThis.__qa = TrustyPages.commands({identity:async () => "qa@example.org", grant:async () => ({scope:"profile"}),
    ended:async () => {}, remember:async () => {}, originOf:async () => null});
  const [tab] = await chrome.tabs.query({url:"https://a.test/*"});
  return tab.id;
}"""
ASK = """async ([tab, op, fields]) => {
  try {
    return await __qa.command({id:1, op, email:"qa@example.org", grant:"g", tab, document:null, ...fields});
  } catch (error) { return {error:error.message}; }
}"""
READ = {"scope_ref": None, "max_chars": 50000, "viewport_only": False, "include_text": False}
FIND = {"role": None, "name": LABEL, "text": None, "within_ref": None, "limit": 10}


def browse(tmp_path, port, granted, body, path="/"):
    """Load the extension copy granting `granted` origins, open a.test `path`, and run `body(ask, tab)`."""
    copy = tmp_path / "extension"
    shutil.copytree(EXTENSION, copy)
    manifest = json.loads((copy / "manifest.json").read_text())
    manifest["host_permissions"] = [f"https://{host}/*" for host in granted]
    (copy / "manifest.json").write_text(json.dumps(manifest))
    with sync_api.sync_playwright() as driver:
        context = driver.chromium.launch_persistent_context(
            str(tmp_path / "profile"), channel="chromium", headless=True, ignore_https_errors=True,
            args=[f"--disable-extensions-except={copy}", f"--load-extension={copy}",
                  "--host-resolver-rules=MAP a.test 127.0.0.1, MAP b.test 127.0.0.1"])
        try:
            worker = context.service_workers[0] if context.service_workers else context.wait_for_event("serviceworker")
            tab = context.pages[0] if context.pages else context.new_page()
            tab.goto(f"https://a.test:{port}{path}")
            tab.wait_for_function("document.querySelector('iframe') && document.readyState === 'complete'")
            child = next(f for f in tab.frames if f.url.startswith("https://b.test"))
            child.wait_for_selector("button")
            tab_id = worker.evaluate(SETUP)
            body(lambda op, fields: worker.evaluate(ASK, [tab_id, op, fields]), child)
        finally:
            context.close()


def test_granted_child_is_read_and_found_with_attribution(site, tmp_path):
    def body(ask, _child):
        result = ask("page_read", READ)["result"]
        assert result["origin"] == f"https://a.test:{site}"
        children = [f for f in result["frames"] if f["id"] != 0]
        assert [f["origin"] for f in children] == [f"https://b.test:{site}"]
        assert result["denied_frames"] == []
        labelled = [e for e in result["elements"] if e["name"] == LABEL]
        assert len(labelled) == 1 and labelled[0]["frame"] == children[0]["id"]
        assert labelled[0]["ref"].startswith(f'{children[0]["id"]}:')
        found = ask("page_find", FIND)["result"]
        assert [m["name"] for m in found["matches"]] == [LABEL]
        assert found["matches"][0]["frame"] == children[0]["id"]
    browse(tmp_path, site, ["a.test", "b.test"], body)


def test_child_without_permission_is_denied(site, tmp_path):
    def body(ask, _child):
        result = ask("page_read", READ)["result"]
        assert result["denied_frames"] == [{"origin": f"https://b.test:{site}", "reason": "no_permission"}]
        assert LABEL not in json.dumps(result)
        assert [f["id"] for f in result["frames"]] == [0]
        found = ask("page_find", FIND)["result"]
        assert found["matches"] == [] and found["denied_frames"][0]["reason"] == "no_permission"
    browse(tmp_path, site, ["a.test"], body)


def test_child_ref_is_refused_after_the_frame_navigates(site, tmp_path):
    def body(ask, child):
        result = ask("page_read", READ)["result"]
        form = next(e for e in result["elements"] if e["name"] == LABEL)
        assert "error" not in ask("page_read", {**READ, "scope_ref": form["ref"]})
        child.evaluate("location.href = '/next'")
        child.wait_for_url("**/next")
        child.wait_for_selector("text=After navigation")
        assert ask("page_read", {**READ, "scope_ref": form["ref"]}) == {"error": "StaleElement"}
        assert ask("page_find", {**FIND, "within_ref": form["ref"]}) == {"error": "StaleElement"}
    browse(tmp_path, site, ["a.test", "b.test"], body)


def test_include_text_reads_top_and_child_text_on_a_link_heavy_page(site, tmp_path):
    # #11284: the links must not take the whole budget from the text, and the child's text is read too.
    def body(ask, _child):
        result = ask("page_read", {**READ, "include_text": True})["result"]
        assert result["text"] != ""
        assert "TOP-TEXT body" in result["text"]
        child = next(f for f in result["frames"] if f["id"] != 0)
        assert f'[frame {child["id"]}]' in result["text"] and LABEL in result["text"]
        assert result["truncated"] is True
        assert len(json.dumps(result, separators=(",", ":")).encode()) <= READ["max_chars"]
    browse(tmp_path, site, ["a.test", "b.test"], body, "/links")
