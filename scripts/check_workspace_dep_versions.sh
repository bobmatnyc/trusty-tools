#!/usr/bin/env bash
#
# check_workspace_dep_versions.sh — every internal `[workspace.dependencies]`
# row's version requirement must accept the member crate's own version (#6776).
#
# Why: a row in the root `[workspace.dependencies]` table carries BOTH a `path`
#   and a `version`. In-tree the path wins, so cargo never evaluates the version
#   requirement and `cargo build` / `cargo check` / `cargo test` all stay green
#   no matter how far the two drift. The requirement is only consulted when the
#   crate is resolved from crates.io — i.e. at publish time, and by any external
#   consumer. #6776 found `trusty-console = { path = …, version = "0.9.0" }`
#   against a crate at `0.11.0`: `^0.9.0` excludes `0.11.0`, so a published
#   consumer resolves a version that does not exist or an ancient one. Same
#   class as #4088, where the path override likewise hid a break a workspace
#   `cargo check` structurally cannot see.
#
# What: reads the root `Cargo.toml`, takes every `[workspace.dependencies]` row
#   that declares `path = "crates/<dir>"` AND a `version`, reads that member's
#   own `[package] version`, and asserts the member version SATISFIES the row's
#   requirement under Cargo's caret rules (including the 0.x rule, where the
#   minor is the breaking component). A row with a `path` and no `version` is
#   in-tree-only resolution and is reported as SKIP, not silently ignored.
#
#   Fails CLOSED. A member manifest that cannot be read, a version that is not a
#   plain `X.Y.Z[-pre]`, and a requirement operator this script does not model
#   (anything but a bare or `^` requirement) are all failures naming the row —
#   an unverifiable row is never a silent pass. It also enforces a SCAN FLOOR
#   (#4618): examining zero rows exits non-zero rather than reporting success
#   over nothing.
#
# Usage:
#   bash scripts/check_workspace_dep_versions.sh
#
# Env: WS_DEP_MANIFEST overrides the root manifest scanned (fixtures only).
#   WS_DEP_MIN_ROWS overrides the scan floor (fixtures only; default 8).
#
# Exit: 0 when every internal row's requirement accepts its member's version;
#   1 on the first drift, unverifiable row, or vacuous scan, naming the row and
#   the exact edit that closes it.
#
# Test: scripts/check_workspace_dep_versions_selftest.sh drives this script over
#   fixture manifests — satisfied rows, the #6776 `^0.9.0` vs `0.11.0` drift,
#   the 0.x minor rule, a `1.x` row, an alias key whose directory differs from
#   the row name, a missing member manifest, an unsupported operator, and an
#   empty table (scan floor) — and asserts the live repo scan passes.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). POSIX tools only, no
#   cargo and no network, so it runs in a toolchain-less shell job.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
MANIFEST="${WS_DEP_MANIFEST:-${REPO_ROOT}/Cargo.toml}"
MIN_ROWS="${WS_DEP_MIN_ROWS:-8}"

# The member manifests are resolved relative to the manifest's own directory so
# a fixture root is self-contained.
MANIFEST_DIR="$(cd "$(dirname "${MANIFEST}")" && pwd)"

failures=0
examined=0

fail() {
  printf '[FAIL] %s\n' "$1" >&2
  failures=$((failures + 1))
}

