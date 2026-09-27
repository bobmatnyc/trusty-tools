#!/usr/bin/env bash
#
# select-test-crates_selftest.sh — regression fixtures for
#   scripts/select-test-crates.sh (#7753).
#
# Why: a selector whose only failure mode is "answer everything" is never
#   noticed as broken; the cases that matter are the ones that narrow the
#   output, and the ones that prove a broken `cargo metadata` still fails open
#   rather than printing nothing.
#
# What: builds a throwaway Cargo workspace with a known dependency shape —
#   isolated (no edges in or out), leaf <- mid <- top (two-hop transitive
#   chain), top -(dev)-> devonly, and base <- consumer1 <- consumer2 (a
#   two-hop REVERSE chain, the shape the selector exists to compute) — and
#   asserts the crate set `select-test-crates.sh --files ...` prints for each
#   case. A fixture rather than this repo's live graph, so an unrelated PR
#   that adds a dependency edge cannot turn these cases red. A second fixture
#   covers the scripts/** and .github/** rules (#7777 ruling). A short LIVE
#   section follows, checking this repo's own trusty-common override and its
#   real scripts/** literal edges.
#
# Usage: bash scripts/select-test-crates_selftest.sh
# Exit: 0 when every case matches; 1 otherwise, printing both sides of each
#   mismatch.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/select-test-crates.sh"

FAILURES=0
CASES=0

fail() {
  FAILURES=$((FAILURES + 1))
  echo "  FAIL: $*"
}

# assert_eq <label> <expected> <actual>
assert_eq() {
  CASES=$((CASES + 1))
  if [ "$2" = "$3" ]; then
    printf '  ok   %-62s -> %s\n' "$1" "$(printf '%s' "$3" | tr '\n' ',')"
  else
    fail "$1: expected '$2', got '$3'"
  fi
}

WORK="$(mktemp -d)" || exit 1
trap 'rm -rf "${WORK}"' EXIT

FIXTURE="${WORK}/fixture"
STUB_DIR="${WORK}/stub"
mkdir -p "${STUB_DIR}"

# ---------------------------------------------------------------------------
# Fixture workspace
#
#   isolated                      no edges in or out — the true "leaf" case
#   leaf <── mid <── top           normal deps, one transitive hop
#   top ──(dev)──> devonly         dev-dependencies count as edges (#7753,
#                                  same rationale as ci-crate-relevance.sh)
#   base <── consumer1 <── consumer2   two-hop REVERSE chain: changing base
#                                  must select consumer2 even though it only
#                                  depends on consumer1, never on base
# ---------------------------------------------------------------------------
new_crate() {
  local dir="$1" name="$2"
  mkdir -p "${FIXTURE}/${dir}/src"
  : >"${FIXTURE}/${dir}/src/lib.rs"
  {
    echo '[package]'
    echo "name = \"${name}\""
    echo 'version = "0.1.0"'
    echo 'edition = "2021"'
  } >"${FIXTURE}/${dir}/Cargo.toml"
}

mkdir -p "${FIXTURE}"
{
  echo '[workspace]'
  echo 'resolver = "2"'
  echo 'members = ["crates/*"]'
} >"${FIXTURE}/Cargo.toml"

new_crate crates/isolated isolated
new_crate crates/leaf leaf
new_crate crates/mid mid
new_crate crates/top top
new_crate crates/devonly devonly
new_crate crates/base base
new_crate crates/consumer1 consumer1
new_crate crates/consumer2 consumer2

{
  echo ''
  echo '[dependencies]'
  echo 'leaf = { path = "../leaf" }'
} >>"${FIXTURE}/crates/mid/Cargo.toml"

{
  echo ''
  echo '[dependencies]'
  echo 'mid = { path = "../mid" }'
  echo ''
  echo '[dev-dependencies]'
  echo 'devonly = { path = "../devonly" }'
} >>"${FIXTURE}/crates/top/Cargo.toml"

{
  echo ''
  echo '[dependencies]'
  echo 'base = { path = "../base" }'
} >>"${FIXTURE}/crates/consumer1/Cargo.toml"

{
  echo ''
  echo '[dependencies]'
  echo 'consumer1 = { path = "../consumer1" }'
} >>"${FIXTURE}/crates/consumer2/Cargo.toml"

# A git repo so fallback_all_crates' `git rev-parse --show-toplevel` resolves
# inside the fixture (same as a real checkout whose cargo binary is broken),
# and so --staged mode has an index to diff against. No commit yet — the
# baseline commit happens after the --files-mode cases below, so any
# Cargo.lock those `cargo metadata` calls write lands in that commit rather
# than showing up as a stray untracked file for the --staged case.
(cd "${FIXTURE}" && git init -q . >/dev/null 2>&1)
(cd "${FIXTURE}" && git config user.email selftest@example.invalid >/dev/null 2>&1)
(cd "${FIXTURE}" && git config user.name selftest >/dev/null 2>&1)

