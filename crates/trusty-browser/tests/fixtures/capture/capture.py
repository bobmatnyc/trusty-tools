"""Capture parity fixtures from the Python trusty-browser 0.7.0 broker and MCP server.

Why: the Rust port must read Python-written state and speak the same wire
format byte for byte (docs/plans/trusty-browser-rust-port.md, section 3).
What: runs the real 0.7.0 ``Broker`` in-process against a temp state dir under
a scratch HOME, drives it over its Unix sockets with a raw client that records
every length-prefixed frame, and runs ``trusty-browser-mcp`` in both modes to
record ``initialize`` and ``tools/list``. Randomness (nonces, ids, AES-GCM
nonces, uuid4) and the audit clock are replaced by seeded functions so a rerun
writes identical files.
Keychain: the vault's service name is replaced by a test-only name, its item
holds a throwaway key generated here, and the item is deleted on exit.

Run (macOS only; see README.md in this directory):
    HOME=$(mktemp -d) <venv>/bin/python capture.py --out ..
"""

from __future__ import annotations

import argparse
import asyncio
import base64
import hashlib
import importlib.metadata
import itertools
import json
import os
import shutil
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import threading
import uuid
from pathlib import Path

TEST_SERVICE = "com.trusty.browser.fixture-capture"
SEED = b"trusty-browser-phase0-fixtures"
HEADER = struct.Struct("=I")

_counter = itertools.count()


def _det(n: int) -> bytes:
    """``n`` deterministic bytes from a seeded SHA-256 counter stream."""
    out = b""
    while len(out) < n:
        out += hashlib.sha256(SEED + str(next(_counter)).encode()).digest()
    return out[:n]


def install_determinism() -> None:
    import secrets

    from trusty_browser import audit, codes, vault

    secrets.token_hex = lambda n=32: _det(n).hex()
    secrets.token_urlsafe = lambda n=32: base64.urlsafe_b64encode(_det(n)).rstrip(b"=").decode()
    uuid.uuid4 = lambda: uuid.UUID(bytes=_det(16), version=4)
    vault.os = _OsWithDetUrandom()
    vault.SERVICE = TEST_SERVICE
    ticks = itertools.count()
    audit.time = type("T", (), {"time": staticmethod(lambda: 1_790_000_000.0 + next(ticks) * 0.25)})
    codes.clock = lambda: 1000.0


class _OsWithDetUrandom:
    """``os`` with ``urandom`` seeded; used only by the vault's AES-GCM nonce."""

    def __getattr__(self, name: str):
        return getattr(os, name)

    @staticmethod
    def urandom(n: int) -> bytes:
        return _det(n)


FIXTURE_KEY = base64.b64encode(hashlib.sha256(SEED + b"keychain").digest()).decode()


class SecurityCli:
    """Keychain access through ``/usr/bin/security`` under the operator's real HOME.

    Why: under a scratch HOME the Security framework finds no default keychain
    and blocks on a UI prompt. The vault's own lookup (service, account,
    base64 decode, AES-GCM) still runs; only the transport changes, and only
    the test service is ever named.
    """

    def __init__(self, keychain: Path) -> None:
        self.keychain = str(keychain)

    def _run(self, *args: str) -> subprocess.CompletedProcess:
        assert args[args.index("-s") + 1] == TEST_SERVICE
        # The throwaway keychain is always named explicitly; the default and
        # search lists are never read or changed.
        import pwd

        env = {**os.environ, "HOME": pwd.getpwuid(os.getuid()).pw_dir}
        return subprocess.run(["/usr/bin/security", *args, self.keychain], env=env,
                              capture_output=True, text=True, timeout=15, stdin=subprocess.DEVNULL)

    def set_password(self, service: str, account: str, value: str) -> None:
        r = self._run("add-generic-password", "-U", "-s", service, "-a", account, "-w", value)
        if r.returncode:
            raise RuntimeError("security add-generic-password failed: " + r.stderr)

    def get_password(self, service: str, account: str) -> str | None:
        r = self._run("find-generic-password", "-s", service, "-a", account, "-w")
        return r.stdout.strip() if r.returncode == 0 else None

    def delete_password(self, service: str, account: str) -> None:
        self._run("delete-generic-password", "-s", service, "-a", account)


