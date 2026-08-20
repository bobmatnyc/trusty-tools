#!/usr/bin/env bash
#
# check-tag-publish-parity-selftest.sh — failing-case fixtures for
# scripts/check-tag-publish-parity.sh.
#
# Why: this gate exists because of a failure nobody could see. On 2026-08-11
#   `tga-v2.17.0` was cut at 246e4ca2 and the crate published from 7d5cf82e1,
#   and every release gate in the repo reported green — check-publish-ready.sh
#   because it only asks whether the tag is an ANCESTOR of origin/main, and
#   preflight-publish.sh because it only asks whether HEAD EQUALS origin/main.
#   A new gate against that has exactly the same problem as the old ones until
#   its FAILING branches are exercised: "the release tag matched" and "the
#   check ran and always returns 0" are indistinguishable from a green run.
#   Every case below is a state the pre-fix release path accepted.
#
# What: builds a synthetic git repo per case — a crates/<dir>/Cargo.toml, a
#   commit history, and tags placed to reproduce one failure — runs the gate
#   against it with --repo/--no-fetch, and asserts BOTH the exit status and the
#   finding code on stderr. Asserting the code is what stops a case from
#   passing for the wrong reason: a fast-forward fixture that failed as
#   TAG-MISSING (say, because the tag name convention changed) would otherwise
#   look like coverage it is not.
#
#   Case 2 is the regression test proper. It replays the fast-forward sequence:
#   tag the release commit, land two unrelated commits, fast-forward the
#   checkout to satisfy preflight CHECK 1, publish. The pre-fix path published
#   that with no complaint.
#
#   Case 8 is the same defect seen from the other side — the tag agrees with
#   HEAD, but the sha1 cargo actually recorded in .cargo_vcs_info.json is a
#   different commit. That is the artifact that made 2026-08-11 provable after
#   the fact, and it is the only check that still works once the upload has
#   happened.
#
#   Cases 1, 6, 7 and 10 are the counterweights: a matching tag, a package-name
#   alias tag, a matching vcs-info record, and an ANNOTATED tag must all exit 0.
#   Without them the failing cases would be satisfied by a gate that rejects
#   everything, and case 10 specifically pins the `^{}` dereference — comparing
#   an annotated tag's OBJECT sha against a commit sha reports drift that does
#   not exist, which is how a correct gate gets disabled after one false alarm.
#
#   Case 11 runs against THIS repo rather than a fixture: the real
#   `tga-v2.17.0` tag against the real current HEAD, which is the real
#   2026-08-11 shape with real objects. It is corroboration, not the load-
#   bearing coverage — it self-skips when the tag is not present locally (a
#   clone without tags) or when HEAD happens to be the tagged commit.
#
# Test: this IS the test. Run directly:
#   bash scripts/check-tag-publish-parity-selftest.sh
#
# Portability: POSIX tools only, bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
GATE="${SCRIPT_DIR}/check-tag-publish-parity.sh"

PASSED=0
FAILED=0
WORK="$(mktemp -d "${TMPDIR:-/tmp}/tag-parity-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

pass_case() { echo "  ok  $1"; PASSED=$((PASSED + 1)); }
fail_case() {
  echo "SELF-TEST FAIL: $1" >&2
  shift
  printf '%s\n' "$@" | sed 's/^/       /' >&2
  FAILED=$((FAILED + 1))
}

# mkrepo <name> <crate-dir> <package-name> <version> -> prints the repo path.
# A minimal but REAL workspace shape: the gate resolves the crate the same way
# check-publish-ready.sh and preflight-publish.sh do, off crates/<dir>/Cargo.toml.
mkrepo() {
  local name="$1" dir="$2" pkg="$3" version="$4"
  local repo="${WORK}/${name}"
  mkdir -p "${repo}/crates/${dir}"
  cat > "${repo}/crates/${dir}/Cargo.toml" <<TOML
[package]
name = "${pkg}"
version = "${version}"
edition = "2021"
TOML
  git -C "$repo" init --quiet
  git -C "$repo" config user.email selftest@example.invalid
  git -C "$repo" config user.name "tag parity selftest"
  git -C "$repo" add -A
  git -C "$repo" commit --quiet -m "release ${pkg} ${version}"
  echo "$repo"
}