ALL_EIGHT="base
consumer1
consumer2
devonly
isolated
leaf
mid
top"

# run <path>... — select-test-crates.sh --files <path>..., from the fixture.
run() {
  (cd "${FIXTURE}" && bash "${SCRIPT}" --files "$@" 2>/dev/null)
}

echo "fixture: leaf-crate change (no dependents) prints only itself"
assert_eq "isolated changes" \
  "isolated" "$(run crates/isolated/src/lib.rs)"

echo "fixture: forward closure — a dependency's change selects its dependents"
assert_eq "leaf changes -> leaf, mid, top (transitive)" \
  "leaf
mid
top" "$(run crates/leaf/src/lib.rs)"
assert_eq "mid changes -> mid, top (direct)" \
  "mid
top" "$(run crates/mid/src/lib.rs)"
assert_eq "devonly changes -> devonly, top (dev-dependency edge)" \
  "devonly
top" "$(run crates/devonly/src/lib.rs)"

echo "fixture: a base crate's change selects its full reverse closure"
assert_eq "base changes -> base, consumer1, consumer2 (two-hop reverse)" \
  "base
consumer1
consumer2" "$(run crates/base/src/lib.rs)"

echo "fixture: non-crate inputs"
assert_eq "docs-only change prints nothing" \
  "" "$(run docs/some-page.md)"
assert_eq "root-level *.md prints nothing" \
  "" "$(run README.md)"
assert_eq "Cargo.lock prints all crates" \
  "${ALL_EIGHT}" "$(run Cargo.lock)"
assert_eq "deny.toml prints all crates" \
  "${ALL_EIGHT}" "$(run deny.toml)"
# #7777 ruling (c): was "scripts/** prints all crates".
assert_eq "an unreferenced scripts/** path prints nothing" \
  "" "$(run scripts/some-gate.sh)"
assert_eq "an unknown path prints all crates (fail open)" \
  "${ALL_EIGHT}" "$(run some/unclassified/path.rs)"

echo "fixture: a broken cargo prints all crates and exits 0"
cat >"${STUB_DIR}/cargo" <<'EOF'
#!/usr/bin/env bash
exit 7
EOF
chmod +x "${STUB_DIR}/cargo"
broken_out="$(cd "${FIXTURE}" && PATH="${STUB_DIR}:${PATH}" bash "${SCRIPT}" --files crates/isolated/src/lib.rs 2>/dev/null)"
broken_exit=$?
assert_eq "broken cargo metadata -> all crates" "${ALL_EIGHT}" "${broken_out}"
assert_eq "broken cargo metadata -> exit 0" "0" "${broken_exit}"

echo "fixture: --cargo-args mode"
assert_eq "isolated --cargo-args -> -p isolated" \
  "-p isolated" "$(cd "${FIXTURE}" && bash "${SCRIPT}" --files crates/isolated/src/lib.rs --cargo-args 2>/dev/null)"

# #7777 review round 3: before the first commit HEAD is unborn, so --staged
# has no diff base and fails open. The exclude list hides every root input and
# crate so the untracked set is one docs file, which alone would select nothing.
echo "fixture: --staged before the first commit fails open"
printf '/*\n!/docs/\n' >"${FIXTURE}/.git/info/exclude"
mkdir -p "${FIXTURE}/docs" && echo probe >"${FIXTURE}/docs/staged-probe.md"
assert_eq "--staged with an unborn HEAD (docs-only untracked set) -> all crates" \
  "${ALL_EIGHT}" "$(cd "${FIXTURE}" && bash "${SCRIPT}" --staged 2>/dev/null)"
rm -rf "${FIXTURE}/docs"
: >"${FIXTURE}/.git/info/exclude"

echo "fixture: --staged mode sees an untracked file"
(cd "${FIXTURE}" && git add -A >/dev/null 2>&1 && git commit -qm base >/dev/null 2>&1)
(cd "${FIXTURE}" && mkdir -p crates/top/src && echo 'pub fn y() {}' >crates/top/src/extra.rs)
staged_out="$(cd "${FIXTURE}" && bash "${SCRIPT}" --staged 2>/dev/null)"
assert_eq "untracked crates/top/src/extra.rs -> top (--staged sees untracked too)" \
  "top" "${staged_out}"
rm -f "${FIXTURE}/crates/top/src/extra.rs"

