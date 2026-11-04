#!/usr/bin/env bash
#
# check_okf_boundary.sh — trusty-okf boundary rules B1-B10 (SPEC-OKF-01
#   §2.7.1) and acceptance criteria 11-15 (§6.2). Issue #10746, slice S1.
#
# Why: the boundary is what lets trusty-okf replace trusty-kb without pulling
#   agent glue, credentials, an MCP server or a network stack into the core.
#   A breach of any rule fails review, so a machine check runs it on every PR
#   rather than leaving it to a reviewer's grep.
#
# What: one named check per criterion or rule. Each prints `ok   <name>` or
#   `FAIL <name>` with the offending crate or file:line.
#     c11_no_agent_or_connector_deps   B1  dependency graph (all features,
#                                          all targets, normal+dev+build)
#                                          names no agent, MCP or connector crate
#     c12_no_agent_paths_or_index_names B3-B5  src/ has no env::var, agent-home
#                                          path, `okg` path word, index name
#     c13_no_connector_locator_variants B6  src/ names no Gmail/Drive variant
#     b6_no_credential_deps            B6  graph has no OAuth/token/keychain crate
#     c14_no_mcp_surface               B7  no bin target, main.rs, src/bin/, MCP
#                                          tool descriptor or stdio server
#     c15_core_network_free            B8  --no-default-features graph has no
#                                          async runtime or socket client, and
#                                          the core builds that way
#     b9_no_extraction_backend         B9  graph has no xberg, ort, candle-core
#   Not checkable at S1, printed as `note:` lines: B2's inverse half
#   (`cargo tree -i trusty-okf` lists trusty-agents; S11), B10 (no spawn code
#   exists until S9/S10), and criterion 13's registry.toml load (S4).
#
#   The src/ greps read raw text, comments included, matching the spec's
#   "a grep of crates/trusty-okf/src". Fails closed: a missing src/, a src/
#   with no .rs file, or a dependency tree that does not start at trusty-okf
#   is a failure.
#
# Usage: bash scripts/check_okf_boundary.sh
# Env (selftest fixtures):
#   OKF_CRATE_DIR   crate directory (default crates/trusty-okf)
#   OKF_TREE_ALL    read this file as the all-features tree instead of cargo
#   OKF_TREE_CORE   read this file as the --no-default-features tree
#   OKF_SKIP_BUILD  1 skips the `cargo check --no-default-features` build
# Exit: 0 clean; 1 on any finding.
# Test: scripts/check_okf_boundary_selftest.sh.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); git, perl, cargo.

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

CRATE_DIR="${OKF_CRATE_DIR:-crates/trusty-okf}"
SRC="$CRATE_DIR/src"
STATUS=0
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# fail <check-name> <newline-separated findings>
fail() { echo "FAIL $1"; printf '%s\n' "$2" | sed 's/^/     /'; STATUS=1; }

if [ ! -d "$SRC" ] || [ -z "$(find "$SRC" -name '*.rs' -type f | head -n 1)" ]; then
  echo "check_okf_boundary: no .rs file under $SRC; nothing to check is a failure" >&2
  exit 1
fi

# tree_of <fixture-var-value> <out> <cargo feature flag>: the crate names in
# trusty-okf's normal+dev+build graph across every target platform.
tree_of() {
  local fixture="$1" out="$2" flag="$3"
  if [ -n "$fixture" ]; then
    cp "$fixture" "$out.raw"
  else
    cargo tree -q --locked -p trusty-okf -e normal,dev,build --target all \
      "$flag" --prefix none --format '{p}' > "$out.raw"
  fi
  awk '{print $1}' "$out.raw" | sort -u > "$out"
  if [ "$(head -n 1 "$out.raw" | awk '{print $1}')" != "trusty-okf" ]; then
    echo "check_okf_boundary: dependency tree ($flag) does not start at trusty-okf" >&2
    exit 1
  fi
}
tree_of "${OKF_TREE_ALL:-}" "$TMP/all" --all-features
tree_of "${OKF_TREE_CORE:-}" "$TMP/core" --no-default-features

# banned_deps <check-name> <tree-file> <regex>: every crate name in the tree
# that matches the anchored extended regex is a finding.
banned_deps() {
  local name="$1" tree="$2" re="$3" hits
  hits="$(grep -E "^($re)\$" "$tree" || true)"
  if [ -n "$hits" ]; then
    fail "$name" "$(printf '%s\n' "$hits" | sed 's/^/banned dependency: /')"
  else
    echo "ok   $name"
  fi
}

