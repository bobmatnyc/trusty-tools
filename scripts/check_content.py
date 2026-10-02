#!/usr/bin/env python3
"""check_content.py — the content/ manifest, versioning and bump gates (#8388).

Why: ADR-0064 versions instructional content apart from every crate. A content
  version means something only if three facts are enforced: each member asset
  declares its own version, content/manifest.toml lists those versions under
  a bundle version that is never lower than any of them, and a change to a
  member lands under a bundle version no content-v* release has used yet.
  Nothing checked any of that before this script.

What: three subcommands, each failing closed with a named reason.

  tree [--expect-version X.Y.Z]
      Validates content/ against content/manifest.toml:
      - the manifest exists, parses, and has exactly the keys below;
      - [bundle].version is SemVer X.Y.Z[-pre]; [bundle].schema_major equals
        SCHEMA_MAJOR in scripts/package_content.sh;
      - content/ holds only instructions/, agents/, skills/, changelog.d/,
        manifest.toml and CONTENT-CHANGELOG.md (owner ruling 2026-10-01),
        no symlink or other non-regular file, and no path outside
        [A-Za-z0-9._/-]+ (git would quote it past the bump and changelog gates);
      - every agent (content/agents/**/*.md) and skill entry point
        (content/skills/<name>.md, content/skills/<name>/SKILL.md) declares
        `metadata: {version: "x.y.z"}`, block or flow form; any other .md
        under the three classes may opt in by declaring one;
      - no member declares a top-level `version:` (it collides with the
        claude-mpm marker in agent_schema.rs; owner ruling 2026-10-01);
      - the [[member]] tables list exactly those files with those versions;
      - the bundle version is not below any member version.
      --expect-version also requires [bundle].version to equal X.Y.Z; the
      content-release workflow passes the version it is about to tag.
  bump [--base REF]
      The PR gate. Diffs the merge base with REF (default $CONTENT_GATE_BASE,
      then origin/main) against HEAD and fails when the bundle version went
      down, or when a member file changed while the bundle version at HEAD is
      already a content-v<version> tag. Like check-pr-version-bump.sh, a
      version no release has used yet may collect several PRs' changes.
  sync
      Rewrites content/manifest.toml's [[member]] tables from the tree, keeping
      [bundle] as it is. Refuses when any required member lacks a version.

Exit: 0 pass; 1 a check failed; 2 bad usage or no Python 3.11+.
Test: scripts/check_content_selftest.sh
"""

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # Python < 3.11 has no stdlib TOML reader.
    print("check_content: needs Python 3.11+ (stdlib tomllib)", file=sys.stderr)
    sys.exit(2)

REPO_ROOT = Path(__file__).resolve().parent.parent
MANIFEST_REL = "content/manifest.toml"
PACKAGER_REL = "scripts/package_content.sh"
CLASSES = ("instructions", "agents", "skills")
TOP_LEVEL = set(CLASSES) | {"changelog.d", "manifest.toml", "CONTENT-CHANGELOG.md"}
SEMVER_RE = re.compile(
    r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z]+(?:\.[0-9A-Za-z]+)*))?",
    re.ASCII,
)
# A YAML key, bare or quoted: `version:`, `"version":` and `'version':` are one key.
KEY_RE = re.compile(r"(?:\"([^\"]*)\"|'([^']*)'|([A-Za-z0-9_.-]+))\s*:(.*)")
# #8388: git quotes any other byte in a path, and a quoted path slips past
# every `content/...` prefix match in the bump and changelog gates.
SAFE_PATH_RE = re.compile(r"[A-Za-z0-9._/-]+", re.ASCII)
HEADER = """\
# Instructional-content manifest (ADR-0064, #8388).
#
# [bundle].version is the content release this tree builds toward; a release
# is tagged content-v<version>. [bundle].schema_major must equal SCHEMA_MAJOR
# in scripts/package_content.sh. Every agent, skill entry point, or other
# content/ file that carries `metadata: {version: "x.y.z"}` frontmatter has
# one [[member]] table, and the bundle version is never below a member's.
#
# Check:      python3 scripts/check_content.py tree
# Regenerate: python3 scripts/check_content.py sync
# How it is gated: docs/reference/content-release.md
"""


