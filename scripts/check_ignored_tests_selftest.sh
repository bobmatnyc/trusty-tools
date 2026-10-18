#!/usr/bin/env bash
#
# check_ignored_tests_selftest.sh — prove scripts/check_ignored_tests.sh counts
# feature-gated `#[ignore]` tests and fails closed when it cannot.
#
# Why: #9663. Gate 4 enumerated with default features only, so an ignored test
#   compiled only under a non-default feature (trusty-mpm's `sm-memory`
#   children, #9658) entered the tree without moving the ratchet. The fix adds
#   a second, featured enumeration; every way that enumeration can fail must
#   stop the gate rather than read as "0 found".
#
# What: copies the gate into a throwaway git repo next to a fixture manifest,
#   puts a stub `cargo` first on PATH, and drives the gate's LIVE path. The stub
#   answers `cargo nextest list` with one canned JSON stream when `--features`
#   is absent and another when it is present, and records every invocation, so
#   each case pins both the verdict (exit code) and the cargo command shape.
#
# Exit codes: 0 = every case behaved; 1 = a case did not.
#
# Test: this IS the test; .github/workflows/pre-publish.yml runs it before
#   Gate 4.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE_SRC="${SCRIPT_DIR}/check_ignored_tests.sh"
WORK="$(mktemp -d -t check_ignored_tests_selftest.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

FAILURES=0

# ── A throwaway repo: the gate resolves its manifest from its own git root ──
REPO="${WORK}/repo"
mkdir -p "${REPO}/scripts" "${WORK}/bin" "${WORK}/fx"
git -C "$REPO" init -q
cp "$GATE_SRC" "${REPO}/scripts/check_ignored_tests.sh"
GATE="${REPO}/scripts/check_ignored_tests.sh"

# ── The stub cargo. Env: STUB_DEFAULT_JSON / STUB_DEFAULT_RC for the default
#    listing, STUB_FEAT_JSON / STUB_FEAT_RC for the featured one, STUB_RUN_RC
#    for `nextest run`. Every invocation is appended to STUB_LOG. ──
cat > "${WORK}/bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_LOG"
[ "${1:-}" = "nextest" ] || { echo "stub cargo: unexpected: $*" >&2; exit 97; }
case "${2:-}" in
  list)
    case " $* " in
      *" --features "*) cat "$STUB_FEAT_JSON"; exit "${STUB_FEAT_RC:-0}" ;;
      *)                cat "$STUB_DEFAULT_JSON"; exit "${STUB_DEFAULT_RC:-0}" ;;
    esac ;;
  run) exit "${STUB_RUN_RC:-0}" ;;
  *) echo "stub cargo: unexpected nextest verb: $*" >&2; exit 97 ;;
esac
EOF
chmod +x "${WORK}/bin/cargo"

# ── Fixtures: the shape of `cargo nextest list --message-format json` ──

# Default features: one selectable test and two unselected ones.
cat > "${WORK}/fx/default.json" <<'EOF'
{"rust-suites": {
  "crate-a": {"package-name": "crate-a", "testcases": {
    "embed::tests::loads_model": {"ignored": true},
    "live::tests::needs_tmux": {"ignored": true}}},
  "crate-b::bin/b": {"package-name": "crate-b", "testcases": {
    "soak::runs_for_minutes": {"ignored": true}}}}}
EOF

# The featured build: the same three plus one test that only exists under the
# feature — the #9658 `hold_the_sm_palace_as_writer_child` shape.
cat > "${WORK}/fx/featured.json" <<'EOF'
{"rust-suites": {
  "crate-a": {"package-name": "crate-a", "testcases": {
    "embed::tests::loads_model": {"ignored": true},
    "live::tests::needs_tmux": {"ignored": true},
    "core::sm::memory_tests::hold_the_palace_as_writer_child": {"ignored": true}}},
  "crate-b::bin/b": {"package-name": "crate-b", "testcases": {
    "soak::runs_for_minutes": {"ignored": true}}}}}
EOF

# A featured build that compiled but listed nothing.
echo '{"rust-suites": {}}' > "${WORK}/fx/empty.json"
# A featured build whose stdout is not the JSON stream at all.
echo "error: could not compile crate-a" > "${WORK}/fx/garbage.txt"

# write_manifest <baseline> [extra row] — one RUN row selecting loads_model.
write_manifest() {
  {
    printf 'crate-a\tembed::tests::loads_model\tfixture row\n'
    [ -z "${2:-}" ] || printf '%s\n' "$2"
    printf '# UNSELECTED-BASELINE: %s\n' "$1"
  } > "${REPO}/scripts/prepublish-ignored-tests.tsv"
}

