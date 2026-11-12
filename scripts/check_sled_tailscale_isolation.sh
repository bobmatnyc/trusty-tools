#!/usr/bin/env bash
#
# check_sled_tailscale_isolation.sh [DIR] — static check that the sled's
# tailscaled runs where the sled's agents cannot control it (#11156 B2).
#
# Why: tailscaled and its LocalAPI socket once ran as the `sled` uid, the uid
#   every agent and Claude Code process runs as. Any of them could then run
#   `tailscale up --auth-key=<other>`, `tailscale set --ssh`, `tailscale
#   funnel` or `tailscale nc`, which breaks DOC-90 IN-3 and IN-7. This test
#   reads the build files, so it runs without docker on a Mac and in CI.
# What: reads DIR (default docker/sled) and asserts:
#   - entrypoint.sh starts no tailscaled;
#   - no script that runs in the app container calls a state-changing
#     `tailscale` subcommand;
#   - the app image (the Dockerfile's last stage) installs no tailscaled and
#     creates no /var/run/tailscale;
#   - a `tailscale-sidecar` stage runs tailscaled as a user that is not
#     `sled` or root, with a 0700 socket directory that user owns, and a uid
#     whose default differs from the app UID and that the build refuses when
#     equal;
#   - README.md starts the sidecar in the app's network namespace.
#   A missing file is a FAIL. Prints PASS or FAIL per row; exits 1 on any FAIL.
# Test: scripts/check_sled_tailscale_isolation.sh is itself the regression
#   test; .github/workflows/sled-static.yml runs it.
#
# Usage: bash scripts/check_sled_tailscale_isolation.sh [DIR]
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); POSIX awk and grep.

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="${1:-${ROOT}/docker/sled}"
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

# code FILE — FILE without comment lines, continuation lines joined.
code() {
    awk '/^[[:space:]]*#/ {next}
         { if (sub(/\\[[:space:]]*$/, "")) { buf = buf $0 " "; next }
           print buf $0; buf = "" }
         END { if (buf != "") print buf }' "$1"
}

for f in Dockerfile entrypoint.sh exercise.sh check.sh claude-run gh-token-run README.md; do
    if [ ! -r "${DIR}/${f}" ]; then
        row "${f} is readable" no "missing in ${DIR}"
        echo "RESULT: ${FAILS} row(s) FAILED"
        exit 1
    fi
done

TMP="$(mktemp -d "${TMPDIR:-/tmp}/sled-ts.XXXXXX")"
trap 'rm -rf "${TMP}"' EXIT
code "${DIR}/Dockerfile" > "${TMP}/df"

# stage NAME — the logical lines of the Dockerfile stage NAME ("" = the last).
stage() {
    awk -v want="$1" '
        toupper($1) == "FROM" {
            n++; name = ""
            for (i = 2; i < NF; i++) if (toupper($i) == "AS") name = $(i + 1)
            names[n] = name
        }
        n { body[n] = body[n] $0 "\n" }
        END {
            pick = n
            if (want != "") { pick = 0; for (i = 1; i <= n; i++) if (names[i] == want) pick = i }
            if (pick) printf "%s", body[pick]
        }' "${TMP}/df"
}

# 1. The app container starts no tailscaled.
hits="$(code "${DIR}/entrypoint.sh" | grep -nE '(^|[^[:alnum:]_-])tailscaled([^[:alnum:]_-]|$)')"
ok=no; [ -z "${hits}" ] && ok=yes
row "entrypoint.sh starts no tailscaled" "${ok}" "$(tr '\n' ';' <<< "${hits}")"

# 2. No script that runs in the app changes Tailscale state.
TS_RE='(^|[^[:alnum:]_-])tailscale[[:space:]]+(up|login|logout|set|switch|funnel|serve|nc|cert|lock)([^[:alnum:]_-]|$)'
hits=""
for f in entrypoint.sh exercise.sh check.sh claude-run gh-token-run; do
    h="$(code "${DIR}/${f}" | grep -nE "${TS_RE}" | sed "s#^#${f}:#" | tr '\n' ';')"
    [ -n "${h}" ] && hits="${hits}${h}"
done
ok=no; [ -z "${hits}" ] && ok=yes
row "no app-side script runs a state-changing tailscale command" "${ok}" "${hits}"

