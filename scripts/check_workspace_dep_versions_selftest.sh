#!/usr/bin/env bash
#
# check_workspace_dep_versions_selftest.sh — mutation self-test for the internal
# `[workspace.dependencies]` version-requirement gate (#6776).
#
# Why: the gate it tests is the only thing standing between a drifted workspace
#   row and a publish-time resolution break, because no cargo command in-tree
#   can see the drift at all (the `path` override wins). A gate in that position
#   is untested code unless something re-proves it can still FAIL — the #4618
#   lesson, and the reason every case below is a manifest that must be rejected.
#
# What: builds throwaway fixture workspaces under a temp dir, points the gate at
#   each via `WS_DEP_MANIFEST`, and asserts the exit status and (where it
#   matters) the reason. Cases:
#     satisfied            every row's requirement accepts its member  -> PASS
#     drift_0x             the real #6776 shape, ^0.9.0 vs 0.11.0      -> FAIL
#     drift_major          ^6.0.1 vs 7.1.0 (major bump)                -> FAIL
#     ok_0x_patch          ^0.5.0 vs 0.5.1 (patch inside the range)    -> PASS
#     ok_partial_req       ^0.47 vs 0.47.3 (two-component requirement) -> PASS
#     ok_major_range       ^1.0.0 vs 1.5.17 (1.x caret is wide)        -> PASS
#     alias_key            row key differs from its directory          -> FAIL
#     missing_member       member Cargo.toml absent                    -> FAIL
#     bad_operator         a requirement shape the gate does not model -> FAIL
#     scan_floor           table present but zero internal rows        -> FAIL
#   Plus a final case asserting the LIVE repo manifest passes, so the gate and
#   the tree it guards cannot disagree silently.
#
# Usage:
#   bash scripts/check_workspace_dep_versions_selftest.sh          # every case
#   bash scripts/check_workspace_dep_versions_selftest.sh drift_0x # one by name
#
# Exit: 0 when every case reaches its expected verdict; 1 naming the first case
#   that did not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). POSIX tools only.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="${SCRIPT_DIR}/check_workspace_dep_versions.sh"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

TMPROOT="$(mktemp -d)"
trap 'rm -rf "${TMPROOT}"' EXIT

failures=0
only="${1:-}"

pass() { printf '  ok   %s\n' "$1"; }
bad() {
  printf '  FAIL %s: %s\n' "$1" "$2" >&2
  failures=$((failures + 1))
}

# want_case <name> <expect: pass|fail> <fixture-dir> [expected-substring]
want_case() {
  local name="$1" expect="$2" dir="$3" needle="${4:-}"
  if [ -n "${only}" ] && [ "${only}" != "${name}" ]; then
    return 0
  fi
  local out status
  # The floor is lowered for fixtures: they carry a handful of rows, not the
  # workspace's full set. The scan_floor case overrides it back up.
  out="$(WS_DEP_MANIFEST="${dir}/Cargo.toml" WS_DEP_MIN_ROWS="${WS_DEP_MIN_ROWS_CASE:-1}" \
    bash "${GATE}" 2>&1)"
  status=$?

  if [ "${expect}" = "pass" ] && [ "${status}" -ne 0 ]; then
    bad "${name}" "expected exit 0, got ${status}. Output: ${out}"
    return 0
  fi
  if [ "${expect}" = "fail" ] && [ "${status}" -eq 0 ]; then
    bad "${name}" "expected NON-ZERO exit, got 0 — the gate accepted a bad manifest. Output: ${out}"
    return 0
  fi
  if [ -n "${needle}" ] && ! printf '%s' "${out}" | grep -qF "${needle}"; then
    bad "${name}" "output missing expected text '${needle}'. Output: ${out}"
    return 0
  fi
  pass "${name}"
}

# member <fixture-dir> <crate-dir> <name> <version>
member() {
  local dir="$1" sub="$2" pkg="$3" ver="$4"
  mkdir -p "${dir}/crates/${sub}"
  cat >"${dir}/crates/${sub}/Cargo.toml" <<EOF
[package]
name    = "${pkg}"
version = "${ver}"
edition = "2021"
EOF
}

# ── satisfied / ok_* cases: one fixture covering every accepting shape ────────
ok_dir="${TMPROOT}/satisfied"
mkdir -p "${ok_dir}"
member "${ok_dir}" trusty-code     trusty-code     0.5.1
member "${ok_dir}" trusty-common   trusty-common   0.47.3
member "${ok_dir}" trusty-mpm      trusty-mpm      1.5.17
cat >"${ok_dir}/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/*"]

