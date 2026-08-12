#!/usr/bin/env bash
#
# check_teardown_guard_selftest.sh — mutation self-test for
# scripts/check_teardown_guard.sh (issue #3049 round 4).
#
# Why: the gate's whole value is that it FAILS on a shape a human reviewer
#   missed three times. A gate whose matcher silently stopped matching reports
#   "OK: … every unguarded site declared" identically to a correct tree, and this
#   repo has shipped that exact bug more than once (#4618, #5440). So the gate's
#   ability to fail is itself tested here, against fixtures that reproduce the
#   three real misses:
#     - a call with NO guard anywhere;
#     - a guard that exists but sits AFTER the write (watch_loop::handle_modified,
#       where a `remove_chunk` loop ran before the acquisition);
#     - a call inside a `tokio::spawn` whose guard is in the enclosing function,
#       which is dropped when that function returns (commands/start/daemon.rs).
#   Plus the fail-closed cases: an empty scan, a stale manifest row, and a
#   CALLER:<fn> naming a function that does not take a guard.
#
# What: builds a throwaway git repo per case, copies the gate and its data files
#   in so the gate's own `SCRIPT_DIR`-derived repo root resolves inside the
#   fixture, and asserts the exit status and the message. The fixtures are padded
#   with enough filler files and correctly-guarded call sites to clear the gate's
#   scan floors, so each case fails (or passes) for the reason under test rather
#   than for a floor breach — except the `empty_scan` case, whose whole point is
#   the floor.
#
# Usage:  bash scripts/check_teardown_guard_selftest.sh
# Exit:   0 when every case behaves; 1 naming the first case that does not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). POSIX tools only.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"

PASSED=0
FAILED=0

# Filler + guarded-site counts must clear MIN_FILES / MIN_SITES / MIN_GUARDS in
# the gate. Read them from the gate itself so a raised floor updates the fixture
# instead of silently turning every case into a floor breach.
GATE="$REPO_ROOT/scripts/check_teardown_guard.sh"
need() { awk -F'=' -v k="$1" '$0 ~ "^"k"=" { print $2+0; exit }' "$GATE"; }
MIN_FILES="$(need MIN_FILES)"
MIN_SITES="$(need MIN_SITES)"
MIN_GUARDS="$(need MIN_GUARDS)"
[ "${MIN_FILES:-0}" -gt 0 ] || { echo "FAIL: could not read MIN_FILES from the gate" >&2; exit 1; }

# Builds a fixture repo in $1. Pass "empty" as $2 to skip all source files.
make_fixture() {
  fx="$1"
  mode="${2:-full}"
  mkdir -p "$fx/scripts/lib" "$fx/crates/trusty-search/src/gen" \
           "$fx/crates/trusty-search/src/service"
  cp "$REPO_ROOT/scripts/check_teardown_guard.sh" "$fx/scripts/"
  cp "$REPO_ROOT/scripts/lib/sloc_awk.sh" "$fx/scripts/lib/"
  cp "$REPO_ROOT/scripts/teardown-guard-methods.tsv" "$fx/scripts/"
  printf '# fixture manifest\n' > "$fx/scripts/teardown-guard-manifest.tsv"

  if [ "$mode" != "empty" ]; then
    # Correctly-guarded sites: enough to clear MIN_SITES and MIN_GUARDS.
    n=$(( MIN_SITES > MIN_GUARDS ? MIN_SITES : MIN_GUARDS ))
    n=$(( n + 2 ))
    i=1
    while [ "$i" -le "$n" ]; do
      cat > "$fx/crates/trusty-search/src/gen/ok_$i.rs" <<EOF
pub async fn guarded_$i(idx: &Indexer, id: &IndexId) {
    let _teardown_guard = crate::service::reindex::acquire_index_teardown_read(id).await;
    idx.index_file("a.rs", "fn a() {}").await.ok();
}
EOF
      i=$(( i + 1 ))
    done
    # Filler files with no call sites, to clear MIN_FILES.
    i=1
    while [ "$i" -le "$(( MIN_FILES + 2 ))" ]; do
      printf 'pub fn filler_%s() {}\n' "$i" > "$fx/crates/trusty-search/src/gen/filler_$i.rs"
      i=$(( i + 1 ))
    done
  fi

  git -C "$fx" init -q
  git -C "$fx" add -A
}

# run_case <name> <expect_exit 0|1> <expected substring, or "" > <mutation fn>
run_case() {
  name="$1"; want_exit="$2"; want_msg="$3"; mutate="$4"
  fx="$(mktemp -d "${TMPDIR:-/tmp}/tdguard-selftest.XXXXXX")"
  make_fixture "$fx" "$([ "$name" = "empty_scan" ] && echo empty || echo full)"
  "$mutate" "$fx"
  git -C "$fx" add -A
  out="$(bash "$fx/scripts/check_teardown_guard.sh" 2>&1)" && rc=0 || rc=$?
  ok=1
  [ "$rc" -eq "$want_exit" ] || ok=0
  if [ -n "$want_msg" ]; then
    case "$out" in *"$want_msg"*) ;; *) ok=0 ;; esac
  fi
  if [ "$ok" -eq 1 ]; then
    echo "PASS: $name"
    PASSED=$(( PASSED + 1 ))
  else
    echo "FAIL: $name — expected exit $want_exit${want_msg:+ containing '$want_msg'}, got exit $rc:" >&2
    echo "$out" | sed 's/^/      /' >&2
    FAILED=$(( FAILED + 1 ))
  fi
  rm -rf "$fx"
}

