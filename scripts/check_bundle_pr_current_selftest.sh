#!/usr/bin/env bash
#
# check_bundle_pr_current_selftest.sh — fixtures for
# scripts/check_bundle_pr_current.sh (issue #11120).
#
# Why: the gate passes most PRs. A gate that passes everything satisfies every
#   pass case too, so each FAIL case below is a state it must refuse: a bundle
#   PR that lacks a bundle change main has, including one that renames,
#   deletes, or adds a bundle file whose path git would quote (non-ASCII, a
#   space), on either side. The tip-rule gate fails the pass cases where a
#   bundle PR lacks only non-bundle commits from main.
# What: builds a scratch repo whose `main` moves on, by a non-bundle and a
#   bundle commit, after each branch is cut, plus m_* bases that each add only
#   the commits a case names, then asserts the gate's exit status, and the
#   remedy text on a FAIL, for each case. The --fetch cases (the workflow's CI
#   mode) run in a clone whose `origin` is that scratch repo, after origin's
#   main moves again. CHECK_BUNDLE_PR_CURRENT=<path> runs the cases against
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
TSV=crates/trusty-mpm/src/bin/tm/commands/fleet/seed_history.tsv
REV=python/trusty-architect/reverted.py
put "${TSV}" h1; put "${REV}" r1
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
# main moves on by a non-bundle commit and a bundle commit, so every branch
# above lacks a bundle change.
g checkout -q main; put README.md moved; commit "main moves"
put python/trusty-architect/main_side.py m1; commit "main changes the bundle"
branch current;    put "${BUNDLE}" v3;                          commit current
branch merged bundle; g merge -q --no-edit main