# commit_more <repo> <n> — land n unrelated commits, as main moving under a
# release run does.
commit_more() {
  local repo="$1" n="$2" i=1
  while [ "$i" -le "$n" ]; do
    echo "unrelated change ${i}" > "${repo}/unrelated-${i}.txt"
    git -C "$repo" add -A
    git -C "$repo" commit --quiet -m "fix(other): unrelated commit ${i}"
    i=$((i + 1))
  done
}

# run_case <label> <expect-exit> <expect-substring|-> <repo> [gate args...]
run_case() {
  local label="$1" want_exit="$2" want_sub="$3" repo="$4"
  shift 4
  local out rc=0
  out="$(bash "$GATE" --repo "$repo" --no-fetch "$@" 2>&1)" || rc=$?
  if [ "$rc" -ne "$want_exit" ]; then
    fail_case "${label}: expected exit ${want_exit}, got ${rc}" "$out"
    return
  fi
  # Here-string, never `printf … | grep -q` — see case 11 for what that costs
  # once the gate's output outgrows the pipe buffer.
  if [ "$want_sub" != "-" ] && ! grep -qF -- "$want_sub" <<< "$out"; then
    fail_case "${label}: exit ${rc} but stderr never said '${want_sub}'" "$out"
    return
  fi
  if [ "$want_sub" = "-" ]; then
    pass_case "${label} -> exit ${rc} (clean)"
  else
    pass_case "${label} -> exit ${rc}, reported ${want_sub}"
  fi
}

# ===========================================================================
# 1. Tag at HEAD — the state a correct release is in. Exit 0.
# ===========================================================================
repo="$(mkrepo tag-at-head trusty-example trusty-example 1.2.3)"
git -C "$repo" tag trusty-example-v1.2.3
run_case "tag at HEAD" 0 "-" "$repo" trusty-example

# ===========================================================================
# 2. THE REGRESSION TEST — fast-forward between tagging and publishing.
#    Tag the release commit, let main move, fast-forward to satisfy
#    preflight-publish.sh CHECK 1, publish. Every pre-fix gate passes here.
# ===========================================================================
repo="$(mkrepo fast-forward trusty-example trusty-example 1.2.3)"
git -C "$repo" tag trusty-example-v1.2.3
commit_more "$repo" 2
run_case "fast-forward drift" 1 "TAG-DRIFT" "$repo" trusty-example

# The remedy has to name the fast-forward, or whoever hits this at 2am reads it
# as a botched tag and re-tags the wrong commit.
out="$(bash "$GATE" --repo "$repo" --no-fetch trusty-example 2>&1 || true)"
if ! grep -qF "ANCESTOR of HEAD" <<< "$out"; then
  fail_case "fast-forward diagnosis: the failure did not identify the fast-forward" "$out"
elif ! grep -qF "git tag -f" <<< "$out"; then
  fail_case "fast-forward diagnosis: no re-tag remedy given" "$out"
elif ! grep -qF "unrelated commit 2" <<< "$out"; then
  fail_case "fast-forward diagnosis: the commits added since the tag were not listed" "$out"
else
  pass_case "the fast-forward failure names the cause, lists the drift, and gives the re-tag remedy"
fi

# ===========================================================================
# 3. Divergent history — the tag is not an ancestor. Same finding, different
#    diagnosis: re-tagging is right, but "you fast-forwarded" would be a lie.
# ===========================================================================
repo="$(mkrepo divergent trusty-example trusty-example 1.2.3)"
base="$(git -C "$repo" rev-parse HEAD)"
git -C "$repo" checkout --quiet -b release
echo "release-only line" > "${repo}/release-only.txt"
git -C "$repo" add -A
git -C "$repo" commit --quiet -m "chore: release-branch commit"
git -C "$repo" tag trusty-example-v1.2.3
git -C "$repo" checkout --quiet "$base"
echo "other line" > "${repo}/other.txt"
git -C "$repo" add -A
git -C "$repo" commit --quiet -m "feat(other): divergent commit"
run_case "divergent tag" 1 "TAG-DRIFT" "$repo" trusty-example
out="$(bash "$GATE" --repo "$repo" --no-fetch trusty-example 2>&1 || true)"
if grep -qF "ANCESTOR of HEAD" <<< "$out"; then
  fail_case "divergent diagnosis: a divergent tag was reported as a fast-forward" "$out"
