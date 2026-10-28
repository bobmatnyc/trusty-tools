#!/usr/bin/env bash
#
# perf_weekly_memory.sh — weekly EVO perf run for trusty-memory (issue #10210,
# phase 2).
#
# Why: the Architect needs one weekly verdict on trusty-memory speed and
#   indexing, taken the same way each week and graded by the shared phase 1
#   compare (scripts/perf_compare.sh). A run that fails, times out, prints no
#   figure or ran on a busy host must never read as ok (#10210 review).
# What: runs four suites on EVO through mpm-utils `evo-gate-log` + `evo`, each
#   run its own gate, in this order:
#     eval        recall eval, debug, --include-ignored         (x1, ruling Q4)
#     profile     recall_supersession_latency_profile, release  (x5)
#     concurrent  concurrent_perf, release, --include-ignored   (x3)
#     slice       slice embed measure, release, --ignored       (x3)
#   Before each run it reads the EVO 1-minute load. A load of 2 or more that
#   does not settle below 2 within --settle-s, or an unreachable EVO, ends the
#   whole run as `skipped`: no further suite runs and nothing is compared. The
#   load after each run is recorded, not gated: the run's own cargo job raises
#   it. A run with a non-zero exit, a timeout, or a missing or malformed figure
#   ends the whole run as `failed`. A timed-out run also stops what it left on
#   EVO (scripts/perf_weekly_evo_reap.sh: its job scope and any queued waiter)
#   and names in the reason what it could not stop. A host probe that fails on
#   a reachable EVO is `failed`, on an unreachable one `skipped`. A TERM or
#   INT stops the running suite's local processes, stops its EVO job the same
#   way as a timeout, and ends the run as `failed`. It runs from the repo
#   root, so `evo` keys this worktree wherever it is started; relative path
#   arguments mean the caller's directory. Otherwise it
#   parses each run's log, takes
#   the median over runs, writes a schema-1 envelope, runs perf_compare.sh
#   against --baseline and records the verdict with perf_status_write.sh.
#   Every output file goes to the gitignored --out-dir.
#
# Usage: scripts/perf_weekly_memory.sh [--baseline FILE] [--out-dir DIR]
#          [--status-file PATH] [--runs-eval N] [--runs-profile N]
#          [--runs-concurrent N] [--runs-slice N] [--suite-timeout S]
#          [--settle-s S] [--toolchain V]
#   Run it from a committed, clean linked worktree (evo refuses a dirty tree).
#   --baseline     default docs/trusty-memory/perf/2026-10-17-0.36.0-evo.json
#   --out-dir      default .trusty-mpm/perf/memory/weekly/<UTC stamp>
#   --status-file  passed to perf_status_write.sh (its default otherwise)
#   --suite-timeout  seconds per run (default 2400); --settle-s (default 300)
#   Test hooks (stubs, used by scripts/perf_weekly_memory_selftest.sh):
#   PERF_RUN_CMD   run as `$PERF_RUN_CMD <gate> cargo <args>` instead of evo
#   PERF_LOAD_CMD  prints the 1-minute load; non-zero exit = EVO unreachable
#   PERF_HOST_CMD  prints 5 lines: OS, CPU model, vCPUs, RAM GB, rustc -V
#   PERF_REAP_CMD  run as `$PERF_REAP_CMD KEY MAX_AGE SHA -- ARGS` instead of
#                  the reap script over ssh; PERF_EVO_KEY sets KEY (else stub)
#
# Exit: 0 ok, 10 warn, 11 alert (perf_compare.sh verdict), 20 failed,
#   30 skipped, 2 bad usage or missing tool. Every exit but 2 writes status.
#
# Test: scripts/perf_weekly_memory_selftest.sh
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux); needs jq, awk, git and
#   timeout (GNU coreutils; gtimeout is accepted).

if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
MPM_UTILS="${MPM_UTILS_SCRIPTS:-/Users/masa/trusty-mpm-projects/bobmatnyc/mpm-utils/scripts}"
EVO_SSH=(ssh -o BatchMode=yes -o ConnectTimeout=10 evo-linux)
TOOL="trusty-memory"
LOAD_MAX=2
SLICE_TEST="engine::retrieval::chunk_backfill_measure_tests::slice_defaults_and_recall_contention_measured"