# ---------------------------------------------------------------------------
# rows — emit `name<TAB>path<TAB>version` for every `[workspace.dependencies]`
# row declaring a `path`. `version` is empty when the row has none. The awk
# arms on the section header and disarms on the next top-level table, so a
# `path = ` in some other section is never picked up.
# ---------------------------------------------------------------------------
rows() {
  awk '
    /^\[workspace\.dependencies\]/ { inside = 1; next }
    /^\[/                          { inside = 0 }
    !inside                        { next }
    # Skip full-line comments and blanks.
    /^[[:space:]]*#/               { next }
    /^[[:space:]]*$/               { next }
    # A row must name a key and carry a crates/ path.
    !/path[[:space:]]*=[[:space:]]*"/ { next }
    {
      line = $0
      name = line
      sub(/[[:space:]]*=.*$/, "", name)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", name)

      path = line
      sub(/^.*path[[:space:]]*=[[:space:]]*"/, "", path)
      sub(/".*$/, "", path)

      ver = ""
      if (line ~ /version[[:space:]]*=[[:space:]]*"/) {
        ver = line
        sub(/^.*version[[:space:]]*=[[:space:]]*"/, "", ver)
        sub(/".*$/, "", ver)
      }
      printf "%s\t%s\t%s\n", name, path, ver
    }
  ' "${MANIFEST}"
}

# ---------------------------------------------------------------------------
# member_version <member-dir> — the member's `[package] version`, or empty when
# the manifest is unreadable or declares none.
# ---------------------------------------------------------------------------
member_version() {
  local dir="$1"
  local file="${MANIFEST_DIR}/${dir}/Cargo.toml"
  [ -f "${file}" ] || return 0
  awk '
    /^\[package\]/            { inside = 1; next }
    /^\[/                     { inside = 0 }
    inside && /^[[:space:]]*version[[:space:]]*=/ {
      v = $0
      sub(/^.*=[[:space:]]*"/, "", v)
      sub(/".*$/, "", v)
      print v
      exit
    }
  ' "${file}"
}

# ---------------------------------------------------------------------------
# Caret-requirement satisfaction, Cargo semantics.
#
# `satisfies <req> <version>` exits 0 when <version> is inside the caret range
# <req> denotes, 1 when it is outside, and 2 when either side is a shape this
# script does not model (the caller turns that into a fail-closed error).
#
# Upper bound increments the LEFTMOST NONZERO component of the requirement as
# written, which is what makes the minor the breaking component for `0.y.z`:
#   ^1.2.3 -> <2.0.0    ^0.2.3 -> <0.3.0    ^0.0.3 -> <0.0.4
#   ^1.2   -> <2.0.0    ^0.2   -> <0.3.0    ^0.0   -> <0.1.0
#   ^1     -> <2.0.0    ^0     -> <1.0.0
# ---------------------------------------------------------------------------
satisfies() {
  local req="$1" ver="$2"

  # Only a bare or explicitly-caret requirement is modelled. Anything else
  # (`=`, `~`, `>=`, `*`, a comma-separated set) is unverifiable here.
  case "${req}" in
    ^*) req="${req#^}" ;;
    [0-9]*) ;;
    *) return 2 ;;
  esac
  case "${req}" in *[!0-9.]*) return 2 ;; esac
  case "${req}" in *..*|.*|*.) return 2 ;; esac

  # A prerelease/build version is not modelled; the workspace has none.
  case "${ver}" in *[!0-9.]*) return 2 ;; esac
  case "${ver}" in *..*|.*|*.) return 2 ;; esac

  local r_major r_minor r_patch r_minor_set r_patch_set
  r_major="${req%%.*}"
  r_minor=0
  r_patch=0
  r_minor_set=0
  r_patch_set=0
  case "${req}" in
    *.*.*)
      r_minor_set=1
      r_patch_set=1
      r_minor="${req#*.}"
      r_minor="${r_minor%%.*}"
      r_patch="${req##*.}"
      ;;
    *.*)
      r_minor_set=1
      r_minor="${req#*.}"
      ;;
  esac
  [ -n "${r_major}" ] || return 2
  [ -n "${r_minor}" ] || return 2
  [ -n "${r_patch}" ] || return 2

  local v_major v_minor v_patch
  v_major="${ver%%.*}"
  v_minor=0
  v_patch=0
  case "${ver}" in
    *.*.*)
      v_minor="${ver#*.}"
      v_minor="${v_minor%%.*}"
      v_patch="${ver##*.}"
      ;;
    *.*)
      v_minor="${ver#*.}"
      ;;
  esac
  [ -n "${v_major}" ] && [ -n "${v_minor}" ] && [ -n "${v_patch}" ] || return 2

  # Lower bound: >= r_major.r_minor.r_patch
  local lo hi
  lo=$(printf '%d%03d%03d' "${r_major}" "${r_minor}" "${r_patch}")

  # Upper bound (exclusive), per the leftmost-nonzero rule above.
  if [ "${r_major}" -ne 0 ]; then
    hi=$(printf '%d%03d%03d' "$((r_major + 1))" 0 0)
  elif [ "${r_minor_set}" -eq 0 ]; then
    hi=$(printf '%d%03d%03d' 1 0 0)
  elif [ "${r_minor}" -ne 0 ]; then
    hi=$(printf '%d%03d%03d' 0 "$((r_minor + 1))" 0)
  elif [ "${r_patch_set}" -eq 0 ]; then
    hi=$(printf '%d%03d%03d' 0 1 0)
  elif [ "${r_patch}" -ne 0 ]; then
    hi=$(printf '%d%03d%03d' 0 0 "$((r_patch + 1))")
  else
    hi=$(printf '%d%03d%03d' 0 0 1)
  fi

  local v
  v=$(printf '%d%03d%03d' "${v_major}" "${v_minor}" "${v_patch}")
  # 10#… so a zero-padded component is never read as octal.
  if [ "$((10#${v}))" -ge "$((10#${lo}))" ] && [ "$((10#${v}))" -lt "$((10#${hi}))" ]; then
    return 0
  fi
  return 1
}

