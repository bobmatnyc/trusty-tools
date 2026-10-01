#!/usr/bin/env bash
# smoke.sh — E2E smoke tests run inside the trusty-e2e Docker container.
#
# Executed by the Dockerfile ENTRYPOINT; also callable directly for debugging.
# Each scenario is self-contained: start daemons, exercise the tool, assert
# expected output, stop daemons, report PASS / FAIL / SKIP.
#
# Exit codes:
#   0  — all mandatory scenarios passed (SKIP is not a failure)
#   1  — one or more scenarios failed
#
# Environment (set by Dockerfile or caller):
#   TRUSTY_SKIP_RAM_CHECK=1   — bypass 16 GB RAM guard (required in Docker)
#   XDG_DATA_HOME             — daemon state root (default: /tmp/trusty-data)
#   HOME                      — required for path expansion (default: /root)
#   E2E_LOG_DIR               — directory for per-tool log files; bind-mounted
#                               by the CI runner so logs survive container exit
#                               (default: /tmp/e2e-logs)
#   TA_PORT                   — trusty-analyze HTTP port (default: 7879)

set -euo pipefail

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0
FAILURES=()

pass() { echo "  [PASS] $1"; PASS_COUNT=$((PASS_COUNT + 1)); }
fail() { echo "  [FAIL] $1"; FAIL_COUNT=$((FAIL_COUNT + 1)); FAILURES+=("$1"); }
skip() { echo "  [SKIP] $1"; SKIP_COUNT=$((SKIP_COUNT + 1)); }

section() { echo ""; echo "=== $1 ==="; }

# Wait up to $2 seconds for $1 (HTTP URL) to return 200.
wait_http() {
    local url="$1"
    local timeout="${2:-30}"
    local elapsed=0
    while ! curl -sf "${url}" > /dev/null 2>&1; do
        sleep 1
        elapsed=$((elapsed + 1))
        if [ "${elapsed}" -ge "${timeout}" ]; then
            echo "  ERROR: ${url} did not become healthy within ${timeout}s" >&2
            return 1
        fi
    done
}

# run_capture VAR LABEL CMD... — run CMD, store its stdout+stderr in VAR.
# On a non-zero exit, print the captured output (so CI shows the error) and
# return CMD's exit code. Call as `if ! run_capture ...; then fail ...; fi`:
# a bare call under `set -e` would end the script. #8937: a bare
# `VAR="$(cmd 2>&1)"` died before the output was ever printed.
run_capture() {
    local __var="$1" label="$2"
    shift 2
    local out rc=0
    out="$("$@" 2>&1)" || rc=$?
    printf -v "${__var}" '%s' "${out}"
    if [ "${rc}" -ne 0 ]; then
        echo "  --- ${label} output (exit ${rc}) ---"
        echo "${out}"
        echo "  --- end ---"
    fi
    return "${rc}"
}

# Wait up to $2 seconds for $1 (a Unix socket path) to exist.
# #8937: trusty-memory and trusty-analyze serve a socket only (ADR-0032).
wait_socket() {
    local sock="$1"
    local timeout="${2:-30}"
    local elapsed=0
    while [ ! -S "${sock}" ]; do
        sleep 1
        elapsed=$((elapsed + 1))
        if [ "${elapsed}" -ge "${timeout}" ]; then
            echo "  ERROR: socket ${sock} did not appear within ${timeout}s" >&2
            return 1
        fi
    done
}

# memory_tool_call TOOL ARGS_JSON — one MCP stdio session against
# trusty-memory: initialize, then a single tools/call; prints the JSON-RPC
# replies. The sleep keeps stdin open until the reply arrives.
memory_tool_call() {
    local tool="$1" args="$2"
    local init='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e-smoke","version":"0"}}}'
    local inited='{"jsonrpc":"2.0","method":"notifications/initialized"}'
    local call
    call="{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"${tool}\",\"arguments\":${args}}}"
    { printf '%s\n' "${init}" "${inited}" "${call}"; sleep 4; } \
        | trusty-memory serve --stdio --palace personal
}

# mcp_result_ok OUTPUT — true when the id-2 reply is a result, not an error.
mcp_result_ok() {
    local reply
    reply="$(echo "$1" | grep '"id":2' || true)"
    [ -n "${reply}" ] && ! echo "${reply}" | grep -q '"error"\|"isError":true'
}

