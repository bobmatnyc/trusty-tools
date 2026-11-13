"""Install and run the per-user Trusty Browser service."""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import plistlib
import subprocess
import sys

from trusty_browser import install_host, service
from trusty_browser.assets import extension_path
from trusty_browser.native_host import private_directory

LABEL = "com.trusty.browser"


def service_args(state: Path) -> list[str]:
    return [sys.executable, str(Path(service.__file__).resolve()), "--state", str(state), "serve"]


def launch_configuration(state: Path) -> str:
    config = {
        "Label": LABEL, "ProgramArguments": service_args(state),
        "RunAtLoad": True, "KeepAlive": True, "Umask": 0o077,
        "StandardErrorPath": str(state / "service.stderr.log"),
        "StandardOutPath": str(state / "service.stdout.log"),
    }
    return plistlib.dumps(config).decode()


def launchctl(*args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["/bin/launchctl", *args], check=check, text=True, capture_output=True)


def installed_content(path: Path, content: str, mode: int) -> bool:
    if not os.path.lexists(path):
        return False
    if (path.is_symlink() or not path.is_file() or path.stat().st_uid != os.getuid()
            or path.stat().st_mode & 0o777 != mode or path.read_text() != content):
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
    if loaded.returncode == 0 and (not present or f"path = {plist}\n" not in loaded.stdout):
        raise ValueError(f"A different launchd job owns {LABEL}")
    owned = [p for p, content, mode in registrations if installed_content(p,content,mode)]
    if install:
        agents.mkdir(mode=0o700, parents=True, exist_ok=True)
        state.mkdir(mode=0o700, parents=True, exist_ok=True)
        private_directory(state)
        extension_path(state)
        install_host.install(state, manifests)
        install_host.write_owned(plist, configuration, 0o600)
        if loaded.returncode != 0:
            launchctl("bootstrap",domain,str(plist))
    else:
        if loaded.returncode == 0:
            launchctl("bootout",target)
        if present:
            plist.unlink()
        for path in owned:
            path.unlink()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", type=Path, default=Path.home()/".trusty"/"browser")
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("serve", help="Run the Python service in the foreground")
    sub.add_parser("revoke", help="Remove the named enrollment").add_argument("workflow")
    sub.add_parser("extension-path", help="Print the unpacked Chrome extension directory")
    sub.add_parser("install-host", help="Register both native messaging hosts for Chrome")
    sub.add_parser("launchd-plist", help="Print the launchd configuration without installing it")
    sub.add_parser("install-service", help="Start at login and register the native messaging hosts")
    sub.add_parser("uninstall-service", help="Stop the service and remove its launchd and host registrations")
    args = parser.parse_args()
    state = args.state.expanduser().resolve()
    try:
        if args.command == "extension-path":
            print(extension_path(state))
        elif args.command == "launchd-plist":
            print(launch_configuration(state),end="")
        elif args.command == "install-host":
            print(install_host.install(state, install_host.default_directory()))
        elif args.command in {"install-service","uninstall-service"}:
            manage(state, args.command == "install-service")
        elif args.command == "revoke":
            params=service_args(state)[:-1] + ["revoke",args.workflow]
            os.execv(sys.executable,params)
        else:
            os.execv(sys.executable,service_args(state))
    except (OSError, ValueError) as exc:
        parser.exit(1, f"{exc}\n")
    except subprocess.CalledProcessError as exc:
        parser.exit(1, f"launchctl failed: {exc.stderr.strip()}\n")
