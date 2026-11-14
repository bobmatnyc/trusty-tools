#!/usr/bin/env bash
#
# check_bundle_pr_current_selftest.sh — fixtures for
# scripts/check_bundle_pr_current.sh (issue #11120).
#
# Why: the gate passes most PRs. A gate that passes everything satisfies every
#   pass case too, so each FAIL case below is a state it must refuse: a bundle
#   PR behind main, including one that renames, deletes, or adds a bundle file
#   whose path git would quote (non-ASCII, a space).
# What: builds a scratch repo with a `main` that moves on after each branch is
#   cut, then asserts the gate's exit status, and the remedy text on a FAIL,
#   for each case. The --fetch cases (the workflow's CI mode) run in a clone
#   whose `origin` is that scratch repo, after origin's main moves again. CHECK_BUNDLE_PR_CURRENT=<path> runs the cases against
#   another implementation.
# Test: this IS the test. Run: bash scripts/check_bundle_pr_current_selftest.sh

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="${CHECK_BUNDLE_PR_CURRENT:-${HERE}/check_bundle_pr_current.sh}"
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1

W="${TMP}/work"
g() { git -C "${W}" "$@" >/dev/null 2>&1; }
put() { mkdir -p "$(dirname "${W}/$1")"; echo "$2" > "${W}/$1"; g add "$1"; }
commit() { g commit -q --allow-empty -m "$1"; }
branch() { g checkout -q -b "$1" "${2:-main}"; }

BUNDLE=python/trusty-architect/fleet_poll.py
E_ACUTE="$(printf '\303\251')"   # U+00E9; git C-quotes it as "\303\251"
git init -q -b main "${W}"
put README.md base; put docs/note.md note; put "${BUNDLE}" v1
put crates/trusty-mpm/src/bin/tm/commands/fleet/seed_history.tsv h1
commit base
OLD_MAIN="$(git -C "${W}" rev-parse main)"

branch plain;      put docs/note.md edited;                    commit plain
branch bundle;     put "${BUNDLE}" v2;                          commit bundle
branch tsv;        put crates/trusty-mpm/src/bin/tm/commands/fleet/seed_history.tsv h2; commit tsv
branch near;       put python/trusty-architect-other/x.py x;    commit near
branch rename_in;  g mv docs/note.md python/trusty-architect/note.md; commit rename_in
branch rename_out; g mv "${BUNDLE}" docs/fleet_poll.py;         commit rename_out
branch empty;      commit empty
branch deleted;    g rm -q "${BUNDLE}";                          commit deleted
branch nonascii;   put "python/trusty-architect/${E_ACUTE}.py" x;  commit nonascii
branch spaced;     put "python/trusty-architect/a b.py" x;       commit spaced
branch outside_na; put "docs/${E_ACUTE}.md" x;                     commit outside_na
branch pairs;      put scripts/check_architect_subproject.sh x;  commit pairs
g checkout -q main; put README.md moved; commit "main moves"
branch current;    put "${BUNDLE}" v3;                          commit current
branch merged bundle; g merge -q --no-edit main
g checkout -q main

PASS=0
FAIL=0
OUT="${TMP}/out"
# expect <case> <want-exit> <grep-on-output-or-empty> -- <gate args...>
expect() {
  local name="$1" want="$2" needle="$3"; shift 4
  local rc=0
  bash "${GATE}" --repo "${REPO:-${W}}" "$@" > "${OUT}" 2>&1 || rc=$?
  if [ "${rc}" -ne "${want}" ]; then
    echo "FAIL ${name}: exit ${rc}, want ${want}"; sed 's/^/    /' "${OUT}"; FAIL=$((FAIL + 1)); return
  fi
  if [ -n "${needle}" ] && ! grep -qF -- "${needle}" "${OUT}"; then
    echo "FAIL ${name}: output lacks '${needle}'"; sed 's/^/    /' "${OUT}"; FAIL=$((FAIL + 1)); return
  fi
  echo "ok   ${name}"; PASS=$((PASS + 1))
}