class Fail(Exception):
    """A check that cannot pass; the message names the file and the fix."""


def parse_semver(text):
    """`(major, minor, patch, pre-or-None)` for a SemVer string, else None."""
    m = SEMVER_RE.fullmatch(text) if isinstance(text, str) else None
    if not m:
        return None
    pre = tuple(m.group(4).split(".")) if m.group(4) else None
    return (int(m.group(1)), int(m.group(2)), int(m.group(3)), pre)


def cmp_semver(a, b):
    """SemVer 2.0 precedence: -1, 0 or 1."""
    if a[:3] != b[:3]:
        return -1 if a[:3] < b[:3] else 1
    pa, pb = a[3], b[3]
    if pa == pb:
        return 0
    if pa is None or pb is None:
        return 1 if pa is None else -1  # a release outranks its pre-releases
    for x, y in zip(pa, pb):
        if x == y:
            continue
        if x.isdigit() and y.isdigit():
            return -1 if int(x) < int(y) else 1
        if x.isdigit() or y.isdigit():
            return -1 if x.isdigit() else 1
        return -1 if x < y else 1
    return (len(pa) > len(pb)) - (len(pa) < len(pb))


def unquote(value):
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        return value[1:-1]
    return value.split(" #", 1)[0].strip()


def key_value(line):
    """`(key, raw_value)` for a `key: value` line, the key unquoted; else None."""
    m = KEY_RE.fullmatch(line)
    if not m:
        return None
    key = next(g for g in m.group(1, 2, 3) if g is not None)
    return key, m.group(4).strip()


def frontmatter_versions(text, rel):
    """Return `(top_level_version, metadata_version)` from a file's frontmatter.

    Reads the YAML subset the content tree uses: top-level `key: value` lines,
    and `metadata:` either as a flow map on one line or as a block whose
    children share one indent. A file with no frontmatter returns (None, None).
    """
    lines = text.lstrip("﻿").splitlines()
    if not lines or lines[0].strip() != "---":
        return None, None
    try:
        end = next(i for i in range(1, len(lines)) if lines[i].strip() == "---")
    except StopIteration:
        raise Fail(f"{rel}: unterminated frontmatter (no closing `---`)") from None
    top = meta = None
    child_indent = None
    in_meta = False
    for line in lines[1:end]:
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        indent = len(line) - len(line.lstrip(" \t"))
        if indent == 0:
            in_meta = False
            kv = key_value(line.rstrip())
            if not kv:
                continue
            key, value = kv
            if key == "version":
                top = unquote(value)
            elif key == "metadata" and not value:
                in_meta, child_indent = True, None
            elif key == "metadata":
                flow = re.fullmatch(r"\{(.*)\}", value)
                if not flow:
                    raise Fail(f"{rel}: `metadata:` must be a map, got `{value}`")
                for item in flow.group(1).split(","):
                    k, _, v = item.partition(":")
                    if unquote(k) == "version":
                        meta = unquote(v)
        elif in_meta:
            if child_indent is None:
                child_indent = indent
            kv = key_value(line.strip())
            if indent == child_indent and kv and kv[0] == "version":
                meta = unquote(kv[1])
    return top, meta


