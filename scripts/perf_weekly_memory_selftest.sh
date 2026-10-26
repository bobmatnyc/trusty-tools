#!/usr/bin/env bash
#
# perf_weekly_memory_selftest.sh — stubbed cases for scripts/perf_weekly_memory.sh
# (issue #10210, phase 2).
#
# Why: the weekly runner must fail closed (#10210 review). A runner only ever
#   seen passing cannot be told apart from one that writes ok whatever the
#   suites did, so each case asserts the exit code, the status.json verdict and
#   which suites ran.
# What: replaces EVO with three stubs through the runner's test hooks:
#   PERF_RUN_CMD prints a fixture log from scripts/test-data/perf-weekly/ per
#   suite (or fails, hangs, or prints no figure on request), PERF_LOAD_CMD
#   prints a set load or fails, PERF_HOST_CMD prints host lines. Cases: a good
#   run (ok, envelope accepted by perf_compare.sh with no missing row), a
#   failing suite, a suite with no figure, a concurrent block missing, a
#   timeout, load 2.0 and 3.5 (skipped, no suite run), load 1.99 (runs) and
#   EVO unreachable (skipped). No EVO, no cargo.
#
# Test: this IS the test. Run directly:
#   ./scripts/perf_weekly_memory_selftest.sh
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux); needs jq, git, timeout.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="$SCRIPT_DIR/perf_weekly_memory.sh"
COMPARE="$SCRIPT_DIR/perf_compare.sh"
BASELINE="$SCRIPT_DIR/../docs/trusty-memory/perf/2026-10-17-0.36.0-evo.json"
FIX="$SCRIPT_DIR/test-data/perf-weekly"

work="$(mktemp -d "${TMPDIR:-/tmp}/perf_weekly_selftest.XXXXXX")"
# PERF_SELFTEST_KEEP=1 keeps the work dir for inspection.
if [ -z "${PERF_SELFTEST_KEEP:-}" ]; then trap 'rm -rf "$work"' EXIT; else echo "work dir: $work"; fi

cat >"$work/run" <<'EOF'
#!/usr/bin/env bash
# Stub for `evo-gate-log ... evo cargo ...`: $1 = gate, rest = cargo argv.
suite="${1#perf-}"
shift
printf '%s %s\n' "$suite" "$*" >>"$STUB_CALLS"
# A failing run still prints its figures: the exit code alone must fail it.
case " ${STUB_FAIL:-} " in *" $suite "*)
	cat "$STUB_FIX/$suite.log"
	echo "error: test failed, to rerun pass --test"
	exit 101
	;;
esac
case " ${STUB_EMPTY:-} " in *" $suite "*) echo "test result: ok. 0 passed"; exit 0 ;; esac
case " ${STUB_HANG:-} " in *" $suite "*) sleep 30 ;; esac
if [ "$suite" = concurrent ] && [ -n "${STUB_DROP_SUSTAINED:-}" ]; then
	sed '/=== test_sustained_load/,$d' "$STUB_FIX/concurrent.log"
	exit 0
fi
cat "$STUB_FIX/$suite.log"
EOF
cat >"$work/load" <<'EOF'
#!/usr/bin/env bash
[ "$STUB_LOAD" != unreachable ] || exit 255
echo "$STUB_LOAD 1.00 1.00 1/500 12345"
EOF
cat >"$work/host" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "Ubuntu 26.04 LTS" "Stub CPU" 28 47 "rustc 1.99.0 (stub 2026-09-28)"
EOF
chmod +x "$work/run" "$work/load" "$work/host"

fail=0
pass() { printf 'PASS %s\n' "$1"; }
bad() {
	printf 'FAIL %s: %s\n' "$1" "$2"
	fail=1
}

# Run the runner with stubs. $1 = case name, $2 = load, rest = runner flags.
# Sets rc, out, status, calls.
run_case() {
	local name="$1" load="$2"
	shift 2
	out="$work/$name/out"
	status="$work/$name/status.json"
	calls="$work/$name/calls"
	mkdir -p "$out"
	: >"$calls"
	rc=0
	STUB_CALLS="$calls" STUB_FIX="$FIX" STUB_LOAD="$load" \
		PERF_RUN_CMD="$work/run" PERF_LOAD_CMD="$work/load" PERF_HOST_CMD="$work/host" \
		"$RUNNER" --out-dir "$out" --status-file "$status" --settle-s 0 \
		--baseline "$BASELINE" "$@" >"$work/$name/stdout" 2>"$work/$name/stderr" || rc=$?
}
verdict() { jq -r '.tools["trusty-memory"].verdict' "$status" 2>/dev/null || echo none; }
reason() { jq -r '.tools["trusty-memory"].reason' "$status" 2>/dev/null || echo none; }
ran() { awk '{print $1}' "$calls" | sort | uniq -c | awk '{printf "%s%s=%s", sep, $2, $1; sep=" "}'; }