echo "fixture: empty change set fails open"
empty_out="$(cd "${FIXTURE}" && bash "${SCRIPT}" --files 2>/dev/null)"
assert_eq "--files with no paths -> all crates" "${ALL_EIGHT}" "${empty_out}"

# ---------------------------------------------------------------------------
# --range mode (#7777 review, finding 4): the original selftest never drove
# `--range` at all, which is exactly where the missing-value infinite loop
# lived (finding 1) — a coverage gap, not a broken assertion mechanism.
# BASE_SHA is the "base" commit made for the --staged case above; a second
# commit here gives --range a real two-commit diff to resolve.
# ---------------------------------------------------------------------------
echo "fixture: --range mode"

BASE_SHA="$(cd "${FIXTURE}" && git rev-parse HEAD)"
(cd "${FIXTURE}" && echo 'pub fn changed() {}' >>crates/mid/src/lib.rs)
(cd "${FIXTURE}" && git add crates/mid/src/lib.rs >/dev/null 2>&1 && git commit -qm "range fixture: touch mid" >/dev/null 2>&1)
RANGE_SHA="$(cd "${FIXTURE}" && git rev-parse HEAD)"

assert_eq "--range <base>..<head> touching mid -> mid, top (same closure as --files)" \
  "mid
top" "$(cd "${FIXTURE}" && bash "${SCRIPT}" --range "${BASE_SHA}..${RANGE_SHA}" 2>/dev/null)"

# Bounded-time: at 7e4efe7f3 (pre-fix, #7777 review round 1) a bare trailing
# `--range` (no value) spins forever at ~100% CPU instead of failing open
# (finding 1) — `timeout` turns that hang into a bounded, assertable
# failure instead of stalling this whole selftest run.
range_missing_out="$(cd "${FIXTURE}" && timeout 8 bash "${SCRIPT}" --range 2>/dev/null)"
range_missing_exit=$?
assert_eq "--range with no value terminates promptly (not a 124 timeout)" \
  "0" "${range_missing_exit}"
assert_eq "--range with no value fails open -> all crates, never nothing" \
  "${ALL_EIGHT}" "${range_missing_out}"

# #7777 review round 2: ci.yml passes `--range ""` when merge-base fails.
assert_eq "--range \"\" (explicit empty string) fails open -> all crates" \
  "${ALL_EIGHT}" "$(cd "${FIXTURE}" && bash "${SCRIPT}" --range "" 2>/dev/null)"

assert_eq "--range with an unresolvable ref fails open -> all crates" \
  "${ALL_EIGHT}" "$(cd "${FIXTURE}" && bash "${SCRIPT}" --range 'no-such-ref..also-fake' 2>/dev/null)"

# #7777 review round 2, MEDIUM: `--range` must not swallow the next
# recognized flag as its value — `--range --cargo-args` used to silently
# take "--cargo-args" as RANGE_SPEC, the resulting git diff failed, and the
# script fell open in the WRONG output format (plain names, not `-p a -p b`)
# instead of respecting the caller's `--cargo-args` request.
assert_eq "--range --cargo-args does not swallow --cargo-args as the range value" \
  "-p base -p consumer1 -p consumer2 -p devonly -p isolated -p leaf -p mid -p top" \
  "$(cd "${FIXTURE}" && bash "${SCRIPT}" --range --cargo-args 2>/dev/null)"

# ---------------------------------------------------------------------------
# nested workspace member reached through an early fail-open (#7777 review
# round 2, HIGH finding 1): `fail_open()` used to skip straight to
# `fallback_all_crates()`'s shallow, one-level `crates/*/Cargo.toml` scan
# whenever ALL_CRATES was not already populated — true for most fail-open
# triggers, including `--files` with no paths, since that fires in step 1,
# before `cargo metadata` has ever run. That shallow scan cannot see a
# member nested inside another crate's directory (this repo's own
# `trusty-agents-ui`, `trusty-audit-ui` — see the root Cargo.toml's
# `members` list). A separate, minimal workspace pins the shape without
# touching the shared 8-crate FIXTURE above.
# ---------------------------------------------------------------------------
echo "fixture: a nested workspace member survives an early fail-open"

NESTED="${WORK}/nested"
mkdir -p "${NESTED}/crates/alpha/src" "${NESTED}/crates/alpha/ui/src-tauri/src"
: >"${NESTED}/crates/alpha/src/lib.rs"
: >"${NESTED}/crates/alpha/ui/src-tauri/src/lib.rs"
{
  echo '[package]'
  echo 'name = "alpha"'
  echo 'version = "0.1.0"'
  echo 'edition = "2021"'
} >"${NESTED}/crates/alpha/Cargo.toml"
{
  echo '[package]'
  echo 'name = "alpha-ui"'
  echo 'version = "0.1.0"'
  echo 'edition = "2021"'
} >"${NESTED}/crates/alpha/ui/src-tauri/Cargo.toml"
{
  echo '[workspace]'
  echo 'resolver = "2"'
  echo 'members = ["crates/*", "crates/alpha/ui/src-tauri"]'
} >"${NESTED}/Cargo.toml"

