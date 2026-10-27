#!/usr/bin/env bash
#
# perf_sampler.sh — read-only daily under-load sampler for trusty-memory and
# trusty-search (issue #10210, phase 4).
#
# Why: the daily Mac sampler (#10210 part 2) watches a live daemon on a busy
#   host. It must never change ranking or load the host further, must label a
#   busy-host sample instead of hiding it, and must never report a failed or
#   cut-short run as a good one (#10210 fail-closed review).
# What: every --interval-s seconds plus 0..--jitter-s seconds of jitter, for
#   --duration-s seconds, takes one sample:
#     load   1- and 15-minute load average; `under_load` is set at L1 >= 2
#            (the memory README idle line)
#     memory `memory.status` and `tools/call console_metrics` on the socket
#     search `search.health`, `search.indexes.list` (details) and the daemon
#            RSS from `ps`
#   It sends no other method: no recall, no search query, no write. A sample
#   reading L1 > --load-stop (default 8) ends the run before any call.
#   Only whitelisted numbers are kept. Palaces and indexes are named by rank
#   label (p01.. / i01.., largest disk bytes first, fixed at first sight); the
#   id-to-label map lives in a temp dir removed on exit. No data root, palace
#   or index name, socket, host path or daemon error text reaches the output.
#   Output, in the gitignored --out-dir: `samples.ndjson` (one row per
#   sample) and `envelope.json` (schema 1, same top-level keys for both
#   tools; `results` holds p50/p95/max per metric over the good samples).
#   Outcomes, fail-closed:
#     ok       every sample good, ran to --duration-s or --max-samples
#     stopped  L1 > --load-stop after at least one good sample (partial)
#     skipped  L1 > --load-stop before the first sample; no call made
#     failed   load unreadable, no daemon socket, no reply, an RPC error, a
#              reply that is not JSON, or a required figure missing, not a
#              number, or zero where zero is impossible (RSS, RAM, disk).
#              The first failed sample ends the run.
#   A failed or skipped run has empty `results`; it never carries a summary.
#   It writes no status file and no committed file (status wiring is phase 5).
#
# Usage: scripts/perf_sampler.sh --tool memory|search [--socket PATH]
#          [--pid PID] [--duration-s S] [--interval-s S] [--jitter-s S]
#          [--max-samples N] [--load-stop L] [--out-dir DIR] [--label mac|evo]
#   --socket       daemon socket (default: the daemon's own resolution from
#                  TRUSTY_DATA_DIR_OVERRIDE / TRUSTY_DATA_DIR /
#                  TRUSTY_SEARCH_SOCKET, else the per-user live location)
#   --pid          search daemon pid for `ps` (default: the socket's holder)
#   --duration-s   default 1800; --interval-s default 10, minimum 10;
#   --jitter-s     default 5; --max-samples default 0 (no cap)
#   --out-dir      default .trusty-mpm/perf/<tool>/sampler/<UTC stamp>
#   Test hooks (scripts/perf_sampler_selftest.sh):
#   PERF_SAMPLER_LOAD_CMD   prints "L1 L15"; non-zero exit = unreadable
#   PERF_SAMPLER_RPC_CMD    run as `$CMD SOCKET REQUEST`, prints the reply
#   PERF_SAMPLER_RSS_CMD    run as `$CMD PID`, prints RSS in KiB
#   PERF_SAMPLER_SLEEP_CMD  run as `$CMD SECONDS` instead of sleep
#
# Exit: 0 ok, 20 failed, 30 skipped, 31 stopped, 2 bad usage or missing tool.
#
# Test: scripts/perf_sampler_selftest.sh (stubbed cases; `--live` adds the
#   sandbox-daemon smoke for both tools)
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux); needs jq, nc, awk, ps.

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OS="$(uname -s)"
UNDER_LOAD=2

usage() {
	printf 'perf_sampler: error: %s\n' "$1" >&2
	exit 2
}

