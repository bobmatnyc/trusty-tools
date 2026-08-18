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
#   "tauri-ui" — trusty-code-gui, trusty-mpm-gui, trusty-agents-ui,
#               trusty-audit-ui (issues #4699, #5477). Embeds the whole desktop
#               window; ABORTS the crate build on any UI-build failure, because
#               a placeholder there would ship a blank app. trusty-agents-ui and
#               trusty-audit-ui are edition 2021, so this block must stay free
#               of let-chains.
#
# The two families are deliberately not merged: their failure semantics differ.
#
# What: Extracts the text between each family's BEGIN/END markers from every
# member's build.rs and asserts the members of a family are byte-for-byte
# identical. Then asserts every crate in scripts/ui-bundle-manifest.tsv re-stamps
# its bundle after building it (#5936 — see check_restamp below). Exits 0 on
# success, 1 on any mismatch with a diff.
#
# Test: Run `bash scripts/check_buildrs_sync.sh` from the workspace root.
# Expected output: one "in sync" line per family, plus one "ui-restamp" line.

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
    "crates/trusty-audit/ui/src-tauri/build.rs"
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

# Assert every crate that ships a committed UI bundle chains a re-stamp to its
# UI build.
#
# Why: each of those crates sets `build.emptyOutDir` in its vite.config.js, so
# `pnpm run build` clears the bundle directory — deleting the committed
# ui-source-hash.txt that scripts/check-ui-bundle-freshness.sh reads. Only
# scripts/stamp-ui-bundle.sh writes it back, and nothing chained the two, so an
# ordinary `cargo build` staged the deletion (#5936). A fifth bundle-shipping
# crate added without the call reintroduces the trap in one sibling only, which
# is exactly the asymmetry this check exists to catch.
#
# What: reads scripts/ui-bundle-manifest.tsv — the list of crates that commit a
# bundle — and requires each one's build.rs to call restamp_ui_bundle(). Fails
# closed: inspecting zero rows is a failure, not a pass.
check_restamp() {
    local manifest="$WORKSPACE_ROOT/scripts/ui-bundle-manifest.tsv"
    local crate rest buildrs checked=0

    if [[ ! -f "$manifest" ]]; then
        echo "ERROR: expected file not found: scripts/ui-bundle-manifest.tsv" >&2
        FAILED=1
        return
    fi

    while IFS=$'\t' read -r crate rest; do
        [[ -z "$crate" || "$crate" == \#* ]] && continue
        buildrs="$WORKSPACE_ROOT/crates/$crate/build.rs"
        if [[ ! -f "$buildrs" ]]; then
            echo "FAIL: ${crate} commits a UI bundle but has no build.rs to re-stamp it." >&2
            FAILED=1
            continue
        fi
        # Drop the definition line first. Every crate carries `fn
        # restamp_ui_bundle(` inside the shared canonical block, so a bare grep
        # matches on all four whether or not any of them CALLS it — a gate that
        # cannot fail. What must be present is an indented call site.
        if ! grep -v '^fn restamp_ui_bundle(' "$buildrs" | grep -q 'restamp_ui_bundle('; then
            echo "FAIL: crates/${crate}/build.rs never calls restamp_ui_bundle()." >&2
            echo "  emptyOutDir deletes ${crate}'s committed ui-source-hash.txt on every" >&2
            echo "  build; without the re-stamp that deletion is staged silently (#5936)." >&2
            echo "  To fix: call restamp_ui_bundle() after the UI build succeeds." >&2
            FAILED=1
            continue
        fi
        checked=$((checked + 1))
    done < "$manifest"

    if [[ "$checked" -eq 0 ]]; then
        echo "ERROR: ui-restamp inspected zero manifest rows — refusing to report a pass." >&2
        FAILED=1
        return
    fi
    echo "ui-restamp: ${checked} bundle-shipping crate(s) re-stamp after their UI build."
}

check_family "daemon" "" "${DAEMON_FILES[@]}"
check_family "tauri-ui" "TAURI UI " "${TAURI_UI_FILES[@]}"
check_restamp

exit "$FAILED"
