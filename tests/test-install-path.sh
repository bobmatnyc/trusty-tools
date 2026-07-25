#!/usr/bin/env bash
# tests/test-install-path.sh — unit tests for install.sh's PATH handling
# (#3874: write PATH idempotently to the file(s) each shell always sources;
# #3897 code-critic follow-up round 1 [WARN]: no PATH tripling in a normal
# login+interactive session, and idempotency detects pre-existing
# differently-quoted entries; round 2 [BLOCK]: the round-1 idempotency
# broadening was an unanchored substring match that silently matched
# commented-out lines and sibling *PATH variables, causing the REAL export
# line to be skipped while the script still exited reporting success).
#
# Why: install.sh previously wrote the PATH export only to `.zshrc`, which
# neither non-interactive (`ssh host cmd`) nor login-but-not-interactive zsh
# invocations source — only `.zshenv` is sourced unconditionally by every zsh
# invocation type. This left `~/.local/bin` off PATH for those shells,
# producing false "not found" preflight warnings and bogus verify results.
# A first fix wrote all three of .zshenv/.zprofile/.zshrc, but a normal
# login+interactive session (a fresh Terminal.app window) sources all three
# in ONE shell, tripling the install dir in PATH on a single fresh install
# (round 1 MEDIUM finding) — fixed by writing only `.zshenv` (sufficient on
# its own; sourced unconditionally). A round-1 second finding showed the
# idempotency check only recognised this script's own exact quoting style,
# so a hand-written differently-quoted PATH entry got a redundant second
# entry appended — fixed by broadening the match to `PATH=.*<dir>`. THAT
# broadening was itself unanchored (round 2 CRITICAL/BLOCK): it silently
# matched a commented-out line naming the dir, and matched `PATH=` as a
# substring of `MANPATH=`/`FPATH=`/etc. and the dir as a substring of a
# longer sibling directory — in both cases the REAL export was never
# written, and the script still exited 0. Fixed with a comment-stripping
# pre-pass plus a properly anchored (word-boundary on `PATH=`, delimiter
# boundary on both sides of the directory) match.
#
# What: sources install.sh with TRUSTY_INSTALL_SOURCE_ONLY=1 (skips the
# network `main` flow) inside a synthetic $HOME, then drives
# `maybe_update_path` directly under each detected-shell branch, asserting:
# - zsh: only .zshenv gets the export line (not .zprofile/.zshrc)
# - bash: both .bashrc/.bash_profile get the export line
# - repeat runs do not duplicate the export line in any file (idempotency)
# - a real login+interactive zsh sourcing every file install.sh could have
#   touched sees the install dir on PATH EXACTLY ONCE (the round-1 tripling
#   regression)
# - a pre-existing, differently-quoted PATH entry is recognised (no second
#   entry appended)
# - a pre-existing COMMENTED-OUT entry naming the dir does NOT block the
#   real export line from being written (round-2 trigger a)
# - a pre-existing sibling `MANPATH=` entry naming the dir does NOT block
#   the real `PATH=` line from being written (round-2 trigger b, part 1)
# - a pre-existing entry naming a longer directory that merely STARTS WITH
#   the install dir (`.../.local/binaries`) does NOT block the real
#   `.../.local/bin` export line from being written (round-2 trigger b,
#   part 2)
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

