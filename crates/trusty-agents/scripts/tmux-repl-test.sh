#!/usr/bin/env bash
# tmux-repl-test.sh
#
# Why: Pipe-based REPL tests (echo | cargo run) miss real terminal rendering
#   bugs — banner layout, cursor issues, async timing problems. A tmux-driven
#   e2e test exercises the binary in a real PTY so regressions surface here
#   instead of in front of users.
# What: DEPLOY test — verifies the installed `open-mpm` binary by default.
#   Launches `open-mpm ctrl` inside a tmux session, waits for the prompt,
#   sends a chat message, and asserts the captured output matches the expected
#   banner + response layout.
# Flags:
#   (default)  Test the installed binary (`which open-mpm`). Fails fast with
#              a helpful message if not installed.
#   --dev      Build the debug binary first (`cargo build`) and test it
#              instead. Use during development before installing.
# Test: Run `./scripts/tmux-repl-test.sh`. Exit 0 = pass, exit 1 = fail with
#   the captured terminal content printed for debugging.

set -u
set -o pipefail

# ---------- config ----------
SESSION="ompm-e2e"
COLS=220
ROWS=50
STARTUP_TIMEOUT_S=45
RESPONSE_TIMEOUT_S=60
POLL_INTERVAL_S=0.5
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# REPL/ctrl mode is provided by the `open-mpm` binary (src/main.rs). The
# `ompm` binary is just an HTTP thin client and does not have ctrl mode.
BIN_NAME="open-mpm"
BIN="${PROJECT_ROOT}/target/debug/${BIN_NAME}"

# ---------- flags ----------
# Default: test the installed binary. Pass --dev to build + test the debug binary.
USE_DEV=false
for arg in "$@"; do
    [[ "$arg" == "--dev" ]] && USE_DEV=true
done

# ---------- output helpers ----------
RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; BOLD=$'\033[1m'; RESET=$'\033[0m'
info()  { echo "${BOLD}[tmux-repl-test]${RESET} $*"; }
fail()  { echo "${RED}${BOLD}[FAIL]${RESET} $*" >&2; }
pass()  { echo "${GREEN}${BOLD}[PASS]${RESET} $*"; }
warn()  { echo "${YELLOW}[warn]${RESET} $*"; }

# ---------- cleanup trap ----------
cleanup() {
    local code=$?
    if tmux has-session -t "$SESSION" 2>/dev/null; then
        info "Cleaning up tmux session '$SESSION'..."
        tmux kill-session -t "$SESSION" 2>/dev/null || true
    fi
    exit $code
}
trap cleanup EXIT INT TERM

# ---------- prerequisites ----------
if ! command -v tmux >/dev/null 2>&1; then
    fail "tmux is not installed. Install via: brew install tmux"
    exit 1
fi
info "tmux: $(tmux -V)"

# Kill any stale session from a previous run
if tmux has-session -t "$SESSION" 2>/dev/null; then
    warn "Stale session '$SESSION' present — killing it."
    tmux kill-session -t "$SESSION" 2>/dev/null || true
fi

# ---------- resolve binary ----------
if $USE_DEV; then
    info "Building ${BIN_NAME} (debug) for --dev mode..."
    cd "$PROJECT_ROOT"
    if ! cargo build --bin "$BIN_NAME" 2>&1 | tail -20; then
        fail "cargo build --bin ${BIN_NAME} failed."
        exit 1
    fi
    if [[ ! -x "$BIN" ]]; then
        fail "Binary not found at $BIN after build."
        exit 1
    fi
    info "Testing debug binary: $BIN"
else
    BIN=$(which "$BIN_NAME" 2>/dev/null) || {
        fail "${BIN_NAME} not installed. Run: cargo install --path . — then re-run this test."
        exit 1
    }
    info "Testing installed binary: $BIN"
fi

# ---------- launch session ----------
info "Creating tmux session '$SESSION' (${COLS}x${ROWS})..."
tmux new-session -d -s "$SESSION" -x "$COLS" -y "$ROWS"

info "Sending run command: OPEN_MPM_PROJECT_DIR=$PROJECT_ROOT RUST_LOG=warn $BIN --ctrl"
tmux send-keys -t "$SESSION" "OPEN_MPM_PROJECT_DIR=$PROJECT_ROOT RUST_LOG=warn $BIN --ctrl" Enter

# ---------- poll for startup ----------
info "Polling for 'ctrl>' prompt (max ${STARTUP_TIMEOUT_S}s)..."
startup_capture=""
deadline=$(( $(date +%s) + STARTUP_TIMEOUT_S ))
while (( $(date +%s) < deadline )); do
    startup_capture="$(tmux capture-pane -t "$SESSION" -p -S -200 2>/dev/null || true)"
    if echo "$startup_capture" | grep -q 'ctrl>'; then
        break
    fi
    sleep "$POLL_INTERVAL_S"
