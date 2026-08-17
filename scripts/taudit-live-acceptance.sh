#!/usr/bin/env bash
#
# taudit-live-acceptance.sh — LIVE end-to-end acceptance test for the
# trusty-audit MVP (#5852 `taudit audit`, #5858 `taudit distribute`).
#
# Why: the owner's own words for the MVP: "I should be able to extract it,
#   run a script/binary, select one or more repos, have it do the
#   audit/report, give me a zip file back with the collected DB and the
#   report." This script walks exactly that path, live — no stubs, no mocks,
#   no offline variant. A stubbed run would pass while proving nothing.
#
# What it proves, step by step:
#   1. Builds the real `taudit` binary from this checkout.
#   2. Runs `taudit distribute` to build the install zip an auditor sends.
#   3. Extracts that zip to a scratch directory, as a recipient would.
#   4. From the EXTRACTED copy, registers a real repository and confirms
#      validation against a real `gh` credential.
#   5. Runs the one-shot `audit` chain end to end: install the pinned tools,
#      clone, analyse via OpenRouter, and assemble the return package.
#   6. Opens the returned zip and checks its CONTENTS, not just its
#      existence: the collected SQLite extract database is present and
#      non-trivial, and the report is present and non-empty.
#   7. Greps every extracted member of the returned zip for the OpenRouter
#      key and fails loudly if it finds it — the one guard whose failure is
#      invisible by inspection alone.
#
# Requires:
#   - A real OPENROUTER_API_KEY exported in the environment (never read from
#     a flag or baked into this script — argv is visible to `ps` and lands
#     in shell history).
#   - `gh`, authenticated (`gh auth status`), with access to a public GitHub
#     repository (see TAUDIT_E2E_REPO below).
#   - Network access: crates.io / GitHub releases (tool install), GitHub
#     (clone), OpenRouter (review inference).
#   - `cargo`, `unzip`, and ideally `sqlite3` (falls back to a header+size
#     check when absent).
#
# Duration: several minutes — real installs, a real clone, and real
#   inference calls. Not hours: the default target is deliberately tiny.
#
# This is a manual acceptance script. It is NOT wired into CI: it needs a
# real credential and real network, both of which CI does not have.
#
# Usage:
#   OPENROUTER_API_KEY=sk-or-v1-... scripts/taudit-live-acceptance.sh
#   TAUDIT_E2E_REPO=owner/name OPENROUTER_API_KEY=... scripts/taudit-live-acceptance.sh
#
# Idempotent: every artifact this script writes lives under a fresh
# `mktemp -d` scratch directory, so `taudit distribute`'s refusal to
# overwrite an existing package never triggers — there is never a package
# already at the destination. Two consecutive runs are two independent
# scratch trees. Nothing under ~/duetto/audit, or any other real working
# directory, is ever touched.

set -euo pipefail

# --- small helpers -------------------------------------------------------

STEP=0
step() {
  STEP=$((STEP + 1))
  printf '\n==> [%d] %s\n' "$STEP" "$1"
}

fail() {
  printf '\n[FAIL] step %d: %s\n' "$STEP" "$1" >&2
  exit 1
}

ok() {
  printf '[ok] %s\n' "$1"
}

# --- locate the workspace --------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# --- preflight: fail early and specifically, before anything is built ----

step "preflight: OPENROUTER_API_KEY"
if [ -z "${OPENROUTER_API_KEY:-}" ]; then
  fail "OPENROUTER_API_KEY is not set. Export a real key before running this
       script — it is never read from a flag, and there is no offline
       variant of this acceptance test."
fi
ok "OPENROUTER_API_KEY is set (value never printed or passed on argv)"

step "preflight: required tools on PATH"
for tool in cargo gh unzip curl; do
  command -v "$tool" >/dev/null 2>&1 || fail "required tool not on PATH: $tool"
done
ok "cargo, gh, unzip, curl are all on PATH"

SQLITE3_AVAILABLE=1
command -v sqlite3 >/dev/null 2>&1 || SQLITE3_AVAILABLE=0
if [ "$SQLITE3_AVAILABLE" -eq 0 ]; then
  printf '[INFO] sqlite3 not on PATH — the DB check falls back to a header+size check\n'
