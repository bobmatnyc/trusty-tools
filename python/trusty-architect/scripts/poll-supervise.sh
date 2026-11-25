#!/usr/bin/env bash
# poll-supervise.sh [poller args] — run fleet-poll.py as a child and restart it when it hangs.
# Why: a hung poller sat in select/poll for hours with stale state and went unnoticed (see #10571).
# A hung process cannot restart itself; this wrapper is the
# independent watcher. start-fleet-poll.sh does not start it yet; wiring it in as the
# poller session command is a follow-up (#10571).
# What: starts the poller, then every POLL_SUPERVISE_TICK s checks the newest of the state-file
# mtime and the child start time. Older than POLL_STALE_S (600) => SIGTERM, then SIGKILL, to the
# child pid only, after `ps` confirms the pid's command line still matches POLL_MATCH. A child that
# exited is restarted after 5 s. Each restart is one line in inbox/poll.log.
# #11369: the interval is poll_interval_s in local/architect.json (read with architect_config.py, as fleet-poll.py
# reads it), or a --interval N passed through; the start line logs it. POLL_STALE_S defaults to twice the interval,
# at least 600, so a slow cadence is never taken for a hang. A config that does not load is logged and the default
# 600 s stands; fleet-poll.py then refuses the same config itself.
# Test: tests/test_poll_supervise.py, tests/test_poll_cadence.py
ROOT=${ARCHITECT_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}
# #10571 S2: siblings by literal path so the pm-guard resolves them
HERE="$(cd "$(dirname "$0")" && pwd)"
STATE=${POLL_STATE_FILE:-$ROOT/inbox/poll-state.json}
LOG=${POLL_LOG:-$ROOT/inbox/poll.log}
interval=""; prev=""
for a in "$@"; do [ "$prev" = "--interval" ] && interval="$a"; prev="$a"; done
cfg_err=""
[ -n "$interval" ] || interval=$(python3 "$HERE/architect_config.py" poll-interval 2>&1) || { cfg_err="$interval"; interval=""; }
case "$interval" in ''|*[!0-9]*) STALE_DEFAULT=600 ;; *) STALE_DEFAULT=$((interval * 2 > 600 ? interval * 2 : 600)) ;; esac
STALE=${POLL_STALE_S:-$STALE_DEFAULT}
TICK=${POLL_SUPERVISE_TICK:-30}
MATCH=${POLL_MATCH:-fleet-poll.py}
MAX_TICKS=${POLL_SUPERVISE_MAX_TICKS:-0}   # 0 = run forever (tests bound it)
log(){ echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) supervisor $1" >> "$LOG"; }
mtime(){ stat -f %m "$1" 2>/dev/null || stat -c %Y "$1" 2>/dev/null || echo 0; }
start(){ if [ -n "${POLL_CMD:-}" ]; then $POLL_CMD & else python3 "$HERE/fleet-poll.py" "$@" & fi
  pid=$!; started=$(date +%s); }
ours(){ [ -n "${pid:-}" ] && ps -p "$pid" -o command= 2>/dev/null | grep -qF -- "$MATCH"; }
stop_child(){ ours || return 0; kill -TERM "$pid" 2>/dev/null
  for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$pid" 2>/dev/null || return 0; sleep 1; done
  ours && kill -KILL "$pid" 2>/dev/null; return 0; }
trap 'stop_child; exit 0' TERM INT
[ -z "$cfg_err" ] || log "config: $cfg_err"
start "$@"; log "started poller pid=$pid interval=${interval:-unknown}s stale=${STALE}s"
n=0
while :; do
  sleep "$TICK" & wait $!
  n=$((n+1)); [ "$MAX_TICKS" -gt 0 ] && [ "$n" -gt "$MAX_TICKS" ] && { stop_child; exit 0; }
  now=$(date +%s); beat=$(mtime "$STATE"); [ "$started" -gt "$beat" ] && beat=$started
  if ! kill -0 "$pid" 2>/dev/null; then
    log "poller pid=$pid exited; restarting"; sleep 5; start "$@"; log "started poller pid=$pid"
  elif [ $((now-beat)) -gt "$STALE" ]; then
    if ours; then log "poller pid=$pid state stale $((now-beat))s (> ${STALE}s); restarting"; stop_child
      start "$@"; log "started poller pid=$pid"
    else log "pid=$pid no longer matches '$MATCH'; not signalled"; start "$@"; log "started poller pid=$pid"; fi
  fi
done