tool="" socket="" pid="" label="" out_dir=""
duration_s=1800 interval_s=10 jitter_s=5 max_samples=0 load_stop=8
while [ "$#" -gt 0 ]; do
	[ "$#" -ge 2 ] || usage "$1 needs a value"
	case "$1" in
	--tool) tool="$2" ;;
	--socket) socket="$2" ;;
	--pid) pid="$2" ;;
	--duration-s) duration_s="$2" ;;
	--interval-s) interval_s="$2" ;;
	--jitter-s) jitter_s="$2" ;;
	--max-samples) max_samples="$2" ;;
	--load-stop) load_stop="$2" ;;
	--out-dir) out_dir="$2" ;;
	--label) label="$2" ;;
	*) usage "unknown argument: $1" ;;
	esac
	shift 2
done
case "$tool" in memory | search) ;; *) usage "--tool must be memory or search" ;; esac
for n in "$duration_s" "$interval_s" "$jitter_s" "$max_samples"; do
	case "$n" in '' | *[!0-9]*) usage "seconds and counts must be whole numbers: '$n'" ;; esac
done
# #10210: the issue sets a >= 10 s interval for the daily sampler.
[ "$interval_s" -ge 10 ] || usage "--interval-s must be at least 10"
[ "$duration_s" -ge 1 ] || usage "--duration-s must be at least 1"
case "$load_stop" in '' | *[!0-9.]*) usage "--load-stop must be a number" ;; esac
case "$pid" in *[!0-9]*) usage "--pid must be a number" ;; esac
[ -n "$label" ] || { if [ "$OS" = Darwin ]; then label=mac; else label=evo; fi; }
case "$label" in mac | evo) ;; *) usage "--label must be mac or evo" ;; esac
for t in jq nc awk ps; do command -v "$t" >/dev/null 2>&1 || usage "$t not found"; done

if [ "$OS" = Darwin ]; then data_home="$HOME/Library/Application Support"; else data_home="${XDG_DATA_HOME:-$HOME/.local/share}"; fi
if [ -z "$socket" ] && [ "$tool" = memory ]; then
	socket="${TRUSTY_DATA_DIR_OVERRIDE:-$data_home}/trusty-memory/trusty-memory.sock"
elif [ -z "$socket" ]; then
	if [ -n "${TRUSTY_SEARCH_SOCKET:-}" ]; then socket="$TRUSTY_SEARCH_SOCKET"
	elif [ -n "${TRUSTY_DATA_DIR:-}" ]; then socket="$TRUSTY_DATA_DIR/trusty-search.sock"
	else socket="${TRUSTY_DATA_DIR_OVERRIDE:-$data_home}/trusty-search/trusty-search.sock"; fi
fi

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
[ -n "$out_dir" ] || out_dir="$REPO_ROOT/.trusty-mpm/perf/$tool/sampler/$stamp"
mkdir -p "$out_dir" || usage "cannot create the output dir"
out_dir="$(cd "$out_dir" && pwd)"
# Output inside a work tree must be git-ignored, or a run could be committed.
if git -C "$out_dir" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
	git -C "$out_dir" check-ignore -q "$out_dir/envelope.json" ||
		usage "refusing the output dir: inside a git work tree and not ignored"
fi
samples="$out_dir/samples.ndjson"
: >"$samples"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/perf_sampler.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
ranks="$scratch/ranks.tsv" # id <TAB> label; never copied to the output
: >"$ranks"

log() { printf 'perf_sampler: %s\n' "$*" >&2; }

read_load() { # prints "L1 L15" or fails
	if [ -n "${PERF_SAMPLER_LOAD_CMD:-}" ]; then
		$PERF_SAMPLER_LOAD_CMD
	elif [ "$OS" = Darwin ]; then
		sysctl -n vm.loadavg | awk '{print $2, $4}'
	else
		awk '{print $1, $3}' /proc/loadavg
	fi | awk 'NR == 1 && $1 ~ /^[0-9]+(\.[0-9]+)?$/ && $2 ~ /^[0-9]+(\.[0-9]+)?$/ {print $1, $2; ok = 1} END {exit !ok}'
}
gt() { awk -v a="$1" -v b="$2" 'BEGIN { exit !(a + 0 > b + 0) }'; }

