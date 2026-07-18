#!/usr/bin/env bash
#
# check_agent_assets.sh — tcode embedded agent-asset staleness guard (issue #2958 Slice E4).
#
# Why: Slice E2/E3 embedded 33 of trusty-mpm's agent `.md` files (5 BASE-*
#   templates + 28 coding-relevant roster agents) into trusty-code as
#   `include_str!` assets under crates/trusty-code/src/assets/agents/, copied
#   byte-for-byte from crates/trusty-mpm/src/assets/agents/ at the time of the
#   copy. Nothing mechanically stops trusty-mpm's source files from drifting
#   away from the tcode copies afterward — an upstream prompt tweak, a typo
#   fix, a new tool restriction — landing in trusty-mpm/src/assets/agents/
#   with zero signal that trusty-code's embedded copy is now stale. This gate
#   closes that gap.
#
# KNOWN INTENTIONAL DEVIATIONS (Slice E3, PR #3041, Bob's 2026-07-18 ruling):
#   four files — qa.md, code-critic.md, code-analyzer.md, web-qa.md — carry a
#   deliberate `tools:` frontmatter restriction in their tcode copy (plus, for
#   three of the four, reworded read-only prose) that is NOT present in the
#   trusty-mpm source. Byte-parity is the wrong test for these four. See the
#   "Tools-restriction deviation" / "Prose deviation follow-up" doc blocks in
#   crates/trusty-code/src/assets/mod.rs for the full rationale.
#
# What: two check strategies, chosen per file:
#   1. PARITY (29 files: 5 BASE-* + 24 roster agents) — the tcode copy must be
#      byte-identical to its trusty-mpm source (same basename). A mismatch
#      means the tcode copy silently drifted (edited without updating the
#      copy) OR trusty-mpm's source moved on without the copy following —
#      either way, FAIL and name the exact two paths to diff.
#   2. PINNED DEVIATION (4 files, $DEVIATED_FILES below) — never byte-compared
#      (they are supposed to differ). Instead this script hashes the CURRENT
#      trusty-mpm SOURCE file and compares it against the sha256 recorded in
#      scripts/agent-asset-pins.tsv. If upstream's source hash no longer
#      matches the pin, upstream changed behind a deliberately-deviated copy —
#      FAIL so a human reconciles the tcode copy on purpose (re-apply the
#      tools: restriction / read-only prose rewrite against the new upstream
#      content) rather than the deviation silently going stale.
#   Three tcode-only files (engineer.md, qa-agent.md, code-reviewer.md — tcode's
#   pre-existing defaults, never sourced from trusty-mpm; tcode's own
#   `engineer.md` deliberately does NOT track trusty-mpm's `engineer.md`,
#   which is excluded from the roster specifically to avoid the name
#   collision) are skipped entirely.
#   Any other tcode `.md` file not in the parity set, the deviated set, or the
#   tcode-only exclude list is flagged as "new roster file unaccounted" — add
#   it to TCODE_ONLY (if genuinely tcode's own) or verify its trusty-mpm
#   source path.
#
#   NOTE ON SCOPE: this only walks the tcode-side files that are tracked in
#   git (`git ls-files`), so it cannot detect a *wholesale deletion* of an
#   embedded copy that Rust's `include_str!` didn't already catch at compile
#   time (a missing embedded asset fails the trusty-code build). The 4 pinned
#   deviated files are the exception — their presence is checked explicitly
#   (see below) since Rust's compile error alone wouldn't explain *why* they
#   went missing.
#
#   --update       recompute the pinned sha256 for every file already listed
#                  in scripts/agent-asset-pins.tsv, after you've deliberately
#                  reconciled the tcode copy with new upstream content. Only
#                  refreshes existing pins; refuses to add a new deviated
#                  file to the pin set.
#   --force-add    like --update but also permits adding a pin for a basename
#                  in $DEVIATED_FILES that has no row in the pins file yet
#                  (i.e. you just declared a NEW deliberate deviation). Escape
#                  hatch; use sparingly and update the mod.rs doc block too.
#
# Usage:
#   bash scripts/check_agent_assets.sh              # check (CI mode)
#   bash scripts/check_agent_assets.sh --update      # re-pin after reconciling
#   bash scripts/check_agent_assets.sh --force-add   # re-pin + allow new deviations
#
# Test: exercised manually per Slice E4's task brief — a clean tree passes; a
#   byte-mutated parity copy fails as "COPY DRIFTED"; a mutated pinned mpm
#   source fails as "UPSTREAM CHANGED"; both revert cleanly. No unit-test
#   harness (pure file/hash comparison against tracked git content, mirrors
#   scripts/check_line_cap.sh and scripts/check_capabilities.sh conventions).
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). POSIX tools only
# (`git`, `awk`, `cmp`, `shasum`/`sha256sum`). No associative arrays.

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

