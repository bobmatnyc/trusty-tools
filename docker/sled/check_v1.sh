#!/usr/bin/env bash
# check_v1.sh [APP] [PROXY] [NETWORK] — DOC-90 section 9 V1 (squid deny log).
# Run on the docker host, from the repo checkout, after the standard exercise
# (stack start, check.sh, exercise.sh):
#   docker/sled/check_v1.sh tm-trusty-cloud-sled tm-trusty-cloud-sled-egress
# Reads the proxy config from the running proxy container and compares it with
# the policy below and with docker/sled/proxy/. Sends the probes from the app
# container, then grades the proxy's access.log and profile.log. Prints PASS or
# FAIL per row and exits non-zero on any FAIL. Fail-closed: an unreadable
# file, a stopped container or a missing log line is a FAIL, never a PASS.
# It prints host names, addresses and squid result codes only; no secret.
# Refs #11073, #11156
set -uo pipefail

APP="${1:-tm-trusty-cloud-sled}"
PROXY="${2:-tm-trusty-cloud-sled-egress}"
NET="${3:-tm-trusty-cloud-sled-internal}"
HERE="$(cd "$(dirname "$0")" && pwd)"

# The V1 policy: every acl, http_access and include line of squid.conf, in
# order. The running squid.conf must hold exactly these lines; an added allow,
# a changed host or a changed regex is drift. The host lists below come from it.
POLICY='acl internal_net src 10.250.78.0/24
acl tls_port port 443
acl connect_method method CONNECT
acl raw_ip dstdom_regex -n ^[0-9.]+$
acl raw_ip dstdom_regex -n -i ^\[?[0-9a-f]*:[0-9a-f:.]*\]?$
acl steady_hosts dstdomain api.anthropic.com
acl steady_hosts dstdomain github.com
acl steady_hosts dstdomain api.github.com
acl steady_hosts dstdomain controlplane.tailscale.com
acl steady_hosts dstdomain login.tailscale.com
acl derp dstdom_regex -i ^derp[0-9]+[a-z]*(-all)?\.tailscale\.com$
http_access deny !internal_net
http_access deny !connect_method
http_access deny connect_method !tls_port
http_access deny connect_method raw_ip
http_access allow connect_method tls_port steady_hosts
http_access allow connect_method tls_port derp
include /etc/squid/profile.d/active.conf
http_access deny all'
LOGIN_POLICY='acl login_hosts dstdomain platform.claude.com
acl login_hosts dstdomain claude.ai
http_access allow connect_method tls_port login_hosts'
export FORBID_RE='(^|\.)(crates\.io|huggingface\.co|registry\.npmjs\.org|pypi\.org|downloads\.claude\.ai)$'