# #10210: the only methods this sampler may send. Anything else is a bug.
rpc() { # $1 = method, $2 = params JSON; prints the `result` or fails with a code
	case "$1" in
	memory.status | search.health | search.indexes.list) ;;
	tools/call) [ "$2" = '{"name":"console_metrics","arguments":{}}' ] || return 3 ;;
	*) return 3 ;;
	esac
	local req reply
	req="$(jq -cn --arg m "$1" --argjson p "$2" '{jsonrpc: "2.0", id: 1, method: $m, params: $p}')"
	if [ -n "${PERF_SAMPLER_RPC_CMD:-}" ]; then
		reply="$($PERF_SAMPLER_RPC_CMD "$socket" "$req" 2>/dev/null)" || reply=""
	else
		[ -S "$socket" ] || return 4
		reply="$(printf '%s\n' "$req" | nc -U -w 5 "$socket" 2>/dev/null)" || reply=""
	fi
	[ -n "$reply" ] || return 5
	jq -e . >/dev/null 2>&1 <<<"$reply" || return 6
	# A daemon error message can name paths; only its code is kept (on stdout).
	if jq -e 'has("error")' >/dev/null <<<"$reply"; then
		jq -r 'if (.error.code | type) == "number" then .error.code else "(not a number)" end' <<<"$reply"
		return 7
	fi
	jq -e 'has("result") and .result != null' >/dev/null <<<"$reply" || return 8
	jq -c .result <<<"$reply"
}
rpc_reason() { # $1 = method, $2 = rpc exit code, $3 = rpc stdout (error code)
	case "$2" in
	3) echo "$1: method not on the read-only allowlist" ;;
	4) echo "$1: no daemon socket" ;;
	5) echo "$1: no reply from the daemon" ;;
	6) echo "$1: reply is not JSON" ;;
	7) echo "$1: daemon returned JSON-RPC error code ${3:-none}" ;;
	*) echo "$1: reply has no result" ;;
	esac
}

# Give every id a stable rank label, largest first at first sight.
# $1 = prefix (p|i), stdin = JSON array of {id, size}. Prints {id: label}.
rank_map() {
	local prefix="$1" next
	jq -r 'sort_by(-(.size // 0), .id) | .[].id' >"$scratch/ids"
	while IFS= read -r id; do
		awk -F'\t' -v id="$id" '$1 == id {f = 1} END {exit !f}' "$ranks" && continue
		next=$(($(awk -F'\t' -v p="$prefix" 'index($2, p) == 1' "$ranks" | wc -l) + 1))
		printf '%s\t%s%02d\n' "$id" "$prefix" "$next" >>"$ranks"
	done <"$scratch/ids"
	jq -Rn '[inputs | split("\t") | {(.[0]): .[1]}] | add // {}' <"$ranks"
}

# Required figures: present, numeric and finite; the `pos` list must be > 0.
check_fields() { # $1 = metrics JSON, $2 = required names, $3 = positive names
	jq -r --arg req "$2" --arg pos "$3" '
		. as $m
		| [($req | split(" ")[] | select(. != "")) as $k
		   | select(($m[$k] | type) != "number" or ($m[$k] | isinfinite) or ($m[$k] | isnan)) | $k]
		  + [($pos | split(" ")[] | select(. != "")) as $k
		   | select(($m[$k] | type) == "number" and $m[$k] <= 0) | "\($k)=0"]
		| join(" ")' <<<"$1"
}

