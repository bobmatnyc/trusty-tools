#!/usr/bin/env bash
#
# package_content_selftest.sh — regression fixtures for
# scripts/package_content.sh (ADR-0064, #8389).
#
# Why: the content bundle is pinned by sha256, so a packer that lets one
#   varying byte in (an mtime, walk order, the runner's uid, the gzip header
#   timestamp) breaks every pin without failing anything. A packer that
#   writes an empty or partial bundle when a source directory is missing is
#   worse: the release pins cleanly and deploys nothing.
#
# What: builds a throwaway content tree and asserts:
#     determinism   two runs over the same tree, with every source file's
#                   mtime and mode changed and a clock tick in between,
#                   give byte-identical tarballs
#     metadata      gzip header mtime is 0; the first entry is
#                   bundle-manifest.toml; entries are sorted; every entry has
#                   mtime 0, uid/gid 0, empty owner names, mode 0644/0755
#     manifest      carries bundle_version, tag and schema_major = 1;
#                   file_count and each class's `files` match the fixture
#     sha256        the sidecar verifies against the tarball
#     fail-open     a missing class directory, an empty one, and a symlink
#                   each exit 1 and leave no tarball behind (the missing
#                   case runs into an out-dir that already held a bundle)
#     usage         a non-SemVer --version, a multi-line one and one holding
#                   a carriage return each exit 2 and write nothing
#     relative      a relative --out-dir and --source-root resolve against
#                   the caller's cwd, not the repo root
#     live          the default path table packages every class of this
#                   checkout with at least one file each
#
# Test: this IS the test. Run directly: bash scripts/package_content_selftest.sh
#   CI runs it in ci.yml's `changes` job and before packaging in
#   content-release.yml.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); needs python3.

# #7812: re-run under bash when invoked as `zsh <this script>`.
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKER="$SCRIPT_DIR/package_content.sh"
CLASSES="agents skills instructions output-styles sm_instructions harness_understanding"

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/package-content.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT

CASES=0
FAILURES=0
pass() { CASES=$((CASES + 1)); printf '  ok   %s\n' "$1"; }
fail() { CASES=$((CASES + 1)); FAILURES=$((FAILURES + 1)); printf '  FAIL %s\n' "$1"; }

sha_check() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum -c "$1"; else shasum -a 256 -c "$1"; fi
}

# new_tree <dir>: a content tree holding every class, files created in
# reverse name order so a walk-order dependency would show.
new_tree() {
  local t="$1" c
  for c in $CLASSES; do
    mkdir -p "$t/$c/z-sub/deeper" "$t/$c/a-sub"
    printf 'z %s\n' "$c" > "$t/$c/z-sub/deeper/zz.md"
    printf 'a %s\n' "$c" > "$t/$c/a-sub/aa.md"
    printf 'top %s\n' "$c" > "$t/$c/README.md"
  done
  printf 'ignored\n' > "$t/agents/.DS_Store"
}

# run_packer <out-dir> <source-root> [version]: prints the exit code.
run_packer() {
  local rc=0
  "$PACKER" --version "${3:-1.2.3}" --out-dir "$1" --source-root "$2" \
    > "$TMP_ROOT/last.log" 2>&1 || rc=$?
  echo "$rc"
}

TARBALL=content-v1.2.3.tar.gz

echo "determinism:"
new_tree "$TMP_ROOT/tree"
rc1="$(run_packer "$TMP_ROOT/out1" "$TMP_ROOT/tree")"
# Perturb everything the packer must ignore: mtimes, modes, and the clock.
find "$TMP_ROOT/tree" -type f -exec touch -t 200001020304 {} +
find "$TMP_ROOT/tree" -type f -exec chmod 600 {} +
sleep 1
rc2="$(run_packer "$TMP_ROOT/out2" "$TMP_ROOT/tree")"
if [ "$rc1" = 0 ] && [ "$rc2" = 0 ] && cmp -s "$TMP_ROOT/out1/$TARBALL" "$TMP_ROOT/out2/$TARBALL"; then
  pass "two runs over the same tree are byte-identical"
