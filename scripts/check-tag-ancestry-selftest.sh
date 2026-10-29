#!/usr/bin/env bash
#
# check-tag-ancestry-selftest.sh — fixtures for scripts/check-tag-ancestry.sh.
#
# Why: the D96 rule (#10388) adds a second passing ref family,
#   release/<crate>-<semver>. A widened gate is only safe if its failing
#   branches are exercised: a gate that passes everything satisfies the pass
#   cases too. Each FAIL case below is a state the gate must refuse.
# What: builds a bare "origin" plus a clone in a temp dir, places commits on
#   main, release/trusty-mpm-1.9.0, release/d66, a feature branch and a
#   detached commit, then asserts the exit status of the gate per case.
#   CHECK_TAG_ANCESTRY=<path> runs the cases against another implementation
#   (used to show the release-branch case is red under the old main-only rule).
# Test: this IS the test. Run: bash scripts/check-tag-ancestry-selftest.sh

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="${CHECK_TAG_ANCESTRY:-${HERE}/check-tag-ancestry.sh}"
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1

g() { git -C "${TMP}/work" "$@" >/dev/null 2>&1; }
commit() { echo "$1" > "${TMP}/work/f"; g add f; g commit -qm "$1"; }

git init -q --bare -b main "${TMP}/origin.git"
git init -q -b main "${TMP}/work"
g remote add origin "${TMP}/origin.git"
commit base;            g push -q origin main
g checkout -q -b release/trusty-mpm-1.9.0; commit rel; g tag trusty-mpm-v1.9.0
g push -q origin release/trusty-mpm-1.9.0
g checkout -q -b release/d66 main;  commit d66;  g tag d66-v1.0.0;  g push -q origin release/d66
g checkout -q -b release/2026-07-07-workspace main; commit ws; g tag ws-v1.0.0
g push -q origin release/2026-07-07-workspace
g checkout -q -b feature/x main;    commit feat; g tag feat-v1.0.0; g push -q origin feature/x
g checkout -q --detach main;        commit detached; g tag detached-v1.0.0
g checkout -q main;     commit mainmore; g tag main-v1.0.0; g push -q origin main

# A clone whose remote is gone: the fetch fails, so the gate must fail.
git clone -q "${TMP}/origin.git" "${TMP}/nofetch" 2>/dev/null
git -C "${TMP}/nofetch" remote set-url origin "${TMP}/does-not-exist.git"
# A clone with no remote-tracking main and fetch disabled: missing ref.
git init -q -b main "${TMP}/noref"
git -C "${TMP}/noref" fetch -q "${TMP}/origin.git" "refs/tags/*:refs/tags/*" 2>/dev/null

pass=0; failn=0
expect() { # <name> <want-exit> <repo> <commit> [extra args...]
  local name="$1" want="$2" repo="$3" ref="$4"; shift 4
  local got=0
  bash "${GATE}" --repo "${repo}" "$@" "${ref}" >"${TMP}/out" 2>&1 || got=$?
  if [[ "${got}" == "${want}" ]]; then
    echo "ok   ${name} (exit ${got})"; pass=$((pass + 1))
  else
    echo "FAIL ${name}: want exit ${want}, got ${got}"; sed 's/^/     /' "${TMP}/out"; failn=$((failn + 1))
  fi
}

W="${TMP}/work"
expect "pass on main"                              0 "${W}" main-v1.0.0
expect "pass on release/trusty-mpm-1.9.0"          0 "${W}" trusty-mpm-v1.9.0
expect "FAIL on release/d66"                       1 "${W}" d66-v1.0.0
expect "FAIL on release/2026-07-07-workspace"      1 "${W}" ws-v1.0.0
expect "FAIL on feature branch"                    1 "${W}" feat-v1.0.0
expect "FAIL on detached commit"                   1 "${W}" detached-v1.0.0
expect "FAIL on unresolvable commit"               1 "${W}" no-such-tag
expect "FAIL when fetch fails"                     1 "${TMP}/nofetch" origin/main
expect "FAIL when origin/main ref is missing"      1 "${TMP}/noref" main-v1.0.0 --no-fetch
expect "--at-tip pass at release branch tip"       0 "${W}" trusty-mpm-v1.9.0 --at-tip
expect "--at-tip FAIL behind main tip"             1 "${W}" main~1 --at-tip

echo "check-tag-ancestry-selftest: ${pass} passed, ${failn} failed"
[[ "${failn}" -eq 0 ]]