# hosts LIST ACL — the dstdomain hosts of ACL in LIST, one per line, sorted.
hosts() { awk -v a="$2" '$1=="acl" && $2==a && $3=="dstdomain" {print $4}' <<< "$1" | sort; }
# hosts_re HOSTS EXTRA — ^(h1|h2|EXTRA):443$ with the dots escaped.
hosts_re() {
    local alt
    alt="$(paste -sd'|' - <<< "${1//./\\.}")"
    echo "^(${alt}${2:+|$2}):443\$"
}
DERP_RE="$(awk '$1=="acl" && $2=="derp" {print $NF}' <<< "${POLICY}")"
derp_core="${DERP_RE#^}"; derp_core="${derp_core%\$}"
STEADY_RE="$(hosts_re "$(hosts "${POLICY}" steady_hosts)" "${derp_core}")"
LOGIN_RE="$(hosts_re "$(hosts "${LOGIN_POLICY}" login_hosts)")"
export STEADY_RE LOGIN_RE

FAILS=0
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

# row NAME OK [DETAIL] — print PASS/FAIL and count failures.
row() {
    if [ "$2" = yes ]; then
        echo "PASS $1"
    else
        echo "FAIL $1${3:+ -- $3}"
        FAILS=$((FAILS + 1))
    fi
}
# die REASON — a FAIL that leaves nothing to grade: stop here.
die() {
    echo "FAIL $1"
    echo "RESULT: V1 stopped, $((FAILS + 1)) row(s) FAILED"
    exit 1
}
app() { docker exec -u sled "${APP}" "$@"; }
running() { [ "$(docker inspect -f '{{.State.Running}}' "$1" 2>/dev/null)" = true ]; }
# pget PATH NAME — copy a file out of the running proxy to $TMP/NAME, or die.
pget() {
    docker exec "${PROXY}" cat "$1" > "${TMP}/$2" 2>/dev/null \
        || die "cannot read $1 in ${PROXY}"
}
# policy FILE — its acl, http_access and include lines, comments dropped.
policy() { grep -E '^(acl|http_access|include)[[:space:]]' "$1"; }
conf() { grep -vE '^[[:space:]]*(#|$)' "$1"; }
alog() { pget /var/log/squid/access.log access.log; }

# 0. The running proxy holds the V1 policy (drift check).
running "${PROXY}" || die "proxy container ${PROXY} is not running"
pget /etc/squid/squid.conf squid.conf
pget /etc/squid/login.conf login.conf
pget /etc/squid/profile.d/active.conf active.conf
d="$(diff <(policy "${TMP}/squid.conf") <(printf '%s\n' "${POLICY}") | grep '^[<>]' | tr '\n' ';')"
ok=no; [ -z "${d}" ] && ok=yes
row "running squid.conf rules equal the V1 policy" "${ok}" "${d}"
d="$(diff <(policy "${TMP}/login.conf") <(printf '%s\n' "${LOGIN_POLICY}") | grep '^[<>]' | tr '\n' ';')"
ok=no; [ -z "${d}" ] && ok=yes
row "running login.conf rules equal the V1 login policy" "${ok}" "${d}"
for f in squid.conf login.conf; do
    ok=no
    [ -r "${HERE}/proxy/${f}" ] && diff -q <(conf "${TMP}/${f}") <(conf "${HERE}/proxy/${f}") > /dev/null && ok=yes
    row "running ${f} equals docker/sled/proxy/${f}" "${ok}"
done
d="$(policy "${TMP}/active.conf" | tr '\n' ';')"
ok=no; [ -z "${d}" ] && ok=yes
row "proxy is in the steady profile (active.conf holds no rule)" "${ok}" "${d}"
ok=no; docker exec "${PROXY}" squid -k parse -f /etc/squid/squid.conf > /dev/null 2>&1 && ok=yes
row "squid -k parse accepts the running squid.conf" "${ok}"

# 1. No route out and no external DNS from the app (OUT-1). Each row first
# proves the app container runs, so a stopped container cannot pass it.
app_up() { running "${APP}" && app true > /dev/null 2>&1; }
ok=no; detail="app container ${APP} is not running"
if app_up; then
    out="$(app getent hosts example.com 2>&1)"; rc=$?
    [ "${rc}" -ne 0 ] && [ -z "${out}" ] && ok=yes
    detail="rc=${rc} ${out}"
fi
row "getent hosts example.com resolves nothing" "${ok}" "${detail}"
# curl 6 = no DNS, 7 = connect refused or unreachable, 28 = timeout. Any other
# code (docker exec 1, a missing curl 127, TLS 35, success 0) is not a pass.
ok=no; detail="app container ${APP} is not running"
if app_up; then
    app curl -sS -m 10 -o /dev/null --noproxy '*' https://example.com > /dev/null 2>&1; rc=$?
    case "${rc}" in 6|7|28) ok=yes ;; esac
    detail="rc=${rc}"
fi
row "curl --noproxy '*' https://example.com fails to connect (curl 6, 7 or 28)" "${ok}" "${detail}"

# 2. The app's network: no default route, and no peer on the internal network
# but the proxy on 3128. The gateway is the host side of the internal bridge.
# A default route has destination 0 and prefix 0; an IPv6 entry with the
# RTF_REJECT flag (0x200, the kernel's unreachable entry on lo) is not a route out.
routes="$(app cat /proc/net/route 2>&1)"; rc=$?
r6="$(app cat /proc/net/ipv6_route 2>/dev/null)"
ok=no
if [ "${rc}" -eq 0 ] && [ -n "${routes}" ]; then
    awk 'NR>1 && $2=="00000000" {f=1} END {exit f}' <<< "${routes}" \
        && ! awk '$1 ~ /^0+$/ && $2=="00" && substr($9, 6, 1) !~ /[2367abef]/ {f=1} END {exit !f}' \
            <<< "${r6}" && ok=yes
