#!/usr/bin/env bash
#
# package_content.sh — build the deterministic `content-vX.Y.Z` bundle that
# `.github/workflows/content-release.yml` publishes (ADR-0064, #8389).
#
# Why: ADR-0064 ships instructional content (agents, skills, PM instruction
#   sections, output styles) on its own release channel, and the runtime
#   resolver pins a bundle by tag + sha256. A pin is only meaningful when the
#   same tree always yields the same bytes, so the tarball must not carry
#   anything that varies between runs: file mtimes, walk order, the packer's
#   uid/gid, or the gzip header timestamp.
#
# What: collects every regular file under each content class's source
#   directory, then writes, into --out-dir:
#     content-v<version>.tar.gz          the bundle
#     content-v<version>.tar.gz.sha256   `<hex>  <name>`, `sha256sum -c` form
#   The tarball's first entry is `bundle-manifest.toml` (bundle version, tag,
#   content schema major, per-class source and file count); the class trees
#   follow as `<class>/<relative path>`, sorted by path. Every entry has
#   mtime 0, uid/gid 0 with empty owner names, and mode 0644 (files) or 0755
#   (directories); the gzip header carries mtime 0 and no file name.
#   Dot-files and dot-directories are skipped. A class whose directory is
#   missing, holds no files, or holds a symlink or other non-regular file
#   fails the run, and no bundle is written: an empty or partial bundle
#   would pin cleanly and deploy nothing (Fail-Open Check).
#
# Source layout: until PHASE_1/PR-D (#8387) moves the tree, content lives in
#   the in-crate asset directories listed in LEGACY_SOURCES below. PR-D flips
#   DEFAULT_SOURCE_ROOT to "content", after which every class is read from
#   content/<class>/ and LEGACY_SOURCES can be deleted.
#
# Usage:
#   bash scripts/package_content.sh --version 0.1.0
#   bash scripts/package_content.sh --version 0.1.0 --out-dir dist \
#     [--source-root <dir>]     # read <dir>/<class>/ instead of the table
#
# Exit: 0 bundle written; 1 a source directory is missing, empty, or holds a
#   non-regular file; 2 bad arguments.
#
# Test: scripts/package_content_selftest.sh
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); needs python3 (the
#   stdlib tarfile/gzip modules give the same bytes under BSD and GNU tar
#   hosts, which differ in the flags that pin these fields).

# #7812: re-run under bash when invoked as `zsh <this script>`.
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Bumped only when the bundle layout or manifest keys change incompatibly.
SCHEMA_MAJOR=1

# PR-D (#8387): set to "content" once the tree lives at content/<class>/.
DEFAULT_SOURCE_ROOT=""

# <class>=<source directory, relative to the repo root>, in manifest order.
LEGACY_SOURCES="
agents=crates/trusty-agents-common/src/assets/agents
skills=crates/trusty-mpm/src/assets/skills
instructions=crates/trusty-mpm/src/assets/instructions
output-styles=crates/trusty-mpm/src/assets/output-styles
sm_instructions=crates/trusty-mpm/src/assets/sm_instructions
harness_understanding=crates/trusty-agents-common/src/assets/harness_understanding
"

usage() {
  echo "usage: bash scripts/package_content.sh --version X.Y.Z [--out-dir DIR] [--source-root DIR]" >&2
  exit 2
}

VERSION=""
OUT_DIR="${REPO_ROOT}/target/content-bundle"
SOURCE_ROOT="${DEFAULT_SOURCE_ROOT}"
while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || usage; VERSION="$2"; shift 2 ;;
    --out-dir) [ $# -ge 2 ] || usage; OUT_DIR="$2"; shift 2 ;;
    --source-root) [ $# -ge 2 ] || usage; SOURCE_ROOT="$2"; shift 2 ;;
    -h|--help) usage ;;
    *) echo "package_content: unknown argument: $1" >&2; usage ;;
  esac
done

# SemVer 2.0 core plus an optional pre-release; no build metadata, since `+`
# is not a safe character in a git tag consumers will type.
if ! printf '%s' "$VERSION" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$'; then
  echo "package_content: --version must be SemVer X.Y.Z[-pre], got '${VERSION}'" >&2
  exit 2
fi

# Build the class list as <class>=<path> lines. A relative path resolves
# against the repo root, so the result does not depend on the caller's cwd.
CLASSES=""
for pair in $LEGACY_SOURCES; do
  class="${pair%%=*}"
  if [ -n "$SOURCE_ROOT" ]; then
    src="${SOURCE_ROOT%/}/${class}"
  else
    src="${pair#*=}"
  fi
  CLASSES="${CLASSES}${class}=${src}