# banned_src <check-name> <perl-regex>: every src/ line matching is a finding.
banned_src() {
  local name="$1" re="$2" hits
  hits="$(find "$SRC" -name '*.rs' -type f | sort |
    OKF_RE="$re" xargs perl -ne 'print "$ARGV:$.: $_" if /$ENV{OKF_RE}/; close ARGV if eof' || true)"
  if [ -n "$hits" ]; then
    fail "$name" "$hits"
  else
    echo "ok   $name"
  fi
}

# B1: agent, MCP and connector crates. trusty-kb is the crate being replaced;
# a dependency on it would carry its MCP server and agent coupling along.
banned_deps c11_no_agent_or_connector_deps "$TMP/all" \
  'trusty-agents|trusty-agents-common|trusty-agents-ui|trusty-mcp|trusty-mcp-services|trusty-gworkspace|trusty-channels|trusty-kb|tc-services'

# B3 (env/home), B4 (agent home layout), B5 (index lifecycle and names).
banned_src c12_no_agent_paths_or_index_names \
  'env::var|knowledge_dir|TRUSTY_AGENTS_HOME|KB_KNOWLEDGE_DIR|home_dir|\.trusty-agents|"trusty-agents|\bokg\b|stores/|assistant-okg-|search\.index\.create|search\.index\.delete'

# B6: connector-specific Locator variants stay in trusty-agents.
banned_src c13_no_connector_locator_variants '\bGmail\b|\bDrive\b'

# B6: no credential handling.
banned_deps b6_no_credential_deps "$TMP/all" \
  'oauth2|yup-oauth2|gcp_auth|keyring|security-framework|secret-service|trusty-secrets|google-[a-z0-9_-]+'

# B7: no MCP surface and no stdio server binary.
mcp_hits=""
nl='
'
if grep -qE '^\[\[bin\]\]' "$CRATE_DIR/Cargo.toml"; then
  mcp_hits="$CRATE_DIR/Cargo.toml declares a [[bin]] target$nl"
fi
[ -e "$SRC/main.rs" ] && mcp_hits="$mcp_hits$SRC/main.rs exists$nl"
[ -e "$SRC/bin" ] && mcp_hits="$mcp_hits$SRC/bin/ exists$nl"
if grep -qE '^(rmcp|trusty-mcp)$' "$TMP/all"; then
  mcp_hits="${mcp_hits}MCP framework crate in the dependency graph$nl"
fi
if [ -n "$mcp_hits" ]; then
  fail c14_no_mcp_surface "${mcp_hits%$nl}"
else
  banned_src c14_no_mcp_surface '"tools/list"|"tools/call"|inputSchema|--stdio|run_stdio_loop'
fi

# B8: the core (no features) has no async runtime and no socket client.
banned_deps c15_core_network_free "$TMP/core" \
  'tokio|async-std|smol|async-io|async-net|mio|reqwest|hyper|hyper-util|ureq|isahc|surf|curl|socket2|h2|tonic|tungstenite|tokio-tungstenite'
if [ "${OKF_SKIP_BUILD:-0}" != 1 ]; then
  if cargo check -q --locked -p trusty-okf --no-default-features > "$TMP/build.log" 2>&1; then
    echo "ok   c15_core_builds_without_default_features"
  else
    echo "FAIL c15_core_builds_without_default_features"
    sed 's/^/     /' "$TMP/build.log"
    STATUS=1
  fi
fi

# B9: no extraction backend is ever linked, under any feature.
banned_deps b9_no_extraction_backend "$TMP/all" 'xberg|ort|ort-sys|candle-core'

echo "note: B2 inverse half (cargo tree -i trusty-okf lists trusty-agents) is checked from S11"
echo "note: B10 (child-process environment) has no spawn code to check until S9/S10"
echo "note: criterion 13 registry.toml load (gmail/drive rows survive) is checked from S4"

if [ "$STATUS" -eq 0 ]; then
  echo "check_okf_boundary: OK"
else
  echo "check_okf_boundary: boundary breach (SPEC-OKF-01 §2.7.1)" >&2
fi
exit "$STATUS"
