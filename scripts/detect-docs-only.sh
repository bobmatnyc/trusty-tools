#!/usr/bin/env bash
#
# detect-docs-only.sh — classify a change set as docs-only or not (issue #4468).
#
# Why: docs-only PRs ran the full Rust build — Clippy, Format, MSRV, Test, and
#   a 15-20 min release build for the search-daemon smoke test — even though
#   nothing in the diff can affect compilation. A `paths:`/`paths-ignore:`
#   filter on the workflow trigger is NOT the fix: GitHub never creates the
#   check runs for a workflow the filters skipped, so a REQUIRED context stays
#   pending forever and the PR becomes unmergeable rather than fast. The same
#   trap is documented in .github/workflows/changelog-fragment.yml. So the
#   exemption lives here, in a script the always-running job consults, and the
#   jobs keep reporting.
#
# What: reads a newline-separated list of changed paths on stdin (or resolves
#   one itself from git when given a base ref) and answers ONE question: does
#   every changed path match a pattern that provably cannot affect a cargo
#   build, test, clippy, or rustfmt result? Despite the legacy output name,
#   `docs_only=true` therefore means "Cargo-inert", not literally "only files
#   below docs/". Emits `docs_only=true|false` on
#   stdout, and appends the same line to $GITHUB_OUTPUT when that is set.
#
#   The classification is a DENYLIST-of-inert, not an allowlist-of-code: a path
#   is inert only when it matches one of the patterns below, and anything
#   unrecognised is treated as code. An unknown new path therefore costs a full
#   build (correct, cheap to fix) instead of silently skipping verification
#   (incorrect, expensive to discover). An EMPTY change set is likewise not
#   docs-only, so a failure to resolve the diff can never turn into a skip.
#
#   Inert (docs-only) paths:
#     docs/**                      project documentation tree
#     website/**                   SvelteKit marketing/docs site (#5094) — its
#                                   own build/lint runs in its own workflow,
#                                   never Cargo's, so it cannot affect a Rust
#                                   result any more than docs/** can.
#     <root>/*.md                  README/CHANGELOG/CONTRIBUTING/SECURITY/CLAUDE
#     LICENSE, LICENSE-*
#     .github/*.md                 e.g. PULL_REQUEST_TEMPLATE.md
#     .github/ISSUE_TEMPLATE/**
#     crates/*/README.md
#     crates/*/CHANGELOG.md
#     crates/*/changelog.d/*       per-PR changelog fragments (#4476)
#     selected docs-governance      pure shell gates and their configuration;
#                                   these validate prose but cannot affect a
#                                   Cargo result
#
#   Deliberately NOT inert, though they look like documentation:
#     crates/*/src/**/*.md   — bundled agent/skill/instruction assets that are
#                              compiled into binaries via include_dir!/
#                              include_str!. Editing one changes program
#                              output and breaks asset-pin and drift tests.
#     other scripts/**       — may be invoked by integration tests or builds.
#     other workflows/**     — unknown workflow changes fail closed.
#     crates/*/changelog.d/*/*  — a NESTED fragment is already a defect the
#                              changelog gate rejects; do not also exempt it.
#
# Usage:
#   git diff --name-only --no-renames "$MERGE_BASE" HEAD | scripts/detect-docs-only.sh
#   DOCS_ONLY_BASE=origin/main scripts/detect-docs-only.sh     # resolves its own diff
#
# Exit: always 0 on a successful classification; non-zero only when a requested
#   base ref cannot be resolved (fail closed — the caller must not guess).
#
# Test: scripts/check-ci-helpers-selftest.sh (`detect-docs-only:` cases) runs
#   this against docs-only, code-only, mixed, embedded-asset, and empty change
#   sets and asserts the emitted verdict for each.

set -euo pipefail

