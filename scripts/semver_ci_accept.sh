#!/usr/bin/env bash
#
# semver_ci_accept.sh — apply preflight CHECK 5's accepted-break decision to a
# check_semver.sh BREAK in the Public API / SemVer PR check (#8372).
#
# Why: the owner ruling of 2026-09-22 lets a 1.x release ship an unbumped break
#   when a committed declaration lists it. PR #8425 taught preflight CHECK 5 that
#   rule, but .github/workflows/semver-checks.yml read check_semver.sh's exit 1
#   alone, so a release PR the owner accepted (PR #8427, trusty-mpm 1.7.0) could
#   never merge. This script is the CI half. The rule itself lives only in
#   scripts/lib/semver_accepted_breaks.sh, which both callers source.
#
# What: reads one check_semver.sh run (its exit status and combined output) that
#   may cover several crates, and exits 0 only when ALL of these hold:
#     - the gate exited 1 (a computed BREAK) and printed no NO VERDICT or NO
#       INVENTORY line, so every selected crate was compared;
#     - every `FAIL <crate>: cargo semver-checks exited` line names a distinct
#       crate, and no other `FAIL ` line exists;
#     - for each such crate, its manifest at --rev gives a version, and
#       semver_accept_decide accepts the crate's own `CHECK`..`FAIL` section with
#       the declaration read from --rev's tree.
#   A crate with no declaration, and every refusal the library makes, leaves the
#   BREAK standing (exit 1). Each accepted crate prints the library's
#   `[WARN] semver: ACCEPTED BREAK` block and a `::warning` annotation line.
#
#   --rev is the commit whose tree holds the declaration and the manifest. In the
#   PR check it is the PR head commit, not the merge commit the job checks out;
#   the library still requires the working-tree copy to equal that blob.
#
#   A declaration counts only once it is on main (#8372), so a PR cannot accept
#   its own break. --merge <commit> (a PR run) names the checked-out merge
#   commit: its second parent must be --rev, and the declaration must already
#   sit on its first parent — the base — with the same mode and blob as on
#   --rev. --main <ref> (tag push, workflow_dispatch) requires --rev to be the
#   checked-out commit and an ancestor of <ref>, so the file it holds is already
#   on main. Exactly one of the two is required.
#
# Usage: scripts/semver_ci_accept.sh [--rev <commit>] (--merge <commit> | --main <ref>)
#          <gate-exit> <gate-log>
# Exit:  0 every break accepted; 1 the BREAK stands; 2 usage error.
#
# Test: scripts/check_semver_selftest.sh, the `ci-accept/` cases.
#
# Portability: bash 3.2 and bash 5; BSD and GNU sed/awk.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
# A full run: never the --check-only preview of an uncommitted working copy.
CHECK_ONLY=0

usage() {
  echo "usage: semver_ci_accept.sh [--rev <commit>] (--merge <commit> | --main <ref>) <gate-exit> <gate-log>" >&2
  exit 2
}
rev="HEAD"
merge=""
main_ref=""
while [ "$#" -gt 2 ]; do
  case "$1" in
    --rev) rev="$2" ;;
    --merge) merge="$2" ;;
    --main) main_ref="$2" ;;
    *) usage ;;
  esac
  shift 2
done
# Exactly one context: a PR run names its merge commit, any other run the
# branch its checkout must already be on. Neither, or both, decides nothing.
if [ "$#" -ne 2 ] || [ -z "$rev" ] || { [ -z "$merge" ] && [ -z "$main_ref" ]; } \
  || { [ -n "$merge" ] && [ -n "$main_ref" ]; }; then
  usage
fi
gate_rc="$1"
gate_log="$2"
if [ ! -f "$gate_log" ]; then
  echo "semver_ci_accept: no gate log at ${gate_log}" >&2
  exit 2
fi

# shellcheck source-path=SCRIPTDIR source=lib/semver_accepted_breaks.sh
. "${SCRIPT_DIR}/lib/semver_accepted_breaks.sh"
if ! SEMVER_ACCEPT_REV="$(git -C "$REPO_ROOT" rev-parse --verify --quiet "${rev}^{commit}")"; then
  echo "[FAIL] semver: --rev '${rev}' is not a commit in this checkout, so no" >&2
  echo "       declaration can be read. The BREAK stands." >&2
  exit 1