else
  fail "two runs differ (exit $rc1 / $rc2); last log: $(cat "$TMP_ROOT/last.log")"
fi

echo "metadata:"
# Written to a file first: bash 3.2 misparses a heredoc inside `$( )`.
cat > "$TMP_ROOT/check_meta.py" <<'PY'
import struct, sys, tarfile
path = sys.argv[1]
with open(path, "rb") as fh:
    head = fh.read(10)
problems = []
if struct.unpack("<I", head[4:8])[0] != 0:
    problems.append("gzip header mtime is not 0")
if head[3] & 0x08:
    problems.append("gzip header carries a file name")
with tarfile.open(path, "r:gz") as tar:
    members = tar.getmembers()
names = [m.name for m in members]
if not names or names[0] != "bundle-manifest.toml":
    problems.append(f"first entry is {names[:1]}, not bundle-manifest.toml")
if names[1:] != sorted(names[1:], key=lambda n: n.encode()):
    problems.append("entries after the manifest are not sorted")
if any(".DS_Store" in n for n in names):
    problems.append("a dot-file was packaged")
for m in members:
    want = 0o755 if m.isdir() else 0o644
    if (m.mtime, m.uid, m.gid, m.uname, m.gname, m.mode) != (0, 0, 0, "", "", want):
        problems.append(f"{m.name}: mtime={m.mtime} uid={m.uid} gid={m.gid} "
                        f"uname={m.uname!r} gname={m.gname!r} mode={oct(m.mode)}")
        break
if problems:
    print("; ".join(problems))
    sys.exit(1)
PY
if meta_err="$(python3 "$TMP_ROOT/check_meta.py" "$TMP_ROOT/out1/$TARBALL" 2>&1)"; then
  pass "fixed gzip mtime, sorted entries, fixed owner/mtime/mode"
else
  fail "metadata: ${meta_err}"
fi

echo "manifest:"
manifest="$(tar -xzOf "$TMP_ROOT/out1/$TARBALL" bundle-manifest.toml 2>/dev/null || true)"
for want in 'bundle_version = "1.2.3"' 'tag = "content-v1.2.3"' 'schema_major = 1'; do
  if printf '%s\n' "$manifest" | grep -qxF "$want"; then
    pass "manifest carries: $want"
  else
    fail "manifest lacks '$want'; got: $manifest"
  fi
done
# new_tree writes 3 files per class (the skipped .DS_Store is not one of them).
want_files=3
want_total=0
for c in $CLASSES; do want_total=$((want_total + want_files)); done
if printf '%s\n' "$manifest" | grep -qxF "file_count = ${want_total}"; then
  pass "manifest file_count = ${want_total}"
else
  fail "manifest file_count is not ${want_total}; got: $manifest"
fi
miscounted=""
for c in $CLASSES; do
  got="$(printf '%s\n' "$manifest" | awk -v n="name = \"$c\"" \
    '$0 == n { f = 1; next } f && /^files = / { print $3; exit }')"
  [ "$got" = "$want_files" ] || miscounted="$miscounted $c=${got:-none}"
done
if [ -z "$miscounted" ]; then
  pass "every class has files = ${want_files}"
else
  fail "class file counts differ from ${want_files}:${miscounted}"
fi

echo "sha256:"
if (cd "$TMP_ROOT/out1" && sha_check "$TARBALL.sha256" >/dev/null 2>&1); then
  pass "sidecar verifies the tarball"
else
  fail "sidecar does not verify: $(cat "$TMP_ROOT/out1/$TARBALL.sha256" 2>&1)"
fi

