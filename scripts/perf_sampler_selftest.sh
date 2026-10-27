#!/usr/bin/env bash
#
# perf_sampler_selftest.sh — cases for scripts/perf_sampler.sh (issue #10210,
# phase 4).
#
# Why: the sampler must fail closed and must not leak a private name or path
#   (#10210 criteria 5 and 6). A sampler only ever seen passing cannot be told
#   apart from one that reports every run as ok, so each case asserts the exit
#   code, the envelope outcome and what the run wrote.
# What: stub mode (the default, no daemon, no cargo) drives the sampler through
#   its PERF_SAMPLER_* hooks: a stub socket client answering from canned
#   replies that carry a private palace/index name and a host path, a load
#   sequence, an RSS reading and a no-op sleep. Cases: a good memory run and a
#   good search run (same top-level envelope keys, rank labels only, no name
#   or path, allowlisted methods only), L1 8.0 (runs), L1 above 8 before the
#   first sample (skipped) and after one (stopped), and one failed case per
#   failure branch: load unreadable, no reply, not JSON, an RPC error whose
#   message names a path, a missing figure, a zero RAM figure, an unreadable
#   RSS and a zero RSS.
#   `--live` instead starts one sandbox daemon per tool with
#   scripts/sandbox_memory_daemon.sh and scripts/sandbox_search_daemon.sh
#   (search pinned to TRUSTY_EMBEDDER=stdio, so no python venv download),
#   creates a palace and an index with private names, runs the real sampler
#   for two samples each, greps both outputs for the names and paths, compares
#   the envelope keys and stops both daemons. It runs installed binaries only.
#
# Test: this IS the test. Run directly:
#   ./scripts/perf_sampler_selftest.sh          # stub cases
#   ./scripts/perf_sampler_selftest.sh --live   # sandbox-daemon smoke
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux); needs jq.

if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SAMPLER="$SCRIPT_DIR/perf_sampler.sh"
SECRET="zz-p4-secret"

work="$(mktemp -d "${TMPDIR:-/tmp}/perf_sampler_selftest.XXXXXX")"
if [ -z "${PERF_SELFTEST_KEEP:-}" ]; then trap 'rm -rf "$work"' EXIT; else echo "work dir: $work"; fi

fail=0
pass() { printf 'PASS %s\n' "$1"; }
bad() {
	printf 'FAIL %s: %s\n' "$1" "$2"
	fail=1
}
# Fails when the output dir names a private palace/index or a host path.
leaks() { # $1 = dir, rest = extra literal strings
	local dir="$1"
	shift
	grep -rlE "$SECRET|/Users/|/home/|/private/|/tmp/|Application Support|data_root|root_path" "$dir" && return 0
	for s in "$@"; do grep -rlF "$s" "$dir" && return 0; done
	return 1
}