else
  pass_case "a divergent tag is not misreported as a fast-forward"
fi

# ===========================================================================
# 4. No tag at all. Publishing would ship a commit nothing names.
# ===========================================================================
repo="$(mkrepo no-tag trusty-example trusty-example 1.2.3)"
run_case "missing tag" 1 "TAG-MISSING" "$repo" trusty-example

# ===========================================================================
# 5. Alias split — both accepted tag series exist (#1128) at DIFFERENT commits.
#    Whichever one a reader checks out, one of them misrepresents the release.
# ===========================================================================
repo="$(mkrepo alias-split trusty-git-analytics tga 2.17.0)"
git -C "$repo" tag tga-v2.17.0
commit_more "$repo" 1
git -C "$repo" tag trusty-git-analytics-v2.17.0
run_case "alias split" 1 "TAG-SPLIT" "$repo" tga

# ===========================================================================
# 6. The tga alias alone, at HEAD — the form 2026-08-11 actually pushed. It
#    must RESOLVE and pass, or case 5 would be satisfied by a gate that simply
#    cannot see alias tags.
# ===========================================================================
repo="$(mkrepo alias-only trusty-git-analytics tga 2.17.0)"
git -C "$repo" tag tga-v2.17.0
run_case "package-name alias tag resolves" 0 "-" "$repo" tga

# ===========================================================================
# 7. vcs-info agrees with the tag. Exit 0.
# ===========================================================================
repo="$(mkrepo vcs-match trusty-example trusty-example 1.2.3)"
git -C "$repo" tag trusty-example-v1.2.3
head_sha="$(git -C "$repo" rev-parse HEAD)"
mkdir -p "${repo}/target/package/trusty-example-1.2.3"
cat > "${repo}/target/package/trusty-example-1.2.3/.cargo_vcs_info.json" <<JSON
{
  "git": {
    "sha1": "${head_sha}"
  },
  "path_in_vcs": "crates/trusty-example"
}
JSON
run_case "vcs-info matches the tag" 0 "-" "$repo" trusty-example --vcs-info auto

# ===========================================================================
# 8. THE 2026-08-11 ARTIFACT — the tag agrees with HEAD, but the sha1 cargo
#    recorded is a different commit. This is the shape of the real evidence
#    (tag 246e4ca2, .cargo_vcs_info.json 7d5cf82e1); the objects here are
#    synthetic because those two commits cannot be reconstructed in a fixture
#    repo, but the comparison under test is identical.
# ===========================================================================
repo="$(mkrepo vcs-mismatch trusty-example trusty-example 1.2.3)"
git -C "$repo" tag trusty-example-v1.2.3
commit_more "$repo" 1
published_sha="$(git -C "$repo" rev-parse HEAD)"
git -C "$repo" reset --hard --quiet trusty-example-v1.2.3
mkdir -p "${repo}/target/package/trusty-example-1.2.3"
cat > "${repo}/target/package/trusty-example-1.2.3/.cargo_vcs_info.json" <<JSON
{
  "git": {
    "sha1": "${published_sha}"
  },
  "path_in_vcs": "crates/trusty-example"
}
JSON
run_case "published commit != tag commit" 1 "VCS-INFO-MISMATCH" "$repo" trusty-example --vcs-info auto
out="$(bash "$GATE" --repo "$repo" --no-fetch trusty-example --vcs-info auto 2>&1 || true)"
if ! grep -qF "$published_sha" <<< "$out"; then
  fail_case "vcs-info mismatch: the failure did not name the commit that actually shipped" "$out"
else
  pass_case "the vcs-info mismatch names the commit that actually shipped"
fi

