#!/usr/bin/env bash
#
# perf_compare.sh — compare two schema-1 perf envelopes and grade the change
# (issue #10210, phase 1).
#
# Why: trusty-memory and trusty-search runs need one shared, deterministic
#   comparison against the pinned baseline, so a weekly run, a selftest and the
#   Architect all read the same verdict. Levels follow the Architect ruling of
#   2026-10-23 on #10210 (Q5): warn at +25% over baseline, alert at +50%.
# What: validates both envelopes (parseable JSON, `schema` 1, a `results`
#   array, same `tool`, same `label` — EVO is compared to EVO only), then pairs
#   result rows by `dimension` + `metric` and compares the baseline row's
#   graded field:
#     - `p95`, where the baseline row's p95 is not null (latency; lower is
#       better);
#     - `value`, for `indexing` rows with a null p95 (`seeded_in`, embed times).
#   Effectiveness rows are not graded here: the recall eval gate is a test
#   failure. delta_pct = (current - baseline) / baseline * 100, rounded to
#   0.01, and the rounded figure decides. Both levels are INCLUSIVE:
#   delta >= 25 is warn, delta >= 50 is alert.
#   Fails closed (#10210 review): a baseline row with no current row is listed
#   under `missing`, and a current row whose field is null or not a number is
#   listed under `unparsed`; either raises the verdict to at least warn. A
#   baseline field that is not a positive finite number, a NaN, infinite or
#   negative current field, or zero graded rows is an error.
#   Prints one JSON report on stdout and one line per graded row plus a
#   verdict line on stderr.
#
# Usage: scripts/perf_compare.sh [--warn-pct N] [--alert-pct N] BASELINE CURRENT
#
# Exit: 0 ok, 10 warn (breach, missing or unparsed row), 11 alert (high),
#   12 error (bad usage, missing or malformed file, wrong schema, tool or label
#   mismatch, zero/negative/NaN/infinite figure, no graded row, jq missing).
#
# Test: scripts/perf_compare_selftest.sh
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux); needs jq 1.6+.

set -euo pipefail

EXIT_OK=0
EXIT_WARN=10
EXIT_ALERT=11
EXIT_ERROR=12

die() {
	printf 'perf_compare: error: %s\n' "$1" >&2
	exit "$EXIT_ERROR"
}

warn_pct=25
alert_pct=50
files=()
while [ "$#" -gt 0 ]; do
	case "$1" in
	--warn-pct)
		[ "$#" -ge 2 ] || die "--warn-pct needs a value"
		warn_pct="$2"
		shift 2
		;;
	--alert-pct)
		[ "$#" -ge 2 ] || die "--alert-pct needs a value"
		alert_pct="$2"
		shift 2
		;;
	-h | --help)
		sed -n '2,30p' "$0"
		exit "$EXIT_OK"
		;;
	-*) die "unknown flag: $1" ;;
	*)
		files+=("$1")
		shift
		;;
	esac
done

[ "${#files[@]}" -eq 2 ] || die "need exactly two envelopes: BASELINE CURRENT"
command -v jq >/dev/null 2>&1 || die "jq not found"
case "$warn_pct$alert_pct" in
*[!0-9.]*) die "--warn-pct and --alert-pct take a non-negative number" ;;
esac

baseline="${files[0]}"
current="${files[1]}"

check_envelope() { # $1 = path, $2 = role
	[ -f "$1" ] || die "$2 envelope not found: $1"
	jq -e . "$1" >/dev/null 2>&1 || die "$2 envelope is not valid JSON: $1"
	jq -e '.schema == 1' "$1" >/dev/null 2>&1 ||
		die "$2 envelope is not schema 1: $1"
	jq -e '(.tool | type) == "string" and (.results | type) == "array"' \
		"$1" >/dev/null 2>&1 ||
		die "$2 envelope lacks a string tool or a results array: $1"
}
check_envelope "$baseline" baseline
check_envelope "$current" current

