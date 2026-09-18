#!/usr/bin/env bash
#
# ci-website-relevance.sh — does this change set need a website CI suite?
#
# Why: `.github/workflows/website-tests.yml` used `paths:` filters, and the
#   filter list included `crates/*/Cargo.toml` and `docs/**`. A Rust-only fix
#   that bumps a version therefore ran the whole website Vitest suite — PR
#   #8272 went red there on a corpus-parse hook timeout while touching the
#   website not at all. A `paths:` filter is also the wrong shape for a job
#   that might ever become a required context: GitHub never creates the check
#   run for a filtered-out workflow, so a required context stays pending
#   forever (the trap `scripts/detect-docs-only.sh` and
#   `.github/workflows/capabilities-drift.yml` both document). The jobs
#   therefore run unconditionally and consult this script, which decides only
#   whether their COSTLY steps execute.
#
# What: reads a newline-separated list of changed paths on stdin and answers
#   one question per MODE, printing `true` or `false`:
#
#     unit    the website CODE suites (`pnpm run test` — unit + smoke).
#             Relevant for a change under website/** that is not prose content
#             (website/src/content/**), or to this script / the workflow.
#     corpus  the real-changelog gate (`pnpm run test:corpus`). Relevant for a
#             changelog fragment, a crate CHANGELOG.md, the changelog module
#             itself, or the flagship list that names which crates the corpus
#             covers.
#     lint    `pnpm lint` over website/**. Relevant for any website change,
#             prose content included — Prettier checks content markdown too.
#
#   DOCS-ONLY, as defined in docs/reference/ci-gates.md, is every changed path
#   matching docs/**, a root *.md, crates/*/changelog.d/**, crates/*/README.md,
#   crates/*/CHANGELOG.md, or website/src/content/**. No such path makes `unit`
#   relevant: a documentation change never has to pass a code test suite. The
#   content gates it DOES owe (`corpus` here, plus the prose linters in their
#   own workflows) still run.
#
#   NOT docs-only, by design and stated here because it looks like an
#   exception: crates/*/src/** is code even when the file ends in `.md` —
#   bundled agent and skill assets are compiled into binaries with include_str!
#   and tests assert on their text. Neither .github/** nor scripts/** is
#   docs-only either.
#
#   An EMPTY change set answers `true` for every mode: a diff that could not be
#   resolved must cost a run, never a silent skip. Same fail-closed rule as
#   detect-docs-only.sh and ci-crate-relevance.sh.
#
# Usage:
#   git diff --name-only --no-renames "$MERGE_BASE" HEAD |
#     scripts/ci-website-relevance.sh unit
#
# Exit: 0 with a verdict on stdout; 2 on an unknown mode.
#
# Test: scripts/check-ci-helpers-selftest.sh (`ci-website-relevance:` cases).

set -euo pipefail

SELF_PATH="scripts/ci-website-relevance.sh"
WORKFLOW_PATH=".github/workflows/website-tests.yml"

# emit <mode> <verdict> — the verdict on stdout, and as `relevant=<verdict>`
# in $GITHUB_OUTPUT when a workflow step is what called this.
emit() {
  echo "$2"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "relevant=$2" >>"${GITHUB_OUTPUT}"
  fi
}

# has_prefix <path> <prefix> — true when <path> sits at or below <prefix>,
# matched on the literal string including its trailing slash, so a sibling
# directory ("website-old/") can never match "website/".
has_prefix() {
  case "$1" in "$2"*) return 0 ;; esac
  return 1
}

# is_relevant <mode> <path>
is_relevant() {
  local mode="$1" p="$2"

  # Both the classifier and the workflow it drives are inputs to every mode:
  # a change to either has to be exercised by the jobs it governs.
  if [ "$p" = "$SELF_PATH" ] || [ "$p" = "$WORKFLOW_PATH" ]; then
    return 0
  fi

  case "$mode" in
    unit)
      has_prefix "$p" "website/src/content/" && return 1
      has_prefix "$p" "website/" && return 0
      ;;
    lint)
      has_prefix "$p" "website/" && return 0
      ;;
    corpus)
      has_prefix "$p" "website/src/lib/changelog/" && return 0
      # RELEASED_FLAGSHIPS names the crates the corpus covers; a crate joining
      # or leaving that list changes what the gate reads.
      [ "$p" = "website/src/lib/site.ts" ] && return 0
      [ "$p" = "website/src/lib/tools.ts" ] && return 0
      # crates/<crate>/CHANGELOG.md and crates/<crate>/changelog.d/<file>.
      # Segment-split, not globbed: in a bash `case`, `*` matches `/` too.
      local -a seg
      IFS='/' read -r -a seg <<<"$p"
      if [ "${#seg[@]}" -eq 3 ] && [ "${seg[0]}" = "crates" ] &&
        [ "${seg[2]}" = "CHANGELOG.md" ]; then
        return 0
      fi
      if [ "${#seg[@]}" -ge 4 ] && [ "${seg[0]}" = "crates" ] &&
        [ "${seg[2]}" = "changelog.d" ]; then
        return 0
      fi
      ;;
  esac

  return 1
}

# Resolve this event's changed paths, or fail closed. Mirrors
# capabilities-drift.yml's classifier: every error arm answers "relevant".
resolve_changed_paths() {
  local base
  if [ "${EVENT_NAME:-}" = "pull_request" ]; then
    base="origin/${BASE_REF:-}"
  else
    base="${PUSH_BEFORE:-}"
  fi

  if [ -z "$base" ] || [ "$base" = "origin/" ] ||
    [ "$base" = "0000000000000000000000000000000000000000" ]; then
    echo "ci-website-relevance: no usable base SHA — answering true (fail closed)" >&2
    return 1
  fi

  local merge_base
  if ! merge_base="$(git merge-base "$base" HEAD 2>/dev/null)"; then
    echo "ci-website-relevance: cannot resolve merge-base against '${base}' — answering true (fail closed)" >&2
    return 1
  fi

  if ! git diff --name-only --no-renames "$merge_base" HEAD; then
    echo "ci-website-relevance: git diff against ${merge_base} failed — answering true (fail closed)" >&2
    return 1
  fi
}

main() {
  local mode="${1:-}"
  case "$mode" in
    unit | corpus | lint) ;;
    *)
      echo "ci-website-relevance: usage: $0 <unit|corpus|lint> [< changed-paths]" >&2
      return 2
      ;;
  esac

  # With EVENT_NAME set (a CI step) the script resolves its own diff; without
  # it, the paths arrive on stdin — the shape the self-test drives.
  local input
  if [ -n "${EVENT_NAME:-}" ]; then
    if ! input="$(resolve_changed_paths)"; then
      emit "$mode" true
      return 0
    fi
  else
    input="$(cat)"
  fi

  local relevant=false count=0 path
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    count=$((count + 1))
    if is_relevant "$mode" "$path"; then
      echo "  ${mode}: relevant  ${path}" >&2
      relevant=true
    fi
  done <<<"$input"

  if [ "$count" -eq 0 ]; then
    echo "ci-website-relevance: empty change set — answering true (fail closed)" >&2
    relevant=true
  fi

  echo "ci-website-relevance: ${mode} -> ${relevant} (${count} changed path(s))" >&2
  emit "$mode" "$relevant"
}

main "$@"
