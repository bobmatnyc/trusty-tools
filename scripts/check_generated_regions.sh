#!/usr/bin/env bash
# check_generated_regions.sh — every generated doc region has an owner.
#
# Why (#5205 follow-up): a marked region is only checked if some crate's
# `tests/generated_docs.rs` calls `assert_region` on that file. Markers pasted
# into a crate with no such test would look generated while being maintained by
# hand — exactly the failure mode the mechanism exists to remove. Coverage is
# opt-in, so this guard makes the opt-in visible rather than assumed.
#
# What: fails when a tracked markdown file carries a BEGIN marker on a line of
# its own but the crate that owns it has no `tests/generated_docs.rs`, and when
# a BEGIN marker has no matching END. The line anchor is load-bearing: prose
# and code fences that merely quote the marker syntax (this file, the reference
# page, the changelog fragments) indent or inline it, and must not be flagged.
# It does NOT flag a crate with no markers — see
# docs/reference/generated-doc-regions.md for why that is deliberate.
#
# #5440-followup — this gate used to fail open two ways, both demonstrated:
#   1. The whole marked-file set came from
#        marked=$(git ls-files '*.md' | xargs grep -l -E '…' 2>/dev/null || true)
#      The `|| true` terminates the pipeline, so ANY tool failure collapsed to an
#      empty string and took the `no generated regions found` early exit. With
#      `git ls-files` exiting 128 the gate exited 0 over a repo containing a real
#      violation. It passed identically when the marker string drifted.
#   2. The BEGIN/END balance read `begins=$(grep -c … || true)`. A hard grep
#      failure prints nothing, leaving BOTH counts empty, and `"" != ""` is
#      false — so the balance check silently agreed.
#   The fix is the shape the sound gates already use (check_agent_assets.sh,
#   check_line_cap.sh, check_doc_numbers.sh, check_generation_artifacts.sh):
#   materialize the enumeration to a file, escalate a non-zero `git ls-files` as
#   a TOOL ERROR instead of swallowing it, and floor a count of what was actually
#   OPENED rather than what was discovered. Every grep exit is now classified —
#   0 match, 1 no-match, >=2 tool error — so a broken tool can never read as a
#   clean tree. Paired self-test: scripts/check_scan_floor_selftest.sh
#   (`generated_regions`, `generated_regions_tool_error`).
#
# Usage: bash scripts/check_generated_regions.sh
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# ---------------------------------------------------------------------------
# Scan floors. A floor must count the WORK, not the discovery (#5440).
# ---------------------------------------------------------------------------

# Minimum tracked markdown files the enumeration must yield. Guards a `git
# ls-files` that succeeds but returns a truncated set. The tree carries ~1289
# tracked *.md; this sits far below that and far above zero.
MIN_MD_FILES=200

# Minimum files that must actually carry a BEGIN marker. This is the count that
# matters: the marker string is the gate's entire input, so if it drifts — a
# rename, a reformat, a stray leading space — every file stops matching while
# the markdown enumeration stays at full strength, and the gate reports a clean
# tree over zero checked regions.
#
# Set from the MEASURED tree: 5 marked files across 3 crates
# (crates/trusty-analyze/{CLAUDE,README}.md, crates/trusty-memory/README.md,
# crates/trusty-search/{CLAUDE,README}.md). Regions are only ever ADDED, so this
# is a ratchet: deliberately retiring one means lowering this constant in the
# same PR, which puts the removal in front of a reviewer instead of silently
# shrinking the gate's coverage to nothing.
MIN_MARKED_FILES=5

status=0

MDLIST="$(mktemp "${TMPDIR:-/tmp}/genregions.mdlist.XXXXXX")"
MARKED="$(mktemp "${TMPDIR:-/tmp}/genregions.marked.XXXXXX")"
trap 'rm -f "$MDLIST" "$MARKED"' EXIT

# ---------------------------------------------------------------------------
# 1. Materialize the enumeration. A non-zero `git ls-files` is a TOOL ERROR,
#    never an empty scan — that conflation is defect (1) above.
# ---------------------------------------------------------------------------
if ! git ls-files '*.md' > "$MDLIST"; then
  echo "FAIL: TOOL ERROR — 'git ls-files' exited non-zero; the markdown set could" >&2
  echo "      not be enumerated, so no generated regions were examined. That is a" >&2
  echo "      broken scan, not a clean tree (#4618, #5440)." >&2
  exit 1
fi

md_count=$(wc -l < "$MDLIST" | tr -d ' ')
if [ "$md_count" -lt "$MIN_MD_FILES" ]; then
  echo "FAIL: SCAN FLOOR — enumerated ${md_count} tracked markdown file(s), below the" >&2
  echo "      declared minimum of ${MIN_MD_FILES} (MIN_MD_FILES in scripts/check_generated_regions.sh)." >&2
  echo "      A gate with nothing to open reports success over nothing (#4618)." >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# 2. Open every enumerated file and classify it. Each grep exit is checked:
