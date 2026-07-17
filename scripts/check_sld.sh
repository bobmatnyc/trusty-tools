#!/usr/bin/env bash
#
# check_sld.sh — Spec-Linked Documentation (DOC-38) lint gate (issue #2854).
#
# Why: DOC-38 defines the SLD reference grammar and spec-document conventions;
#   without a gate, declared source↔spec links rot silently and specs drift from
#   the standard. This wrapper runs the `sld-lint` binary over the whole
#   checkout, mirroring `check_line_cap.sh`'s "bash script, exit 0 = clean"
#   ergonomics so CI (.github/workflows/sld-lint.yml) and the pre-commit hook
#   invoke it identically.
#
# What: builds+runs `trusty-sld-lint` against the repo root. In DEFAULT mode the
#   linter resolves every declared reference (inline blocks across crates/**,
#   `spec_refs:` frontmatter in docs/specs/**/*.md) and applies the full DOC-38
#   §4 spec-document checks only to opted-in (frontmatter-bearing) specs — so it
#   passes on the current tree while the retrofit (DOC-38 §10 F5/F6) lands.
#   Documented pre-existing exceptions are grandfathered in
#   .sld-lint-allowlist.tsv (a ratchet that can only shrink).
#
# Usage:
#   bash scripts/check_sld.sh            # default (grandfathering) mode
#   bash scripts/check_sld.sh --strict   # full checks on EVERY spec (post-retrofit)
#   bash scripts/check_sld.sh --help     # full check inventory + strictness model
#
# Exit: 0 when clean; non-zero when any error-severity finding survives the
#   allowlist. Extra args are passed through to the binary.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Reuse the lightweight `sld` grammar from trusty-common; only builds the linter
# crate + its deps, not the whole workspace.
exec cargo run --quiet --locked -p trusty-sld-lint --bin sld-lint -- \
  --root "$REPO_ROOT" "$@"
