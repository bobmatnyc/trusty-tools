#!/usr/bin/env bash
# scripts/check_buildrs_sync.sh
#
# Why: Cargo cannot share a build script as a library, so crates that need the
# same build-time logic duplicate it by necessity. This script is the anti-drift
# gate that fails CI whenever the copies diverge. Two independent families exist:
#
#   "daemon"  — trusty-memory, trusty-analyze, trusty-console, trusty-search
#               (issue #987). Embeds an OPTIONAL web UI; degrades to a
#               placeholder when the JS toolchain is missing.
#   "tauri-ui" — trusty-code-gui, trusty-mpm-gui, trusty-agents-ui (issue
#               #4699). Embeds the whole desktop window; ABORTS the crate build
#               on any UI-build failure, because a placeholder there would ship
#               a blank app. trusty-agents-ui is edition 2021, so this block
#               must stay free of let-chains.
#
# The two families are deliberately not merged: their failure semantics differ.
#
# What: Extracts the text between each family's BEGIN/END markers from every
# member's build.rs and asserts the members of a family are byte-for-byte
# identical. Exits 0 on success, 1 on any mismatch with a diff.
#
# Test: Run `bash scripts/check_buildrs_sync.sh` from the workspace root.
# Expected output: one "in sync" line per family.

set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

DAEMON_FILES=(
    "crates/trusty-memory/build.rs"
    "crates/trusty-analyze/build.rs"
    "crates/trusty-console/build.rs"
    "crates/trusty-search/build.rs"
)

TAURI_UI_FILES=(
    "crates/trusty-code-gui/build.rs"
    "crates/trusty-mpm-gui/build.rs"
    "crates/trusty-agents/ui/src-tauri/build.rs"
)

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

FAILED=0

# Assert every build.rs in one family carries a byte-identical canonical block.
#
# $1 = family name (used in messages and temp filenames)
# $2 = marker infix, e.g. "" for `── CANONICAL BLOCK BEGIN` or "TAURI UI " for
#      `── TAURI UI CANONICAL BLOCK BEGIN`
# $3+ = repo-relative build.rs paths
check_family() {
    local family="$1" marker="$2"
    shift 2
    local files=("$@")
    local reference="" reference_file="" rel abs block_file

    for rel in "${files[@]}"; do
        abs="$WORKSPACE_ROOT/$rel"
        if [[ ! -f "$abs" ]]; then
            echo "ERROR: expected file not found: $rel" >&2
            FAILED=1
            continue
        fi
        block_file="$TMP_DIR/${family}_$(echo "$rel" | tr '/' '_').block"
        sed -n "/── ${marker}CANONICAL BLOCK BEGIN/,/── ${marker}CANONICAL BLOCK END/p" \
            "$abs" > "$block_file"
        if [[ ! -s "$block_file" ]]; then
            echo "ERROR: no ${marker}CANONICAL BLOCK found in $rel" >&2
            FAILED=1
            continue
        fi
        if [[ -z "$reference" ]]; then
            reference="$block_file"
            reference_file="$rel"
        elif ! diff -q "$reference" "$block_file" > /dev/null 2>&1; then
            echo "FAIL: ${family} canonical block in $rel differs from $reference_file:" >&2
            diff "$reference" "$block_file" >&2
            echo "To fix: make every ${family} build.rs share $reference_file's block." >&2
            FAILED=1
        fi
    done

    if [[ -n "$reference" ]]; then
        echo "${family}: build.rs canonical blocks are in sync across ${#files[@]} crates."
    fi
}

check_family "daemon" "" "${DAEMON_FILES[@]}"
check_family "tauri-ui" "TAURI UI " "${TAURI_UI_FILES[@]}"

exit "$FAILED"