#    0 = marker found, 1 = absent (normal), >=2 = the tool itself failed.
#    `opened` counts files actually READ — the work, not the discovery.
# ---------------------------------------------------------------------------
: > "$MARKED"
opened=0
while IFS= read -r file; do
  [ -z "$file" ] && continue
  # A tracked path can legitimately be absent from the worktree mid-rebase;
  # that is a tool/state error for this gate, not a silently unmarked file.
  if [ ! -f "$file" ]; then
    echo "FAIL: TOOL ERROR — tracked markdown file '$file' is not readable in the" >&2
    echo "      worktree, so it could not be checked for generated regions (#5440)." >&2
    exit 1
  fi
  set +e
  grep -q -E '^<!-- BEGIN GENERATED: ' "$file"
  rc=$?
  set -e
  case "$rc" in
    0) printf '%s\n' "$file" >> "$MARKED" ;;
    1) ;;
    *)
      echo "FAIL: TOOL ERROR — 'grep' exited ${rc} scanning '$file'; the marker search" >&2
      echo "      failed rather than finding nothing. NOT a pass (#5440)." >&2
      exit 1
      ;;
  esac
  opened=$((opened + 1))
done < "$MDLIST"

marked_count=$(wc -l < "$MARKED" | tr -d ' ')

# ---------------------------------------------------------------------------
# 3. Floor the marked count. This is the arm that catches marker-string drift,
#    which defect (1) let through with a healthy-looking summary.
# ---------------------------------------------------------------------------
if [ "$marked_count" -lt "$MIN_MARKED_FILES" ]; then
  echo "FAIL: SCAN FLOOR — opened ${opened} markdown file(s) but found generated-region" >&2
  echo "      markers in only ${marked_count}, below the declared minimum of ${MIN_MARKED_FILES}" >&2
  echo "      (MIN_MARKED_FILES in scripts/check_generated_regions.sh)." >&2
  echo "      Either a region was retired without lowering the floor, or the marker" >&2
  echo "      string drifted and this gate is now checking nothing (#5440)." >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# 4. The actual checks, over files proven to carry markers.
# ---------------------------------------------------------------------------
while IFS= read -r file; do
  [ -z "$file" ] && continue

  # Every BEGIN needs its matching END, or the region silently swallows the
  # rest of the file when it is rewritten. `grep -c` prints the count and exits
  # 1 on zero matches, so only >=2 is a failure — and an EMPTY count is a tool
  # failure, which defect (2) above read as a balanced pair.
  set +e
  begins=$(grep -c -E '^<!-- BEGIN GENERATED: ' "$file"); brc=$?
  ends=$(grep -c -E '^<!-- END GENERATED: ' "$file"); erc=$?
  set -e
  if [ "$brc" -ge 2 ] || [ "$erc" -ge 2 ] || [ -z "$begins" ] || [ -z "$ends" ]; then
    echo "FAIL: TOOL ERROR — could not count BEGIN/END markers in '$file'" >&2
    echo "      (grep exits ${brc}/${erc}). Two empty counts compare EQUAL, which is" >&2
    echo "      how this check used to pass over a failure (#5440)." >&2
    exit 1
  fi
  if [ "$begins" -lt 1 ]; then
    echo "FAIL: TOOL ERROR — '$file' matched the marker search but recounts 0 BEGIN" >&2
    echo "      markers; the two passes disagree, so neither can be trusted (#5440)." >&2
    exit 1
  fi
  if [ "$begins" != "$ends" ]; then
    echo "ERROR: $file has $begins BEGIN and $ends END markers"
    status=1
  fi

  # Markers only mean something if a test in the owning crate checks them.
  case "$file" in
    crates/*)
      crate_dir="crates/$(echo "$file" | cut -d/ -f2)"
      if [ ! -f "$crate_dir/tests/generated_docs.rs" ]; then
        echo "ERROR: $file has generated regions but $crate_dir/tests/generated_docs.rs does not exist,"
        echo "       so nothing checks them. Add that test (see docs/reference/generated-doc-regions.md)."
        status=1
      fi
      ;;
    *)
      echo "ERROR: $file has generated regions but is outside crates/, where no test claims it."
      echo "       Move the region into a crate, or extend this script with an explicit owner."
      status=1
      ;;
  esac
done < "$MARKED"

if [ "$status" = 0 ]; then
  echo "check_generated_regions: OK (opened ${opened} markdown file(s); ${marked_count} carry generated regions, all claimed by a generated_docs test)"
fi
exit "$status"