usage() {
	printf 'perf_weekly_memory: error: %s\n' "$1" >&2
	exit 2
}

REAP_SLACK_S=120 # #10210 critic: added to the run's age when reaping EVO

baseline="$REPO_ROOT/docs/trusty-memory/perf/2026-10-17-0.36.0-evo.json"
out_dir=""
status_file=""
runs_eval=1 runs_profile=5 runs_concurrent=3 runs_slice=3
suite_timeout=2400 settle_s=300 settle_step=20 toolchain="1.99.0"
while [ "$#" -gt 0 ]; do
	[ "$#" -ge 2 ] || usage "$1 needs a value"
	case "$1" in
	--baseline) baseline="$2" ;;
	--out-dir) out_dir="$2" ;;
	--status-file) status_file="$2" ;;
	--runs-eval) runs_eval="$2" ;;
	--runs-profile) runs_profile="$2" ;;
	--runs-concurrent) runs_concurrent="$2" ;;
	--runs-slice) runs_slice="$2" ;;
	--suite-timeout) suite_timeout="$2" ;;
	--settle-s) settle_s="$2" ;;
	--toolchain) toolchain="$2" ;;
	*) usage "unknown argument: $1" ;;
	esac
	shift 2
done
for n in "$runs_eval" "$runs_profile" "$runs_concurrent" "$runs_slice" "$suite_timeout" "$settle_s"; do
	case "$n" in '' | *[!0-9]*) usage "counts and seconds must be whole numbers: '$n'" ;; esac
done
for n in "$runs_eval" "$runs_profile" "$runs_concurrent" "$runs_slice"; do
	[ "$n" -ge 1 ] || usage "every suite needs at least one run"