nested_out="$(cd "${NESTED}" && bash "${SCRIPT}" --files 2>/dev/null)"
assert_eq "fail-open before cargo metadata has run still finds a nested member" \
  "alpha
alpha-ui" "${nested_out}"

# ---------------------------------------------------------------------------
# scripts/** and .github/** (#7777 owner ruling 2026-09-23). Its own
# workspace, named after the real crates the ruling cites, so the canary set
# and the UI-crate relevance list resolve without an override:
#
#   trusty-mpm        src names "scripts/check_changelog_fragment.sh" in code
#                     and "scripts/unrelated.sh" only in comments
#   trusty-search     build.rs names "scripts/check-ui-bundle-freshness.sh"
#   trusty-console    build.rs names "../../scripts/check-ui-bundle-freshness.sh"
#   search-consumer   depends on trusty-search — rule 4 must NOT select it
#   bystander         test fixture strings "scripts/go.sh", "scripts/ingest.sh"
#                     — neither file exists; src/paths.rs names dot.sh, fmt.sh,
#                     up.sh and abs.sh through `./`, `{r}/`, `../` and `/abs/`
#                     prefixes, mine.sh only as "myscripts/mine.sh", and
#                     include_str!s h.sh (deleted / renamed in a --range copy)
#   trusty-mpm-gui    the one Tauri UI crate ci-crate-relevance.sh is asked about
#   scripts/sign.sh   contains `codesign`; scripts/sub/sign.sh and
#                     scripts/sign.txt do too but sit outside the scan's scope
# ---------------------------------------------------------------------------
echo "fixture: scripts/** and .github/** selection rules (#7777 ruling)"

SR="${WORK}/scriptref"
sr_crate() {
  local name="$1"
  mkdir -p "${SR}/crates/${name}/src"
  : >"${SR}/crates/${name}/src/lib.rs"
  printf '[package]\nname = "%s"\nversion = "0.1.0"\nedition = "2021"\n' "$name" >"${SR}/crates/${name}/Cargo.toml"
}
mkdir -p "${SR}/scripts" "${SR}/.github/workflows"
printf '[workspace]\nresolver = "2"\nmembers = ["crates/*"]\n' >"${SR}/Cargo.toml"
for c in trusty-common trusty-mpm trusty-search trusty-console search-consumer bystander trusty-mpm-gui; do
  sr_crate "$c"
done
printf '\n[dependencies]\ntrusty-search = { path = "../trusty-search" }\n' >>"${SR}/crates/search-consumer/Cargo.toml"
cat >"${SR}/crates/trusty-mpm/src/lib.rs" <<'EOF'
// Runs scripts/unrelated.sh? No: this comment must not count.
/// Nor does this doc line naming "scripts/unrelated.sh".
pub fn gate(root: &std::path::Path) -> std::path::PathBuf {
    root.join("scripts/check_changelog_fragment.sh")
}
EOF
cat >"${SR}/crates/trusty-search/build.rs" <<'EOF'
fn main() {
    let _ = std::path::Path::new(".").join("scripts/check-ui-bundle-freshness.sh");
}
EOF
cat >"${SR}/crates/trusty-console/build.rs" <<'EOF'
fn main() {
    let _ = std::path::Path::new("../../scripts/check-ui-bundle-freshness.sh");
}
EOF
mkdir -p "${SR}/crates/bystander/tests"
cat >"${SR}/crates/bystander/tests/fixtures.rs" <<'EOF'
#[test]
fn fixture_strings() {
    assert_ne!("scripts/go.sh", "scripts/ingest.sh");
}
EOF
cat >"${SR}/crates/bystander/src/paths.rs" <<'EOF'
pub const HOOK: &str = include_str!("../../../scripts/h.sh");
pub fn paths(r: &str) -> [String; 5] {
    [
        "./scripts/dot.sh".to_string(),
        format!("{r}/scripts/fmt.sh"),
        "../scripts/up.sh".to_string(),
        "/abs/scripts/abs.sh".to_string(),
        "myscripts/mine.sh".to_string(),
    ]
}
EOF
for f in scripts/check_changelog_fragment.sh scripts/check-ui-bundle-freshness.sh scripts/unrelated.sh \
  scripts/dot.sh scripts/fmt.sh scripts/up.sh scripts/abs.sh scripts/mine.sh scripts/h.sh \
  .github/workflows/ci.yml .github/workflows/other.yml; do
  echo '# fixture' >"${SR}/${f}"