TCODE_DIR="crates/trusty-code/src/assets/agents"
MPM_DIR="crates/trusty-mpm/src/assets/agents"
PINS="scripts/agent-asset-pins.tsv"

# tcode's own pre-existing defaults — never sourced from trusty-mpm.
TCODE_ONLY="engineer.md qa-agent.md code-reviewer.md"

# The 4 files with a Bob-approved deliberate deviation from byte-parity
# (Slice E3, PR #3041). Pinned by trusty-mpm SOURCE hash, not byte-compared.
DEVIATED_FILES="code-analyzer.md code-critic.md qa.md web-qa.md"

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

# get_pinned_hash <mpm-source-path> — prints the recorded sha256, or nothing.
get_pinned_hash() {
  local path="$1"
  awk -F'\t' -v p="$path" '$0 !~ /^#/ && $1 == p { print $2 }' "$PINS" 2>/dev/null
}

# DEVIATED_FILES/TCODE_ONLY are fixed, glob-free basename lists declared
# above — safe to word-split into arrays.
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
    echo "# agent-asset-pins.tsv — pinned trusty-mpm source hashes for deliberately"
    echo "# deviated tcode agent copies (issue #2958 Slice E4)."
    echo "#"
    echo "# Format: <mpm-source-relative-path><TAB><sha256-of-mpm-source-file>"
    echo "#"
    echo "# These 4 files are the KNOWN INTENTIONAL DEVIATIONS from byte-parity"
    echo "# documented in crates/trusty-code/src/assets/mod.rs (\"Tools-restriction"
    echo "# deviation\" / \"Prose deviation follow-up\" doc blocks, Slice E3, PR #3041):"
    echo "# the tcode copy carries an added \`tools:\` frontmatter line plus (for three"
    echo "# of the four) reworded read-only prose, so it can never be byte-identical"
    echo "# to the trusty-mpm source it was derived from."
    echo "#"
    echo "# check_agent_assets.sh does NOT byte-compare these 4 tcode copies against"
    echo "# their mpm source (that would always fail by design). Instead it hashes the"
    echo "# CURRENT mpm SOURCE file and compares against the pin recorded here. If the"
    echo "# mpm source hash no longer matches, upstream changed behind a deliberately"
    echo "# deviated copy — the guard fails so a human reconciles the tcode copy on"
    echo "# purpose, rather than silently drifting."
    echo "#"
    echo "# Regenerate a pin (only after manually reconciling the tcode copy with the"
    echo "# new upstream content) with: scripts/check_agent_assets.sh --update"
  } > "$NEWPINS"

  err=0
  for base in "${DEVIATED_ARR[@]}"; do
    mpm_path="$MPM_DIR/$base"
    if [ ! -f "$mpm_path" ]; then
      echo "REFUSE: $mpm_path does not exist — cannot pin a missing source file." >&2
      err=1
      continue
    fi
    existing="$(get_pinned_hash "$mpm_path")"
    if [ -z "$existing" ] && [ "$ALLOW_NEW" -ne 1 ]; then
      echo "REFUSE: $mpm_path has no existing pin in $PINS (new deviation)." >&2
      echo "        Pass --force-add to add it deliberately, and update the" >&2
      echo "        'Tools-restriction deviation' doc block in" >&2
      echo "        crates/trusty-code/src/assets/mod.rs to document it." >&2
      err=1
      continue
    fi
    new_hash="$(sha256_of "$mpm_path")"
    printf '%s\t%s\n' "$mpm_path" "$new_hash" >> "$NEWPINS"
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

