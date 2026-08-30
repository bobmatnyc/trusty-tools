#!/usr/bin/env bash
#
# check-ui-bundle-freshness-selftest.sh — failing-case fixtures for
# scripts/check-ui-bundle-freshness.sh (#3606).
#
# Why: the gate this exercises was written because three publishes in a row
#   shipped a stale UI bundle with every existing gate green. A fourth gate
#   inherits that problem until its FAILING branches are pinned: "the bundle was
#   fresh" and "the check enumerated nothing and returned 0" produce the same
#   output. Half the cases below are therefore vacuous-scan refusals — an empty
#   manifest, a manifest row pointing at an empty bundle directory, a UI source
#   tree with no files, a bundle whose index.html references no assets. Each one
#   MUST exit nonzero; a gate that passes them is the failure this whole change
#   exists to prevent.
#
# What: builds a synthetic git repo per case — its own scripts/ui-bundle-manifest.tsv
#   and crates/<crate>/{ui,bundle} trees — runs the gate against it with --repo,
#   and asserts BOTH the exit status and the finding code on stderr. Asserting
#   the code is what stops a case passing for the wrong reason: a stale-bundle
#   fixture that failed as MANIFEST-GAP (say, because the manifest column order
#   changed) would otherwise look like coverage it is not.
#
#   Case 3 is the regression test proper — it reproduces #3509's shape, a
#   commit touching only ui/src while the committed bundle stays put, which is
#   exactly what trusty-search 0.37.0 published.
#
#   Case 18 runs against THIS repo rather than a fixture: the real 0.37.0
#   publish commit fc7f396f, with real objects. Corroboration, not the
#   load-bearing coverage — it self-skips in a shallow clone that lacks the
#   commit.
#
# Test: this IS the test. Run directly:
#   bash scripts/check-ui-bundle-freshness-selftest.sh
#
# Portability: POSIX tools only, bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
GATE="${SCRIPT_DIR}/check-ui-bundle-freshness.sh"

PASSED=0
FAILED=0
SKIPPED=0
WORK="$(mktemp -d "${TMPDIR:-/tmp}/ui-bundle-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

pass_case() {
  echo "  ok  $1"
  PASSED=$((PASSED + 1))
}
fail_case() {
  echo "SELF-TEST FAIL: $1" >&2
  shift
  printf '%s\n' "$@" | sed 's/^/       /' >&2
  FAILED=$((FAILED + 1))
}
skip_case() {
  echo "  -- skip $1"
  SKIPPED=$((SKIPPED + 1))
}

TAB="$(printf '\t')"

# mkrepo <name> -> prints the repo path. Empty workspace, no manifest yet.
mkrepo() {
  local repo="${WORK}/$1"
  mkdir -p "$repo"
  git -C "$repo" init --quiet
  git -C "$repo" config user.email selftest@example.invalid
  git -C "$repo" config user.name "ui bundle selftest"
  echo "$repo"
}

# add_manifest <repo> <row>... — write scripts/ui-bundle-manifest.tsv.
add_manifest() {
  local repo="$1"
  shift
  mkdir -p "${repo}/scripts"
  {
    echo "# crate_dir${TAB}source_dir${TAB}bundle_dir"
    printf '%s\n' "$@"
  } > "${repo}/scripts/ui-bundle-manifest.tsv"
}

# add_crate <repo> <crate> — a minimal manifest so resolve_crate_dir works.
add_crate() {
  local repo="$1" crate="$2"
  mkdir -p "${repo}/crates/${crate}"
  cat > "${repo}/crates/${crate}/Cargo.toml" <<TOML
[package]
name = "${crate}"
version = "0.1.0"
edition = "2021"
TOML
}

# add_ui_source <repo> <crate> <marker>
add_ui_source() {
  local repo="$1" crate="$2" marker="$3"
  mkdir -p "${repo}/crates/${crate}/ui/src/lib/styles"
  echo "export const APP = '${marker}';" > "${repo}/crates/${crate}/ui/src/main.js"
  echo "[data-theme='${marker}'] { color: #fff; }" > "${repo}/crates/${crate}/ui/src/lib/styles/tokens.css"
  echo '{"name":"ui","scripts":{"build":"vite build"}}' > "${repo}/crates/${crate}/ui/package.json"
  echo '<html><body><div id="app"></div></body></html>' > "${repo}/crates/${crate}/ui/index.html"
}

