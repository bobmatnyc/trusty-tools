"""Serve the key-press fixture for a live trusty-browser check (#11286).

Why: #11286 closes only after Enter and a modified key reach a real page's
handlers through the installed extension. What: serves tests/fixtures/keys.html
on http://127.0.0.1 (inside the URL floor) and prints the agent steps; the
page log is part of the page text, so ``page_read`` shows what the page saw.
Test: the same fixture runs headless in tests/test_pages_dom.py
`test_type_emits_key_events_and_enter_submits_the_form`.

Run: ``python tests/keys_fixture.py [--port N]``; stop with Ctrl-C.
"""

from __future__ import annotations

import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

PAGE = (Path(__file__).with_name("fixtures") / "keys.html").read_bytes()

STEPS = """\
Live check for #11286, against {url}

1. Open {url} in the enrolled Chrome profile. Click the Trusty Browser
   toolbar button on that tab and mint a tab code; save it to a 0600 file.
2. Agent: browser_connect(code_file=<file>), browser_tabs, browser_select(tab=<id>).
3. page_read(lease) -> refs for "User", "First" and "Third".
4. page_type(lease, ref=<User>, text="aB\\n", request_id="t1").
   page_read: the log lists "keydown Shift+B @user", "keydown Enter @user",
   "click @submit" and "submit user=aB".
5. page_press(lease, ref=<First>, key="Tab", request_id="p1"), then
   page_press(lease, key="Tab", modifiers=["Shift"], request_id="p2").
   page_read: "focus @link", then "keydown Shift+Tab @link" and "focus @first".
6. page_press(lease, ref=<Third>, key="a", modifiers=["Control"], request_id="p3").
   page_read: "keydown Control @third", "keydown Control+a @third".
7. browser_disconnect. Record the page_read text of steps 4 to 6 in the issue.
"""


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_: object) -> None:
        pass

    def do_GET(self) -> None:  # noqa: N802 -- BaseHTTPRequestHandler naming
        if self.path.split("?")[0] not in ("/", "/keys.html"):
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(PAGE)))
        self.end_headers()
        self.wfile.write(PAGE)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--port", type=int, default=0, help="0 picks a free port")
    server = ThreadingHTTPServer(("127.0.0.1", parser.parse_args().port), Handler)
    print(
        STEPS.format(url=f"http://127.0.0.1:{server.server_address[1]}/keys.html"),
        flush=True,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
