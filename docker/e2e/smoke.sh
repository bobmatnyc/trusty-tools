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

# Compare semver: returns 0 (true) if $1 >= $2.
# Works for simple X.Y.Z strings without pre-release suffixes.
semver_gte() {
    local a="$1" b="$2"
    # Sort the two versions; if $a comes last (or equals $b) it is >= $b.
    local sorted
    sorted=$(printf '%s\n%s\n' "$a" "$b" | sort -V | tail -1)
    [ "$sorted" = "$a" ]
}

mkdir -p "${XDG_DATA_HOME:-/tmp/trusty-data}"

# ---------------------------------------------------------------------------
# SCENARIO 1: trusty-search
# ---------------------------------------------------------------------------
section "Scenario 1: trusty-search"

TS_BIN="$(command -v trusty-search)"
TS_VERSION="$(trusty-search --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')"
echo "  Binary : ${TS_BIN}"
echo "  Version: ${TS_VERSION}"

# Start daemon in background (foreground flag keeps it in-process).
TRUSTY_SKIP_RAM_CHECK=1 trusty-search start > /tmp/ts.log 2>&1 &
TS_PID=$!

# Wait for HTTP to come up on the auto-selected port.
sleep 3
TS_PORT="$(trusty-search port 2>/dev/null || echo '7878')"
echo "  Port   : ${TS_PORT}"

if wait_http "http://127.0.0.1:${TS_PORT}/health" 30; then
    pass "trusty-search daemon healthy"
else
    fail "trusty-search daemon did not start"
    kill "${TS_PID}" 2>/dev/null || true
fi

# Create a lexical-only index over the fixture repo (no ONNX required).
# IMPORTANT: the directory must NOT be under a path component named "fixtures"
# (trusty-search's walker skips dirs named "fixtures" — see SKIP_DIRS in
# crates/trusty-search/src/service/walker.rs). We use /e2e/sample-code.
FIXTURE_DIR="/e2e/sample-code"
INDEX_ID="smoke-fixture"

echo "  Indexing ${FIXTURE_DIR} ..."
INDEX_LOG="$(trusty-search index "${FIXTURE_DIR}" --name "${INDEX_ID}" --lexical-only 2>&1)"
echo "  Index output: ${INDEX_LOG}"
if echo "${INDEX_LOG}" | grep -q "chunks"; then
    pass "trusty-search index created"
else
    fail "trusty-search index failed (no chunks in output)"
fi

# Run a query and assert we get a hit on 'authenticate' using the CLI.
# (The /grep HTTP endpoint requires POST with JSON body; the CLI `query`
# subcommand is simpler and always works regardless of lexical/semantic mode.)
echo "  Running query for 'authenticate' ..."
QUERY_OUT="$(trusty-search query 'authenticate' --index "${INDEX_ID}" 2>&1 || echo '')"
echo "  Query output (first 5 lines):"
echo "${QUERY_OUT}" | head -5

if echo "${QUERY_OUT}" | grep -qi "authenticate\|auth\.rs"; then
    pass "trusty-search query returned results for 'authenticate'"
else
    fail "trusty-search search returned no results for 'authenticate'"
fi

# ---------------------------------------------------------------------------
# Indexing-hygiene assertion (version-gated: requires >= 0.25.0)
# ---------------------------------------------------------------------------
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

# Start daemon in foreground on a fixed port to avoid port-file races.
TRUSTY_MEMORY_HTTP="127.0.0.1:7070"
trusty-memory serve --foreground --http "${TRUSTY_MEMORY_HTTP}" > /tmp/tm.log 2>&1 &
TM_PID=$!

if wait_http "http://${TRUSTY_MEMORY_HTTP}/health" 60; then
    pass "trusty-memory daemon healthy"
else
    echo "  --- daemon log ---"
    cat /tmp/tm.log
    echo "  --- end ---"
    fail "trusty-memory daemon did not start"
    kill "${TM_PID}" 2>/dev/null || true
    TM_PID=""
fi

