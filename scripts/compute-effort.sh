#!/usr/bin/env bash
# compute-effort.sh — empirical commit-effort scoring
#
# Why: Commits in this repo range from XS (single Cargo.toml bump) to XL
# (multi-hundred-LoC refactors) in the same session. Carrying an empirically-
# derived effort indicator on each commit lets future analytics queries
# (commit-velocity, scope-creep detection, retro-style "what did we ship this
# week") use a calibrated, log-scaled scalar rather than raw LoC.
#
# What: Reads a git range, computes diff statistics, applies a composite
# formula (LoC + file count + tests factor, with cyclomatic complexity
# deferred to v2), maps the score onto T-shirt sizes, and prints JSON on
# stdout. Pure bash — only deps are git, awk, bc.
#
# Test: tests/test-compute-effort.sh — synthetic git repos exercise empty
# diff, single-file commit, multi-file commit, deleted-file diff, and JSON
# well-formedness.
#
# Usage: compute-effort.sh [<git-range>]
#   - default range: HEAD~1..HEAD
#   - examples:
#       compute-effort.sh                       # last commit
#       compute-effort.sh HEAD~5..HEAD          # last five commits
#       compute-effort.sh abc123..def456        # arbitrary range
#       compute-effort.sh HEAD                  # staged + last commit?? -- no, use staged check
#
# Output (stdout, single line JSON):
#   {"size":"M","score":8.30,"loc":243,"files":4,"test_loc":118,"tests_factor":0.85}
#
# Exit codes:
#   0  success — JSON written to stdout
#   1  git failure, empty diff, or other unrecoverable error (message on stderr)

set -euo pipefail

# THRESHOLDS — keep in sync with docs/research/commit-effort-spec-*.md
#
# Composite formula:
#   effort_score = ALPHA * log2(LoC + 1)
#                + BETA  * log2(files + 1)
#                + GAMMA * sum(delta-CC)               # v1: GAMMA=0
#                + DELTA * tests_factor
#
#   tests_factor = 1 - 0.3 * min(test_LoC / max(LoC, 1), 1)
#
# Calibrated against trusty-tools' last 100 commits; see spec doc for the
# histogram and tuning notes.
readonly ALPHA=1.0
readonly BETA=1.5
readonly GAMMA=0.0
readonly DELTA=1.0

readonly XS_MAX=6.0
readonly S_MAX=10.0
readonly M_MAX=14.0
readonly L_MAX=18.0
# > L_MAX => XL

# Regex matching files we treat as tests for the tests_factor.
# Covers: anything under a tests/ dir, *_test.rs / test_*.rs, *.spec.{ts,js,...},
# anything under __tests__/.
readonly TEST_REGEX='(^|/)(tests?|__tests__)/|(^|/)(test_[^/]+|[^/]+_test)\.(rs|py|go|js|ts|tsx)$|(^|/)[^/]+\.spec\.(rs|py|go|js|ts|tsx|jsx)$'

# --- helpers ---------------------------------------------------------------

die() {
    echo "compute-effort: $*" >&2
    exit 1
}

# log2(x) via bc -l. bc has only natural log, so divide by ln(2).
log2() {
    local x="$1"
    echo "l($x) / l(2)" | bc -l
}

# Round to two decimals.
round2() {
    printf '%.2f' "$1"
}

# Round to integer (away from zero).
round_int() {
    printf '%.0f' "$1"
}

# --- main ------------------------------------------------------------------

range="${1:-HEAD~1..HEAD}"

# Validate we're in a git repo.
git rev-parse --git-dir >/dev/null 2>&1 \
    || die "not a git repository (or git not on PATH)"

# Validate the range resolves.
if ! git rev-list --no-walk "$range" -- >/dev/null 2>&1; then
    # Try as a range-spec (e.g. A..B) by asking rev-list with the range form.
    if ! git rev-list "$range" >/dev/null 2>&1; then
        die "cannot resolve git range: $range"
    fi
fi

# Per-file stats via --numstat. Each line is:
#   <added>\t<deleted>\t<path>
# For binary files added/deleted are '-' — we skip those (treat as 0 LoC).
numstat=$(git diff --numstat "$range" 2>/dev/null) \
    || die "git diff failed for range: $range"

if [[ -z "$numstat" ]]; then
    die "empty diff for range: $range"
fi

# Parse numstat with awk: accumulate total LoC (added + deleted, ignoring
# binary '-' entries), file count, test LoC, and test file count.
parsed=$(awk -v test_re="$TEST_REGEX" '
    BEGIN { loc=0; files=0; test_loc=0; test_files=0 }
    {
        added=$1
        deleted=$2
        # path is everything from $3 onwards (handles renames "old => new" too)
        path=$3
        for (i=4; i<=NF; i++) path = path " " $i

        # Skip binary diffs ('-' added/deleted) — treat as zero LoC contribution.
        if (added == "-" || deleted == "-") {
            # Still count the file (a binary file change is a change), but
            # contribute 0 to LoC totals.
            files += 1
            next
        }

        file_loc = added + deleted
        loc += file_loc
        files += 1

        if (path ~ test_re) {
            test_loc += file_loc
            test_files += 1
        }
    }
    END {
        printf("%d %d %d %d\n", loc, files, test_loc, test_files)
    }
' <<<"$numstat")

# shellcheck disable=SC2206
parts=($parsed)
loc=${parts[0]}
files=${parts[1]}
test_loc=${parts[2]}
# test_files=${parts[3]}   # currently unused; retained in awk for future use

# Defensive: if everything came out as binary (files > 0, loc = 0), still
# allow the score (tiny commit) — but don't divide by zero.
loc_safe=$(( loc > 0 ? loc : 0 ))

# tests_factor = 1 - 0.3 * min(test_loc / max(loc, 1), 1)
# When loc=0, set ratio=0 (no penalty/bonus); tests_factor=1.0.
if [[ "$loc_safe" -gt 0 ]]; then
    ratio=$(echo "scale=6; r = $test_loc / $loc_safe; if (r > 1) r = 1; r" | bc -l)
else
    ratio="0"
fi
tests_factor=$(echo "scale=6; 1 - 0.3 * $ratio" | bc -l)

# Composite score:
#   ALPHA * log2(loc + 1) + BETA * log2(files + 1) + GAMMA * 0 + DELTA * tests_factor
loc_term=$(echo "scale=6; $ALPHA * $(log2 $((loc + 1)))" | bc -l)
files_term=$(echo "scale=6; $BETA * $(log2 $((files + 1)))" | bc -l)
tests_term=$(echo "scale=6; $DELTA * $tests_factor" | bc -l)

score=$(echo "scale=6; $loc_term + $files_term + $tests_term" | bc -l)

# Map score → T-shirt size.
size=$(awk -v s="$score" \
    -v xs="$XS_MAX" -v sm="$S_MAX" -v md="$M_MAX" -v lg="$L_MAX" '
    BEGIN {
        if (s <= xs) print "XS"
        else if (s <= sm) print "S"
        else if (s <= md) print "M"
        else if (s <= lg) print "L"
        else print "XL"
    }
')

# Format outputs.
score_fmt=$(round2 "$score")
tests_factor_fmt=$(round2 "$tests_factor")

# Emit JSON on stdout.
printf '{"size":"%s","score":%s,"loc":%d,"files":%d,"test_loc":%d,"tests_factor":%s}\n' \
    "$size" "$score_fmt" "$loc" "$files" "$test_loc" "$tests_factor_fmt"