sample_memory() { # sets metrics, per_rank, version, residency
	local status cm code
	status="$(rpc memory.status '{}')" || { code=$?; fail_reason="$(rpc_reason memory.status "$code" "$status")"; return 1; }
	cm="$(rpc tools/call '{"name":"console_metrics","arguments":{}}')" || {
		code=$?
		fail_reason="$(rpc_reason console_metrics "$code" "$cm")"
		return 1
	}
	cm="$(jq -c '.content[0].text // empty | fromjson? // empty' <<<"$cm")"
	[ -n "$cm" ] || { fail_reason="console_metrics: reply carries no metrics report"; return 1; }
	metrics="$(jq -c --argjson s "$status" '.metrics
		| {ram_bytes, ram_heap_bytes, ram_file_backed_bytes, disk_bytes, palace_count,
		   cached_palace_count, total_drawers, total_vectors, total_kg_triples, total_rooms,
		   status_palace_count: $s.palace_count, status_cached_palace_count: $s.cached_palace_count,
		   status_total_drawers: $s.total_drawers, status_total_vectors: $s.total_vectors}' <<<"$cm")"
	missing="$(check_fields "$metrics" "ram_bytes disk_bytes palace_count cached_palace_count total_drawers total_vectors total_kg_triples status_palace_count status_total_drawers" "ram_bytes disk_bytes")"
	[ -z "$missing" ] || { fail_reason="memory: required figure missing, not a number or zero: $missing"; return 1; }
	jq -c '[.metrics.palaces[]? | {id: (.id // .name), size: .disk_bytes}]' <<<"$cm" >"$scratch/list"
	map="$(rank_map p <"$scratch/list")"
	per_rank="$(jq -c --argjson map "$map" '[.metrics.palaces[]? | {label: $map[(.id // .name)],
		disk_bytes, drawer_count, vector_count, kg_triple_count}] | sort_by(.label)' <<<"$cm")"
	version="$(jq -r '.version // "unknown"' <<<"$status")"
	residency="$(jq -c '{cached: .cached_palace_count, total: .palace_count}' <<<"$metrics")"
}

daemon_pid() {
	[ -z "$pid" ] || { echo "$pid"; return 0; }
	local phys cand
	# #10210: the daemon binds a temp name and renames it, so lsof on the
	# socket path finds nothing; its daemon.lock beside the socket names it.
	phys="$(cd -P "$(dirname "$socket")" 2>/dev/null && pwd)/$(basename "$socket")"
	for cand in $(head -c 32 "$(dirname "$socket")/daemon.lock" 2>/dev/null | tr -cd '0-9') \
		$(lsof -t "$phys" 2>/dev/null); do
		case "$(ps -o comm= -p "$cand" 2>/dev/null)" in *trusty-search*) echo "$cand"; return 0 ;; esac
	done
	return 1
}
read_rss() { # $1 = pid; prints KiB
	if [ -n "${PERF_SAMPLER_RSS_CMD:-}" ]; then $PERF_SAMPLER_RSS_CMD "$1"; else ps -o rss= -p "$1"; fi |
		awk 'NR == 1 && $1 ~ /^[0-9]+$/ {print $1; ok = 1} END {exit !ok}'
}

sample_search() { # sets metrics, per_rank, version, residency
	local health list code dpid rss
	health="$(rpc search.health '{}')" || { code=$?; fail_reason="$(rpc_reason search.health "$code" "$health")"; return 1; }
	list="$(rpc search.indexes.list '{"details":true}')" || {
		code=$?
		fail_reason="$(rpc_reason search.indexes.list "$code" "$list")"
		return 1
	}
	dpid="$(daemon_pid)" || { fail_reason="search: daemon pid not found for ps"; return 1; }
	rss="$(read_rss "$dpid")" || { fail_reason="search: ps gave no RSS for the daemon"; return 1; }
	metrics="$(jq -c --argjson rss "$rss" --argjson list "$list" '
		{daemon_rss_kb: $rss, health_rss_mb: .rss_mb, cpu_pct, uptime_secs, indexes,
		 indexes_populated, indexes_empty, total_chunks, disk_bytes,
		 background_reindex_queue_depth, deferred_embed_queue_depth,
		 embedder_recent_timeout_count, listed_indexes: ($list.indexes | length),
		 health_ok: (if .status == "ok" then 1 else 0 end)}' <<<"$health")"
	missing="$(check_fields "$metrics" "daemon_rss_kb cpu_pct uptime_secs indexes total_chunks listed_indexes" "daemon_rss_kb")"
	[ -z "$missing" ] || { fail_reason="search: required figure missing, not a number or zero: $missing"; return 1; }
	jq -c '[.indexes[]? | if type == "object" then {id, size: .size_bytes} else {id: ., size: 0} end]' \
		<<<"$list" >"$scratch/list"
	map="$(rank_map i <"$scratch/list")"
	per_rank="$(jq -c --argjson map "$map" '[.indexes[]? | select(type == "object")
		| {label: $map[.id], size_bytes}] | sort_by(.label)' <<<"$list")"
	version="$(jq -r '.version // "unknown"' <<<"$health")"
	residency="$(jq -c '{cached: .warmboot_summary.indexes_loaded, total: .indexes}' <<<"$health")"
}

