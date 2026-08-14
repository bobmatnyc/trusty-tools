#!/usr/bin/env bash
#
# check_agent_assets.sh — trusty-code embedded agent-asset guard (issue #2958
# Slice E4; reshaped when the roster consolidated onto one physical copy).
#
# Why: this gate used to byte-compare 30 trusty-code agent `.md` copies against
#   their trusty-mpm sources, because both crates shipped their own copy of the
#   same file and nothing stopped one from drifting. That comparison is gone
#   along with the duplication: all 42 agent assets now live once, in
#   crates/trusty-agents-common/src/assets/agents/, and both crates embed THAT
#   file via `trusty_agents_common::agent_assets`. One physical file consumed by
#   two crates is a COMPILE-TIME property — strictly stronger than a CI diff,
#   which could only report drift after it had already landed on main.
#
#   Two things a compiler still cannot see, and this script is what checks them:
#
#   1. PINNED DEVIATION (4 files, $DEVIATED_FILES) — trusty-code keeps four
#      deliberately forked copies (qa.md, code-critic.md, code-analyzer.md,
#      web-qa.md) carrying a restrictive `tools:` frontmatter line plus, for
#      three of them, reworded read-only prose. They are supposed to differ, so
#      byte-parity is the wrong test. Instead this hashes the CURRENT SHARED
#      source file and compares it against the sha256 recorded in
#      scripts/agent-asset-pins.tsv. If the shared source moved on behind a
#      deliberately-deviated fork, FAIL so a human reconciles the fork on
#      purpose rather than letting the deviation go stale. See the
#      "Tools-restriction deviation" / "Prose deviation follow-up" doc blocks in
#      crates/trusty-code/src/assets/mod.rs for the full rationale.
#
#   2. NO RE-COPY — the successor to the parity check. Consolidation is only
#      durable if nobody re-adds a local copy of a shared agent. trusty-code's
#      agents directory must therefore contain EXACTLY the 8 accounted files:
#      the 4 pinned deviations above plus 4 tcode-only defaults ($TCODE_ONLY)
#      that have no shared counterpart. Any other `.md` appearing there is a
#      reintroduced duplicate — the precise regression the one-copy ruling
#      forbids — and fails here.
#
# What: two checks, plus a floor. Neither walks the shared roster looking for
#   drift, because a single file cannot drift from itself; both are about
#   trusty-code's own directory and its relationship to the shared source.
#
#   NOTE ON SCOPE: this only walks tcode-side files tracked in git
#   (`git ls-files`). A wholesale deletion of a shared asset needs no check here
#   — `include_str!` fails the build at compile time, and
#   `trusty_agents_common::agent_assets`'s own `table_matches_the_directory`
#   test fails if a roster file is added or removed without wiring it up.
#
#   --update       recompute the pinned sha256 for every file already listed in
#                  scripts/agent-asset-pins.tsv, after you have deliberately
#                  reconciled the trusty-code fork with new shared content. Only
#                  refreshes existing pins; refuses to add a new deviated file.
#                  Rewrites the file WHOLESALE from $DEVIATED_FILES — there is
#                  no way to re-pin a subset, so rebase before running it.
#   --force-add    like --update but also permits adding a pin for a basename in
#                  $DEVIATED_FILES with no row yet (i.e. you just declared a NEW
#                  deliberate deviation). Escape hatch; update the mod.rs doc
#                  block too.
#
# Usage:
#   bash scripts/check_agent_assets.sh              # check (CI mode)
#   bash scripts/check_agent_assets.sh --update      # re-pin after reconciling
#   bash scripts/check_agent_assets.sh --force-add   # re-pin + allow new deviations
#
# Test: exercised manually — a clean tree passes; a mutated shared source behind
#   a pinned fork fails as "UPSTREAM CHANGED"; an extra `.md` dropped into
#   trusty-code's agents dir fails as "RE-COPIED SHARED ASSET"; all revert
#   cleanly. No unit-test harness (pure file/hash comparison against tracked git
#   content, mirroring scripts/check_line_cap.sh and check_capabilities.sh).
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). POSIX tools only
# (`git`, `awk`, `shasum`/`sha256sum`). No associative arrays.

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

TCODE_DIR="crates/trusty-code/src/assets/agents"
SHARED_DIR="crates/trusty-agents-common/src/assets/agents"
PINS="scripts/agent-asset-pins.tsv"

# tcode's own defaults — no shared counterpart, never tracked against one.
# `engineer.md` deliberately does NOT track the shared `engineer.md`, which is
# excluded from tcode's roster specifically to avoid that name collision;
# `pm.md` was added for #3437 as tcode's own orchestrator default.
TCODE_ONLY="engineer.md qa-agent.md code-reviewer.md pm.md"

# The 4 files with a Bob-approved deliberate deviation from the shared source
# (Slice E3, PR #3041). Pinned by SHARED source hash, never byte-compared.
DEVIATED_FILES="code-analyzer.md code-critic.md qa.md web-qa.md"

