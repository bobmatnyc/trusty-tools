#!/usr/bin/env bash
# shellcheck disable=SC2016  # the stub check scripts are written literally
#
# check_sled_launch.sh [DIR] — offline test of docker/sled/launch.sh (#11156).
#
# Why: launch.sh is the only supported way to start a sled (DOC-90 §12). Each
#   of its refusals must keep firing, or a published port, a second network or
#   a token in env starts a sled silently. This runs on a Mac and in CI with no
#   docker: a stub `docker` (scripts/test-data/sled-launch/docker) serves
#   canned `version` and `inspect` JSON.
# What: copies DIR (default docker/sled) to a temp dir, swaps check_v1.sh and
#   check_tailscale.sh for stubs, then runs launch.sh once per case: the happy
#   paths (main layout, sidecar layout, --check-only) end READY or CHECK OK;
#   every refusal case must exit 1 with its "REFUSE <class>" line and stop the
#   app. Prints PASS or FAIL per case; exits 1 on any FAIL. A missing file is
#   a FAIL.
# Test: this script is the regression test; run it against a copy of DIR with
#   one check removed to see that case go red.
#
# Usage: scripts/check_sled_launch.sh [DIR]   (needs bash and jq)

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="${1:-${ROOT}/docker/sled}"
STUB="${ROOT}/scripts/test-data/sled-launch/docker"
FAILS=0
command -v jq > /dev/null || { echo "FAIL jq is not installed"; exit 1; }
for f in launch.sh launch_lib.sh proxy/squid.conf; do
    [ -f "${SRC}/${f}" ] || { echo "FAIL ${SRC}/${f} is missing"; exit 1; }
done

T="$(mktemp -d)"
trap 'rm -rf "${T}"' EXIT
P=tm-trusty-cloud-sled
SUBNET="$(awk '$1=="acl" && $2=="internal_net" {print $4}' "${SRC}/proxy/squid.conf")"
ME="$(id -u)"

# The host side: state, secrets 0600 in 0700 dirs, a model cache.
ST="${T}/state-root"; MODELS="${T}/models"
mkdir -p "${ST}/state" "${ST}/secrets" "${MODELS}" "${T}/bin"
chmod 700 "${ST}" "${ST}/state" "${ST}/secrets"
for s in github-token trusty-console-token; do
    printf 'x\n' > "${ST}/secrets/${s}"; chmod 600 "${ST}/secrets/${s}"
done
cp "${STUB}" "${T}/bin/docker"; chmod +x "${T}/bin/docker"

# A clean container: hardened, running, no ports, no host namespace.
base() { # NAME NETMODE NETS(json object) CAPS(json) READONLY
    jq -n --arg n "$1" --arg mode "$2" --argjson nets "$3" --argjson caps "$4" --argjson ro "$5" '[{
      Id: ("id-" + $n), Name: ("/" + $n), State: {Running: true},
      Config: {Env: ["PATH=/usr/bin", "TRUSTY_CONSOLE_TOKEN_FILE=/run/secrets/trusty-console-token"]},
      HostConfig: {NetworkMode: $mode, PortBindings: {}, PublishAllPorts: false, Privileged: false,
        CapDrop: ["ALL"], CapAdd: $caps, SecurityOpt: ["no-new-privileges"], ReadonlyRootfs: $ro,
        PidMode: "", IpcMode: "private", UTSMode: "", UsernsMode: "", Devices: [],
        PidsLimit: 128, Memory: 268435456, MemorySwap: 268435456, NanoCpus: 1000000000, Tmpfs: {}},
      NetworkSettings: {Networks: $nets, Ports: {}}, Mounts: []}]'
}
bind() { jq -n --arg d "$1" --arg s "$2" --argjson rw "$3" '{Type: "bind", Source: $s, Destination: $d, RW: $rw}'; }

