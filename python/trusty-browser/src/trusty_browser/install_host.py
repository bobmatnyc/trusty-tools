"""Register the Chrome native host for this user without replacing an unrelated registration."""

import argparse
import json
import os
from pathlib import Path
import shlex
import sys
import tempfile

from trusty_browser.native_host import ORIGIN, private_directory

NAME = "com.trusty.browser"


def write_owned(path: Path, content: str, mode: int) -> None:
    """Publish a complete file once. An identical, owner-controlled install is idempotent."""
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as handle:
        temporary = Path(handle.name)
        try:
            os.fchmod(handle.fileno(), mode)
            handle.write(content.encode())
            handle.flush()
            try:
                os.link(temporary, path)
            except FileExistsError:
                if (path.is_symlink() or not path.is_file() or path.stat().st_uid != os.getuid()
                        or path.stat().st_mode & 0o777 != mode or path.read_text() != content):
                    raise FileExistsError(f"existing file differs: {path}") from None
        finally:
            temporary.unlink(missing_ok=True)


def install(state: Path, manifest_dir: Path) -> Path:
    state = state.expanduser().absolute()
    state.mkdir(parents=True, mode=0o700, exist_ok=True)
    private_directory(state)
    manifest_dir = manifest_dir.expanduser()
    manifest_dir.mkdir(parents=True, mode=0o700, exist_ok=True)
    for path, content, mode in registration_files(state, manifest_dir):
        write_owned(path, content, mode)
    return manifest_dir / f"{NAME}.json"


def registration_files(state: Path, manifest_dir: Path) -> list[tuple[Path, str, int]]:
    entries: list[tuple[Path, str, int]] = []
    host = Path(__file__).resolve().with_name("native_host.py")
    for suffix, socket_name in (("", "service.sock"), (".enroll", "enrollment.sock")):
        launcher = state / ("chrome-native-host" + suffix)
        command = shlex.join([sys.executable, str(host), "--socket", str(state / socket_name)])
        entries.append((launcher, '#!/bin/sh\nexec ' + command + ' "$@"\n', 0o700))
        name = NAME + suffix
        manifest: dict[str, object] = {"name": name, "description": "Trusty Browser native host",
                                       "type": "stdio", "path": str(launcher), "allowed_origins": [ORIGIN]}
        path = manifest_dir / f"{name}.json"
        entries.append((path, json.dumps(manifest, indent=2) + "\n", 0o600))
    return entries


def default_directory() -> Path:
    if sys.platform == "darwin":
        return Path.home() / "Library/Application Support/Google/Chrome/NativeMessagingHosts"
    if sys.platform.startswith("linux"):
        return Path.home() / ".config/google-chrome/NativeMessagingHosts"
    raise RuntimeError("a Unix operating system is required")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-dir", type=Path, required=True)
    parser.add_argument("--manifest-dir", type=Path, default=default_directory())
    args = parser.parse_args()
    print(install(args.state_dir, args.manifest_dir))


if __name__ == "__main__":
    main()