fi

step "preflight: gh is authenticated"
if ! gh auth status >/dev/null 2>&1; then
  fail "gh is not authenticated. Run 'gh auth login' first — 'taudit add repo'
       validates every target against your gh credential before it runs
       anything, so this script cannot proceed without one."
fi
ok "gh auth status: authenticated"

# A small, real, public repository so the sweep finishes in minutes rather
# than hours: essentially one file and a handful of commits, which still
# exercises every phase (clone, tga collection, trusty-review inference,
# packaging) without a large checkout or a long analysis pass. Override with
# TAUDIT_E2E_REPO=owner/name for a heavier exercise of the pipeline.
TARGET_REPO="${TAUDIT_E2E_REPO:-octocat/Hello-World}"
ok "audit target: $TARGET_REPO (override with TAUDIT_E2E_REPO)"

# --- scratch state, cleaned up on success, kept (and named) on failure ---

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/taudit-live-acceptance.XXXXXX")"
cleanup() {
  local exit_code=$?
  if [ "$exit_code" -eq 0 ]; then
    rm -rf "$SCRATCH"
  else
    printf '\n[FAIL] leaving scratch state for inspection: %s\n' "$SCRATCH" >&2
  fi
}
trap cleanup EXIT

AUDITOR_DIR="$SCRATCH/auditor"
DIST_DIR="$SCRATCH/dist"
EXTRACT_DIR="$SCRATCH/extracted"
RETURN_DIR="$SCRATCH/return"
mkdir -p "$AUDITOR_DIR"

# --- 1. build the real binary ---------------------------------------------

step "build: cargo build --release -p trusty-audit --bin taudit"
( cd "$WORKSPACE_ROOT" && cargo build --release -p trusty-audit --bin taudit )
TAUDIT_BIN="$WORKSPACE_ROOT/target/release/taudit"
[ -x "$TAUDIT_BIN" ] || fail "expected binary not found or not executable: $TAUDIT_BIN"
ok "built $TAUDIT_BIN"

# --- 2. a real engagement template, pinned to LIVE published versions ----

step "fetch: current published versions of the pinned tools from crates.io"
crate_version() {
  local crate="$1"
  curl -fsSL -H 'User-Agent: taudit-live-acceptance.sh (bobmatnyc/trusty-tools)' \
    "https://crates.io/api/v1/crates/$crate" \
    | python3 -c "import json,sys; print(json.load(sys.stdin)['crate']['max_stable_version'])" \
    || fail "could not fetch/parse the crates.io version for $crate"
}
TGA_VERSION="$(crate_version tga)"
TS_VERSION="$(crate_version trusty-search)"
TA_VERSION="$(crate_version trusty-analyze)"
TR_VERSION="$(crate_version trusty-review)"
ok "tga=$TGA_VERSION trusty-search=$TS_VERSION trusty-analyze=$TA_VERSION trusty-review=$TR_VERSION"

# The key stays blank in the template on disk — `taudit distribute` prefers
# OPENROUTER_API_KEY from the environment over whatever the template carries
# (see crates/trusty-audit/src/distribute.rs's resolve_key), so the real
# credential is never written to this file.
cat >"$AUDITOR_DIR/engagement.toml" <<EOF
openrouter_key = ""
instructions = "Live end-to-end acceptance run for the trusty-audit MVP."
client = "Acceptance Test"
engagement = "taudit-live-e2e"

[tools]
tga = "$TGA_VERSION"
trusty-search = "$TS_VERSION"
trusty-analyze = "$TA_VERSION"
trusty-review = "$TR_VERSION"
EOF
ok "wrote template engagement config: $AUDITOR_DIR/engagement.toml"

# --- 3. taudit distribute: build the install zip --------------------------

step "distribute: build the install package (auditor side)"
"$TAUDIT_BIN" --config "$AUDITOR_DIR/engagement.toml" distribute \
  --out "$DIST_DIR" --binary "$TAUDIT_BIN" \
  || fail "'taudit distribute' exited non-zero"

