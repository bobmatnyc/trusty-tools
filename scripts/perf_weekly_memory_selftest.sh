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
#   EVO unreachable (skipped). PERF_REAP_CMD runs scripts/perf_weekly_evo_reap.sh
#   against a fake EVO (ps, kill, pkill, systemctl stubs): a timeout stops this
#   run's scope and queued waiter only, and stays failed when the scope will
#   not stop or EVO cannot be reached, and finds a job EVO reports up to the
#   120 s reap slack older than the run. A host probe failing on a reachable
#   EVO is failed. A run started outside the repo runs its suites in the repo
#   root. A TERM or INT to a runner with a hung suite leaves no suite process,
#   stops the EVO job and replaces a previous ok with failed. No EVO, no cargo.
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
pwd -P >>"$STUB_PWD"
# A failing run still prints its figures: the exit code alone must fail it.
case " ${STUB_FAIL:-} " in *" $suite "*)
	cat "$STUB_FIX/$suite.log"
	echo "error: test failed, to rerun pass --test"
	exit 101
	;;
esac
case " ${STUB_EMPTY:-} " in *" $suite "*) echo "test result: ok. 0 passed"; exit 0 ;; esac
# A hung suite: STUB_PIDS gets this stub's pid and its sleep's, so a case
# can check that both are gone once the runner has exited.
case " ${STUB_HANG:-} " in *" $suite "*)
	sleep 30 &
	echo "$$ $!" >"$STUB_PIDS"
	wait
	;;
esac
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
if [ "$STUB_LOAD" = unreachable ]; then
	echo "ssh: connect to host evo-linux port 22: Operation timed out" >&2
	exit 255
fi
if [ -n "${STUB_HOST_FAIL:-}" ]; then
	echo "error: toolchain '1.99.0' is not installed" >&2
	exit 1
fi
printf '%s\n' "Ubuntu 26.04 LTS" "Stub CPU" 28 47 "rustc 1.99.0 (stub 2026-09-28)"
EOF
# The reap hook runs the real EVO-side reap script against a fake EVO: ps,
# pkill, kill and systemctl read and change $STUB_EVO, and log to $STUB_REAPED.
# STUB_REAP_DOWN=1 is an EVO the reap's ssh cannot reach; STUB_STUCK=1 is a
# scope that stays active through stop and SIGKILL.
cat >"$work/reap" <<EOF
#!/usr/bin/env bash
if [ -n "\${STUB_REAP_DOWN:-}" ]; then
	echo "ssh: connect to host evo-linux port 22: Operation timed out" >&2
	exit 255
fi
PATH="$work/bin:\$PATH" EVO_REAP_ROOT="\$STUB_EVO" exec "$SCRIPT_DIR/perf_weekly_evo_reap.sh" "\$@"
EOF
mkdir -p "$work/bin"
cat >"$work/bin/ps" <<'EOF'
#!/usr/bin/env bash
# `ps -eo pid=,etimes=,args= -ww` lists procs; `ps -o pid= -p N` prints N if live.
if [ "$1" = -eo ]; then awk -F'\t' '{print $1, $2, $3}' "$STUB_EVO/procs"; exit 0; fi
awk -F'\t' -v p="$4" '$1 == p {print $1}' "$STUB_EVO/procs"
EOF
cat >"$work/bin/kill" <<'EOF'
#!/usr/bin/env bash
echo "kill $*" >>"$STUB_REAPED"
awk -F'\t' -v p="$2" '$1 != p' "$STUB_EVO/procs" >"$STUB_EVO/procs.new"
mv "$STUB_EVO/procs.new" "$STUB_EVO/procs"
EOF
cat >"$work/bin/pkill" <<'EOF'
#!/usr/bin/env bash
echo "pkill $*" >>"$STUB_REAPED"
EOF
cat >"$work/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
shift # --user
case "$1" in
stop)
	echo "systemctl stop $2" >>"$STUB_REAPED"
	[ -n "${STUB_STUCK:-}" ] || { grep -vxF "$2" "$STUB_EVO/active" >"$STUB_EVO/active.new"; mv "$STUB_EVO/active.new" "$STUB_EVO/active"; }
	;;
is-active) exec grep -qxF "$3" "$STUB_EVO/active" ;;
kill) echo "systemctl kill $2 $3" >>"$STUB_REAPED" ;;
esac
exit 0
EOF
chmod +x "$work/run" "$work/load" "$work/host" "$work/reap" "$work/bin/"*