# ===========================================================================
# 9. --vcs-info with nothing to read. An explicit request to verify must never
#    degrade into a skip — that is the fail-open shape this whole file exists
#    to rule out.
# ===========================================================================
repo="$(mkrepo vcs-absent trusty-example trusty-example 1.2.3)"
git -C "$repo" tag trusty-example-v1.2.3
run_case "vcs-info absent fails closed" 1 "VCS-INFO-MISSING" "$repo" trusty-example --vcs-info auto

# A vcs-info file with no git.sha1 is unreadable, not empty-and-fine.
mkdir -p "${repo}/target/package/trusty-example-1.2.3"
echo '{"path_in_vcs": "crates/trusty-example"}' \
  > "${repo}/target/package/trusty-example-1.2.3/.cargo_vcs_info.json"
run_case "vcs-info without git.sha1 fails closed" 1 "VCS-INFO-UNREADABLE" "$repo" trusty-example --vcs-info auto

# ===========================================================================
# 10. An ANNOTATED tag at HEAD. `git ls-remote` and `refs/tags/<t>` resolve to
#     the tag OBJECT; comparing that against a commit sha reports drift that
#     does not exist, and one false alarm on a correct release is how a gate
#     gets routed around.
# ===========================================================================
repo="$(mkrepo annotated trusty-example trusty-example 1.2.3)"
git -C "$repo" tag -a trusty-example-v1.2.3 -m "release 1.2.3"
run_case "annotated tag at HEAD" 0 "-" "$repo" trusty-example

# ===========================================================================
# 11. Corroboration against this repo's real history: tga-v2.17.0 versus the
#     current HEAD. Self-skips rather than failing when the tag is absent (a
#     clone without tags) or when HEAD is the tagged commit itself.
# ===========================================================================
REAL_TAG="tga-v2.17.0"
REAL_TAG_SHA="$(git -C "$REPO_ROOT" rev-parse --verify --quiet "refs/tags/${REAL_TAG}^{commit}" || true)"
REAL_HEAD="$(git -C "$REPO_ROOT" rev-parse "HEAD^{commit}")"
if [ -z "$REAL_TAG_SHA" ]; then
  echo "  --  real-history case skipped: ${REAL_TAG} is not present locally"
elif [ "$REAL_TAG_SHA" = "$REAL_HEAD" ]; then
  echo "  --  real-history case skipped: HEAD is ${REAL_TAG}'s own commit"
else
  rc=0
  out="$(bash "$GATE" --repo "$REPO_ROOT" --no-fetch tga 2.17.0 2>&1)" || rc=$?
  if [ "$rc" -eq 0 ]; then
    fail_case "real-history: the gate passed ${REAL_TAG} (${REAL_TAG_SHA}) against HEAD (${REAL_HEAD})" "$out"
  # Here-strings, NOT `printf … | grep -qF`. `grep -q` exits on the FIRST match
  # and TAG-DRIFT is on the gate's third line, so printf is left writing into a
  # closed pipe; past the pipe buffer it dies on SIGPIPE, `set -o pipefail`
  # promotes that 141 to the pipeline's status, and the test reads "the needle
  # was absent" from output that plainly contains it. The gate's output grows
  # with every commit between tga-v2.17.0 and HEAD — it reached ~43 KB and the
  # job started failing with `printf: write error: Broken pipe` beside a
  # verbatim "FAIL: TAG-DRIFT" line.
  elif ! grep -qF "TAG-DRIFT" <<< "$out"; then
    fail_case "real-history: exit ${rc} but not reported as TAG-DRIFT" "$out"
  elif ! grep -qF "$REAL_TAG_SHA" <<< "$out"; then
    fail_case "real-history: the failure did not name the tagged commit" "$out"
  else
    pass_case "real ${REAL_TAG} (${REAL_TAG_SHA}) vs real HEAD -> TAG-DRIFT"
  fi
fi

echo
if [ "$FAILED" -ne 0 ]; then
  echo "check-tag-publish-parity-selftest: ${PASSED} passed, ${FAILED} FAILED." >&2
  exit 1
fi
echo "check-tag-publish-parity-selftest: ${PASSED} passed, 0 failed."
exit 0