def load_manifest(text, origin):
    """Parse manifest text into `(version_str, schema_major, {path: version})`."""
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as err:
        raise Fail(f"{origin}: not valid TOML: {err}") from None
    extra = set(data) - {"bundle", "member"}
    if extra:
        raise Fail(f"{origin}: unknown top-level key(s): {', '.join(sorted(extra))}")
    bundle = data.get("bundle")
    if not isinstance(bundle, dict):
        raise Fail(f"{origin}: no [bundle] table")
    extra = set(bundle) - {"version", "schema_major"}
    if extra:
        raise Fail(f"{origin}: unknown [bundle] key(s): {', '.join(sorted(extra))}")
    version = bundle.get("version")
    if parse_semver(version) is None:
        raise Fail(f"{origin}: [bundle].version must be SemVer X.Y.Z[-pre], got {version!r}")
    schema = bundle.get("schema_major")
    if not isinstance(schema, int) or isinstance(schema, bool) or schema < 1:
        raise Fail(f"{origin}: [bundle].schema_major must be a positive integer, got {schema!r}")
    members = {}
    raw = data.get("member", [])
    if not isinstance(raw, list):
        raise Fail(f"{origin}: `member` must be an array of [[member]] tables")
    for entry in raw:
        if not isinstance(entry, dict) or set(entry) != {"path", "version"}:
            raise Fail(f"{origin}: each [[member]] holds exactly `path` and `version`, got {entry!r}")
        path, ver = entry["path"], entry["version"]
        if not isinstance(path, str) or path in members:
            raise Fail(f"{origin}: [[member]] path {path!r} is not a string or is listed twice")
        if parse_semver(ver) is None:
            raise Fail(f"{origin}: [[member]] {path} version must be SemVer, got {ver!r}")
        members[path] = ver
    return version, schema, members


def required_member(rel):
    """True for an agent or a skill entry point, which must carry a version."""
    parts = rel.split("/")
    if parts[0] == "agents":
        return rel.endswith(".md")
    if parts[0] == "skills":
        return (len(parts) == 2 and rel.endswith(".md")) or (len(parts) == 3 and parts[2] == "SKILL.md")
    return False


def scan_tree(root):
    """Walk content/ and return `{path-relative-to-content: version}`."""
    content = root / "content"
    if not content.is_dir() or content.is_symlink():
        raise Fail("content/ is missing; ADR-0064 keeps the content tree at the repo root")
    errors, members = [], {}
    for entry in sorted(os.listdir(content)):
        if entry.startswith("."):
            continue
        if entry not in TOP_LEVEL:
            errors.append(f"content/{entry}: outside the content IA (instructions/, agents/, skills/ only)")
    # #8388: every path under content/, not only members, so no quoted path reaches a gate.
    for dirpath, dirs, files in os.walk(content):
        for name in sorted(dirs) + sorted(files):
            rel = (Path(dirpath) / name).relative_to(content).as_posix()
            if not SAFE_PATH_RE.fullmatch(rel):
                errors.append(f"content/{rel!r}: path must match [A-Za-z0-9._/-]+; git quotes any other byte")
    for cls in CLASSES:
        base = content / cls
        if not base.exists() and not base.is_symlink():
            continue
        if base.is_symlink() or not base.is_dir():
            errors.append(f"content/{cls}: symlink or non-directory; the bundle holds regular files only")
            continue
        for dirpath, dirs, files in os.walk(base):
            dirs[:] = sorted(d for d in dirs if not d.startswith("."))
            for name in sorted(dirs) + sorted(files):
                path = Path(dirpath) / name
                rel = path.relative_to(content).as_posix()
                if path.is_symlink() or not (path.is_dir() or path.is_file()):
                    errors.append(f"content/{rel}: symlink or non-regular file; the bundle holds regular files only")
                    continue
                if name.startswith(".") or path.is_dir() or not name.endswith(".md"):
                    continue
                try:
                    top, meta = frontmatter_versions(path.read_text(encoding="utf-8"), f"content/{rel}")
                except (Fail, UnicodeDecodeError) as err:
                    errors.append(str(err))
                    continue
                if top is not None:
                    errors.append(f"content/{rel}: top-level `version:` is not used; declare `metadata: {{version: \"x.y.z\"}}`")
                if meta is None:
                    if required_member(rel):
                        errors.append(f"content/{rel}: missing `metadata: {{version: \"x.y.z\"}}` frontmatter")
                    continue
                if parse_semver(meta) is None:
                    errors.append(f"content/{rel}: metadata.version must be SemVer X.Y.Z[-pre], got {meta!r}")
                    continue
                members[rel] = meta
    if errors:
        raise Fail("\n".join(errors))
    return members