fi
row "app has no default route (IPv4 or IPv6)" "${ok}" "$(tr '\n' ';' <<< "${routes}")"
echo "INFO app routes (/proc/net/route):"; printf '%s\n' "${routes}" | sed 's/^/INFO   /'
if app sh -c 'command -v ip' > /dev/null 2>&1; then
    echo "INFO app ip route:"; app ip route 2>&1 | sed 's/^/INFO   /'
fi
GW="$(docker network inspect -f '{{range .IPAM.Config}}{{.Gateway}}{{end}}' "${NET}" 2>/dev/null)"
PIP="$(docker inspect -f "{{with index .NetworkSettings.Networks \"${NET}\"}}{{.IPAddress}}{{end}}" "${PROXY}" 2>/dev/null)"
# tcp HOST PORT — one TCP connect from the app, 3 s bound:
# open, refused, timeout or "other: <message>".
tcp() {
    local o rc
    # shellcheck disable=SC2016 # $0 and $1 expand in the app's bash, not here
    o="$(app timeout 3 bash -c 'exec 3<>"/dev/tcp/$0/$1"' "$1" "$2" 2>&1)"; rc=$?
    case "${rc}:${o}" in
        0:*) echo open ;;
        124:*) echo timeout ;;
        *[Rr]efused*) echo refused ;;
        *) echo "other: $(tail -n 1 <<< "${o}" | sed 's/.*: //')" ;;
    esac
}
# #11156 B3: the gateway rows probe every TCP port and the common UDP ports,
# so a host service on any port (2222, say) fails V1. Only a silent drop or no
# route passes; an open port, a refusal (the host answered) or any other result
# fails. The scan grades itself: each worker reports how many ports it probed
# and probes the proxy's 3128 the same way, which must be open. A missing tool,
# a lost worker, a short count or a scan past SCAN_BOUND seconds is a FAIL.
SCAN_JOBS="${V1_SCAN_JOBS:-256}"     # parallel workers in the app
SCAN_WAIT="${V1_SCAN_WAIT:-1}"       # seconds per connect
SCAN_BOUND="${V1_SCAN_BOUND:-900}"   # seconds for the whole scan
UDP_PORTS="53 123 443 41641"
# The TCP scan, run by the app's bash: GW JOBS WAIT CONTROL-IP. One line per
# port that did not pass ("port P RESULT"), then one "done I COUNT CONTROL BAD"
# line per worker, BAD being its count of port lines.
read -r -d '' TCP_SCAN <<'EOF'
gw=$0 jobs=$1 wait=$2 ctl=$3
command -v timeout > /dev/null || { echo "error: no timeout command in the app"; exit 3; }
# probe_class RC OUTPUT — the child prints "s" just before it connects, so a
# timeout with no marker means the connect never began (#11156): not a drop.
probe_class() {
    local m
    case "$2" in s|s$'\n'*) ;; *) echo other:not-started; return ;; esac
    case "$1:$2" in
        0:*) echo open ;;
        124:s) echo timeout ;;
        *[Rr]efused*) echo refused ;;
        *"No route to host"*|*"Network is unreachable"*) echo unreachable ;;
        *) m="${2##*$'\n'}"; m="${m##*: }"; echo "other:rc$1:${m// /_}" ;;
    esac
}
probe() {
    local o rc
    o="$(timeout "${wait}" bash -c 'echo s; exec 3<>"/dev/tcp/$0/$1"' "$1" "$2" 2>&1)"; rc=$?
    probe_class "${rc}" "${o}"
}
worker() {
    local p n=0 b=0 r
    for ((p = $1; p <= 65535; p += jobs)); do
        r="$(probe "${gw}" "${p}")"; n=$((n + 1))
        case "${r}" in timeout|unreachable) ;; *) b=$((b + 1)); echo "port ${p} ${r}" ;; esac
    done
    echo "done $1 ${n} $(probe "${ctl}" 3128) ${b}"
}
for ((i = 1; i <= jobs; i++)); do worker "${i}" & done
wait
EOF
# The UDP probe, run by the app's bash: HOST PORT HEX-PAYLOAD WAIT. Prints
# answered, refused (ICMP port unreachable), timeout, unreachable or other.
# The payload goes to a file first, and cat sends it in one write, so one
# datagram carries all of it (#11156).
read -r -d '' UDP_PROBE <<'EOF'
f="$(mktemp)" || { echo "other:mktemp"; exit 0; }
printf '%b' "$(sed 's/../\\x&/g' <<< "$2")" > "${f}" || { rm -f "${f}"; echo "other:payload"; exit 0; }
exec 3<>"/dev/udp/$0/$1" 2> /dev/null || { rm -f "${f}"; echo "other:socket"; exit 0; }
cat "${f}" >&3 2> /dev/null || { rm -f "${f}"; echo "other:write"; exit 0; }
rm -f "${f}"
o="$(timeout "$3" head -c 1 <&3 2>&1 > /dev/null)"; rc=$?
case "${rc}:${o}" in
    0:*) echo answered ;;
    124:*) echo timeout ;;
    *[Rr]efused*) echo refused ;;
    *"No route to host"*|*"unreachable"*) echo unreachable ;;
    *) echo "other:${o##*: }" ;;