done
# Codesign rule: in scope only as scripts/<name>.sh, like trusty-common's scan.
mkdir -p "${SR}/scripts/sub"
echo 'codesign --force --sign - "$APP"' >"${SR}/scripts/sign.sh"
echo 'codesign --force --sign - "$APP"' >"${SR}/scripts/sub/sign.sh"
echo 'codesign --force --sign - "$APP"' >"${SR}/scripts/sign.txt"
echo 'echo "no signing here"' >"${SR}/scripts/nosign.sh"
(cd "${SR}" && git init -q . && git config user.email selftest@example.invalid &&
  git config user.name selftest && git add -A && git commit -qm base) >/dev/null 2>&1

sr_run() { (cd "${SR}" && bash "${SCRIPT}" --files "$@" 2>/dev/null); }

assert_eq "ci.yml-only diff -> canary + the one relevant UI crate" \
  "trusty-common
trusty-mpm
trusty-mpm-gui" "$(sr_run .github/workflows/ci.yml)"
assert_eq "select-test-crates.sh diff -> canary only (UI crate inert)" \
  "trusty-common
trusty-mpm" "$(sr_run scripts/select-test-crates.sh)"
assert_eq "check_changelog_fragment.sh -> trusty-mpm only" \
  "trusty-mpm" "$(sr_run scripts/check_changelog_fragment.sh)"
assert_eq "check-ui-bundle-freshness.sh -> trusty-console + trusty-search" \
  "trusty-console
trusty-search" "$(sr_run scripts/check-ui-bundle-freshness.sh)"
assert_eq "a script named only in comments -> nothing" \
  "" "$(sr_run scripts/unrelated.sh)"
assert_eq "an unreferenced .github/** file -> nothing" \
  "" "$(sr_run .github/workflows/other.yml)"
assert_eq "fixture-shaped literal with no file on disk -> nothing" \
  "" "$(sr_run scripts/ingest.sh)"
assert_eq "crate change + fixture-shaped nonexistent path -> nothing extra" \
  "trusty-mpm-gui" "$(sr_run crates/trusty-mpm-gui/src/lib.rs scripts/go.sh)"

# #7777 review round 2: a `/` before the path is a boundary; a name byte is not.
assert_eq "path form \"./scripts/dot.sh\" -> bystander" \
  "bystander" "$(sr_run scripts/dot.sh)"
assert_eq "path form format!(\"{r}/scripts/fmt.sh\") -> bystander" \
  "bystander" "$(sr_run scripts/fmt.sh)"
assert_eq "path form \"../scripts/up.sh\" -> bystander" \
  "bystander" "$(sr_run scripts/up.sh)"
assert_eq "path form \"/abs/scripts/abs.sh\" -> bystander" \
  "bystander" "$(sr_run scripts/abs.sh)"
assert_eq "\"myscripts/mine.sh\" does not name scripts/mine.sh -> nothing" \
  "" "$(sr_run scripts/mine.sh)"

# #7777 ruling 2026-09-23 23:17Z: a scripts/*.sh containing `codesign` is in
# trusty-common's codesign_scripts scan, so it selects trusty-common. Same
# scope as that scan: no subdirectory, extension exactly `sh`.
assert_eq "codesign: scripts/sign.sh containing codesign -> trusty-common" \
  "trusty-common" "$(sr_run scripts/sign.sh)"
assert_eq "codesign: scripts/nosign.sh without codesign -> nothing" \
  "" "$(sr_run scripts/nosign.sh)"
assert_eq "codesign: scripts/sub/sign.sh (subdirectory) -> nothing" \
  "" "$(sr_run scripts/sub/sign.sh)"
assert_eq "codesign: scripts/sign.txt (not .sh) -> nothing" \
  "" "$(sr_run scripts/sign.txt)"
SR_NOCOMMON="${WORK}/scriptref-nocommon"
cp -R "${SR}" "${SR_NOCOMMON}" && rm -rf "${SR_NOCOMMON}/crates/trusty-common"
assert_eq "codesign: trusty-common not a workspace member -> all crates" \
  "bystander
search-consumer
trusty-console
trusty-mpm
trusty-mpm-gui
trusty-search" "$(cd "${SR_NOCOMMON}" && bash "${SCRIPT}" --files scripts/sign.sh 2>/dev/null)"

