#!/usr/bin/env bash
#
# perf_compare_selftest.sh — fixture cases for scripts/perf_compare.sh and
# scripts/perf_status_write.sh (issue #10210, phase 1).
#
# Why: a comparison only ever seen passing cannot be told apart from one that
#   prints ok unconditionally. Each case asserts the exit code AND the verdict
#   written to the status file, and two cases sit exactly on the warn and alert
#   levels so that `>` in place of `>=` fails here.
# What: runs perf_compare.sh against fixtures in scripts/test-data/perf-compare/
#   and pipes each report through perf_status_write.sh into a temp status file.
#   baseline.json and current-2026-10-23.json are cut from the two committed
#   docs/trusty-memory/perf/*-evo.json files; the p95-* fixtures are
#   baseline.json with the rulings-off p95 scaled; err-* fixtures must exit 12.
#   It then checks the status writer: skipped streak, reason rule, other tools
#   kept, a non-ignored in-repo path refused, and the default path git-ignored.
#
# Test: this IS the test. Run directly:
#   ./scripts/perf_compare_selftest.sh
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux); needs jq and git.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
COMPARE="$SCRIPT_DIR/perf_compare.sh"
STATUS="$SCRIPT_DIR/perf_status_write.sh"
FIX="$SCRIPT_DIR/test-data/perf-compare"
TOOL="trusty-memory"

work="$(mktemp -d "${TMPDIR:-/tmp}/perf_compare_selftest.XXXXXX")"
trap 'rm -rf "$work"' EXIT
status_file="$work/status.json"

fail=0
pass() { printf 'PASS %s\n' "$1"; }
bad() {
	printf 'FAIL %s: %s\n' "$1" "$2"
	fail=1
}
expect() { # $1 = name, $2 = got, $3 = want
	if [ "$2" = "$3" ]; then pass "$1"; else bad "$1" "got '$2', want '$3'"; fi
}

# fixture<TAB>exit<TAB>verdict<TAB>metric whose row is checked<TAB>delta<TAB>severity
TAB="$(printf '\t')"
OFF="memory_recall_latency_rulings-off"
CASES="current-2026-10-23${TAB}11${TAB}alert${TAB}seeded_in${TAB}56.48${TAB}alert
current-2026-10-23${TAB}11${TAB}alert${TAB}${OFF}${TAB}-4.97${TAB}ok
baseline${TAB}0${TAB}ok${TAB}${OFF}${TAB}0${TAB}ok
p95-plus24${TAB}0${TAB}ok${TAB}${OFF}${TAB}24${TAB}ok
p95-plus25${TAB}10${TAB}warn${TAB}${OFF}${TAB}25${TAB}warn
p95-plus26${TAB}10${TAB}warn${TAB}${OFF}${TAB}26${TAB}warn
p95-plus50${TAB}11${TAB}alert${TAB}${OFF}${TAB}50${TAB}alert
p95-plus51${TAB}11${TAB}alert${TAB}${OFF}${TAB}51${TAB}alert
p95-minus60${TAB}0${TAB}ok${TAB}${OFF}${TAB}-60${TAB}ok"

while IFS="$TAB" read -r fixture want_exit want_verdict metric want_delta want_sev; do
	name="$fixture/$metric"
	out="$work/$fixture.report.json"
	rc=0
	"$COMPARE" "$FIX/baseline.json" "$FIX/$fixture.json" >"$out" 2>"$work/$fixture.err" || rc=$?
	[ "$rc" = "$want_exit" ] || {
		bad "$name" "exit $rc, want $want_exit"
		continue
	}
	got="$(jq -r --arg m "$metric" \
		'"\(.verdict) \(.rows[] | select(.metric == $m) | "\(.delta_pct) \(.severity)")"' "$out")"
	[ "$got" = "$want_verdict $want_delta $want_sev" ] || {
		bad "$name" "got '$got', want '$want_verdict $want_delta $want_sev'"
		continue
	}
	rm -f "$status_file"
	"$STATUS" --tool "$TOOL" --compare "$out" --status-file "$status_file" 2>/dev/null ||
		{
			bad "$name" "status write failed"
			continue
		}
	written="$(jq -r --arg t "$TOOL" '.tools[$t].verdict' "$status_file")"
	[ "$written" = "$want_verdict" ] || {
		bad "$name" "status.json verdict '$written', want '$want_verdict'"
		continue
	}
	pass "$name exit=$rc verdict=$want_verdict delta=$want_delta% status=$written"
done <<EOF
$CASES
EOF

# The real comparison must report the p95 and seeded_in deltas, skip the
# effectiveness gate rows, and list the row the newer file lacks.
real="$work/current-2026-10-23.report.json"
if jq -e '([.rows[].metric] | index("hit@1")) == null
	and (.missing == ["concurrent_perf test_concurrent_rw WRITE"])
	and ([.rows[] | select(.field == "p95")] | length) == 3' "$real" >/dev/null; then
	pass "current-2026-10-23 rows: 3 p95, seeded_in graded, hit@1 not graded, WRITE missing"
