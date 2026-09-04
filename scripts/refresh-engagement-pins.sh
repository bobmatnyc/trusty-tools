#!/usr/bin/env bash
#
# refresh-engagement-pins.sh — hold the engagement template's sibling-tool pins
# at the workspace versions (#6772).
#
# Why: `crates/trusty-audit/templates/engagement.template.toml` pins the four
#   sibling tools an engagement runs (`tga`, `trusty-search`, `trusty-analyze`,
#   `trusty-review`) as literal versions, and nothing bumps them. The file is
#   also `include_str!`-ed into `instructions::ENGAGEMENT_TEMPLATE` and written
#   out verbatim by `taudit distribute`, so a stale pin ships inside the binary
#   and into every client package built from it. At 7cfeda52d the template
#   pinned tga 6.0.0 / trusty-analyze 0.12.5 / trusty-review 0.33.0 while the
#   same release train published tga 7.0.0 / 0.12.6 / 0.33.1 (#6772; PR #6723
#   was the previous instance of the same drift).
#
# What: reads the ACTIVE `[tools]` table out of the template, asks
#   `cargo metadata --no-deps` for each pinned package's current workspace
#   version, and either rewrites the pins to match (default) or reports the
#   mismatches and exits nonzero (`--check`). Both modes touch only the version
#   literal on a pin line inside the `[tools]` table — comments, the commented
#   `# [tools]` digest example, key order and every other byte are preserved,
#   so a second run over an already-refreshed template is a no-op.
#
#   Both pin spellings the template documents are handled:
#
#       tga = "7.1.0"
#       trusty-review = { version = "0.34.0", sha256 = "…" }
#
#   Fails closed: a `[tools]` table that is missing, empty, or names a package
#   this workspace does not build is a usage error (exit 2), never a silent
#   pass. A gate that reports success over a table it could not read is the
#   failure mode this script exists to remove.
#
#   NOT what this decides: whether a lagging pin is ACCEPTABLE. A pin may
#   legitimately lag when the sibling crate is not part of the release train.
#   That judgement lives in `scripts/preflight-publish.sh` CHECK 10, which
#   consults crates.io; this script only answers "does the pin equal the
#   workspace version".
#
# Usage:
#   scripts/refresh-engagement-pins.sh [--check] [--repo <dir>]
#
#   --check        report stale pins instead of rewriting them. One
#                  `STALE <pkg> pinned=<x> workspace=<y>` line per mismatch on
#                  stdout, then exit 1. Exit 0 and a single OK line when every
#                  pin is current.
#   --repo <dir>   operate on a different repo root (self-test only).
#   -h|--help      print this header and exit 0.
#
# Exit codes: 0 = pins are current (`--check`), or were rewritten/left alone
#   (default). 1 = at least one stale pin (`--check` only). 2 = usage error,
#   unreadable template, unreadable metadata, or a pin naming a package that is
#   not a workspace member.
#
# Test: scripts/refresh-engagement-pins-selftest.sh drives both modes over
#   synthetic single-file workspaces — a current table, a stale table, the
#   inline-table digest spelling, an unknown package, an empty table, a missing
#   `[tools]` header, and an idempotence case that reruns the rewrite and
#   diffs the bytes. It also runs `--check` against the REAL checked-out
#   template so the live repo is proven, not only a fixture.
#
# Portability: POSIX tools plus python3 (already required by check_semver.sh),
#   bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

for arg in "$@"; do
  case "$arg" in
    -h|--help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
  esac
done

usage() {
  echo "usage: scripts/refresh-engagement-pins.sh [--check] [--repo <dir>]" >&2
  exit 2
}

CHECK_ONLY=0
REPO_ARG=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --check) CHECK_ONLY=1; shift ;;
    --repo)
      [ "$#" -ge 2 ] || usage
      REPO_ARG="$2"
      shift 2
      ;;
    *)
      echo "refresh-engagement-pins: unknown argument: $1" >&2
      usage
      ;;
  esac
done