# A canary missing from the workspace fails open rather than testing less.
SR_NOCANARY="${WORK}/scriptref-nocanary"
cp -R "${SR}" "${SR_NOCANARY}" && rm -rf "${SR_NOCANARY}/crates/trusty-mpm"
assert_eq "canary crate not a workspace member -> all crates (fail open)" \
  "bystander
search-consumer
trusty-common
trusty-console
trusty-mpm-gui
trusty-search" "$(cd "${SR_NOCANARY}" && bash "${SCRIPT}" --files .github/workflows/ci.yml 2>/dev/null)"

# #7777 review round 2: a script deleted or renamed in the range is absent on
# disk but existed at the range base, so the crate naming it is still selected.
SR_RANGE="${WORK}/scriptref-range"
cp -R "${SR}" "${SR_RANGE}"
srr() { (cd "${SR_RANGE}" && "$@") >/dev/null 2>&1; }
SRR_BASE="$(cd "${SR_RANGE}" && git rev-parse HEAD)"
srr git rm -q scripts/h.sh
srr git commit -qm "delete h.sh"
SRR_DEL="$(cd "${SR_RANGE}" && git rev-parse HEAD)"
assert_eq "--range deleting include_str!'d scripts/h.sh -> bystander" \
  "bystander" "$(cd "${SR_RANGE}" && bash "${SCRIPT}" --range "${SRR_BASE}..${SRR_DEL}" 2>/dev/null)"
srr git checkout -q "${SRR_BASE}"
srr git mv scripts/h.sh scripts/h-renamed.sh
srr git commit -qm "rename h.sh"
SRR_REN="$(cd "${SR_RANGE}" && git rev-parse HEAD)"
assert_eq "--range a...b renaming scripts/h.sh -> bystander" \
  "bystander" "$(cd "${SR_RANGE}" && bash "${SCRIPT}" --range "${SRR_BASE}...${SRR_REN}" 2>/dev/null)"
srr git checkout -q "${SRR_BASE}"
srr git rm -q scripts/h.sh
# The earlier runs' untracked Cargo.lock would read as a root input (ALL).
echo Cargo.lock >>"${SR_RANGE}/.git/info/exclude"
assert_eq "--staged deletion of scripts/h.sh -> bystander" \
  "bystander" "$(cd "${SR_RANGE}" && bash "${SCRIPT}" --staged 2>/dev/null)"
# The range names scripts/go.sh, but HEAD sits at the base: the path is on
# neither the disk nor the base, so bystander's fixture string still counts
# for nothing.
srr git reset -q --hard "${SRR_BASE}"
srr sh -c 'echo "# fixture" >scripts/go.sh && git add scripts/go.sh && git commit -qm "add go.sh"'
SRR_GO="$(cd "${SR_RANGE}" && git rev-parse HEAD)"
srr git checkout -q "${SRR_BASE}"
assert_eq "--range naming a path absent on disk and at base -> nothing" \
  "" "$(cd "${SR_RANGE}" && bash "${SCRIPT}" --range "${SRR_BASE}..${SRR_GO}" 2>/dev/null)"
# A codesign script deleted in the range, or edited to drop codesign, left
# the scan's set: its content at the range base still selects trusty-common.
srr git rm -q scripts/sign.sh
srr git commit -qm "delete sign.sh"
SRR_SIGNDEL="$(cd "${SR_RANGE}" && git rev-parse HEAD)"
assert_eq "codesign: --range deleting scripts/sign.sh -> trusty-common" \
  "trusty-common" "$(cd "${SR_RANGE}" && bash "${SCRIPT}" --range "${SRR_BASE}..${SRR_SIGNDEL}" 2>/dev/null)"
srr git checkout -q "${SRR_BASE}"
srr sh -c 'echo "echo unsigned" >scripts/sign.sh && git commit -qam "drop codesign from sign.sh"'
SRR_SIGNEDIT="$(cd "${SR_RANGE}" && git rev-parse HEAD)"
assert_eq "codesign: --range dropping codesign from sign.sh -> trusty-common" \
  "trusty-common" "$(cd "${SR_RANGE}" && bash "${SCRIPT}" --range "${SRR_BASE}..${SRR_SIGNEDIT}" 2>/dev/null)"
# #7777 review round 2, HIGH: `<sha>^!` is one commit's own diff. Its base is
# `<sha>^`; read as a literal ref it resolved nothing and selected nothing.
# HEAD sits at each deleting commit so the deleted file is absent on disk.
srr git checkout -q "${SRR_DEL}"
assert_eq "--range <sha>^! deleting include_str!'d scripts/h.sh -> bystander" \
  "bystander" "$(cd "${SR_RANGE}" && bash "${SCRIPT}" --range "${SRR_DEL}^!" 2>/dev/null)"
