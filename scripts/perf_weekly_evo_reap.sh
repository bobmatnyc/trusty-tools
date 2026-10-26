#!/usr/bin/env bash
#
# perf_weekly_evo_reap.sh — stop the EVO job a timed-out weekly perf run left
# behind (issue #10210, phase 2). Runs ON EVO: scripts/perf_weekly_memory.sh
# sends it over `ssh evo-linux bash -s -- ...`.
#
# Why: the runner's timeout kills only the Mac side (evo-gate-log, evo, ssh).
#   On EVO, mpm-utils `evo-run` keeps the worktree-key lock and one of the
#   EVO_MAX_JOBS slots, a SIGTERM to it lets the job finish, and a waiter
#   still queued on a lock starts cargo later with nobody reading it. Only
#   `systemctl --user stop <evo-job-*.scope>` stops a running job.
# What: KEY is the worktree key (`evo key`), MAX_AGE the seconds since the run
#   started (plus slack), SHA the commit evo sent, ARGS the cargo argv after
#   `cargo`. It acts only on an `evo-run --key KEY --sha SHA ... -- cargo ARGS`
#   process no older than MAX_AGE, so another key, commit, argv or an older
#   job is never touched:
#     - a waiter (no job record of its own): SIGTERM to it and its child
#       (flock or sleep), SIGKILL after 5 s; it has started no cargo yet;
#     - the running job: the one whose pid is in ~/evo/run/KEY.job with this
#       argv; its scope is the unit in ~/evo/run/KEY.scope, accepted only as
#       `evo-job-<KEY, unit-safe>-<pid>-*.scope` (evo-run's own name). It
#       runs `systemctl --user stop`, then `kill --signal=KILL` if still
#       active. evo-run then frees its locks itself.
#   One line per action on stdout; a `leftover:` line names what it could not
#   stop and the command to stop it by hand.
#
# Usage: perf_weekly_evo_reap.sh KEY MAX_AGE SHA -- ARGS...
# Exit: 0 nothing of this run is left, 1 a leftover, 2 bad usage.
# EVO_REAP_ROOT moves ~/evo (tests). ps, pkill, kill and systemctl come from PATH.
#
# Test: scripts/perf_weekly_memory_selftest.sh, cases timeout, timeout-stuck.

set -u
[ "$#" -ge 4 ] && [ "$4" = -- ] || { echo "usage: perf_weekly_evo_reap.sh KEY MAX_AGE SHA -- ARGS..." >&2; exit 2; }
key=$1 max_age=$2 sha=$3
shift 4
case "$key" in '' | *[!A-Za-z0-9_.-]*) echo "bad KEY: '$key'" >&2; exit 2 ;; esac
case "$max_age" in '' | *[!0-9]*) echo "bad MAX_AGE: '$max_age'" >&2; exit 2 ;; esac
want="cargo $*"
run="${EVO_REAP_ROOT:-$HOME/evo}/run"
safe="$(printf '%s' "$key" | LC_ALL=C tr -c 'A-Za-z0-9_.-' '_' | cut -c1-64)"
left=0

alive() { [ -n "$(ps -o pid= -p "$1" 2>/dev/null)" ]; }
gone_within() { # $1 = pid, $2 = seconds
	local i=0
	while [ "$i" -lt "$2" ]; do alive "$1" || return 0; sleep 1; i=$((i + 1)); done
	! alive "$1"
}

# This run's evo-run processes: pid per line.
ours() {
	ps -eo pid=,etimes=,args= -ww 2>/dev/null | while read -r pid et args; do
		case "$et" in '' | *[!0-9]*) continue ;; esac
		[ "$et" -le "$max_age" ] || continue
		case "$args" in *"evo-run --key $key --sha $sha "*" -- $want") echo "$pid" ;; esac
	done
}

jobpid=""
rec="$(cat "$run/$key.job" 2>/dev/null)"
case "$rec" in *" key=$key args=$want")
	jobpid="${rec#* pid=}"
	jobpid="${jobpid%% *}"
	;;
esac

pids="$(ours)"
running=""
for pid in $pids; do
	if [ "$pid" = "$jobpid" ]; then
		running="$pid"
		continue
	fi
	# A waiter: bash defers its TERM trap until the child exits, so the
	# child goes too. It holds no job, so a SIGKILL leaves nothing behind.
	pkill -TERM -P "$pid" 2>/dev/null
	env kill -TERM "$pid" 2>/dev/null
	if ! gone_within "$pid" 5; then
		pkill -KILL -P "$pid" 2>/dev/null
		env kill -KILL "$pid" 2>/dev/null
	fi
	if gone_within "$pid" 2; then
		echo "stopped queued waiter pid $pid"
	else
		echo "leftover: queued evo-run pid $pid; stop it by hand: ssh evo-linux 'kill -KILL $pid'"
		left=1
	fi
done

if [ -n "$running" ]; then
	unit="$(cat "$run/$key.scope" 2>/dev/null)"
	case "$unit" in
	"evo-job-$safe-$running-"*.scope)
		systemctl --user stop "$unit" >/dev/null 2>&1
		if systemctl --user is-active --quiet "$unit" 2>/dev/null; then
			systemctl --user kill --signal=KILL "$unit" >/dev/null 2>&1
			sleep 2
		fi
		if systemctl --user is-active --quiet "$unit" 2>/dev/null; then
			echo "leftover: scope $unit still active; stop it by hand: ssh evo-linux 'systemctl --user kill --signal=KILL $unit'"
			left=1
		else
			echo "stopped scope $unit"
		fi
		;;
	*)
		echo "leftover: evo-run pid $running runs with no scope of its own (marker '$unit'); stop it by hand: ssh evo-linux 'pkill -TERM -P $running'"
		left=1
		;;
	esac
fi

[ -n "$pids" ] || echo "no job of this run on EVO"
exit "$left"
