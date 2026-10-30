#!/usr/bin/env bash
#
# perf_status_write.sh — record the latest perf run per tool in the Architect
# status file (issue #10210, phase 1).
#
# Why: the Architect learns of a warn, alert, failed or skipped perf run from
#   one file its poller reads (ruling of 2026-10-23 on #10210, Q1): the
#   gitignored `.trusty-mpm/perf/status.json`, one entry per tool. The file is
#   local state and must never be committed.
# What: builds one record and replaces the tool's entry in the status file,
#   keeping every other tool's entry. Record fields: `ts` (UTC), `verdict`
#   (ok | warn | alert | failed | skipped), `job`, `reason`, `envelope`,
#   `skipped_streak` (consecutive skipped runs, 0 after any other verdict; the
#   reader warns at 1 and alerts at 2, ruling Q5), and, from a perf_compare.sh
#   report, `commit`, `label`, `baseline` {date, commit} and `metrics` (the
#   graded rows, each with its baseline delta).
#   The write is atomic: a temp file in the same directory, then `mv`.
#   It refuses a status-file path inside a git work tree that git does not
#   ignore, so the file cannot be staged by accident.
#
# Usage: scripts/perf_status_write.sh --tool NAME
#          (--compare REPORT | --verdict ok|warn|alert|failed|skipped)
#          [--reason TEXT] [--job NAME] [--envelope PATH] [--status-file PATH]
#   --compare REPORT   a perf_compare.sh stdout report; its verdict is used
#   --verdict V        verdict for a run with no report; failed and skipped
#                      need --reason
#   --status-file PATH default: env PERF_STATUS_FILE, else
#                      <repo>/.trusty-mpm/perf/status.json
#
# Exit: 0 written, 2 bad usage, refused path or write failure.
#
# Test: scripts/perf_compare_selftest.sh
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux); needs jq 1.6+.

set -euo pipefail

die() {
	printf 'perf_status_write: error: %s\n' "$1" >&2
	exit 2
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

tool=""
verdict=""
compare=""
reason=""
job="weekly"
envelope=""
status_file="${PERF_STATUS_FILE:-$REPO_ROOT/.trusty-mpm/perf/status.json}"

while [ "$#" -gt 0 ]; do
	case "$1" in
	--tool | --verdict | --compare | --reason | --job | --envelope | --status-file)
		[ "$#" -ge 2 ] || die "$1 needs a value"
		case "$1" in
		--tool) tool="$2" ;;
		--verdict) verdict="$2" ;;
		--compare) compare="$2" ;;
		--reason) reason="$2" ;;
		--job) job="$2" ;;
		--envelope) envelope="$2" ;;
		--status-file) status_file="$2" ;;
		esac
		shift 2
		;;
	-h | --help)
		sed -n '2,36p' "$0"
		exit 0
		;;
	*) die "unknown argument: $1" ;;
	esac
done

command -v jq >/dev/null 2>&1 || die "jq not found"
[ -n "$tool" ] || die "--tool is required"
if [ -n "$compare" ] && [ -n "$verdict" ]; then
	die "--compare and --verdict are exclusive"
fi
if [ -n "$compare" ]; then
	[ -f "$compare" ] || die "compare report not found: $compare"
	jq -e '.kind == "perf_compare" and (.verdict | type) == "string"' \
		"$compare" >/dev/null 2>&1 || die "not a perf_compare report: $compare"
	report_tool="$(jq -r .tool "$compare")"
	[ "$report_tool" = "$tool" ] ||
		die "report is for $report_tool, not $tool"
	verdict="$(jq -r .verdict "$compare")"
fi
case "$verdict" in
ok | warn | alert) ;;
failed | skipped) [ -n "$reason" ] || die "verdict $verdict needs --reason" ;;
"") die "one of --compare or --verdict is required" ;;
*) die "unknown verdict: $verdict" ;;
esac

status_dir="$(dirname "$status_file")"
mkdir -p "$status_dir" || die "cannot create $status_dir"
# A status file inside a work tree must be git-ignored, or it could be staged.
if git -C "$status_dir" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
	git -C "$status_dir" check-ignore -q "$status_file" ||
		die "refusing $status_file: inside a git work tree and not ignored"
fi

previous='{}'
if [ -s "$status_file" ]; then
	previous="$(jq -c . "$status_file" 2>/dev/null)" ||
		die "existing status file is not valid JSON: $status_file"
fi
report='null'
[ -z "$compare" ] || report="$(jq -c . "$compare")"

tmp="$(mktemp "$status_dir/.status.json.XXXXXX")" || die "mktemp failed"
trap 'rm -f "$tmp"' EXIT
jq -n --argjson prev "$previous" --argjson report "$report" \
	--arg tool "$tool" --arg verdict "$verdict" --arg reason "$reason" \
	--arg job "$job" --arg envelope "$envelope" \
	--arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
	($prev.tools[$tool] // {}) as $old
	| {ts: $ts, verdict: $verdict, job: $job,
	   reason: (if $reason == "" then null else $reason end),
	   envelope: (if $envelope == "" then null else $envelope end),
	   skipped_streak: (if $verdict == "skipped"
	                    then (if $old.verdict == "skipped"
	                          then ($old.skipped_streak // 0) + 1 else 1 end)
	                    else 0 end)}
	+ (if $report == null then {}
	   else {commit: $report.current.commit, label: $report.label,
	         baseline: $report.baseline | {date, commit},
	         metrics: $report.rows, missing: $report.missing} end)
	| . as $record
	| {schema: 1, tools: (($prev.tools // {}) + {($tool): $record})}' \
	>"$tmp" || die "could not build the status record"
mv "$tmp" "$status_file" || die "could not write $status_file"
trap - EXIT
printf 'perf_status_write: %s %s -> %s\n' "$tool" "$verdict" "$status_file" >&2