# 1. Good run at load 1.99 (just under the gate): ok, every suite at its count.
run_case good 1.99
got="$rc $(verdict) $(ran)"
want="0 ok concurrent=3 eval=1 profile=5 slice=3"
if [ "$got" = "$want" ]; then pass "good run: $got"; else bad "good run" "got '$got', want '$want'"; fi

# The envelope must pass perf_compare.sh on its own, with no missing or
# unparsed row, and carry the medians of the fixtures.
crc=0
"$COMPARE" "$BASELINE" "$out/envelope.json" >"$work/good.report" 2>/dev/null || crc=$?
if [ "$crc" = 0 ] && jq -e '.verdict == "ok" and .missing == [] and .unparsed == []
	and (.rows | length) == 11' "$work/good.report" >/dev/null; then
	pass "good envelope accepted by perf_compare.sh: exit 0, 11 graded rows, none missing"
else
	bad "good envelope" "compare exit $crc: $(jq -c '{verdict, missing, unparsed, n: (.rows | length)}' "$work/good.report" 2>&1)"
fi
got="$(jq -r '[(.results | length),
	(.results[] | select(.metric == "seeded_in") | .value),
	(.results[] | select(.metric == "concurrent_perf test_concurrent_rw WRITE") | .p95),
	(.results[] | select(.metric == "query_embed_under_slice") | .p95),
	(.results[] | select(.metric == "embed_64_chunk_batch") | .value)] | map(tostring) | join(" ")' "$out/envelope.json")"
want="19 431.2 1347.473 1236.305 1251.297"
if [ "$got" = "$want" ]; then pass "envelope figures: $got"; else bad "envelope figures" "got '$got', want '$want'"; fi
if grep -q '^eval cargo +1.99.0 test -p trusty-memory --test recall_eval -- --include-ignored --nocapture$' "$calls" &&
	grep -q 'env.TRUSTY_RECALL_SUPERSESSION_PROFILE="1"' "$calls"; then
	pass "eval runs --include-ignored; profile sets TRUSTY_RECALL_SUPERSESSION_PROFILE"
else
	bad "suite argv" "$(head -n 2 "$calls")"
fi

# 2-6. Failures: status failed, exit 20, no envelope, later suites not run.
# case<TAB>env<TAB>extra flag<TAB>reason substring<TAB>suites run
TAB="$(printf '\t')"
FAILS="suite-fails${TAB}STUB_FAIL=profile${TAB}-${TAB}profile1 exited 101${TAB}eval=1 profile=1
no-figure${TAB}STUB_EMPTY=slice${TAB}-${TAB}slice1 exited 0 but printed no parseable figure${TAB}concurrent=3 eval=1 profile=5 slice=1
eval-no-figure${TAB}STUB_EMPTY=eval${TAB}-${TAB}eval1 exited 0 but printed no parseable figure${TAB}eval=1
block-missing${TAB}STUB_DROP_SUSTAINED=1${TAB}-${TAB}concurrent1 exited 0 but printed no parseable figure${TAB}concurrent=1 eval=1 profile=5
timeout${TAB}STUB_HANG=eval${TAB}--suite-timeout=1${TAB}eval1 timed out after 1s${TAB}eval=1"
while IFS="$TAB" read -r name envset flag want_reason want_ran; do
	extra=()
	[ "$flag" = - ] || extra=("${flag%%=*}" "${flag#*=}")
	export "${envset?}"
	run_case "$name" 0.5 ${extra[@]+"${extra[@]}"}
	unset "${envset%%=*}"
	got="$rc $(verdict) $(ran)"
	if [ "$got" != "20 failed $want_ran" ]; then
		bad "$name" "got '$got', want '20 failed $want_ran'"
	elif ! reason | grep -qF "$want_reason"; then
		bad "$name" "reason '$(reason)' lacks '$want_reason'"
	elif [ -e "$out/envelope.json" ]; then
		bad "$name" "an envelope was written for a failed run"
	else
		pass "$name: exit 20, status failed, ran $want_ran"
	fi
done <<EOF
$FAILS
EOF

# 7-9. Busy or unreachable EVO: skipped, exit 30, no suite runs at all.
for load in 2.00 3.5 unreachable; do
	run_case "load-$load" "$load"
	got="$rc $(verdict) calls=$(wc -l <"$calls" | tr -d ' ')"
	if [ "$got" = "30 skipped calls=0" ] && [ ! -e "$out/envelope.json" ]; then
		pass "load $load: exit 30, skipped ($(reason)), no suite run"
	else
		bad "load $load" "got '$got', want '30 skipped calls=0'"
	fi
done

if [ "$fail" -ne 0 ]; then
	echo "perf_weekly_memory_selftest: FAILED"
	exit 1
fi
echo "perf_weekly_memory_selftest: all cases passed"