if [ "$OS" = Darwin ]; then
	host_os="macOS $(sw_vers -productVersion 2>/dev/null || echo unknown)"
	host_cpu="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
	host_ram="$(sysctl -n hw.memsize 2>/dev/null | awk '{printf "%d", $1 / 1073741824 + 0.5}')"
else
	# shellcheck source=/dev/null
	host_os="$(. /etc/os-release 2>/dev/null && echo "${NAME:-Linux} ${VERSION_ID:-}" || echo Linux)"
	host_cpu="$(awk -F': ' '/model name/ {print $2; exit}' /proc/cpuinfo)"
	host_ram="$(awk '/MemTotal/ {printf "%d", $2 / 1048576 + 0.5}' /proc/meminfo)"
fi
commit="$(git -C "$SCRIPT_DIR" rev-parse HEAD 2>/dev/null || echo unknown)"
utc_start="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
start_s="$(date +%s)"
version=unknown residency='{"cached":null,"total":null}'
load_first="" load_last=""

# Write the envelope and exit. $1 = outcome, $2 = reason (empty for ok).
finish() {
	local outcome="$1" reason="$2" rc=0
	case "$outcome" in failed) rc=20 ;; skipped) rc=30 ;; stopped) rc=31 ;; esac
	[ -z "$reason" ] || log "$outcome: $reason"
	jq -s --arg tool "trusty-$tool" --arg version "$version" --arg commit "$commit" \
		--arg label "$label" --arg os "$host_os" --arg cpu "$host_cpu" --argjson ram "${host_ram:-0}" \
		--arg utc_start "$utc_start" --arg utc_end "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
		--arg outcome "$outcome" --arg reason "$reason" --argjson residency "$residency" \
		--arg first "$load_first" --arg last "$load_last" --argjson interval "$interval_s" \
		--argjson jitter "$jitter_s" --argjson duration "$duration_s" --argjson stop "$load_stop" \
		--argjson ul "$UNDER_LOAD" '
		def ld($s): if $s == "" then null else ($s | split(" ") | {l1: (.[0] | tonumber), l15: (.[1] | tonumber)}) end;
		def pct($xs; $p): ($xs | sort) as $s | $s[((($p * ($s | length)) | ceil) - 1) | if . < 0 then 0 else . end];
		def unit($k): if ($k | test("_bytes$")) then "bytes" elif ($k | test("_kb$")) then "KiB"
			elif ($k | test("_mb$")) then "MiB" elif ($k | test("_pct$")) then "pct"
			elif ($k | test("_secs$")) then "s" else "count" end;
		def row($dim; $k; $xs; $extra): {dimension: $dim, metric: $k, unit: unit($k), n: ($xs | length),
			k: null, p50: pct($xs; 0.5), p95: pct($xs; 0.95), max: ($xs | max), value: $xs[-1],
			threshold: null, notes: "under-load sampler; nearest-rank percentiles over good samples"} + $extra;
		[.[] | select(.status == "good")] as $good
		| (if $outcome == "ok" or $outcome == "stopped" then
			([$good[].metrics | to_entries[] | select(.value | type == "number")] | group_by(.key)
			 | map(row("sampler"; .[0].key; map(.value); {})))
			+ ([$good[] | .ranks[] | . as $r | to_entries[] | select(.key != "label" and (.value | type) == "number")
			    | {label: $r.label, key, value}] | group_by([.label, .key])
			   | map(row("storage"; .[0].key; map(.value); {rank_label: .[0].label})))
		   else [] end) as $results
		| {schema: 1, kind: "perf_sampler", tool: $tool, version: $version, commit: $commit,
		   label: $label, host: {id: $label, os: $os, cpu: $cpu, ram_gb: $ram},
		   date: ($utc_start[0:10]), utc_start: $utc_start, utc_end: $utc_end, corpus: "live",
		   load: {first: ld($first), last: ld($last), max_l1: ([.[].load1] | max),
		          max_l15: ([.[].load15] | max), stop_above_l1: $stop, under_load_at_l1: $ul},
		   outcome: $outcome, reason: (if $reason == "" then null else $reason end),
		   sampling: {interval_s: $interval, jitter_s: $jitter, duration_s: $duration,
		              samples: length, good: ($good | length),
		              under_load: ([$good[] | select(.under_load)] | length)},
		   residency: $residency, results: $results}' "$samples" >"$out_dir/envelope.json.tmp" ||
		{ log "could not build the envelope"; rm -f "$out_dir/envelope.json.tmp"; exit 20; }
	mv "$out_dir/envelope.json.tmp" "$out_dir/envelope.json"
	log "trusty-$tool $outcome (exit $rc); envelope in the output dir"
	exit "$rc"
}