done
for t in jq awk git; do command -v "$t" >/dev/null 2>&1 || usage "$t not found"; done
TIMEOUT="$(command -v timeout || command -v gtimeout || true)"
[ -n "$TIMEOUT" ] || usage "timeout (GNU coreutils) not found"
# #10210 critic: `evo` keys the cwd's worktree, so run from REPO_ROOT; a
# relative path argument still names a path under the caller's cwd.
abs_path() { case "$1" in '' | /*) printf '%s' "$1" ;; *) printf '%s/%s' "$PWD" "$1" ;; esac; }
baseline="$(abs_path "$baseline")"
out_dir="$(abs_path "$out_dir")"
status_args=()
[ -z "$status_file" ] || status_args=(--status-file "$(abs_path "$status_file")")
# The same holds for path env inputs: perf_status_write.sh reads
# PERF_STATUS_FILE, this script MPM_UTILS_SCRIPTS (as MPM_UTILS).
if [ -n "${PERF_STATUS_FILE:-}" ]; then
	PERF_STATUS_FILE="$(abs_path "$PERF_STATUS_FILE")"
	export PERF_STATUS_FILE
fi
MPM_UTILS="$(abs_path "$MPM_UTILS")"
cd "$REPO_ROOT" || usage "cannot cd to $REPO_ROOT"
[ -f "$baseline" ] || usage "baseline not found: $baseline"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
[ -n "$out_dir" ] || out_dir="$REPO_ROOT/.trusty-mpm/perf/memory/weekly/$stamp"
mkdir -p "$out_dir" || usage "cannot create $out_dir"
out_dir="$(cd "$out_dir" && pwd)"
# Output inside a work tree must be git-ignored, or a run could be committed.
if git -C "$out_dir" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
	git -C "$out_dir" check-ignore -q "$out_dir/envelope.json" ||
		usage "refusing $out_dir: inside a git work tree and not ignored"
fi
envelope="$out_dir/envelope.json"
loads="$out_dir/load.tsv"
: >"$loads"

log() { printf 'perf_weekly_memory: %s\n' "$*" | tee -a "$out_dir/runner.log" >&2; }

# Record a run that produced no comparison, then exit.
finish() { # $1 = failed|skipped, $2 = reason
	trap '' TERM INT # a late signal must not cut the status write short
	local rc=20
	[ "$1" = skipped ] && rc=30
	log "$1: $2"
	"$SCRIPT_DIR/perf_status_write.sh" --tool "$TOOL" --job weekly --verdict "$1" \
		--reason "$2" --envelope "$out_dir" ${status_args[@]+"${status_args[@]}"} ||
		log "status write failed"
	exit "$rc"
}

# #10210 critic: a TERM/INT must not leave the suite's processes running or
# last week's status in place. The suite runs as a background job, so `wait`
# returns at once on a signal; timeout(1) passes the TERM to its process
# group and its -k 30 bounds the wait here.
block="" in_flight="" t0=0
# shellcheck disable=SC2329 # invoked by the traps below
on_signal() { # $1 = TERM|INT
	trap '' TERM INT
	local why="interrupted by SIG$1${block:+ during $block}" jobs_left
	jobs_left="$(jobs -p)"
	if [ -n "$jobs_left" ]; then
		# shellcheck disable=SC2086 # one pid per word
		kill -TERM $jobs_left 2>/dev/null || true
		wait || true
	fi
	[ -z "$in_flight" ] || reap_and_fail "$why" $((SECONDS - t0))
	finish failed "$why"
}
trap 'on_signal TERM' TERM
trap 'on_signal INT' INT

read_load() {
	if [ -n "${PERF_LOAD_CMD:-}" ]; then
		$PERF_LOAD_CMD
	else
		"${EVO_SSH[@]}" 'cat /proc/loadavg'
	fi | awk 'NR == 1 && $1 ~ /^[0-9]+(\.[0-9]+)?$/ {print $1; ok = 1} END {exit !ok}'
}

# Wait up to --settle-s for a load under 2; end the run as skipped otherwise.
gate_load() { # $1 = block name
	local waited=0 load
	while :; do
		load="$(read_load)" || finish skipped "EVO unreachable: no load reading before $1"
		printf '%s\tbefore\t%s\t%s\n' "$1" "$(date -u +%H:%M:%SZ)" "$load" >>"$loads"
		awk -v l="$load" -v m="$LOAD_MAX" 'BEGIN {exit !(l < m)}' && return 0
		[ "$waited" -lt "$settle_s" ] ||
			finish skipped "EVO 1-minute load $load >= $LOAD_MAX before $1 (waited ${waited}s)"
		sleep "$settle_step"
		waited=$((waited + settle_step))
	done
}

if [ -z "${PERF_RUN_CMD:-}" ]; then
	[ -z "$(git -C "$REPO_ROOT" status --porcelain)" ] ||
		finish failed "worktree is dirty; evo-gate-log needs a committed clean tree"
	[ -x "$MPM_UTILS/evo-gate-log" ] || finish failed "evo-gate-log not found in $MPM_UTILS"
	evo_key="$(cd "$REPO_ROOT" && "$MPM_UTILS/evo" key)" || finish failed "evo key failed in $REPO_ROOT"
else
	evo_key="${PERF_EVO_KEY:-stub}"
fi
commit="$(git -C "$REPO_ROOT" rev-parse HEAD)"

# #10210 critic: a timeout kills only the Mac side; stop this run's EVO job.
# Prints one line per action; non-zero when something may be left.
reap_evo() { # $1 = seconds since the run started; rest = cargo argv
	local age=$(($1 + REAP_SLACK_S))
	shift
	if [ -n "${PERF_REAP_CMD:-}" ]; then
		$PERF_REAP_CMD "$evo_key" "$age" "$commit" -- "$@"
	elif [ -n "${PERF_RUN_CMD:-}" ]; then
		echo "stubbed run and no PERF_REAP_CMD: nothing reaped"
		return 1
	else
		"$TIMEOUT" -k 10 120 "${EVO_SSH[@]}" "bash -s -- $(printf '%q ' "$evo_key" "$age" "$commit" -- "$@")" \
			<"$SCRIPT_DIR/perf_weekly_evo_reap.sh"
	fi
}

# Stop the suite's EVO job, then end the run as failed. The reason gets the
# reap's lines and, when something may be left, the command to check by hand.
reap_and_fail() { # $1 = reason, $2 = seconds since the run started; uses args
	trap '' TERM INT # the cleanup runs to its end; the run is failed already
	local reap_rc=0 reap_out
	reap_out="$(reap_evo "$2" "+$toolchain" "${args[@]}" 2>&1)" || reap_rc=$?
	reap_out="$(printf '%s\n' "$reap_out" | awk 'NF {printf "%s%s", sep, $0; sep = "; "}')"
	[ "$reap_rc" -eq 0 ] || reap_out="${reap_out:+$reap_out; }EVO cleanup exited $reap_rc; check by hand: ssh evo-linux \"systemctl --user list-units 'evo-job-*'; pgrep -af 'evo-run --key $evo_key'\""
	finish failed "$1; EVO cleanup: ${reap_out:-no output}"
}

# The command that runs one gate, as an array: the stub or evo-gate-log + evo.
run_cmd() { # $1 = gate name; sets cmd=(...)
	if [ -n "${PERF_RUN_CMD:-}" ]; then
		# shellcheck disable=SC2206 # a stub command line, split on purpose
		cmd=($PERF_RUN_CMD "$1")
	else
		cmd=("$MPM_UTILS/evo-gate-log" --tag perf-weekly --gate "$1" "$MPM_UTILS/evo")
	fi
	cmd+=(cargo "+$toolchain")
}

suite_args() { # $1 = suite; prints one cargo argument per line
	case "$1" in
	eval) printf '%s\n' test -p trusty-memory --test recall_eval -- --include-ignored --nocapture ;;
	profile) printf '%s\n' test -p trusty-memory --release --config \
		'env.TRUSTY_RECALL_SUPERSESSION_PROFILE="1"' --test recall_supersession \
		recall_supersession_latency_profile -- --nocapture ;;
	concurrent) printf '%s\n' test -p trusty-memory --release --test concurrent_perf \
		-- --include-ignored --nocapture ;;
	slice) printf '%s\n' test -p trusty-memory --release --lib -- --ignored --exact \
		"$SLICE_TEST" --nocapture ;;
	esac
}

# Duration Debug text (12.5ms, 1.2s, 830µs, 90ns) to the unit in $1, in awk.
AWK_DUR='BEGIN { CONVFMT = "%.10g"; OFMT = "%.10g" }
function dur(t, unit,   n, u, f) {
	if (!match(t, /^[0-9]+(\.[0-9]+)?/)) return "x"
	n = substr(t, 1, RLENGTH) + 0; u = substr(t, RLENGTH + 1)
	if (u == "s") f = 1e3; else if (u == "ms") f = 1; else if (u == "µs" || u == "us") f = 1e-3
	else if (u == "ns") f = 1e-6; else return "x"
	return (unit == "s") ? n * f / 1e3 : n * f
}'

# Parse one run log into one JSON object; non-zero exit when a figure is
# missing or malformed. Every key below must be present for the run to count.
parse_run() { # $1 = suite, $2 = log
	case "$1" in
	eval)
		local lines
		lines="$(awk 'index($0, "recall-eval-summary {") {print substr($0, index($0, "{"))}' "$2" |
			jq -c 'select(.mode == "mechanisms on")' 2>/dev/null)" || return 1
		[ "$(printf '%s' "$lines" | grep -c .)" = 1 ] || return 1
		printf '%s\n' "$lines" | jq -ce '
			if ([.n, .k, .hit_at_1, .hit_at_3, .hit_at_5, .mrr, .recall_at_k,
			     .superseded_above_current, .superseded_groups] | all(type == "number"))
			then . else error("eval summary lacks a figure") end'
		;;
	profile)
		awk "$AWK_DUR"'
		/seeded_in=/ && /leg off = / {
			c++
			match($0, /seeded_in=[^ ]+/); s = dur(substr($0, RSTART + 10, RLENGTH - 10), "s")
			match($0, /leg off = [0-9]+\/[0-9]+/); split(substr($0, RSTART + 10, RLENGTH - 10), off, "/")
			match($0, /leg on = [0-9]+\/[0-9]+/); split(substr($0, RSTART + 9, RLENGTH - 9), on, "/")
		}
		END {
			if (c != 1 || s == "x" || off[2] == "" || on[2] == "") exit 1
			printf "{\"seeded_in\":%s,\"off_p50\":%s,\"off_p95\":%s,\"on_p50\":%s,\"on_p95\":%s}\n", s, off[1], off[2], on[1], on[2]
		}' "$2"
		;;
	concurrent)
		# Blocks start at "=== test_x" and own the two-space lines after them.
		# A field seen twice (interleaved output) or missing fails the run.
		awk "$AWK_DUR"'
		function put(k, v) { if (v == "x" || v == "") bad = 1; if ((t, k) in f) bad = 1; f[t, k] = v }
		function stats(pre, line,   i, n, a, kv) {
			gsub(/= +/, "=", line); n = split(line, a, /[ \t]+/)
			for (i = 1; i <= n; i++) { split(a[i], kv, "=")
				if (kv[1] == "p50" || kv[1] == "p95" || kv[1] == "max") put(pre kv[1], dur(kv[2], "ms")) }
		}
		match($0, /=== test_[a-z_]+/) { t = substr($0, RSTART + 4, RLENGTH - 4); next }
		t != "" && /^  / {
			if (match($0, /throughput=[0-9.]+/)) put("ops", substr($0, RSTART + 11, RLENGTH - 11))
			if ($0 ~ /^  latency: /) stats("", $0)
			if ($0 ~ /^  WRITE: /) stats("w_", $0)
			if ($0 ~ /^  READ : /) stats("r_", $0)
		}
		END {
			need["test_concurrent_reads"] = "ops p50 p95 max"
			need["test_sustained_load"] = "ops"
			need["test_concurrent_rw"] = "ops w_p50 w_p95 w_max r_p50 r_p95 r_max"
			need["test_burst"] = "ops p50 p95 max"
			out = ""
			for (t in need) { n = split(need[t], ks, " "); obj = ""
				for (i = 1; i <= n; i++) { if (!((t, ks[i]) in f)) bad = 1
					obj = obj (i > 1 ? "," : "") "\"" ks[i] "\":" f[t, ks[i]] }
				out = out (out == "" ? "" : ",") "\"" t "\":{" obj "}" }
			if (bad) exit 1
			print "{" out "}"
		}' "$2"
		;;
	slice)
		awk "$AWK_DUR"'
		function q(ph, line,   i, n, a, kv) {
			gsub(/= +/, "=", line); n = split(line, a, /[ \t]+/)
			for (i = 1; i <= n; i++) { split(a[i], kv, "=")
				if (kv[1] == "p50" || kv[1] == "p95" || kv[1] == "max") { v = dur(kv[2], "ms"); if (v == "x") bad = 1; f[ph kv[1]] = v } }
			seen[ph]++
		}
		/MEASURE embed_64_chunks runs=\[/ {
			c++
			match($0, /runs=\[[^]]*\]/); n = split(substr($0, RSTART + 6, RLENGTH - 7), r, /, */)
			if (n != 3) bad = 1
			for (i = 1; i <= n; i++) { b[i] = dur(r[i], "ms"); if (b[i] == "x") bad = 1 }
			match($0, /per_chunk=[^ ]+/); pc = dur(substr($0, RSTART + 10, RLENGTH - 10), "ms")
			if (pc == "x") bad = 1
		}
		/MEASURE query_embed phase=idle / { q("i_", $0) }
		/MEASURE query_embed phase=contention / { q("c_", $0) }
		END {
			if (bad || c != 1 || seen["i_"] != 1 || seen["c_"] != 1) exit 1
			# median of three batches
			lo = b[1]; hi = b[1]; for (i = 2; i <= 3; i++) { if (b[i] < lo) lo = b[i]; if (b[i] > hi) hi = b[i] }
			med = b[1] + b[2] + b[3] - lo - hi
			printf "{\"per_chunk\":%s,\"batch\":%s,\"i_p50\":%s,\"i_p95\":%s,\"i_max\":%s,\"c_p50\":%s,\"c_p95\":%s,\"c_max\":%s}\n", pc, med, f["i_p50"], f["i_p95"], f["i_max"], f["c_p50"], f["c_p95"], f["c_max"]
		}' "$2"
		;;
	esac
}