esac
EOF
# scan_verdict RC OUTPUT JOBS — "yes" only for a complete, clean scan; else
# the reason. Fail-closed: anything it cannot account for is not a pass.
# #11156: 256 workers share one stream, so a line spliced by a stray message
# (a fork retry, say) could hide a port line. Every line must be a whole port
# or done line, and the done lines' BAD counts must all be 0.
scan_verdict() {
    local rc="$1" out="$2" jobs="$3" stray done_n total ctl bad nbad
    [ "${rc}" -eq 0 ] || { echo "scan exited ${rc} (124 = past the bound)"; return; }
    grep -q '^error' <<< "${out}" && { grep '^error' <<< "${out}" | head -n 1; return; }
    stray="$(grep -vE '^(port [0-9]+ [^ ]+|done [0-9]+ [0-9]+ [^ ]+ [0-9]+)$' <<< "${out}" | head -n 3 | tr '\n' ';')"
    [ -z "${stray}" ] || { echo "unexpected scan output: ${stray}"; return; }
    done_n="$(awk '$1=="done" {print $2}' <<< "${out}" | sort -u | wc -l | tr -d ' ')"
    total="$(awk '$1=="done" {s+=$3} END {print s+0}' <<< "${out}")"
    ctl="$(awk '$1=="done" && $4!="open"' <<< "${out}" | wc -l | tr -d ' ')"
    nbad="$(awk '$1=="done" {s+=$5} END {print s+0}' <<< "${out}")"
    bad="$(grep '^port ' <<< "${out}" | head -n 20 | tr '\n' ';')"
    if [ "$(grep -c '^done ' <<< "${out}")" -ne "${jobs}" ] || [ "${done_n}" -ne "${jobs}" ] \
        || [ "${total}" -ne 65535 ]; then
        echo "incomplete: ${done_n}/${jobs} workers, ${total}/65535 ports"
    elif [ "${ctl}" -ne 0 ]; then
        echo "control probe of the proxy's 3128 not open in ${ctl} worker(s)"
    elif [ "${nbad}" -ne 0 ] || [ -n "${bad}" ]; then
        echo "answered: ${nbad} port(s) reported by the workers: ${bad}"
    else
        echo yes
    fi
}
# udp_verdict RESULT — "yes" for a drop or no route, else the result.
udp_verdict() { case "$1" in timeout|unreachable) echo yes ;; *) echo "$1" ;; esac; }
# Payloads: a DNS query for the root NS, an NTP client request, a QUIC packet
# of an unknown version (a QUIC server answers with version negotiation), and
# two bytes for WireGuard (Tailscale), which answers nothing; there only an
# ICMP refusal shows the host is reachable.
udp_payload() {
    case "$1" in
        53) echo 7631010000010000000000000000020001 ;;
        123) printf '1b%094d\n' 0 ;;
        443) printf 'c01a2a3a4a08010203040506070800%02370d\n' 0 ;;
        *) echo 7631 ;;
    esac
}
ok=no; detail="no gateway address for network ${NET}"
if ! [[ "${SCAN_JOBS}" =~ ^[1-9][0-9]{0,4}$ ]] || [ "${SCAN_JOBS}" -gt 65535 ]; then
    detail="V1_SCAN_JOBS=${SCAN_JOBS} is not 1-65535"
