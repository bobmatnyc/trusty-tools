#!/usr/bin/env bash
# launch.sh --tag SHA [--name PREFIX] [--state DIR] [--models DIR] [--check-only]
#
# Why: DOC-90 §12 — a sled reaches the internet only through the egress proxy,
#   and this script is the only supported way to start one. Hand-run `docker
#   run` lines drift: one `-p`, one `--network bridge` or one `-e TOKEN=` and
#   the boundary is gone with nothing to say so (#11156).
# What: in order — require docker >= SLED_MIN_DOCKER and jq; check the secret
#   files' host mode and owner; create the --internal network (IPv6 off) on the
#   subnet read from proxy/squid.conf; start the proxy hardened on that network
#   plus the default bridge; start the app hardened on the internal network
#   only, with no published port; start the tailscale sidecar when its image
#   exists; inspect everything and REFUSE on any boundary break; then run
#   check.sh, exercise.sh, check_v1.sh (V1) and check_tailscale.sh (V5) when
#   present, and print READY, naming the checks run, only when all pass. Any
#   refusal stops what this run started and exits 1. An inspect error is a
#   refusal, never a pass. --check-only re-runs the inspection of a running
#   sled, for example to find a network attached after launch. It acts only on
#   a container carrying the trusty.sled.launch label launch sets; a refusal
#   stops the app, and an unlabelled target is refused and left running.
# Test: scripts/check_sled_launch.sh (stub docker, one case per refusal).
#
# Names: app PREFIX, proxy PREFIX-egress, network PREFIX-internal, sidecar
# PREFIX-tailscale (default PREFIX tm-trusty-cloud-sled). Images keep their
# fixed names, tagged SHA. The proxy also answers as tm-trusty-cloud-sled-egress
# on the internal network, the name the app image's HTTPS_PROXY uses.
# Env: SLED_UID/SLED_GID (default: yours; must equal the image build args),
# SLED_CHECK_TRIES (3) and SLED_CHECK_PAUSE (30 s) for the stack check.
# Refs #11156
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR source=launch_lib.sh
. "${HERE}/launch_lib.sh"

PREFIX=tm-trusty-cloud-sled
TAG=""; STATE=""; MODELS="${HOME}/.cache/fastembed"; CHECK_ONLY=no
while [ $# -gt 0 ]; do
    case "$1" in
        --tag) TAG="${2:-}"; shift 2 ;;
        --name) PREFIX="${2:-}"; shift 2 ;;
        --state) STATE="${2:-}"; shift 2 ;;
        --models) MODELS="${2:-}"; shift 2 ;;
        --check-only) CHECK_ONLY=yes; shift ;;
        *) echo "usage: launch.sh --tag SHA [--name PREFIX] [--state DIR] [--models DIR] [--check-only]" >&2; exit 2 ;;
    esac
done
case "${PREFIX}" in ''|*[!a-z0-9-]*) echo "launch: bad --name" >&2; exit 2 ;; esac
APP="${PREFIX}"; PROXY="${PREFIX}-egress"; NET="${PREFIX}-internal"; SIDE="${PREFIX}-tailscale"
PROXY_ALIAS=tm-trusty-cloud-sled-egress
STATE="${STATE:-${HOME}/sled-state/${PREFIX}}"
SLED_UID="${SLED_UID:-$(id -u)}"; SLED_GID="${SLED_GID:-$(id -g)}"
TRIES="${SLED_CHECK_TRIES:-3}"; PAUSE="${SLED_CHECK_PAUSE:-30}"
for v in "SLED_UID=${SLED_UID}" "SLED_GID=${SLED_GID}" "SLED_CHECK_TRIES=${TRIES}" "SLED_CHECK_PAUSE=${PAUSE}"; do
    case "${v#*=}" in ''|*[!0-9]*) echo "launch: ${v%%=*} must be a whole number" >&2; exit 2 ;; esac
done
[ "${TRIES}" -ge 1 ] || { echo "launch: SLED_CHECK_TRIES must be at least 1" >&2; exit 2; }
V5="${HERE}/check_tailscale.sh"
# Every container launch starts carries this label; --check-only acts on no other.
LABEL="trusty.sled.launch"
STARTED=""
# started NAME — add NAME to the stop list once its create/run succeeded.
started() { STARTED="$1 ${STARTED}"; }
# labelled NAME — NAME exists and carries LABEL=PREFIX.
labelled() {
    [ "$(docker container inspect "$1" 2>/dev/null \
        | jq -r --arg k "${LABEL}" '.[0].Config.Labels[$k] // empty' 2>/dev/null)" = "${PREFIX}" ]
}

# refuse LINES — print the refusals, stop what this run started (or the app on
# --check-only), exit 1.
refuse() {
    printf '%s\n' "$1"
    local c
    for c in ${STARTED}; do docker stop "$c" > /dev/null 2>&1 && echo "launch: stopped ${c}"; done
    echo "RESULT: REFUSED, the sled is not ready"
    exit 1
}
need() { command -v "$1" > /dev/null 2>&1 || refuse "REFUSE host: $1 is not installed"; }