def packager_schema_major(root):
    try:
        text = (root / PACKAGER_REL).read_text(encoding="utf-8")
    except OSError as err:
        raise Fail(f"{PACKAGER_REL}: cannot read SCHEMA_MAJOR: {err}") from None
    m = re.search(r"^SCHEMA_MAJOR=([0-9]+)$", text, re.M)
    if not m:
        raise Fail(f"{PACKAGER_REL}: no `SCHEMA_MAJOR=<n>` line")
    return int(m.group(1))


def read_manifest(root):
    path = root / MANIFEST_REL
    if not path.is_file():
        raise Fail(f"{MANIFEST_REL} is missing; every content tree carries one")
    return load_manifest(path.read_text(encoding="utf-8"), MANIFEST_REL)


def cmd_tree(root, expect):
    version, schema, listed = read_manifest(root)
    errors = []
    packager = packager_schema_major(root)
    if schema != packager:
        errors.append(f"{MANIFEST_REL}: schema_major {schema} differs from {PACKAGER_REL} SCHEMA_MAJOR={packager}")
    if expect is not None and expect != version:
        errors.append(f"{MANIFEST_REL}: [bundle].version is {version}, but the release is {expect}; bump the manifest first")
    try:
        on_disk = scan_tree(root)
    except Fail as err:
        errors.extend(str(err).splitlines())
        on_disk = None
    if on_disk is not None:
        for rel in sorted(set(on_disk) - set(listed)):
            errors.append(f"content/{rel}: version {on_disk[rel]} is not listed in {MANIFEST_REL} (run: python3 scripts/check_content.py sync)")
        for rel in sorted(set(listed) - set(on_disk)):
            errors.append(f"{MANIFEST_REL}: lists {rel}, which is not a versioned file under content/")
        for rel in sorted(set(listed) & set(on_disk)):
            if listed[rel] != on_disk[rel]:
                errors.append(f"{MANIFEST_REL}: lists {rel} at {listed[rel]}, the file declares {on_disk[rel]}")
    bundle = parse_semver(version)
    for rel, ver in sorted(listed.items()):
        if cmp_semver(bundle, parse_semver(ver)) < 0:
            errors.append(f"{MANIFEST_REL}: bundle version {version} is lower than member {rel} at {ver}")
    if errors:
        raise Fail("\n".join(errors))
    print(f"check_content tree: bundle {version}, schema_major {schema}, {len(listed)} member(s) — OK")


def cmd_sync(root):
    version, schema, _ = read_manifest(root)
    members = scan_tree(root)
    out = [HEADER, "[bundle]", f'version = "{version}"', f"schema_major = {schema}"]
    for rel, ver in sorted(members.items()):
        out += ["", "[[member]]", f'path = "{rel}"', f'version = "{ver}"']
    (root / MANIFEST_REL).write_text("\n".join(out) + "\n", encoding="utf-8")
    print(f"check_content sync: wrote {len(members)} member(s) to {MANIFEST_REL}")


def git(root, *args):
    proc = subprocess.run(["git", *args], cwd=root, capture_output=True, text=True, encoding="utf-8", errors="surrogateescape")
    if proc.returncode != 0:
        raise Fail(f"git {' '.join(args)} failed: {proc.stderr.strip()}")
    return proc.stdout


def manifest_at(root, rev):
    """The manifest text at `rev`, or None when the path is absent there."""
    if not git(root, "ls-tree", "--name-only", rev, "--", MANIFEST_REL).strip():
        return None
    return git(root, "show", f"{rev}:{MANIFEST_REL}")


