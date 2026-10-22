#!/usr/bin/env python3
"""architect_export.py — stage the Architect's framework files, scrubbed (#9171).

Why: the Architect prototype checkout mixes framework files with instance data
  (owner names, accounts, hosts). A hand copy into python/trusty-architect/ is
  not reviewable; an allow-listed, rewritten and gated export is.
What: copies only the paths a manifest lists (default
  scripts/architect_export_manifest.txt) from --source into --staging, applies
  an optional out-of-tree rewrite table, then runs the shared leak table
  (scripts/architect_leak_patterns.txt, through check_content.leak_findings)
  plus a REQUIRED out-of-tree deny file over every staged file's content and
  path. Instance paths (local/, owner/, records/, inbox/, docs/briefs/, .env*,
  .mcp.json, ...), symlinks and `..` are refused even when listed. Nothing is
  written unless the gate is clean; --dry-run writes nothing at all.

  Manifest line: SRC_REL[<TAB>DEST_REL]; a SRC_REL ending in /** copies every
  file under that directory, DEST_REL is then a directory. `#` starts a comment.
  Rewrite file: FROM<TAB>TO, literal and case-sensitive, longest FROM first;
  every FROM also becomes a case-insensitive REWRITE_KEY leak rule.
  Deny file: the ARCHITECT_LEAK_DENY_FILE format, one name per line.

Exit: 0 staged and clean; 1 leak residue, named by class and file:line, never by
  the matched text; 2 bad configuration or a refused path. Nothing is staged on 1 or 2.
Test: scripts/tests/test_architect_export.py
"""

import argparse
import os
import re
import shutil
import sys
import tempfile
from pathlib import Path, PurePosixPath

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))
from check_content import Fail, leak_findings, load_leak_rules  # noqa: E402  (same directory)

PATTERNS = SCRIPTS / "architect_leak_patterns.txt"
MANIFEST = SCRIPTS / "architect_export_manifest.txt"
# Instance data, never framework: refused even when the manifest lists it (#9171).
DENY_TOP = {"local", "owner", "records", "inbox"}
DENY_PREFIX = ("docs/briefs/", ".claude/settings", ".claude/worktrees/")
DENY_PART = {".git", ".trusty-mpm", ".trusty-channels", "__pycache__", ".pytest_cache"}
DENY_NAME = re.compile(r"\.env.*|\.mcp\.json|.*\.bak|.*\.lock")
# A repo root holding either file is a trusty-tools checkout or worktree (#10161).
TT_MARKERS = ("crates/trusty-common/Cargo.toml", "scripts/architect_leak_patterns.txt")


class Refuse(Exception):
    """A configuration or path the tool will not act on (exit 2)."""


def forbidden(rel):
    """True when a source or staging path names instance data (case-insensitive)."""
    low = rel.lower()
    parts = low.split("/")
    return (
        parts[0] in DENY_TOP
        or any(low.startswith(p) or low == p.rstrip("/") for p in DENY_PREFIX)
        or bool(DENY_PART & set(parts))
        or any(DENY_NAME.fullmatch(p) for p in parts)
    )


def clean_rel(text, num):
    """The normalised relative path, or Refuse for `..`, an absolute path or a hard-denied path."""
    path = PurePosixPath(text)
    if not text or path.is_absolute() or ".." in path.parts or "\\" in text or path.as_posix() == ".":
        raise Refuse(f"manifest line {num}: path escapes the tree: {text!r}")
    rel = path.as_posix()
    if forbidden(rel):
        raise Refuse(f"manifest line {num}: forbidden path (instance data): {rel}")
    return rel


def parse_manifest(text):
    """`(line, src_rel, dest_rel, recursive)` rows; Refuse on a malformed, escaping or forbidden row."""
    rows = []
    for num, line in enumerate(text.splitlines(), 1):
        line = line.rstrip("\r")
        if not line.strip() or line.startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) > 2:
            raise Refuse(f"manifest line {num}: expected SRC_REL[<TAB>DEST_REL]")
        recursive = fields[0].endswith("/**")
        src = fields[0][:-3] if recursive else fields[0]
        rows.append((num, clean_rel(src, num), clean_rel(fields[1] if len(fields) == 2 else src, num), recursive))
    if not rows:
        raise Refuse("the manifest lists no paths")
    return rows