else
	bad "current-2026-10-23 rows" "$(jq -c '{rows: [.rows[] | [.metric, .field]], missing}' "$real")"
fi

# Error envelopes: exit 12 and nothing on stdout.
for fixture in err-malformed err-schema-2 err-tool err-label err-no-results no-such-file; do
	rc=0
	"$COMPARE" "$FIX/baseline.json" "$FIX/$fixture.json" >"$work/err.out" 2>"$work/err.err" || rc=$?
	if [ "$rc" = 12 ] && [ ! -s "$work/err.out" ]; then
		pass "$fixture exit=12"
	else
		bad "$fixture" "exit $rc, stdout $(wc -c <"$work/err.out") bytes; want 12 and empty"
	fi
done
rc=0
"$COMPARE" "$FIX/baseline.json" >/dev/null 2>&1 || rc=$?
expect "one argument exit=12" "$rc" 12

# Fail closed (#10210 review): an ungradable or missing row raises the verdict,
# and a figure with no meaningful percentage is an error.
# baseline<TAB>current<TAB>exit<TAB>report check ("-": stdout must be empty)
CLOSED="baseline${TAB}err-empty-results${TAB}12${TAB}-
err-empty-results${TAB}baseline${TAB}12${TAB}-
baseline${TAB}missing-renamed${TAB}10${TAB}.verdict == \"warn\" and .missing == [\"$OFF\"]
baseline${TAB}unparsed-null-p95${TAB}10${TAB}.verdict == \"warn\" and .unparsed == [\"$OFF\"]
baseline${TAB}unparsed-string-p95${TAB}10${TAB}.verdict == \"warn\" and .unparsed == [\"$OFF\"]
err-zero-baseline${TAB}p95-plus24${TAB}12${TAB}-
err-negative-baseline${TAB}baseline${TAB}12${TAB}-
baseline${TAB}err-nan-p95${TAB}12${TAB}-
err-nan-p95${TAB}baseline${TAB}12${TAB}-"
while IFS="$TAB" read -r base cur want_exit check; do
	name="$base vs $cur"
	rc=0
	"$COMPARE" "$FIX/$base.json" "$FIX/$cur.json" >"$work/closed.out" 2>/dev/null || rc=$?
	if [ "$rc" != "$want_exit" ]; then
		bad "$name" "exit $rc, want $want_exit"
	elif [ "$check" = "-" ] && [ -s "$work/closed.out" ]; then
		bad "$name" "stdout not empty on an error"
	elif [ "$check" != "-" ] && ! jq -e "$check" "$work/closed.out" >/dev/null 2>&1; then
		bad "$name" "report fails: $check"
	else
		pass "$name exit=$rc"
	fi
done <<EOF
$CLOSED
EOF

# Status writer.
rm -f "$status_file"
"$STATUS" --tool trusty-search --verdict ok --status-file "$status_file" 2>/dev/null
"$STATUS" --tool "$TOOL" --verdict skipped --reason "EVO unreachable" --status-file "$status_file" 2>/dev/null
"$STATUS" --tool "$TOOL" --verdict skipped --reason "EVO load 2.4" --status-file "$status_file" 2>/dev/null
got="$(jq -r '"\(.tools["trusty-memory"] | "\(.verdict) \(.skipped_streak) \(.reason)") \(.tools["trusty-search"].verdict)"' "$status_file")"
expect "two skipped runs: streak=2, other tool kept" "$got" "skipped 2 EVO load 2.4 ok"
"$STATUS" --tool "$TOOL" --verdict failed --reason "suite exit 101" --status-file "$status_file" 2>/dev/null
got="$(jq -r '.tools["trusty-memory"] | "\(.verdict) \(.skipped_streak)"' "$status_file")"
expect "failed run resets streak" "$got" "failed 0"

rc=0
"$STATUS" --tool "$TOOL" --verdict skipped --status-file "$status_file" 2>/dev/null || rc=$?
expect "skipped without --reason exit=2" "$rc" 2

rc=0
"$STATUS" --tool trusty-search --compare "$real" --status-file "$status_file" 2>/dev/null || rc=$?
expect "report for another tool exit=2" "$rc" 2

tracked="$FIX/status.json"
rc=0
"$STATUS" --tool "$TOOL" --verdict ok --status-file "$tracked" 2>/dev/null || rc=$?
if [ "$rc" = 2 ] && [ ! -e "$tracked" ]; then
	pass "non-ignored in-repo status path refused"
else
	rm -f "$tracked"
	bad "non-ignored path" "exit $rc"
fi

if git -C "$REPO_ROOT" check-ignore -q .trusty-mpm/perf/status.json; then
	pass "default .trusty-mpm/perf/status.json is git-ignored"
else
	bad "default status path" "not git-ignored"
fi

if [ "$fail" -ne 0 ]; then
	echo "perf_compare_selftest: FAILED"
	exit 1
fi
echo "perf_compare_selftest: all cases passed"