# fixtures DIR — write the happy-path fixtures for one case.
fixtures() {
    local d="$1" mounts
    mkdir -p "$d"
    printf '26.1.4\n' > "$d/version"
    printf '%s\n' "${P}:t" "${P}-egress:t" > "$d/images"
    mounts="$(jq -s . <(bind /data "${ST}/state" true) <(bind /models/fastembed "${MODELS}" false) \
        <(bind /run/secrets/github-token "${ST}/secrets/github-token" false) \
        <(bind /run/secrets/trusty-console-token "${ST}/secrets/trusty-console-token" false))"
    base "${P}" "${P}-internal" "{\"${P}-internal\":{}}" '[]' true \
        | jq --argjson m "${mounts}" '.[0].Mounts = $m | .[0].HostConfig.Tmpfs = {"/tmp":"mode=1777","/var/tmp":"mode=1777","/var/run/tailscale":"mode=0700"}' \
        > "$d/inspect-${P}.json"
    base "${P}-egress" "${P}-internal" "{\"${P}-internal\":{},\"bridge\":{}}" '["CHOWN","SETUID","SETGID","DAC_OVERRIDE","KILL"]' true \
        | jq '.[0].HostConfig.Tmpfs = {"/var/log/squid":"","/run":"","/etc/squid/profile.d":""}' > "$d/inspect-${P}-egress.json"
    base "${P}-tailscale" "container:${P}" '{}' '[]' false \
        | jq --arg v "${P}-tailscale" '.[0].Mounts = [{Type: "volume", Name: $v, Source: "/var/lib/docker/volumes/x/_data", Destination: "/var/lib/tailscale", RW: true}]' \
        > "$d/inspect-${P}-tailscale.json"
    jq -n --arg s "${SUBNET}" --arg a "${P}" '[{Internal: true, EnableIPv6: false,
        IPAM: {Config: [{Subnet: $s}]}, Containers: {x: {Name: $a}, y: {Name: ($a + "-egress")}}}]' \
        > "$d/network-${P}-internal.json"
}

# mutate DIR FILE JQ — apply JQ to one fixture.
mutate() { jq "$3" "$1/$2" > "$1/$2.new" && mv "$1/$2.new" "$1/$2"; }

# case_run NAME RC PATTERN SETUP [launch args...] — fresh sled copy and stub
# dir; SETUP is a function name run with the stub dir as $1.
case_run() {
    local name="$1" want_rc="$2" pat="$3" setup="$4" d out rc=0
    shift 4
    d="${T}/c-${name}"; rm -rf "$d"; mkdir -p "$d/sled/proxy" "$d/stub"
    cp "${SRC}/launch.sh" "${SRC}/launch_lib.sh" "$d/sled/"
    cp "${SRC}/proxy/squid.conf" "$d/sled/proxy/"
    printf '#!/usr/bin/env bash\nexit "$(cat %s/v1.rc 2>/dev/null || echo 0)"\n' "$d/stub" > "$d/sled/check_v1.sh"
    chmod +x "$d/sled/launch.sh" "$d/sled/check_v1.sh"
    fixtures "$d/stub"
    "${setup}" "$d"
    out="$(PATH="${T}/bin:${PATH}" STUB_DIR="$d/stub" SLED_CHECK_PAUSE=0 \
        "$d/sled/launch.sh" --state "${ST}" --models "${MODELS}" "$@" 2>&1)" || rc=$?
    if [ "${rc}" = "${want_rc}" ] && grep -qE -- "${pat}" <<< "${out}" \
        && { [ "${want_rc}" = 0 ] || grep -q "docker stop ${P}\$" "$d/stub/calls.log" 2>/dev/null \
             || { [ "${*: -1}" != --check-only ] \
                  && ! grep -q "run -d --name ${P} " "$d/stub/calls.log" 2>/dev/null; }; }; then
        echo "PASS ${name}"
    else
        echo "FAIL ${name} (rc=${rc}, want ${want_rc} and /${pat}/)"
        printf '%s\n' "${out}" | tail -n 6 | sed 's/^/    /'
        FAILS=$((FAILS + 1))
    fi
}

# Setups: each takes the case dir.
none() { :; }
v5() { printf '#!/usr/bin/env bash\nexit "$(cat %s/v5.rc 2>/dev/null || echo 0)"\n' "$1/stub" > "$1/sled/check_tailscale.sh"; chmod +x "$1/sled/check_tailscale.sh"; }
sidecar() { v5 "$1"; echo "${P}-tailscale:t" >> "$1/stub/images"
    mutate "$1/stub" "inspect-${P}.json" '.[0].HostConfig.Tmpfs |= del(."/var/run/tailscale")'; }
