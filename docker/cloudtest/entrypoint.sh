#!/usr/bin/env bash
# Supervisor: start the three daemons in the background, log each to
# /data/logs/<name>.log, forward TERM/INT, exit when any daemon exits.
# Refs #11054
set -uo pipefail

mkdir -p "${HOME}" "${XDG_DATA_HOME}" /data/logs /data/work
# Nothing runs yet, so a socket left by an earlier container is stale.
rm -f "${XDG_DATA_HOME}/trusty-memory/trusty-memory.sock" \
      "${XDG_DATA_HOME}/trusty-search/trusty-search.sock"

# See https://github.com/bobmatnyc/trusty-tools/issues/11054: pid reuse across container restarts
lock=/data/xdg/trusty-search/daemon.lock
if [ -e "${lock}" ]; then
    rm -f "${lock}"
    echo "$(date -u +%FT%TZ) removed stale lock ${lock}" >> /data/logs/supervisor.log
fi

# Rung 2: the Claude credential is read only by claude-run, per invocation. The
# daemons below never see it. This line records presence, not the value.
if [ -e /run/secrets/claude.env ]; then
    echo "$(date -u +%FT%TZ) claude credential file present (not read here)" >> /data/logs/supervisor.log
fi

pids=()
start() {
    local name="$1"; shift
    echo "$(date -u +%FT%TZ) starting ${name}: $*" >> /data/logs/supervisor.log
    "$@" >> "/data/logs/${name}.log" 2>&1 &
    pids+=("$!")
}

stop_all() {
    trap '' TERM INT
    echo "$(date -u +%FT%TZ) stopping daemons" >> /data/logs/supervisor.log
    kill -TERM "${pids[@]}" 2>/dev/null || true
    wait 2>/dev/null
}
trap 'stop_all; exit 0' TERM INT

start tm tm daemon
start trusty-memory trusty-memory serve --foreground
start trusty-search trusty-search start --foreground

# Returns when the first daemon exits; the container then stops so the failure
# is visible in `docker ps`.
wait -n
rc=$?
echo "$(date -u +%FT%TZ) a daemon exited (rc=${rc}); shutting down" >> /data/logs/supervisor.log
stop_all
exit 1