if [ -n "$REPO_ARG" ]; then
  REPO_ROOT="$(cd "$REPO_ARG" && pwd)"
else
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
fi

TEMPLATE_REL="crates/trusty-audit/templates/engagement.template.toml"
TEMPLATE="${REPO_ROOT}/${TEMPLATE_REL}"

if [ ! -f "$TEMPLATE" ]; then
  echo "refresh-engagement-pins: ERROR: no template at ${TEMPLATE}" >&2
  exit 2
fi

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/refresh-engagement-pins.XXXXXX")"
trap 'rm -rf "$SCRATCH"' EXIT

# ---------------------------------------------------------------------------
# Read the ACTIVE [tools] table: `pkg<TAB>pinned-version` per line.
#
# `in_tools` flips only on a bare `[header]` at column 0, so the commented
# `# [tools]` digest example further down the file is never entered, and the
# table ends at the next real header.
# ---------------------------------------------------------------------------
PINS="${SCRATCH}/pins.tsv"
awk '
  /^\[/ { in_tools = ($0 == "[tools]"); next }
  !in_tools { next }
  /^[[:space:]]*#/ { next }
  match($0, /^[A-Za-z0-9_.-]+/) {
    name = substr($0, RSTART, RLENGTH)
    rest = substr($0, RSTART + RLENGTH)
    if (rest !~ /^[[:space:]]*=/) next
    # Inline-table spelling: narrow to the inner `version` key first, so a
    # sha256 digest that follows it is never mistaken for the version.
    if (rest ~ /^[[:space:]]*=[[:space:]]*\{/) {
      if (!match(rest, /version[[:space:]]*=[[:space:]]*"[^"]*"/)) next
      rest = substr(rest, RSTART, RLENGTH)
    }
    sub(/^[^"]*"/, "", rest)
    sub(/".*$/, "", rest)
    if (rest != "") printf "%s\t%s\n", name, rest
  }
' "$TEMPLATE" > "$PINS"

if [ ! -s "$PINS" ]; then
  echo "refresh-engagement-pins: ERROR: ${TEMPLATE_REL} has no readable pins in a" >&2
  echo "       [tools] table. The table is REQUIRED (see the template's own note:" >&2
  echo "       there is no \"latest\"), so an empty or missing one is a defect in the" >&2
  echo "       template, not a state this gate may pass over." >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# Workspace versions. `cargo metadata` is the single source of truth here — it
# resolves the package name directly (crates/trusty-git-analytics/ is package
# `tga`), which a crates/<dir>/Cargo.toml scan would have to special-case.
# ---------------------------------------------------------------------------
META="${SCRATCH}/metadata.json"
if ! cargo metadata --no-deps --format-version 1 --manifest-path "${REPO_ROOT}/Cargo.toml" \
     > "$META" 2> "${SCRATCH}/meta-err.txt"; then
  echo "refresh-engagement-pins: ERROR: 'cargo metadata --no-deps' failed:" >&2
  sed 's/^/       /' "${SCRATCH}/meta-err.txt" >&2
  exit 2
fi

WANTED="${SCRATCH}/wanted.tsv"
WANTED_ERR="${SCRATCH}/wanted.err"
if ! python3 - "$META" "$PINS" > "$WANTED" 2> "$WANTED_ERR" <<'PY'
import json
import sys

meta_path, pins_path = sys.argv[1], sys.argv[2]
with open(meta_path, encoding="utf-8") as fh:
    versions = {p["name"]: p["version"] for p in json.load(fh)["packages"]}

missing = []
out = []
with open(pins_path, encoding="utf-8") as fh:
    for line in fh:
        line = line.rstrip("\n")
        if not line:
            continue
        name, pinned = line.split("\t", 1)
        if name not in versions:
            missing.append(name)
            continue
        out.append(f"{name}\t{pinned}\t{versions[name]}")

if missing:
    print("MISSING\t" + ",".join(missing), file=sys.stderr)
    raise SystemExit(3)

print("\n".join(out))
PY
then
  echo "refresh-engagement-pins: ERROR: ${TEMPLATE_REL}'s [tools] table pins a package" >&2
  echo "       this workspace does not build:" >&2
  sed 's/^/       /' "$WANTED_ERR" >&2
  echo "       Every pin must name a workspace package, so the release train can" >&2
  echo "       keep it current. Fix the pin name in the template." >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# --check: report, never write.
# ---------------------------------------------------------------------------
if [ "$CHECK_ONLY" -eq 1 ]; then
  STALE=0
  while IFS=$'\t' read -r name pinned wanted; do
    [ -n "$name" ] || continue
    if [ "$pinned" != "$wanted" ]; then
      echo "STALE ${name} pinned=${pinned} workspace=${wanted}"
      STALE=$((STALE + 1))
    fi
  done < "$WANTED"

  if [ "$STALE" -gt 0 ]; then
    echo "refresh-engagement-pins: ${STALE} stale pin(s) in ${TEMPLATE_REL}." >&2
    echo "       Refresh them: scripts/refresh-engagement-pins.sh" >&2
    exit 1
  fi

  echo "refresh-engagement-pins: OK — every [tools] pin in ${TEMPLATE_REL} names the"
  echo "  crate's current workspace version."
  exit 0
fi

# ---------------------------------------------------------------------------
# Default: rewrite the version literal on each stale pin line, in place.
# ---------------------------------------------------------------------------
UPDATED="${SCRATCH}/engagement.template.toml"
awk -v want_file="$WANTED" '
  BEGIN {
    while ((getline line < want_file) > 0) {
      n = split(line, f, "\t")
      if (n >= 3) want[f[1]] = f[3]
    }
  }
  /^\[/ { in_tools = ($0 == "[tools]"); print; next }
  !in_tools || /^[[:space:]]*#/ { print; next }
  {
    if (!match($0, /^[A-Za-z0-9_.-]+/)) { print; next }
    name = substr($0, RSTART, RLENGTH)
    if (!(name in want)) { print; next }
    head = substr($0, 1, RSTART + RLENGTH - 1)
    rest = substr($0, RSTART + RLENGTH)
    if (rest !~ /^[[:space:]]*=/) { print; next }
    # Replace the FIRST quoted literal after `=` for the plain spelling, and
    # the one belonging to the inner `version` key for the inline-table
    # spelling — never a sha256 digest that follows it.
    if (rest ~ /^[[:space:]]*=[[:space:]]*\{/) {
      if (!match(rest, /version[[:space:]]*=[[:space:]]*"[^"]*"/)) { print; next }
      pre = substr(rest, 1, RSTART - 1)
      mid = substr(rest, RSTART, RLENGTH)
      post = substr(rest, RSTART + RLENGTH)
      sub(/"[^"]*"$/, "\"" want[name] "\"", mid)
      print head pre mid post
    } else {
      if (!match(rest, /"[^"]*"/)) { print; next }
      pre = substr(rest, 1, RSTART - 1)
      post = substr(rest, RSTART + RLENGTH)
      print head pre "\"" want[name] "\"" post
    }
  }
' "$TEMPLATE" > "$UPDATED"

if cmp -s "$TEMPLATE" "$UPDATED"; then
  echo "refresh-engagement-pins: no change — every [tools] pin already names the"
  echo "  crate's current workspace version."
  exit 0
fi

cp "$UPDATED" "$TEMPLATE"
echo "refresh-engagement-pins: rewrote ${TEMPLATE_REL}:"
while IFS=$'\t' read -r name pinned wanted; do
  [ -n "$name" ] || continue
  [ "$pinned" = "$wanted" ] && continue
  echo "  ${name}: ${pinned} -> ${wanted}"
done < "$WANTED"
echo "  Rebuild trusty-audit so instructions::ENGAGEMENT_TEMPLATE picks the new"
echo "  bytes up — 'taudit distribute' ships the compiled copy, not this file."
exit 0