# expect_exit <want> <name> <grep-pattern|-> <gate args...>
# Runs the gate through the stub; asserts its exit code and, unless the
# pattern is `-`, that its combined output matches the pattern.
expect_exit() {
  local want="$1" name="$2" pat="$3"
  shift 3
  local got=0
  : > "${WORK}/cargo.log"
  STUB_LOG="${WORK}/cargo.log" PATH="${WORK}/bin:${PATH}" \
    bash "$GATE" "$@" > "${WORK}/out.txt" 2>&1 || got=$?
  if [ "$got" -ne "$want" ]; then
    echo "  FAIL  $name — wanted exit $want, got $got" >&2
    sed 's/^/        /' "${WORK}/out.txt" >&2
    FAILURES=$((FAILURES + 1))
  elif [ "$pat" != "-" ] && ! grep -qE -- "$pat" "${WORK}/out.txt"; then
    echo "  FAIL  $name — exit $got as wanted, but no line matches /$pat/" >&2
    sed 's/^/        /' "${WORK}/out.txt" >&2
    FAILURES=$((FAILURES + 1))
  else
    echo "  ok    $name (exit $got)"
  fi
}

# expect_log <name> <grep-pattern> [absent] — assert on the recorded cargo calls.
expect_log() {
  local name="$1" pat="$2" mode="${3:-present}" hit=0
  grep -qE -- "$pat" "${WORK}/cargo.log" && hit=1
  if { [ "$mode" = present ] && [ "$hit" -eq 1 ]; } \
    || { [ "$mode" = absent ] && [ "$hit" -eq 0 ]; }; then
    echo "  ok    $name"
  else
    echo "  FAIL  $name — /$pat/ expected $mode in the cargo calls:" >&2
    sed 's/^/        /' "${WORK}/cargo.log" >&2
    FAILURES=$((FAILURES + 1))
  fi
}

echo "check_ignored_tests selftest"

export STUB_DEFAULT_JSON="${WORK}/fx/default.json" STUB_DEFAULT_RC=0
export STUB_FEAT_JSON="${WORK}/fx/featured.json" STUB_FEAT_RC=0 STUB_RUN_RC=0

# ── #9663: the feature-only test is counted (criteria 1 and 2) ──

# Baseline 2 is the default-feature count: the unseen third test must turn
# the ratchet red. The pre-#9663 gate read 2 and passed.
write_manifest 2
expect_exit 1 "a feature-only #[ignore] test above the baseline trips the ratchet" \
  "FAIL	RATCHET	3 ignored" --list-only
expect_log "the featured enumeration passes the explicit feature list" \
  "nextest list .*--features trusty-mpm/sm-memory,trusty-mpm/manager-memory,trusty-mpm/memory-test-support"

write_manifest 3
expect_exit 0 "the feature-only test is counted and named once the baseline holds it" \
  "FEATURE-ONLY	crate-a	core::sm::memory_tests::hold_the_palace_as_writer_child" --list-only
expect_exit 0 "the summary counts the union of both enumerations" \
  "4 ignored test\(s\) in the workspace; 1 selected to RUN; 3 NOT run" --list-only

# ── Fail-closed arms of the featured enumeration (criterion 4) ──

STUB_FEAT_RC=101 expect_exit 3 "a featured build that fails to compile is refused, not read as 0" \
  "--features .* exited 101" --list-only
STUB_FEAT_JSON="${WORK}/fx/empty.json" expect_exit 3 \
  "a featured enumeration listing zero ignored tests is refused" "FAIL	VACUOUS" --list-only
STUB_FEAT_JSON="${WORK}/fx/garbage.txt" expect_exit 3 \
  "a featured enumeration that is not JSON is refused" "FAIL	ENUMERATION" --list-only

# ── The default-feature arms still hold ──

STUB_DEFAULT_RC=101 expect_exit 3 "a failed default enumeration is refused" \
  "exited 101" --list-only
STUB_DEFAULT_JSON="${WORK}/fx/empty.json" expect_exit 3 \
  "a default enumeration listing zero ignored tests is refused" "FAIL	VACUOUS" --list-only

# ── A row the run phase cannot honour (criterion 6) ──

write_manifest 2 "$(printf 'crate-a\tcore::sm::memory_tests::hold_the_palace_as_writer_child\tfixture')"
expect_exit 3 "a RUN row selecting a feature-only test is refused" \
  "FAIL	ROW-NEEDS-FEATURES" --list-only

# ── The run phase keeps default features and the same selection (criterion 5) ──

write_manifest 3
expect_exit 0 "the run phase runs the default-feature selection" "-"
expect_log "the run phase is invoked with the selected filterset" \
  "nextest run .*-E \(package\(crate-a\) and test\(=embed::tests::loads_model\)\)$"
expect_log "the run phase does not pass the feature list" "nextest run .*--features" absent
STUB_RUN_RC=100 expect_exit 1 "a failing selected test fails the gate" "-"

# ── The captured-stream entry point needs both streams ──

expect_exit 0 "--from-list scores a captured pair of streams" "3 NOT run" \
  --from-list "${WORK}/fx/default.json" --from-features-list "${WORK}/fx/featured.json"
expect_exit 2 "--from-list without the featured stream is a usage error" "-" \
  --from-list "${WORK}/fx/default.json"

if [ "$FAILURES" -ne 0 ]; then
  echo "check_ignored_tests selftest: $FAILURES case(s) failed" >&2
  exit 1
fi

echo "check_ignored_tests selftest: all cases passed"