# Compare semver: returns 0 (true) if $1 >= $2.
# Works for simple X.Y.Z strings without pre-release suffixes.
semver_gte() {
    local a="$1" b="$2"
    # Sort the two versions; if $a comes last (or equals $b) it is >= $b.
    local sorted
    sorted=$(printf '%s\n%s\n' "$a" "$b" | sort -V | tail -1)
    [ "$sorted" = "$a" ]
}

# Log directory: bind-mounted by CI so logs survive the --rm container exit.
E2E_LOG_DIR="${E2E_LOG_DIR:-/tmp/e2e-logs}"
mkdir -p "${XDG_DATA_HOME:-/tmp/trusty-data}" "${E2E_LOG_DIR}"

# trusty-analyze HTTP port (configurable for parallel test runs).
TA_PORT="${TA_PORT:-7879}"

# ---------------------------------------------------------------------------
# SCENARIO 1: trusty-search
# ---------------------------------------------------------------------------
section "Scenario 1: trusty-search"

TS_BIN="$(command -v trusty-search)"
TS_VERSION="$(trusty-search --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')"
echo "  Binary : ${TS_BIN}"
echo "  Version: ${TS_VERSION}"

# Start daemon in background (foreground flag keeps it in-process).
TRUSTY_SKIP_RAM_CHECK=1 trusty-search start > "${E2E_LOG_DIR}/ts.log" 2>&1 &
TS_PID=$!

# Wait for HTTP to come up on the auto-selected port.
sleep 3
TS_PORT="$(trusty-search port 2>/dev/null || echo '7878')"
echo "  Port   : ${TS_PORT}"

if wait_http "http://127.0.0.1:${TS_PORT}/health" 30; then
    pass "trusty-search daemon healthy"

    # Create a lexical-only index over the fixture repo (no ONNX required).
    # IMPORTANT: the directory must NOT be under a path component named "fixtures"
    # (trusty-search's walker skips dirs named "fixtures" — see SKIP_DIRS in
    # crates/trusty-search/src/service/walker.rs). We use /e2e/sample-code.
    FIXTURE_DIR="/e2e/sample-code"
    INDEX_ID="smoke-fixture"

    # #8937: indexing is default-deny (#767). Approve the fixture root with the
    # supported verb before indexing; the allowlist persists under $HOME, so
    # scenario 4 reuses it.
    echo "  Approving ${FIXTURE_DIR} for indexing ..."
    if ! run_capture APPROVE_LOG "trusty-search index add" \
        trusty-search index add "${FIXTURE_DIR}" --name "${INDEX_ID}"; then
        fail "trusty-search index add ${FIXTURE_DIR} failed (see output above)"
    fi

    echo "  Indexing ${FIXTURE_DIR} ..."
    INDEX_OK=1
    if ! run_capture INDEX_LOG "trusty-search index" \
        trusty-search index "${FIXTURE_DIR}" --name "${INDEX_ID}" --lexical-only; then
        INDEX_OK=0
        fail "trusty-search index failed (non-zero exit, see output above)"
    else
        echo "  Index output: ${INDEX_LOG}"
        if echo "${INDEX_LOG}" | grep -q "chunks"; then
            pass "trusty-search index created"
        else
            fail "trusty-search index failed (no chunks in output)"
        fi
    fi

    # Run a query and assert we get a hit on 'authenticate' using the CLI.
    # (The /grep HTTP endpoint requires POST with JSON body; the CLI `query`
    # subcommand is simpler and always works regardless of lexical/semantic mode.)
    if [ "${INDEX_OK}" -eq 1 ]; then
        echo "  Running query for 'authenticate' ..."
        if ! run_capture QUERY_OUT "trusty-search query" \
            trusty-search query 'authenticate' --index "${INDEX_ID}"; then
            fail "trusty-search query failed (non-zero exit, see output above)"
        else
            echo "  Query output (first 5 lines):"
            echo "${QUERY_OUT}" | head -5
            if echo "${QUERY_OUT}" | grep -qi "authenticate\|auth\.rs"; then
                pass "trusty-search query returned results for 'authenticate'"
            else
                fail "trusty-search search returned no results for 'authenticate'"
            fi
        fi
    else
        skip "trusty-search query skipped: index was not created"
    fi

    # -------------------------------------------------------------------------
    # Indexing-hygiene assertion (version-gated: requires >= 0.25.0)
    # -------------------------------------------------------------------------
    HYGIENE_MIN_VERSION="0.25.0"
    if semver_gte "${TS_VERSION}" "${HYGIENE_MIN_VERSION}"; then
        echo "  Running indexing-hygiene assertion (${TS_VERSION} >= ${HYGIENE_MIN_VERSION}) ..."

        # The fixture data/ directory contains a >64KiB JSON file.
        # With hygiene defaults, the data/ dir and large JSON files should be
        # excluded from the index. Verify by checking listed chunks for the file.
        CHUNKS_OUT="$(curl -sf "http://127.0.0.1:${TS_PORT}/indexes/${INDEX_ID}/chunks?limit=1000" 2>/dev/null || echo '{}')"

        DATA_JSON_HITS="$(echo "${CHUNKS_OUT}" | grep -c 'large_dataset\.json' || true)"
        if [ "${DATA_JSON_HITS}" -eq 0 ]; then
            pass "hygiene: data/large_dataset.json excluded from index (>64KiB .json in data/)"
        else
            fail "hygiene: data/large_dataset.json was NOT excluded — hygiene defaults missing"
        fi
    else
        skip "hygiene assertion requires trusty-search >= ${HYGIENE_MIN_VERSION}, installed ${TS_VERSION} — skipping"
    fi