srr git checkout -q "${SRR_SIGNDEL}"
assert_eq "codesign: --range <sha>^! deleting scripts/sign.sh -> trusty-common" \
  "trusty-common" "$(cd "${SR_RANGE}" && bash "${SCRIPT}" --range "${SRR_SIGNDEL}^!" 2>/dev/null)"
# A base git diff accepts but that is not one commit fails open, never empty.
assert_eq "--range <sha>^- (base is not one commit) -> all crates" \
  "bystander
search-consumer
trusty-common
trusty-console
trusty-mpm
trusty-mpm-gui
trusty-search" "$(cd "${SR_RANGE}" && bash "${SCRIPT}" --range "${SRR_SIGNDEL}^-" 2>/dev/null)"
# #7777 review round 3: a merge commit's `^!` is a combined diff, which omits
# a change one parent already carried. Evil merge: the first parent deletes
# check_changelog_fragment.sh (named by trusty-mpm), the merge itself adds
# zz-evil.md. Both merges fail open.
SR_ALL="bystander
search-consumer
trusty-common
trusty-console
trusty-mpm
trusty-mpm-gui
trusty-search"
srr git checkout -q "${SRR_BASE}"
srr git rm -q scripts/check_changelog_fragment.sh
srr git commit -qm "first parent: delete check_changelog_fragment.sh"
SRR_P1="$(cd "${SR_RANGE}" && git rev-parse HEAD)"
srr git checkout -q "${SRR_BASE}"
srr sh -c 'echo side >side.md && git add side.md && git commit -qm "second parent: side.md"'
SRR_P2="$(cd "${SR_RANGE}" && git rev-parse HEAD)"
srr git checkout -q "${SRR_P1}"
srr git merge -q --no-ff --no-commit "${SRR_P2}"
srr sh -c 'echo evil >zz-evil.md && git add zz-evil.md && git commit -qm "evil merge"'
SRR_EVIL="$(cd "${SR_RANGE}" && git rev-parse HEAD)"
assert_eq "--range <evil-merge>^! -> all crates, never empty" \
  "${SR_ALL}" "$(cd "${SR_RANGE}" && bash "${SCRIPT}" --range "${SRR_EVIL}^!" 2>/dev/null)"
srr git checkout -q "${SRR_P1}"
srr git merge -q --no-ff -m "clean merge" "${SRR_P2}"
SRR_CLEAN="$(cd "${SR_RANGE}" && git rev-parse HEAD)"
assert_eq "--range <clean-merge>^! -> all crates" \
  "${SR_ALL}" "$(cd "${SR_RANGE}" && bash "${SCRIPT}" --range "${SRR_CLEAN}^!" 2>/dev/null)"

# The literal scan needs git; outside a repo it must fail open, never answer
# "no reference". Asserted on a path that answers `trusty-mpm` when git works.
SR_NOGIT="${WORK}/scriptref-nogit"
cp -R "${SR}" "${SR_NOGIT}" && rm -rf "${SR_NOGIT}/.git"
assert_eq "literal scan unavailable (no git repo) -> all crates" \
  "bystander
search-consumer
trusty-common
trusty-console
trusty-mpm
trusty-mpm-gui
trusty-search" "$(cd "${SR_NOGIT}" && bash "${SCRIPT}" --files scripts/check_changelog_fragment.sh 2>/dev/null)"

# ---------------------------------------------------------------------------
# #7777 review round 3: a fail-open that can name no crate used to exit 0 with
# empty stdout, which ci-affected-test-plan.sh reads as count=0 and a green
# check. Each such arm exits 3. EMPTY_DIR has no workspace and no git repo, so
# neither cargo metadata nor the crates/*/Cargo.toml fallback finds a crate.
# ---------------------------------------------------------------------------
echo "fixture: a fail-open that can name no crate exits 3"
EMPTY_DIR="${WORK}/empty"
mkdir -p "${EMPTY_DIR}"
(cd "${EMPTY_DIR}" && bash "${SCRIPT}" --files >/dev/null 2>&1)
assert_eq "fail-open with an empty fallback scan -> exit 3" "3" "$?"
# A failing mktemp stub, not a TMPDIR under a regular file: macOS
# /usr/bin/mktemp -d ignores TMPDIR when given no template.
MKTEMP_STUB_DIR="${WORK}/stub-mktemp"
mkdir -p "${MKTEMP_STUB_DIR}"
printf '#!/usr/bin/env bash\nexit 1\n' >"${MKTEMP_STUB_DIR}/mktemp"
chmod +x "${MKTEMP_STUB_DIR}/mktemp"
(cd "${FIXTURE}" && PATH="${MKTEMP_STUB_DIR}:${PATH}" bash "${SCRIPT}" --files crates/isolated/src/lib.rs >/dev/null 2>&1)
assert_eq "mktemp -d failure -> exit 3" "3" "$?"

