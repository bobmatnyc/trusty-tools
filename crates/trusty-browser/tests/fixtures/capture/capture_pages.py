"""Capture the page-op parity fixtures from the Python trusty-browser 0.7.0 broker.

Why: phase 3 of the Rust port (leases, journal, every page op) must answer
byte for byte as Python does; ``capture.py`` drives only ``tabs`` and a
malformed ``select``.
What: one sequential script against the real 0.7.0 ``Broker``: pair, a paired
worker, Connect, two agents, then every page op, ``release`` and ``switch``,
the journal (retry, ``Conflict``), each ``request_id`` refusal, broker-side
validation refusals, worker refusals (relayed fields, unknown names,
``OutcomeUnknown``), ``TabClaimed`` between sessions, ``StaleLease`` and a
lost reply, and the asset stamp in the hello acknowledgement. The driver plays both the agents and the worker, so frames are
recorded in causal order and a replay can run without concurrency. The page
clocks (``time.monotonic``, ``time.time``) are fixed so lease deadlines and
``expires_at`` are constant. Writes ``frames_pages/`` and ``audit_pages.jsonl``.
Run through ``capture.sh`` (throwaway keychain, scratch HOME, no dialog).
"""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import capture  # noqa: E402

HELLO_NONCE = "c" * 64
ORIGIN = "https://example.test"
PROBE = {"access": True, "document": "doc-1", "origin": ORIGIN, "title": "Example"}


class Pages:
    """A recorder whose entries name a connection and an action (send, recv, close)."""

    def __init__(self, out: Path, state: Path) -> None:
        self.dir = out / "frames_pages"
        if self.dir.exists():
            shutil.rmtree(self.dir)
        self.dir.mkdir(parents=True)
        self.state = state
        self.index: list[dict[str, object]] = []
        self.conns: dict[str, tuple] = {}

    def _add(self, conn: str, action: str, label: str, raw: bytes | None) -> None:
        entry: dict[str, object] = {"conn": conn, "action": action, "label": label}
        if raw is not None:
            name = f"{len(self.index):03d}-{label}.bin"
            (self.dir / name).write_bytes(raw)
            entry["file"] = name
        self.index.append(entry)

    def write(self, rel: str, data: bytes) -> None:
        """Put a file into the state dir before the broker reads it; the replay does the same."""
        target = self.state / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        name = f"{len(self.index):03d}-state-file.bin"
        (self.dir / name).write_bytes(data)
        self.index.append({"conn": "-", "action": "write", "path": rel, "file": name})

    async def open(self, conn: str, sock: str) -> None:
        r, w = await asyncio.open_unix_connection(str(self.state / sock))
        self.conns[conn] = (r, w)
        self.index.append({"conn": conn, "action": "open", "socket": sock})

    async def send(self, conn: str, label: str, frame: dict) -> None:
        body = json.dumps(frame, separators=(",", ":"), ensure_ascii=False).encode()
        raw = capture.HEADER.pack(len(body)) + body
        self._add(conn, "send", label, raw)
        _, w = self.conns[conn]
        w.write(raw)
        await w.drain()

    async def recv(self, conn: str, label: str) -> dict:
        r, _ = self.conns[conn]
        head = await asyncio.wait_for(r.readexactly(4), 10)
        body = await r.readexactly(capture.HEADER.unpack(head)[0])
        self._add(conn, "recv", label, head + body)
        return json.loads(body)

    async def close(self, conn: str, settle: float = 0.2) -> None:
        _, w = self.conns.pop(conn)
        w.close()
        self._add(conn, "close", "close", None)
        await asyncio.sleep(settle)

    async def once(self, conn: str, sock: str, label: str, frame: dict) -> dict:
        await self.open(conn, sock)
        await self.send(conn, label, frame)
        reply = await self.recv(conn, label + "-reply")
        await self.close(conn, 0.05)
        return reply

    async def op(self, agent: str, label: str, request: dict, worker: dict | str | None = None) -> dict:
        """One agent request; ``worker`` is the worker's answer when a command is expected.

        ``{"result": ...}`` answers ok, ``{"error": ..., ...}`` refuses with those
        fields, ``"close"`` drops the worker connection instead of answering.
        """
        await self.send(agent, label, request)
        if worker is not None:
            cmd = await self.recv("worker", label + "-command")
            if worker == "close":
                await self.close("worker", 0.1)
            else:
                assert isinstance(worker, dict)
                reply: dict = {"id": cmd["id"]}
                if "error" in worker:
                    reply |= {"ok": False, **worker}
                else:
                    email = "other@example.test" if worker.get("email_override") else cmd["email"]
                    reply |= {"ok": True, "email": email, "result": worker["result"]}
                    if "tab" in cmd:
                        reply["tab"] = cmd["tab"]
                await self.send("worker", label + "-worker-reply", reply)
        return await self.recv(agent, label + "-reply")