else
    fail "trusty-search daemon did not start"
    kill "${TS_PID}" 2>/dev/null || true
fi

# Stop trusty-search.
trusty-search stop > /dev/null 2>&1 || kill "${TS_PID}" 2>/dev/null || true
wait "${TS_PID}" 2>/dev/null || true
echo "  Daemon stopped."

# ---------------------------------------------------------------------------
# SCENARIO 2: trusty-memory
# ---------------------------------------------------------------------------
section "Scenario 2: trusty-memory"

TM_BIN="$(command -v trusty-memory)"
TM_VERSION="$(trusty-memory --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')"
echo "  Binary : ${TM_BIN}"
echo "  Version: ${TM_VERSION}"

# Start the daemon in the foreground. Since #6286 (ADR-0032) it serves a Unix
# socket only; --http is ignored, so there is no HTTP /health. #8937: readiness
# is the socket appearing, and tool calls go through `serve --stdio`.
TM_SOCK="${XDG_DATA_HOME:-/tmp/trusty-data}/trusty-memory/trusty-memory.sock"
trusty-memory serve --foreground > "${E2E_LOG_DIR}/tm.log" 2>&1 &
TM_PID=$!

if wait_socket "${TM_SOCK}" 60; then
    pass "trusty-memory daemon healthy (socket ${TM_SOCK})"
else
    echo "  --- daemon log ---"
    cat "${E2E_LOG_DIR}/tm.log"
    echo "  --- end ---"
    fail "trusty-memory daemon did not start"
    kill "${TM_PID}" 2>/dev/null || true
    TM_PID=""
fi

if [ -n "${TM_PID}" ]; then
    # Create 'personal' palace (force: skip project-slug validation).
    echo "  Creating palace 'personal' ..."
    if run_capture CREATE_OUT "palace_create" memory_tool_call palace_create \
        '{"name":"personal","force":true}' \
        && mcp_result_ok "${CREATE_OUT}"; then
        pass "trusty-memory palace created"
    else
        echo "  Create response: ${CREATE_OUT:-<none>}"
        fail "trusty-memory palace create failed"
    fi

    # Store a memory in the palace.
    echo "  Storing memory ..."
    if run_capture REMEMBER_OUT "memory_remember" memory_tool_call memory_remember \
        '{"palace":"personal","force":true,"text":"trusty-memory smoke test: remember this sentinel value 42xyzABC"}' \
        && mcp_result_ok "${REMEMBER_OUT}"; then
        pass "trusty-memory memory stored"
    else
        echo "  Remember response: ${REMEMBER_OUT:-<none>}"
        fail "trusty-memory memory store failed"
    fi

    # Recall and assert the sentinel text comes back.
    echo "  Recalling memory ..."
    sleep 2  # Allow indexing to complete before recall.
    if run_capture RECALL_OUT "memory_recall" memory_tool_call memory_recall \
        '{"palace":"personal","query":"sentinel value 42xyzABC","top_k":5}' \
        && echo "${RECALL_OUT}" | grep -q '42xyzABC\|sentinel'; then
        pass "trusty-memory recall returned stored text"
    else
        echo "  Recall response: ${RECALL_OUT:-<none>}"
        fail "trusty-memory recall did not return stored text"
    fi

    kill "${TM_PID}" 2>/dev/null || true
    wait "${TM_PID}" 2>/dev/null || true
    echo "  Daemon stopped."