REMEDY="python3 scripts/architect_bundle_meta.py history --check"
expect "no bundle file, behind main -> pass"       0 "PASS: no Architect" -- --head plain --base main
expect "bundle file, behind main -> fail"          1 "${REMEDY}"           -- --head bundle --base main
expect "fail names the merge step"                 1 "git merge origin/main" -- --head bundle --base main
expect "fail lists the bundle file"                1 "  ${BUNDLE}"         -- --head bundle --base main
expect "seed_history.tsv, behind main -> fail"     1 "FAIL BEHIND"         -- --head tsv --base main
expect "bundle file, head contains main -> pass"   0 "contains main"       -- --head current --base main
expect "bundle file, main merged in -> pass"       0 "contains main"       -- --head merged --base main
expect "rename into the bundle, behind -> fail"    1 "  python/trusty-architect/note.md" -- --head rename_in --base main
expect "rename out of the bundle, behind -> fail"  1 "  ${BUNDLE}"         -- --head rename_out --base main
expect "prefix look-alike dir, behind -> pass"     0 "PASS: no Architect" -- --head near --base main
expect "empty diff, behind main -> pass"           0 "PASS: no Architect" -- --head empty --base main
expect "head is main's tip -> pass"                0 "PASS: no Architect" -- --head main --base main
expect "head is an old main commit -> pass"        0 "PASS: no Architect" -- --head "${OLD_MAIN}" --base main
expect "deleted bundle file, behind -> fail"       1 "  ${BUNDLE}"         -- --head deleted --base main
expect "non-ASCII bundle path, behind -> fail"     1 "  python/trusty-architect/${E_ACUTE}.py"         -- --head nonascii --base main
expect "spaced bundle path, behind -> fail"        1 "  python/trusty-architect/a b.py" -- --head spaced --base main
expect "non-ASCII path outside bundle -> pass"     0 "PASS: no Architect" -- --head outside_na --base main
expect "pair-table script, behind -> fail"         1 "  scripts/check_architect_subproject.sh" -- --head pairs --base main
expect "--range, rename out, behind -> fail"       1 "FAIL BEHIND"         -- --head rename_out --base main --range "${OLD_MAIN}..rename_out"

printf 'docs/note.md\n%s\n' "${BUNDLE}" > "${TMP}/files"
: > "${TMP}/none"
expect "--files with a bundle path, behind -> fail" 1 "FAIL BEHIND"        -- --head plain --base main --files "${TMP}/files"
expect "--files empty list -> pass"                 0 "PASS: no Architect" -- --head bundle --base main --files "${TMP}/none"
expect "unknown head -> error"                      2 "does not resolve"   -- --head 0000000000000000000000000000000000000000 --base main
expect "unknown base -> error"                      2 "does not resolve"   -- --head bundle --base origin/nope
expect "missing --base -> usage"                    2 "usage:"             -- --head bundle
expect "--files and --range together -> usage"      2 "usage:"             -- --head bundle --base main --files "${TMP}/none" --range a..b

# CI mode fixtures: a clone whose origin is W. A PR head lives only under
# refs/pull/N/head, which a clone does not fetch, so --fetch must fetch it.
C="${TMP}/clone"
git clone -q "${W}" "${C}" >/dev/null 2>&1
CUR_TIP="$(git -C "${W}" rev-parse current)"
g checkout -q --detach main; put "${BUNDLE}" v4; commit pr7
PR7="$(git -C "${W}" rev-parse HEAD)"; g update-ref refs/pull/7/head "${PR7}"
g checkout -q main; put README.md moved-again; commit "main moves again"
g checkout -q --detach main; put "${BUNDLE}" v5; commit pr8
PR8="$(git -C "${W}" rev-parse HEAD)"; g update-ref refs/pull/8/head "${PR8}"
g checkout -q main

REPO="${C}"
expect "--fetch, base branch missing -> error"     2 "cannot fetch branch" -- --fetch origin --head "${PR8}" --base nope
expect "--fetch, head SHA missing -> error"        2 "cannot fetch head"   -- --fetch origin --head 0000000000000000000000000000000000000000 --base main
expect "--fetch, short head -> error"              2 "full SHA"            -- --fetch origin --head "${PR8:0:12}" --base main
expect "--fetch, option-shaped head -> error"      2 "hex SHA"             -- --fetch origin --head --upload-pack=x --base main
expect "--fetch, head behind refreshed main -> fail" 1 "FAIL BEHIND"       -- --fetch origin --head "${PR7}" --base main
expect "--fetch, stale-current head -> fail"       1 "FAIL BEHIND"         -- --fetch origin --head "${CUR_TIP}" --base main
expect "--fetch, head contains main -> pass"       0 "contains refs/remotes/origin/main" -- --fetch origin --head "${PR8}" --base main
unset REPO

echo "check_bundle_pr_current selftest: ${PASS} passed, ${FAIL} failed"
[ "${FAIL}" -eq 0 ]
