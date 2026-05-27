#!/usr/bin/env bash
# insert-effort-trailers.sh — pre-commit prepare-commit-msg hook entry point
#
# Why: Keeps compute-effort.sh pure (JSON in/out, no message mutation). This
# wrapper handles the commit-message-mutation side effect — invoked by the
# pre-commit framework at the prepare-commit-msg stage, it computes effort
# for the staged diff and appends Effort / Effort-Score / Effort-Breakdown
# trailers to the draft commit message so the user can review or override
# before saving.
#
# What: Reads the commit-message-file path from $1 and the commit source from
# $2. Skips merge and squash commits. Computes effort for the staged diff
# (i.e. comparing the index against HEAD). Uses git interpret-trailers to
# append/replace the three trailers in-place. Never blocks a commit — any
# failure prints a warning to stderr and exits 0.
#
# Test: tests/test-compute-effort.sh covers the underlying compute-effort.sh.
# This wrapper is exercised manually by committing in a repo with the hook
# installed; behaviour is observable in the editor's draft message.

set -uo pipefail

msg_file="${1:-}"
commit_source="${2:-}"

# Bail cleanly if invoked without a message file (shouldn't happen via hook).
[[ -z "$msg_file" || ! -f "$msg_file" ]] && exit 0

# Skip merge, squash, and amend (template / message / commit sources still
# get trailers; these three are noisy / dangerous to rewrite).
case "$commit_source" in
    merge|squash) exit 0 ;;
esac

# Resolve script directory so the hook works from any cwd.
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
compute="$script_dir/compute-effort.sh"

[[ -x "$compute" ]] || { echo "insert-effort-trailers: $compute not executable" >&2; exit 0; }

# Compute effort for the staged diff (index vs HEAD). --cached makes git diff
# operate on the staged content, which is what the hook should score.
#
# Strategy: write a tiny shim range. compute-effort.sh accepts a range, so we
# call git diff --cached --numstat directly here for the staged case rather
# than retrofitting --cached into the script. Mirror the script's parsing.
numstat=$(git diff --cached --numstat 2>/dev/null) || {
    echo "insert-effort-trailers: git diff --cached failed" >&2
    exit 0
}

if [[ -z "$numstat" ]]; then
    # No staged changes (e.g. amend with no edits). Skip silently.
    exit 0
fi

# Reuse the same THRESHOLDS / formula by sourcing constants from compute-effort.sh
# via a one-shot helper invocation: feed the staged diff through awk + bc
# inline. To avoid duplication, we instead invoke compute-effort.sh against a
# synthetic range "--cached" by temporarily creating a worktree-relative
# helper. Simpler: replicate just the awk/bc pipeline here.

# THRESHOLDS — must match scripts/compute-effort.sh.
ALPHA=1.0
BETA=1.5
DELTA=1.0
XS_MAX=6.0
S_MAX=10.0
M_MAX=14.0
L_MAX=18.0

TEST_REGEX='(^|/)(tests?|__tests__)/|(^|/)(test_[^/]+|[^/]+_test)\.(rs|py|go|js|ts|tsx)$|(^|/)[^/]+\.spec\.(rs|py|go|js|ts|tsx|jsx)$'

parsed=$(awk -v test_re="$TEST_REGEX" '
    BEGIN { loc=0; files=0; test_loc=0 }
    {
        added=$1; deleted=$2
        path=$3; for (i=4; i<=NF; i++) path = path " " $i
        if (added == "-" || deleted == "-") { files += 1; next }
        file_loc = added + deleted
        loc += file_loc; files += 1
        if (path ~ test_re) test_loc += file_loc
    }
    END { printf("%d %d %d\n", loc, files, test_loc) }
' <<<"$numstat")

# shellcheck disable=SC2206
parts=($parsed)
loc=${parts[0]}
files=${parts[1]}
test_loc=${parts[2]}

# Compute score (replicates compute-effort.sh formula).
loc_safe=$(( loc > 0 ? loc : 0 ))
if [[ "$loc_safe" -gt 0 ]]; then
    ratio=$(echo "scale=6; r = $test_loc / $loc_safe; if (r > 1) r = 1; r" | bc -l)
else
    ratio="0"
fi
tests_factor=$(echo "scale=6; 1 - 0.3 * $ratio" | bc -l)
loc_term=$(echo "scale=6; $ALPHA * (l(${loc}+1) / l(2))" | bc -l)
files_term=$(echo "scale=6; $BETA * (l(${files}+1) / l(2))" | bc -l)
score=$(echo "scale=6; $loc_term + $files_term + $DELTA * $tests_factor" | bc -l)

size=$(awk -v s="$score" -v xs="$XS_MAX" -v sm="$S_MAX" -v md="$M_MAX" -v lg="$L_MAX" '
    BEGIN {
        if (s <= xs) print "XS"
        else if (s <= sm) print "S"
        else if (s <= md) print "M"
        else if (s <= lg) print "L"
        else print "XL"
    }
')

score_fmt=$(printf '%.2f' "$score")

# Append trailers (replace existing ones if already present).
git interpret-trailers --in-place \
    --if-exists replace \
    --trailer "Effort: $size" \
    --trailer "Effort-Score: $score_fmt" \
    --trailer "Effort-Breakdown: $loc LoC | $files files | $test_loc test LoC" \
    "$msg_file" 2>/dev/null || {
    echo "insert-effort-trailers: git interpret-trailers failed (non-fatal)" >&2
}

exit 0