# A version manager's shim does not run under the sandbox's `env -i` PATH, so
# the live mode passes the first real binary on PATH.
real_bin() {
	local d
	local IFS=:
	for d in $PATH; do
		case "$d" in */shims | */shims/*) continue ;; esac
		[ -x "$d/$1" ] && { echo "$d/$1"; return 0; }
	done
	echo "$1"
}

# --live: one real sandbox daemon per tool, two real samples each (#10210
# criterion 5 grep test and the phase 4 binary smoke).
live() {
	local mem="$work/m" srch="$work/s" mpid spid out
	# Short dirs: the sandbox socket path must stay under 100 bytes.
	mem="$(mktemp -d /tmp/p4m.XXXXXX)" srch="$(mktemp -d /tmp/p4s.XXXXXX)"
	# trusty-search refuses an index root under any temp dir, so the empty
	# root sits in the ignored .trusty-mpm/ of this checkout.
	local root="$SCRIPT_DIR/../.trusty-mpm/perf/selftest/$SECRET-root-$$"
	mkdir -p "$root"
	root="$(cd "$root" && pwd)"
	# A fresh search daemon accepts no index root (default-deny); allow this
	# one in the sandbox HOME's config dir (macOS and XDG locations).
	local cfg
	for cfg in "$srch/home/Library/Application Support/trusty-search" "$srch/home/.config/trusty-search"; do
		mkdir -p "$cfg"
		printf '[[index]]\npath = "%s"\n' "$root" >"$cfg/allowlist.toml"
	done
	"$SCRIPT_DIR/sandbox_memory_daemon.sh" --bin "$(real_bin trusty-memory)" --dir "$mem" >"$work/mem.log" 2>&1 &
	mpid=$!
	TRUSTY_EMBEDDER=stdio "$SCRIPT_DIR/sandbox_search_daemon.sh" --bin "$(real_bin trusty-search)" \
		--dir "$srch" >"$work/srch.log" 2>&1 &
	spid=$!
	local msock="$mem/data/trusty-memory/trusty-memory.sock" ssock="$srch/data/trusty-search.sock"
	for _ in $(seq 1 60); do [ -S "$msock" ] && [ -S "$ssock" ] && break; sleep 1; done
	if [ -S "$msock" ] && [ -S "$ssock" ]; then
		printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"palace_create\",\"params\":{\"name\":\"$SECRET-palace\",\"force\":true}}" |
			nc -U -w 5 "$msock" >"$work/mk-palace" || true
		printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"search.index.create\",\"params\":{\"id\":\"$SECRET-index\",\"root_path\":\"$root\"}}" |
			nc -U -w 5 "$ssock" >"$work/mk-index" || true
		for t in memory search; do
			out="$work/live-$t"
			local sock="$msock" rc=0
			[ "$t" = memory ] || sock="$ssock"
			PERF_SAMPLER_SLEEP_CMD=true "$SAMPLER" --tool "$t" --socket "$sock" --max-samples 2 \
				--out-dir "$out" >/dev/null 2>"$work/live-$t.err" || rc=$?
			local named
			named="$(jq -r '[.results[] | select(.rank_label) | .rank_label] | unique | join(",")' "$out/envelope.json" 2>/dev/null || echo none)"
			if [ "$rc" != 0 ]; then
				bad "live $t" "exit $rc: $(jq -r .reason "$out/envelope.json" 2>/dev/null || tail -n 1 "$work/live-$t.err")"
			elif [ -z "$named" ] || [ "$named" = none ]; then
				bad "live $t" "the private palace/index was not sampled (rank labels '$named'; setup: $(head -c 200 "$work/mk-palace" "$work/mk-index" | tr '\n' ' '))"
			elif leak="$(leaks "$out" "$mem" "$srch" "$HOME")"; then
				bad "live $t" "private name or path in $leak"
			else
				pass "live $t: exit 0, $(jq -r '"\(.outcome) good=\(.sampling.good) under_load=\(.sampling.under_load) version=\(.version)"' "$out/envelope.json"), rank labels $named, no name or path"
			fi
		done
		if [ "$(jq -c keys "$work/live-memory/envelope.json" 2>/dev/null)" = "$(jq -c keys "$work/live-search/envelope.json" 2>/dev/null)" ]; then
			pass "live: same top-level envelope keys for memory and search"
		else
			bad "live keys" "memory and search envelope keys differ"
		fi
	else
		bad "live" "a sandbox socket did not appear within 60 s: memory: $(tail -n 2 "$work/mem.log" | tr '\n' ' ') search: $(tail -n 2 "$work/srch.log" | tr '\n' ' ')"
	fi
	"$SCRIPT_DIR/sandbox_memory_daemon.sh" --stop "$mem" >/dev/null 2>&1 || bad "live" "memory sandbox did not stop"
	"$SCRIPT_DIR/sandbox_search_daemon.sh" --stop "$srch" >/dev/null 2>&1 || bad "live" "search sandbox did not stop"
	wait "$mpid" "$spid" 2>/dev/null || true
	rm -rf "$mem" "$srch"
	rmdir "$root" 2>/dev/null || true
}
if [ "${1:-}" = --live ]; then
	live
	if [ "$fail" -ne 0 ]; then echo "perf_sampler_selftest --live: FAILED"; exit 1; fi
	echo "perf_sampler_selftest --live: all cases passed"
	exit 0
fi

cat >"$work/rpc" <<'EOF'
#!/usr/bin/env bash
# Stub socket client: $1 = socket, $2 = request. Replies from canned JSON.
m="$(jq -r .method <<<"$2")"
[ "$m" != tools/call ] || m="tools/call:$(jq -r .params.name <<<"$2")"
echo "$m" >>"$STUB_CALLS"
case " ${STUB_BREAK:-} " in
*" noreply "*) exit 0 ;;
*" notjson "*) echo "<html>bad gateway</html>"; exit 0 ;;
*" rpcerror "*) echo '{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"no project root at /Users/stub/zz-p4-secret-palace"}}'; exit 0 ;;
esac
case "$m" in
memory.status) r='{"cached_palace_count":1,"data_root":"/Users/stub/Library/Application Support/trusty-memory","palace_count":2,"total_drawers":40,"total_kg_triples":9,"total_vectors":40,"version":"9.9.9"}' ;;
tools/call:console_metrics)
	ram=1000000
	case " ${STUB_BREAK:-} " in *" zeroram "*) ram=0 ;; esac
	rep="$(jq -cn --argjson ram "$ram" '{service_id: "trusty-memory", status: "ok", version: "9.9.9",
		metrics: {data_root: "/Users/stub/Library/Application Support/trusty-memory", ram_bytes: $ram,
		ram_heap_bytes: 900, ram_file_backed_bytes: 100, disk_bytes: 5000, palace_count: 2,
		cached_palace_count: 1, total_drawers: 40, total_vectors: 40, total_kg_triples: 9, total_rooms: 1,
		palaces: [{id: "zz-p4-secret-small", name: "zz-p4-secret-small", disk_bytes: 1000, drawer_count: 10,
		  vector_count: 10, kg_triple_count: 2},
		 {id: "zz-p4-secret-big", name: "zz-p4-secret-big", disk_bytes: 4000, drawer_count: 30,
		  vector_count: 30, kg_triple_count: 7}]}}')"
	case " ${STUB_BREAK:-} " in *" missing "*) rep="$(jq -c 'del(.metrics.total_vectors)' <<<"$rep")" ;; esac
	r="$(jq -cn --arg t "$rep" '{content: [{type: "text", text: $t}]}')"
	;;
search.health) r='{"status":"ok","version":"0.61.0","rss_mb":20,"cpu_pct":1.5,"uptime_secs":100,"indexes":2,"indexes_populated":2,"indexes_empty":0,"total_chunks":300,"disk_bytes":9000,"background_reindex_queue_depth":0,"deferred_embed_queue_depth":0,"embedder_recent_timeout_count":0,"transport":{"socket_path":"/Users/stub/.trusty/trusty-search.sock"},"warmboot_summary":{"indexes_loaded":2}}' ;;
search.indexes.list) r='{"indexes":[{"id":"zz-p4-secret-a","root_path":"/Users/stub/code/zz-p4-secret-a","size_bytes":100},{"id":"zz-p4-secret-b","root_path":"/Users/stub/code/zz-p4-secret-b","size_bytes":900}]}' ;;
*) echo '{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"stub: unexpected method"}}'; exit 0 ;;
esac
jq -cn --argjson r "$r" '{jsonrpc: "2.0", id: 1, result: $r}'
EOF
cat >"$work/load" <<'EOF'
#!/usr/bin/env bash
# Prints the next "L1 L15" line of $STUB_LOADS, repeating the last one.
n=$(($(cat "$STUB_LOADN") + 1)); echo "$n" >"$STUB_LOADN"
line="$(tr ',' '\n' <<<"$STUB_LOADS" | sed -n "${n}p")"
[ -n "$line" ] || line="$(tr ',' '\n' <<<"$STUB_LOADS" | tail -n 1)"
[ "$line" != fail ] || exit 1
echo "$line 1.00"
EOF
cat >"$work/rss" <<'EOF'
#!/usr/bin/env bash
case " ${STUB_BREAK:-} " in *" norss "*) exit 1 ;; *" zerorss "*) echo 0; exit 0 ;; esac
echo 51200
EOF
chmod +x "$work/rpc" "$work/load" "$work/rss"

# run_case NAME TOOL LOADS [sampler flags]; sets rc, out, env (envelope), calls.
run_case() {
	local name="$1" tool="$2" loads="$3"
	shift 3
	out="$work/$name/out"
	calls="$work/$name/calls"
	mkdir -p "$out"
	: >"$calls"
	echo 0 >"$work/$name/loadn"
	rc=0
	STUB_CALLS="$calls" STUB_LOADS="$loads" STUB_LOADN="$work/$name/loadn" \
		PERF_SAMPLER_RPC_CMD="$work/rpc" PERF_SAMPLER_LOAD_CMD="$work/load" \
		PERF_SAMPLER_RSS_CMD="$work/rss" PERF_SAMPLER_SLEEP_CMD=true \
		"$SAMPLER" --tool "$tool" --socket "$work/none.sock" --pid 4242 --out-dir "$out" \
		--max-samples 3 "$@" >"$work/$name/stdout" 2>"$work/$name/stderr" || rc=$?
	env="$out/envelope.json"
}
summary() { jq -r '"\(.outcome) good=\(.sampling.good) results=\(.results | length)"' "$env" 2>/dev/null || echo "no-envelope"; }

# 1-2. Good runs: ok, rank labels only, nothing private, allowlisted methods.
for tool in memory search; do
	run_case "good-$tool" "$tool" "1.50"
	got="$rc $(summary)"
	if [ "$rc" != 0 ] || ! jq -e '.outcome == "ok" and .sampling.good == 3 and (.results | length) > 0
		and .reason == null' "$env" >/dev/null; then
		bad "good $tool" "got '$got'"
	elif leak="$(leaks "$out")"; then
		bad "good $tool" "private name or path in $leak"
	elif sort -u "$calls" | grep -qvxE 'memory.status|tools/call:console_metrics|search.health|search.indexes.list'; then
		bad "good $tool" "sent a method off the allowlist: $(sort -u "$calls" | tr '\n' ' ')"
	else
		pass "good $tool: $got, methods $(sort -u "$calls" | tr '\n' ' ')"
	fi
	cp "$env" "$work/env-$tool.json"
done
labels="$(jq -r '[.results[] | select(.rank_label) | .rank_label] | unique | join(",")' "$work/env-memory.json") $(jq -r '[.results[] | select(.rank_label) | .rank_label] | unique | join(",")' "$work/env-search.json")"
big="$(jq -r '.results[] | select(.rank_label == "p01" and .metric == "disk_bytes") | .p50' "$work/env-memory.json")"
if [ "$labels" = "p01,p02 i01,i02" ] && [ "$big" = 4000 ]; then
	pass "rank labels: $labels; p01 is the largest palace (disk_bytes $big)"
else
	bad "rank labels" "got '$labels', p01 disk_bytes '$big'"
fi
# 3. One reader: memory and search envelopes carry the same top-level keys.
km="$(jq -c 'keys' "$work/env-memory.json")"
ks="$(jq -c 'keys' "$work/env-search.json")"
if [ "$km" = "$ks" ] && jq -e '.schema == 1 and (.results[0] | has("p95") and has("metric"))' "$work/env-search.json" >/dev/null; then
	pass "same top-level envelope keys for memory and search: $km"
else
	bad "envelope keys" "memory $km vs search $ks"
fi

# 4. L1 exactly 8 is not above the stop line: the run completes.
run_case load-8 memory "8.00"
if [ "$rc" = 0 ] && [ "$(summary)" = "ok good=3 results=$(jq '.results | length' "$work/env-memory.json")" ]; then
	pass "L1 8.00: runs, $(summary)"
else
	bad "L1 8.00" "got '$rc $(summary)'"
fi
# 5. L1 above 8 before any sample: skipped, no daemon call, no results.
run_case load-skip search "8.01"
if [ "$rc" = 30 ] && jq -e '.outcome == "skipped" and .results == [] and .sampling.good == 0
	and (.reason | test("1-minute load 8.01 > 8"))' "$env" >/dev/null && [ ! -s "$calls" ]; then
	pass "L1 8.01 first: exit 30, skipped ($(jq -r .reason "$env")), no call made"
else
	bad "L1 8.01 first" "got '$rc $(summary)', calls $(wc -l <"$calls" | tr -d ' ')"
fi
# 6. L1 above 8 after one good sample: stopped, the reason recorded, exit 31.
run_case load-stop memory "1.00,9.10"
if [ "$rc" = 31 ] && jq -e '.outcome == "stopped" and .sampling.good == 1
	and (.reason | test("1-minute load 9.10 > 8; stopped after 1 good"))' "$env" >/dev/null; then
	pass "L1 9.10 mid-run: exit 31, stopped ($(jq -r .reason "$env"))"
else
	bad "L1 9.10 mid-run" "got '$rc $(summary)'"
fi

# 7-14. Each failure branch: exit 20, outcome failed, empty results, no leak.
# case<TAB>tool<TAB>loads<TAB>STUB_BREAK<TAB>reason substring
TAB="$(printf '\t')"
FAILS="load-unreadable${TAB}memory${TAB}fail${TAB}-${TAB}load average unreadable
no-reply${TAB}memory${TAB}1.00${TAB}noreply${TAB}memory.status: no reply from the daemon
not-json${TAB}search${TAB}1.00${TAB}notjson${TAB}search.health: reply is not JSON
rpc-error${TAB}memory${TAB}1.00${TAB}rpcerror${TAB}memory.status: daemon returned JSON-RPC error code -32603
missing-figure${TAB}memory${TAB}1.00${TAB}missing${TAB}required figure missing, not a number or zero: total_vectors
zero-ram${TAB}memory${TAB}1.00${TAB}zeroram${TAB}required figure missing, not a number or zero: ram_bytes=0
no-rss${TAB}search${TAB}1.00${TAB}norss${TAB}search: ps gave no RSS for the daemon
zero-rss${TAB}search${TAB}1.00${TAB}zerorss${TAB}required figure missing, not a number or zero: daemon_rss_kb=0"
while IFS="$TAB" read -r name tool loads brk want; do
	STUB_BREAK="${brk#-}" run_case "$name" "$tool" "$loads"
	if [ "$rc" != 20 ] || ! jq -e '.outcome == "failed" and .results == [] and .sampling.good == 0' "$env" >/dev/null; then
		bad "$name" "got '$rc $(summary)'"
	elif ! jq -r .reason "$env" | grep -qF "$want"; then
		bad "$name" "reason '$(jq -r .reason "$env")' lacks '$want'"
	elif leak="$(leaks "$out")"; then
		bad "$name" "private name or path in $leak"
	else
		pass "$name: exit 20, failed ($(jq -r .reason "$env"))"
	fi
done <<EOF
$FAILS
EOF

# 15. A failure after good samples still fails the run and drops the summary.
run_case late-fail memory "1.00,1.00,fail"
if [ "$rc" = 20 ] && jq -e '.outcome == "failed" and .results == [] and .sampling.good == 2' "$env" >/dev/null; then
	pass "failure after 2 good samples: exit 20, failed, no summary"
else
	bad "late failure" "got '$rc $(summary)'"
fi

# 16. The interval floor is 10 s (#10210).
rc=0
"$SAMPLER" --tool memory --interval-s 9 --out-dir "$work/floor" >/dev/null 2>&1 || rc=$?
if [ "$rc" = 2 ]; then pass "--interval-s 9 refused: exit 2"; else bad "interval floor" "exit $rc"; fi

if [ "$fail" -ne 0 ]; then
	echo "perf_sampler_selftest: FAILED"
	exit 1
fi
echo "perf_sampler_selftest: all cases passed"