# ---------------------------------------------------------------------------
# bash 3.2 path (#7777 review, finding 2): macOS ships bash 3.2.57 as
# /bin/bash. `declare -A` there is a non-fatal error under `set -uo
# pipefail` (no `-e`), so the unguarded script fell through to exit 0 with
# EMPTY stdout for a real crate-affecting change — indistinguishable from a
# legitimate "nothing to test" answer. Only meaningful where /bin/bash is
# actually pre-4 (this host); a Linux CI runner's /bin/bash is typically
# bash 4+ already, so this skips there rather than asserting something that
# was never reachable.
# ---------------------------------------------------------------------------
echo "fixture: bash 3.2 compatibility guard"
LEGACY_BASH="/bin/bash"
LEGACY_MAJOR=""
# shellcheck disable=SC2016  # single-quoted: BASH_VERSINFO must expand inside the child bash, not this one
[ -x "${LEGACY_BASH}" ] && LEGACY_MAJOR="$("${LEGACY_BASH}" -c 'echo "${BASH_VERSINFO[0]}"' 2>/dev/null)"
if [ -n "${LEGACY_MAJOR}" ] && [ "${LEGACY_MAJOR}" -lt 4 ] 2>/dev/null; then
  legacy_stderr="${WORK}/legacy-bash.stderr"
  legacy_out="$(cd "${FIXTURE}" && "${LEGACY_BASH}" "${SCRIPT}" --files crates/mid/src/lib.rs 2>"${legacy_stderr}")"
  assert_eq "bash <4: real crate change never prints empty (fails open to all crates)" \
    "${ALL_EIGHT}" "${legacy_out}"
  case "$(cat "${legacy_stderr}" 2>/dev/null)" in
    *"bash 4+"*)
      CASES=$((CASES + 1))
      printf '  ok   %-62s -> %s\n' "bash <4 warns loudly on stderr" "present"
      ;;
    *)
      fail "bash <4 stderr warning missing"
      ;;
  esac
  (cd "${EMPTY_DIR}" && "${LEGACY_BASH}" "${SCRIPT}" --files x >/dev/null 2>&1)
  assert_eq "bash <4 with no crate to name -> exit 3" "3" "$?"
else
  echo "  skip: /bin/bash on this host is not pre-4 — nothing to guard here (macOS repro in #7777 review)"
fi

echo "live: this repo's own trusty-common feature override"
live_out="$(cd "${REPO_ROOT}" && bash "${SCRIPT}" --files crates/trusty-common/src/lib.rs --cargo-args 2>/dev/null)"
case "${live_out}" in
  *"-p trusty-common --features unconditional-only"*)
    CASES=$((CASES + 1))
    printf '  ok   %-62s -> %s\n' "trusty-common --cargo-args carries the override" "present"
    ;;
  *)
    fail "trusty-common --cargo-args override missing: got '${live_out}'"
    ;;
esac

echo "live: this repo's own scripts/** literal edges (#7777 ruling)"
live_run() { (cd "${REPO_ROOT}" && bash "${SCRIPT}" --files "$@" 2>/dev/null); }
assert_eq "live: check_changelog_fragment.sh -> trusty-mpm" \
  "trusty-mpm" "$(live_run scripts/check_changelog_fragment.sh)"
assert_eq "live: check-ui-bundle-freshness.sh -> trusty-console + trusty-search" \
  "trusty-console
trusty-search" "$(live_run scripts/check-ui-bundle-freshness.sh)"
assert_eq "live: codesign build-console-saver.sh -> trusty-common" \
  "trusty-common" "$(live_run scripts/build-console-saver.sh)"
assert_eq "live: codesign install-trusty-mpm-signed.sh -> trusty-common" \
  "trusty-common" "$(live_run scripts/install-trusty-mpm-signed.sh)"
# The job's helper scripts select the canary. The Tauri UI crates the
# relevance union may add are that rule's business, not this assertion's.
live_canary() {
  live_run "$1" | grep -vxE 'trusty-(agents-ui|audit-ui|code-gui|mpm-gui)'
}
for helper in ci-create-local-main.sh ci-free-disk-space.sh ci-apt-install.sh; do
  assert_eq "live: helper ${helper} -> canary trusty-common + trusty-mpm" \
    "trusty-common
trusty-mpm" "$(live_canary "scripts/${helper}")"
done

echo
echo "${CASES} cases, ${FAILURES} failures"
[ "${FAILURES}" -eq 0 ]