echo "fail-open:"
# expect_refusal <label> <tree> <out-dir>: exit 1 and no bundle left behind.
expect_refusal() {
  local rc
  rc="$(run_packer "$3" "$2")"
  if [ "$rc" = 1 ] && [ ! -e "$3/$TARBALL" ] && [ ! -e "$3/$TARBALL.sha256" ]; then
    pass "$1 -> exit 1, no bundle"
  else
    fail "$1: exit $rc, out-dir holds: $(find "$3" -type f 2>/dev/null | tr '\n' ' ')"
  fi
}
new_tree "$TMP_ROOT/missing"
rm -rf "$TMP_ROOT/missing/output-styles"
cp -R "$TMP_ROOT/out1" "$TMP_ROOT/out-missing"   # holds a good bundle already
expect_refusal "missing class directory" "$TMP_ROOT/missing" "$TMP_ROOT/out-missing"
new_tree "$TMP_ROOT/empty"
rm -rf "$TMP_ROOT/empty/skills" && mkdir "$TMP_ROOT/empty/skills"
expect_refusal "empty class directory" "$TMP_ROOT/empty" "$TMP_ROOT/out-empty"
new_tree "$TMP_ROOT/link"
ln -s /etc/hosts "$TMP_ROOT/link/agents/escape.md"
expect_refusal "symlink in a class" "$TMP_ROOT/link" "$TMP_ROOT/out-link"

echo "usage:"
# expect_usage <label> <version>: exit 2 and nothing written to the out-dir.
# grep checks each line on its own, so a multi-line value passed a
# line-anchored check when any one line was valid.
expect_usage() {
  local rc out="$TMP_ROOT/out-usage-$CASES"
  rc="$(run_packer "$out" "$TMP_ROOT/tree" "$2")"
  if [ "$rc" = 2 ] && [ -z "$(find "$out" -type f 2>/dev/null)" ]; then
    pass "$1 -> exit 2, nothing written"
  else
    fail "$1: exit $rc, out-dir holds: $(find "$out" -type f 2>/dev/null | tr '\n' ' ')"
  fi
}
expect_usage "non-SemVer version" "v1.2"
expect_usage "multi-line version" $'1.0.0\ntag=x'
expect_usage "version with a carriage return" $'1.0.0\n1.0.0\r'

echo "relative:"
mkdir -p "$TMP_ROOT/cwd"
rc=0
(cd "$TMP_ROOT/cwd" && "$PACKER" --version 1.2.3 --out-dir rel-out --source-root ../tree) \
  > "$TMP_ROOT/rel.log" 2>&1 || rc=$?
if [ "$rc" = 0 ] && [ -f "$TMP_ROOT/cwd/rel-out/$TARBALL" ] && [ ! -e "$SCRIPT_DIR/../rel-out" ]; then
  pass "relative --out-dir and --source-root resolve against the caller's cwd"
else
  fail "relative paths: exit $rc; $(cat "$TMP_ROOT/rel.log")"
fi

echo "live:"
rc=0
"$PACKER" --version 0.0.0 --out-dir "$TMP_ROOT/out-live" > "$TMP_ROOT/live.log" 2>&1 || rc=$?
live="$(tar -xzOf "$TMP_ROOT/out-live/content-v0.0.0.tar.gz" bundle-manifest.toml 2>/dev/null || true)"
missing_classes=""
for c in $CLASSES; do
  printf '%s\n' "$live" | grep -qxF "name = \"$c\"" || missing_classes="$missing_classes $c"
done
if [ "$rc" = 0 ] && [ -z "$missing_classes" ] && ! printf '%s\n' "$live" | grep -qx 'files = 0'; then
  pass "default path table packages every class of this checkout"
else
  fail "live run: exit $rc, missing classes:${missing_classes:- none}; $(cat "$TMP_ROOT/live.log")"
fi

echo
if [ "$FAILURES" -gt 0 ]; then
  echo "package_content_selftest: ${FAILURES}/${CASES} case(s) FAILED"
  exit 1
fi
echo "package_content_selftest: ${CASES}/${CASES} case(s) passed"