host_probe() {
	if [ -n "${PERF_HOST_CMD:-}" ]; then
		$PERF_HOST_CMD
	else
		"${EVO_SSH[@]}" ". /etc/os-release && echo \"\$PRETTY_NAME\" &&
			lscpu | sed -n 's/^Model name: *//p' | head -n 1 && nproc &&
			awk '/MemTotal/ {print int(\$2 / 1048576 + 0.5)}' /proc/meminfo &&
			\$HOME/.cargo/bin/rustc +$toolchain -V"
	fi
}
# #10210 critic: only an unreachable EVO is skipped; a probe that fails on a
# reachable one (a missing toolchain) is a failure of this run.
host_rc=0
host_txt="$(host_probe 2>"$out_dir/host.err")" || host_rc=$?
if [ "$host_rc" -ne 0 ]; then
	host_why="$(grep -m 1 . "$out_dir/host.err" || true)"
	read_load >/dev/null 2>&1 ||
		finish skipped "EVO unreachable: host probe exited $host_rc and no load reading${host_why:+ ($host_why)}"
	finish failed "host probe exited $host_rc on a reachable EVO: ${host_why:-no stderr}"
fi
[ "$(printf '%s\n' "$host_txt" | grep -c .)" -ge 5 ] || finish failed "host probe printed fewer than 5 lines"

