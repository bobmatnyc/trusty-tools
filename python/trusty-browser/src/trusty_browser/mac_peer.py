"""Identify a Unix peer using Darwin kernel process metadata and live code signing."""

from __future__ import annotations

import ctypes
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
from typing import Protocol


class LocalSocket(Protocol):
    def getsockopt(self, level: int, optname: int, buflen: int, /) -> bytes: ...


def kernel_pid(sock: LocalSocket) -> int:
    if sys.platform != "darwin":
        raise PermissionError("Chrome enrollment requires macOS peer authentication")
    (pid,) = struct.unpack("=i", sock.getsockopt(0, 2, 4))  # LOCAL_PEERPID
    if pid <= 1:
        raise PermissionError("invalid peer PID")
    return int(pid)


def process_exe(pid: int) -> Path:
    lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    buf = ctypes.create_string_buffer(4096)
    if lib.proc_pidpath(pid, buf, len(buf)) <= 0:
        raise PermissionError("cannot identify peer executable")
    return Path(os.fsdecode(buf.value)).resolve(strict=True)


def parent_pid(pid: int) -> int:
    lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    # proc_bsdshortinfo for PROC_PIDT_SHORTBSDINFO (sys/proc_info.h).
    info = ctypes.create_string_buffer(64)
    count = lib.proc_pidinfo(pid, 13, 0, info, len(info))
    if count < 40:
        raise PermissionError("cannot identify peer parent")
    actual, parent = struct.unpack_from("=II", info)
    if actual != pid or parent <= 1:
        raise PermissionError("invalid peer ancestry")
    return int(parent)


def process_argv(pid: int) -> list[str]:
    libc = ctypes.CDLL(None, use_errno=True)
    mib = (ctypes.c_int * 3)(1, 49, pid)  # CTL_KERN.KERN_PROCARGS2
    size = ctypes.c_size_t(0)
    if (
        libc.sysctl(mib, 3, None, ctypes.byref(size), None, 0)
        or not 4 < size.value <= 16_777_216
    ):
        raise PermissionError("cannot identify peer arguments")
    buf = ctypes.create_string_buffer(size.value)
    if libc.sysctl(mib, 3, buf, ctypes.byref(size), None, 0):
        raise PermissionError("cannot read peer arguments")
    raw = buf.raw[: size.value]
    (argc,) = struct.unpack_from("=i", raw)
    # Skip kernel exec_path and its NUL padding, then read exactly argc strings.
    offset = raw.find(b"\0", 4)
    if argc < 1 or offset == -1:
        raise PermissionError("invalid peer arguments")
    while offset < len(raw) and raw[offset] == 0:
        offset += 1
    args = raw[offset:].split(b"\0", argc)
    if len(args) != argc + 1:
        raise PermissionError("truncated peer arguments")
    return [os.fsdecode(arg) for arg in args[:argc]]


CHROME_EXE = Path("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
CHROME_REQUIREMENT = 'anchor apple generic and identifier "com.google.Chrome" and certificate leaf[subject.OU] = "EQHXZ8M8AV"'
ORIGIN = "chrome-extension://jabfhcolknbbdajilgpficniedhcfahc/"


def verify_chrome_host(sock: LocalSocket, state: Path) -> None:
    pid = kernel_pid(sock)
    parent = parent_pid(pid)
    host = Path(__file__).resolve().with_name("native_host.py")
    expected = [str(host), "--socket", str(state / "enrollment.sock"), ORIGIN]
    if (
        process_exe(pid) != process_exe(os.getpid())
        or process_argv(pid)[1:] != expected
        or process_exe(parent) != CHROME_EXE
    ):
        raise PermissionError("the enrolled Chrome native host is required")
    # An attacker can spawn another copy of the signed browser with a replacement
    # extension or profile directory. Enrollment is scoped to installed Chrome Stable.
    unsafe = (
        "--user-data-dir",
        "--load-extension",
        "--disable-extensions-except",
        "--remote-debugging-port",
        "--remote-debugging-pipe",
    )
    if any(arg.partition("=")[0] in unsafe for arg in process_argv(parent)[1:]):
        raise PermissionError("expected the existing Chrome profile store")
    # A numeric PID verifies the *running* guest code, not just the binary on disk.
    checked = subprocess.run(
        ["/usr/bin/codesign", "-v", "-R=" + CHROME_REQUIREMENT, str(parent)],
        timeout=5,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if checked.returncode or parent_pid(pid) != parent or kernel_pid(sock) != pid:
        raise PermissionError("Chrome code identity could not be verified")