# add_bundle <repo> <crate> <bundle-dir-rel> <hash> [base]
# Writes a Vite-shaped bundle: index.html plus content-hashed assets.
add_bundle() {
  local repo="$1" crate="$2" rel="$3" hash="$4" base="${5:-./}"
  local dir="${repo}/crates/${crate}/${rel}"
  mkdir -p "${dir}/assets"
  echo "/* built ${hash} */" > "${dir}/assets/index-${hash}.js"
  echo "/* built ${hash} */" > "${dir}/assets/index-${hash}.css"
  cat > "${dir}/index.html" <<HTML
<html><head>
<link rel="preconnect" href="https://fonts.googleapis.com">
<script type="module" src="${base}assets/index-${hash}.js"></script>
<link rel="stylesheet" href="${base}assets/index-${hash}.css">
</head><body></body></html>
HTML
}

# stamp <repo> <crate> — record the current source digest into the bundle, as a
# real build does via `make release-prep`. A fixture that never stamps exercises
# STAMP-MISSING, not freshness, so every freshness case must call this.
stamp() {
  bash "${SCRIPT_DIR}/stamp-ui-bundle.sh" --repo "$1" "$2" 2> /dev/null
}

commit_all() {
  local repo="$1" msg="$2"
  git -C "$repo" add -A
  git -C "$repo" commit --quiet -m "$msg"
}

# run_case <label> <expect-exit> <expect-substring|-> <repo> [gate args...]
run_case() {
  local label="$1" want_exit="$2" want_sub="$3" repo="$4"
  shift 4
  local out rc=0
  out="$(bash "$GATE" --repo "$repo" "$@" 2>&1)" || rc=$?
  if [ "$rc" -ne "$want_exit" ]; then
    fail_case "${label}: expected exit ${want_exit}, got ${rc}" "$out"
    return
  fi
  if [ "$want_sub" != "-" ] && ! printf '%s' "$out" | grep -qF -- "$want_sub"; then
    fail_case "${label}: exit ${rc} but output never said '${want_sub}'" "$out"
    return
  fi
  pass_case "${label} -> exit ${rc}${want_sub:+ (${want_sub})}"
}

echo "check-ui-bundle-freshness-selftest: running"

# ---------------------------------------------------------------------------
# Case 1 — source and bundle committed together. PASS, and the PASS line must
# state the counts it inspected, not merely that nothing failed.
# ---------------------------------------------------------------------------
R="$(mkrepo case1)"
add_crate "$R" cratea
add_ui_source "$R" cratea dark
add_bundle "$R" cratea ui-dist AAA111
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
stamp "$R" cratea
commit_all "$R" "feat(ui): dark mode + rebuilt bundle"
run_case "case1 fresh (same commit)" 0 "4 blob hash(es)" "$R"
run_case "case1 counts source files" 0 "4 source file(s)" "$R"

# ---------------------------------------------------------------------------
# Case 2 — bundle regenerated in a LATER commit than the source change.
# ---------------------------------------------------------------------------
R="$(mkrepo case2)"
add_crate "$R" cratea
add_ui_source "$R" cratea light
add_bundle "$R" cratea ui-dist AAA111
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
stamp "$R" cratea
commit_all "$R" "initial"
add_ui_source "$R" cratea dark
commit_all "$R" "feat(ui): dark mode"
rm -rf "${R}/crates/cratea/ui-dist"
add_bundle "$R" cratea ui-dist BBB222
stamp "$R" cratea
commit_all "$R" "chore(ui): rebuild bundle"
run_case "case2 fresh (bundle rebuilt after source)" 0 "asset ref(s) resolved" "$R"

# ---------------------------------------------------------------------------
# Case 3 — THE REGRESSION TEST. #3509's shape: a commit touching only ui/src,
# with the committed bundle left untouched. This is what 0.37.0 published.
# ---------------------------------------------------------------------------
R="$(mkrepo case3)"
add_crate "$R" cratea
add_ui_source "$R" cratea light
add_bundle "$R" cratea ui-dist AAA111
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
stamp "$R" cratea
commit_all "$R" "initial"
add_ui_source "$R" cratea dark
commit_all "$R" "feat(ui): migrate to Foundry v2 tokens"
run_case "case3 stale bundle (#3606 shape)" 1 "BUNDLE-STALE" "$R"

# ---------------------------------------------------------------------------
# Case 4 — the ui/dist layout (bundle nested INSIDE the source dir), same
# staleness. The nested layout is where an unfiltered source scan would count
# the bundle's own files as source and never report drift.
# ---------------------------------------------------------------------------
R="$(mkrepo case4)"
add_crate "$R" crateb
add_ui_source "$R" crateb light
add_bundle "$R" crateb ui/dist AAA111
add_manifest "$R" "crateb${TAB}crates/crateb/ui${TAB}crates/crateb/ui/dist"
stamp "$R" crateb
commit_all "$R" "initial"
add_ui_source "$R" crateb dark
commit_all "$R" "feat(ui): dark mode"
run_case "case4 stale bundle (nested ui/dist layout)" 1 "BUNDLE-STALE" "$R"