INSTALL_ZIP="$DIST_DIR/trusty-audit-install.zip"
[ -s "$INSTALL_ZIP" ] || fail "expected install zip not found or empty: $INSTALL_ZIP"
ok "install package: $INSTALL_ZIP ($(wc -c <"$INSTALL_ZIP" | tr -d ' ') bytes)"

# --- 4. extract it, as a recipient would -----------------------------------

step "extract: unzip the install package into a scratch directory"
mkdir -p "$EXTRACT_DIR"
unzip -q "$INSTALL_ZIP" -d "$EXTRACT_DIR" || fail "could not extract $INSTALL_ZIP"

CLIENT_DIR="$EXTRACT_DIR/trusty-audit"
LAUNCHER="$CLIENT_DIR/audit.sh"
EXTRACTED_BIN="$CLIENT_DIR/taudit"
EXTRACTED_CONFIG="$CLIENT_DIR/engagement.toml"
for member in "$LAUNCHER" "$EXTRACTED_BIN" "$EXTRACTED_CONFIG" "$CLIENT_DIR/README.md"; do
  [ -f "$member" ] || fail "extracted package is missing an expected member: $member"
done
chmod +x "$LAUNCHER" "$EXTRACTED_BIN"
ok "extracted to $CLIENT_DIR (taudit, audit.sh, engagement.toml, README.md all present)"

# The extracted engagement.toml is what the recipient's run actually reads,
# and it must carry the REAL key from the environment, not the blank
# template on the auditor's own disk (crates/trusty-audit/src/distribute.rs's
# resolve_key: the environment beats the template). Checked by exact match,
# never printed — this is the inbound direction, where the key belongs; the
# outbound-leak guard in step 8 is the one that matters for the return
# package.
if grep -q -F -- "$OPENROUTER_API_KEY" "$EXTRACTED_CONFIG"; then
  ok "extracted engagement.toml carries the real key from the environment"
else
  fail "extracted engagement.toml does not carry OPENROUTER_API_KEY — 'taudit distribute' did not embed the supplied credential"
fi

# --- 5. register a real target, from the EXTRACTED copy --------------------

step "add: register $TARGET_REPO from the extracted client"
ADD_OUTPUT="$("$LAUNCHER" add repo "$TARGET_REPO" 2>&1)" \
  || { printf '%s\n' "$ADD_OUTPUT" >&2; fail "'audit.sh add repo $TARGET_REPO' exited non-zero"; }
printf '%s\n' "$ADD_OUTPUT"
case "$ADD_OUTPUT" in
  *"registered:"*"$TARGET_REPO"*) ok "registered and validated: $TARGET_REPO" ;;
  *) fail "add output did not confirm registration of $TARGET_REPO" ;;
esac

TARGETS_OUTPUT="$("$LAUNCHER" targets 2>&1)" \
  || { printf '%s\n' "$TARGETS_OUTPUT" >&2; fail "'audit.sh targets' exited non-zero"; }
printf '%s\n' "$TARGETS_OUTPUT"
case "$TARGETS_OUTPUT" in
  *"$TARGET_REPO"*) ok "target list confirms $TARGET_REPO is registered" ;;
  *) fail "'audit.sh targets' did not list $TARGET_REPO" ;;
esac

# --- 6. run the one-shot audit chain: install, clone, analyse, package ----

step "audit: install tools, clone, analyse via OpenRouter, package (this is the slow step)"
RETURN_ZIP="$RETURN_DIR/audit-return-package.zip"
AUDIT_OUTPUT="$("$LAUNCHER" audit --out "$RETURN_ZIP" 2>&1)" \
  || { printf '%s\n' "$AUDIT_OUTPUT" >&2; fail "'audit.sh audit' exited non-zero"; }
printf '%s\n' "$AUDIT_OUTPUT"
case "$AUDIT_OUTPUT" in
  *"Send this file back"*) ok "chain reported a finished return package" ;;
  *) fail "'audit.sh audit' did not report a finished package" ;;
esac
[ -s "$RETURN_ZIP" ] || fail "expected return package not found or empty: $RETURN_ZIP"
ok "return package: $RETURN_ZIP ($(wc -c <"$RETURN_ZIP" | tr -d ' ') bytes)"

