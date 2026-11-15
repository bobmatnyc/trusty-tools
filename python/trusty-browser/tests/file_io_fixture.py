"""A loopback page with a file input and two download triggers (#11289).

Run it for a live check: ``python tests/file_io_fixture.py`` prints the page URL.
The page shows each picked file's name, size and text in ``#picked`` and posts
it to ``/upload``; ``#server`` shows what the server received. ``#download``
is a same-origin attachment link and ``#blob`` saves a generated blob.
"""

from __future__ import annotations

import threading
from collections.abc import Iterator
from contextlib import contextmanager
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DOWNLOAD_TEXT = "trusty-browser download fixture\n"
PAGE = """<!doctype html><title>File IO fixture</title>
<main>
  <label>Attachment <input id="file" type="file"></label>
  <label>Many <input id="many" type="file" multiple></label>
  <input id="off" type="file" disabled aria-label="Disabled">
  <p id="picked"></p><p id="server"></p>
  <a id="download" href="/download.txt">Download report</a>
  <button id="blob">Save blob</button>
</main>
<script>
const show = async input => {
  const parts = [];
  for (const f of input.files) parts.push(f.name + ":" + f.size + ":" + f.type + ":" + await f.text());
  picked.textContent = parts.join("|");
  const form = new FormData();
  for (const f of input.files) form.append("file", f);
  const r = await fetch("/upload", {method:"POST", body:form});
  server.textContent = await r.text();
};
file.addEventListener("change", () => show(file));
many.addEventListener("change", () => show(many));
blob.onclick = () => {
  const a = document.createElement("a");
  a.href = URL.createObjectURL(new Blob(["blob body"], {type:"text/plain"}));
  a.download = "generated.txt";
  a.click();
};
</script>
"""


@dataclass
class FileSite:
    origin: str
    uploads: list[bytes] = field(default_factory=list)


@contextmanager
def file_site() -> Iterator[FileSite]:
    """Serve the fixture on 127.0.0.1; ``uploads`` collects each POST body."""

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_: object) -> None:
            pass

        def reply(self, body: str, kind: str, **headers: str) -> None:
            data = body.encode()
            self.send_response(200)
            self.send_header("Content-Type", kind)
            self.send_header("Content-Length", str(len(data)))
            for key, value in headers.items():
                self.send_header(key.replace("_", "-"), value)
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self) -> None:
            if self.path == "/download.txt":
                disposition = 'attachment; filename="report.txt"'
                return self.reply(
                    DOWNLOAD_TEXT, "text/plain", Content_Disposition=disposition
                )
            if self.path == "/":
                return self.reply(PAGE, "text/html; charset=utf-8")
            self.send_error(404)

        def do_POST(self) -> None:
            size = int(self.headers.get("Content-Length", "0"))
            if self.path != "/upload" or not 0 < size <= 1024 * 1024:
                return self.send_error(400)
            body = self.rfile.read(size)
            site.uploads.append(body)
            self.reply(f"received {len(body)} bytes", "text/plain")

    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        site = FileSite(f"http://127.0.0.1:{server.server_port}")
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield site
        finally:
            server.shutdown()
            thread.join()


if __name__ == "__main__":
    with file_site() as running:
        print(running.origin + "/", flush=True)
        threading.Event().wait()
