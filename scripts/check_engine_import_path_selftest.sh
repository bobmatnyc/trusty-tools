#!/usr/bin/env bash
#
# check_engine_import_path_selftest.sh — mutation self-test for
# scripts/check_engine_import_path.sh (issue #9270).
#
# Why: a gate whose matcher silently stopped matching reports OK exactly like a
#   clean tree (#4618, #5440). Its ability to fail is tested here before CI
#   trusts its pass.
# What: one throwaway git repo per case, with the gate copied into its
#   `scripts/` so the gate's repo root resolves inside the fixture. Each case
#   asserts the exit status and, where it matters, the message. Source-scan
#   cases pin the defect (a `use` line, an inline type path, an attribute), the
#   accepted forms (comment lines in Rust and TOML, a `changelog.d/` fragment,
#   the two owning crates, a path outside `crates/`) and the fail-closed scan
#   floor. `--dep-graph` cases drive the gate over captured `cargo tree` text:
#   a clean pair, the engine leaking into the default graph, the feature graph
#   missing it, and an empty tree.
#
# Usage: bash scripts/check_engine_import_path_selftest.sh
# Exit:  0 when every case behaves; 1 naming each case that does not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$SCRIPT_DIR/check_engine_import_path.sh"
PASSED=0
FAILED=0
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

# new_fixture <name>: a git repo holding the gate and one Rust crate.
new_fixture() {
  local dir="$TMP_ROOT/$1"
  mkdir -p "$dir/scripts" "$dir/crates/consumer/src"
  cp "$GATE" "$dir/scripts/check_engine_import_path.sh"
  printf 'fn main() {}\n' > "$dir/crates/consumer/src/main.rs"
  git -C "$dir" init -q
  printf '%s\n' "$dir"
}

# check <name> <want-exit> <needle-or-empty> <output> <status>
check() {
  local name="$1" want="$2" needle="$3" out="$4" status="$5"
  if [ "$status" -ne "$want" ]; then
    echo "FAIL $name: exit $status, want $want"
    printf '%s\n' "$out" | sed 's/^/    /'
    FAILED=$((FAILED + 1))
    return
  fi
  if [ -n "$needle" ] && ! printf '%s' "$out" | grep -qF -- "$needle"; then
    echo "FAIL $name: output lacks '$needle'"
    printf '%s\n' "$out" | sed 's/^/    /'
    FAILED=$((FAILED + 1))
    return
  fi
  echo "ok   $name (exit $status)"
  PASSED=$((PASSED + 1))
}

# run_scan <name> <want-exit> <needle> <dir> [min-files]: the source scan.
run_scan() {
  local name="$1" want="$2" needle="$3" dir="$4" min="${5:-1}" out status
  git -C "$dir" add -A
  set +e
  out="$(ENGINE_PATH_MIN_FILES="$min" bash "$dir/scripts/check_engine_import_path.sh" 2>&1)"
  status=$?
  set -e
  check "$name" "$want" "$needle" "$out" "$status"
}

# run_graph <name> <want-exit> <needle> <default-tree> <feature-tree>
run_graph() {
  local name="$1" want="$2" needle="$3" dir out status
  dir="$(new_fixture "$name")"
  printf '%s' "$4" > "$dir/default.txt"
  printf '%s' "$5" > "$dir/features.txt"
  set +e
  out="$(ENGINE_TREE_DEFAULT="$dir/default.txt" ENGINE_TREE_FEATURES="$dir/features.txt" \
    bash "$dir/scripts/check_engine_import_path.sh" --dep-graph 2>&1)"
  status=$?
  set -e
  check "$name" "$want" "$needle" "$out" "$status"
}

# ── Source scan: defects ──────────────────────────────────────────────────────
d="$(new_fixture use_line)"
printf 'use trusty_common::memory_core::store::OpenIntent;\n' > "$d/crates/consumer/src/a.rs"
run_scan use_line 1 'crates/consumer/src/a.rs:1:' "$d"

d="$(new_fixture inline_path)"
printf 'struct S {\n    id: trusty_common::memory_core::palace::PalaceId,\n}\n' \
  > "$d/crates/consumer/src/a.rs"
run_scan inline_path 1 'crates/consumer/src/a.rs:2:' "$d"