"
done

mkdir -p "$OUT_DIR"
cd "$REPO_ROOT"

CLASSES="$CLASSES" python3 - "$VERSION" "$SCHEMA_MAJOR" "$OUT_DIR" <<'PY'
import gzip
import hashlib
import io
import json
import os
import stat
import sys
import tarfile

version, schema_major, out_dir = sys.argv[1], int(sys.argv[2]), sys.argv[3]
tag = f"content-v{version}"
name = f"{tag}.tar.gz"


def die(msg):
    print(f"package_content: {msg}; no bundle written", file=sys.stderr)
    sys.exit(1)


def collect(cls, src):
    """Return one class tree's (archive_path, abs_path, is_dir) entries and file count."""
    if not os.path.isdir(src) or os.path.islink(src):
        die(f"source directory for class '{cls}' is missing: {src}")
    entries = [(cls, src, True)]
    files = 0
    for root, dirs, names in os.walk(src):
        dirs[:] = [d for d in dirs if not d.startswith(".")]
        for d in dirs:
            path = os.path.join(root, d)
            if os.path.islink(path):
                die(f"symlink in class '{cls}': {path}")
            entries.append((f"{cls}/{os.path.relpath(path, src)}", path, True))
        for n in names:
            if n.startswith("."):
                continue
            path = os.path.join(root, n)
            if not stat.S_ISREG(os.lstat(path).st_mode):
                die(f"non-regular file in class '{cls}': {path}")
            entries.append((f"{cls}/{os.path.relpath(path, src)}", path, False))
            files += 1
    if files == 0:
        die(f"source directory for class '{cls}' holds no files: {src}")
    return entries, files


# A stale bundle from an earlier run must not survive a failed one.
for stale in (name, f"{name}.sha256"):
    if os.path.exists(os.path.join(out_dir, stale)):
        os.remove(os.path.join(out_dir, stale))

classes, entries = [], []
for line in os.environ["CLASSES"].splitlines():
    if not line:
        continue
    cls, src = line.split("=", 1)
    got, files = collect(cls, src)
    classes.append((cls, src, files))
    entries.extend(got)
entries.sort(key=lambda e: e[0].encode("utf-8"))

manifest = [
    "# Generated by scripts/package_content.sh (ADR-0064). Do not edit.",
    f'bundle_version = "{version}"',
    f'tag = "{tag}"',
    f"schema_major = {schema_major}",
    f"file_count = {sum(c[2] for c in classes)}",
]
for cls, src, files in classes:
    manifest += ["", "[[class]]", f'name = "{cls}"', f"source = {json.dumps(src)}", f"files = {files}"]
manifest_bytes = ("\n".join(manifest) + "\n").encode("utf-8")


def info(arcname, is_dir, size=0):
    ti = tarfile.TarInfo(arcname)
    ti.type = tarfile.DIRTYPE if is_dir else tarfile.REGTYPE
    ti.mode = 0o755 if is_dir else 0o644
    ti.size = size
    ti.mtime = 0
    ti.uid = ti.gid = 0
    ti.uname = ti.gname = ""
    return ti


tmp = os.path.join(out_dir, f".{name}.partial")
try:
    with open(tmp, "wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0, compresslevel=9) as gz:
            with tarfile.open(fileobj=gz, mode="w", format=tarfile.PAX_FORMAT) as tar:
                tar.addfile(info("bundle-manifest.toml", False, len(manifest_bytes)),
                            io.BytesIO(manifest_bytes))
                for arcname, path, is_dir in entries:
                    if is_dir:
                        tar.addfile(info(arcname, True))
                    else:
                        with open(path, "rb") as fh:
                            tar.addfile(info(arcname, False, os.path.getsize(path)), fh)
    with open(tmp, "rb") as fh:
        digest = hashlib.sha256(fh.read()).hexdigest()
    os.replace(tmp, os.path.join(out_dir, name))
finally:
    if os.path.exists(tmp):
        os.remove(tmp)

with open(os.path.join(out_dir, f"{name}.sha256"), "w", encoding="utf-8") as fh:
    fh.write(f"{digest}  {name}\n")
print(f"package_content: wrote {os.path.join(out_dir, name)} "
      f"({sum(c[2] for c in classes)} files, schema {schema_major}, sha256 {digest})")
PY