seq=0 good=0
while :; do
	seq=$((seq + 1))
	if ! load="$(read_load)"; then
		finish failed "sample $seq: load average unreadable"
	fi
	load_last="$load"
	[ -n "$load_first" ] || load_first="$load"
	l1="${load%% *}" l15="${load##* }"
	if gt "$l1" "$load_stop"; then
		jq -cn --argjson seq "$seq" --argjson l1 "$l1" --argjson l15 "$l15" \
			'{seq: $seq, load1: $l1, load15: $l15, status: "stopped"}' >>"$samples"
		why="sample $seq: 1-minute load $l1 > $load_stop"
		if [ "$good" -eq 0 ]; then finish skipped "$why; no sample taken"; fi
		finish stopped "$why; stopped after $good good samples"
	fi
	fail_reason="" metrics="" per_rank="[]"
	ok_sample=1
	if [ "$tool" = memory ]; then sample_memory || ok_sample=0; else sample_search || ok_sample=0; fi
	if [ "$ok_sample" -eq 0 ]; then
		jq -cn --argjson seq "$seq" --argjson l1 "$l1" --argjson l15 "$l15" --arg r "$fail_reason" \
			'{seq: $seq, load1: $l1, load15: $l15, status: "failed", reason: $r}' >>"$samples"
		finish failed "sample $seq: $fail_reason"
	fi
	good=$((good + 1))
	jq -cn --argjson seq "$seq" --argjson t "$(($(date +%s) - start_s))" --argjson l1 "$l1" \
		--argjson l15 "$l15" --argjson ul "$UNDER_LOAD" --argjson m "$metrics" --argjson r "$per_rank" \
		'{seq: $seq, t_s: $t, load1: $l1, load15: $l15, under_load: ($l1 >= $ul), status: "good",
		  metrics: $m, ranks: $r}' >>"$samples"
	if [ "$max_samples" -gt 0 ] && [ "$seq" -ge "$max_samples" ]; then break; fi
	wait_s=$((interval_s + (jitter_s > 0 ? RANDOM % (jitter_s + 1) : 0)))
	[ $(($(date +%s) - start_s + wait_s)) -le "$duration_s" ] || break
	${PERF_SAMPLER_SLEEP_CMD:-sleep} "$wait_s"
done
[ "$good" -ge 1 ] || finish failed "no good sample"
finish ok ""
