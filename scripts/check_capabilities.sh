#!/usr/bin/env bash
#
# check_capabilities.sh — tm-capabilities generated-skill drift gate (issue #2913).
#
# Why: the `tm-capabilities` bundled skill (CLI command tree, MCP tool
#   catalog, agent roster, skill catalog, doctor checks) is produced by
#   `tm generate capabilities` from the harness's own in-process data and
#   committed like any other bundled skill (never build.rs, never
#   deploy-time-dynamic — see the issue #2913 design-research brief). Without
#   a gate, the committed output silently rots the moment a CLI command, MCP
#   tool, agent, or skill is added/removed/renamed without re-running the
#   generator. This wrapper mirrors `scripts/check_line_cap.sh`'s and
#   `scripts/check_sld.sh`'s "bash script, exit 0 = clean" ergonomics so CI
#   and the pre-commit hook can invoke it identically.
#
# What: builds+runs `tm generate capabilities --check`, which regenerates
#   every derived file to memory and diffs it against the committed copies
#   under `crates/trusty-mpm/src/assets/skills/tm-capabilities*` — never
#   writes. `references/workflows.md` is hand-authored and is not part of the
#   generated set, so it is never diffed by this check.
#
# Usage:
#   bash scripts/check_capabilities.sh
#
# Exit: 0 when the committed output matches the generator exactly; non-zero
#   (with a per-file drift summary on stderr) when it does not. To fix:
#   `cargo run -p trusty-mpm --bin tm -- generate capabilities` then commit
#   the diff.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

exec cargo run --quiet -p trusty-mpm --bin tm -- generate capabilities --check