runs_json="$out_dir/runs.jsonl"
: >"$runs_json"
cmd_lines=""
for suite in eval profile concurrent slice; do
	case "$suite" in
	eval) count="$runs_eval" ;; profile) count="$runs_profile" ;;
	concurrent) count="$runs_concurrent" ;; slice) count="$runs_slice" ;;
	esac
	args=()
	while IFS= read -r a; do args+=("$a"); done <<EOF
$(suite_args "$suite")
EOF
	cmd_lines="$cmd_lines$suite: cargo +$toolchain ${args[*]}
"
	k=1
	while [ "$k" -le "$count" ]; do
		block="$suite$k"
		gate_load "$block"
		run_log="$out_dir/$block.log"
		log "run $block ($k/$count)"
		rc=0
		run_cmd "perf-$suite"
		t0=$SECONDS
		in_flight=1
		"$TIMEOUT" -k 30 "$suite_timeout" "${cmd[@]}" "${args[@]}" >"$run_log" 2>&1 </dev/null &
		wait "$!" || rc=$?
		in_flight=""
		took=$((SECONDS - t0))
		after="$(read_load 2>/dev/null || echo unreachable)"
		printf '%s\tafter\t%s\t%s\n' "$block" "$(date -u +%H:%M:%SZ)" "$after" >>"$loads"
		# 137 after the deadline is the -k SIGKILL; earlier it is EVO's own rc.
		if [ "$rc" -eq 124 ] || { [ "$rc" -eq 137 ] && [ "$took" -ge "$suite_timeout" ]; }; then
			reap_and_fail "$block timed out after ${suite_timeout}s (log $run_log)" "$took"
		fi
		[ "$rc" -eq 0 ] || finish failed "$block exited $rc (log $run_log)"
		parsed="$(parse_run "$suite" "$run_log")" ||
			finish failed "$block exited 0 but printed no parseable figure (log $run_log)"
		printf '%s\n' "$parsed" | jq -c --arg s "$suite" '{suite: $s, run: .}' >>"$runs_json" ||
			finish failed "$block figure is not JSON"
		k=$((k + 1))
	done
