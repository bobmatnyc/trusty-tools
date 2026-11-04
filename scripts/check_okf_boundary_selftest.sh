#!/usr/bin/env bash
#
# check_okf_boundary_selftest.sh — mutation self-test for
#   scripts/check_okf_boundary.sh (issue #10746).
#
# Why: a gate whose matcher silently stopped matching reports OK exactly like a
#   clean crate (#4618, #5440). Its ability to fail is tested before CI trusts
#   its pass.
# What: one throwaway git repo per case, holding a fixture crate and synthetic
#   `cargo tree` output (OKF_TREE_ALL / OKF_TREE_CORE); the build step is
#   skipped. Each case asserts the exit status and, for a failure, the named
#   check. Cases cover every check failing once, the accepted forms (a
#   feature-only network crate, an `okg_` identifier), and the fail-closed
#   paths (no source, a tree that does not start at trusty-okf, a perl that
#   errors).
#
# Usage: bash scripts/check_okf_boundary_selftest.sh
# Exit:  0 when every case behaves; 1 naming each case that does not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$SCRIPT_DIR/check_okf_boundary.sh"
PASSED=0
FAILED=0
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

TREE_HEAD='trusty-okf v0.1.0 (/x/crates/trusty-okf)'

# new_fixture <name>: a git repo holding a clean crate and clean trees.
new_fixture() {
  local dir="$TMP_ROOT/$1"
  mkdir -p "$dir/crates/trusty-okf/src"
  printf '[package]\nname = "trusty-okf"\nversion = "0.1.0"\n' > "$dir/crates/trusty-okf/Cargo.toml"
  printf '//! Bundle root only.\npub fn open(root: &std::path::Path) -> bool { root.exists() }\n' \
    > "$dir/crates/trusty-okf/src/lib.rs"
  printf '%s\nserde v1.0.0\n' "$TREE_HEAD" > "$dir/all.txt"
  printf '%s\n' "$TREE_HEAD" > "$dir/core.txt"
  printf '%s\ntrusty-agents v0.39.4 (/x/crates/trusty-agents)\n' "$TREE_HEAD" > "$dir/inv.txt"
  git -C "$dir" init -q
  printf '%s\n' "$dir"
}

# run_case <name> <expected-exit> <expected-message-or-empty> <dir>
# CASE_PATH, when set, replaces PATH for the gate (stub-tool cases).
run_case() {
  local name="$1" want="$2" needle="$3" dir="$4" out status
  set +e
  out="$(cd "$dir" && PATH="${CASE_PATH:-$PATH}" OKF_TREE_ALL=all.txt OKF_TREE_CORE=core.txt \
    OKF_TREE_INV=inv.txt OKF_SKIP_BUILD=1 bash "$GATE" 2>&1)"
  status=$?
  set -e
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
  echo "ok   $name"
  PASSED=$((PASSED + 1))
}

# add_src <dir> <line>: append a line of Rust to the fixture crate.
add_src() { printf '%s\n' "$2" >> "$1/crates/trusty-okf/src/lib.rs"; }

# 1. Clean crate passes. A network crate behind a feature (all-features tree
#    only) and an `okg_` identifier are both allowed.
d="$(new_fixture clean)"
printf 'tokio v1.0.0\n' >> "$d/all.txt"
add_src "$d" 'pub fn okg_register_source() {}'
run_case "clean crate passes" 0 "check_okf_boundary: OK" "$d"

# 2. B1: an agent crate in the graph (criterion 11).
d="$(new_fixture agent-dep)"
printf 'trusty-agents v0.9.0 (/x/crates/trusty-agents)\n' >> "$d/all.txt"
run_case "agent dependency fails" 1 "FAIL c11_no_agent_or_connector_deps" "$d"

# 3. B1: an MCP crate in the graph fails c11 and c14.
d="$(new_fixture mcp-dep)"
printf 'trusty-mcp v0.2.0 (/x/crates/trusty-mcp)\n' >> "$d/all.txt"
run_case "MCP framework dependency fails c11" 1 "banned dependency: trusty-mcp" "$d"
run_case "MCP framework dependency fails c14" 1 "MCP framework crate in the dependency graph" "$d"