need docker; need jq
ver="$(docker version --format '{{.Server.Version}}' 2>/dev/null)"
version_ge "${ver}" "${SLED_MIN_DOCKER}" \
    || refuse "REFUSE host: docker engine '${ver:-unreadable}' is older than ${SLED_MIN_DOCKER}"
SUBNET="$(squid_subnet "${HERE}/proxy/squid.conf")" \
    || refuse "REFUSE config: proxy/squid.conf must hold exactly one 'acl internal_net src'"

# Secrets present on the host decide the app's mount allowlist.
SECRETS="github-token trusty-console-token"
[ -e "${STATE}/secrets/claude.env" ] && SECRETS="${SECRETS} claude.env"

# app_allow — the app's mount allowlist, "<dest> <source> <rw>" per line.
app_allow() {
    local s
    echo "/data ${STATE}/state true"
    echo "/models/fastembed ${MODELS} false"
    for s in ${SECRETS}; do echo "/run/secrets/${s} ${STATE}/secrets/${s} false"; done
}

# inspect_one NAME — the container's inspect JSON, or a refusal on stdout+rc 1.
inspect_one() { docker container inspect "$1" 2>/dev/null || { echo "REFUSE inspect: docker container inspect $1 failed"; return 1; }; }

# inspect_all SIDECAR(yes|no) — every refusal line for the running sled.
inspect_all() {
    local j app_tmpfs="/tmp /var/tmp" app_id
    [ "$1" = no ] && app_tmpfs="${app_tmpfs} /var/run/tailscale"
    if j="$(docker network inspect "${NET}" 2>/dev/null)"; then
        check_network_json "$j" "${SUBNET}" "${APP} ${PROXY}"
    else
        echo "REFUSE inspect: docker network inspect ${NET} failed"
    fi
    if j="$(inspect_one "${PROXY}")"; then
        check_running "$j"; check_ports "$j"; check_networks "$j" "${NET}" "${NET} bridge"
        check_privileged "$j"; check_caps "$j" "${SLED_PROXY_CAPS}"; check_namespaces "$j" ""
        check_hardening "$j" yes; check_mounts "$j" "" "/var/log/squid /etc/squid/profile.d /run /var/spool/squid /var/cache/squid /tmp"
        check_env "$j"
    else echo "$j"; fi
    if j="$(inspect_one "${APP}")"; then
        app_id="$(printf '%s' "$j" | jq -r '.[0].Id // empty' 2>/dev/null)"
        check_running "$j"; check_ports "$j"; check_networks "$j" "${NET}" "${NET}"
        check_privileged "$j"; check_caps "$j" ""; check_namespaces "$j" ""
        check_hardening "$j" yes; check_mounts "$j" "$(app_allow)" "${app_tmpfs}"
        check_env "$j"
    else echo "$j"; return; fi
    [ "$1" = yes ] || return 0
    local mode="container:${APP}${app_id:+|container:${app_id}}"
    if j="$(inspect_one "${SIDE}")"; then
        check_running "$j"; check_ports "$j"; check_networks "$j" "${mode}" ""
        check_privileged "$j"; check_caps "$j" ""; check_namespaces "$j" "${mode}"
        check_hardening "$j" no; check_mounts "$j" "/var/lib/tailscale ${SIDE} true" ""
        check_env "$j"
    else echo "$j"; fi
}

if [ "${CHECK_ONLY}" = yes ]; then
    # Refuse a target launch did not start, and leave it running.
    labelled "${APP}" || refuse "REFUSE target: ${APP} does not carry ${LABEL}=${PREFIX}; nothing stopped"
    started "${APP}"
    labelled "${SIDE}" && started "${SIDE}"
    side=no
    if docker container inspect "${SIDE}" > /dev/null 2>&1; then side=yes
    elif [ -e "${V5}" ]; then refuse "REFUSE sidecar: ${SIDE} is not running and V5 needs it"; fi
    out="$(inspect_all "${side}")"
    [ -z "${out}" ] || refuse "${out}"
    echo "RESULT: CHECK OK, ${APP} holds the enforced topology"
    exit 0
fi

[ -n "${TAG}" ] || { echo "launch: --tag is required" >&2; exit 2; }
# Nothing of this sled may exist yet: launch never adopts a hand-made one.
for c in "${APP}" "${PROXY}" "${SIDE}"; do
    docker container inspect "$c" > /dev/null 2>&1 && refuse "REFUSE exists: container ${c}; tear it down first"
done
docker network inspect "${NET}" > /dev/null 2>&1 && refuse "REFUSE exists: network ${NET}; tear it down first"

out="$(check_private "${STATE}" "${SLED_UID}" d; check_private "${STATE}/state" "${SLED_UID}" d
       check_private "${STATE}/secrets" "${SLED_UID}" d
       for s in ${SECRETS}; do check_private "${STATE}/secrets/${s}" "${SLED_UID}" f; done)"