# ---------------------------------------------------------------------------
# Case 5 — VACUOUS SCAN: a manifest row whose bundle directory tracks nothing.
# ---------------------------------------------------------------------------
R="$(mkrepo case5)"
add_crate "$R" cratea
add_ui_source "$R" cratea dark
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
commit_all "$R" "ui source only, no bundle"
run_case "case5 vacuous: empty bundle dir" 1 "MANIFEST-STALE" "$R"

# ---------------------------------------------------------------------------
# Case 6 — VACUOUS SCAN: a row whose UI source tree tracks no build inputs
# (only prose, which the gate excludes). Nothing to compare the bundle against.
# ---------------------------------------------------------------------------
R="$(mkrepo case6)"
add_crate "$R" cratea
mkdir -p "${R}/crates/cratea/ui"
echo "# UI" > "${R}/crates/cratea/ui/README.md"
add_bundle "$R" cratea ui-dist AAA111
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
commit_all "$R" "bundle without source"
run_case "case6 vacuous: no UI source files" 1 "NO-SOURCES" "$R"

# ---------------------------------------------------------------------------
# Case 7 — VACUOUS SCAN: no manifest at all.
# ---------------------------------------------------------------------------
R="$(mkrepo case7)"
add_crate "$R" cratea
add_ui_source "$R" cratea dark
add_bundle "$R" cratea ui-dist AAA111
commit_all "$R" "no manifest"
run_case "case7 vacuous: manifest file missing" 1 "MANIFEST-MISSING" "$R"

# ---------------------------------------------------------------------------
# Case 8 — VACUOUS SCAN: a manifest that declares zero crates. This is the
# shape a well-meaning "temporarily disable the gate" edit produces.
# ---------------------------------------------------------------------------
R="$(mkrepo case8)"
add_crate "$R" cratea
add_ui_source "$R" cratea dark
add_bundle "$R" cratea ui-dist AAA111
mkdir -p "${R}/scripts"
printf '# crate_dir\tsource_dir\tbundle_dir\n#\n' > "${R}/scripts/ui-bundle-manifest.tsv"
commit_all "$R" "comment-only manifest"
run_case "case8 vacuous: manifest declares zero crates" 1 "MANIFEST-MISSING" "$R"

# ---------------------------------------------------------------------------
# Case 9 — a crate ships a committed bundle but has no manifest row. A new UI
# crate must not be able to sit outside the gate.
# ---------------------------------------------------------------------------
R="$(mkrepo case9)"
add_crate "$R" cratea
add_ui_source "$R" cratea dark
add_bundle "$R" cratea ui-dist AAA111
add_crate "$R" cratec
add_ui_source "$R" cratec dark
add_bundle "$R" cratec ui/dist CCC333
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
stamp "$R" cratea
commit_all "$R" "second UI crate, unlisted"
run_case "case9 unlisted UI crate" 1 "MANIFEST-GAP" "$R"

# ---------------------------------------------------------------------------
# Case 10 — half-mirrored bundle: index.html arrived, the hashed assets did
# not. Ancestry alone calls this fresh; the asset check is what catches it.
# ---------------------------------------------------------------------------
R="$(mkrepo case10)"
add_crate "$R" cratea
add_ui_source "$R" cratea dark
add_bundle "$R" cratea ui-dist AAA111
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
stamp "$R" cratea
rm -f "${R}/crates/cratea/ui-dist/assets/index-AAA111.js"
commit_all "$R" "partial mirror"
run_case "case10 partially mirrored bundle" 1 "ASSET-MISSING" "$R"

# ---------------------------------------------------------------------------
# Case 11 — VACUOUS SCAN: an index.html referencing zero local assets. The
# asset check would otherwise report success having resolved nothing.
# ---------------------------------------------------------------------------
R="$(mkrepo case11)"
add_crate "$R" cratea
add_ui_source "$R" cratea dark
mkdir -p "${R}/crates/cratea/ui-dist/assets"
echo "/* orphan */" > "${R}/crates/cratea/ui-dist/assets/index-AAA111.js"
echo '<html><head><link href="https://fonts.googleapis.com"></head><body></body></html>' \
  > "${R}/crates/cratea/ui-dist/index.html"
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
stamp "$R" cratea
commit_all "$R" "index with no local refs"
run_case "case11 vacuous: zero asset references" 1 "NO-ASSET-REFS" "$R"