KEY="perf-wt-0123456789"
SHA="$(git -C "$SCRIPT_DIR/.." rev-parse HEAD)"
EVAL_ARGV="cargo +1.99.0 test -p trusty-memory --test recall_eval -- --include-ignored --nocapture"
OUR="evo-job-$KEY-4242-1761480000-7.scope"
OTHER="evo-job-other-wt-9876543210-5151-1761480000-9.scope"
# A fake EVO: this run's job (4242) and its queued twin (4343), another
# worktree's job (5151) and an older job with this run's key and argv (4040).
# STUB_JOB_AGE sets the age in seconds of this run's job and its twin.
fake_evo() { # $1 = dir
	mkdir -p "$1/run"
	{
		printf '4242\t%s\tbash .local/bin/evo-run --key %s --sha %s --cwd  --color never -- %s\n' "${STUB_JOB_AGE:-5}" "$KEY" "$SHA" "$EVAL_ARGV"
		printf '4343\t%s\tbash .local/bin/evo-run --key %s --sha %s --cwd  --color never -- %s\n' "${STUB_JOB_AGE:-3}" "$KEY" "$SHA" "$EVAL_ARGV"
		printf '5151\t4\tbash .local/bin/evo-run --key other-wt-9876543210 --sha %s --cwd  --color never -- %s\n' "$SHA" "$EVAL_ARGV"
		printf '4040\t99999\tbash .local/bin/evo-run --key %s --sha %s --cwd  --color never -- %s\n' "$KEY" "$SHA" "$EVAL_ARGV"
	} >"$1/procs"
	printf 'start=2026-10-26T12:00:00Z slot=1 pid=4242 key=%s args=%s\n' "$KEY" "$EVAL_ARGV" >"$1/run/$KEY.job"
	echo "$OUR" >"$1/run/$KEY.scope"
	printf '%s\n' "$OUR" "$OTHER" >"$1/active"
}

fail=0
pass() { printf 'PASS %s\n' "$1"; }
bad() {
	printf 'FAIL %s: %s\n' "$1" "$2"
	fail=1
}