class Recorder:
    def __init__(self, out: Path) -> None:
        self.dir = out / "frames"
        self.dir.mkdir(parents=True, exist_ok=True)
        self.index: list[dict[str, object]] = []

    def add(self, sock: str, direction: str, name: str, raw: bytes) -> None:
        file = f"{len(self.index):02d}-{name}.bin"
        (self.dir / file).write_bytes(raw)
        self.index.append({"file": file, "socket": sock, "direction": direction, "message": name})


class Wire:
    """A raw framed client that records each frame's exact bytes."""

    def __init__(self, rec: Recorder, sock_name: str, reader, writer) -> None:
        self.rec, self.sock, self.r, self.w = rec, sock_name, reader, writer

    @classmethod
    async def open(cls, rec: Recorder, state: Path, name: str) -> "Wire":
        r, w = await asyncio.open_unix_connection(str(state / name))
        return cls(rec, name, r, w)

    async def send(self, label: str, frame: dict) -> None:
        body = json.dumps(frame, separators=(",", ":"), ensure_ascii=False).encode()
        raw = HEADER.pack(len(body)) + body
        self.rec.add(self.sock, "client_to_broker", label, raw)
        self.w.write(raw)
        await self.w.drain()

    async def recv(self, label: str) -> dict:
        head = await asyncio.wait_for(self.r.readexactly(4), 10)
        body = await self.r.readexactly(HEADER.unpack(head)[0])
        self.rec.add(self.sock, "broker_to_client", label, head + body)
        return json.loads(body)

    def close(self) -> None:
        self.w.close()


async def once(rec, state, sock, label, frame, refusals, reply_label=None):
    wire = await Wire.open(rec, state, sock)
    await wire.send(label, frame)
    reply = await wire.recv(reply_label or label + "-reply")
    wire.close()
    if reply.get("ok") is False:
        refusals.append({"socket": sock, "case": label, "request": frame, "reply": reply})
    return reply


def credential(n: int, email: str) -> dict[str, str]:
    return {
        "profile": str(uuid.UUID(bytes=hashlib.sha256(SEED + b"profile%d" % n).digest()[:16], version=4)),
        "email": email,
        "key": hashlib.sha256(SEED + b"browser-key%d" % n).hexdigest(),
    }


async def worker(wire: Wire, stop: asyncio.Event) -> None:
    """A fake extension worker: answer every broker command for the paired email."""
    seen = 0
    while not stop.is_set():
        try:
            cmd = await wire.recv(f"worker-command-{seen}")
        except (asyncio.IncompleteReadError, ConnectionError, TimeoutError):
            return
        op = cmd.get("op")
        result: object = {}
        if op == "page_tabs":
            result = {"tabs": [{"tab": 7, "window": 1, "index": 0, "active": True,
                                "access": True, "origin": "example.test", "title": "Example"}]}
        reply = {"id": cmd["id"], "ok": True, "email": cmd["email"], "result": result}
        if "tab" in cmd:
            reply["tab"] = cmd["tab"]
        await wire.send(f"worker-reply-{seen}", reply)
        seen += 1


async def handshake(rec, state, cred, refusals, *, label="pair"):
    wire = await Wire.open(rec, state, "service.sock")
    hello = {"type": "hello", "profile": cred["profile"], "email": cred["email"],
             "incarnation": "inc-1", "nonce": _det(32).hex()}
    await wire.send(f"{label}-hello", hello)
    challenge = await wire.recv(f"{label}-challenge")
    from trusty_browser.protocol import pairing_proof

    assert challenge["proof"] == pairing_proof(cred["key"], "broker", hello, challenge["nonce"])
    proof = pairing_proof(cred["key"], "extension", hello, challenge["nonce"])
    await wire.send(f"{label}-proof", {"type": "proof", "proof": proof})
    ack = await wire.recv(f"{label}-ack")
    assert ack["ok"] is True, ack
    return wire, {"hello": hello, "server_nonce": challenge["nonce"], "key": cred["key"],
                  "broker_proof": challenge["proof"], "extension_proof": proof}