# Isolated bases for the narrowed rule: each m_* branch is OLD_MAIN plus only
# the commits named, and the `bundle` branch (OLD_MAIN + a bundle edit) is
# judged against it, so the commits named are all it lacks.
branch m_nb1 "${OLD_MAIN}"; put README.md nb1; commit "nb1 docs"
branch m_dir "${OLD_MAIN}"; put python/trusty-architect/main_side.py d; commit "dir bundle"
branch m_tsv "${OLD_MAIN}"; put "${TSV}" h9; commit "tsv bundle"
branch m_sev "${OLD_MAIN}"; put README.md s1; commit "sev docs 1"
put docs/a.md a; commit "sev docs 2"
put crates/trusty-mpm/src/bin/tm/commands/fleet/seed_table.rs t; commit "sev bundle commit"
put docs/b.md b; commit "sev docs 3"
branch m_rev "${OLD_MAIN}"; put "${REV}" r2; commit "rev change"
g revert --no-edit HEAD
branch m_renin "${OLD_MAIN}"; g mv docs/note.md python/trusty-architect/moved.md; commit "rename into bundle"
branch m_renout "${OLD_MAIN}"; g mv "${REV}" docs/reverted.py; commit "rename out of bundle"
branch m_del "${OLD_MAIN}"; g rm -q "${REV}"; commit "delete bundle file"
branch m_na "${OLD_MAIN}"; put "python/trusty-architect/m${E_ACUTE}.py" x; commit "non-ASCII bundle"
branch m_sp "${OLD_MAIN}"; put "python/trusty-architect/m b.py" x; commit "spaced bundle"
# A side branch that changed the bundle, merged into main: the side commit is
# what the head lacks.
branch side "${OLD_MAIN}"; put python/trusty-architect/side.py s; commit "side bundle"
branch m_mrg "${OLD_MAIN}"; put README.md mrg; commit "mrg docs"
g merge -q --no-ff --no-edit side
# A merge whose resolution itself edits a bundle file ("evil merge").
branch side2 "${OLD_MAIN}"; put docs/c.md c; commit "side2 docs"
branch m_evil "${OLD_MAIN}"; put README.md evil; commit "evil docs"
g merge -q --no-ff --no-commit side2; put python/trusty-architect/evil.py e; commit "evil merge"
# Criss-cross: X and Y each merge the other's first commit, so head X2 and
# base Y2 have two merge bases (X1 and Y1).
branch x1 "${OLD_MAIN}"; put "${BUNDLE}" x1; commit x1
branch y1 "${OLD_MAIN}"; put README.md y1; commit y1
branch x2 x1; g merge -q --no-edit y1
branch y2 y1; g merge -q --no-edit x1
branch y3 y2; put python/trusty-architect/y3.py y; commit "y3 bundle"
g checkout -q --orphan orphan; put README.md orphan; commit orphan
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
# The narrowed rule (#11120 follow-up): behind main only by non-bundle
# commits passes; behind by any bundle commit fails and names it.
expect "behind by one non-bundle commit -> pass"   0 "1 commit(s) behind m_nb1" -- --head bundle --base m_nb1
expect "non-bundle pass says none touches bundle"  0 "none of those commits changes" -- --head bundle --base m_nb1
expect "main changed a bundle dir file -> fail"    1 "  python/trusty-architect/main_side.py" -- --head bundle --base m_dir
expect "main fail lists the commit"                1 "dir bundle"          -- --head bundle --base m_dir
expect "main changed seed_history.tsv -> fail"     1 "  ${TSV}"            -- --head bundle --base m_tsv
expect "4 behind, 1 bundle commit -> fail"         1 "sev bundle commit"   -- --head bundle --base m_sev
expect "4 behind, fail counts all four"            1 "4 commit(s) on m_sev" -- --head bundle --base m_sev
expect "main changed then reverted -> fail"        1 "  ${REV}"            -- --head bundle --base m_rev
expect "main renamed a file into bundle -> fail"   1 "  python/trusty-architect/moved.md" -- --head bundle --base m_renin
expect "main renamed a file out of bundle -> fail" 1 "  ${REV}"            -- --head bundle --base m_renout
expect "main deleted a bundle file -> fail"        1 "  ${REV}"            -- --head bundle --base m_del
expect "main non-ASCII bundle path -> fail"        1 "  python/trusty-architect/m${E_ACUTE}.py" -- --head bundle --base m_na
expect "main spaced bundle path -> fail"           1 "  python/trusty-architect/m b.py" -- --head bundle --base m_sp
expect "main merged a bundle side branch -> fail"  1 "side bundle"         -- --head bundle --base m_mrg
expect "main evil merge edits bundle -> fail"      1 "  python/trusty-architect/evil.py" -- --head bundle --base m_evil
expect "criss-cross, no bundle commit -> pass"     0 "1 commit(s) behind y2" -- --head x2 --base y2
expect "criss-cross, bundle commit -> fail"        1 "  python/trusty-architect/y3.py" -- --head x2 --base y3
expect "no shared history -> error"                2 "share no history"    -- --head orphan --base main --files "${TMP}/files"
# A mktemp that always fails, first on PATH (macOS mktemp ignores a bad TMPDIR).
mkdir -p "${TMP}/fakebin"; printf '#!/bin/sh\nexit 1\n' > "${TMP}/fakebin/mktemp"; chmod +x "${TMP}/fakebin/mktemp"
OLD_PATH="${PATH}"; PATH="${TMP}/fakebin:${PATH}"
expect "temp file cannot be made -> error"         2 "temp directory"      -- --head bundle --base main
PATH="${OLD_PATH}"

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
g checkout -q main; put python/trusty-architect/main_side.py m2; commit "main moves again"
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
g checkout -q main; put README.md moved-third; commit "main moves, docs only"
expect "--fetch, behind by a docs commit -> pass"  0 "1 commit(s) behind refs/remotes/origin/main" -- --fetch origin --head "${PR8}" --base main
unset REPO

# A shallow clone hides the merge base; the gate must refuse, not guess.
S="${TMP}/shallow"
git clone -q --depth 1 --no-single-branch "file://${W}" "${S}" >/dev/null 2>&1
REPO="${S}"
expect "shallow repository -> error"               2 "shallow"             -- --head origin/bundle --base origin/main
unset REPO

echo "check_bundle_pr_current selftest: ${PASS} passed, ${FAIL} failed"
[ "${FAIL}" -eq 0 ]