done

# Take a final capture to be safe
startup_capture="$(tmux capture-pane -t "$SESSION" -p -S -200 2>/dev/null || true)"

echo ""
echo "${BOLD}---------- STARTUP CAPTURE ----------${RESET}"
echo "$startup_capture"
echo "${BOLD}-------------------------------------${RESET}"
echo ""

# ---------- assertions: startup ----------
fail_count=0
assert_present() {
    local needle="$1" label="$2"
    if echo "$startup_capture" | grep -qF "$needle"; then
        pass "Startup contains: $label"
    else
        fail "Startup MISSING: $label  (expected substring: '$needle')"
        fail_count=$((fail_count + 1))
    fi
}
assert_absent() {
    local needle="$1" label="$2"
    if echo "$startup_capture" | grep -qE "$needle"; then
        fail "Startup contains forbidden: $label  (matched: '$needle')"
        fail_count=$((fail_count + 1))
    else
        pass "Startup does not contain: $label"
    fi
}

assert_present "╭─── open-mpm ctrl" "banner top (╭─── open-mpm ctrl)"
assert_present "╰─" "banner bottom (╰─)"
assert_present "All systems go" "'All systems go' status line"
assert_present "ctrl>" "'ctrl>' prompt"

# Order check: ctrl> must appear AFTER the ╰─ closing line.
banner_close_line=$(echo "$startup_capture" | grep -n "╰─" | head -1 | cut -d: -f1 || true)
prompt_line=$(echo "$startup_capture" | grep -n "ctrl>" | head -1 | cut -d: -f1 || true)
if [[ -n "$banner_close_line" && -n "$prompt_line" ]]; then
    if (( prompt_line > banner_close_line )); then
        pass "'ctrl>' appears AFTER banner close (line $prompt_line > $banner_close_line)"
    else
        fail "'ctrl>' appears BEFORE/AT banner close (prompt line $prompt_line, close line $banner_close_line) — status messages leaked above banner."
        fail_count=$((fail_count + 1))
    fi
else
    fail "Could not locate banner close or prompt line for ordering check."
    fail_count=$((fail_count + 1))
fi

# No error: lines in startup
assert_absent '^error:' "'error:' line"

if (( fail_count > 0 )); then
    fail "Startup assertions failed ($fail_count). Aborting before chat phase."
    exit 1
fi

# ---------- send chat message ----------
info "Sending chat message: 'hello'"
tmux send-keys -t "$SESSION" "hello" Enter

# Wait until response indicator appears (alt-screen's fixed buffer means
# line count doesn't grow; we look for ratatui's `⏺` response glyph + the
# echoed `❯ hello` line landing in the chat scrollback). Pre-#268 the test
# polled `wc -l` because the legacy crossterm renderer scrolled scrollback;
# the ratatui port replaces that with content-based assertions that survive
# the fixed-height alt-screen buffer.
info "Waiting for response (max ${RESPONSE_TIMEOUT_S}s)..."
response_capture="$startup_capture"
deadline=$(( $(date +%s) + RESPONSE_TIMEOUT_S ))
while (( $(date +%s) < deadline )); do
    response_capture="$(tmux capture-pane -t "$SESSION" -p -S -500 2>/dev/null || true)"
    if echo "$response_capture" | grep -q '⏺' \
       && echo "$response_capture" | grep -q '❯ hello'; then
        break
    fi
    sleep "$POLL_INTERVAL_S"
done

# Final capture
response_capture="$(tmux capture-pane -t "$SESSION" -p -S -500 2>/dev/null || true)"

echo ""
echo "${BOLD}---------- POST-CHAT CAPTURE ----------${RESET}"
echo "$response_capture"
echo "${BOLD}---------------------------------------${RESET}"
echo ""

# ---------- assertions: response ----------
if echo "$response_capture" | grep -q '❯ hello'; then
    pass "User prompt echoed as '❯ hello' in chat area"
else
    fail "User prompt '❯ hello' not visible in chat area"
    fail_count=$((fail_count + 1))
fi
if echo "$response_capture" | grep -q '⏺'; then
    pass "Response indicator '⏺' present"
else
    fail "Response indicator '⏺' missing — LLM response may not have arrived"
    fail_count=$((fail_count + 1))
fi

if echo "$response_capture" | grep -qE '^error: controller error'; then
    fail "Response contains 'error: controller error' line"
    fail_count=$((fail_count + 1))
else
    pass "Response does not contain 'error: controller error'"
fi

# ---------- summary ----------
echo ""
if (( fail_count == 0 )); then
    pass "All assertions passed."
    exit 0
else
    fail "$fail_count assertion(s) failed."
    exit 1
fi
