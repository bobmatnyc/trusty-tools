import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile

import pytest

from trusty_browser import install_host
from trusty_browser.native_host import ORIGIN

ROOT = Path(install_host.__file__).parent


def frame(message):
    payload = json.dumps(message).encode()
    return struct.pack("=I", len(payload)) + payload


@pytest.fixture
def short_path():
    with tempfile.TemporaryDirectory(dir="/tmp") as directory:
        yield Path(directory)


def test_registered_host_round_trips_native_messages_to_unix_socket(short_path):
    tmp_path = short_path
    state = tmp_path / "state"
    manifest_dir = tmp_path / "manifests"
    manifest = install_host.install(state, manifest_dir)
    assert install_host.install(state, manifest_dir) == manifest
    settings = json.loads(manifest.read_text())
    assert settings["allowed_origins"] == [ORIGIN]
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
        sockpath = state / "service.sock"
        server.bind(str(sockpath))
        os.chmod(sockpath, 0o600)
        server.listen()
        server.settimeout(3)
        with subprocess.Popen([settings["path"], ORIGIN], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE) as child:
            with server.accept()[0] as peer:
                peer.settimeout(3)
                hello, reply = frame({"profile": "test"}), frame({"ok": True})
                child.stdin.write(hello[:2])
                child.stdin.flush()
                child.stdin.write(hello[2:])
                child.stdin.flush()
                assert peer.recv(len(hello), socket.MSG_WAITALL) == hello
                peer.sendall(reply)
                assert child.stdout.read(len(reply)) == reply
                peer.shutdown(socket.SHUT_RDWR)
                peer.close()
                assert child.wait(3) == 0


def test_installer_preserves_unrelated_manifest(tmp_path):
    (tmp_path / "com.trusty.browser.json").write_text("{}")
    with pytest.raises(FileExistsError):
        install_host.install(tmp_path / "state", tmp_path)
    assert (tmp_path / "com.trusty.browser.json").read_text() == "{}"


@pytest.mark.parametrize("origin", ["chrome-extension://evil/", ""])
def test_native_host_refuses_foreign_origin(tmp_path, origin):
    result = subprocess.run([sys.executable, str(ROOT / "native_host.py"), "--socket",
                             str(tmp_path / "service.sock"), origin], capture_output=True)
    assert result.returncode != 0 and b"origin" in result.stderr

def test_host_refuses_oversize_before_forwarding(short_path):
    manifest = install_host.install(short_path, short_path / "manifests")
    host = json.loads(manifest.read_text())["path"]
    path = short_path / "service.sock"
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
        server.bind(str(path))
        os.chmod(path, 0o600)
        server.listen()
        server.settimeout(3)
        with subprocess.Popen([host, ORIGIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE) as child, server.accept()[0] as peer:
            child.stdin.write(struct.pack("=I", 1024 * 1024 + 1))
            child.stdin.flush()
            assert child.wait(3) == 1
            assert peer.recv(1) == b""
            assert b"length" in child.stderr.read()


def test_manifest_assets_match_registered_native_origin():
    import base64
    import hashlib

    extension = ROOT / "extension"
    manifest = json.loads((extension / "manifest.json").read_text())
    digest = hashlib.sha256(base64.b64decode(manifest["key"])).hexdigest()[:32]
    extension_id = "".join(chr(ord("a") + int(n, 16)) for n in digest)
    assert ORIGIN == f"chrome-extension://{extension_id}/"
    assets = [manifest["background"]["service_worker"], manifest["options_page"],
              *manifest["icons"].values(), *manifest["action"]["default_icon"].values()]
    for content_script in manifest["content_scripts"]:
        assets.extend(content_script["js"])
    assert all((extension / name).is_file() for name in assets)


def test_fake_bootstrap_service_receives_no_credential(short_path):
    manifest = install_host.install(short_path, short_path / "manifests")
    enroll = manifest.with_name("com.trusty.browser.enroll.json")
    bootstrap = json.loads(enroll.read_text())["path"]
    path = short_path / "enrollment.sock"
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as fake:
        fake.bind(str(path))
        os.chmod(path, 0o600)
        fake.listen()
        fake.settimeout(5)
        with subprocess.Popen([bootstrap, ORIGIN], stdin=subprocess.PIPE, stderr=subprocess.PIPE) as process:
            process.stdin.write(frame({"key":"private-test-key"}))
            process.stdin.flush()
            with fake.accept()[0] as peer:
                assert process.wait(5) != 0
                assert peer.recv(1) == b""


def test_chrome_stdin_eof_terminates_host_during_partial_service_frame(short_path):
    manifest = install_host.install(short_path, short_path / "manifests")
    host = json.loads(manifest.read_text())["path"]
    path = short_path / "service.sock"
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
        server.bind(str(path))
        os.chmod(path, 0o600)
        server.listen()
        server.settimeout(3)
        with subprocess.Popen([host, ORIGIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE) as child:
            with server.accept()[0] as peer:
                peer.sendall(struct.pack("=I", 12) + b"{")
                child.stdin.close()
                assert child.wait(3) == 0
                assert child.stdout.read() == b""