fi

# ---------------------------------------------------------------------------
# SCENARIO 3: trusty-mpm
# ---------------------------------------------------------------------------
section "Scenario 3: trusty-mpm"

MPM_BIN="$(command -v tm || command -v trusty-mpm)"
MPM_VERSION="$(${MPM_BIN} --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')"
echo "  Binary : ${MPM_BIN}"
echo "  Version: ${MPM_VERSION}"

# Verify both installed binaries exist (single-install convention).
if command -v tm > /dev/null 2>&1 && command -v trusty-mpm > /dev/null 2>&1; then
    pass "trusty-mpm: both 'tm' and 'trusty-mpm' binaries installed"
else
    MISSING=""
    command -v tm > /dev/null 2>&1 || MISSING="${MISSING} tm"
    command -v trusty-mpm > /dev/null 2>&1 || MISSING="${MISSING} trusty-mpm"
    fail "trusty-mpm: missing binaries:${MISSING}"
fi

# Verify version output is non-empty.
MPM_VER_OUT="$(tm --version 2>&1)"
echo "  Version output: ${MPM_VER_OUT}"
if [ -n "${MPM_VER_OUT}" ]; then
    pass "trusty-mpm: --version returned non-empty output"
else
    fail "trusty-mpm: --version returned empty output"
fi

# Verify --help exits cleanly (non-zero is expected for --help on some CLIs,
# so we capture stderr and check for expected content instead of exit code).
HELP_OUT="$(tm --help 2>&1 || true)"
echo "  Help output (first 3 lines): $(echo "${HELP_OUT}" | head -3)"
if echo "${HELP_OUT}" | grep -qi "usage\|subcommand\|daemon\|session\|mpm\|trusty"; then
    pass "trusty-mpm: --help output contains expected content"
else
    fail "trusty-mpm: --help output does not contain expected content"
fi

# Start the daemon and verify it comes up.
echo "  Starting tm daemon ..."
tm start > "${E2E_LOG_DIR}/mpm.log" 2>&1 &
MPM_DAEMON_PID=$!
sleep 5

STATUS_OUT="$(tm status 2>&1 || true)"
echo "  Status: ${STATUS_OUT}"
if echo "${STATUS_OUT}" | grep -qi "running\|ok\|active\|daemon\|version\|sessions"; then
    pass "trusty-mpm: daemon start + status ok"
else
    # Status might exit non-zero if daemon is not up — that counts as fail.
    echo "  --- daemon log ---"
    cat "${E2E_LOG_DIR}/mpm.log"
    echo "  --- end ---"
    fail "trusty-mpm: daemon status did not indicate running"
fi

tm stop > /dev/null 2>&1 || kill "${MPM_DAEMON_PID}" 2>/dev/null || true
wait "${MPM_DAEMON_PID}" 2>/dev/null || true
echo "  Daemon stopped."

# ---------------------------------------------------------------------------
# SCENARIO 4: trusty-analyze
# ---------------------------------------------------------------------------
section "Scenario 4: trusty-analyze"

TA_BIN="$(command -v trusty-analyze)"
TA_VERSION="$(trusty-analyze --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')"
echo "  Binary : ${TA_BIN}"
echo "  Version: ${TA_VERSION}"

# trusty-analyze requires a running trusty-search daemon.
echo "  Starting trusty-search for analyze scenario ..."
TRUSTY_SKIP_RAM_CHECK=1 trusty-search start > "${E2E_LOG_DIR}/ts2.log" 2>&1 &
TS2_PID=$!
sleep 3
TS2_PORT="$(trusty-search port 2>/dev/null || echo '7878')"
echo "  trusty-search port: ${TS2_PORT}"