[workspace.dependencies]
# A comment row must not be parsed as a dependency.
trusty-code = { path = "crates/trusty-code", version = "0.5.0" }
trusty-common = { path = "crates/trusty-common", version = "0.47" }
trusty-mpm = { path = "crates/trusty-mpm", version = "1.0.0" }
serde = { version = "1", features = ["derive"] }

[profile.release]
lto = true
EOF
want_case satisfied      pass "${ok_dir}" "All 3 internal row(s)"
want_case ok_0x_patch    pass "${ok_dir}" "trusty-code            req 0.5.0"
want_case ok_partial_req pass "${ok_dir}" "trusty-common          req 0.47"
want_case ok_major_range pass "${ok_dir}" "trusty-mpm             req 1.0.0"

# ── drift_0x: the exact #6776 shape ──────────────────────────────────────────
d0="${TMPROOT}/drift_0x"
mkdir -p "${d0}"
member "${d0}" trusty-console trusty-console 0.11.0
cat >"${d0}/Cargo.toml" <<'EOF'
[workspace.dependencies]
trusty-console = { path = "crates/trusty-console", version = "0.9.0" }
EOF
want_case drift_0x fail "${d0}" 'version = "0.11"'

# ── drift_major: a major-version bump left behind ────────────────────────────
dm="${TMPROOT}/drift_major"
mkdir -p "${dm}"
member "${dm}" trusty-git-analytics tga 7.1.0
cat >"${dm}/Cargo.toml" <<'EOF'
[workspace.dependencies]
tga = { path = "crates/trusty-git-analytics", version = "6.0.1" }
EOF
# The 0.y.z note must NOT appear for a 7.x crate.
want_case drift_major fail "${dm}" 'version = "7.1"'
if [ -z "${only}" ] || [ "${only}" = "drift_major_no_0x_note" ]; then
  out="$(WS_DEP_MANIFEST="${dm}/Cargo.toml" WS_DEP_MIN_ROWS=1 bash "${GATE}" 2>&1)"
  if printf '%s' "${out}" | grep -qF '0.y.z'; then
    bad drift_major_no_0x_note "the 0.y.z hint leaked onto a 7.x crate: ${out}"
  else
    pass drift_major_no_0x_note
  fi
fi

# ── alias_key: the row key is an alias; the DIRECTORY decides the member ─────
# `tga` -> crates/trusty-git-analytics is the real shape in this workspace. A
# parser that derived the member from the row NAME would read no manifest here
# and must not silently pass.
ak="${TMPROOT}/alias_key"
mkdir -p "${ak}"
member "${ak}" trusty-git-analytics tga 7.1.0
cat >"${ak}/Cargo.toml" <<'EOF'
[workspace.dependencies]
tga = { path = "crates/trusty-git-analytics", version = "6.0.1" }
EOF
want_case alias_key fail "${ak}" 'crates/trusty-git-analytics is 7.1.0'

# ── missing_member: unverifiable row must fail closed ────────────────────────
mm="${TMPROOT}/missing_member"
mkdir -p "${mm}"
cat >"${mm}/Cargo.toml" <<'EOF'
[workspace.dependencies]
trusty-gone = { path = "crates/trusty-gone", version = "0.1.0" }
EOF
want_case missing_member fail "${mm}" 'cannot read [package] version'

# ── bad_operator: a requirement shape the gate does not model ────────────────
bo="${TMPROOT}/bad_operator"
mkdir -p "${bo}"
member "${bo}" trusty-thing trusty-thing 0.4.0
cat >"${bo}/Cargo.toml" <<'EOF'
[workspace.dependencies]
trusty-thing = { path = "crates/trusty-thing", version = ">=0.1, <0.5" }
EOF
want_case bad_operator fail "${bo}" 'unmodelled version shape'

# ── scan_floor: a table with no internal rows must not report success ────────
sf="${TMPROOT}/scan_floor"
mkdir -p "${sf}"
cat >"${sf}/Cargo.toml" <<'EOF'
[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
EOF
want_case scan_floor fail "${sf}" 'SCAN FLOOR'

# ── the live repo must pass its own gate ─────────────────────────────────────
if [ -z "${only}" ] || [ "${only}" = "live_repo" ]; then
  out="$(cd "${REPO_ROOT}" && bash "${GATE}" 2>&1)"
  status=$?
  if [ "${status}" -ne 0 ]; then
    bad live_repo "the live workspace manifest does not pass. Output: ${out}"
  else
    pass live_repo
  fi
fi

if [ "${failures}" -gt 0 ]; then
  printf '\n%d selftest case(s) failed.\n' "${failures}" >&2
  exit 1
fi
printf '\nAll selftest cases passed.\n'