base_tool="$(jq -r .tool "$baseline")"
cur_tool="$(jq -r .tool "$current")"
[ "$base_tool" = "$cur_tool" ] ||
	die "tool mismatch: baseline $base_tool, current $cur_tool"
base_label="$(jq -r '.label // ""' "$baseline")"
cur_label="$(jq -r '.label // ""' "$current")"
[ "$base_label" = "$cur_label" ] ||
	die "label mismatch: baseline '$base_label', current '$cur_label' (compare like hosts only)"

report="$(
	jq -n --slurpfile b "$baseline" --slurpfile c "$current" \
		--argjson warn "$warn_pct" --argjson alert "$alert_pct" '
	def key: "\(.dimension)|\(.metric)";
	def num: type == "number";
	def finite: num and (isnan | not) and (isinfinite | not);
	def field($row):
		if $row.p95 != null then "p95"
		elif $row.dimension == "indexing" and $row.value != null then "value"
		else null end;
	def grade($d):
		if $d >= $alert then "alert"
		elif $d >= $warn then "warn"
		else "ok" end;
	($b[0]) as $base | ($c[0]) as $cur
	| ($cur.results | map({key: key, value: .}) | from_entries) as $cur_by_key
	# #10210 review: every baseline figure must be a positive finite number.
	| [ $base.results[]
	    | . as $br
	    | field($br) as $f
	    | select($f != null)
	    | $br[$f] as $bv
	    | if ($bv | finite) and $bv > 0 then .
	      else error("baseline \($br.metric) \($f) is \($bv | tojson), not a positive finite number")
	      end
	    | {row: $br, f: $f, cr: $cur_by_key[($br | key)]} ] as $pairs
	| if ($pairs | length) == 0 then error("no graded row in the baseline") else . end
	| [ $pairs[] | select(.cr == null) | .row.metric ] as $missing
	| [ $pairs[] | select(.cr != null and (.cr[.f] | num | not)) | .row.metric ] as $unparsed
	| [ $pairs[]
	    | select(.cr != null and (.cr[.f] | num))
	    | .row[.f] as $bv | .cr[.f] as $cv
	    | if ($cv | finite) and $cv >= 0 then .
	      else error("current \(.row.metric) \(.f) is \($cv | tojson), not a finite non-negative number")
	      end
	    | (((($cv - $bv) / $bv * 100) * 100 | round) / 100) as $d
	    | {dimension: .row.dimension, metric: .row.metric, field: .f,
	       unit: .row.unit, baseline: $bv, current: $cv, delta_pct: $d,
	       severity: grade($d)} ] as $rows
	| if ($rows | length) == 0 then error("no baseline row could be graded against the current envelope") else . end
	| ([ $rows[].severity ]) as $sev
	| {schema: 1, kind: "perf_compare", tool: $base.tool, label: $base.label,
	   baseline: ($base | {date, commit, version}),
	   current: ($cur | {date, commit, version}),
	   warn_pct: $warn, alert_pct: $alert,
	   # A missing or unparsed row raises the verdict to warn; alert outranks it.
	   verdict: (if ($sev | index("alert")) then "alert"
	             elif ($sev | index("warn")) or ($missing | length) > 0
	                  or ($unparsed | length) > 0 then "warn"
	             else "ok" end),
	   rows: $rows, missing: $missing, unparsed: $unparsed}'
)" || die "comparison failed"

printf '%s\n' "$report"
printf '%s\n' "$report" | jq -r '
	(.rows[] | "\(.severity)\t\(.metric) \(.field)\t\(.baseline) -> \(.current)\t\(.delta_pct)%"),
	(.missing[] | "missing\t\(.)"),
	(.unparsed[] | "unparsed\t\(.)"),
	"verdict\t\(.verdict)"' | sed 's/^/perf_compare: /' >&2

case "$(printf '%s\n' "$report" | jq -r .verdict)" in
ok) exit "$EXIT_OK" ;;
warn) exit "$EXIT_WARN" ;;
alert) exit "$EXIT_ALERT" ;;
*) die "unexpected verdict" ;;
esac
