"""Relay Chrome's length-prefixed byte stream to the enrolled service's private Unix socket."""

import argparse
from trusty_browser.mac_peer import kernel_pid, process_argv, process_exe
import os
from pathlib import Path
from queue import Queue
import struct
from threading import Thread
import socket
import stat
import sys

ORIGIN = "chrome-extension://jabfhcolknbbdajilgpficniedhcfahc/"


def private_directory(directory: Path) -> None:
    entry = directory.lstat()
    if (not stat.S_ISDIR(entry.st_mode) or entry.st_uid != os.getuid()
            or entry.st_mode & 0o077):
        raise PermissionError("the service directory must belong to this user and have mode 0700")


def bridge(path: Path, origin: str) -> None:
    if origin != ORIGIN:
        raise PermissionError("unexpected Chrome extension origin")
    private_directory(path.parent)
    entry = path.lstat()
    if not stat.S_ISSOCK(entry.st_mode) or entry.st_uid != os.getuid() or entry.st_mode & 0o077:
        raise PermissionError("expected a private user-owned service socket")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as service:
        service.connect(str(path))
        if path.name == "enrollment.sock":
            verify_enrollment_service(service, path)
        done: Queue[OSError | None] = Queue()

        def transfer(source: int, destination: int) -> None:
            try:
                while header := read_exact(source, 4, allow_eof=True):
                    length, = struct.unpack("=I", header)
                    if not 0 < length <= 1024 * 1024:
                        raise OSError("invalid native message length")
                    message = memoryview(header + read_exact(source, length))
                    while message:
                        message = message[os.write(destination, message):]
            except OSError as exc:
                done.put(exc)
            else:
                done.put(None)

        # Each direction must notice EOF even if the opposite sender stalled mid-frame.
        for src, dst in ((0, service.fileno()), (service.fileno(), 1)):
            Thread(target=transfer, args=(src, dst), daemon=True).start()
        error = done.get()
        if error:
            raise error


def verify_enrollment_service(service: socket.socket, path: Path) -> None:
    """Release bootstrap credentials only to the installed code and expected runtime."""
    if sys.platform != "darwin":
        raise PermissionError("popup enrollment currently requires macOS peer verification")
    pid = kernel_pid(service)
    if process_exe(pid) != process_exe(os.getpid()):
        raise PermissionError("unexpected enrollment service interpreter")
    args = process_argv(pid)
    expected = Path(__file__).resolve().with_name("service.py")
    if (len(args) != 5 or args[1] != str(expected) or args[2] != "--state"
            or not Path(args[3]).is_absolute() or Path(args[3]).resolve() != path.parent.resolve()
            or args[4] != "serve"):
        raise PermissionError("unexpected enrollment service command")


def read_exact(fd: int, count: int, allow_eof: bool = False) -> bytes:
    data = bytearray()
    while len(data) < count:
        chunk = os.read(fd, count - len(data))
        if not chunk:
            if not data and allow_eof:
                return b""
            raise OSError("truncated native message")
        data.extend(chunk)
    return bytes(data)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", type=Path, required=True)
    parser.add_argument("origin")
    args = parser.parse_args()
    try:
        bridge(args.socket, args.origin)
    except OSError as exc:
        print(f"Trusty Browser host: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