def ok(result: object) -> dict:
    return {"result": result}


async def drive(state: Path, out: Path) -> None:
    from trusty_browser import service
    from trusty_browser.protocol import pairing_proof

    rec = Pages(out, state)
    alpha = capture.credential(1, "alpha@example.test")
    service.verify_chrome_host = lambda *_: None  # same seam the 0.7.0 tests use
    async with service.Broker(state):
        await rec.once("enroll-pair", "enrollment.sock", "enroll-pair", {"op": "pair", **alpha})
        # #11363: the hello acknowledgement carries the published asset stamp.
        manifest = {"manifest_version": 3, "version": "0.7.0", "version_name": "0.7.0+0123456789abcdef"}
        rec.write("extension/manifest.json", (json.dumps(manifest, indent=2) + "\n").encode())
        await rec.open("worker", "service.sock")
        hello = {"type": "hello", "profile": alpha["profile"], "email": alpha["email"],
                 "incarnation": "inc-1", "nonce": HELLO_NONCE}
        await rec.send("worker", "hello", hello)
        challenge = await rec.recv("worker", "challenge")
        proof = pairing_proof(alpha["key"], "extension", hello, challenge["nonce"])
        await rec.send("worker", "proof", {"type": "proof", "proof": proof})
        assert (await rec.recv("worker", "ack"))["ok"] is True
        await rec.once("enroll-connect", "enrollment.sock", "connect", {"op": "connect", **alpha})
        await rec.open("a", "agent.sock")
        await rec.send("a", "attach-a", {"op": "attach"})
        await rec.recv("a", "attach-a-ack")
        A = "a"
        lease = (await rec.op(A, "select", {"op": "select", "tab": 7}, ok(PROBE)))["lease"]
        L = {"op": None, "lease": lease}

        def req(op: str, **fields: object) -> dict:
            return {**L, "op": op, **fields}

        read = {"document": "doc-2", "origin": ORIGIN, "url": ORIGIN + "/a", "title": "Example",
                "text": "", "elements": [{"ref": "e1", "role": "button", "name": "Go"}], "truncated": False}
        await rec.op(A, "page-read", req("page_read"), ok(read))
        await rec.op(A, "page-read-controls", req("page_read", max_chars=1000, viewport_only=True,
                                                  include_text=True, scope_ref="e1"), ok(read))
        found = {"document": "doc-2", "origin": ORIGIN, "matches": [{"ref": "e1"}], "truncated": False}
        await rec.op(A, "page-find", req("page_find", role="button", limit=3), ok(found))
        await rec.op(A, "page-click", req("page_click", request_id="r1", ref="e1"), ok({"document": "doc-2"}))
        await rec.op(A, "page-click-retry", req("page_click", request_id="r1", ref="e1"))
        await rec.op(A, "page-click-conflict", req("page_click", request_id="r1", ref="e9"))
        await rec.op(A, "page-click-no-request-id", req("page_click", ref="e1"))
        await rec.op(A, "page-click-empty-request-id", req("page_click", request_id="", ref="e1"))
        await rec.op(A, "page-click-int-request-id", req("page_click", request_id=5, ref="e1"))
        dialog = {"document": "doc-2", "dialog": {"type": "confirm", "message": "Sure?", "accepted": True}}
        await rec.op(A, "page-click-dialog", req("page_click", request_id="r1b", ref="e1", dialog="accept"),
                     ok(dialog))
        await rec.op(A, "page-hover", req("page_hover", request_id="r2", ref="e1"), ok({"document": "doc-2"}))
        cut = {"document": "doc-2", "truncated": True, "typed": 2, "accepted": 1}
        await rec.op(A, "page-type", req("page_type", request_id="r3", ref="e1", text="hi"), ok(cut))
        scrolled = {"document": "doc-2", "scroll_y": 100, "scroll_height": 900, "viewport_height": 600,
                    "grew": False, "at_end": False}
        await rec.op(A, "page-scroll", req("page_scroll", request_id="r4", to="down", ref=None, wait_ms=0),
                     ok(scrolled))
        await rec.op(A, "page-press", req("page_press", request_id="r5", ref=None, key="Enter",
                                          modifiers=["Shift"]), ok({"document": "doc-2"}))
        await rec.op(A, "page-select", req("page_select", request_id="r6", ref="e2", value="x"),
                     ok({"document": "doc-2", "changed": True, "value": "x"}))
        await rec.op(A, "page-drag", req("page_drag", request_id="r7", ref="e1", target="e2"),
                     ok({"document": "doc-2", "dropped": True}))
        files = [{"name": "a.txt", "type": "text/plain", "data": "aGk="}]
        await rec.op(A, "page-upload", req("page_upload", request_id="r8", ref="e3", files=files),
                     ok({"document": "doc-2"}))
        await rec.op(A, "page-upload-retry", req("page_upload", request_id="r8", ref="e3", files=files))
        download = {"id": 1, "path": "/tmp/a.txt", "url": ORIGIN + "/a.txt", "mime": "text/plain",
                    "state": "complete", "bytes": 2, "total": 2.0, "error": None, "started": "2026-01-01T00:00:00Z"}
        await rec.op(A, "page-downloads", req("page_downloads"), ok({"origin": ORIGIN, "downloads": [download]}))
        landed = {"access": True, "document": "doc-3", "origin": "https://next.example.test", "settled": True}
        await rec.op(A, "page-navigate", req("page_navigate", request_id="r9", url="https://next.example.test/p"),
                     ok(landed))
        await rec.op(A, "page-navigate-floor", req("page_navigate", request_id="r10", url="http://evil.test/"))
        await rec.op(A, "page-navigate-userinfo", req("page_navigate", request_id="r10",
                                                      url="https://u@example.test/"))
        await rec.op(A, "page-back", req("page_back", request_id="r11"), ok(landed))
        await rec.op(A, "page-forward", req("page_forward", request_id="r12"), {"error": "NoHistory", "tab": 7})
        await rec.op(A, "page-reload", req("page_reload", request_id="r13"),
                     ok({"access": False, "document": None, "origin": None, "settled": False}))
        await rec.op(A, "page-click-document-changed", req("page_click", request_id="r14", ref="e1"),
                     {"error": "DocumentChanged", "tab": 7, "document": "doc-4", "junk": 1})
        await rec.op(A, "page-type-text-dropped", req("page_type", request_id="r15", ref="e1", text="abc"),
                     {"error": "TextDropped", "tab": 7, "typed": 3, "accepted": 1, "entered": 1})
        await rec.op(A, "page-type-text-dropped-retry", req("page_type", request_id="r15", ref="e1", text="abc"))
        # Broker-side validation refusals: nothing reaches the worker.
        await rec.op(A, "page-read-bad-max-chars", req("page_read", max_chars=10))
        await rec.op(A, "page-find-no-terms", req("page_find"))
        await rec.op(A, "page-press-bad-key", req("page_press", request_id="r16", ref=None, key="\n",
                                                  modifiers=[]))
        await rec.op(A, "page-press-dup-mods", req("page_press", request_id="r16", ref=None, key="a",
                                                   modifiers=["Shift", "Shift"]))
        await rec.op(A, "page-scroll-element-no-ref", req("page_scroll", request_id="r16", to="element",
                                                          ref=None, wait_ms=0))
        await rec.op(A, "page-upload-bad-base64", req("page_upload", request_id="r16", ref="e3",
                                                      files=[{"name": "a", "type": "", "data": "a"}]))
        await rec.op(A, "page-click-extra", req("page_click", request_id="r16", ref="e1", x=1))
        await rec.op(A, "select-with-request-id", {"op": "select", "tab": 7, "request_id": "r17"})
        await rec.op(A, "select-negative-tab", {"op": "select", "tab": -1})
        await rec.op(A, "select-bool-tab", {"op": "select", "tab": True})
        await rec.op(A, "select-malformed-probe", {"op": "select", "tab": 7}, ok({}))
        await rec.op(A, "tabs", {"op": "tabs"}, ok({"tabs": [
            {"tab": 7, "window": 1, "index": 0, "active": True, "access": True, "origin": ORIGIN,
             "title": "Example", "extra": 1},
            {"tab": "x"}, 3, {"tab": 8, "window": 1}]}))
        # Worker refusals: an unknown name drops the lease; then StaleLease.
        await rec.op(A, "page-hover-weird", req("page_hover", request_id="r18", ref="e1"), {"error": "Weird"})
        await rec.op(A, "page-read-stale", req("page_read"))
        await rec.op(A, "release-stale", req("release"))
        # A worker OutcomeUnknown is journaled; the retry answers before lease validation.
        lease = (await rec.op(A, "select-again", {"op": "select", "tab": 7}, ok(PROBE)))["lease"]
        L["lease"] = lease
        await rec.op(A, "page-hover-unknown", req("page_hover", request_id="r19", ref="e1"),
                     {"error": "OutcomeUnknown", "tab": 7})
        await rec.op(A, "page-hover-unknown-retry", req("page_hover", request_id="r19", ref="e1"))
        await rec.op(A, "page-read-after-unknown", req("page_read"))
        lease = (await rec.op(A, "select-third", {"op": "select", "tab": 7}, ok(PROBE)))["lease"]
        L["lease"] = lease
        await rec.op(A, "page-read-identity", req("page_read"), {"result": read, "email_override": True})
        lease = (await rec.op(A, "select-fourth", {"op": "select", "tab": 7}, ok(PROBE)))["lease"]
        L["lease"] = lease
        await rec.op(A, "switch", req("switch"), ok({"window": 1, "focused": True}))
        await rec.op(A, "switch-bad-shape", req("switch"), ok({"window": 1}))
        lease = (await rec.op(A, "select-fifth", {"op": "select", "tab": 7}, ok(PROBE)))["lease"]
        L["lease"] = lease
        # A second agent: one controller per tab.
        await rec.open("b", "agent.sock")
        await rec.send("b", "attach-b", {"op": "attach"})
        await rec.recv("b", "attach-b-ack")
        await rec.op("b", "b-select-claimed", {"op": "select", "tab": 7})
        await rec.op("b", "b-switch-foreign-lease", {"op": "switch", "lease": lease})
        await rec.op(A, "release", req("release"))
        await rec.op(A, "release-again", req("release"))
        await rec.op("b", "b-select", {"op": "select", "tab": 7}, ok(PROBE))
        # A same-lease select re-probes and keeps the token.
        await rec.op("b", "b-select-repeat", {"op": "select", "tab": 7}, ok(PROBE))
        await rec.op(A, "bad-frame-op", {"op": 5})
        # A lost reply on a mutating op: OutcomeUnknown, the grant enters its grace.
        lease_b = (await rec.op("b", "b-select-8", {"op": "select", "tab": 8},
                                ok({**PROBE, "document": "doc-8"})))["lease"]
        await rec.op("b", "b-page-click-lost", {"op": "page_click", "lease": lease_b, "request_id": "rb1",
                                                "ref": "e1"}, "close")
        await rec.op("b", "b-page-click-lost-retry", {"op": "page_click", "lease": lease_b,
                                                      "request_id": "rb1", "ref": "e1"})
        await rec.op(A, "tabs-no-peer", {"op": "tabs"})
        await rec.close("a")
        await rec.close("b")
    (rec.dir / "index.json").write_text(json.dumps(rec.index, indent=2, ensure_ascii=False) + "\n")