# ---------------------------------------------------------------------------
# Case 12 — bundle with assets but no entry point.
# ---------------------------------------------------------------------------
R="$(mkrepo case12)"
add_crate "$R" cratea
add_ui_source "$R" cratea dark
mkdir -p "${R}/crates/cratea/ui-dist/assets"
echo "/* orphan */" > "${R}/crates/cratea/ui-dist/assets/index-AAA111.js"
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
stamp "$R" cratea
commit_all "$R" "bundle without index.html"
run_case "case12 bundle has no index.html" 1 "NO-INDEX" "$R"

# ---------------------------------------------------------------------------
# Case 13 — absolute asset refs under a deploy base (trusty-console sets
# vite base '/ui/'). These must RESOLVE, not report ASSET-MISSING; a false
# alarm here is how a correct gate gets deleted after one bad week.
# ---------------------------------------------------------------------------
R="$(mkrepo case13)"
add_crate "$R" cratea
add_ui_source "$R" cratea dark
add_bundle "$R" cratea ui/dist AAA111 "/ui/"
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui/dist"
stamp "$R" cratea
commit_all "$R" "absolute base refs"
run_case "case13 absolute /base/ asset refs resolve" 0 "2 asset ref(s) resolved" "$R"

# ---------------------------------------------------------------------------
# Case 14 — a prose-only change under ui/ must NOT demand a rebuild.
# ---------------------------------------------------------------------------
R="$(mkrepo case14)"
add_crate "$R" cratea
add_ui_source "$R" cratea dark
add_bundle "$R" cratea ui-dist AAA111
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
stamp "$R" cratea
commit_all "$R" "initial"
echo "# UI notes" > "${R}/crates/cratea/ui/README.md"
commit_all "$R" "docs(ui): notes"
run_case "case14 prose-only ui/ change is not staleness" 0 "-" "$R"

# ---------------------------------------------------------------------------
# Case 15 — an UNCOMMITTED source edit is staleness too. Without --rev the
# digest reads files off disk, so there is no window where a working tree is
# stale but the gate says otherwise.
# ---------------------------------------------------------------------------
R="$(mkrepo case15)"
add_crate "$R" cratea
add_ui_source "$R" cratea light
add_bundle "$R" cratea ui-dist AAA111
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
stamp "$R" cratea
commit_all "$R" "initial"
echo "[data-theme='dark'] { color: #000; }" > "${R}/crates/cratea/ui/src/lib/styles/tokens.css"
run_case "case15 uncommitted source edit" 1 "BUNDLE-STALE" "$R"

# ---------------------------------------------------------------------------
# Case 16 — the same edit, rebuilt AND re-stamped, clears it. Editing the
# bundle alone does not (that is case 19); the stamp is what clears the gate.
# ---------------------------------------------------------------------------
R="$(mkrepo case16)"
add_crate "$R" cratea
add_ui_source "$R" cratea light
add_bundle "$R" cratea ui-dist AAA111
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
stamp "$R" cratea
commit_all "$R" "initial"
echo "[data-theme='dark'] { color: #000; }" > "${R}/crates/cratea/ui/src/lib/styles/tokens.css"
rm -rf "${R}/crates/cratea/ui-dist"
add_bundle "$R" cratea ui-dist BBB222
stamp "$R" cratea
run_case "case16 uncommitted edit, rebuilt and re-stamped" 0 "-" "$R"

# ---------------------------------------------------------------------------
# Case 17 — a crate with no UI at all exits 0 with an explicit N/A line, so
# preflight-publish.sh can call this for every crate it releases.
# ---------------------------------------------------------------------------
R="$(mkrepo case17)"
add_crate "$R" cratea
add_ui_source "$R" cratea dark
add_bundle "$R" cratea ui-dist AAA111
add_crate "$R" plainlib
mkdir -p "${R}/crates/plainlib/src"
echo "pub fn f() {}" > "${R}/crates/plainlib/src/lib.rs"
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
stamp "$R" cratea
commit_all "$R" "workspace with one UI crate"
run_case "case17 non-UI crate is N/A" 0 "tracks no committed UI bundle" "$R" plainlib

