#!/usr/bin/env bash
# rung2.sh — rung-2 smoke, run inside the app container as user cloudtest:
#   docker exec -u cloudtest tm-trusty-cloud-cloudtest rung2.sh
# Needs check.sh to have run once (it stores the memory sentinel this reads back).
# Six steps, each PASS/FAIL; raw output goes to /data/logs/rung2-<ts>.log.
# Claude steps need a login: the interactive one (/data/home/.claude/.credentials.json,
# checked with test -s only, never read) or a token file. Without either they print
# SKIP, not FAIL. This script, its log and its output never hold a credential. Refs #11054
set -uo pipefail

TS="$(date -u +%Y%m%dT%H%M%SZ)"
LOG="/data/logs/rung2-${TS}.log"
mkdir -p /data/logs /data/work
: > "${LOG}"
FAILS=0
MCP_CONFIG="/data/work/rung2-mcp.json"

# result NAME RC OK — print PASS/FAIL and count failures.
result() {
    if [ "$3" = "yes" ]; then
        echo "PASS $1"
    else
        echo "FAIL $1 (rc=$2); see ${LOG}"
        FAILS=$((FAILS + 1))
    fi
}

# claude_step NAME REGEX ARGS... — run claude-run, PASS when it exits 0, the
# JSON says is_error false and the output matches REGEX.
claude_step() {
    local name="$1" re="$2" out rc=0 ok=no
    shift 2
    out="$(timeout 240 claude-run "$@" 2>&1)" || rc=$?
    { echo "=== ${name} (rc=${rc}) ==="; printf '%s\n' "${out}"; } >> "${LOG}"
    if [ "${rc}" -eq 0 ] && grep -q '"is_error":false' <<< "${out}" && grep -qE "${re}" <<< "${out}"; then
        ok=yes
    fi
    result "${name}" "${rc}" "${ok}"
}

# skip NAME REASON — print SKIP; not a failure.
skip() { echo "SKIP $1 (awaiting owner login)"; echo "=== $1 SKIP: awaiting owner login ===" >> "${LOG}"; }

# Login state: yes/no only. test -s never reads the credential file.
LOGGED_IN=no
if test -s /data/home/.claude/.credentials.json || test -s /run/secrets/claude.env; then LOGGED_IN=yes; fi
echo "claude logged-in: ${LOGGED_IN}"

echo "rung2.sh -> ${LOG}"

# 1. Plain headless prompt: proves the token, the proxy and api.anthropic.com.
if [ "${LOGGED_IN}" = yes ]; then
claude_step "claude -p READY" '"result":"[^"]*READY' \
    -p "Reply with the single word READY" --output-format json
else
    skip "claude -p READY"
fi

# 2. trusty-backed prompt through the in-container stdio servers. The memory
# server and tm run under `env -u` so the token cannot reach them.
cat > "${MCP_CONFIG}" <<'JSON'
{"mcpServers":{
 "tm":{"command":"env","args":["-u","CLAUDE_CODE_OAUTH_TOKEN","tm","serve","--stdio"]},
 "mem":{"command":"env","args":["-u","CLAUDE_CODE_OAUTH_TOKEN","trusty-memory","serve","--stdio","--palace","cloudtest"]},
 "search":{"command":"env","args":["-u","CLAUDE_CODE_OAUTH_TOKEN","trusty-search","serve"]}
}}
JSON
# The stored memory is "cloudtest cloudtest-sentinel-42xyz: remember this value".
# The prompt does not contain "42xyz", so a reply with it came from the tool.
if [ "${LOGGED_IN}" = yes ]; then
claude_step "claude -p + trusty memory_recall" '42xyz' \
    -p 'Call the mcp__mem__memory_recall tool with palace "cloudtest", query "remember this value", top_k 3. Then reply with the full text of the first memory it returns.' \
    --mcp-config "${MCP_CONFIG}" --strict-mcp-config \
    --allowedTools mcp__mem__memory_recall --output-format json
else
    skip "claude -p + trusty memory_recall"
fi

# 3. Negative: a non-allowlisted host through the proxy must fail with a 403.
out="$(curl -sS -m 10 https://example.com 2>&1)"; rc=$?
{ echo "=== proxy deny example.com (rc=${rc}) ==="; printf '%s\n' "${out}"; } >> "${LOG}"
ok=no; [ "${rc}" -ne 0 ] && grep -q '403' <<< "${out}" && ok=yes
result "proxy denies example.com" "${rc}" "${ok}"

# 4. Negative: a CONNECT to an IP literal must fail with a 403 (squid.conf -n).
out="$(curl -sS -m 10 https://1.1.1.1 2>&1)"; rc=$?
{ echo "=== proxy deny IP literal 1.1.1.1 (rc=${rc}) ==="; printf '%s\n' "${out}"; } >> "${LOG}"
ok=no; [ "${rc}" -ne 0 ] && grep -q '403' <<< "${out}" && ok=yes
result "proxy denies IP-literal CONNECT" "${rc}" "${ok}"

# 5. Negative: bypassing the proxy must fail (internal network has no route out).
out="$(curl -sS -m 10 --noproxy '*' https://api.anthropic.com 2>&1)"; rc=$?
{ echo "=== direct egress (rc=${rc}) ==="; printf '%s\n' "${out}"; } >> "${LOG}"
ok=no; [ "${rc}" -ne 0 ] && ok=yes
result "direct egress blocked" "${rc}" "${ok}"

# 6. The daemons must not hold the token. Counts variable NAMES only.
leaks=0
for p in /proc/[0-9]*; do
    [ -r "${p}/environ" ] || continue
    n="$(tr '\0' '\n' < "${p}/environ" 2>/dev/null | grep -c '^CLAUDE_CODE_OAUTH_TOKEN=')"
    leaks=$((leaks + n))
done
echo "=== token name present in ${leaks} process environment(s) ===" >> "${LOG}"
ok=no; [ "${leaks}" -eq 0 ] && ok=yes
result "no process environment holds the token" "${leaks}" "${ok}"

echo "log: ${LOG}"
echo "squid evidence: docker exec tm-trusty-cloud-egress cat /var/log/squid/access.log"
if [ "${FAILS}" -gt 0 ]; then
    echo "RESULT: ${FAILS} step(s) FAILED"
    exit 1
fi
echo "RESULT: all steps PASSED"
