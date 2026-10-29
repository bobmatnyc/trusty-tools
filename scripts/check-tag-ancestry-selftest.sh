#!/usr/bin/env bash
#
# check-tag-ancestry-selftest.sh — fixtures for scripts/check-tag-ancestry.sh.
#
# Why: the D96 rule (#10388) adds a second passing ref family,
#   release/<crate>-<semver>. A widened gate is only safe if its failing
#   branches are exercised: a gate that passes everything satisfies the pass
#   cases too. Each FAIL case below is a state the gate must refuse, including
#   a release branch whose tag does not match its Cargo.toml or its tip.
# What: builds a bare "origin" plus a clone in a temp dir, places commits on
#   main, release/* branches, a feature branch and a detached commit, then
#   asserts the exit status of the gate per case.
#   CHECK_TAG_ANCESTRY=<path> runs the cases against another implementation
#   (used to show new cases are red under an older rule).
# Test: this IS the test. Run: bash scripts/check-tag-ancestry-selftest.sh

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="${CHECK_TAG_ANCESTRY:-${HERE}/check-tag-ancestry.sh}"
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1

W="${TMP}/work"
g() { git -C "${W}" "$@" >/dev/null 2>&1; }
commit() { # <msg> [<crate> <version>]
  echo "$1" > "${W}/f"; g add f
  if [[ $# -ge 3 ]]; then
    mkdir -p "${W}/crates/$2"
    printf '[package]\nname = "%s"\nversion = "%s"\n\n[dependencies]\nx = { version = "9.9.9" }\n' "$2" "$3" \
      > "${W}/crates/$2/Cargo.toml"
    g add "crates/$2/Cargo.toml"
  fi
  g commit -qm "$1"
}
branch() { g checkout -q -b "$1" main; }

git init -q --bare -b main "${TMP}/origin.git"
git init -q -b main "${W}"
g remote add origin "${TMP}/origin.git"
commit base;            g push -q origin main
branch release/trusty-mpm-1.9.0; commit rel trusty-mpm 1.9.0; g tag trusty-mpm-v1.9.0
g push -q origin release/trusty-mpm-1.9.0
branch release/d66;  commit d66 d66 1.0.0;  g tag d66-v1.0.0;  g push -q origin release/d66
branch release/2026-07-07-workspace; commit ws ws 1.0.0; g tag ws-v1.0.0
g push -q origin release/2026-07-07-workspace
branch feature/x;    commit feat feat 1.0.0; g tag feat-v1.0.0; g push -q origin feature/x
g checkout -q --detach main; commit detached; g tag detached-v1.0.0
# Version mismatch: tag says 1.2.3, Cargo.toml says 1.2.4.
branch release/trusty-x-1.2.3; commit vx trusty-x 1.2.4; g tag trusty-x-v1.2.3
g push -q origin release/trusty-x-1.2.3
# Non-tip: tagged commit then one more commit on the release branch.
branch release/trusty-n-1.0.0; commit n1 trusty-n 1.0.0; g tag trusty-n-v1.0.0; commit n2
g push -q origin release/trusty-n-1.0.0
# Wrong crate: tip of a qualifying branch, tag names a crate with no dir / other version.
branch release/trusty-w-1.0.0; commit w trusty-w 1.0.0 ; commit w2 trusty-z 2.0.0
g tag trusty-y-v1.0.0; g tag trusty-z-v1.0.0; g push -q origin release/trusty-w-1.0.0
# Pre-release branch and tag.
branch release/trusty-r-2.0.0-rc.1; commit r trusty-r 2.0.0-rc.1; g tag trusty-r-v2.0.0-rc.1
g push -q origin release/trusty-r-2.0.0-rc.1
# Deleted remote release branch: a clone fetched it, then origin deleted it.
branch release/trusty-d-1.0.0; commit d trusty-d 1.0.0; g tag trusty-d-v1.0.0
g push -q origin release/trusty-d-1.0.0
g checkout -q main;     commit mainmore; g tag main-v1.0.0; g push -q origin main
g push -q origin --tags   # clones below must resolve tags, or a case fails for the wrong reason
git clone -q "${TMP}/origin.git" "${TMP}/stale" 2>/dev/null
git -C "${TMP}/stale" fetch -q "${TMP}/origin.git" "refs/tags/*:refs/tags/*" 2>/dev/null
g push -q origin --delete release/trusty-d-1.0.0

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
t() { expect "$1" "$2" "${W}" "$3" --tag "$3" "${@:4}"; }

t "pass on main"                                0 main-v1.0.0
t "pass on release/trusty-mpm-1.9.0"            0 trusty-mpm-v1.9.0
t "FAIL on release/d66"                         1 d66-v1.0.0
t "FAIL on release/2026-07-07-workspace"        1 ws-v1.0.0
t "FAIL on feature branch"                      1 feat-v1.0.0
t "FAIL on detached commit"                     1 detached-v1.0.0
expect "FAIL on unresolvable commit"            1 "${W}" no-such-tag
expect "FAIL when fetch fails"                  1 "${TMP}/nofetch" origin/main
expect "FAIL when origin/main ref is missing"   1 "${TMP}/noref" main-v1.0.0 --no-fetch
t "--at-tip pass at release branch tip"         0 trusty-mpm-v1.9.0 --at-tip
expect "--at-tip FAIL behind main tip"          1 "${W}" main~1 --at-tip
t "FAIL version mismatch (Cargo.toml 1.2.4)"    1 trusty-x-v1.2.3
t "FAIL non-tip commit on release branch"       1 trusty-n-v1.0.0
t "FAIL wrong crate (no crates/trusty-y)"       1 trusty-y-v1.0.0
t "FAIL wrong crate (trusty-z is 2.0.0)"        1 trusty-z-v1.0.0
expect "FAIL release tip with no identity"      1 "${W}" trusty-mpm-v1.9.0
expect "pass with --crate/--version"            0 "${W}" trusty-mpm-v1.9.0 --crate trusty-mpm --version 1.9.0
t "pass on rc release branch"                   0 trusty-r-v2.0.0-rc.1
expect "FAIL on deleted remote release branch (--prune)" 1 "${TMP}/stale" trusty-d-v1.0.0 --tag trusty-d-v1.0.0

echo "check-tag-ancestry-selftest: ${pass} passed, ${failn} failed"
[[ "${failn}" -eq 0 ]]
