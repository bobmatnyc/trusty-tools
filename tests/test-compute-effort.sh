#!/usr/bin/env bash
# tests/test-compute-effort.sh — unit tests for scripts/compute-effort.sh
#
# Why: Verifies JSON contract, edge cases (empty diff, deleted file, binary
# entries), and threshold behaviour stay stable as the script evolves.
#
# What: Spins up a series of synthetic temp git repos with `git init` +
# scripted commits, runs compute-effort.sh against known ranges, and asserts
# the JSON output matches expectations. Plain bash assertions — no bats
# dependency.
#
# Test: This file *is* the test. Run with: tests/test-compute-effort.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
COMPUTE="$REPO_ROOT/scripts/compute-effort.sh"

# Track pass/fail counts.
PASS=0
FAIL=0

# --- assertion helpers ------------------------------------------------------

# assert_eq <actual> <expected> <description>
assert_eq() {
    local actual="$1"
    local expected="$2"
    local desc="$3"
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

# assert_contains <haystack> <needle> <description>
assert_contains() {
    local haystack="$1"
    local needle="$2"
    local desc="$3"
    if [[ "$haystack" == *"$needle"* ]]; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc"
        echo "    haystack: $haystack"
        echo "    needle:   $needle"
        FAIL=$((FAIL + 1))
    fi
}

# assert_exit_nonzero <command...> <description>
assert_exit_nonzero() {
    local desc="${@: -1}"
    local cmd=("${@:1:$#-1}")
    set +e
    "${cmd[@]}" >/dev/null 2>&1
    local rc=$?
    set -e
    if [[ "$rc" -ne 0 ]]; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc (expected non-zero exit, got $rc)"
        FAIL=$((FAIL + 1))
    fi
}

# Create a temp git repo and cd into it.
# Sets the global REPO_DIR (the caller does `make_repo` without command
# substitution, so the cd persists in the caller's shell). Local hooksPath
# is unset to /dev/null so synthetic commits don't inherit the parent
# repo's pre-commit framework hooks.
make_repo() {
    REPO_DIR=$(mktemp -d -t compute-effort-test-XXXXXX)
    cd "$REPO_DIR"
    git init -q
    git config user.email "test@example.com"
    git config user.name "Test"
    git config commit.gpgsign false
    git config core.hooksPath /dev/null
}

# Wrapper to keep commits hook-free even if make_repo's hooksPath override
# is overridden by some environment quirk. --no-verify is justified here:
# this is test infrastructure, not user-facing commits.
commit_q() {
    git commit -q --no-verify "$@"
}

cleanup_repo() {
    local dir="$1"
    [[ -n "$dir" && -d "$dir" ]] && rm -rf "$dir"
}

# --- test cases ------------------------------------------------------------

test_single_file_commit() {
    echo "TEST: single-file small commit produces XS or S"

    make_repo
    echo "hello" > a.txt
    git add a.txt
    commit_q -m "initial"
    echo "world" >> a.txt
    git add a.txt
    commit_q -m "second"

    local out
    out=$("$COMPUTE" HEAD~1..HEAD)
    assert_contains "$out" '"loc":1' "loc == 1"
    assert_contains "$out" '"files":1' "files == 1"
    # 1 LoC + 1 file should be XS (score ~= 1.0 + 1.5 + 1.0 = 3.5; <= 6)
    assert_contains "$out" '"size":"XS"' "size XS for tiny commit"
    # JSON well-formedness: should parse with jq.
    if command -v jq >/dev/null 2>&1; then
        echo "$out" | jq . >/dev/null
        assert_eq "$?" "0" "jq parses output cleanly"
    fi
    cleanup_repo "$REPO_DIR"
}

test_multi_file_commit() {
    echo "TEST: multi-file medium commit"

    make_repo
    for i in 1 2 3 4; do
        for j in $(seq 1 30); do
            echo "line $j of file $i" >> "f${i}.txt"
        done
    done
    git add .
    commit_q -m "initial"

    # Modify all four files.
    for i in 1 2 3 4; do
        echo "appended" >> "f${i}.txt"
    done
    git add .
    commit_q -m "modify all"

    local out
    out=$("$COMPUTE" HEAD~1..HEAD)
    assert_contains "$out" '"files":4' "files == 4"
    assert_contains "$out" '"loc":4' "loc == 4 (one line per file)"
    cleanup_repo "$REPO_DIR"
}

test_empty_diff_exits_nonzero() {
    echo "TEST: empty-diff range returns exit 1"

    make_repo
    echo "x" > a.txt
    git add a.txt
    commit_q -m "initial"
    # HEAD..HEAD is an empty range.
    assert_exit_nonzero "$COMPUTE" "HEAD..HEAD" "exit non-zero for empty diff"
    cleanup_repo "$REPO_DIR"
}

test_deleted_file() {
    echo "TEST: deleted-file diff entries don't crash"

    make_repo
    echo "x" > a.txt
    echo "y" > b.txt
    git add a.txt b.txt
    commit_q -m "initial"
    git rm -q a.txt
    commit_q -m "delete a"

    local out
    out=$("$COMPUTE" HEAD~1..HEAD)
    # Deletion of 1-line file: 0 added, 1 deleted = 1 LoC.
    assert_contains "$out" '"files":1' "files == 1 for delete"
    assert_contains "$out" '"loc":1' "loc accounts for deleted lines"
    cleanup_repo "$REPO_DIR"
}

test_binary_file() {
    echo "TEST: binary-file diff entries treated as 0 LoC"

    make_repo
    # Create a small binary blob.
    head -c 256 /dev/urandom > blob.bin
    git add blob.bin
    commit_q -m "initial"
    # Modify the binary.
    head -c 256 /dev/urandom > blob.bin
    git add blob.bin
    commit_q -m "modify blob"

    local out
    out=$("$COMPUTE" HEAD~1..HEAD)
    # Binary file contributes 0 LoC but should still increment the file count.
    assert_contains "$out" '"files":1' "binary modification counted as 1 file"
    assert_contains "$out" '"loc":0' "binary modification contributes 0 LoC"
    cleanup_repo "$REPO_DIR"
}

test_tests_factor() {
    echo "TEST: test-file LoC reduces tests_factor"

    make_repo
    # Two commits so HEAD~1..HEAD is non-empty and contains the test files.
    echo "seed" > seed.txt
    git add .
    commit_q -m "initial"

    mkdir -p src tests
    for j in $(seq 1 50); do echo "src $j" >> src/main.rs; done
    for j in $(seq 1 30); do echo "test $j" >> tests/foo_test.rs; done
    git add .
    commit_q -m "add code+tests"

    local out
    out=$("$COMPUTE" HEAD~1..HEAD)
    # tests_factor = 1 - 0.3 * (30/80) = 1 - 0.1125 = 0.8875 → rounded 0.89
    assert_contains "$out" '"test_loc":30' "test_loc detected"
    # The factor should be less than 1.0 since tests are present.
    if [[ "$out" == *'"tests_factor":1.00'* ]]; then
        echo "  FAIL: tests_factor should be < 1.00 when tests present"
        echo "    got: $out"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: tests_factor reduced when tests present"
        PASS=$((PASS + 1))
    fi
    cleanup_repo "$REPO_DIR"
}

test_json_wellformed() {
    echo "TEST: output is valid JSON parseable by jq"
    if ! command -v jq >/dev/null 2>&1; then
        echo "  SKIP: jq not installed"
        return
    fi

    make_repo
    echo "x" > a.txt
    git add a.txt
    commit_q -m "initial"
    echo "y" > a.txt
    git add a.txt
    commit_q -m "modify"

    local out
    out=$("$COMPUTE" HEAD~1..HEAD)
    local size_val
    size_val=$(echo "$out" | jq -r .size)
    assert_contains "XS S M L XL" "$size_val" "jq extracts a valid size"

    local score_val
    score_val=$(echo "$out" | jq -r .score)
    # score should parse as a number > 0
    if awk "BEGIN { exit !($score_val > 0) }"; then
        echo "  PASS: jq extracts a positive numeric score"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: score is not a positive number ($score_val)"
        FAIL=$((FAIL + 1))
    fi
    cleanup_repo "$REPO_DIR"
}

test_invalid_range() {
    echo "TEST: invalid git range exits non-zero"

    make_repo
    echo "x" > a.txt
    git add a.txt
    commit_q -m "initial"
    assert_exit_nonzero "$COMPUTE" "nonexistent..reference" "invalid range fails"
    cleanup_repo "$REPO_DIR"
}

# --- runner ---------------------------------------------------------------

echo "==========================================================="
echo "compute-effort.sh test suite"
echo "==========================================================="
test_single_file_commit
test_multi_file_commit
test_empty_diff_exits_nonzero
test_deleted_file
test_binary_file
test_tests_factor
test_json_wellformed
test_invalid_range

echo "==========================================================="
echo "Result: $PASS passed, $FAIL failed"
echo "==========================================================="

[[ "$FAIL" -eq 0 ]] || exit 1
exit 0
