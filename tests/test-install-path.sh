#!/usr/bin/env bash
# tests/test-install-path.sh — unit tests for install.sh's PATH handling
# (#3874: write to .zshenv/.zprofile/.zshrc idempotently, not just .zshrc).
#
# Why: install.sh previously wrote the PATH export only to `.zshrc`, which
# neither login (`ssh host cmd`, non-interactive) nor login-but-not-
# interactive zsh invocations source — only `.zshenv` is sourced
# unconditionally. This left `~/.local/bin` off PATH for those shells,
# producing false "not found" preflight warnings and bogus verify results.
#
# What: sources install.sh with TRUSTY_INSTALL_SOURCE_ONLY=1 (skips the
# network `main` flow) inside a synthetic $HOME, then drives
# `maybe_update_path` directly under each detected-shell branch, asserting:
# - zsh: all three of .zshenv/.zprofile/.zshrc get the export line
# - bash: both .bashrc/.bash_profile get the export line
# - repeat runs do not duplicate the export line in any file (idempotency)
#
# Test: This file *is* the test. Run with: tests/test-install-path.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
INSTALL_SH="$REPO_ROOT/install.sh"

PASS=0
FAIL=0

assert_eq() {
    local actual="$1" expected="$2" desc="$3"
    if [[ "$actual" == "$expected" ]]; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc"
        echo "    expected: $expected"
        echo "    actual:   $actual"
        FAIL=$((FAIL + 1))
    fi
}

# count_matches <file> <needle> — occurrences of an exact-substring line.
count_matches() {
    local file="$1" needle="$2"
    [[ -f "$file" ]] || { echo 0; return; }
    grep -cF "$needle" "$file" 2>/dev/null || echo 0
}

# Runs maybe_update_path("$FAKE_HOME/.local/bin") inside a fresh subshell
# with $HOME/$SHELL overridden, sourcing install.sh in source-only mode.
run_maybe_update_path() {
    local home="$1" shell_path="$2"
    HOME="$home" SHELL="$shell_path" ASSUME_YES=1 TRUSTY_INSTALL_SOURCE_ONLY=1 \
        bash -c '. "$1"; maybe_update_path "$HOME/.local/bin"' _ "$INSTALL_SH"
}

test_zsh_writes_all_three_files() {
    echo "--- test_zsh_writes_all_three_files ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    local needle="PATH='${home}/.local/bin'"
    assert_eq "$(count_matches "$home/.zshenv" "$needle")" "1" "zshenv gets the export line"
    assert_eq "$(count_matches "$home/.zprofile" "$needle")" "1" "zprofile gets the export line"
    assert_eq "$(count_matches "$home/.zshrc" "$needle")" "1" "zshrc gets the export line"

    rm -rf "$home"
}

test_bash_writes_both_files() {
    echo "--- test_bash_writes_both_files ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    run_maybe_update_path "$home" "/bin/bash" >/dev/null

    local needle="PATH='${home}/.local/bin'"
    assert_eq "$(count_matches "$home/.bashrc" "$needle")" "1" "bashrc gets the export line"
    assert_eq "$(count_matches "$home/.bash_profile" "$needle")" "1" "bash_profile gets the export line"

    rm -rf "$home"
}

test_repeat_install_is_idempotent() {
    echo "--- test_repeat_install_is_idempotent ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    run_maybe_update_path "$home" "/bin/zsh" >/dev/null
    run_maybe_update_path "$home" "/bin/zsh" >/dev/null
    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    local needle="PATH='${home}/.local/bin'"
    assert_eq "$(count_matches "$home/.zshenv" "$needle")" "1" "zshenv has exactly one export line after 3 runs"
    assert_eq "$(count_matches "$home/.zprofile" "$needle")" "1" "zprofile has exactly one export line after 3 runs"
    assert_eq "$(count_matches "$home/.zshrc" "$needle")" "1" "zshrc has exactly one export line after 3 runs"

    rm -rf "$home"
}

test_already_on_path_is_noop() {
    echo "--- test_already_on_path_is_noop ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    mkdir -p "$home/.local/bin"
    HOME="$home" SHELL="/bin/zsh" ASSUME_YES=1 TRUSTY_INSTALL_SOURCE_ONLY=1 \
        PATH="$home/.local/bin:$PATH" \
        bash -c '. "$1"; maybe_update_path "$HOME/.local/bin"' _ "$INSTALL_SH" >/dev/null

    assert_eq "$([[ -f "$home/.zshrc" ]] && echo yes || echo no)" "no" "no RC file created when dir already on PATH"

    rm -rf "$home"
}

echo "==========================================================="
echo "install.sh PATH-handling test suite (#3874)"
echo "==========================================================="
test_zsh_writes_all_three_files
test_bash_writes_both_files
test_repeat_install_is_idempotent
test_already_on_path_is_noop

echo "==========================================================="
echo "Result: $PASS passed, $FAIL failed"
echo "==========================================================="

[[ "$FAIL" -eq 0 ]] || exit 1
exit 0