# is_inert_path <path> — true when the path cannot affect a cargo result.
#
# Matching is done on SPLIT SEGMENTS, not on glob patterns: in a bash `case`,
# `*` happily matches `/`, so a pattern like `crates/*/changelog.d/*.md` would
# also accept `crates/a/src/assets/changelog.d/x.md`. Segment-count checks make
# each rule mean exactly the depth it appears to mean.
is_inert_path() {
  local p="$1"
  local -a seg
  IFS='/' read -r -a seg <<<"$p"
  local n=${#seg[@]}

  # Documentation-governance inputs are executable CI configuration, but they
  # are pure shell/text checks and cannot change a Rust build. Their own
  # focused workflows still run, so treating them as Cargo-inert avoids
  # Clippy/Test/MSRV/UI/CUDA work without weakening their verification.
  case "$p" in
    .doc-number-allowlist.tsv | \
      .sld-lint-allowlist.tsv | \
      .test-pointer-allowlist.tsv | \
      scripts/check_adr.sh | \
      scripts/check_doc_numbers.sh | \
      scripts/check_sld.sh | \
      scripts/check_test_pointers.sh | \
      scripts/check_token_drift.mjs | \
      scripts/check_token_drift.test.mjs | \
      scripts/check_capabilities.sh | \
      .github/workflows/doc-numbers.yml | \
      .github/workflows/sld-lint.yml | \
      .github/workflows/test-pointers.yml | \
      .github/workflows/token-drift.yml | \
      .github/workflows/capabilities-drift.yml | \
      .github/workflows/version-parity.yml)
      return 0
      ;;
  esac

  # docs/** — the project documentation tree, at any depth.
  if [ "$n" -ge 2 ] && [ "${seg[0]}" = "docs" ]; then
    return 0
  fi

  # website/** — the SvelteKit marketing/docs site, at any depth (#5094). It
  # builds and lints under its own workflow, never Cargo's.
  if [ "$n" -ge 2 ] && [ "${seg[0]}" = "website" ]; then
    return 0
  fi

  case "$n" in
    1)
      # Repo-root markdown and licence files.
      case "${seg[0]}" in
        LICENSE | LICENSE-*) return 0 ;;
        *.md) return 0 ;;
      esac
      ;;
    2)
      # .github/<file>.md — e.g. PULL_REQUEST_TEMPLATE.md.
      if [ "${seg[0]}" = ".github" ]; then
        case "${seg[1]}" in *.md) return 0 ;; esac
      fi
      ;;
    3)
      # .github/ISSUE_TEMPLATE/<file>
      if [ "${seg[0]}" = ".github" ] && [ "${seg[1]}" = "ISSUE_TEMPLATE" ]; then
        return 0
      fi
      # crates/<crate>/README.md | crates/<crate>/CHANGELOG.md
      if [ "${seg[0]}" = "crates" ]; then
        case "${seg[2]}" in README.md | CHANGELOG.md) return 0 ;; esac
      fi
      ;;
    4)
      # crates/<crate>/changelog.d/<file>.md
      if [ "${seg[0]}" = "crates" ] && [ "${seg[2]}" = "changelog.d" ]; then
        case "${seg[3]}" in *.md) return 0 ;; esac
      fi
      ;;
  esac

  return 1
}

main() {
  local input
  if [ -n "${DOCS_ONLY_BASE:-}" ]; then
    local merge_base
    if ! merge_base="$(git merge-base "${DOCS_ONLY_BASE}" HEAD 2>/dev/null)"; then
      echo "detect-docs-only: cannot resolve merge-base against '${DOCS_ONLY_BASE}'" >&2
      return 2
    fi
    input="$(git diff --name-only --no-renames "${merge_base}" HEAD)"
  else
    input="$(cat)"
  fi

  local docs_only=true
  local count=0
  local path
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    count=$((count + 1))
    if is_inert_path "$path"; then
      echo "  inert: ${path}" >&2
    else
      echo "  code : ${path}" >&2
      docs_only=false
    fi
  done <<<"$input"

  # An empty change set is never docs-only: it means the diff could not be
  # resolved, and a skip must never be the consequence of a lookup failure.
  if [ "$count" -eq 0 ]; then
    echo "detect-docs-only: empty change set — treating as code (fail closed)" >&2
    docs_only=false
  fi

  echo "detect-docs-only: ${count} changed path(s) -> docs_only=${docs_only}" >&2
  echo "docs_only=${docs_only}"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "docs_only=${docs_only}" >>"${GITHUB_OUTPUT}"
  fi
}

main "$@"