def main() -> None:
    out = Path(sys.argv[sys.argv.index("--out") + 1]).resolve()
    if sys.platform != "darwin":
        sys.exit("capture needs macOS")
    home = Path(os.environ["HOME"]).resolve()
    if home == Path(os.path.expanduser("~" + os.environ.get("USER", ""))).resolve():
        sys.exit("run under a scratch HOME (HOME=$(mktemp -d)), never the real one")
    keychain = os.environ.get("TB_CAPTURE_KEYCHAIN")
    if not keychain or not Path(keychain).is_file():
        sys.exit("run through capture.sh, which creates the throwaway keychain")
    capture.install_determinism()
    from trusty_browser import leases, pages, vault

    fixed = type("T", (), {"monotonic": staticmethod(lambda: 5000.0),
                           "time": staticmethod(lambda: 1_790_000_000.0)})
    pages.time = fixed
    leases.time = fixed
    import keyring.backends.macOS as mac

    store = capture.SecurityCli(Path(keychain))
    mac.Keyring = lambda: store
    root = Path(tempfile.mkdtemp(dir="/tmp", prefix="tb-capture-pages-"))
    state = root / "state"
    state.mkdir(mode=0o700)
    account = str(state.resolve())
    store.set_password(capture.TEST_SERVICE, account, capture.FIXTURE_KEY)
    try:
        assert vault.SERVICE == capture.TEST_SERVICE
        asyncio.run(drive(state, out))
        shutil.copyfile(state / "audit.jsonl", out / "audit_pages.jsonl")
    finally:
        store.delete_password(capture.TEST_SERVICE, account)
        shutil.rmtree(root)


if __name__ == "__main__":
    main()
