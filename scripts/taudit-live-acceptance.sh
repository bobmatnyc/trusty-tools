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
#   6. Opens the returned zip and checks its DATA, not just its shape: the
#      extract database holds ROWS in the tables the audit is made of, more
#      than one commit, more than one author, and the report names real
#      files from the audited repository.
#   7. Greps every extracted member of the returned zip for the OpenRouter
#      key and fails loudly if it finds it — the one guard whose failure is
#      invisible by inspection alone.
#
# What it deliberately does NOT do any more (#5915, #5916):
#   - It does not count `sqlite_master` rows. That counts TABLES, which a
#     migration creates whether or not one byte of data was ever collected,
#     so it passed against a database holding nothing.
#   - It does not check the report is merely non-empty. A report assembled
#     from an empty extract is several KiB of headings.
#   - It does not put its scratch tree under `mktemp -d`. On macOS that is
#     `/var/folders/...`, which `trusty-search` refuses to index — so the
#     script structurally guaranteed the very refusal it should have been
#     able to catch, and no placement bug could ever make it fail.
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
# per-run directory beneath ~/.taudit-acceptance, so `taudit distribute`'s
# refusal to overwrite an existing package never triggers — there is never a
# package already at the destination. Two consecutive runs are two
# independent scratch trees. Nothing under ~/duetto/audit is ever touched.
#
# NOT under `mktemp -d`: see the note above. `~/.taudit-acceptance` is a
# location `trusty-search` permits, which is the point — the script must be
# able to observe a placement bug rather than guarantee one.
#
# What this script leaves behind on your machine, and how to remove it:
#   - `~/.trusty-tools/trusty-audit/work` — the run's own tree. `rm -rf` it.
#   - One `trusty-search` allowlist row per audited clone, in
#     `~/Library/Application Support/trusty-search/allowlist.toml`. The script
#     prints them and asserts it destroyed no PRE-EXISTING row, but it does
#     not remove its own: `trusty-search index remove` ignores its path
#     argument in 0.45.1 and resolves the index from the CWD instead, so
#     calling it here would drop an unrelated index. Remove them by hand.

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

# A small, real, public repository with a REAL history: 1.2 MiB, ~400
# commits by ~30 authors over a decade, and actual Rust source for the code
# analysis to have something to name. The previous default,
# octocat/Hello-World, was too small to test anything — 3 commits means a
# "more than one commit" assertion barely separates a full clone from a
# shallow one, and a repository of one README gives the code-analysis leg no
# file to mention whether it worked or not.
# Override with TAUDIT_E2E_REPO=owner/name for a heavier exercise.
TARGET_REPO="${TAUDIT_E2E_REPO:-BurntSushi/xsv}"
ok "audit target: $TARGET_REPO (override with TAUDIT_E2E_REPO)"

# --- scratch state, cleaned up on success, kept (and named) on failure ---