async def drive(state: Path, out: Path) -> dict:
    from trusty_browser import service

    rec = Recorder(out)
    refusals: list[dict] = []
    alpha = credential(1, "alpha@example.test")
    beta = credential(2, "bêta.ünï@exämple.test")
    alpha_new = credential(3, "alpha@example.test")

    async with service.Broker(state) as broker:
        # One enrollment call with the real Chrome-host check: this process is not Chrome.
        await once(rec, state, "enrollment.sock", "enroll-host-refused", {"op": "pair", **alpha}, refusals)
        service.verify_chrome_host = lambda *_: None  # same seam the 0.7.0 tests use
        await once(rec, state, "enrollment.sock", "enroll-pair", {"op": "pair", **alpha}, refusals)
        await once(rec, state, "enrollment.sock", "enroll-pair-repeat", {"op": "pair", **alpha}, refusals)
        await once(rec, state, "enrollment.sock", "enroll-pair-beta", {"op": "pair", **beta}, refusals)
        await once(rec, state, "enrollment.sock", "enroll-conflict-key",
                   {"op": "pair", **{**alpha, "key": "0" * 64}}, refusals)
        await once(rec, state, "enrollment.sock", "enroll-invalid", {"op": "pair", **alpha, "x": 1}, refusals)
        await once(rec, state, "enrollment.sock", "enroll-removed-op", {"op": "enroll", **alpha}, refusals)
        await once(rec, state, "enrollment.sock", "enroll-unpair-bad",
                   {"op": "unpair", **{**beta, "key": "1" * 64}}, refusals)

        # Handshake refusals.
        for label, hello in (
            ("hello-profile-mismatch", {"type": "hello", "profile": alpha["profile"],
                                         "email": "other@example.test", "incarnation": "i", "nonce": "a" * 64}),
            ("hello-invalid-nonce", {"type": "hello", "profile": alpha["profile"],
                                      "email": alpha["email"], "incarnation": "i", "nonce": "A" * 64}),
            ("hello-extra-field", {"type": "hello", "profile": alpha["profile"], "email": alpha["email"],
                                    "incarnation": "i", "nonce": "a" * 64, "x": 1}),
        ):
            await once(rec, state, "service.sock", label, hello, refusals)
        wire = await Wire.open(rec, state, "service.sock")
        bad = {"type": "hello", "profile": beta["profile"], "email": beta["email"],
               "incarnation": "i", "nonce": "b" * 64}
        await wire.send("hello-proof-mismatch", bad)
        await wire.recv("hello-proof-mismatch-challenge")
        await wire.send("proof-mismatch", {"type": "proof", "proof": "0" * 64})
        reply = await wire.recv("proof-mismatch-reply")
        refusals.append({"socket": "service.sock", "case": "proof-mismatch", "request": bad, "reply": reply})
        wire.close()

        # Agent before any Connect.
        await once(rec, state, "agent.sock", "agent-attach-not-connected", {"op": "attach"}, refusals)

        # A real paired worker, Connect, attach, a page op, revoke.
        browser, vector = await handshake(rec, state, alpha, refusals)
        stop = asyncio.Event()
        work = asyncio.create_task(worker(browser, stop))
        await once(rec, state, "enrollment.sock", "popup-connect", {"op": "connect", **alpha}, refusals)
        await once(rec, state, "enrollment.sock", "popup-grants", {"op": "grants", **alpha}, refusals)
        await once(rec, state, "agent.sock", "agent-profiles", {"op": "profiles"}, refusals)
        await once(rec, state, "agent.sock", "agent-attach-extra", {"op": "attach", "x": 1}, refusals)
        agent = await Wire.open(rec, state, "agent.sock")
        await agent.send("agent-attach", {"op": "attach"})
        await agent.recv("agent-attach-ack")
        await agent.send("agent-tabs", {"op": "tabs"})
        await agent.recv("agent-tabs-reply")
        await agent.send("agent-select-no-request-id", {"op": "select", "tab": 7})
        r = await agent.recv("agent-select-no-request-id-reply")
        refusals.append({"socket": "agent.sock", "case": "select-no-request-id",
                         "request": {"op": "select", "tab": 7}, "reply": r})
        await agent.send("agent-unknown-op", {"op": "nope"})
        r = await agent.recv("agent-unknown-op-reply")
        refusals.append({"socket": "agent.sock", "case": "unknown-op", "request": {"op": "nope"}, "reply": r})
        await once(rec, state, "operator.sock", "operator-grants", {"op": "grants"}, refusals)
        await once(rec, state, "operator.sock", "operator-pairings", {"op": "pairings"}, refusals)
        await once(rec, state, "operator.sock", "operator-revoke-unknown",
                   {"op": "revoke_grant", "grant": "g-000000000000"}, refusals)
        await once(rec, state, "operator.sock", "operator-unpair-unknown",
                   {"op": "unpair", "profile": str(uuid.UUID(int=1, version=4))}, refusals)
        await once(rec, state, "operator.sock", "operator-bad-op", {"op": "bogus"}, refusals)
        await once(rec, state, "operator.sock", "operator-revoke-grants", {"op": "revoke_grants"}, refusals)
        await asyncio.sleep(0.3)  # let the grant_end notice reach the worker
        agent.close()
        # Same-email re-pair under a new profile id replaces alpha's row.
        await once(rec, state, "enrollment.sock", "enroll-pair-replace", {"op": "pair", **alpha_new}, refusals)
        stop.set()
        browser.close()
        await asyncio.sleep(0.1)
        work.cancel()

    (out / "frames" / "index.json").write_text(json.dumps(rec.index, indent=2, ensure_ascii=False) + "\n")
    (out / "refusals.json").write_text(json.dumps(refusals, indent=2, ensure_ascii=False) + "\n")
    return {"live_handshake": vector, "pairings": [alpha_new, beta]}