# ---------------------------------------------------------------------------
# Case 18 — the real incident, against this repo's real objects: trusty-search
# at fc7f396f, the commit 0.37.0 was published from.
# ---------------------------------------------------------------------------
if git -C "$REPO_ROOT" rev-parse --verify --quiet 'fc7f396f^{commit}' > /dev/null; then
  # Every commit before this gate landed predates the stamp, so the real
  # incident reports STAMP-MISSING rather than a digest mismatch. The commit
  # diagnostic is what carries the finding for pre-stamp history, and asserting
  # it is what keeps this case about #3509 rather than about a missing file.
  run_case "case18 real 0.37.0 publish commit" 1 "STAMP-MISSING" \
    "$REPO_ROOT" --rev fc7f396f trusty-search
  run_case "case18 names the source change the bundle missed" 1 \
    "Foundry v2 tokens (#3509)" "$REPO_ROOT" --rev fc7f396f trusty-search
else
  skip_case "case18 real 0.37.0 publish commit (fc7f396f not in this clone)"
fi

# ---------------------------------------------------------------------------
# Case 19 — THE LAUNDERING REGRESSION. A review broke the ancestry version of
# this gate in three commits: real source change (caught), then one unrelated
# line appended to the bundle's index.html — and the still-stale bundle passed,
# because any commit touching the bundle directory read as "rebuilt after the
# source". The built JS never changed. Content cannot be laundered that way.
# ---------------------------------------------------------------------------
R="$(mkrepo case19)"
add_crate "$R" cratea
add_ui_source "$R" cratea light
add_bundle "$R" cratea ui-dist AAA111
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
stamp "$R" cratea
commit_all "$R" "c1: source v1 + bundle v1 (stamped)"
add_ui_source "$R" cratea dark
commit_all "$R" "c2: source v2, bundle NOT rebuilt"
run_case "case19 source changed, no rebuild" 1 "BUNDLE-STALE" "$R"
printf '<!-- unrelated tweak -->\n' >> "${R}/crates/cratea/ui-dist/index.html"
commit_all "$R" "c3: unrelated one-line edit to bundle index.html"
if ! git -C "$R" show "HEAD:crates/cratea/ui-dist/assets/index-AAA111.js" | grep -q 'built AAA111'; then
  fail_case "case19 fixture broken: the built asset was modified, so this proves nothing"
else
  run_case "case19 launder attempt via bundle-dir edit" 1 "BUNDLE-STALE" "$R"
fi

# ---------------------------------------------------------------------------
# Case 20 — the false-positive exit. A source edit that produces a
# byte-identical bundle still moves the digest, so the gate fails; the remedy
# has to leave something to commit or it is not a remedy. Re-stamping changes
# ui-source-hash.txt, which is why no override flag exists.
# ---------------------------------------------------------------------------
R="$(mkrepo case20)"
add_crate "$R" cratea
add_ui_source "$R" cratea dark
add_bundle "$R" cratea ui-dist AAA111
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
stamp "$R" cratea
commit_all "$R" "initial"
printf '// comment only — output is byte-identical\n' >> "${R}/crates/cratea/ui/src/main.js"
run_case "case20 comment-only source edit fails" 1 "BUNDLE-STALE" "$R"
stamp "$R" cratea
if [ -z "$(git -C "$R" status --porcelain -- crates/cratea/ui-dist)" ]; then
  fail_case "case20 re-stamp left nothing to commit — the remedy cannot clear the gate"
else
  pass_case "case20 re-stamp produces a committable change"
fi
run_case "case20 re-stamp clears it" 0 "-" "$R"

# ---------------------------------------------------------------------------
# Case 21 — a stamp is not trusted for existing. A hand-written digest that
# does not match the source is BUNDLE-STALE, not a pass.
# ---------------------------------------------------------------------------
R="$(mkrepo case21)"
add_crate "$R" cratea
add_ui_source "$R" cratea dark
add_bundle "$R" cratea ui-dist AAA111
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
echo "0000000000000000000000000000000000000000 4" \
  > "${R}/crates/cratea/ui-dist/ui-source-hash.txt"
commit_all "$R" "hand-written stamp"
run_case "case21 forged stamp digest" 1 "BUNDLE-STALE" "$R"

# ---------------------------------------------------------------------------
# Case 22 — a stamp file with no digest line (comments only) is STAMP-MISSING,
# not an empty-but-fine digest that matches an empty source.
# ---------------------------------------------------------------------------
R="$(mkrepo case22)"
add_crate "$R" cratea
add_ui_source "$R" cratea dark
add_bundle "$R" cratea ui-dist AAA111
add_manifest "$R" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
printf '# nothing here\n\n' > "${R}/crates/cratea/ui-dist/ui-source-hash.txt"
commit_all "$R" "digest-less stamp"
run_case "case22 stamp with no digest" 1 "STAMP-MISSING" "$R"

