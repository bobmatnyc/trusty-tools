#!/usr/bin/env bash
#
# test_trusty_common_lanes.sh — run every trusty-common coverage lane (#4474).
#
# Why: `trusty-common` declares `default = []` and gates 25+ modules behind 47
#   opt-in features, so no single `cargo test -p trusty-common` run covers the
#   crate. #4901 made the bare, zero-feature run fail rather than pass over the
#   modules it never compiled; that leaves the runs that DO name a feature, each
#   of which still covers only what it named. `--features inference-client`
#   never compiled `inference::bedrock`; `credentials`, `session-naming` and
#   `memory-core` each shipped a PR whose prescribed gate never ran their tests.
#   This script is the one command that means "the whole crate".
#
# What: reads the lanes from `[package.metadata.trusty-test-coverage]` in
#   crates/trusty-common/Cargo.toml via `cargo metadata`, and runs
#   `cargo test -p trusty-common --features <lane> --no-fail-fast` for each.
#   The lane list is NOT duplicated here — `tests/feature_coverage.rs` proves
#   those same rows cover every declared feature, so the statement this script
#   executes and the statement CI checks cannot drift apart.
#
#   Exits non-zero if any lane fails, after running them all. Lane output goes
#   to a per-lane file under a temp directory; a passing lane prints its counts
#   only, a failing lane prints its full output.
#
# Usage:
#   bash scripts/test_trusty_common_lanes.sh              # every lane
#   bash scripts/test_trusty_common_lanes.sh core symgraph # named lanes only
#   CARGO_TEST_ARGS="--release" bash scripts/test_trusty_common_lanes.sh
#
# This is a hardening/pre-publish gate, not an inner-loop command — the `core`
# lane alone builds a bundled ONNX Runtime. For an ordinary change, run the
# lane that covers what you touched.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${REPO_ROOT}/crates/trusty-common/Cargo.toml"

if ! command -v jq >/dev/null 2>&1; then
    echo "[FAIL] jq is required to read the coverage lanes from cargo metadata." >&2
    exit 2
fi

# `--no-deps` keeps this to the workspace's own packages; the lanes live in
# trusty-common's package metadata, which cargo passes through verbatim.
metadata="$(cargo metadata --format-version 1 --no-deps --manifest-path "${MANIFEST}" 2>/dev/null)"
if [[ -z "${metadata}" ]]; then
    echo "[FAIL] cargo metadata produced no output for ${MANIFEST}." >&2
    exit 2
fi

# One "name<TAB>feat,feat,feat" row per lane.
lanes="$(
    printf '%s' "${metadata}" | jq -r '
        .packages[]
        | select(.name == "trusty-common")
        | .metadata["trusty-test-coverage"].lanes[]
        | "\(.name)\t\(.features | join(","))"
    '
)"

if [[ -z "${lanes}" ]]; then
    echo "[FAIL] no lanes in [package.metadata.trusty-test-coverage] — see #4474." >&2
    exit 2
fi

requested=("$@")
outdir="$(mktemp -d)"
trap 'rm -rf "${outdir}"' EXIT

failed=()
ran=0

while IFS=$'\t' read -r name features; do
    [[ -z "${name}" ]] && continue

    if [[ ${#requested[@]} -gt 0 ]]; then
        wanted=0
        for r in "${requested[@]}"; do
            [[ "${r}" == "${name}" ]] && wanted=1
        done
        [[ ${wanted} -eq 0 ]] && continue
    fi

    log="${outdir}/${name}.txt"
    echo "==> lane ${name}: --features ${features}"
    # shellcheck disable=SC2086  # CARGO_TEST_ARGS is a deliberate word-split.
    cargo test -p trusty-common --features "${features}" --no-fail-fast \
        ${CARGO_TEST_ARGS:-} >"${log}" 2>&1
    status=$?
    ran=$((ran + 1))

    if [[ ${status} -eq 0 ]]; then
        grep -E '^test result:' "${log}" | sed 's/^/    /'
        echo "    [PASS] lane ${name}"
    else
        echo "    [FAIL] lane ${name} (exit ${status})"
        cat "${log}"
        failed+=("${name}")
    fi
done <<<"${lanes}"

if [[ ${ran} -eq 0 ]]; then
    echo "[FAIL] no lane matched: ${requested[*]}" >&2
    exit 2
fi

if [[ ${#failed[@]} -gt 0 ]]; then
    echo "[FAIL] ${#failed[@]} of ${ran} lanes failed: ${failed[*]}" >&2
    exit 1
fi

echo "[PASS] ${ran} lane(s) passed."