test_zsh_writes_only_zshenv() {
    echo "--- test_zsh_writes_only_zshenv ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    local needle="PATH='${home}/.local/bin'"
    assert_eq "$(count_matches "$home/.zshenv" "$needle")" "1" "zshenv gets the export line"
    assert_eq "$([[ -f "$home/.zprofile" ]] && echo yes || echo no)" "no" "zprofile is NOT written (would double-source with .zshenv)"
    assert_eq "$([[ -f "$home/.zshrc" ]] && echo yes || echo no)" "no" "zshrc is NOT written (would double-source with .zshenv)"

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

# #3897 code-critic MEDIUM finding 1 regression test: a normal Terminal.app
# window is a login+interactive zsh shell, which sources .zshenv
# unconditionally, THEN .zprofile (it's a login shell), THEN .zshrc (it's
# interactive). If install.sh ever again writes the same export line to more
# than one of those files, this test catches the resulting PATH tripling
# directly (rather than asserting per-file line counts, which is exactly the
# coverage gap that let the original regression through).
test_login_interactive_zsh_sources_dir_exactly_once() {
    echo "--- test_login_interactive_zsh_sources_dir_exactly_once ---"
    if ! command -v zsh >/dev/null 2>&1; then
        echo "  SKIP: zsh not available on this host"
        return
    fi
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    # A real login+interactive zsh sources EVERY startup file it finds for
    # that invocation type (.zshenv always; .zprofile/.zshrc/.zlogin only if
    # present — none of the latter exist here after the fix, so zsh silently
    # skips them). ZDOTDIR pinned to the synthetic HOME so this is hermetic
    # regardless of the outer environment's zsh config.
    local result_path
    result_path=$(HOME="$home" ZDOTDIR="$home" zsh -l -i -c 'print -r -- $PATH' 2>/dev/null)
    local count
    count=$(printf '%s' "$result_path" | tr ':' '\n' | grep -cF "${home}/.local/bin")
    assert_eq "$count" "1" "install dir appears exactly once in PATH after a real login+interactive zsh sources every written file"

    rm -rf "$home"
}

# #3897 code-critic MEDIUM finding 2 regression test: a pre-existing
# hand-written PATH export in a different quoting style than this script's
# own must be recognised, not duplicated.
test_differently_quoted_existing_entry_is_recognised() {
    echo "--- test_differently_quoted_existing_entry_is_recognised ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    mkdir -p "$home"
    # Simulate a user's own hand-written entry, double-quoted with a $HOME
    # reference expanded to the literal path (as a real shell would leave it
    # once written) — NOT this script's own single-quoted format.
    printf 'export PATH="%s/.local/bin:$PATH"\n' "$home" >"$home/.zshenv"

    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    local occurrences
    occurrences=$(grep -cF "${home}/.local/bin" "$home/.zshenv")
    assert_eq "$occurrences" "1" "pre-existing differently-quoted entry is not duplicated"

    rm -rf "$home"
}

# #3897 code-critic BLOCK finding, trigger (a): a broadened idempotency
# match on a bare "PATH=.*<dir>" substring silently matched a COMMENTED-OUT
# line naming the dir, so `_append_path_export` thought a real export was
# already present and skipped writing one — leaving `.zshenv` with ZERO
# live `export PATH=` lines while the script exited reporting success. The
# real fix must strip full-line comments before matching, so a disabled
# entry never blocks the real write.
test_commented_out_entry_does_not_block_real_write() {
    echo "--- test_commented_out_entry_does_not_block_real_write ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    mkdir -p "$home"
    printf '# export PATH="%s/.local/bin:$PATH"  (disabled)\n' "$home" >"$home/.zshenv"

    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    local real_lines
    real_lines=$(grep -Ec "^[[:space:]]*export PATH=" "$home/.zshenv")
    assert_eq "$real_lines" "1" "a commented-out entry does not block the real export line from being written"

    rm -rf "$home"
}

# #3897 code-critic BLOCK finding, trigger (b) part 1: "PATH=" is a
# substring of MANPATH=/FPATH=/PYTHONPATH=/CLASSPATH=, so an unanchored
# match on a sibling *PATH variable naming the install dir also silently
# blocked the real PATH export from ever being written.
test_sibling_path_var_does_not_block_real_write() {
    echo "--- test_sibling_path_var_does_not_block_real_write ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    mkdir -p "$home"
    printf 'export MANPATH="%s/.local/bin:$MANPATH"\n' "$home" >"$home/.zshenv"

    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    local real_lines
    real_lines=$(grep -Ec "^[[:space:]]*export PATH=" "$home/.zshenv")
    assert_eq "$real_lines" "1" "a sibling MANPATH= entry naming the dir does not block the real PATH= line from being written"

    rm -rf "$home"
}

# #3897 code-critic BLOCK finding, trigger (b) part 2: with no right-hand
# word-boundary check, ".../.local/bin" matched inside ".../.local/binaries"
# — a different, longer directory that merely starts with the install dir's
# path — also silently blocking the real write.
test_dir_as_prefix_of_longer_path_does_not_block_real_write() {
    echo "--- test_dir_as_prefix_of_longer_path_does_not_block_real_write ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    mkdir -p "$home"
    printf 'export PATH="%s/.local/binaries:$PATH"\n' "$home" >"$home/.zshenv"

    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    local real_lines
    real_lines=$(grep -Ec "^[[:space:]]*export PATH=" "$home/.zshenv")
    assert_eq "$real_lines" "2" "an unrelated .../.local/binaries entry does not block the real .../.local/bin export line from being written (2 total export lines: the pre-existing unrelated one + the new real one)"

    local dir_lines
    dir_lines=$(grep -cF "PATH='${home}/.local/bin'" "$home/.zshenv")
    assert_eq "$dir_lines" "1" "exactly one export line names the exact install dir (not the .../.local/binaries lookalike)"

    rm -rf "$home"
}

echo "==========================================================="
echo "install.sh PATH-handling test suite (#3874, #3897 follow-up)"
echo "==========================================================="
test_zsh_writes_only_zshenv
test_bash_writes_both_files
test_repeat_install_is_idempotent
test_already_on_path_is_noop
test_login_interactive_zsh_sources_dir_exactly_once
test_differently_quoted_existing_entry_is_recognised
test_commented_out_entry_does_not_block_real_write
test_sibling_path_var_does_not_block_real_write
test_dir_as_prefix_of_longer_path_does_not_block_real_write

echo "==========================================================="
echo "Result: $PASS passed, $FAIL failed"
echo "==========================================================="

[[ "$FAIL" -eq 0 ]] || exit 1
exit 0