def proof_vectors(live: dict) -> list[dict]:
    from trusty_browser.protocol import pairing_proof

    cases = [live["hello"] | {"key": live["key"], "server_nonce": live["server_nonce"]}]
    cases.append({"type": "hello", "profile": credential(2, "")["profile"], "email": "bêta.ünï@exämple.test",
                  "incarnation": "inc-ü", "nonce": _det(32).hex(), "key": credential(2, "")["key"],
                  "server_nonce": _det(32).hex()})
    vectors = []
    for case in cases:
        hello = {k: case[k] for k in ("type", "profile", "email", "incarnation", "nonce")}
        for role in ("broker", "extension"):
            transcript = json.dumps(
                ["trusty-browser-native-v1", role, "chrome-extension://jabfhcolknbbdajilgpficniedhcfahc",
                 hello["profile"], hello["email"], hello["incarnation"], hello["nonce"], case["server_nonce"]],
                separators=(",", ":"), ensure_ascii=False).encode()
            vectors.append({"role": role, "key": case["key"], "hello": hello,
                            "server_nonce": case["server_nonce"], "transcript_hex": transcript.hex(),
                            "mac": pairing_proof(case["key"], role, hello, case["server_nonce"])})
    return vectors


def fake_agent_sock(path: Path) -> threading.Thread:
    """Answer the workflow-mode ``open`` call so ``trusty-browser-mcp`` reaches tools/list."""
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(str(path))
    srv.listen(1)

    def serve() -> None:
        conn, _ = srv.accept()
        with conn:
            (n,) = HEADER.unpack(conn.recv(4))
            conn.recv(n)
            body = b'{"ok":true}'
            conn.sendall(HEADER.pack(len(body)) + body)
            conn.recv(1)  # held until the MCP process exits
        srv.close()

    t = threading.Thread(target=serve, daemon=True)
    t.start()
    return t


