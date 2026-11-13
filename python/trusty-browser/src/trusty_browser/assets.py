"""Publish immutable extension assets through a stable path used by Chrome."""
from __future__ import annotations

import fcntl
import hashlib
import os
from pathlib import Path
import shutil
import tempfile

from trusty_browser.enrollment import private_directory


def signature(directory: Path) -> str:
    result = hashlib.sha256()
    for f in sorted(directory.rglob("*")):
        if f.is_symlink():
            raise ValueError("Unexpected symlink inside extension assets")
        if f.is_file():
            result.update(str(f.relative_to(directory)).encode() + b"\0")
            result.update(hashlib.sha256(f.read_bytes()).digest())
    return result.hexdigest()


def extension_path(state: Path) -> Path:
    private_directory(state)
    assets = state / "assets"
    private_directory(assets)
    source = Path(__file__).with_name("extension")
    target = assets / signature(source)
    link = state / "extension"
    with (assets / "install.lock").open("ab") as lock:
        fcntl.flock(lock,fcntl.LOCK_EX)
        if target.is_symlink():
            raise ValueError("Unexpected symlink at the extension installation")
        if target.exists():
            if not target.is_dir() or signature(target) != target.name:
                raise ValueError("Installed extension assets differ from this package")
        else:
            with tempfile.TemporaryDirectory(dir=assets) as scratch:
                copy = Path(scratch) / "extension"
                shutil.copytree(source,copy)
                if signature(copy) != target.name:
                    raise ValueError("Extension changed during installation")
                copy.rename(target)
        if os.path.lexists(link):
            if (not link.is_symlink() or link.readlink().parent != Path("assets")
                    or len(link.readlink().name) != 64
                    or any(ch not in "0123456789abcdef" for ch in link.readlink().name)):
                raise ValueError(f"Refusing to replace an unrelated path: {link}")
            if link.readlink() == target.relative_to(state):
                return link
        with tempfile.TemporaryDirectory(dir=state) as scratch:
            temporary = Path(scratch) / "extension"
            temporary.symlink_to(target.relative_to(state))
            os.replace(temporary,link)
    return link
