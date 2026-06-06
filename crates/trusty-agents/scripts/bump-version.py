#!/usr/bin/env python3
"""Bump semantic version in both Cargo.toml files.

Usage: python3 scripts/bump-version.py [patch|minor|major]
"""

import re
import sys

CARGO_FILES = [
    "Cargo.toml",
    "ui/src-tauri/Cargo.toml",
]


def bump(bump_type: str) -> None:
    # Read root Cargo.toml to determine current version
    root_content = open("Cargo.toml").read()
    ver = re.search(r'^version = "(\d+)\.(\d+)\.(\d+)"', root_content, re.M)
    if not ver:
        print("ERROR: could not find version in Cargo.toml", file=sys.stderr)
        sys.exit(1)

    major, minor, patch = int(ver.group(1)), int(ver.group(2)), int(ver.group(3))

    if bump_type == "patch":
        patch += 1
    elif bump_type == "minor":
        minor += 1
        patch = 0
    elif bump_type == "major":
        major += 1
        minor = 0
        patch = 0
    else:
        print(
            f"ERROR: unknown bump type '{bump_type}' (use patch/minor/major)",
            file=sys.stderr,
        )
        sys.exit(1)

    new_ver = f"{major}.{minor}.{patch}"

    for path in CARGO_FILES:
        content = open(path).read()
        updated = re.sub(
            r'^version = "\d+\.\d+\.\d+"',
            f'version = "{new_ver}"',
            content,
            count=1,
            flags=re.M,
        )
        if updated == content:
            print(f"  WARNING: no version line found in {path}", file=sys.stderr)
        else:
            open(path, "w").write(updated)
            print(f"  {path}: bumped to {new_ver}")

    print(f"{bump_type.capitalize()} bumped to {new_ver}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(
            "Usage: python3 scripts/bump-version.py [patch|minor|major]",
            file=sys.stderr,
        )
        sys.exit(1)
    bump(sys.argv[1])