# The exact number of `.md` files trusty-code may carry: 4 deviated + 4
# tcode-only. A gate that examined nothing must never report OK (#4618), and
# after consolidation this count IS the scan floor — it is exact, not a
# minimum, because any additional file is by definition a re-copied duplicate.
EXPECTED_TCODE_FILES=8

# ---------------------------------------------------------------------------
# Mode parsing
# ---------------------------------------------------------------------------
MODE="check"
ALLOW_NEW=0
for arg in "$@"; do
  case "$arg" in
    --update)    MODE="update" ;;
    --force-add) MODE="update"; ALLOW_NEW=1 ;;
    -h|--help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "check_agent_assets: unknown argument: $arg" >&2
      echo "usage: check_agent_assets.sh [--update | --force-add]" >&2
      exit 2
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
sha256_of() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

is_in() {
  local needle="$1"; shift
  local x
  for x in "$@"; do
    [ "$x" = "$needle" ] && return 0
  done
  return 1
}

# get_pinned_hash <shared-source-path> — prints the recorded sha256, or nothing.
get_pinned_hash() {
  local path="$1"
  awk -F'\t' -v p="$path" '$0 !~ /^#/ && $1 == p { print $2 }' "$PINS" 2>/dev/null
}

# DEVIATED_FILES/TCODE_ONLY are fixed, glob-free basename lists declared above —
# safe to word-split into arrays.
# shellcheck disable=SC2206
DEVIATED_ARR=($DEVIATED_FILES)
# shellcheck disable=SC2206
TCODE_ONLY_ARR=($TCODE_ONLY)

# ===========================================================================
# UPDATE MODE  (--update / --force-add)
# ===========================================================================
if [ "$MODE" = "update" ]; then
  NEWPINS="$(mktemp "${TMPDIR:-/tmp}/agentpins.new.XXXXXX")"
  trap 'rm -f "$NEWPINS"' EXIT

  {
    echo "# agent-asset-pins.tsv — pinned SHARED agent-asset source hashes for"
    echo "# deliberately deviated trusty-code copies (issue #2958 Slice E4)."
    echo "#"
    echo "# Format: <shared-source-relative-path><TAB><sha256-of-that-file>"
    echo "#"
    echo "# These 4 files are the KNOWN INTENTIONAL DEVIATIONS documented in"
    echo "# crates/trusty-code/src/assets/mod.rs (\"Tools-restriction deviation\" /"
    echo "# \"Prose deviation follow-up\" doc blocks, Slice E3, PR #3041): the tcode"
    echo "# copy carries an added \`tools:\` frontmatter line plus (for three of the"
    echo "# four) reworded read-only prose, so it can never be byte-identical to the"
    echo "# shared asset it was derived from."
    echo "#"
    echo "# Every OTHER agent asset is not copied at all — trusty-mpm and trusty-code"
    echo "# both embed the one file under crates/trusty-agents-common/src/assets/"
    echo "# agents/ via trusty_agents_common::agent_assets, so there is nothing to"
    echo "# pin. check_agent_assets.sh hashes the CURRENT shared source for these 4"
    echo "# and compares against the pin here; if it no longer matches, the shared"
    echo "# source changed behind a deliberately deviated fork and the guard fails so"
    echo "# a human reconciles that fork on purpose."
    echo "#"
    echo "# Regenerate (only after manually reconciling the trusty-code fork with the"
    echo "# new shared content) with: scripts/check_agent_assets.sh --update"
  } > "$NEWPINS"

  err=0
  for base in "${DEVIATED_ARR[@]}"; do
    shared_path="$SHARED_DIR/$base"
    if [ ! -f "$shared_path" ]; then
      echo "REFUSE: $shared_path does not exist — cannot pin a missing source file." >&2
      err=1
      continue
    fi
    existing="$(get_pinned_hash "$shared_path")"
    if [ -z "$existing" ] && [ "$ALLOW_NEW" -ne 1 ]; then
      echo "REFUSE: $shared_path has no existing pin in $PINS (new deviation)." >&2
      echo "        Pass --force-add to add it deliberately, and update the" >&2
      echo "        'Tools-restriction deviation' doc block in" >&2
      echo "        crates/trusty-code/src/assets/mod.rs to document it." >&2
      err=1
      continue
    fi
    new_hash="$(sha256_of "$shared_path")"
    printf '%s\t%s\n' "$shared_path" "$new_hash" >> "$NEWPINS"
  done

  if [ "$err" -ne 0 ]; then
    echo "check_agent_assets --update aborted: unresolved issues above." >&2
    exit 1
  fi

  mv "$NEWPINS" "$PINS"
  trap - EXIT
  echo "check_agent_assets: wrote $PINS with ${#DEVIATED_ARR[@]} pinned deviation(s)."
  exit 0
fi

# ===========================================================================
# CHECK MODE
# ===========================================================================
FAIL=0

if [ ! -f "$PINS" ]; then
  echo "FAIL: pins file $PINS is missing." >&2
  exit 1
fi