no_mutation() { :; }

mutate_unguarded() {
  cat > "$1/crates/trusty-search/src/service/writer.rs" <<'EOF'
pub async fn writes_without_any_guard(idx: &Indexer) {
    idx.commit_parsed_batch(parsed, false).await.ok();
}
EOF
}

mutate_late_guard() {
  # The watch_loop::handle_modified shape: a real guard, taken too late.
  cat > "$1/crates/trusty-search/src/service/writer.rs" <<'EOF'
pub async fn guard_after_the_write(idx: &Indexer, id: &IndexId) {
    idx.remove_chunk("stale:1:2").await.ok();
    let _teardown_guard = crate::service::reindex::acquire_index_teardown_read(id).await;
    idx.index_file("a.rs", "fn a() {}").await.ok();
}
EOF
}

mutate_spawn_escape() {
  # The commands/start/daemon.rs shape: the guard is real, but the write runs in
  # a spawned task that outlives it.
  cat > "$1/crates/trusty-search/src/service/writer.rs" <<'EOF'
pub async fn guard_does_not_reach_the_spawn(idx: Indexer, id: IndexId) {
    let _teardown_guard = crate::service::reindex::acquire_index_teardown_read(&id).await;
    tokio::spawn(async move {
        idx.commit_parsed_batch(parsed, false).await.ok();
    });
}
EOF
}

mutate_deferred_spawn_escape() {
  # The same escape as `spawn_escape`, reached through an idiomatic refactor:
  # name the future first (for tracing, instrumentation, or a conditional
  # spawn), then spawn the binding. The `spawn(` and the write no longer share
  # a line or a brace, so a gate that only opens a scope when `spawn(` and a
  # brace-open coincide attributes the write to the enclosing function's guard —
  # which is dropped the moment that function returns. #3049 round 4 review
  # demonstrated exactly this against the first version of the gate: it reported
  # OK, exit 0.
  cat > "$1/crates/trusty-search/src/service/writer.rs" <<'EOF'
pub async fn guard_does_not_reach_the_deferred_spawn(idx: Indexer, id: IndexId) {
    let _teardown_guard = crate::service::reindex::acquire_index_teardown_read(&id).await;
    let fut = async move { idx.commit_parsed_batch(parsed, false).await };
    tokio::spawn(fut);
}
EOF
}

mutate_guarded_ok() {
  cat > "$1/crates/trusty-search/src/service/writer.rs" <<'EOF'
pub async fn correctly_guarded(idx: &Indexer, id: &IndexId) {
    let _teardown_guard = crate::service::reindex::acquire_index_teardown_read(id).await;
    idx.commit_parsed_batch(parsed, false).await.ok();
}
EOF
}

mutate_stale_row() {
  mutate_guarded_ok "$1"
  printf 'crates/trusty-search/src/service/writer.rs\tcommit_parsed_batch\tcorrectly_guarded\tEXEMPT\tthis site is guarded, so the row is a lie\n' \
    >> "$1/scripts/teardown-guard-manifest.tsv"
}

mutate_bogus_caller() {
  mutate_unguarded "$1"
  printf 'crates/trusty-search/src/service/writer.rs\tcommit_parsed_batch\twrites_without_any_guard\tCALLER:no_such_function\tclaims an ancestor that does not exist\n' \
    >> "$1/scripts/teardown-guard-manifest.tsv"
}

mutate_empty_reason() {
  mutate_unguarded "$1"
  printf 'crates/trusty-search/src/service/writer.rs\tcommit_parsed_batch\twrites_without_any_guard\tEXEMPT\t\n' \
    >> "$1/scripts/teardown-guard-manifest.tsv"
}

run_case empty_scan     1 "SCAN FLOOR"           no_mutation
run_case unguarded_call 1 "UNDECLARED"           mutate_unguarded
run_case late_guard     1 "UNDECLARED"           mutate_late_guard
run_case spawn_escape   1 "UNDECLARED"           mutate_spawn_escape
run_case deferred_spawn_escape 1 "UNDECLARED"    mutate_deferred_spawn_escape
run_case guarded_ok     0 "OK: teardown guard"   mutate_guarded_ok
run_case stale_row      1 "STALE ROW"            mutate_stale_row
run_case bogus_caller   1 "UNVERIFIABLE CALLER"  mutate_bogus_caller
run_case empty_reason   1 "NO REASON"            mutate_empty_reason

echo "teardown-guard selftest: ${PASSED} passed, ${FAILED} failed"
[ "$FAILED" -eq 0 ] || exit 1