done
block=""

# Assemble the envelope: medians over runs. A suite with fewer parsed runs
# than it ran is a failure, never a shorter median.
os="$(sed -n 1p <<<"$host_txt")"
cpu="$(sed -n 2p <<<"$host_txt")"
vcpu="$(sed -n 3p <<<"$host_txt")"
ram="$(sed -n 4p <<<"$host_txt")"
rustc_v="$(sed -n 5p <<<"$host_txt")"
case "$ram" in '' | *[!0-9]*) finish failed "host probe RAM is not a number: '$ram'" ;; esac
version="$(awk -F'"' '/^version *=/ {print $2; exit}' "$REPO_ROOT/crates/trusty-memory/Cargo.toml")"
jq -s --arg commit "$commit" --arg version "$version" --arg date "$(date -u +%Y-%m-%d)" \
	--arg os "$os" --arg cpu "$cpu ($vcpu vCPU)" --argjson ram "$ram" \
	--arg toolchain "+$toolchain, $rustc_v" --arg cmds "$cmd_lines" \
	--rawfile loads "$loads" \
	--argjson want "{\"eval\":$runs_eval,\"profile\":$runs_profile,\"concurrent\":$runs_concurrent,\"slice\":$runs_slice}" '
	def med: sort | length as $n
		| if $n == 0 then error("no runs")
		  elif $n % 2 == 1 then .[($n - 1) / 2]
		  else (.[$n / 2 - 1] + .[$n / 2]) / 2 end;
	def r3: . * 1000 | round / 1000;
	def runs($s): [.[] | select(.suite == $s) | .run]
		| if length != $want[$s] then error("\($s): \(length) parsed runs, \($want[$s]) ran") else . end;
	def row($dim; $metric; $unit; $n; $p50; $p95; $max; $value; $note):
		{dimension: $dim, metric: $metric, unit: $unit, n: $n, k: null,
		 p50: $p50, p95: $p95, max: $max, value: $value,
		 threshold: (if $dim == "effectiveness" then "eval gate (test)"
		             else "warn +25%, alert +50% over baseline" end),
		 notes: "\($note) Fresh temp dirs, access counts start at zero. Toolchain \($toolchain)."};
	def lat($rs; $pre; $metric; $unit; $n; $note):
		row("speed"; $metric; $unit; $n; ([$rs[][$pre + "p50"]] | med | r3);
		    ([$rs[][$pre + "p95"]] | med | r3); ([$rs[][$pre + "max"]] | med | r3); null;
		    "\($note) p50, p95 and max are medians over \($rs | length) runs; p95 per run: \([$rs[][$pre + "p95"] | r3]).");
	. as $all
	| ($all | runs("eval")) as $ev | ($ev[0]) as $e
	| ($all | runs("profile")) as $pr
	| ($all | runs("concurrent")) as $co
	| ($all | runs("slice")) as $sl
	| (if ($ev | map(del(.group_ranks)) | unique | length) != 1
	   then error("eval runs disagree; ranks must be deterministic") else . end)
	| ($co | map(.test_concurrent_reads)) as $cr | ($co | map(.test_concurrent_rw)) as $rw
	| ($co | map(.test_burst)) as $cb | ($co | map(.test_sustained_load)) as $cs
	| {schema: 1, tool: "trusty-memory", version: $version, commit: $commit, label: "evo",
	   host: {id: "EVO-X3", os: $os, cpu: $cpu, ram_gb: $ram}, date: $date,
	   corpus: "recall_eval \($e.n) queries/\($e.superseded_groups) groups; supersession profile 20000 drawers; slice measure; concurrent_perf temp daemons",
	   load: {before: ($loads | split("\n") | map(split("\t") | select(length == 4 and .[1] == "before") | "\(.[0])=\(.[3]) at \(.[2])") | join("; ")),
	          after: ($loads | split("\n") | map(split("\t") | select(length == 4 and .[1] == "after") | "\(.[0])=\(.[3]) at \(.[2])") | join("; ")),
	          notes: "1-minute load per block (name, phase, UTC, load). before is gated at < 2; after is recorded only, since the block'"'"'s own cargo job raises it."},
	   residency: {cached: null, total: null, notes: "n/a: in-process suites, no live daemon"},
	   notes: "perf_weekly_memory.sh. Toolchain \($toolchain). Commands:\n\($cmds)",
	   results: [
	     row("effectiveness"; "hit@1"; "queries/\($e.n)"; $e.n; null; null; null; $e.hit_at_1; "Mechanisms on."),
	     row("effectiveness"; "superseded_above_current"; "groups/\($e.superseded_groups)"; $e.superseded_groups; null; null; null; $e.superseded_above_current; "Mechanisms on."),
	     row("effectiveness"; "MRR"; "ratio"; $e.n; null; null; null; $e.mrr; "From recall-eval-summary."),
	     row("effectiveness"; "hit@3"; "queries/\($e.n)"; $e.n; null; null; null; $e.hit_at_3; ""),
	     row("effectiveness"; "hit@5"; "queries/\($e.n)"; $e.n; null; null; null; $e.hit_at_5; ""),
	     (row("effectiveness"; "recall@k"; "ratio"; $e.n; null; null; null; $e.recall_at_k; "k=\($e.k).") | .k = $e.k),
	     ([$pr[].off_p95] | med) as $p | row("speed"; "memory_recall_latency_rulings-off"; "us"; 300; ([$pr[].off_p50] | med); $p; ([$pr[].off_p95] | max); $p; "Release, 20000 drawers, 300 queries. p95 per run: \([$pr[].off_p95]); max is the max of the p95s."),
	     ([$pr[].on_p95] | med) as $p | row("speed"; "memory_recall_latency_rulings-on"; "us"; 300; ([$pr[].on_p50] | med); $p; ([$pr[].on_p95] | max); $p; "Release, 20000 drawers, 300 queries. p95 per run: \([$pr[].on_p95]); max is the max of the p95s."),
	     row("indexing"; "seeded_in"; "s"; ($pr | length); null; null; ([$pr[].seeded_in] | max | r3); ([$pr[].seeded_in] | med | . * 10 | round / 10); "Seed time for 20000 drawers, median; per run: \([$pr[].seeded_in | . * 10 | round / 10])."),
	     (lat($cr; ""; "concurrent_perf test_concurrent_reads"; "ms (latency); ops/s (value)"; ($co | length); "concurrent_perf.") | .value = ([$cr[].ops] | med)),
	     row("speed"; "concurrent_perf test_sustained_load"; "ms (latency); ops/s (value)"; ($co | length); null; null; null; ([$cs[].ops] | med); "ops/s per run: \([$cs[].ops])."),
	     row("speed"; "concurrent_perf test_concurrent_rw (total)"; "ms (latency); ops/s (value)"; ($co | length); null; null; null; ([$rw[].ops] | med); "ops/s per run: \([$rw[].ops])."),
	     lat($rw; "w_"; "concurrent_perf test_concurrent_rw WRITE"; "ms (latency); ops/s (value)"; ($co | length); "concurrent_perf."),
	     lat($rw; "r_"; "concurrent_perf test_concurrent_rw READ"; "ms (latency); ops/s (value)"; ($co | length); "concurrent_perf."),
	     (lat($cb; ""; "concurrent_perf test_burst"; "ms (latency); ops/s (value)"; ($co | length); "concurrent_perf.") | .value = ([$cb[].ops] | med)),
	     row("indexing"; "embed_per_chunk"; "ms"; ($sl | length); null; null; null; ([$sl[].per_chunk] | med | r3); "Per run: \([$sl[].per_chunk | r3])."),
	     row("indexing"; "embed_64_chunk_batch"; "ms"; ($sl | length); null; null; null; ([$sl[].batch] | med | r3); "Median of each run'"'"'s 3 batches; per run: \([$sl[].batch | r3])."),
	     lat($sl; "i_"; "query_embed_idle"; "ms"; ($sl | length) * 200; "Slice measure, idle."),
	     lat($sl; "c_"; "query_embed_under_slice"; "ms"; ($sl | length) * 200; "Slice measure, beside a paced 64-chunk embed.")
	   ]}' "$runs_json" >"$envelope.tmp" 2>"$out_dir/envelope.err" ||
	finish failed "could not assemble the envelope: $(head -c 300 "$out_dir/envelope.err")"
mv "$envelope.tmp" "$envelope"
log "envelope $envelope"

rc=0
"$SCRIPT_DIR/perf_compare.sh" "$baseline" "$envelope" >"$out_dir/compare.json" 2>"$out_dir/compare.err" || rc=$?
cat "$out_dir/compare.err" >&2
case "$rc" in
0 | 10 | 11) ;;
*) finish failed "perf_compare.sh exited $rc: $(tail -n 1 "$out_dir/compare.err")" ;;
esac
"$SCRIPT_DIR/perf_status_write.sh" --tool "$TOOL" --job weekly --compare "$out_dir/compare.json" \
	--envelope "$envelope" ${status_args[@]+"${status_args[@]}"} ||
	finish failed "status write failed for a compared run"
log "verdict $(jq -r .verdict "$out_dir/compare.json") (exit $rc)"
exit "$rc"