# Deliberately NOT `mktemp -d`: on macOS that is /var/folders/..., which
# `trusty-search` refuses to index (SENSITIVE_PATH_PREFIXES). A run whose
# every path is pre-refused cannot distinguish a working pipeline from a
# broken one. `~/.taudit-acceptance` is permitted — it is not `~` itself and
# not one of SENSITIVE_HOME_TOP_DIRS (Desktop, Downloads, Documents,
# Pictures, Movies, Music, Library).
SCRATCH="$HOME/.taudit-acceptance/run-$$-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$SCRATCH"
case "$SCRATCH" in
  /var/folders/*|/private/var/folders/*|/tmp/*|/private/tmp/*)
    fail "scratch root landed on a path trusty-search refuses to index: $SCRATCH" ;;
esac
cleanup() {
  local exit_code=$?
  if [ "$exit_code" -eq 0 ]; then
    rm -rf "$SCRATCH"
  else
    printf '\n[FAIL] leaving scratch state for inspection: %s\n' "$SCRATCH" >&2
  fi
}
trap cleanup EXIT

# The run's own tree is no longer beside the extracted package (#5915): it is
# at the client's default, which is the one home location trusty-search will
# index. A previous run's tree would make "did this run collect anything"
# unanswerable, so it starts clean.
WORK_ROOT="$HOME/.trusty-tools/trusty-audit/work"
rm -rf "$WORK_ROOT"
ok "work root reset: $WORK_ROOT"

# The allowlist rows this run adds live outside the work root, so `rm -rf` on
# either does not undo them. Snapshot what was approved BEFORE, so the run
# can be shown to have destroyed none of it.
ALLOWLIST="$HOME/Library/Application Support/trusty-search/allowlist.toml"
ALLOWLIST_BEFORE="$SCRATCH/allowlist-before.toml"
if [ -f "$ALLOWLIST" ]; then
  cp "$ALLOWLIST" "$ALLOWLIST_BEFORE"
  ok "snapshotted the pre-existing trusty-search allowlist ($(grep -c '^\[\[index\]\]' "$ALLOWLIST_BEFORE" || true) entries)"
else
  : >"$ALLOWLIST_BEFORE"
  ok "no pre-existing trusty-search allowlist to preserve"
fi

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
# The check this script used to make was `SELECT count(*) FROM sqlite_master`
# — a count of TABLES. Every migration creates those on an empty database, so
# it passed against an extract holding no data at all, which is exactly the
# state #5916 (shallow clone) and #5915 (unapproved index) produced. Count
# ROWS in the tables the audit is actually made of.
if [ "$SQLITE3_AVAILABLE" -eq 0 ]; then
  fail "sqlite3 is not on PATH. This script's central claim is that the collected
       database holds DATA, and only sqlite3 can check that. A size-and-header
       fallback passes against an empty extract, which is the defect class this
       test exists to catch. Install sqlite3 and re-run."
fi

query() {
  sqlite3 "$DB_FILE" "$1" 2>/dev/null || printf 'ERR'
}

COMMIT_ROWS="$(query 'SELECT count(*) FROM commits;')"
AUTHOR_ROWS="$(query 'SELECT count(*) FROM authors;')"
FILE_ROWS="$(query 'SELECT count(*) FROM files;')"
case "$COMMIT_ROWS$AUTHOR_ROWS$FILE_ROWS" in
  *ERR*) fail "the extract is missing the core audit tables (commits/authors/files): $DB_FILE" ;;
esac

# #5916: `taudit clone` appended `--depth=1`, so every repository reached tga
# as ONE synthetic commit by ONE author, with the whole tree credited to
# whoever last touched each line. A full clone of the default target is ~400
# commits by ~30 authors. Asserting "more than one" is the weakest statement
# that a shallow clone cannot satisfy.
[ "$COMMIT_ROWS" -gt 1 ] || fail "the extract holds $COMMIT_ROWS commit(s).
       One commit is the signature of a shallow clone (#5916): the whole
       history collapses to a single synthetic commit and every metric derived
       from it — authors, tenure, churn over time — is fabricated."
[ "$AUTHOR_ROWS" -gt 1 ] || fail "the extract holds $AUTHOR_ROWS author(s) across $COMMIT_ROWS commits.
       A real history of this repository has ~30. One author is what a shallow
       clone produces (#5916)."
[ "$FILE_ROWS" -gt 0 ] || fail "the extract holds no file rows — nothing was collected: $DB_FILE"

# A period whose start equals its end is the other shallow-clone signature:
# one commit means one timestamp, so the "last 52 weeks" window is a point.
COMMIT_SPAN_DAYS="$(query "SELECT CAST(julianday(max(timestamp)) - julianday(min(timestamp)) AS INTEGER) FROM commits;")"
case "$COMMIT_SPAN_DAYS" in
  ERR|'') printf '[INFO] commits.timestamp is absent or unreadable — span check skipped\n' ;;
  *) [ "$COMMIT_SPAN_DAYS" -gt 0 ] || fail "every commit in the extract shares one date —
       the history has no span, which is what a depth-1 clone produces (#5916)." ;;
esac

ok "DB holds real data: $COMMIT_ROWS commits, $AUTHOR_ROWS authors, $FILE_ROWS files, ${COMMIT_SPAN_DAYS:-?} days of history ($DB_BYTES bytes)"

REPORT_FILE="$(find "$RETURN_EXTRACT" -path '*/reports/*/report.md' -type f -print -quit)"
[ -n "$REPORT_FILE" ] || fail "no reports/*/report.md member found in the return package"
REPORT_BYTES="$(wc -c <"$REPORT_FILE" | tr -d ' ')"
[ "$REPORT_BYTES" -gt 0 ] || fail "report is empty: $REPORT_FILE"

# #5915: "non-empty" was never the question. A report assembled from an
# extract the code analysis never read is several KiB of headings with no
# finding under them — which is precisely what an unapproved checkout
# produced, silently, on every run. The report must name files that exist in
# the repository that was audited.
step "verify: the report names real files from the audited repository"
CHECKOUT_DIR="$(find "$WORK_ROOT/repos" -maxdepth 1 -mindepth 1 -type d -print -quit 2>/dev/null || true)"
[ -n "$CHECKOUT_DIR" ] || fail "no checkout under $WORK_ROOT/repos — the clone phase left nothing behind"

