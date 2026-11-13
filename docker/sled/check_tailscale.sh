#!/usr/bin/env bash
# check_tailscale.sh [APP] [SIDECAR] — DOC-90 section 9 V5 (Tailscale control
# isolation, IN-7). Run on the docker host, from the repo checkout, with the
# app and the tailscaled sidecar running:
#   docker/sled/check_tailscale.sh tm-trusty-cloud-sled tm-trusty-cloud-sled-tailscale
# Proves live that no process running as the app's `sled` uid can drive
# tailscaled: as `sled`, `tailscale up`, `set --ssh`, `funnel` and `nc` each
# fail to reach the daemon, the socket is absent and unwritable in the app, and
# the sidecar's tailscaled runs as another uid with a 0700 socket directory.
# Also reads the sidecar's prefs: SSH off and no Funnel (IN-3). Prints PASS or
# FAIL per row and exits non-zero on any FAIL. Fail-closed: a stopped
# container, a missing binary or an unreadable value is a FAIL, never a PASS.
# It sends an invalid auth key only; it prints no secret. Refs #11156
# Test: scripts/check_sled_tailscale_isolation.sh is the offline, static half.
set -uo pipefail

APP="${1:-tm-trusty-cloud-sled}"
SIDE="${2:-tm-trusty-cloud-sled-tailscale}"
SOCK=/var/run/tailscale/tailscaled.sock
FAILS=0

# row NAME OK [DETAIL] — print PASS/FAIL and count failures.
row() {
    if [ "$2" = yes ]; then
        echo "PASS $1"
    else
        echo "FAIL $1${3:+ -- $3}"
        FAILS=$((FAILS + 1))
    fi
}
die() {
    echo "FAIL $1"
    echo "RESULT: V5 stopped, $((FAILS + 1)) row(s) FAILED"
    exit 1
}
running() { [ "$(docker inspect -f '{{.State.Running}}' "$1" 2>/dev/null)" = true ]; }
app() { docker exec -u sled "${APP}" "$@"; }
side() { docker exec "${SIDE}" "$@"; }
one_line() { tr '\n' ' ' | cut -c1-200; }

running "${APP}" || die "app container ${APP} is not running"
running "${SIDE}" || die "sidecar container ${SIDE} is not running"

# 1. The sidecar shares the app's network namespace, and nothing else.
app_id="$(docker inspect -f '{{.Id}}' "${APP}" 2>/dev/null)"
mode="$(docker inspect -f '{{.HostConfig.NetworkMode}}' "${SIDE}" 2>/dev/null)"
ok=no
case "${mode}" in "container:${app_id}"|"container:${APP}") ok=yes ;; esac
row "sidecar network mode is the app's namespace" "${ok}" "${mode}"
pidmode="$(docker inspect -f '{{.HostConfig.PidMode}}' "${SIDE}" 2>/dev/null)"
ok=no; [ -z "${pidmode}" ] && ok=yes
row "sidecar has its own PID namespace" "${ok}" "${pidmode}"

# 2. Distinct uids: tailscaled (sidecar PID 1) is neither root nor sled.
sled_uid="$(app id -u 2>/dev/null)"
ts_uid="$(side stat -c %u /proc/1 2>/dev/null)"
ts_comm="$(side cat /proc/1/comm 2>/dev/null)"
ok=no
[ "${ts_comm}" = tailscaled ] && [ -n "${sled_uid}" ] && [ -n "${ts_uid}" ] \
    && [ "${ts_uid}" != 0 ] && [ "${ts_uid}" != "${sled_uid}" ] && ok=yes
row "sidecar tailscaled uid (${ts_uid:-?}) is not root and not sled (${sled_uid:-?})" "${ok}" "pid1=${ts_comm:-?}"

# 3. The sidecar socket and its directory belong to that uid; the dir is 0700.
d="$(side stat -c '%u %a' /var/run/tailscale 2>/dev/null)"
ok=no; [ -n "${ts_uid}" ] && [ "${d}" = "${ts_uid} 700" ] && ok=yes
row "sidecar /var/run/tailscale is owned by ${ts_uid:-?}, mode 700" "${ok}" "${d:-unreadable}"
d="$(side stat -c '%u %F' "${SOCK}" 2>/dev/null)"
ok=no; [ -n "${ts_uid}" ] && [ "${d}" = "${ts_uid} socket" ] && ok=yes
row "sidecar ${SOCK} is a socket owned by ${ts_uid:-?}" "${ok}" "${d:-unreadable}"

