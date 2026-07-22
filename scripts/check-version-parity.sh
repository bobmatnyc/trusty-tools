#!/usr/bin/env bash
#
# check-version-parity.sh — version-parity drift gate (issue #3366).
#
# Why: `trusty-common` and `trusty-agents-common` each carried several
#   commits of source changes that landed on `main` WITHOUT a matching
#   version bump, while the OLD version number was already live on
#   crates.io. `cargo build`/`cargo test` never caught this because the
#   workspace's `[patch.crates-io]` table redirects sibling deps to local
#   paths for in-repo builds — the gap only surfaced when `cargo publish`
#   resolved a crate standalone against the real registry, turning a
#   merge-time defect into a release-time blocker (`cargo publish -p
#   trusty-mpm --dry-run` failing with "symbol not found" errors). See
#   issue #3366 for the full incident.
#
# What: builds and runs the `publish-guard` binary (crates/trusty-publish-guard)
#   against this checkout. For every publishable workspace crate (anything
#   NOT `publish = false`), it compares the crate's local `src/` tree against
#   the crates.io tarball for that crate's OWN current `Cargo.toml` version:
#     - version not live yet  -> PASS (nothing to compare; ordinary pre-release state)
#     - source matches        -> PASS
#     - source differs        -> FAIL (the #3366 defect: unpublished drift
#                                 under an already-published version number)
#     - registry unreachable / unexpected response -> FAIL (fails CLOSED;
#                                 an unverifiable crate is never a silent pass)
#
# Usage:
#   scripts/check-version-parity.sh
#
# Exit: 0 when every publishable crate is at parity (or not yet published);
#   non-zero the moment any crate shows drift or could not be verified.
#
# Test: crates/trusty-publish-guard/src/lib.rs unit-tests the drift-detection
#   engine (extraction, diffing, parity decision) against an in-memory fake
#   registry — no network required for `cargo test -p trusty-publish-guard`.
#   This script itself just wires that engine to the real crates.io API and
#   is exercised by running it against this repo (see the #3366 PR
#   description for the raw output confirming it reproduces the real,
#   already-live drift on several crates).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

exec cargo run --quiet --locked -p trusty-publish-guard --bin publish-guard -- --root "${REPO_ROOT}"
