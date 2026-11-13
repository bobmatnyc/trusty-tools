#!/usr/bin/env bash
# Supervisor: start the three trusty daemons in the background, log each to
# /data/logs/<name>.log, forward TERM/INT, exit when any exits. tailscaled is
# not started here: it runs in the tailscale-sidecar container as its own uid,
# so no process in this container can drive it (#11156 B2).
# Derived from docker/cloudtest. Refs #11054 #11073
# Test: scripts/check_sled_tailscale_isolation.sh asserts this starts no tailscaled.
set -uo pipefail

# Owner-only files: the exercise logs hold command output.
umask 077
mkdir -p "${HOME}" "${XDG_DATA_HOME}" /data/logs /data/work
chmod 0700 /data/logs
# #11156 B2: tailscaled state from an image that ran it as this uid. A node key
# here is readable by every agent; the sidecar keeps its own state volume.
if [ -e /data/tailscale ]; then
    rm -rf /data/tailscale
    echo "$(date -u +%FT%TZ) removed /data/tailscale (Tailscale state lives in the sidecar)" >> /data/logs/supervisor.log
fi
# Logs left by an earlier container on the same volume.
find /data/logs -type f -exec chmod 0600 {} +
# Nothing runs yet, so a socket left by an earlier container is stale.
rm -f "${XDG_DATA_HOME}/trusty-memory/trusty-memory.sock" \
      "${XDG_DATA_HOME}/trusty-search/trusty-search.sock"

# See https://github.com/bobmatnyc/trusty-tools/issues/11054: pid reuse across container restarts
lock=/data/xdg/trusty-search/daemon.lock
if [ -e "${lock}" ]; then
    rm -f "${lock}"
    echo "$(date -u +%FT%TZ) removed stale lock ${lock}" >> /data/logs/supervisor.log
fi

# Secrets are read only by claude-run and gh-token-run, per invocation. The
# daemons below never see them. These lines record presence, not the value.
for s in /run/secrets/claude.env /run/secrets/github-token /run/secrets/trusty-console-token; do
    if [ -e "${s}" ]; then
        echo "$(date -u +%FT%TZ) secret file ${s} present (not read here)" >> /data/logs/supervisor.log
    fi
done

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