# Real source files from the clone, by basename. A report that read the code
# names some of them; one assembled from nothing names none.
NAMED=0
NAMED_EXAMPLES=""
while IFS= read -r candidate; do
  base="$(basename "$candidate")"
  if grep -q -F -- "$base" "$REPORT_FILE"; then
    NAMED=$((NAMED + 1))
    [ -n "$NAMED_EXAMPLES" ] && NAMED_EXAMPLES="$NAMED_EXAMPLES, "
    NAMED_EXAMPLES="$NAMED_EXAMPLES$base"
  fi
done <<EOF
$(find "$CHECKOUT_DIR" -type f \( -name '*.rs' -o -name '*.py' -o -name '*.go' -o -name '*.ts' -o -name '*.js' -o -name '*.java' \) -not -path '*/.git/*' | head -40)
EOF

[ "$NAMED" -gt 0 ] || fail "the report ($REPORT_BYTES bytes) names not one source file from
       $CHECKOUT_DIR. That is what a report looks like when the code-analysis
       leg read nothing — trusty-search is default-deny, so an unapproved
       checkout is refused and tga still exits 0 (#5915)."
ok "report names $NAMED source file(s) from the checkout: $NAMED_EXAMPLES"
ok "report is present and substantive: $REPORT_FILE ($REPORT_BYTES bytes)"

# --- 8. the guard that matters most: no credential in the return package --

step "verify: the OpenRouter key never reaches the return package"
LEAK_HITS="$(grep -a -r -l -F -- "$OPENROUTER_API_KEY" "$RETURN_EXTRACT" 2>/dev/null || true)"
if [ -n "$LEAK_HITS" ]; then
  printf 'the OpenRouter key was found in:\n%s\n' "$LEAK_HITS" >&2
  fail "OPENROUTER_API_KEY leaked into the return package (see file list above; value withheld from this message)"
fi
ok "grepped every extracted member of the return package — no credential found"

# --- 9. the approvals this run added, and the ones it must not have removed --

step "verify: the run added allowlist rows and destroyed no pre-existing one"
if [ -s "$ALLOWLIST_BEFORE" ]; then
  MISSING=""
  while IFS= read -r prior; do
    grep -q -F -- "$prior" "$ALLOWLIST" 2>/dev/null || MISSING="$MISSING$prior
"
  done <<EOF
$(grep '^path *=' "$ALLOWLIST_BEFORE" || true)
EOF
  if [ -n "$MISSING" ]; then
    printf '%s\n' "$MISSING" >&2
    fail "the run removed allowlist entries that existed before it (listed above)"
  fi
  ok "every pre-existing allowlist entry survived the run"
fi

APPROVED="$(grep '^path *=' "$ALLOWLIST" 2>/dev/null | grep -F -- "$WORK_ROOT" || true)"
if [ -n "$APPROVED" ]; then
  ok "the run approved these clones for indexing (remove by hand — see the header):"
  printf '%s\n' "$APPROVED"
else
  fail "no allowlist row names a clone under $WORK_ROOT.
       The audit reported success, but nothing approved a checkout — which
       means trusty-search refused the index and the code analysis read
       nothing (#5915). A report can still be produced in that state, which is
       why this check exists rather than trusting the exit status."
fi

# --- verdict -----------------------------------------------------------------

REPO_COUNT="$(find "$RETURN_EXTRACT" -path '*/extract/*.db' -type f | wc -l | tr -d ' ')"
printf '\n================ VERDICT: PASS ================\n'
printf 'target repository:    %s\n' "$TARGET_REPO"
printf 'repositories audited: %s (extract DB present)\n' "$REPO_COUNT"
printf 'install package:      %s\n' "$INSTALL_ZIP"
printf 'return package:       %s\n' "$RETURN_ZIP"
printf 'collected DB:         %s (%s bytes)\n' "$DB_FILE" "$DB_BYTES"
printf 'collected data:       %s commits, %s authors, %s files, %s days\n' \
  "$COMMIT_ROWS" "$AUTHOR_ROWS" "$FILE_ROWS" "${COMMIT_SPAN_DAYS:-?}"
printf 'report:               %s (%s bytes, names %s checkout file(s))\n' \
  "$REPORT_FILE" "$REPORT_BYTES" "$NAMED"
printf 'credential leak check: none found\n'
printf 'work root (kept):     %s\n' "$WORK_ROOT"
printf 'scratch (removed on exit): %s\n' "$SCRATCH"
printf '=================================================\n'
