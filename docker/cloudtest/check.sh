#!/usr/bin/env bash
# check.sh [--verify-persist] — run inside the container:
#   docker exec tm-trusty-cloud-cloudtest check.sh
#   docker exec tm-trusty-cloud-cloudtest check.sh --verify-persist   (after restart)
# Every step prints PASS/FAIL; raw output goes to /data/logs/check-<ts>.log.
# Runs every step, exits non-zero if any FAILed. Refs #11054
set -uo pipefail

MODE="full"
[ "${1:-}" = "--verify-persist" ] && MODE="persist"

TS="$(date -u +%Y%m%dT%H%M%SZ)"
LOG="/data/logs/check-${TS}.log"
mkdir -p /data/logs /data/work
: > "${LOG}"

WORK="/data/work/cloudtest-demo"      # not under /tmp: trusty-search denies it
INDEX="cloudtest-demo"
PALACE="cloudtest"
SENTINEL="cloudtest-sentinel-42xyz"
SEARCH_MODE_FILE="/data/work/search-mode"
FAILS=0

# step NAME REGEX CMD... — PASS when CMD exits 0 and (REGEX empty or matches).
# Output is appended to the log (no timeout here: CMD may be a shell function); the first lines of a failure are echoed.
step() {
    local name="$1" re="$2" out rc=0
    shift 2
    out="$("$@" 2>&1)" || rc=$?
    {
        echo "=== ${name} (rc=${rc}) ==="
        echo "\$ $*"
        printf '%s\n' "${out}"
    } >> "${LOG}"
    if [ "${rc}" -eq 0 ] && { [ -z "${re}" ] || grep -qE "${re}" <<< "${out}"; }; then
        echo "PASS ${name}"
        return 0
    fi
    echo "FAIL ${name} (rc=${rc})"
    printf '%s\n' "${out}" | head -n 8 | sed 's/^/    /'
    FAILS=$((FAILS + 1))
    return 1
}

# mcp BIN_AND_ARGS... : reads METHOD and PARAMS from env MCP_METHOD / MCP_PARAMS.
# One stdio session: initialize, initialized, one request. Succeeds when the
# id-2 reply is a result.
mcp() {
    local init='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"cloudtest-check","version":"0"}}}'
    local inited='{"jsonrpc":"2.0","method":"notifications/initialized"}'
    local call="{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"${MCP_METHOD}\",\"params\":${MCP_PARAMS}}"
    local out reply
    out="$({ printf '%s\n' "${init}" "${inited}" "${call}"; sleep 6; } | "$@" 2>&1)" || true
    printf '%s\n' "${out}"
    reply="$(grep '"id":2' <<< "${out}" || true)"
    [ -n "${reply}" ] && ! grep -qE '"error"|"isError":true' <<< "${reply}"
}
mcp_list() { MCP_METHOD=tools/list MCP_PARAMS='{}' mcp "$@"; }
mcp_tool() { # BIN... -- TOOL ARGS_JSON handled by callers via env
    MCP_METHOD=tools/call MCP_PARAMS="{\"name\":\"${MCP_TOOL}\",\"arguments\":${MCP_ARGS}}" mcp "$@"
}
memory_tool() { MCP_TOOL="$1" MCP_ARGS="$2" mcp_tool trusty-memory serve --stdio --palace "${PALACE}"; }

# Wait up to $2 s for a command to succeed.
wait_for() {
    local i=0
    until eval "$1" >/dev/null 2>&1; do
        i=$((i + 1)); [ "${i}" -ge "$2" ] && return 1; sleep 1
    done
}

recall_step() {
    sleep 2
    step "memory_recall" "${SENTINEL}" \
        memory_tool memory_recall "{\"palace\":\"${PALACE}\",\"query\":\"${SENTINEL}\",\"top_k\":5}"
}
query_step() {
    step "search query" "cloudtest_sentinel_fn" \
        trusty-search query cloudtest_sentinel_fn --index "${INDEX}"
}

echo "check.sh ${MODE} -> ${LOG}"

# shellcheck disable=SC2016  # evaluated by wait_for
step "daemons up (sockets)" "" wait_for \
    '[ -S "${XDG_DATA_HOME}/trusty-memory/trusty-memory.sock" ] && [ -S "${XDG_DATA_HOME}/trusty-search/trusty-search.sock" ] && tm status' 90

if [ "${MODE}" = "persist" ]; then
    recall_step
    query_step
    echo "search mode recorded by first run: $(cat "${SEARCH_MODE_FILE}" 2>/dev/null || echo unknown)"
else
    step "tm doctor" "" timeout 300 tm doctor
    step "mcp tm tools/list" '"tools"' mcp_list tm serve --stdio
    step "mcp trusty-memory tools/list" '"tools"' mcp_list trusty-memory serve --stdio --palace "${PALACE}"
    step "mcp trusty-search tools/list" '"tools"' mcp_list trusty-search serve

    # Local demo repo for project registration and indexing.
    if [ ! -d "${WORK}/.git" ]; then
        mkdir -p "${WORK}/src"
        printf 'pub fn cloudtest_sentinel_fn() -> u32 {\n    42\n}\n' > "${WORK}/src/lib.rs"
        printf '# cloudtest-demo\nDemo repo for the headless stack check.\n' > "${WORK}/README.md"
        git -C "${WORK}" init -q -b main \
            && git -C "${WORK}" add -A \
            && git -C "${WORK}" -c user.name=cloudtest -c user.email=cloudtest@localhost \
                commit -q -m "demo repo" || echo "demo repo setup failed" >> "${LOG}"
    fi
    step "tm projects register" "cloudtest-demo" \
        tm projects register cloudtest-demo --repo-url "file://${WORK}"

    step "memory palace_create" "" \
        memory_tool palace_create "{\"name\":\"${PALACE}\",\"force\":true}"
    step "memory_remember" "" \
        memory_tool memory_remember "{\"palace\":\"${PALACE}\",\"force\":true,\"text\":\"cloudtest ${SENTINEL}: remember this value\"}"
    recall_step

    step "search index add" "" trusty-search index add "${WORK}" --name "${INDEX}"
    # Try semantic first (model from FASTEMBED_CACHE_DIR, no network); fall back
    # to lexical-only and report which one ran.
    if step "search index (semantic)" "" timeout 600 trusty-search index "${WORK}" --name "${INDEX}" --force; then
        echo semantic > "${SEARCH_MODE_FILE}"
        echo "INFO search mode: semantic"
    else
        FAILS=$((FAILS - 1))   # the fallback decides; the semantic miss is reported, not fatal
        echo "INFO semantic index failed; falling back to --lexical-only"
        if step "search index (lexical-only)" "" timeout 600 trusty-search index "${WORK}" --name "${INDEX}" --force --lexical-only; then
            echo lexical-only > "${SEARCH_MODE_FILE}"
            echo "INFO search mode: lexical-only (semantic unavailable)"
        fi
    fi
    query_step
fi

echo "log: ${LOG}"
if [ "${FAILS}" -gt 0 ]; then
    echo "RESULT: ${FAILS} step(s) FAILED"
    exit 1
fi
echo "RESULT: all steps PASSED"