d="$(new_fixture attribute)"
printf '#[doc(alias = "trusty_common::memory_core")]\nfn f() {}\n' > "$d/crates/consumer/src/a.rs"
run_scan attribute 1 'crates/consumer/src/a.rs:1:' "$d"

d="$(new_fixture code_beside_comment)"
printf '// trusty_common::memory_core in a comment\nuse trusty_common::memory_core::registry;\n' \
  > "$d/crates/consumer/src/a.rs"
run_scan code_beside_comment 1 'crates/consumer/src/a.rs:2:' "$d"

# ── Source scan: accepted forms ───────────────────────────────────────────────
d="$(new_fixture comment_lines)"
cat > "$d/crates/consumer/src/a.rs" <<'RS'
//! Mirrors `trusty_common::memory_core::timeouts`.
/// See `trusty_common::memory_core::registry`.
// trusty_common::memory_core is the old path.
/* trusty_common::memory_core */
/*
 * trusty_common::memory_core inside a block comment
 */
fn f() {}
RS
printf '[package]\nname = "consumer"\n# trusty_common::memory_core is reached via trusty-memory\n' \
  > "$d/crates/consumer/Cargo.toml"
run_scan comment_lines 0 'OK:' "$d"

d="$(new_fixture changelog_fragment)"
mkdir -p "$d/crates/consumer/changelog.d"
printf 'Changed\n\n- moved off `trusty_common::memory_core`\n' \
  > "$d/crates/consumer/changelog.d/9270-engine-import-path.md"
run_scan changelog_fragment 0 'OK:' "$d"

d="$(new_fixture owning_crates)"
mkdir -p "$d/crates/trusty-common/src" "$d/crates/trusty-memory/src"
printf 'pub use crate::memory_core;\nuse trusty_common::memory_core::store;\n' \
  > "$d/crates/trusty-common/src/lib.rs"
printf 'pub use trusty_common::memory_core as engine;\n' > "$d/crates/trusty-memory/src/lib.rs"
run_scan owning_crates 0 'OK:' "$d"

d="$(new_fixture outside_crates)"
mkdir -p "$d/docs"
printf 'The engine is `trusty_common::memory_core`.\n' > "$d/docs/adr.md"
run_scan outside_crates 0 'OK:' "$d"

# ── Source scan: fail closed ──────────────────────────────────────────────────
d="$(new_fixture empty_scan)"
run_scan empty_scan 1 'SCAN FLOOR' "$d" 200

d="$(new_fixture unknown_arg)"
set +e
out="$(bash "$d/scripts/check_engine_import_path.sh" --bogus 2>&1)"
status=$?
set -e
check unknown_arg 2 'unknown argument' "$out" "$status"

# ── --dep-graph ───────────────────────────────────────────────────────────────
CLEAN_DEFAULT='trusty-mpm v1.9.0 (/x/crates/trusty-mpm)
trusty-common v0.63.0 (/x/crates/trusty-common)
tokio v1.48.0
'
FULL_FEATURES='trusty-mpm v1.9.0 (/x/crates/trusty-mpm)
trusty-common v0.63.0 (/x/crates/trusty-common)
hnsw_rs v0.3.4
ort v2.0.0-rc.12
trusty-memory v0.35.1 (/x/crates/trusty-memory)
'

run_graph graph_clean 0 'OK: default trusty-mpm graph' "$CLEAN_DEFAULT" "$FULL_FEATURES"
run_graph graph_default_hnsw 1 "pulls in 'hnsw_rs'" \
  "${CLEAN_DEFAULT}hnsw_rs v0.3.4
" "$FULL_FEATURES"
run_graph graph_default_trusty_memory 1 "pulls in 'trusty-memory'" \
  "${CLEAN_DEFAULT}trusty-memory v0.35.1 (/x/crates/trusty-memory)
" "$FULL_FEATURES"
run_graph graph_default_ort 1 "pulls in 'ort'" \
  "${CLEAN_DEFAULT}ort v2.0.0-rc.12
" "$FULL_FEATURES"
run_graph graph_features_lack_engine 1 "does not pull in 'trusty-memory'" \
  "$CLEAN_DEFAULT" "$CLEAN_DEFAULT"
run_graph graph_empty_tree 1 'SCAN FLOOR' '' "$FULL_FEATURES"

echo "check_engine_import_path_selftest: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
