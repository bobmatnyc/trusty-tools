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
#   Inert ONLY when ADDED or MODIFIED (owner ruling 2026-09-27, ADR-0064):
#     crates/trusty-mpm/src/assets/**/*.md
#     crates/trusty-agents-common/src/assets/**/*.md
#                                 instruction content compiled in via
#                                 include_str!. An edit cannot change whether
#                                 the workspace compiles, lints or formats.
#                                 What it CAN break — the resident budgets, the
#                                 generated tm-capabilities skill, the
#                                 asset-content tests — capabilities-drift.yml
#                                 still runs, because both roots sit in
#                                 trusty-mpm's build closure.
#     content/**                  the post-PHASE_1 home of the same content.
#   A DELETE (or the delete half of a rename, since the diff is taken with
#   --no-renames) or a type change under those roots is CODE: it removes a
#   path an include_str! names. A bare path with no status column is CODE
#   too — the status is what makes the edit inert.
#
#   Deliberately NOT inert, though they look like documentation:
#     crates/*/src/**/*.md   — outside the two roots above (e.g. trusty-code's
#                              forked agents): compiled in via include_dir!/
#                              include_str!, and asset-pin tests read them.
#     non-.md files under the two asset roots — manifests, JSON schemas and
#                              hook scripts are parsed or executed.
#     other scripts/**       — may be invoked by integration tests or builds.
#     other workflows/**     — unknown workflow changes fail closed.
#     crates/*/changelog.d/*/*  — a NESTED fragment is already a defect the
#                              changelog gate rejects; do not also exempt it.
#
# Usage:
#   scripts/detect-docs-only.sh --instruction-assets < paths
#     prints each stdin path whose ADD or MODIFY is inert instruction content
#     (the second list above). The capabilities-drift relevance step and
#     scripts/check_asset_test_filter.py read the asset-content definition
#     from here, so it has one copy (#8378).
#   git diff --name-status --no-renames "$MERGE_BASE" HEAD | scripts/detect-docs-only.sh
#   git diff --name-only --no-renames "$MERGE_BASE" HEAD | scripts/detect-docs-only.sh
#   DOCS_ONLY_BASE=origin/main scripts/detect-docs-only.sh     # resolves its own diff
#
#   A stdin line is `<status><TAB><path>` (git --name-status) or a bare
#   `<path>`. Only the status form can make an instruction asset inert.
#
# Exit: always 0 on a successful classification; non-zero only when a requested
#   base ref cannot be resolved (fail closed — the caller must not guess).
#
# Test: scripts/check-ci-helpers-selftest.sh (`detect-docs-only:` cases) runs
#   this against docs-only, code-only, mixed, embedded-asset, instruction-asset
#   add/modify/delete, and empty change sets and asserts the verdict for each.

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
      scripts/check_trusty_code_specs.sh | \
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

# is_inert_instruction_asset <status> <path> — true when <path> is instruction
# content whose add or modify cannot change a Cargo result (ADR-0064). Any
# other status — D, T, or `?` for a bare path — answers false.
is_inert_instruction_asset() {
  case "$1" in A | M) ;; *) return 1 ;; esac
  case "$2" in
    content/?*) return 0 ;;
    crates/trusty-mpm/src/assets/?*.md) return 0 ;;
    crates/trusty-agents-common/src/assets/?*.md) return 0 ;;
  esac
  return 1
}

main() {
  if [ "${1:-}" = "--instruction-assets" ]; then
    local p
    while IFS= read -r p; do
      if [ -n "$p" ] && is_inert_instruction_asset M "$p"; then
        printf '%s\n' "$p"
      fi
    done
    return 0
  fi

  local input
  if [ -n "${DOCS_ONLY_BASE:-}" ]; then
    local merge_base
    if ! merge_base="$(git merge-base "${DOCS_ONLY_BASE}" HEAD 2>/dev/null)"; then
      echo "detect-docs-only: cannot resolve merge-base against '${DOCS_ONLY_BASE}'" >&2
      return 2
    fi
    # --name-status: an instruction asset is inert only when added or
    # modified, so each path's status is part of the answer.
    if ! input="$(git diff --name-status --no-renames "${merge_base}" HEAD)"; then
      echo "detect-docs-only: git diff against '${merge_base}' failed" >&2
      return 2
    fi
  else
    input="$(cat)"
  fi

  local docs_only=true
  local count=0
  local tab line status path
  tab="$(printf '\t')"
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    count=$((count + 1))
    # `<status><TAB><path>` from --name-status, or a bare path. A status is one
    # capital letter plus an optional similarity score (R100, C75).
    status="?"
    path="$line"
    case "$line" in
      [ACDMRTUXB]"$tab"* | [ACDMRTUXB][0-9]*"$tab"*)
        status="${line%%"$tab"*}"
        status="${status:0:1}"
        path="${line#*"$tab"}"
        ;;
    esac
    if is_inert_path "$path"; then
      echo "  inert: ${status} ${path}" >&2
    elif is_inert_instruction_asset "$status" "$path"; then
      echo "  inert: ${status} ${path} (instruction content, added or modified)" >&2
    else
      echo "  code : ${status} ${path}" >&2
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