elif [ -n "${GW}" ] && [ -n "${PIP}" ]; then
    echo "INFO scanning TCP 1-65535 on ${GW}: ${SCAN_JOBS} workers, ${SCAN_WAIT}s per port, bound ${SCAN_BOUND}s"
    # #11156: the bound runs inside the app, so it kills the workers too (its
    # process group); the host timeout only backs up a hung docker CLI.
    out="$(timeout "$((SCAN_BOUND + 30))" docker exec -u sled "${APP}" \
        timeout -k 5 "${SCAN_BOUND}" bash -c "${TCP_SCAN}" \
        "${GW}" "${SCAN_JOBS}" "${SCAN_WAIT}" "${PIP}" 2>&1)"; rc=$?
    v="$(scan_verdict "${rc}" "${out}" "${SCAN_JOBS}")"
    [ "${v}" = yes ] && ok=yes
    detail="${v}"
elif [ -n "${GW}" ]; then
    detail="no proxy address on ${NET} for the scan's control probe"
fi
row "app reaches no TCP port 1-65535 on the internal gateway ${GW:-?}" "${ok}" "${detail}"
# udp HOST PORT — one UDP probe from the app, 3 s for a reply, 30 s bound.
udp() {
    timeout 30 docker exec -u sled "${APP}" timeout -k 2 20 bash -c "${UDP_PROBE}" \
        "$1" "$2" "$(udp_payload "$2")" 3 2>&1 || echo "other:exec failed"
}
# The control: Docker's embedded DNS in the app must answer the DNS probe, so
# a probe that cannot see a reply cannot pass the gateway rows.
res="$(udp 127.0.0.11 53 | tail -n 1)"; ok=no; [ "${res}" = answered ] && ok=yes
row "UDP probe control: the app's embedded DNS 127.0.0.11:53 answers" "${ok}" "${res}"
for port in ${UDP_PORTS}; do
    ok=no; detail="no gateway address for network ${NET}"
    if [ -n "${GW}" ]; then
        res="$(udp "${GW}" "${port}" | tail -n 1)"; detail="$(udp_verdict "${res}")"
        echo "INFO udp ${GW}:${port} -> ${res}"
        [ "${detail}" = yes ] && ok=yes
    fi
    row "app gets no UDP answer from the internal gateway ${GW:-?}:${port}" "${ok}" "${detail}"
done
ok=no; detail="no proxy address on ${NET}"
if [ -n "${PIP}" ]; then
    res="$(tcp "${PIP}" 3128)"; detail="3128=${res};"
    [ "${res}" = open ] && ok=yes
    for port in 22 80 443; do
        r="$(tcp "${PIP}" "${port}")"; detail="${detail}${port}=${r};"
        [ "${r}" = open ] && ok=no
    done
    echo "INFO proxy ${PIP}: ${detail}"
fi
row "app reaches the proxy ${PIP:-?} on 3128 only" "${ok}" "${detail}"

# 3. Probes through the proxy; each must add a TCP_DENIED/403 CONNECT line
# written after the probes began: after line BEFORE and at or after time T0.
alog
before="$(wc -l < "${TMP}/access.log")"
t0="$(date +%s)"
probes=("example.com:443|https://example.com"
        "1.1.1.1:443|https://1.1.1.1"
        "[2606:4700:4700::1111]:443|https://[2606:4700:4700::1111]")
for p in "${probes[@]}"; do
    app curl -sS -m 10 -o /dev/null "${p#*|}" > /dev/null 2>&1
done
sleep 2
alog
[ -s "${TMP}/access.log" ] || die "access.log is empty after the probes"
tail -n +"$((before + 1))" "${TMP}/access.log" | awk -v t="${t0}" '$1+0 >= t' > "${TMP}/probe.log"
for p in "${probes[@]}"; do
    target="${p%%|*}"
    ok=no
    awk -v t="${target}" '$4=="TCP_DENIED/403" && $6=="CONNECT" && $7==t {f=1} END {exit !f}' \
        "${TMP}/probe.log" && ok=yes
    row "CONNECT ${target} logged TCP_DENIED/403 after the probe began" "${ok}" \
        "$(awk -v t="${target}" '$7==t {print $4, $6, $7}' "${TMP}/probe.log" | head -n 3 | tr '\n' ';')"
done