def mcp_capture(venv_bin: Path, args: list[str], env: dict) -> dict:
    proc = subprocess.Popen([str(venv_bin / "trusty-browser-mcp"), *args], stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
    assert proc.stdin and proc.stdout
    msgs = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize",
         "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "fixture-capture", "version": "0"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    ]
    lines = {}
    for msg in msgs:
        proc.stdin.write((json.dumps(msg) + "\n").encode())
        proc.stdin.flush()
        if "id" in msg:
            lines[msg["method"]] = proc.stdout.readline().decode().rstrip("\n")
    proc.stdin.close()
    proc.wait(timeout=10)
    return lines


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", type=Path, required=True)
    opts = parser.parse_args()
    if sys.platform != "darwin":
        sys.exit("capture needs macOS: the 0.7.0 vault seals with the login Keychain")
    home = Path(os.environ["HOME"]).resolve()
    if home == Path(os.path.expanduser("~" + os.environ.get("USER", ""))).resolve():
        sys.exit("run under a scratch HOME (HOME=$(mktemp -d)), never the real one")
    out = opts.out.resolve()
    install_determinism()
    import keyring.backends.macOS as mac

    from trusty_browser import vault

    keychain = os.environ.get("TB_CAPTURE_KEYCHAIN")
    if not keychain or not Path(keychain).is_file():
        sys.exit("run through capture.sh, which creates the throwaway keychain")
    store = SecurityCli(Path(keychain))
    mac.Keyring = lambda: store  # the vault imports Keyring inside _key()
    root = Path(tempfile.mkdtemp(dir="/tmp", prefix="tb-capture-"))
    state = root / "state"
    state.mkdir(mode=0o700)
    account = str(state.resolve())
    store.set_password(TEST_SERVICE, account, FIXTURE_KEY)
    try:
        assert vault.SERVICE == TEST_SERVICE
        result = asyncio.run(drive(state, out))
        shutil.copyfile(state / "registry.sqlite3", out / "registry.sqlite3")
        shutil.copyfile(state / "audit.jsonl", out / "audit.jsonl")
        (out / "proof_vectors.json").write_text(
            json.dumps(proof_vectors(result["live_handshake"]), indent=2, ensure_ascii=False) + "\n")
        (out / "vault.json").write_text(json.dumps({
            "note": "throwaway key generated for these fixtures; deleted from the Keychain after capture",
            "keychain_service": TEST_SERVICE,
            "key_b64": FIXTURE_KEY,
            "aad_prefix": "browser:",
            "pairings": result["pairings"],
        }, indent=2, ensure_ascii=False) + "\n")
    finally:
        store.delete_password(TEST_SERVICE, account)
        shutil.rmtree(root)

    venv_bin = Path(sys.executable).parent
    mcp_dir = out / "mcp"
    mcp_dir.mkdir(exist_ok=True)
    env = {**os.environ, "PYTHONHASHSEED": "0"}
    connect_state = Path(tempfile.mkdtemp(dir="/tmp", prefix="tb-mcp-"))
    try:
        for name, line in mcp_capture(venv_bin, ["--state", str(connect_state)], env).items():
            (mcp_dir / f"connect.{name.replace('/', '_')}.json").write_text(line + "\n")
        connect_state.chmod(0o700)
        cred = connect_state / "cred.json"
        cred.write_text(json.dumps({"workflow": "fixture", "ticket": "plain-ticket"}))
        cred.chmod(0o600)
        t = fake_agent_sock(connect_state / "agent.sock")
        args = ["--state", str(connect_state), "--credential", str(cred)]
        for name, line in mcp_capture(venv_bin, args, env).items():
            (mcp_dir / f"workflow.{name.replace('/', '_')}.json").write_text(line + "\n")
        t.join(timeout=5)
    finally:
        shutil.rmtree(connect_state)

    versions = {p: importlib.metadata.version(p) for p in ("trusty-browser", "mcp", "pydantic", "cryptography", "keyring")}
    versions["python"] = sys.version.split()[0]
    versions["sqlite"] = sqlite3.sqlite_version
    (out / "manifest.json").write_text(json.dumps({"captured_from": versions}, indent=2) + "\n")


if __name__ == "__main__":
    main()