# ---------------------------------------------------------------------------
# Case 23 — #5936: every crate that builds its bundle IN PLACE must re-stamp
# from its Vite config. Runs against THIS repo, not a fixture.
#
# `build.emptyOutDir: true` deletes the tracked ui-source-hash.txt on every
# `pnpm build`, so a row whose bundle_dir IS the Vite outDir needs
# scripts/lib/vite-stamp-bundle.mjs wired in to put it back.
#
# This asserts the WIRING, not the stamping — the stamping itself is cases 20
# and 21. #5936 recurred four times precisely because a fix that reached one
# crate left the other two exposed, so the uniformity is what needs a gate.
#
# #6155: SCOPE IS DECIDED BY THE EXEMPTION LIST, NEVER BY WHAT THE CONFIG SAYS.
# An earlier revision of this case derived candidacy from the config — skipping
# a row whose vite.config.js was missing or whose outDir was not a literal the
# regex matched. That moved the #5936 failure class into the selection
# mechanism: the one row least able to answer for itself was the one that
# quietly left the count. Membership is now a standing decision recorded below,
# and an in-scope row that cannot be read FAILS. Cases 25a-25c drive both
# unreadable shapes plus a positive control.
# ---------------------------------------------------------------------------

# Rows whose committed bundle is NOT its Vite outDir, so a mirror step owns the
# stamp instead. Space-separated manifest row keys; everything else is in scope.
#
# trusty-search: ui-dist/ is a mirror of the console's ui-search-dist/, stamped
# by `make -C crates/trusty-search sync-ui` (#6155). Its source_dir names the
# console's Vite project, whose outDir is that OTHER bundle — so reading the
# config would place this row out of scope for a reason that is true by
# accident. Exempting it says so on purpose.
STAMP_WIRING_EXEMPT="trusty-search"