live() { mkdir -p "$1/stub/live"; touch "$1/stub/live/${P}" "$1/stub/live/${P}-egress" "$1/stub/live/${P}-internal"; }
old_docker() { printf '25.0.3\n' > "$1/stub/version"; }
port() { mutate "$1/stub" "inspect-${P}.json" '.[0].HostConfig.PortBindings = {"7788/tcp": [{"HostIp": "", "HostPort": "7788"}]}'; }
net2() { mutate "$1/stub" "inspect-${P}.json" '.[0].NetworkSettings.Networks.bridge = {}'; }
priv() { mutate "$1/stub" "inspect-${P}.json" '.[0].HostConfig.Privileged = true'; }
capadd() { mutate "$1/stub" "inspect-${P}.json" '.[0].HostConfig.CapAdd = ["NET_ADMIN"]'; }
hostpid() { mutate "$1/stub" "inspect-${P}.json" '.[0].HostConfig.PidMode = "host"'; }
sock() { mutate "$1/stub" "inspect-${P}.json" '.[0].Mounts += [{Type: "bind", Source: "/var/run/docker.sock", Destination: "/var/run/docker.sock", RW: true}]'; }
tokenenv() { mutate "$1/stub" "inspect-${P}.json" '.[0].Config.Env += ["GH_TOKEN=x"]'; }
writable() { mutate "$1/stub" "inspect-${P}.json" '.[0].HostConfig.ReadonlyRootfs = false'; }
notinternal() { mutate "$1/stub" "network-${P}-internal.json" '.[0].Internal = false'; }
garbage() { printf 'not json\n' > "$1/stub/inspect-${P}.json"; }
badmode() { chmod 644 "${ST}/secrets/github-token"; }
v1fail() { echo 1 > "$1/stub/v1.rc"; }
v5fail() { sidecar "$1"; echo 1 > "$1/stub/v5.rc"; }
sidepriv() { sidecar "$1"; mutate "$1/stub" "inspect-${P}-tailscale.json" '.[0].HostConfig.Privileged = true'; }
latenet() { live "$1"; net2 "$1"; }

case_run happy-main 0 'RESULT: READY' none --tag t
case_run happy-sidecar 0 'RESULT: READY' sidecar --tag t
case_run check-only-clean 0 'RESULT: CHECK OK' live --check-only
case_run docker-too-old 1 'REFUSE host: docker engine' old_docker --tag t
case_run published-port 1 'REFUSE ports:' port --tag t
case_run second-network 1 'REFUSE network: attached' net2 --tag t
case_run privileged 1 'REFUSE privileged:' priv --tag t
case_run added-cap 1 'REFUSE caps: added NET_ADMIN' capadd --tag t
case_run host-pid 1 'REFUSE namespace: host' hostpid --tag t
case_run docker-sock-mount 1 'REFUSE mounts: /var/run/docker.sock' sock --tag t
case_run token-env 1 'REFUSE env: GH_TOKEN' tokenenv --tag t
case_run writable-root 1 'REFUSE hardening: root filesystem' writable --tag t
case_run network-not-internal 1 'REFUSE network: not --internal' notinternal --tag t
case_run inspect-garbage 1 'REFUSE inspect:' garbage --tag t
case_run secret-mode 1 'REFUSE secrets: .*github-token has mode 644' badmode --tag t
chmod 600 "${ST}/secrets/github-token"
SLED_UID=$((ME + 1)) case_run secret-owner 1 'REFUSE secrets: .*owned by uid' none --tag t
case_run v1-fail 1 'REFUSE check: V1 FAILED' v1fail --tag t
case_run v5-no-sidecar 1 'REFUSE sidecar:' v5 --tag t
case_run v5-fail 1 'REFUSE check: V5 FAILED' v5fail --tag t
case_run sidecar-privileged 1 'REFUSE privileged:' sidepriv --tag t
case_run check-only-late-network 1 'REFUSE network: attached' latenet --check-only

if [ "${FAILS}" -gt 0 ]; then echo "RESULT: ${FAILS} case(s) FAILED"; exit 1; fi
echo "RESULT: all cases passed"
