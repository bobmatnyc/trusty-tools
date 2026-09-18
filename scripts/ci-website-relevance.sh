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
#             (website/src/content/**), to this script / the workflow, to one of
#             the Rust SOURCE files `site.test.ts` pins values out of
#             (UNIT_RUST_INPUTS below), or to root Cargo.toml's `rust-version`
#             line ALONE — see root_cargo_touches_msrv.
#     corpus  the website CONTENT gate (`pnpm run test:corpus`), which is every
#             suite that reads real repository content: the six-crate changelog
#             corpus, the 27-page docs corpus, the flagship pages, and the
#             landing-page claims grounded in crates/**. Relevant for docs/**,
#             website/src/content/**, a changelog fragment, a crate CHANGELOG.md
#             or Cargo.toml, the website library code under test, and the two
#             crate sources a fact card counts (CORPUS_RUST_INPUTS below).
#     lint    `pnpm lint` over website/**. Relevant for any website change,
#             prose content included — Prettier checks content markdown too.
#
#   DOCS-ONLY, as defined in docs/reference/ci-gates.md, is every changed path
#   matching docs/**, a root *.md, crates/*/changelog.d/**, crates/*/README.md,
#   crates/*/CHANGELOG.md, or website/src/content/**. No such path makes `unit`
#   relevant: a documentation change never has to pass a code test suite. The
#   content gates it DOES owe (`corpus` here, plus the prose linters in their
#   own workflows) still run — and since the unit project walks no repository
#   content at all (#8272), `corpus` is the ONLY suite that can see a docs or
#   content change, which is why its set below is the wider one.
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

# Rust SOURCE files the website unit suite reads and pins values out of:
# `site.test.ts`'s `STABLE_SET matches stable_set.rs` re-derives the advertised
# member list from the first, and its platform case re-derives the Tier-1
# triples from the second. Both were named in website-tests.yml's old `paths:`
# filter and are code, never documentation — a change to either must still pay
# for the unit suite. Commit 819f55cc9 took `StableMember::new(` from 7
# occurrences to 9 and broke this suite on 2026-08-16, which is the whole
# reason the two paths were listed.
#
# An EXACT path list, never a `crates/*/src/**` prefix: every other crate
# source file reaches no website test, and widening this to a prefix would put
# the website suite back on most Rust PRs — the cost PR #8272 paid.
UNIT_RUST_INPUTS="
crates/trusty-installer/src/commands/stable_set.rs
crates/trusty-installer/src/download/platform.rs
"

# Rust SOURCE files the CORPUS suites count values out of: `tools.corpus.test.ts`
# re-derives each "MCP tools: N" fact card from the crate source the daemon
# actually serves. Exact paths, for the same reason UNIT_RUST_INPUTS is exact.
CORPUS_RUST_INPUTS="
crates/trusty-memory/src/tools/mod.rs
crates/trusty-search/src/mcp/tools/descriptors.rs
"

# Root Cargo.toml, and the one line in it the website suite reads.
#
# `site.test.ts`'s `MSRV matches the workspace rust-version` pins the advertised
# minimum against `[workspace.package] rust-version`, so the file IS a unit
# input — but only for that line. Restoring the whole file as a trigger would
# run the 7-minute unit suite on every version-bump PR, because a bump edits
# dependency rows in the same file; that cost is what #8272 paid and what this
# classifier exists to remove.
ROOT_CARGO_PATH="Cargo.toml"
MSRV_DIFF_LINE='^[+-][[:space:]]*rust-version[[:space:]]*='

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

# is_listed <path> <list> — exact membership in a newline-separated list.
# Unquoted expansion on purpose: the list word-splits into one candidate per
# entry.
is_listed() {
  local candidate
  # shellcheck disable=SC2086
  for candidate in $2; do
    [ "$1" = "$candidate" ] && return 0
  done
  return 1
}

# root_cargo_touches_msrv — true when this event's diff of root Cargo.toml adds
# or removes a `rust-version =` line, and only then.
#
# FAIL CLOSED: an uncomputable diff answers "relevant", the same rule every
# other error arm in this script follows. ROOT_CARGO_DIFF is a test seam —
# scripts/check-ci-helpers-selftest.sh feeds a diff in rather than minting a
# repository — and is never set in CI.
root_cargo_touches_msrv() {
  local diff base
  if [ -n "${ROOT_CARGO_DIFF:-}" ]; then
    diff="${ROOT_CARGO_DIFF}"
  elif ! base="$(resolve_base)"; then
    echo "ci-website-relevance: no base for root ${ROOT_CARGO_PATH} diff — answering true (fail closed)" >&2
    return 0
  elif ! diff="$(git diff "${base}...HEAD" -- "${ROOT_CARGO_PATH}" 2>/dev/null)"; then
    echo "ci-website-relevance: cannot diff root ${ROOT_CARGO_PATH} — answering true (fail closed)" >&2
    return 0
  fi

  printf '%s\n' "$diff" | grep -qE "${MSRV_DIFF_LINE}"
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
      is_listed "$p" "${UNIT_RUST_INPUTS}" && return 0
      if [ "$p" = "${ROOT_CARGO_PATH}" ]; then
        root_cargo_touches_msrv && return 0
        return 1
      fi
      ;;
    lint)
      has_prefix "$p" "website/" && return 0
      ;;
    corpus)
      # The website library every corpus suite exercises: changelog, docs,
      # flagship, install and the site/tools records they assert against.
      has_prefix "$p" "website/src/lib/" && return 0
      # The prose the flagship corpus renders, and the two files that decide
      # which projects run at all.
      has_prefix "$p" "website/src/content/" && return 0
      [ "$p" = "website/vite.config.ts" ] && return 0
      [ "$p" = "website/package.json" ] && return 0
      # The documentation corpus itself — the 27 published pages, the manifest
      # that publishes them, and every file a published page links to.
      has_prefix "$p" "docs/" && return 0
      # The bootstrap script the landing page tells a reader to curl.
      [ "$p" = "install.sh" ] && return 0
      is_listed "$p" "${CORPUS_RUST_INPUTS}" && return 0
      # crates/<crate>/CHANGELOG.md, crates/<crate>/Cargo.toml (the package name
      # and release state a flagship record claims), and
      # crates/<crate>/changelog.d/<file>. Segment-split, not globbed: in a bash
      # `case`, `*` matches `/` too.
      local -a seg
      IFS='/' read -r -a seg <<<"$p"
      if [ "${#seg[@]}" -eq 3 ] && [ "${seg[0]}" = "crates" ] &&
        { [ "${seg[2]}" = "CHANGELOG.md" ] || [ "${seg[2]}" = "Cargo.toml" ]; }; then
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

# resolve_base — this event's merge-base on stdout, or non-zero when it cannot
# be computed. Shared by the changed-path walk and the root-Cargo.toml probe so
# both fail closed off the same base.
resolve_base() {
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

  if ! git merge-base "$base" HEAD 2>/dev/null; then
    echo "ci-website-relevance: cannot resolve merge-base against '${base}' — answering true (fail closed)" >&2
    return 1
  fi
}

# Resolve this event's changed paths, or fail closed. Mirrors
# capabilities-drift.yml's classifier: every error arm answers "relevant".
resolve_changed_paths() {
  local merge_base
  merge_base="$(resolve_base)" || return 1

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