# stamp_wiring_report <repo_root> <manifest_path> <exempt_keys>
#
# Why: case 23's verdict has to be drivable against a fixture, so the two
# unreadable-config shapes can be proven to FAIL rather than vanish (#6155).
# What: prints one `OK <key>` or `FINDING <key> <message>` line per in-scope
# row, then `CHECKED <n>`. Every non-exempt row is counted before it is read,
# so no row can leave the tally by being unreadable.
# Test: `case23` drives it against this repo; `case25a`/`case25b`/`case25c`
# drive the missing-config, dynamic-outDir, and correct-config fixtures.
stamp_wiring_report() {
  local repo="$1" manifest="$2" exempt="$3"
  local checked=0 crate src_dir bundle_dir _rest config out_dir resolved
  while IFS="$(printf '\t')" read -r crate src_dir bundle_dir _rest; do
    case "${crate:-}" in '' | \#*) continue ;; esac
    case " ${exempt} " in *" ${crate} "*) continue ;; esac

    # Counted FIRST. Everything below can only turn this row into a finding,
    # never into a row that was never here.
    checked=$((checked + 1))

    if [ -z "${src_dir:-}" ] || [ -z "${bundle_dir:-}" ]; then
      echo "FINDING ${crate} row is missing a source_dir or bundle_dir column"
      continue
    fi
    # source_dir is a comma-separated list; the CURRENT path is where a build
    # runs (#6155).
    src_dir="${src_dir%%,*}"
    config="${repo}/${src_dir}/vite.config.js"

    if [ ! -f "$config" ]; then
      echo "FINDING ${crate} ${src_dir}/vite.config.js is missing, so nothing can say whether ${bundle_dir} is its outDir"
      continue
    fi
    out_dir="$(sed -n "s/.*outDir:[[:space:]]*['\"]\([^'\"]*\)['\"].*/\1/p" "$config" | head -1)"
    if [ -z "$out_dir" ]; then
      echo "FINDING ${crate} ${src_dir}/vite.config.js declares no literal build.outDir; this check cannot tell where the bundle lands"
      continue
    fi
    case "$out_dir" in
      ../*) resolved="$(dirname "$src_dir")/${out_dir#../}" ;;
      *) resolved="${src_dir}/${out_dir}" ;;
    esac
    if [ "$bundle_dir" != "$resolved" ]; then
      echo "FINDING ${crate} outDir resolves to ${resolved}, not the manifest's ${bundle_dir}; wire the stamp or record an exemption"
      continue
    fi
    if ! grep -qF "stampUiBundle('${crate}')" "$config"; then
      echo "FINDING ${crate} ${src_dir}/vite.config.js does not call stampUiBundle('${crate}') — emptyOutDir deletes ${bundle_dir}/ui-source-hash.txt and nothing re-stamps it (#5936)"
      continue
    fi
    echo "OK ${crate}"
  done < "$manifest"
  echo "CHECKED ${checked}"
}

SW_REPORT="$(stamp_wiring_report "$REPO_ROOT" "${SCRIPT_DIR}/ui-bundle-manifest.tsv" "$STAMP_WIRING_EXEMPT")"
WIRED_CHECKED="$(printf '%s\n' "$SW_REPORT" | awk '$1 == "CHECKED" { print $2 }')"

printf '%s\n' "$SW_REPORT" | grep '^OK ' | while read -r _ crate; do
  echo "  ok  case23 ${crate} re-stamps its bundle from vite.config.js"
done
# The subshell above cannot touch PASSED, so account for the OK rows here.
SW_OK_COUNT="$(printf '%s\n' "$SW_REPORT" | grep -c '^OK ' || true)"
PASSED=$((PASSED + SW_OK_COUNT))

SW_FINDINGS="$(printf '%s\n' "$SW_REPORT" | grep '^FINDING ' || true)"
if [ -n "$SW_FINDINGS" ]; then
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    fail_case "case23 ${line#FINDING }"
  done <<EOF_SW
$SW_FINDINGS
EOF_SW
fi

# A loop that matched nothing would report all-clear having inspected nothing —
# the vacuous-scan shape every case above exists to refuse.
if [ "${WIRED_CHECKED:-0}" -eq 0 ]; then
  fail_case "case23 inspected no crates — every manifest row is exempt or the manifest is empty"
else
  pass_case "case23 inspected ${WIRED_CHECKED} in-scope bundle crate(s)"
fi

# An exemption that names a row the manifest no longer has is dead text, and
# dead text in a scope list is how a real row gets exempted by a later rename.
SW_EXEMPT_LIVE=1
for exempt_key in $STAMP_WIRING_EXEMPT; do
  if ! awk -F'\t' -v k="$exempt_key" '$1 == k { found = 1 } END { exit !found }' \
    "${SCRIPT_DIR}/ui-bundle-manifest.tsv"; then
    fail_case "case23 STAMP_WIRING_EXEMPT names '${exempt_key}', which has no manifest row" \
      "A stale exemption silently widens as rows are renamed. Drop it or fix the key."
    SW_EXEMPT_LIVE=0
  fi
done
[ "$SW_EXEMPT_LIVE" -eq 1 ] && pass_case "case23 every STAMP_WIRING_EXEMPT key has a live manifest row"

# ---------------------------------------------------------------------------
# Case 25 — #6155: the row-selection mechanism itself must not fail open.
#
# case23 decides scope from STAMP_WIRING_EXEMPT, so an in-scope row whose
# vite.config.js cannot be read has to become a FINDING. When candidacy was
# derived from the config instead, both shapes below left the tally silently —
# a vacuous pass wearing the shape of a clean run. 25c is the positive control:
# without it, 25a and 25b would also pass against a harness that never returns
# OK at all.
# ---------------------------------------------------------------------------
sw_fixture() {
  local repo="$1"
  mkdir -p "${repo}/crates/cratea/ui"
  add_manifest "$repo" "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui/dist"
}

# assert_sw <label> <repo> <expect: FINDING|OK> <expect_checked>
assert_sw() {
  local label="$1" repo="$2" expect="$3" expect_checked="$4"
  local report checked
  report="$(stamp_wiring_report "$repo" "${repo}/scripts/ui-bundle-manifest.tsv" "")"
  checked="$(printf '%s\n' "$report" | awk '$1 == "CHECKED" { print $2 }')"
  if [ "$checked" != "$expect_checked" ]; then
    fail_case "${label}: CHECKED=${checked}, expected ${expect_checked}" \
      "A row that leaves the count is the fail-open shape this case exists to refuse." \
      "$report"
  elif ! printf '%s\n' "$report" | grep -q "^${expect} "; then
    fail_case "${label}: report has no ${expect} line" "$report"
  else
    pass_case "${label} -> ${expect}, CHECKED=${checked}"
  fi
}

R="$(mkrepo case25a)"
sw_fixture "$R"
assert_sw "case25a missing vite.config.js is a finding, not a skip" "$R" FINDING 1

R="$(mkrepo case25b)"
sw_fixture "$R"
cat > "${R}/crates/cratea/ui/vite.config.js" <<'VITE'
import { defineConfig } from 'vite';
const target = process.env.OUT || 'dist';
export default defineConfig({
  plugins: [stampUiBundle('cratea')],
  build: { outDir: target, emptyOutDir: true },
});
VITE
assert_sw "case25b dynamic outDir is a finding, not a skip" "$R" FINDING 1

R="$(mkrepo case25c)"
sw_fixture "$R"
cat > "${R}/crates/cratea/ui/vite.config.js" <<'VITE'
import { defineConfig } from 'vite';
import { stampUiBundle } from '../../../scripts/lib/vite-stamp-bundle.mjs';
export default defineConfig({
  plugins: [stampUiBundle('cratea')],
  build: { outDir: 'dist', emptyOutDir: true },
});
VITE
assert_sw "case25c control: a correct config still reports OK" "$R" OK 1

# ---------------------------------------------------------------------------
# Case 24 — #6155: one crate, two committed bundles. trusty-console packages
# its own ui/dist AND the trusty-search SPA it serves at /tools/search/, whose
# source lives in another crate.
#
# The two rows must not share a key, because stamp-ui-bundle.sh stamps by key
# and each crate's build.rs stamps its own name after building only its own UI.
# A shared key would have that build certify a bundle it never rebuilt.
# So the second row is keyed `<crate>-<suffix>`, and three things follow, each
# asserted below:
#   a. the gate asked about `<crate>` inspects BOTH rows,
#   b. staleness in the second bundle fails the gate asked about `<crate>` —
#      which is what preflight-publish.sh CHECK 7 runs,
#   c. stamping `<crate>` does NOT re-stamp the `<crate>-<suffix>` row.
# (c) is the one that would silently launder a stale bundle if it regressed.
# ---------------------------------------------------------------------------
R="$(mkrepo case24)"
add_crate "$R" cratea
add_crate "$R" crateb
add_ui_source "$R" cratea dark
add_ui_source "$R" crateb light
add_bundle "$R" cratea ui-dist AAA111
add_bundle "$R" cratea ui-extra-dist BBB222
add_manifest "$R" \
  "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist" \
  "cratea-extra${TAB}crates/crateb/ui${TAB}crates/cratea/ui-extra-dist"
stamp "$R" cratea
stamp "$R" cratea-extra
commit_all "$R" "two bundles, both fresh"
run_case "case24a --crate cratea inspects the suffixed row too" 0 "PASS: cratea-extra" "$R" cratea

# The second bundle's source moves; its bundle does not.
echo "export const APP = 'light-v2';" > "${R}/crates/crateb/ui/src/main.js"
commit_all "$R" "crateb source moves, cratea/ui-extra-dist does not"
run_case "case24b sibling staleness fails the cratea gate" 1 "BUNDLE-STALE" "$R" cratea

# Stamping the OWN row must not clear the suffixed row's finding.
stamp "$R" cratea
run_case "case24c stamping cratea does not launder cratea-extra" 1 "BUNDLE-STALE" "$R" cratea

# ---------------------------------------------------------------------------
# Case 24d/24e — #6155: the MANIFEST-GAP safety net must see a SECOND bundle.
#
# Discovery used to collapse a bundle path to its crate name and ask "does this
# crate have a row?" — which answers yes for a crate shipping two bundles with
# a row for only one. Deleting or mistyping the second row then left the gate
# inspecting one bundle and reporting OK: the #3606 shape, with no alarm.
# 24d deletes the row; 24e keeps it but keys it where the crate-scoped gate
# will never reach it. Both must fire MANIFEST-GAP.
# ---------------------------------------------------------------------------
R="$(mkrepo case24de)"
add_crate "$R" cratea
add_crate "$R" crateb
add_ui_source "$R" cratea dark
add_ui_source "$R" crateb light
add_bundle "$R" cratea ui-dist AAA111
add_bundle "$R" cratea ui-extra-dist BBB222

# 24d — the second bundle ships with NO row at all.
add_manifest "$R" \
  "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist"
stamp "$R" cratea
commit_all "$R" "cratea ships two bundles, manifest declares one"
run_case "case24d an undeclared second bundle fires MANIFEST-GAP" 1 "MANIFEST-GAP" "$R" cratea

# 24e — the row exists and names the right bundle_dir, but its key is one the
# crate-scoped gate never inspects, so the bundle ships unchecked anyway.
add_manifest "$R" \
  "cratea${TAB}crates/cratea/ui${TAB}crates/cratea/ui-dist" \
  "crateaextra${TAB}crates/crateb/ui${TAB}crates/cratea/ui-extra-dist"
stamp "$R" cratea
stamp "$R" crateaextra
commit_all "$R" "second row present but keyed out of cratea's scope"
run_case "case24e a row keyed outside the crate's scope fires MANIFEST-GAP" 1 "MANIFEST-GAP" "$R" cratea

echo
echo "check-ui-bundle-freshness-selftest: ${PASSED} passed, ${FAILED} failed, ${SKIPPED} skipped"
[ "$FAILED" -eq 0 ] || exit 1
exit 0
