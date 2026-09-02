#!/usr/bin/env bash
#
# required-checks.sh — print the LIVE required status-check contexts (#6653).
#
# Why: root CLAUDE.md ("What CI actually gates") tells every reader to run the
#   `gh api .../branches/main/protection --jq '.required_status_checks.contexts'`
#   read live, every time, because a hand-copied list cost PR #5836 a merge.
#   The instruction is correct and still gets typed by hand each time, which is
#   how the jq path gets mistyped and how an empty result gets read as "nothing
#   is required". This is the same read with the two failure modes closed.
#
# What: prints one required context per line, in the order GitHub returns them.
#   The base branch is the first positional argument (default `main`); the
#   repository defaults to the current directory's remote and can be overridden
#   with --repo <owner/repo>.
#
#   `tm pr queue-check` performs the identical read through
#   `trusty_common::gh::GhCommand` so an agent never has to shell out; this
#   script is the standalone form for a shell, a Makefile, or CI.
#
# Usage:
#   bash scripts/required-checks.sh                 # base: main, repo: cwd remote
#   bash scripts/required-checks.sh develop
#   bash scripts/required-checks.sh main --repo bobmatnyc/trusty-tools
#
# Exit:
#   0  one or more contexts printed
#   1  `gh` failed, or the list is EMPTY. An empty list is a failure, never a
#      pass: "no required contexts" and "the read did not work" are
#      indistinguishable from the output alone, and treating either as a pass
#      removes the last gate silently.

set -euo pipefail

BASE="main"
REPO=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      REPO="${2:-}"
      if [[ -z "$REPO" ]]; then
        echo "required-checks.sh: --repo needs an owner/repo argument" >&2
        exit 1
      fi
      shift 2
      ;;
    -h|--help)
      sed -n '2,32p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    -*)
      echo "required-checks.sh: unknown option '$1'" >&2
      exit 1
      ;;
    *)
      BASE="$1"
      shift
      ;;
  esac
done

if ! command -v gh >/dev/null 2>&1; then
  echo "required-checks.sh: gh is not installed or not on PATH." >&2
  exit 1
fi

if [[ -z "$REPO" ]]; then
  if ! REPO="$(gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null)"; then
    echo "required-checks.sh: cannot resolve owner/repo from this directory;" >&2
    echo "                    pass --repo <owner/repo>." >&2
    exit 1
  fi
fi

if ! CONTEXTS="$(gh api "repos/${REPO}/branches/${BASE}/protection" \
                   --jq '.required_status_checks.contexts[]' 2>&1)"; then
  echo "required-checks.sh: cannot read branch protection for ${REPO}@${BASE}." >&2
  echo "$CONTEXTS" >&2
  exit 1
fi

# Trim blank lines before deciding the list is non-empty — a protection object
# with a null `required_status_checks` yields empty output at exit 0.
CONTEXTS="$(printf '%s\n' "$CONTEXTS" | sed '/^[[:space:]]*$/d')"

if [[ -z "$CONTEXTS" ]]; then
  echo "required-checks.sh: ${REPO}@${BASE} lists NO required status checks." >&2
  echo "                    Refusing to report that as a pass — see the header." >&2
  exit 1
fi

printf '%s\n' "$CONTEXTS"