if ! wait_http "http://127.0.0.1:${TS2_PORT}/health" 30; then
    fail "trusty-search dependency for analyze did not start"
    echo "  --- ts log ---"
    cat "${E2E_LOG_DIR}/ts2.log"
    echo "  --- end ---"
    kill "${TS2_PID}" 2>/dev/null || true
    TS2_PID=""
fi

# Index the fixture for analyze to use.
if [ -n "${TS2_PID}" ]; then
    # #8937: the registry persists across daemon restarts and a root may belong
    # to one index only (#2336, #3993). /e2e/sample-code is already owned by
    # scenario 1's "smoke-fixture", so a second name got "409 Conflict".
    # Re-registering the same name over the same root is idempotent.
    ANALYZE_INDEX="smoke-fixture"
    if ! run_capture TS2_INDEX_LOG "trusty-search index (analyze)" \
        trusty-search index "/e2e/sample-code" --name "${ANALYZE_INDEX}" --lexical-only; then
        fail "trusty-search index for analyze failed (see output above)"
    fi
    echo "${TS2_INDEX_LOG}" > "${E2E_LOG_DIR}/ts2-index.log"

    # Start trusty-analyze daemon.
    # Note: --search-url is a GLOBAL flag (before the subcommand), not a serve flag.
    # Use the TRUSTY_SEARCH_URL env var to keep the invocation readable.
    # #8937: the daemon serves a Unix socket only (ADR-0032); TA_PORT is unused.
    TA_SOCK="${XDG_DATA_HOME:-/tmp/trusty-data}/trusty-analyze/trusty-analyze.sock"
    echo "  Starting trusty-analyze daemon (socket ${TA_SOCK}) ..."
    TRUSTY_SEARCH_URL="http://127.0.0.1:${TS2_PORT}" \
        trusty-analyze serve --foreground > "${E2E_LOG_DIR}/ta.log" 2>&1 &
    TA_PID=$!

    if wait_socket "${TA_SOCK}" 30; then
        pass "trusty-analyze daemon healthy"
    else
        echo "  --- analyze log ---"
        cat "${E2E_LOG_DIR}/ta.log"
        echo "  --- end ---"
        fail "trusty-analyze daemon did not start"
        kill "${TA_PID}" 2>/dev/null || true
        TA_PID=""
    fi

    if [ -n "${TA_PID}" ]; then
        # Run one-shot complexity analysis on the fixture index.
        echo "  Running trusty-analyze analyze ${ANALYZE_INDEX} ..."
        ANALYZE_OUT="$(trusty-analyze analyze "${ANALYZE_INDEX}" --top-k 5 2>&1 || true)"
        echo "  Analyze output:"
        echo "${ANALYZE_OUT}" | head -20

        if echo "${ANALYZE_OUT}" | grep -qiE "chunk|file|complex|grade|smell|analy"; then
            pass "trusty-analyze: analyze returned structured output"
        else
            fail "trusty-analyze: analyze returned no recognizable output"
        fi

        # `status` probes the socket and prints DOWN (exit 0) when nothing answers.
        if run_capture TA_STATUS "trusty-analyze status" trusty-analyze status \
            && ! echo "${TA_STATUS}" | grep -q 'DOWN'; then
            echo "  Status: ${TA_STATUS}"
            pass "trusty-analyze: status reports the daemon up"
        else
            echo "  Status: ${TA_STATUS:-<none>}"
            fail "trusty-analyze: status does not report the daemon up"
        fi

        kill "${TA_PID}" 2>/dev/null || true
        wait "${TA_PID}" 2>/dev/null || true
    fi

    trusty-search stop > /dev/null 2>&1 || kill "${TS2_PID}" 2>/dev/null || true
    wait "${TS2_PID}" 2>/dev/null || true
    echo "  Daemons stopped."
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "=========================================="
echo "  E2E Smoke Test Summary"
echo "=========================================="
echo "  PASS: ${PASS_COUNT}"
echo "  FAIL: ${FAIL_COUNT}"
echo "  SKIP: ${SKIP_COUNT}"
if [ "${#FAILURES[@]}" -gt 0 ]; then
    echo ""
    echo "  Failed assertions:"
    for f in "${FAILURES[@]}"; do
        echo "    - ${f}"
    done
fi
echo "=========================================="

if [ "${FAIL_COUNT}" -gt 0 ]; then
    exit 1
fi
exit 0