# Set up one case. $1 = case name, $2 = load. Sets name, load, out, status,
# calls, reaped, pwds, pids.
prep_case() {
	name="$1" load="$2"
	out="$work/$name/out"
	status="$work/$name/status.json"
	calls="$work/$name/calls"
	reaped="$work/$name/reaped"
	pwds="$work/$name/pwds"
	pids="$work/$name/pids"
	mkdir -p "$out"
	: >"$calls"
	: >"$reaped"
	: >"$pwds"
	fake_evo "$work/$name/evo"
}
# Replace this (sub)shell with the runner and its stubs, started in CASE_CWD
# (default: here). Rest = runner flags; later flags override the defaults.
# CASE_STATUS_ENV, when set, is passed as PERF_STATUS_FILE instead of
# --status-file.
runner_exec() {
	cd "${CASE_CWD:-.}" || exit 99
	local st=(--status-file "$status")
	if [ -n "${CASE_STATUS_ENV:-}" ]; then
		st=()
		export PERF_STATUS_FILE="$CASE_STATUS_ENV"
	fi
	STUB_CALLS="$calls" STUB_FIX="$FIX" STUB_LOAD="$load" STUB_EVO="$work/$name/evo" STUB_REAPED="$reaped" \
		STUB_PWD="$pwds" STUB_PIDS="$pids" \
		PERF_RUN_CMD="$work/run" PERF_LOAD_CMD="$work/load" PERF_HOST_CMD="$work/host" \
		PERF_REAP_CMD="$work/reap" PERF_EVO_KEY="$KEY" \
		exec "$RUNNER" --out-dir "$out" ${st[@]+"${st[@]}"} --settle-s 0 \
		--baseline "$BASELINE" "$@" >"$work/$name/stdout" 2>"$work/$name/stderr"
}
# Run the runner with stubs to its end. $1 = case name, $2 = load, rest =
# runner flags. Sets rc and everything prep_case sets.
run_case() {
	prep_case "$1" "$2"
	shift 2
	rc=0
	(runner_exec "$@") || rc=$?
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

# 7-10. A timeout stops this run's EVO job and its queued twin, never another
# key's job or an older one, and stays failed whatever the cleanup did.
# timeout-reap-slack: EVO reports the job 100 s older than the Mac measured
# the run; the reap's 120 s slack still finds it (a 30 s slack did not).
# case<TAB>env<TAB>reason substrings, | separated
REAPS="timeout-reaped${TAB}STUB_NONE=1${TAB}stopped queued waiter pid 4343|stopped scope $OUR
timeout-reap-slack${TAB}STUB_JOB_AGE=100${TAB}stopped queued waiter pid 4343|stopped scope $OUR
timeout-stuck${TAB}STUB_STUCK=1${TAB}leftover: scope $OUR still active|systemctl --user kill --signal=KILL $OUR|EVO cleanup exited 1; check by hand
timeout-reap-down${TAB}STUB_REAP_DOWN=1${TAB}EVO cleanup exited 255; check by hand|evo-run --key $KEY"
want_reaped="pkill -TERM -P 4343
kill -TERM 4343
systemctl stop $OUR"
while IFS="$TAB" read -r name envset want_reason; do
	export "${envset?}"
	STUB_HANG="eval" run_case "$name" 0.5 --suite-timeout 1
	unset "${envset%%=*}"
	got="$rc $(verdict) $(ran)"
	miss=""
	old_ifs="$IFS"
	IFS="|"
	for w in $want_reason; do reason | grep -qF "$w" || miss="$w"; done
	IFS="$old_ifs"
	if [ "$got" != "20 failed eval=1" ]; then
		bad "$name" "got '$got', want '20 failed eval=1'"
	elif ! reason | grep -qF "eval1 timed out after 1s"; then
		bad "$name" "reason '$(reason)' lacks the timeout"
	elif [ -n "$miss" ]; then
		bad "$name" "reason '$(reason)' lacks '$miss'"
	elif { [ "$name" = timeout-reaped ] || [ "$name" = timeout-reap-slack ]; } &&
		[ "$(cat "$reaped")" != "$want_reaped" ]; then
		bad "$name" "reaped '$(tr '\n' '|' <"$reaped")', want only 4343 and $OUR"
	elif grep -qE '5151|4040|other-wt' "$reaped"; then
		bad "$name" "touched another job: $(tr '\n' '|' <"$reaped")"
	else
		pass "$name: exit 20, status failed, $(reason | sed 's/.*EVO cleanup: //')"
	fi
done <<EOF
$REAPS
EOF

# 11. A host probe failing on a reachable EVO (no +1.99.0) is failed, not skipped.
STUB_HOST_FAIL=1 run_case host-fails 0.5
got="$rc $(verdict) calls=$(wc -l <"$calls" | tr -d ' ')"
want_reason="host probe exited 1 on a reachable EVO: error: toolchain '1.99.0' is not installed"
if [ "$got" != "20 failed calls=0" ]; then
	bad "host probe fails" "got '$got', want '20 failed calls=0'"
elif [ "$(reason)" != "$want_reason" ]; then
	bad "host probe fails" "reason '$(reason)', want '$want_reason'"
else
	pass "host probe fails on a reachable EVO: exit 20, failed ($(reason))"
fi

# 12-14. Busy or unreachable EVO: skipped, exit 30, no suite runs at all.
for load in 2.00 3.5 unreachable; do
	run_case "load-$load" "$load"
	got="$rc $(verdict) calls=$(wc -l <"$calls" | tr -d ' ')"
	if [ "$got" = "30 skipped calls=0" ] && [ ! -e "$out/envelope.json" ]; then
		pass "load $load: exit 30, skipped ($(reason)), no suite run"
	else
		bad "load $load" "got '$got', want '30 skipped calls=0'"
	fi
done
# An unreachable EVO is skipped on the host probe's own evidence.
want_reason="EVO unreachable: host probe exited 255 and no load reading (ssh: connect to host evo-linux port 22: Operation timed out)"
if [ "$(reason)" = "$want_reason" ]; then
	pass "unreachable EVO: reason names the probe exit and the missing load"
else
	bad "unreachable reason" "got '$(reason)', want '$want_reason'"
fi

# 15. Started outside the repo: every suite runs in the repo root (where
# `evo key` is taken), and relative --out-dir and --status-file still mean
# the caller's directory.
repo="$(cd "$SCRIPT_DIR/.." && pwd -P)"
elsewhere="$work/elsewhere"
mkdir -p "$elsewhere"
CASE_CWD="$elsewhere" run_case outside 0.5 --out-dir rel-out --status-file rel/status.json
status="$elsewhere/rel/status.json"
got="$rc $(verdict) $(ran)"
want="0 ok concurrent=3 eval=1 profile=5 slice=3"
wrong_cwd="$({ grep -vxF "$repo" "$pwds" || true; } | sort -u | tr '\n' ' ')"
if [ "$got" != "$want" ]; then
	bad "outside cwd" "got '$got', want '$want' ($(tail -n 1 "$work/outside/stderr"))"
elif [ -n "$wrong_cwd" ] || [ ! -s "$pwds" ]; then
	bad "outside cwd" "suites ran in '${wrong_cwd:-nowhere}', want $repo"
elif [ ! -f "$elsewhere/rel-out/envelope.json" ]; then
	bad "outside cwd" "no envelope under the caller's rel-out"
else
	pass "outside cwd: suites ran in the repo root; relative out-dir and status file under the caller's cwd"
fi
# A relative PERF_STATUS_FILE also means the caller's cwd, not the repo root
# (where perf_status_write.sh refuses a path git does not ignore).
CASE_CWD="$elsewhere" CASE_STATUS_ENV=rel-env-status/status.json run_case outside-env 0.5 --out-dir rel-out-env
status="$elsewhere/rel-env-status/status.json"
rmdir "$repo/rel-env-status" 2>/dev/null || true
got="$rc $(verdict)"
if [ "$got" = "0 ok" ]; then
	pass "outside cwd: relative PERF_STATUS_FILE lands under the caller's cwd"
else
	bad "outside cwd env" "got '$got', want '0 ok' ($(tail -n 1 "$work/outside-env/stderr"))"
fi

# 16-17. A TERM or INT mid-suite stops the suite's processes, stops its EVO
# job, and leaves status failed, never the previous ok. set -m starts the
# runner in its own process group with INT not ignored, as a terminal would.
for sig in TERM INT; do
	prep_case "interrupt-$sig" 0.5
	printf '{"schema":1,"tools":{"trusty-memory":{"verdict":"ok","reason":null}}}\n' >"$status"
	set -m
	(STUB_HANG="eval" runner_exec --suite-timeout 60) &
	runner=$!
	set +m
	i=0
	while [ ! -s "$pids" ] && [ "$i" -lt 100 ]; do sleep 0.1; i=$((i + 1)); done
	kill -"$sig" "$runner"
	# Bound the wait: a runner still up after 30 s is killed and fails below.
	i=0
	while ps -o stat= -p "$runner" 2>/dev/null | grep -qv '^Z' && [ "$i" -lt 300 ]; do
		sleep 0.1
		i=$((i + 1))
	done
	kill -KILL "$runner" 2>/dev/null || true
	rc=0
	wait "$runner" || rc=$?
	live=""
	stub_pid="" sleep_pid=""
	read -r stub_pid sleep_pid <"$pids" || true
	for p in $stub_pid $sleep_pid; do ps -p "$p" >/dev/null 2>&1 && live="$live $p"; done
	# shellcheck disable=SC2086 # one pid per word; never leave a stub behind
	[ -z "$live" ] || kill -KILL $live 2>/dev/null || true
	want_reason="interrupted by SIG$sig during eval1; EVO cleanup: stopped queued waiter pid 4343; stopped scope $OUR"
	if [ ! -s "$pids" ]; then
		bad "interrupt $sig" "the hung suite never started"
	elif [ -n "$live" ]; then
		bad "interrupt $sig" "suite processes still running after the runner exited:$live (exit $rc, verdict $(verdict))"
	elif [ "$rc $(verdict)" != "20 failed" ]; then
		bad "interrupt $sig" "got '$rc $(verdict)', want '20 failed'"
	elif [ "$(reason)" != "$want_reason" ]; then
		bad "interrupt $sig" "reason '$(reason)', want '$want_reason'"
	else
		pass "interrupt $sig: suite processes gone, EVO job stopped, exit 20, status failed"
	fi
done

if [ "$fail" -ne 0 ]; then
	echo "perf_weekly_memory_selftest: FAILED"
	exit 1
fi
echo "perf_weekly_memory_selftest: all cases passed"