def pair_name_status(raw):
    """(status, path) pairs from `git diff -z --name-status --no-renames`.

    Why: an R or C record carries two paths, so two of them leave an even field
      count and a parity check pairs them wrongly without an error (#8388).
    What: pairs the fields, then requires every status to be exactly one of
      A, D, M, T; anything else raises Fail naming the status and path.
    Test: selftest case bump-pairing-rejects-rename-status.
    """
    fields = raw.split("\0")
    if fields and fields[-1] == "":
        fields.pop()
    pairs = list(zip(fields[0::2], fields[1::2]))
    for status, path in pairs:
        if not re.fullmatch(r"[ADMT]", status):
            raise Fail(f"git diff --name-status: unexpected status {status!r} for path {path!r}; the status/path pairing is not trustworthy")
    if len(pairs) * 2 != len(fields):
        raise Fail(f"git diff --name-status: odd field count {len(fields)}; cannot pair status and path")
    return pairs


def cmd_bump(root, base):
    try:
        merge_base = git(root, "merge-base", base, "HEAD").strip()
    except Fail as err:
        raise Fail(f"cannot find a merge base between '{base}' and HEAD ({err}); fetch the base ref") from None
    # #8388: -z, so git never quotes a path (`"content/agents/caf\303\251.md"`).
    diff = pair_name_status(git(root, "diff", "-z", "--name-status", "--no-renames", merge_base, "HEAD"))
    if not diff:
        raise Fail(f"SCAN FLOOR — the diff {merge_base[:10]}..HEAD lists 0 changed paths; nothing was examined")
    changed = sorted(p for _, p in diff if p.split("/")[0] == "content" and p.count("/") >= 2 and p.split("/")[1] in CLASSES)
    head_text = manifest_at(root, "HEAD")
    if head_text is None:
        raise Fail(f"{MANIFEST_REL} is missing at HEAD; the content gates need it")
    head_ver = load_manifest(head_text, f"HEAD:{MANIFEST_REL}")[0]
    base_text = manifest_at(root, merge_base)
    base_ver = None if base_text is None else load_manifest(base_text, f"{merge_base[:10]}:{MANIFEST_REL}")[0]
    if base_ver is not None and cmp_semver(parse_semver(head_ver), parse_semver(base_ver)) < 0:
        raise Fail(f"{MANIFEST_REL}: bundle version went down, {base_ver} -> {head_ver}")
    if not changed:
        print(f"check_content bump: {len(diff)} path(s), no content member changed; bundle {head_ver} — OK")
        return
    tags = set(git(root, "tag", "--list", "content-v*").split())
    if not tags:
        raise Fail("no content-v* tag in this checkout, so whether the bundle version shipped cannot be established (git fetch --tags origin)")
    if f"content-v{head_ver}" in tags:
        raise Fail(
            f"{len(changed)} content member path(s) changed, e.g. {changed[0]}, but bundle version {head_ver} "
            f"is already released as content-v{head_ver}; raise [bundle].version in {MANIFEST_REL}"
        )
    was = "introduced" if base_ver is None else f"was {base_ver}"
    print(f"check_content bump: {len(changed)} content member path(s) changed; bundle {head_ver} ({was}) is unreleased — OK")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="cmd", required=True)
    tree = sub.add_parser("tree")
    tree.add_argument("--expect-version")
    bump = sub.add_parser("bump")
    bump.add_argument("--base", default=os.environ.get("CONTENT_GATE_BASE") or "origin/main")
    sub.add_parser("sync")
    args = parser.parse_args()
    try:
        if args.cmd == "tree":
            cmd_tree(REPO_ROOT, args.expect_version)
        elif args.cmd == "bump":
            cmd_bump(REPO_ROOT, args.base)
        else:
            cmd_sync(REPO_ROOT)
    except Fail as err:
        for line in str(err).splitlines():
            print(f"FAIL {line}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