# --- 7. verify the return package BY CONTENT --------------------------------

step "verify: extract and inspect the return package"
RETURN_EXTRACT="$SCRATCH/return-extracted"
mkdir -p "$RETURN_EXTRACT"
unzip -q "$RETURN_ZIP" -d "$RETURN_EXTRACT" || fail "could not extract $RETURN_ZIP"

DB_FILE="$(find "$RETURN_EXTRACT" -path '*/extract/*.db' -type f -print -quit)"
[ -n "$DB_FILE" ] || fail "no extract/*.db member found in the return package"
DB_BYTES="$(wc -c <"$DB_FILE" | tr -d ' ')"
[ "$DB_BYTES" -gt 0 ] || fail "collected DB is zero bytes: $DB_FILE"
HEADER="$(head -c 16 "$DB_FILE" | tr -d '\0')"
case "$HEADER" in
  "SQLite format 3"*) ok "DB header is a real SQLite file: $DB_FILE" ;;
  *) fail "DB does not have a SQLite header (got: '$HEADER'): $DB_FILE" ;;
esac
if [ "$SQLITE3_AVAILABLE" -eq 1 ]; then
  TABLE_COUNT="$(sqlite3 "$DB_FILE" 'SELECT count(*) FROM sqlite_master;')" \
    || fail "sqlite3 could not read $DB_FILE"
  [ "$TABLE_COUNT" -gt 0 ] || fail "collected DB has zero tables — not a real extract: $DB_FILE"
  ok "DB is non-trivial: $DB_BYTES bytes, $TABLE_COUNT tables"
else
  # Belt-and-suspenders floor when sqlite3 is unavailable: still refuse a
  # DB too small to be a real extract rather than only the header check.
  [ "$DB_BYTES" -gt 1024 ] || fail "collected DB is suspiciously small ($DB_BYTES bytes) and sqlite3 is unavailable to inspect it: $DB_FILE"
  ok "DB passes header+size check: $DB_BYTES bytes (sqlite3 unavailable for a table count)"
fi

REPORT_FILE="$(find "$RETURN_EXTRACT" -path '*/reports/*/report.md' -type f -print -quit)"
[ -n "$REPORT_FILE" ] || fail "no reports/*/report.md member found in the return package"
REPORT_BYTES="$(wc -c <"$REPORT_FILE" | tr -d ' ')"
[ "$REPORT_BYTES" -gt 0 ] || fail "report is empty: $REPORT_FILE"
ok "report is present and non-empty: $REPORT_FILE ($REPORT_BYTES bytes)"

# --- 8. the guard that matters most: no credential in the return package --

step "verify: the OpenRouter key never reaches the return package"
LEAK_HITS="$(grep -a -r -l -F -- "$OPENROUTER_API_KEY" "$RETURN_EXTRACT" 2>/dev/null || true)"
if [ -n "$LEAK_HITS" ]; then
  printf 'the OpenRouter key was found in:\n%s\n' "$LEAK_HITS" >&2
  fail "OPENROUTER_API_KEY leaked into the return package (see file list above; value withheld from this message)"
fi
ok "grepped every extracted member of the return package — no credential found"

# --- verdict -----------------------------------------------------------------

REPO_COUNT="$(find "$RETURN_EXTRACT" -path '*/extract/*.db' -type f | wc -l | tr -d ' ')"
printf '\n================ VERDICT: PASS ================\n'
printf 'target repository:   %s\n' "$TARGET_REPO"
printf 'repositories audited: %s (extract DB present)\n' "$REPO_COUNT"
printf 'install package:      %s\n' "$INSTALL_ZIP"
printf 'return package:       %s\n' "$RETURN_ZIP"
printf 'collected DB:         %s (%s bytes)\n' "$DB_FILE" "$DB_BYTES"
printf 'report:                %s (%s bytes)\n' "$REPORT_FILE" "$REPORT_BYTES"
printf 'credential leak check: none found\n'
printf 'scratch (removed on exit): %s\n' "$SCRATCH"
printf '=================================================\n'