# --- 1. Pinned deviated files: must exist in tcode dir, and their mpm SOURCE
#        hash must still match the pin. Never byte-compared against tcode. ---
for base in "${DEVIATED_ARR[@]}"; do
  tcode_path="$TCODE_DIR/$base"
  mpm_path="$MPM_DIR/$base"

  if [ ! -f "$tcode_path" ]; then
    echo "FAIL: MISSING COPY — $tcode_path is a declared deviated file (see" >&2
    echo "      DEVIATED_FILES in scripts/check_agent_assets.sh) but does not exist." >&2
    FAIL=1
    continue
  fi
  if [ ! -f "$mpm_path" ]; then
    echo "FAIL: MISSING SOURCE — $mpm_path (source for the deviated copy $tcode_path)" >&2
    echo "      no longer exists. If trusty-mpm genuinely removed this agent," >&2
    echo "      retire the tcode copy and drop it from DEVIATED_FILES + $PINS." >&2
    FAIL=1
    continue
  fi

  pinned="$(get_pinned_hash "$mpm_path")"
  if [ -z "$pinned" ]; then
    echo "FAIL: UNPINNED DEVIATION — $mpm_path has no entry in $PINS." >&2
    echo "      Run: scripts/check_agent_assets.sh --force-add" >&2
    FAIL=1
    continue
  fi

  current="$(sha256_of "$mpm_path")"
  if [ "$current" != "$pinned" ]; then
    echo "FAIL: UPSTREAM CHANGED BEHIND A DEVIATED COPY — $mpm_path no longer" >&2
    echo "      matches its pin in $PINS (pinned=$pinned current=$current)." >&2
    echo "      Reconcile $tcode_path with the new upstream content on purpose" >&2
    echo "      (it deliberately deviates: tools: restriction + reworded prose)," >&2
    echo "      then run: scripts/check_agent_assets.sh --update" >&2
    FAIL=1
  fi
done

# --- 2. Parity files: every tracked tcode .md not in TCODE_ONLY or
#        DEVIATED_FILES must byte-match its trusty-mpm source. ---
while IFS= read -r f; do
  [ -n "$f" ] || continue
  base="${f##*/}"

  if is_in "$base" "${TCODE_ONLY_ARR[@]}"; then
    continue
  fi
  if is_in "$base" "${DEVIATED_ARR[@]}"; then
    continue
  fi

  mpm_path="$MPM_DIR/$base"
  if [ ! -f "$mpm_path" ]; then
    echo "FAIL: NEW ROSTER FILE UNACCOUNTED — $f has no trusty-mpm source at" >&2
    echo "      $mpm_path and is not declared tcode-only (TCODE_ONLY in" >&2
    echo "      scripts/check_agent_assets.sh). Either add it to TCODE_ONLY (if it" >&2
    echo "      is genuinely tcode's own, not copied from trusty-mpm) or restore" >&2
    echo "      the trusty-mpm source path." >&2
    FAIL=1
    continue
  fi

  if ! cmp -s "$f" "$mpm_path"; then
    echo "FAIL: COPY DRIFTED — $f no longer byte-matches $mpm_path." >&2
    echo "      Either restore byte-parity (unintentional edit), or if this is a" >&2
    echo "      deliberate new deviation: add '$base' to DEVIATED_FILES in" >&2
    echo "      scripts/check_agent_assets.sh, pin it with --force-add, and" >&2
    echo "      document it in the 'Tools-restriction deviation' doc block in" >&2
    echo "      crates/trusty-code/src/assets/mod.rs." >&2
    FAIL=1
  fi
done < <(git ls-files "$TCODE_DIR/*.md")

if [ "$FAIL" -ne 0 ]; then
  echo "agent-assets: FAILED — see FAIL lines above." >&2
  exit 1
fi

echo "agent-assets: 29 byte-parity file(s) match, ${#DEVIATED_ARR[@]} pinned deviation(s) match upstream — OK."
exit 0
