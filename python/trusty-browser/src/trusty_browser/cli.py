"""Install and run the per-user Trusty Browser service."""

from __future__ import annotations

import argparse
import json
import os
import plistlib
import re
import socket
import struct
import subprocess
import sys
from pathlib import Path

from trusty_browser import install_host, service
from trusty_browser.assets import extension_path
from trusty_browser.native_host import private_directory

LABEL = "com.trusty.browser"


def service_args(state: Path) -> list[str]:
    return [
        sys.executable,
        str(Path(service.__file__).resolve()),
        "--state",
        str(state),
        "serve",
    ]


def launch_configuration(state: Path) -> str:
    config = {
        "Label": LABEL,
        "ProgramArguments": service_args(state),
        "RunAtLoad": True,
        "KeepAlive": True,
        "Umask": 0o077,
        "StandardErrorPath": str(state / "service.stderr.log"),
        "StandardOutPath": str(state / "service.stdout.log"),
    }
    return plistlib.dumps(config).decode()


def launchctl(*args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["/bin/launchctl", *args], check=check, text=True, capture_output=True
    )


def installed_content(path: Path, content: str, mode: int) -> bool:
    if not os.path.lexists(path):
        return False
    if (
        path.is_symlink()
        or not path.is_file()
        or path.stat().st_uid != os.getuid()
        or path.stat().st_mode & 0o777 != mode
        or path.read_text() != content
    ):
        raise ValueError(f"Refusing to change an unrelated registration: {path}")
    return True


def manage(state: Path, install: bool) -> None:
    if sys.platform != "darwin":
        raise ValueError("launchd service installation requires macOS")
    domain = f"gui/{os.getuid()}"
    target = f"{domain}/{LABEL}"
    agents = Path.home() / "Library/LaunchAgents"
    plist = agents / f"{LABEL}.plist"
    manifests = install_host.default_directory()
    registrations = install_host.registration_files(state, manifests)
    configuration = launch_configuration(state)
    present = installed_content(plist, configuration, 0o600)
    loaded = launchctl("print", target, check=False)
    if loaded.returncode == 0 and (
        not present or f"path = {plist}\n" not in loaded.stdout
    ):
        raise ValueError(f"A different launchd job owns {LABEL}")
    owned = [
        p for p, content, mode in registrations if installed_content(p, content, mode)
    ]
    if install:
        agents.mkdir(mode=0o700, parents=True, exist_ok=True)
        state.mkdir(mode=0o700, parents=True, exist_ok=True)
        private_directory(state)
        extension_path(state)
        install_host.install(state, manifests)
        install_host.write_owned(plist, configuration, 0o600)
        if loaded.returncode != 0:
            launchctl("bootstrap", domain, str(plist))
    else:
        if loaded.returncode == 0:
            launchctl("bootout", target)
        if present:
            plist.unlink()
        for path in owned:
            path.unlink()


# #11239: operator verbs for activation codes and pairings; contract "Operator ops".
GRANT_ID = re.compile(r"g-[0-9a-f]{12}")
PROFILE_ID = re.compile(
    r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}"
)
HEADER = struct.Struct("=I")


def operator_call(state: Path, frame: dict[str, str]) -> dict[str, object]:
    """Send one frame on operator.sock and return the reply.

    Test: `test_operator_verbs_validate_input_and_print_the_reply`.
    """
    body = json.dumps(frame).encode()
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as wire:
            wire.settimeout(15)
            wire.connect(str(state / "operator.sock"))
            wire.sendall(HEADER.pack(len(body)) + body)
            with wire.makefile("rb") as stream:
                (size,) = HEADER.unpack(stream.read(HEADER.size))
                reply = json.loads(stream.read(size))
    except (OSError, struct.error, ValueError) as exc:
        raise ValueError(
            "The Trusty Browser service is not reachable on operator.sock"
        ) from exc
    if not isinstance(reply, dict):  # main() reports ValueError as an exit-1 message
        raise ValueError("Invalid reply from the Trusty Browser service")  # noqa: TRY004
    return reply


def operator_frame(args: argparse.Namespace) -> dict[str, str] | None:
    """The operator frame for an operator verb, ``None`` for any other verb."""
    verb = args.command
    if verb in ("grants", "revoke-grants", "pairings"):
        return {"op": verb.replace("-", "_")}
    if verb == "revoke-grant":
        if not GRANT_ID.fullmatch(args.grant):
            raise ValueError(
                "A grant id is g- followed by 12 lower-case hex characters"
            )
        return {"op": "revoke_grant", "grant": args.grant}
    if verb == "unpair":
        if not PROFILE_ID.fullmatch(args.profile):
            raise ValueError("A profile id is a lower-case UUID v4")
        return {"op": "unpair", "profile": args.profile}
    return None


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--state", type=Path, default=Path.home() / ".trusty" / "browser"
    )
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("serve", help="Run the Python service in the foreground")
    sub.add_parser("revoke", help="Remove the named enrollment").add_argument(
        "workflow"
    )
    sub.add_parser(
        "extension-path", help="Print the unpacked Chrome extension directory"
    )
    sub.add_parser(
        "install-host", help="Register both native messaging hosts for Chrome"
    )
    sub.add_parser(
        "launchd-plist", help="Print the launchd configuration without installing it"
    )
    sub.add_parser(
        "install-service", help="Start at login and register the native messaging hosts"
    )
    sub.add_parser(
        "uninstall-service",
        help="Stop the service and remove its launchd and host registrations",
    )
    sub.add_parser("grants", help="List unredeemed activation codes and live grants")
    sub.add_parser("revoke-grant", help="End one code or grant by id").add_argument(
        "grant"
    )
    sub.add_parser("revoke-grants", help="End every code and grant")
    sub.add_parser("pairings", help="List paired Chrome profiles")
    sub.add_parser(
        "unpair", help="Remove a profile's pairing and end its grants"
    ).add_argument("profile")
    args = parser.parse_args()
    state = args.state.expanduser().resolve()
    try:
        frame = operator_frame(args)
        if frame is not None:
            reply = operator_call(state, frame)
            print(json.dumps(reply, separators=(",", ":")))
            if reply.get("ok") is not True:
                parser.exit(1)
            return
        if args.command == "extension-path":
            print(extension_path(state))
        elif args.command == "launchd-plist":
            print(launch_configuration(state), end="")
        elif args.command == "install-host":
            print(install_host.install(state, install_host.default_directory()))
        elif args.command in {"install-service", "uninstall-service"}:
            manage(state, args.command == "install-service")
        elif args.command == "revoke":
            params = service_args(state)[:-1] + ["revoke", args.workflow]
            os.execv(sys.executable, params)
        else:
            os.execv(sys.executable, service_args(state))
    except (OSError, ValueError) as exc:
        parser.exit(1, f"{exc}\n")
    except subprocess.CalledProcessError as exc:
        parser.exit(1, f"launchctl failed: {exc.stderr.strip()}\n")