fi

# The base the declaration must already be on (#8372). A PR run: the merge
# commit's first parent, which is the base-branch commit this checkout merges
# into — the event's base.sha is a snapshot that can lag it (#4688). Its second
# parent must be --rev, the PR head, or this is not that PR's merge. Any other
# run: the checked-out commit itself, which must already be on --main.
base=""
if [ -n "$merge" ]; then
  parents="$(git -C "$REPO_ROOT" rev-list --parents -n 1 "${merge}^{commit}" -- 2> /dev/null)"
  read -r _ p1 p2 extra <<< "$parents"
  if [ -z "${p2:-}" ] || [ -n "${extra:-}" ] || [ "$p2" != "$SEMVER_ACCEPT_REV" ]; then
    echo "[FAIL] semver: --merge '${merge}' is not a two-parent merge whose second parent is the" >&2
    echo "       PR head ${SEMVER_ACCEPT_REV}, so the base branch a declaration must already" >&2
    echo "       be on is unknown. The BREAK stands." >&2
    exit 1
  fi
  base="$p1"
else
  checked_out="$(git -C "$REPO_ROOT" rev-parse --verify --quiet 'HEAD^{commit}')"
  if [ "$SEMVER_ACCEPT_REV" != "$checked_out" ] \
    || ! git -C "$REPO_ROOT" merge-base --is-ancestor "$SEMVER_ACCEPT_REV" "$main_ref" 2> /dev/null; then
    echo "[FAIL] semver: with no pull request, a declaration counts only at the checked-out" >&2
    echo "       commit, and only once that commit is on ${main_ref}. --rev ${SEMVER_ACCEPT_REV}" >&2
    echo "       is not both, so this run could accept a declaration main never reviewed." >&2
    echo "       The BREAK stands." >&2
    exit 1
  fi
  echo "semver_ci_accept: no pull request context — the declaration is read from the checked-out" >&2
  echo "       commit ${SEMVER_ACCEPT_REV}, which is already on ${main_ref}." >&2
fi

if [ "$gate_rc" != "1" ]; then
  echo "[FAIL] semver: check_semver.sh exited ${gate_rc}. A declaration accepts only a" >&2
  echo "       computed BREAK (exit 1), never a gate that could not compare." >&2
  exit 1
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/semver-ci-accept.XXXXXX")"
trap 'rm -rf "$work"' EXIT
esc="$(printf '\033')"
sed "s/${esc}\[[0-9;]*m//g" "$gate_log" > "${work}/clean"

if grep -Eq '^NO (VERDICT|INVENTORY) ' "${work}/clean"; then
  echo "[FAIL] semver: this run also reported NO VERDICT or NO INVENTORY, so part of it" >&2
  echo "       compared nothing. A declaration never covers a blind gate:" >&2
  grep -E '^NO (VERDICT|INVENTORY) ' "${work}/clean" | sed 's/^/         /' >&2
  exit 1
fi

fail_re='^FAIL ([A-Za-z0-9_-]+): cargo semver-checks exited [0-9]+ against baseline '
grep '^FAIL ' "${work}/clean" > "${work}/fails" || true
if [ ! -s "${work}/fails" ] || grep -Evq "$fail_re" "${work}/fails"; then
  echo "[FAIL] semver: the gate's FAIL lines could not be read as one break per crate," >&2
  echo "       so no declaration is checked against them:" >&2
  sed 's/^/         /' "${work}/fails" >&2
  exit 1
fi
sed -E "s/${fail_re}.*/\1/" "${work}/fails" > "${work}/crates"
if [ -n "$(sort "${work}/crates" | uniq -d)" ]; then
  echo "[FAIL] semver: a crate has more than one FAIL line, so its break list is ambiguous." >&2
  exit 1
fi

