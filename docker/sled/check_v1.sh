#!/usr/bin/env bash
# check_v1.sh [APP] [PROXY] — DOC-90 section 9 V1 (squid deny log). Run on the
# docker host after the standard exercise (stack start, check.sh, exercise.sh):
#   docker/sled/check_v1.sh tm-trusty-cloud-sled tm-trusty-cloud-sled-egress
# Sends the probes from the app container, then reads the proxy's access.log
# and profile.log. Prints PASS or FAIL per row and exits non-zero on any FAIL.
# It prints host names and squid result codes only; the log holds no secret.
# Refs #11073
set -uo pipefail

APP="${1:-tm-trusty-cloud-sled}"
PROXY="${2:-tm-trusty-cloud-sled-egress}"

# The steady and login profiles, as in proxy/squid.conf and proxy/login.conf;
# the first row fails if squid.conf's steady list differs from STEADY_HOSTS.
STEADY_HOSTS="api.anthropic.com api.github.com controlplane.tailscale.com github.com login.tailscale.com"
export STEADY_RE='^(api\.anthropic\.com|github\.com|api\.github\.com|controlplane\.tailscale\.com|login\.tailscale\.com|derp[0-9]+[a-z]*(-all)?\.tailscale\.com):443$'
export LOGIN_RE='^(platform\.claude\.com|claude\.ai):443$'
export FORBID_RE='(^|\.)(crates\.io|huggingface\.co|registry\.npmjs\.org|pypi\.org|downloads\.claude\.ai)$'

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
app() { docker exec -u sled "${APP}" "$@"; }
alog() { docker exec "${PROXY}" cat /var/log/squid/access.log > "${TMP}/access.log"; }

# 0. The proxy's steady list is the one this check grades against.
got="$(docker exec "${PROXY}" awk '$1=="acl" && $2=="steady_hosts" && $3=="dstdomain" {print $4}' \
    /etc/squid/squid.conf | sort | tr '\n' ' ' | sed 's/ $//')"
ok=no; [ "${got}" = "${STEADY_HOSTS}" ] && ok=yes
row "proxy squid.conf steady hosts equal the V1 list" "${ok}" "squid.conf has: ${got}"
ok=no; [ "$(docker exec "${PROXY}" sled-profile show)" = steady ] && ok=yes
row "proxy is in the steady profile" "${ok}"

# 1. No route out and no external DNS from the app (OUT-1).
out="$(app getent hosts example.com 2>&1)"; rc=$?
ok=no; [ "${rc}" -ne 0 ] && [ -z "${out}" ] && ok=yes
row "getent hosts example.com resolves nothing" "${ok}" "rc=${rc} ${out}"
out="$(app curl -sS -m 10 -o /dev/null --noproxy '*' https://example.com 2>&1)"; rc=$?
ok=no; [ "${rc}" -ne 0 ] && ok=yes
row "curl --noproxy '*' https://example.com fails" "${ok}" "rc=${rc}"

# 2. Probes through the proxy; each must add a TCP_DENIED/403 CONNECT line.
alog
before="$(wc -l < "${TMP}/access.log")"
probes=("example.com:443|https://example.com"
        "1.1.1.1:443|https://1.1.1.1"
        "[2606:4700:4700::1111]:443|https://[2606:4700:4700::1111]")
for p in "${probes[@]}"; do
    app curl -sS -m 10 -o /dev/null "${p#*|}" > /dev/null 2>&1
done
sleep 2
alog
tail -n +"$((before + 1))" "${TMP}/access.log" > "${TMP}/probe.log"
for p in "${probes[@]}"; do
    target="${p%%|*}"
    ok=no
    awk -v t="${target}" '$4=="TCP_DENIED/403" && $6=="CONNECT" && $7==t {f=1} END {exit !f}' \
        "${TMP}/probe.log" && ok=yes
    row "CONNECT ${target} logged TCP_DENIED/403" "${ok}" \
        "$(awk -v t="${target}" '$7==t {print $4, $6, $7}' "${TMP}/probe.log" | head -n 3 | tr '\n' ';')"
done

# 3. Every tunnel names a steady host, or a login host inside a logged login
# window. A tunnel's start is its log time minus its elapsed milliseconds.
docker exec "${PROXY}" cat /var/log/squid/profile.log > "${TMP}/profile.log" 2>/dev/null || : > "${TMP}/profile.log"
awk 'FNR==NR {pt[++n]=$1; pp[n]=$2; next}
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
ok=no; [ ! -s "${TMP}/bad-tunnels" ] && ok=yes
row "every TCP_TUNNEL names a steady host (or a login host in a login window)" "${ok}" \
    "$(tr '\n' ';' < "${TMP}/bad-tunnels")"

# 4. No attempt at all, allowed or denied, to a package or model registry.
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
