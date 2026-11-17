"""Publish the extension assets into the one stable directory Chrome loads.

#11363: Chrome stores the resolved path of an unpacked extension, so a
directory per content hash left profiles on old assets. ``state/extension`` is
now a real directory updated in place, and every old ``assets/<hash>``
directory becomes a symlink to it, so any stored path serves current assets.
"""
from __future__ import annotations

import fcntl
import hashlib
import json
import os
from pathlib import Path
import tempfile

from trusty_browser.enrollment import private_directory

HEX = set("0123456789abcdef")
# The worker reads this manifest key and reloads when the broker reports another one.
STAMP = "version_name"


def signature(directory: Path) -> str:
    result = hashlib.sha256()
    for f in sorted(directory.rglob("*")):
        if f.is_symlink():
            raise ValueError("Unexpected symlink inside extension assets")
        if f.is_file():
            result.update(str(f.relative_to(directory)).encode() + b"\0")
            result.update(hashlib.sha256(f.read_bytes()).digest())
    return result.hexdigest()


def stamp(source: Path) -> str:
    """The short asset signature written into the published manifest."""
    return signature(source)[:16]


def published(state: Path) -> str | None:
    """The asset stamp of the installed extension, ``None`` when there is none."""
    try:
        manifest = json.loads((state / "extension" / "manifest.json").read_text())
    except (OSError, ValueError):
        return None
    value = manifest.get(STAMP) if isinstance(manifest, dict) else None
    return value.rsplit("+", 1)[-1] if isinstance(value, str) and "+" in value else None


def hash_name(name: str) -> bool:
    return len(name) == 64 and set(name) <= HEX


def write_file(target: Path, data: bytes) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=target.parent, delete=False) as handle:
        handle.write(data)
    os.replace(handle.name, target)


def update(source: Path, target: Path) -> None:
    """Make ``target`` hold exactly ``source``'s files, manifest.json last.

    Why: Chrome reads the manifest first on reload, so it must never name a
    file that is not yet in place (#11363). What: each file is written to a
    temp file and moved over with ``os.replace``; stale files are removed.
    Test: `test_republish_reaches_a_profile_holding_an_old_hash_path`.
    """
    wanted = {
        f.relative_to(source): f
        for f in source.rglob("*")
        if f.is_file() and f.name != ".DS_Store"
    }
    manifest = Path("manifest.json")
    for rel in sorted(wanted, key=lambda p: p == manifest):
        data = wanted[rel].read_bytes()
        if rel == manifest:
            body = json.loads(data)
            body[STAMP] = f"{body['version']}+{stamp(source)}"
            data = (json.dumps(body, indent=2) + "\n").encode()
        dest = target / rel
        if dest.is_symlink() or not dest.is_file() or dest.read_bytes() != data:
            write_file(dest, data)
    for old in sorted(target.rglob("*"), reverse=True):
        rel = old.relative_to(target)
        if old.is_symlink() or (old.is_file() and rel not in wanted):
            old.unlink()
        elif old.is_dir() and not any(old.iterdir()):
            old.rmdir()


def migrate(state: Path, assets: Path) -> None:
    """Turn every ``assets/<hash>`` directory into a symlink to ``../extension``."""
    for entry in assets.iterdir():
        if not hash_name(entry.name) or entry.is_symlink():
            continue
        if not entry.is_dir():
            raise ValueError(f"Refusing to replace an unrelated path: {entry}")
        with tempfile.TemporaryDirectory(dir=assets) as scratch:
            link = Path(scratch) / "link"
            link.symlink_to(Path("..") / "extension")
            moved = Path(scratch) / "old"
            entry.rename(moved)
            os.replace(link, entry)


def extension_path(state: Path, source: Path | None = None) -> Path:
    """Publish the packaged extension into ``state/extension`` and return that path.

    Test: `test_stable_assets_are_complete_and_refuse_overwriting`,
    `test_republish_reaches_a_profile_holding_an_old_hash_path`.
    """
    private_directory(state)
    assets = state / "assets"
    private_directory(assets)
    source = source or Path(__file__).with_name("extension")
    target = state / "extension"
    with (assets / "install.lock").open("ab") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        for entry in assets.iterdir():
            if entry.is_symlink() and hash_name(entry.name):
                if entry.readlink() != Path("..") / "extension":
                    raise ValueError(f"Unexpected symlink at {entry}")
        if target.is_symlink():
            pointed = target.readlink()
            if pointed.parent != Path("assets") or not hash_name(pointed.name):
                raise ValueError(f"Refusing to replace an unrelated path: {target}")
            # One-time migration from the symlinked layout: a real directory.
            target.unlink()
        elif os.path.lexists(target) and not target.is_dir():
            raise ValueError(f"Refusing to replace an unrelated path: {target}")
        if not target.exists():
            with tempfile.TemporaryDirectory(dir=state) as scratch:
                fresh = Path(scratch) / "extension"
                fresh.mkdir(mode=0o700)
                update(source, fresh)
                fresh.rename(target)
        else:
            update(source, target)
        migrate(state, assets)
    return target