# 4-7. B3-B5: src literals (criterion 12).
d="$(new_fixture env-var)"
add_src "$d" 'pub fn h() -> Option<String> { std::env::var("HOME").ok() }'
run_case "env::var in src fails" 1 "FAIL c12_no_agent_paths_or_index_names" "$d"
d="$(new_fixture okg-path)"
add_src "$d" 'pub fn p(r: &std::path::Path) -> std::path::PathBuf { r.join("okg") }'
run_case "okg path literal fails" 1 'lib.rs:3: pub fn p' "$d"
d="$(new_fixture index-name)"
add_src "$d" 'pub const ID: &str = "assistant-okg-main";'
run_case "assistant-okg- literal fails" 1 "FAIL c12_no_agent_paths_or_index_names" "$d"
d="$(new_fixture index-create)"
add_src "$d" 'pub const M: &str = "search.index.create";'
run_case "index create method fails" 1 "FAIL c12_no_agent_paths_or_index_names" "$d"

# #10746 H2: every route to std::env::var / var_os fails c12 (B3).
d="$(new_fixture env-brace-import)"
add_src "$d" 'use std::env::{var};'
add_src "$d" 'pub fn h() -> Option<String> { var("HOME").ok() }'
run_case "env brace import fails" 1 "FAIL c12_no_agent_paths_or_index_names" "$d"
d="$(new_fixture env-brace-list)"
add_src "$d" 'use std::env::{args, var};'
add_src "$d" 'pub fn h() -> Option<String> { var("HOME").ok() }'
run_case "env brace list import fails" 1 "FAIL c12_no_agent_paths_or_index_names" "$d"
d="$(new_fixture env-alias)"
add_src "$d" 'use std::{env as e};'
add_src "$d" 'pub fn h() -> Option<std::ffi::OsString> { e::var_os("HOME") }'
run_case "env module alias fails" 1 "FAIL c12_no_agent_paths_or_index_names" "$d"
d="$(new_fixture env-multiline)"
printf 'use std::{\n    env::{\n        var_os as v,\n    },\n};\npub fn h() -> bool { v("HOME").is_some() }\n' \
  >> "$d/crates/trusty-okf/src/lib.rs"
run_case "env multi-line group import fails" 1 "lib.rs:4:" "$d"

# #10746 H3: a string literal naming trusty-agents anywhere fails c12 (B4).
d="$(new_fixture agents-mid-literal)"
add_src "$d" 'pub fn p(r: &std::path::Path) -> std::path::PathBuf { r.join("x/trusty-agents/y") }'
run_case "trusty-agents mid-literal fails" 1 "FAIL c12_no_agent_paths_or_index_names" "$d"
d="$(new_fixture agents-config-literal)"
add_src "$d" 'pub const P: &str = ".config/trusty-agents";'
run_case "trusty-agents config literal fails" 1 "FAIL c12_no_agent_paths_or_index_names" "$d"

# 8. B6: a connector Locator variant (criterion 13).
d="$(new_fixture gmail)"
add_src "$d" 'pub enum Locator { DocStore, Gmail }'
run_case "Gmail variant fails" 1 "FAIL c13_no_connector_locator_variants" "$d"

# 9. B6: a credential crate.
d="$(new_fixture keyring)"
printf 'keyring v3.0.0\n' >> "$d/all.txt"
run_case "keychain dependency fails" 1 "FAIL b6_no_credential_deps" "$d"

# 10-12. B7: MCP surface (criterion 14).
d="$(new_fixture bin-target)"
printf '[[bin]]\nname = "trusty-okf"\n' >> "$d/crates/trusty-okf/Cargo.toml"
run_case "bin target fails" 1 "declares a [[bin]] target" "$d"
# #10746 S8: src/main.rs is the CLI; it may read its arguments and nothing
# else of the environment, and it may not serve stdio.
d="$(new_fixture main-rs)"
printf 'fn main() {\n    let a: Vec<_> = std::env::args_os().collect();\n    drop(a);\n}\n' \
  > "$d/crates/trusty-okf/src/main.rs"