def no_symlink(source, rel, num):
    """`source/rel`, after refusing a symlink at any component of it."""
    path = source
    for part in rel.split("/"):
        path = path / part
        if path.is_symlink():
            raise Refuse(f"manifest line {num}: symlink refused: {rel}")
    return path


def expand(source, rows):
    """`(line, src_rel, dest_rel)` per file to stage; Refuse on anything not a plain listed file.

    Why: the manifest is an allow-list, so a listed path that is missing, a
      symlink, or a `/**` tree that reaches instance data must stop the export,
      never shrink it silently.
    What: walks each `/**` row without following links; every file found is
      re-checked against the hard deny list, at both its source and staging path.
    Test: `test_forbidden_path_reached_by_glob`, `test_symlink_refused`,
      `test_missing_listed_source_file_refused`.
    """
    out = []
    for num, src, dest, recursive in rows:
        path = no_symlink(source, src, num)
        if not recursive:
            if not path.is_file():
                raise Refuse(f"manifest line {num}: {src} is not a regular file in the source")
            out.append((num, src, dest))
            continue
        if not path.is_dir():
            raise Refuse(f"manifest line {num}: {src}/** is not a directory in the source")
        found = []
        for dirpath, dirs, files in os.walk(path):
            dirs.sort()
            for name in dirs + files:
                full = Path(dirpath) / name
                inner = full.relative_to(path).as_posix()
                if full.is_symlink():
                    raise Refuse(f"manifest line {num}: symlink refused: {src}/{inner}")
                if name in files:
                    if not full.is_file():
                        raise Refuse(f"manifest line {num}: {src}/{inner} is not a regular file")
                    found.append((num, clean_rel(f"{src}/{inner}", num), clean_rel(f"{dest}/{inner}", num)))
        if not found:
            raise Refuse(f"manifest line {num}: {src}/** holds no files")
        out.extend(sorted(found))
    dests = [d for _, _, d in out]
    if len(set(dests)) != len(dests):
        raise Refuse("two manifest rows stage the same path")
    return out


def repo_roots(path):
    """Each ancestor of the resolved `path`, itself included, holding a `.git` dir or a `.git` file (worktree)."""
    real = path.resolve()
    return [a for a in (real, *real.parents) if os.path.lexists(a / ".git")]


def out_of_tree(value, label, flag, env, required):
    """The resolved path of an out-of-tree input file, or None when optional and unset."""
    if not value:
        if required:
            raise Refuse(f"{label} is required (--{flag} or {env}); it is never skipped")
        return None
    path = Path(value)
    if not path.is_file():
        raise Refuse(f"{label} missing: {value}")
    # #10161: any trusty-tools checkout or worktree, not only this one. Another repo (the source) is fine.
    if any((root / m).is_file() for root in repo_roots(path) for m in TT_MARKERS):
        raise Refuse(f"{label} {value} is inside a trusty-tools checkout or worktree; keep it out of tree")
    return path


def load_rewrites(path):
    """`{FROM: TO}` from the rewrite file; Refuse on an empty file or a malformed row. Never prints a value."""
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as err:
        raise Refuse(f"rewrite file unreadable: {path}: {type(err).__name__}") from None
    pairs = {}
    for num, line in enumerate(text.splitlines(), 1):
        line = line.rstrip("\r")
        if not line.strip() or line.startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) != 2 or not fields[0] or fields[0] in pairs:
            raise Refuse(f"{path}:{num}: expected FROM<TAB>TO with a new, non-empty FROM")
        pairs[fields[0]] = fields[1]
    if not pairs:
        raise Refuse(f"rewrite file {path} holds no rows")
    return pairs


def check_staging(source, staging):
    """Refuse a staging dir that is non-empty, overlaps the source, or sits in any git repo or worktree."""
    src, dst = source.resolve(), staging.resolve()
    if dst == src or src in dst.parents or dst in src.parents:
        raise Refuse(f"staging {staging} is inside the source, or holds it")
    if repo_roots(staging):  # See #10161
        raise Refuse(f"staging {staging} is inside a git repository or worktree; stage out of tree, then copy")
    if staging.exists() and (not staging.is_dir() or any(staging.iterdir())):
        raise Refuse(f"staging {staging} must be absent or an empty directory")