# --- 1. Pinned deviated files: must exist in the tcode dir, and their SHARED
#        source hash must still match the pin. Never byte-compared. ---
for base in "${DEVIATED_ARR[@]}"; do
  tcode_path="$TCODE_DIR/$base"
  shared_path="$SHARED_DIR/$base"

  if [ ! -f "$tcode_path" ]; then
    echo "FAIL: MISSING FORK — $tcode_path is a declared deviated file (see" >&2
    echo "      DEVIATED_FILES in scripts/check_agent_assets.sh) but does not exist." >&2
    FAIL=1
    continue
  fi
  if [ ! -f "$shared_path" ]; then
    echo "FAIL: MISSING SOURCE — $shared_path (the shared source the fork" >&2
    echo "      $tcode_path deviates from) no longer exists. If this agent was" >&2
    echo "      genuinely retired, drop the tcode fork too and remove it from" >&2
    echo "      DEVIATED_FILES + $PINS." >&2
    FAIL=1
    continue
  fi

  pinned="$(get_pinned_hash "$shared_path")"
  if [ -z "$pinned" ]; then
    echo "FAIL: UNPINNED DEVIATION — $shared_path has no entry in $PINS." >&2
    echo "      Run: scripts/check_agent_assets.sh --force-add" >&2
    FAIL=1
    continue
  fi

  current="$(sha256_of "$shared_path")"
  if [ "$current" != "$pinned" ]; then
    echo "FAIL: SHARED SOURCE CHANGED BEHIND A DEVIATED FORK — $shared_path no" >&2
    echo "      longer matches its pin in $PINS (pinned=$pinned current=$current)." >&2
    echo "      Reconcile $tcode_path with the new content on purpose (it" >&2
    echo "      deliberately deviates: tools: restriction + reworded prose)," >&2
    echo "      then run: scripts/check_agent_assets.sh --update" >&2
    FAIL=1
  fi
done

# --- 2. No re-copy. trusty-code's agents dir must hold ONLY the 8 accounted
#        files; anything else is a shared asset copied back in. ---
#
# #4618: the enumeration is materialised into a temp file rather than consumed
# from a `< <(git ls-files ...)` process substitution. A process substitution
# runs in a subshell exempt from BOTH `set -e` and `pipefail`, so a `git
# ls-files` that exited 128 fed the loop an empty stream and the gate reported
# OK over a genuinely broken scan. Redirecting from a file makes the exit status
# observable AND still runs the loop body in the current shell.
TCODE_LIST="$(mktemp "${TMPDIR:-/tmp}/agentassets.list.XXXXXX")"
trap 'rm -f "$TCODE_LIST"' EXIT
if ! git ls-files "$TCODE_DIR/*.md" > "$TCODE_LIST"; then
  echo "FAIL: TOOL ERROR — 'git ls-files $TCODE_DIR/*.md' exited non-zero." >&2
  echo "      The file set could not be enumerated, so nothing was checked." >&2
  echo "      This is NOT a pass (issue #4618)." >&2
  exit 1
fi

SEEN_COUNT=0
while IFS= read -r f; do
  [ -n "$f" ] || continue
  base="${f##*/}"
  SEEN_COUNT=$((SEEN_COUNT + 1))

  if is_in "$base" "${TCODE_ONLY_ARR[@]}"; then
    continue
  fi
  if is_in "$base" "${DEVIATED_ARR[@]}"; then
    continue
  fi

  echo "FAIL: RE-COPIED SHARED ASSET — $f is neither a declared tcode-only" >&2
  echo "      default (TCODE_ONLY) nor a pinned deviation (DEVIATED_FILES)." >&2
  echo "      Agent assets live ONCE, in $SHARED_DIR, and are embedded through" >&2
  echo "      trusty_agents_common::agent_assets by every consumer. Delete this" >&2
  echo "      file and reference the shared const instead. If it genuinely is" >&2
  echo "      tcode's own new agent with no shared counterpart, add it to" >&2
  echo "      TCODE_ONLY and raise EXPECTED_TCODE_FILES." >&2
  FAIL=1
done < "$TCODE_LIST"

# --- 3. Exact-count floor (#4618). Assert we examined what we expected to. ---
if [ "$SEEN_COUNT" -ne "$EXPECTED_TCODE_FILES" ]; then
  echo "FAIL: FILE COUNT — examined ${SEEN_COUNT} trusty-code agent file(s), expected" >&2
  echo "      exactly ${EXPECTED_TCODE_FILES} (4 pinned deviations + 4 tcode-only defaults)." >&2
  echo "      Fewer means a fork or default was deleted, or the enumeration broke;" >&2
  echo "      more means a shared asset was copied back in. A gate that scans" >&2
  echo "      nothing cannot fail, so this is a failure, not an OK (issue #4618)." >&2
  FAIL=1
fi

if [ "$FAIL" -ne 0 ]; then
  echo "agent-assets: FAILED — see FAIL lines above." >&2
  exit 1
fi

echo "agent-assets: ${SEEN_COUNT} trusty-code agent file(s), all accounted; ${#DEVIATED_ARR[@]} pinned deviation(s) match the shared source — OK."
echo "agent-assets: the other 42 assets are single-copy in $SHARED_DIR (no drift possible)."
exit 0