run_case "CLI main.rs reading its arguments passes" 0 "check_okf_boundary: OK" "$d"
d="$(new_fixture main-rs-env-var)"
printf 'fn main() {\n    let _ = std::env::args_os();\n    let _ = std::env::var("HOME");\n}\n' \
  > "$d/crates/trusty-okf/src/main.rs"
run_case "env::var in main.rs fails" 1 "main.rs:3:" "$d"
d="$(new_fixture args-in-lib)"
add_src "$d" 'pub fn a() -> usize { std::env::args_os().count() }'
run_case "args_os outside main.rs fails" 1 "FAIL c12_no_agent_paths_or_index_names" "$d"
d="$(new_fixture main-rs-stdio)"
printf 'fn main() {\n    let _ = std::env::args_os();\n    let _ = "serve";\n}\n' \
  > "$d/crates/trusty-okf/src/main.rs"
run_case "main.rs with a serve verb fails" 1 "FAIL c14_no_mcp_surface" "$d"
d="$(new_fixture bin-dir)"
mkdir -p "$d/crates/trusty-okf/src/bin"
printf 'fn main() {}\n' > "$d/crates/trusty-okf/src/bin/x.rs"
run_case "src/bin/ fails" 1 "src/bin/ exists" "$d"
d="$(new_fixture tool-descriptor)"
add_src "$d" 'pub const T: &str = "tools/list";'
run_case "MCP tool descriptor fails" 1 "FAIL c14_no_mcp_surface" "$d"

# 13. B8: an async runtime in the core graph (criterion 15).
d="$(new_fixture core-tokio)"
printf 'tokio v1.0.0\n' >> "$d/core.txt"
run_case "async runtime in core fails" 1 "FAIL c15_core_network_free" "$d"

# 14. B9: an extraction backend under any feature.
d="$(new_fixture ort)"
printf 'ort v2.0.0\n' >> "$d/all.txt"
run_case "extraction backend fails" 1 "FAIL b9_no_extraction_backend" "$d"

# #10746 S11: B2 inverse half, trusty-agents must depend on trusty-okf.
d="$(new_fixture no-agents-dependent)"
printf '%s\n' "$TREE_HEAD" > "$d/inv.txt"
run_case "no trusty-agents dependent fails" 1 "FAIL b2_trusty_agents_depends_on_okf" "$d"
d="$(new_fixture agents-common-dependent)"
printf '%s\ntrusty-agents-common v0.1.0 (/x/crates/trusty-agents-common)\n' "$TREE_HEAD" > "$d/inv.txt"
run_case "a near-name dependent is not trusty-agents" 1 "FAIL b2_trusty_agents_depends_on_okf" "$d"

# 15-16. Fail closed.
d="$(new_fixture no-src)"
rm "$d/crates/trusty-okf/src/lib.rs"
run_case "no source fails" 1 "nothing to check is a failure" "$d"
d="$(new_fixture bad-tree)"
printf 'serde v1.0.0\n' > "$d/all.txt"
run_case "tree not rooted at trusty-okf fails" 1 "does not start at trusty-okf" "$d"


# #10746 H1: a source scan whose perl errors fails closed, both on a non-zero
# exit and on stderr output with exit 0.
stub="$TMP_ROOT/stub-perl-exit"
mkdir -p "$stub"
printf '#!/bin/sh\necho "stub perl: boom" >&2\nexit 2\n' > "$stub/perl"
chmod +x "$stub/perl"
d="$(new_fixture perl-exit)"
CASE_PATH="$stub:$PATH" run_case "perl exiting non-zero fails" 1 "source scan failed" "$d"
stub="$TMP_ROOT/stub-perl-stderr"
mkdir -p "$stub"
printf '#!/bin/sh\necho "Can'"'"'t open lib.rs" >&2\nexit 0\n' > "$stub/perl"
chmod +x "$stub/perl"
d="$(new_fixture perl-stderr)"
CASE_PATH="$stub:$PATH" run_case "perl writing stderr fails" 1 "source scan failed" "$d"

echo "check_okf_boundary_selftest: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