def stage(args):
    """Validate, rewrite and gate in memory; write only a clean result. Returns the exit code."""
    source, staging = Path(args.source), Path(args.staging)
    if not source.is_dir():
        raise Refuse(f"source {source} is not a directory")
    check_staging(source, staging)
    deny = out_of_tree(args.deny_file, "deny file", "deny-file", "ARCHITECT_LEAK_DENY_FILE", True)
    rw_path = out_of_tree(args.rewrite_file, "rewrite file", "rewrite-file", "ARCHITECT_EXPORT_REWRITE_FILE", False)
    pairs = load_rewrites(rw_path) if rw_path else {}
    manifest = Path(args.manifest)
    try:
        text = manifest.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as err:
        raise Refuse(f"manifest unreadable: {manifest}: {type(err).__name__}") from None
    files = expand(source, parse_manifest(text))
    try:
        rules = load_leak_rules(PATTERNS, deny=deny)
    except Fail as err:
        raise Refuse(str(err)) from None
    if pairs:
        keys = sorted(pairs, key=len, reverse=True)  # longest first: `/Users/x` before `x`
        rx = re.compile("|".join(re.escape(k) for k in keys))
        alt = "|".join(re.escape(k) for k in keys)
        rules.append(("REWRITE_KEY", re.compile(r"(^|[^A-Za-z0-9_])(" + alt + r")([^A-Za-z0-9_]|$)", re.I), None))
    staged = []
    for num, src, dest in files:
        path = source / src
        data, count = path.read_bytes(), 0
        if pairs:
            try:
                new, count = rx.subn(lambda m: pairs[m.group(0)], data.decode("utf-8"))
                data = new.encode("utf-8")
            except UnicodeDecodeError:
                pass  # not UTF-8: copied verbatim; the gate scans it as bytes
        staged.append((num, dest, data, count, path.stat().st_mode & 0o777))
    # Paths first: a file whose staging path carries a leak is never named.
    path_hits = leak_findings([(n, d.encode("utf-8")) for n, d, _, _, _ in staged], rules)
    withheld = {n for _, n, _, _ in path_hits}
    shown = {d: (f"<path of manifest line {n}>" if n in withheld else d) for n, d, _, _, _ in staged}
    residue = {f"RESIDUE {c}: path of manifest line {n}" for c, n, _, _ in path_hits}
    residue |= {f"RESIDUE {c}: {shown[d]}:{line}" for c, d, line, _ in leak_findings([(d, b) for _, d, b, _, _ in staged], rules)}
    if args.dry_run:
        for _, dest, _, count, _ in staged:
            print(f"would stage {shown[dest]} ({count} rewrite(s))")
    if residue:
        for line in sorted(residue):
            print(line, file=sys.stderr)
        print(f"architect_export: {len(residue)} leak residue hit(s); nothing staged", file=sys.stderr)
        return 1
    if args.dry_run:
        print(f"architect_export: dry run, {len(staged)} file(s), {sum(s[3] for s in staged)} rewrite(s), gate clean; nothing written")
        return 0
    # #10161: build a sibling temp tree and rename it into place, so a failed copy leaves no partial export.
    staging.parent.mkdir(parents=True, exist_ok=True)
    tmp = Path(tempfile.mkdtemp(prefix=f".{staging.name}.partial-", dir=staging.parent))
    try:
        for _, dest, data, _, mode in staged:
            out = tmp / dest
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_bytes(data)
            os.chmod(out, mode)
        if staging.exists():
            staging.rmdir()  # check_staging proved it empty
        tmp.rename(staging)
    except BaseException:
        shutil.rmtree(tmp, ignore_errors=True)
        raise
    for _, dest, _, count, _ in staged:
        print(f"staged {dest} ({count} rewrite(s))")
    print(f"architect_export: staged {len(staged)} file(s) into {staging}, gate clean")
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--source", required=True, help="the Architect checkout to export from (read only)")
    parser.add_argument("--staging", required=True, help="an absent or empty directory outside every git repository")
    parser.add_argument("--manifest", default=str(MANIFEST), help="allow-list (default: the committed starter)")
    parser.add_argument("--deny-file", default=os.environ.get("ARCHITECT_LEAK_DENY_FILE", ""))
    parser.add_argument("--rewrite-file", default=os.environ.get("ARCHITECT_EXPORT_REWRITE_FILE", ""))
    parser.add_argument("--dry-run", action="store_true", help="print what would be staged; write nothing")
    args = parser.parse_args(argv)
    try:
        return stage(args)
    except Refuse as err:
        for line in str(err).splitlines():
            print(f"FAIL {line}", file=sys.stderr)
        return 2
    except OSError as err:
        print(f"FAIL I/O error: {type(err).__name__}: {err.filename}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