# 3b. IP literals curl would rewrite to dotted form, sent as a raw CONNECT so
# the proxy sees the literal itself (#11156): hex, dotted hex, octal, integer
# forms of 1.1.1.1. Pass: 403 (http_access deny) or 400 (squid rejects the
# authority). A 200 is a tunnel; a 5xx means an allow matched.
# rawconnect TARGET — the proxy's status code for CONNECT TARGET, or "none".
rawconnect() {
    local line
    # shellcheck disable=SC2016 # $0 and $1 expand in the app's bash, not here
    line="$(app timeout 10 bash -c 'exec 3<>"/dev/tcp/$0/3128" || exit 1
        printf "CONNECT %s HTTP/1.1\r\nHost: %s\r\n\r\n" "$1" "$1" >&3
        IFS= read -r l <&3 && printf "%s\n" "${l}"' "${PIP}" "$1" 2>/dev/null)"
    case "${line}" in
        HTTP/*) awk '{print $2}' <<< "${line}" ;;
        *) echo none ;;
    esac
}
for target in 0x01010101:443 0x01.0x01.0x01.0x01:443 0001.0001.0001.0001:443 16843009:443; do
    ok=no; detail="no proxy address on ${NET}"
    if [ -n "${PIP}" ]; then
        code="$(rawconnect "${target}")"; detail="status=${code}"
        case "${code}" in 400|403) ok=yes ;; esac
    fi
    row "raw CONNECT ${target} refused by the proxy (403 or 400)" "${ok}" "${detail}"
done

# 4. Every tunnel names a steady host, or a login host inside a logged login
# window. A tunnel's start is its log time minus its elapsed milliseconds.
# FILENAME==ARGV[1], not FNR==NR: an empty profile.log must not make awk read
# access.log as profile records. With no profile record, nothing is a login window.
if docker exec "${PROXY}" cat /var/log/squid/profile.log > "${TMP}/profile.log" 2>/dev/null \
    && [ -s "${TMP}/profile.log" ]; then
    row "profile.log is readable and not empty" yes
else
    : > "${TMP}/profile.log"
    row "profile.log is readable and not empty" no "the entrypoint writes one steady line per start"
fi
awk 'FILENAME==ARGV[1] {pt[++n]=$1; pp[n]=$2; next}
     $4 ~ /^TCP_TUNNEL\// {
         h=$7
         if (h ~ ENVIRON["STEADY_RE"]) next
         if (h ~ ENVIRON["LOGIN_RE"]) {
             s=$1-$2/1000; p="steady"
             for (i=1; i<=n; i++) if (pt[i]+0 <= s) p=pp[i]
             if (p=="login") next
         }
         print $4, $6, h
     }' "${TMP}/profile.log" "${TMP}/access.log" | sort | uniq -c > "${TMP}/bad-tunnels"
tunnels="$(awk '$4 ~ /^TCP_TUNNEL\//' "${TMP}/access.log" | wc -l | tr -d ' ')"
ok=no; [ "${tunnels}" -gt 0 ] && ok=yes
row "the exercise opened at least one tunnel (${tunnels})" "${ok}" "run exercise.sh first"
ok=no; awk '$4 ~ /^TCP_TUNNEL\// && $7=="api.anthropic.com:443" {f=1} END {exit !f}' "${TMP}/access.log" && ok=yes
row "at least one TCP_TUNNEL to api.anthropic.com:443" "${ok}" "run exercise.sh first"
ok=no; [ ! -s "${TMP}/bad-tunnels" ] && ok=yes
row "every TCP_TUNNEL names a steady host (or a login host in a login window)" "${ok}" \
    "$(tr '\n' ';' < "${TMP}/bad-tunnels")"

# 5. No attempt at all, allowed or denied, to a package or model registry.
# access.log is not empty here (checked above), so an unread log cannot pass.
awk '{u=$7; sub(/^[a-z]+:\/\//, "", u); sub(/\/.*/, "", u); sub(/:[0-9]+$/, "", u)
      if (u ~ ENVIRON["FORBID_RE"]) print $4, $6, u}' "${TMP}/access.log" | sort | uniq -c > "${TMP}/forbidden"
ok=no; [ ! -s "${TMP}/forbidden" ] && ok=yes
row "no CONNECT to crates.io, huggingface.co, registry.npmjs.org, pypi.org, downloads.claude.ai" "${ok}" \
    "$(tr '\n' ';' < "${TMP}/forbidden")"

# Inventory for the report: every host seen, by result code.
echo "INFO hosts seen in access.log (count code method host):"
awk '{print $4, $6, $7}' "${TMP}/access.log" | sort | uniq -c | sed 's/^/INFO   /'

if [ "${FAILS}" -gt 0 ]; then
    echo "RESULT: V1 ${FAILS} row(s) FAILED"
    exit 1
fi
echo "RESULT: V1 PASSED"