# manifest_version <package> — the `version` of the one crates/*/Cargo.toml at
# $SEMVER_ACCEPT_REV whose first `name` is <package>; empty when not exactly one.
manifest_version() {
  local pkg="$1" path body found="" n=0
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    body="$(git -C "$REPO_ROOT" show "${SEMVER_ACCEPT_REV}:${path}" 2> /dev/null)" || continue
    [ "$(printf '%s\n' "$body" | grep -m1 -E '^name[[:space:]]*=' \
      | sed -E 's/^name[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/')" = "$pkg" ] || continue
    n=$((n + 1))
    found="$(printf '%s\n' "$body" \
      | grep -m1 -E '^version[[:space:]]*=[[:space:]]*"[0-9]+\.[0-9]+\.[0-9]+"' \
      | sed -E 's/^version[[:space:]]*=[[:space:]]*"([0-9]+\.[0-9]+\.[0-9]+)".*/\1/')"
  done <<EOF
$(git -C "$REPO_ROOT" ls-tree --name-only "$SEMVER_ACCEPT_REV" -- crates/ 2> /dev/null \
    | sed 's|$|/Cargo.toml|')
EOF
  [ "$n" -eq 1 ] && printf '%s' "$found"
}

refused=0
# fd 3, so nothing inside the loop can consume the crate list from stdin.
while IFS= read -r pkg <&3; do
  version="$(manifest_version "$pkg")"
  if [ -z "$version" ]; then
    echo "[FAIL] semver: ${pkg} breaks its public API, and no single crates/*/Cargo.toml" >&2
    echo "       at ${SEMVER_ACCEPT_REV} names it with a version, so no declaration applies." >&2
    refused=1
    continue
  fi
  rel="$(semver_accept_rel "$pkg" "$version")"
  if ! semver_accept_present "$rel"; then
    echo "[FAIL] semver: ${pkg} ${version} breaks its public API, and ${rel}" >&2
    echo "       does not exist at ${SEMVER_ACCEPT_REV}. The BREAK stands." >&2
    refused=1
    continue
  fi
  # #8372: a PR cannot accept its own break. Same mode and blob on the base as
  # on the head, or nothing is accepted.
  if [ -n "$base" ]; then
    at_base="$(git -C "$REPO_ROOT" ls-tree "$base" -- "$rel" 2> /dev/null)"
    if [ -z "$at_base" ]; then
      echo "[FAIL] semver: ${pkg} ${version} breaks its public API, and ${rel}" >&2
      echo "       exists only on this PR, not on the base branch at ${base}. A PR cannot" >&2
      echo "       accept its own break: land the declaration on main in its own PR first." >&2
      refused=1
      continue
    elif [ "$at_base" != "$(git -C "$REPO_ROOT" ls-tree "$SEMVER_ACCEPT_REV" -- "$rel" 2> /dev/null)" ]; then
      echo "[FAIL] semver: ${pkg} ${version} breaks its public API, and this PR changes ${rel}" >&2
      echo "       from the copy on the base branch at ${base}. A PR cannot rewrite the" >&2
      echo "       declaration that accepts its break: land the declaration on main in its own PR first." >&2
      refused=1
      continue
    fi
    echo "semver_ci_accept: ${rel} is on the base branch at ${base}, and this PR leaves it unchanged." >&2
  fi
  # This crate's own run: its CHECK line through its FAIL line. The library
  # refuses a section holding any other crate's CHECK or lint summary.
  awk -v c="CHECK ${pkg}: " -v f="FAIL ${pkg}: " '
    index($0, c) == 1 { on = 1 }
    on { print }
    on && index($0, f) == 1 { exit }
  ' "${work}/clean" > "${work}/section"
  if ! semver_accept_decide "${work}/section" "$pkg" "$version" 2> "${work}/decide"; then
    cat "${work}/decide" >&2
    refused=1
    continue
  fi
  cat "${work}/decide" >&2
  # One annotation line per crate, printed only if every crate is accepted.
  # `%` is the workflow-command escape character.
  printf '::warning title=SemVer ACCEPTED BREAK::%s\n' "$(sed -n '1,4p' "${work}/decide" \
    | sed 's/^\[WARN\] semver: //; s/^ *//' | tr '\n' ' ' | sed 's/%/%25/g; s/ *$//')" \
    >> "${work}/annotations"
done 3< "${work}/crates"

if [ "$refused" -ne 0 ]; then
  echo "semver_ci_accept: the BREAK stands — see docs/reference/semver-gate.md, \"Accepted breaks\"." >&2
  exit 1
fi
cat "${work}/annotations"
echo "semver_ci_accept: every break in this run is accepted by a committed declaration at ${SEMVER_ACCEPT_REV}."
exit 0
