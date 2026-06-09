#!/usr/bin/env bash
# scripts/check_buildrs_sync.sh
#
# Why: trusty-memory, trusty-analyze, trusty-console, and trusty-search each
# contain an identical "CANONICAL BLOCK" in their build.rs (issue #987). Because
# Cargo cannot share build scripts as a library, the block is duplicated by
# necessity; this script is the anti-drift gate that fails CI whenever the
# copies diverge.
#
# What: Extracts the text between "CANONICAL BLOCK BEGIN" and
# "CANONICAL BLOCK END" from each build.rs and asserts they are byte-for-byte
# identical. Exits 0 on success, 1 on any mismatch with a diff.
#
# Test: Run `bash scripts/check_buildrs_sync.sh` from the workspace root.
# Expected output: "build.rs canonical blocks are in sync across all 4 crates."

set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

FILES=(
    "crates/trusty-memory/build.rs"
    "crates/trusty-analyze/build.rs"
    "crates/trusty-console/build.rs"
    "crates/trusty-search/build.rs"
)

extract_canonical_block() {
    local file="$1"
    sed -n '/── CANONICAL BLOCK BEGIN/,/── CANONICAL BLOCK END/p' "$file"
}

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

FAILED=0
REFERENCE=""
REFERENCE_FILE=""

for rel in "${FILES[@]}"; do
    abs="$WORKSPACE_ROOT/$rel"
    if [[ ! -f "$abs" ]]; then
        echo "ERROR: expected file not found: $rel" >&2
        FAILED=1
        continue
    fi
    block_file="$TMP_DIR/$(echo "$rel" | tr '/' '_').block"
    extract_canonical_block "$abs" > "$block_file"
    if [[ ! -s "$block_file" ]]; then
        echo "ERROR: no CANONICAL BLOCK found in $rel" >&2
        FAILED=1
        continue
    fi
    if [[ -z "$REFERENCE" ]]; then
        REFERENCE="$block_file"
        REFERENCE_FILE="$rel"
    else
        if ! diff -q "$REFERENCE" "$block_file" > /dev/null 2>&1; then
            echo "FAIL: canonical block in $rel differs from $REFERENCE_FILE:" >&2
            diff "$REFERENCE" "$block_file" >&2
            FAILED=1
        fi
    fi
done

if [[ "$FAILED" -eq 0 ]]; then
    echo "build.rs canonical blocks are in sync across all ${#FILES[@]} crates."
    exit 0
else
    echo "" >&2
    echo "To fix: update all four build.rs files to share the same canonical block." >&2
    echo "The reference implementation is in $REFERENCE_FILE." >&2
    exit 1
fi
