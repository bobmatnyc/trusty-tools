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
# Usage: bash scripts/check_generated_regions.sh
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

status=0
marked=$(git ls-files '*.md' | xargs grep -l -E '^<!-- BEGIN GENERATED: ' 2>/dev/null || true)

if [[ -z "$marked" ]]; then
  echo "check_generated_regions: no generated regions found"
  exit 0
fi

while IFS= read -r file; do
  [[ -z "$file" ]] && continue

  # Every BEGIN needs its matching END, or the region silently swallows the
  # rest of the file when it is rewritten.
  begins=$(grep -c -E '^<!-- BEGIN GENERATED: ' "$file" || true)
  ends=$(grep -c -E '^<!-- END GENERATED: ' "$file" || true)
  if [[ "$begins" != "$ends" ]]; then
    echo "ERROR: $file has $begins BEGIN and $ends END markers"
    status=1
  fi

  # Markers only mean something if a test in the owning crate checks them.
  if [[ "$file" == crates/* ]]; then
    crate_dir="crates/$(echo "$file" | cut -d/ -f2)"
    if [[ ! -f "$crate_dir/tests/generated_docs.rs" ]]; then
      echo "ERROR: $file has generated regions but $crate_dir/tests/generated_docs.rs does not exist,"
      echo "       so nothing checks them. Add that test (see docs/reference/generated-doc-regions.md)."
      status=1
    fi
  else
    echo "ERROR: $file has generated regions but is outside crates/, where no test claims it."
    echo "       Move the region into a crate, or extend this script with an explicit owner."
    status=1
  fi
done <<< "$marked"

if [[ "$status" == 0 ]]; then
  echo "check_generated_regions: OK ($(echo "$marked" | wc -l | tr -d ' ') file(s) claimed by a generated_docs test)"
fi
exit "$status"