# ---------------------------------------------------------------------------
# Main scan
# ---------------------------------------------------------------------------
if [ ! -f "${MANIFEST}" ]; then
  printf '[FAIL] root manifest not found: %s\n' "${MANIFEST}" >&2
  exit 1
fi

printf 'Checking internal [workspace.dependencies] version requirements in %s\n' \
  "${MANIFEST}"

while IFS="$(printf '\t')" read -r name path req; do
  [ -n "${name}" ] || continue

  if [ -z "${req}" ]; then
    printf '[SKIP] %-22s %s — path-only row, no version requirement to check\n' \
      "${name}" "${path}"
    continue
  fi

  crate_ver="$(member_version "${path}")"
  if [ -z "${crate_ver}" ]; then
    fail "$(printf '%s: cannot read [package] version from %s/Cargo.toml — row is unverifiable' \
      "${name}" "${path}")"
    examined=$((examined + 1))
    continue
  fi

  examined=$((examined + 1))

  set +e
  satisfies "${req}" "${crate_ver}"
  verdict=$?
  set -e

  case "${verdict}" in
    0)
      printf '[ OK ] %-22s req %-8s accepts crate %s\n' "${name}" "${req}" "${crate_ver}"
      ;;
    1)
      suggest="$(printf '%s' "${crate_ver}" | awk -F. '{ print $1"."$2 }')"
      # The 0.x note only applies when the member really is a 0.y.z crate.
      if [ "${crate_ver%%.*}" = "0" ]; then
        note=' (for 0.y.z the MINOR is the breaking component)'
      else
        note=''
      fi
      fail "$(printf '%s: root Cargo.toml requires version = "%s" but %s is %s.\n         ^%s does not accept %s%s.\n         Fix: set the row to version = "%s" in [workspace.dependencies].' \
        "${name}" "${req}" "${path}" "${crate_ver}" \
        "${req}" "${crate_ver}" "${note}" "${suggest}")"
      ;;
    *)
      fail "$(printf '%s: unmodelled version shape (req "%s", crate "%s"). Extend %s rather than skipping the row.' \
        "${name}" "${req}" "${crate_ver}" "$(basename "$0")")"
      ;;
  esac
done <<EOF
$(rows)
EOF

# Scan floor (#4618): a run that examined nothing must not report success.
if [ "${examined}" -lt "${MIN_ROWS}" ]; then
  printf '[FAIL] SCAN FLOOR: examined %d internal row(s), expected at least %d.\n' \
    "${examined}" "${MIN_ROWS}" >&2
  printf '       Either [workspace.dependencies] lost its internal rows or the\n' >&2
  printf '       parser stopped matching them. A vacuous scan is not a pass.\n' >&2
  exit 1
fi

if [ "${failures}" -gt 0 ]; then
  printf '\n%d internal [workspace.dependencies] row(s) drifted from their member crate.\n' \
    "${failures}" >&2
  exit 1
fi

printf '\nAll %d internal row(s) accept their member crate version.\n' "${examined}"