if [ -n "${TM_PID}" ]; then
    # Create 'personal' palace (always valid regardless of cwd / project root).
    echo "  Creating palace 'personal' ..."
    CREATE_OUT="$(curl -sf -X POST "http://${TRUSTY_MEMORY_HTTP}/api/v1/palaces" \
        -H 'Content-Type: application/json' \
        -d '{"name":"personal"}' 2>/dev/null || echo '{}')"
    echo "  Create response: ${CREATE_OUT}"

    if echo "${CREATE_OUT}" | grep -q '"id"'; then
        pass "trusty-memory palace created"
    else
        fail "trusty-memory palace create failed"
    fi

    # Store a memory in the palace.
    REMEMBER_TEXT="trusty-memory smoke test: remember this sentinel value 42xyzABC"
    echo "  Storing memory ..."
    REMEMBER_OUT="$(curl -sf -X POST "http://${TRUSTY_MEMORY_HTTP}/api/v1/palaces/personal/drawers" \
        -H 'Content-Type: application/json' \
        -d "{\"content\":\"${REMEMBER_TEXT}\"}" 2>/dev/null || echo '{}')"
    echo "  Remember response: ${REMEMBER_OUT}"

    if echo "${REMEMBER_OUT}" | grep -q '"id"'; then
        pass "trusty-memory memory stored"
    else
        fail "trusty-memory memory store failed"
    fi

    # Recall and assert the sentinel text comes back.
    echo "  Recalling memory ..."
    sleep 2  # Allow indexing to complete before recall.
    RECALL_OUT="$(curl -sf "http://${TRUSTY_MEMORY_HTTP}/api/v1/palaces/personal/recall?q=sentinel+value+42xyzABC&top_k=5" \
        2>/dev/null || echo '{}')"
    echo "  Recall response: ${RECALL_OUT}"

    if echo "${RECALL_OUT}" | grep -q '42xyzABC\|sentinel'; then
        pass "trusty-memory recall returned stored text"
    else
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
tm start > /tmp/mpm.log 2>&1 &
MPM_DAEMON_PID=$!
sleep 5

STATUS_OUT="$(tm status 2>&1 || true)"
echo "  Status: ${STATUS_OUT}"
if echo "${STATUS_OUT}" | grep -qi "running\|ok\|active\|daemon\|version\|sessions"; then
    pass "trusty-mpm: daemon start + status ok"
else
    # Status might exit non-zero if daemon is not up — that counts as fail.
    echo "  --- daemon log ---"
    cat /tmp/mpm.log
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
TRUSTY_SKIP_RAM_CHECK=1 trusty-search start > /tmp/ts2.log 2>&1 &
TS2_PID=$!
sleep 3
TS2_PORT="$(trusty-search port 2>/dev/null || echo '7878')"
echo "  trusty-search port: ${TS2_PORT}"

if ! wait_http "http://127.0.0.1:${TS2_PORT}/health" 30; then
    fail "trusty-search dependency for analyze did not start"
    echo "  --- ts log ---"
    cat /tmp/ts2.log
    echo "  --- end ---"
    kill "${TS2_PID}" 2>/dev/null || true
    TS2_PID=""
fi

# Index the fixture for analyze to use.
if [ -n "${TS2_PID}" ]; then
    ANALYZE_INDEX="smoke-analyze"
    trusty-search index "/e2e/sample-code" --name "${ANALYZE_INDEX}" --lexical-only \
        > /tmp/ts2-index.log 2>&1 || true

    # Start trusty-analyze daemon.
    # Note: --search-url is a GLOBAL flag (before the subcommand), not a serve flag.
    # Use the TRUSTY_SEARCH_URL env var to keep the invocation readable.
    echo "  Starting trusty-analyze daemon ..."
    TRUSTY_SEARCH_URL="http://127.0.0.1:${TS2_PORT}" \
        trusty-analyze serve --foreground > /tmp/ta.log 2>&1 &
    TA_PID=$!

    if wait_http "http://127.0.0.1:7879/health" 30; then
        pass "trusty-analyze daemon healthy"
    else
        echo "  --- analyze log ---"
        cat /tmp/ta.log
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

        # Also verify the health endpoint reports search_reachable.
        TA_HEALTH="$(curl -sf "http://127.0.0.1:7879/health" 2>/dev/null || echo '{}')"
        echo "  Health: ${TA_HEALTH}"
        if echo "${TA_HEALTH}" | grep -q '"status"'; then
            pass "trusty-analyze: health endpoint returned structured JSON"
        else
            fail "trusty-analyze: health endpoint response missing 'status' field"
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