# 4. Inside the app: no tailscaled, no reachable socket.
procs="$(app sh -c 'cat /proc/[0-9]*/comm 2>/dev/null' 2>/dev/null)"; rc=$?
ok=no; [ "${rc}" -eq 0 ] && [ -n "${procs}" ] && ! grep -qx tailscaled <<< "${procs}" && ok=yes
row "no tailscaled process in the app container" "${ok}" "rc=${rc}"
ok=no; app sh -c 'command -v tailscaled' > /dev/null 2>&1 || ok=yes
row "no tailscaled binary on the app's PATH" "${ok}"
ok=no
app sh -c "test -e ${SOCK} || test -w /var/run/tailscale || test -w /run/tailscale" \
    > /dev/null 2>&1 || ok=yes
row "app: ${SOCK} is absent and its directory is not writable by sled" "${ok}"
unix="$(app cat /proc/net/unix 2>/dev/null)"; rc=$?
ok=no; [ "${rc}" -eq 0 ] && ! grep -qE ' @[^ ]*tailscale' <<< "${unix}" && ok=yes
row "app: no abstract unix socket names tailscale" "${ok}" "rc=${rc}"
# The shared network namespace exposes any TCP listener the sidecar opens
# (SOCKS5, HTTP proxy, debug) to the app. State 0A is LISTEN; field 8 is uid.
tcp="$(app cat /proc/net/tcp /proc/net/tcp6 2>/dev/null)"; rc=$?
ok=no; detail="rc=${rc}"
if [ "${rc}" -eq 0 ] && [ -n "${tcp}" ] && [ -n "${ts_uid}" ]; then
    l="$(awk -v u="${ts_uid}" '$4 == "0A" && $8 == u {print $2}' <<< "${tcp}" | tr '\n' ';')"
    [ -z "${l}" ] && ok=yes
    detail="listeners owned by ${ts_uid}: ${l:-none}"
fi
row "app netns: no TCP LISTEN socket owned by the sidecar uid ${ts_uid:-?}" "${ok}" "${detail}"

# 5. As sled, every state-changing command fails to reach a daemon. A daemon
# that answered and refused (an invalid key, a usage error) is a FAIL: the
# point is that sled reaches no daemon at all.
ok=no; app sh -c 'command -v tailscale' > /dev/null 2>&1 && ok=yes
row "tailscale client present in the app (needed for the rows below)" "${ok}"
NOREACH='failed to connect to local [Tt]ailscale|no such file or directory|permission denied|connection refused'
probes=("up --auth-key=tskey-auth-sledcheck-invalid --timeout=5s"
        "set --ssh"
        "funnel --bg 7788"
        "nc 100.100.100.100 80")
for p in "${probes[@]}"; do
    # shellcheck disable=SC2086 # the probe is a word list on purpose
    out="$(app timeout 20 tailscale ${p} < /dev/null 2>&1)"; rc=$?
    ok=no
    [ "${rc}" -ne 0 ] && grep -qiE "${NOREACH}" <<< "${out}" && ok=yes
    row "as sled, 'tailscale ${p%% *}' cannot reach tailscaled" "${ok}" "rc=${rc} $(one_line <<< "${out}")"
done

# 6. The sidecar's own state: SSH off and no Funnel (IN-3).
prefs="$(side tailscale debug prefs 2>/dev/null)"; rc=$?
ok=no
[ "${rc}" -eq 0 ] && grep -qE '"RunSSH":[[:space:]]*false' <<< "${prefs}" && ok=yes
row "sidecar prefs: RunSSH is false" "${ok}" "rc=${rc}"
fs="$(side tailscale funnel status --json 2>/dev/null)"; rc=$?
ok=no
[ "${rc}" -eq 0 ] && ! grep -q '"AllowFunnel"' <<< "${fs}" && ok=yes
row "sidecar: tailscale funnel status shows no funnel" "${ok}" "rc=${rc} $(one_line <<< "${fs}")"

if [ "${FAILS}" -gt 0 ]; then
    echo "RESULT: V5 ${FAILS} row(s) FAILED"
    exit 1
fi
echo "RESULT: V5 PASSED"