# 3. The app image holds no tailscaled and no tailscaled socket directory.
stage "" > "${TMP}/app"
daemon_stages="$(awk 'toupper($1) == "FROM" { name = ""
                        for (i = 2; i < NF; i++) if (toupper($i) == "AS") name = $(i + 1) }
                      /tailscaled/ && name != "" { print name }' "${TMP}/df" | sort -u)"
hits="$(grep -nE '(^|[^[:alnum:]_-])tailscaled([^[:alnum:]_-]|$)' "${TMP}/app")"
# A COPY of a whole directory from a stage that builds tailscaled copies it too.
for s in ${daemon_stages}; do
    h="$(awk -v s="${s}" 'toupper($1) == "COPY" && $2 == "--from=" s {
            for (i = 3; i < NF; i++) if ($i ~ /\/$/) print "COPY --from=" s " " $i }' "${TMP}/app")"
    [ -n "${h}" ] && hits="${hits}${hits:+;}${h}"
done
ok=no; [ -z "${hits}" ] && ok=yes
row "the app image installs no tailscaled" "${ok}" "$(tr '\n' ';' <<< "${hits}")"
hits="$(grep -nE '/(var/)?run/tailscale' "${TMP}/app")"
ok=no; [ -z "${hits}" ] && ok=yes
row "the app image creates no /var/run/tailscale" "${ok}" "$(tr '\n' ';' <<< "${hits}")"

# 4. tailscaled runs in its own stage, as its own non-root user.
stage tailscale-sidecar > "${TMP}/side"
if [ ! -s "${TMP}/side" ]; then
    row "Dockerfile has a tailscale-sidecar stage" no
else
    row "Dockerfile has a tailscale-sidecar stage" yes
    user="$(awk 'toupper($1) == "USER" {u = $2} END {print u}' "${TMP}/side")"
    ok=no
    case "${user}" in ''|sled|sled:*|root|root:*|0|0:*) ;; *) ok=yes ;; esac
    row "sidecar USER is neither sled nor root (${user:-none})" "${ok}"
    ok=no
    grep -qE "install -d -m 0?700 -o ${user} -g [^ ]+ .*/var/run/tailscale" "${TMP}/side" && ok=yes
    row "sidecar socket dir /var/run/tailscale is 0700 and owned by ${user:-?}" "${ok}"
    ep="$(grep -E '^ENTRYPOINT' "${TMP}/side")"
    ok=no
    grep -q 'tailscaled' <<< "${ep}" && grep -q -- '--socket=/var/run/tailscale/' <<< "${ep}" \
        && grep -q -- '--tun=userspace-networking' <<< "${ep}" && ok=yes
    row "sidecar ENTRYPOINT runs tailscaled on /var/run/tailscale in userspace mode" "${ok}" "${ep}"
    ts_uid="$(sed -nE 's/^ARG TS_UID=([0-9]+).*/\1/p' "${TMP}/side" | tail -n 1)"
    app_uid="$(sed -nE 's/^ARG UID=([0-9]+).*/\1/p' "${TMP}/app" | tail -n 1)"
    ok=no
    [ -n "${ts_uid}" ] && [ -n "${app_uid}" ] && [ "${ts_uid}" != 0 ] \
        && [ "${ts_uid}" != "${app_uid}" ] && ok=yes
    row "sidecar TS_UID default (${ts_uid:-?}) is not 0 and differs from app UID (${app_uid:-?})" "${ok}"
    ok=no
    grep -qE '"\$\{TS_UID\}" -eq "\$\{UID\}"' "${TMP}/side" && ok=yes
    row "sidecar build refuses TS_UID equal to the app UID" "${ok}"
fi

# 5. The README starts the sidecar in the app's network namespace.
ok=no
grep -qE -- '--network container:tm-trusty-cloud-sled([^-]|$)' "${DIR}/README.md" && ok=yes
row "README.md starts the sidecar with --network container:tm-trusty-cloud-sled" "${ok}"

if [ "${FAILS}" -gt 0 ]; then
    echo "RESULT: ${FAILS} row(s) FAILED"
    exit 1
fi
echo "RESULT: sled tailscale isolation PASSED"