[ -z "${out}" ] || refuse "${out}"
[ -d "${MODELS}" ] && [ ! -L "${MODELS}" ] || refuse "REFUSE mounts: model cache ${MODELS} is not a directory"

IMG_APP="tm-trusty-cloud-sled:${TAG}"; IMG_PROXY="tm-trusty-cloud-sled-egress:${TAG}"
IMG_SIDE="tm-trusty-cloud-sled-tailscale:${TAG}"
docker image inspect "${IMG_APP}" > /dev/null 2>&1 || refuse "REFUSE image: ${IMG_APP} not found"
docker image inspect "${IMG_PROXY}" > /dev/null 2>&1 || refuse "REFUSE image: ${IMG_PROXY} not found"
SIDECAR=no
docker image inspect "${IMG_SIDE}" > /dev/null 2>&1 && SIDECAR=yes

docker network create --internal --ipv6=false --subnet "${SUBNET}" "${NET}" > /dev/null \
    || refuse "REFUSE network: docker network create ${NET} failed"

proxy_caps=(); for c in ${SLED_PROXY_CAPS}; do proxy_caps+=(--cap-add "$c"); done
docker create --name "${PROXY}" --label "${LABEL}=${PREFIX}" \
    --network "${NET}" --network-alias "${PROXY_ALIAS}" \
    --cap-drop ALL "${proxy_caps[@]}" --security-opt no-new-privileges \
    --read-only --tmpfs /var/log/squid --tmpfs /etc/squid/profile.d --tmpfs /run \
    --tmpfs /var/spool/squid --tmpfs /var/cache/squid --tmpfs /tmp \
    --pids-limit 128 --memory 256m --memory-swap 256m --cpus 1 \
    "${IMG_PROXY}" > /dev/null || refuse "REFUSE proxy: docker create failed"
started "${PROXY}"
# Both networks before start: the entrypoint binds squid to the internal
# address only (proxy/sled-bind-addr), so the bridge gets no answer on 3128.
docker network connect bridge "${PROXY}" || refuse "REFUSE proxy: network connect bridge failed"
docker start "${PROXY}" > /dev/null || refuse "REFUSE proxy: docker start failed"

app_mounts=(-v "${STATE}/state:/data" -v "${MODELS}:/models/fastembed:ro")
for s in ${SECRETS}; do app_mounts+=(-v "${STATE}/secrets/${s}:/run/secrets/${s}:ro"); done
app_tmpfs=(--tmpfs /tmp:mode=1777 --tmpfs /var/tmp:mode=1777)
# Main's image runs tailscaled in the app; with a sidecar the app has no socket dir.
[ "${SIDECAR}" = no ] && app_tmpfs+=(--tmpfs "/var/run/tailscale:mode=0700,uid=${SLED_UID},gid=${SLED_GID}")
docker run -d --name "${APP}" --label "${LABEL}=${PREFIX}" --network "${NET}" \
    --cap-drop ALL --security-opt no-new-privileges --read-only "${app_tmpfs[@]}" \
    --pids-limit 1024 --memory 6g --memory-swap 6g --cpus 4 \
    "${app_mounts[@]}" "${IMG_APP}" > /dev/null || refuse "REFUSE app: docker run failed"
started "${APP}"

if [ "${SIDECAR}" = yes ]; then
    docker run -d --name "${SIDE}" --label "${LABEL}=${PREFIX}" --network "container:${APP}" \
        --cap-drop ALL --security-opt no-new-privileges \
        --pids-limit 256 --memory 256m --memory-swap 256m --cpus 0.5 \
        -v "${SIDE}:/var/lib/tailscale" "${IMG_SIDE}" > /dev/null \
        || refuse "REFUSE sidecar: docker run failed"
    started "${SIDE}"
elif [ -e "${V5}" ]; then
    refuse "REFUSE sidecar: ${IMG_SIDE} not found and V5 needs it"
fi

out="$(inspect_all "${SIDECAR}")"
[ -z "${out}" ] || refuse "${out}"
echo "PASS inspection: ${APP} holds the enforced topology"

# The V1 standard exercise, then V1 and V5. Any FAIL refuses.
n=1
until docker exec -u sled "${APP}" check.sh; do
    [ "$n" -ge "${TRIES}" ] && refuse "REFUSE check: check.sh FAILED ${TRIES} time(s)"
    n=$((n + 1)); sleep "${PAUSE}"
done
docker exec -u sled "${APP}" exercise.sh || refuse "REFUSE check: exercise.sh FAILED"
"${HERE}/check_v1.sh" "${APP}" "${PROXY}" "${NET}" || refuse "REFUSE check: V1 FAILED"
passed="V1 PASS"
if [ -e "${V5}" ]; then
    "${V5}" "${APP}" "${SIDE}" || refuse "REFUSE check: V5 FAILED"
    passed="${passed}, V5 PASS"
else
    passed="${passed}; V5 not present"
fi
# R2 (#11156): name what was not checked; DOC-90 §9 numbers V2-V4.
echo "RESULT: READY (${passed}; V2-V4 not scripted, see #11167)"
