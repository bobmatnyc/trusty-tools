#!/usr/bin/env bash
#
# ci-gate-relevance.sh — decide whether a change set reaches one of the two
#   pure-shell gates `ci.yml` path-filters at the job level (#8378).
#
# Why: `Durable writes hold the teardown guard` and `Every tmux -t target is
#   exact` ran on every PR, docs-only ones included, because a required
#   context behind a `paths:` trigger filter never reports (#4468). They now
#   live in ci.yml behind the `changes` job, skip at the JOB level when this
#   script answers `false`, and the `CI gate` job accepts that skip only when
#   this script said so. A skipped job reports success to branch protection;
#   a missing one blocks forever — the skip is the safe shape, the trigger
#   filter is not.
#
# What: prints `true` when any changed path is an input of the named gate,
#   `false` otherwise. Inputs mirror what each gate scans:
#     teardown-guard  crates/trusty-search/src/**, the gate's two TSVs, its
#                     script and selftest, and scripts/lib/sloc_awk.sh, which
#                     the gate sources
#     tmux-targets    every *.rs *.sh *.swift *.ts *.tsx *.js *.mjs *.svelte
#                     outside node_modules, every crates/*/src/assets/**/*.md,
#                     the allowlist TSV, and the gate's script and selftest
#   Both modes: this script and .github/workflows/ci.yml, the workflow that
#   hosts the gates — a change to either must exercise the jobs it governs.
#   The file status is ignored: a DELETE can strand an allowlist or manifest
#   row, which is itself a gate failure.
#
#   FAIL CLOSED. No usable base, an unresolvable merge-base, a failed diff, an
#   empty change set, or a C-quoted path (git quotes a path holding a
#   non-ASCII byte, a quote or a backslash; the quoted form matches no rule)
#   all answer `true`.
#
# Usage:
#   git diff --name-only --no-renames "$MERGE_BASE" HEAD |
#     bash scripts/ci-gate-relevance.sh tmux-targets
#   EVENT_NAME=pull_request BASE_REF=main bash scripts/ci-gate-relevance.sh teardown-guard
#
# Output: `true` or `false` on stdout. When $GITHUB_OUTPUT is set, also
#   `<mode>_relevant=<verdict>` with `-` mapped to `_` (teardown_guard_relevant,
#   tmux_targets_relevant).
#
# Test: scripts/ci-gate-selftest.sh (`ci-gate-relevance:` cases).

set -euo pipefail

SELF_PATH="scripts/ci-gate-relevance.sh"
WORKFLOW_PATH=".github/workflows/ci.yml"

emit() {
  echo "$2"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    printf '%s_relevant=%s\n' "$(printf '%s' "$1" | tr - _)" "$2" >>"${GITHUB_OUTPUT}"
  fi
}

# is_relevant <mode> <path>
is_relevant() {
  local mode="$1" p="$2"

  if [ "$p" = "$SELF_PATH" ] || [ "$p" = "$WORKFLOW_PATH" ]; then
    return 0
  fi
  # A C-quoted path cannot be matched against any rule below.
  case "$p" in \"*) return 0 ;; esac

  case "$mode" in
    teardown-guard)
      case "$p" in
        crates/trusty-search/src/?*) return 0 ;;
        scripts/teardown-guard-methods.tsv | scripts/teardown-guard-manifest.tsv) return 0 ;;
        scripts/check_teardown_guard.sh | scripts/check_teardown_guard_selftest.sh) return 0 ;;
        scripts/lib/sloc_awk.sh) return 0 ;;
      esac
      ;;
    tmux-targets)
      case "$p" in
        scripts/tmux-exact-targets-allowlist.tsv) return 0 ;;
        scripts/check_tmux_exact_targets.sh | scripts/check_tmux_exact_targets_selftest.sh) return 0 ;;
        node_modules/* | */node_modules/*) return 1 ;;
        *.rs | *.sh | *.swift | *.ts | *.tsx | *.js | *.mjs | *.svelte) return 0 ;;
        # In a bash `case`, `*` matches `/`: any depth below assets/.
        crates/*/src/assets/*.md) return 0 ;;
      esac
      ;;
  esac
  return 1
}

# changed_paths — this event's changed paths, one per line, or non-zero.
changed_paths() {
  local base merge_base
  if [ "${EVENT_NAME:-}" = "pull_request" ]; then
    base="origin/${BASE_REF:-}"
  else
    base="${PUSH_BEFORE:-}"
  fi
  if [ -z "$base" ] || [ "$base" = "origin/" ] ||
    [ "$base" = "0000000000000000000000000000000000000000" ]; then
    echo "ci-gate-relevance: no usable base SHA — answering true (fail closed)" >&2
    return 1
  fi
  if ! merge_base="$(git merge-base "$base" HEAD 2>/dev/null)"; then
    echo "ci-gate-relevance: cannot resolve merge-base against '${base}' — answering true (fail closed)" >&2
    return 1
  fi
  if ! git diff --name-only --no-renames "$merge_base" HEAD; then
    echo "ci-gate-relevance: git diff against ${merge_base} failed — answering true (fail closed)" >&2
    return 1
  fi
}

main() {
  local mode="${1:-}"
  case "$mode" in
    teardown-guard | tmux-targets) ;;
    *)
      echo "ci-gate-relevance: usage: $0 <teardown-guard|tmux-targets> [< changed-paths]" >&2
      return 2
      ;;
  esac

  # With EVENT_NAME set (a CI step) the script resolves its own diff; without
  # it, the paths arrive on stdin — the shape the selftest drives.
  local input
  if [ -n "${EVENT_NAME:-}" ]; then
    if ! input="$(changed_paths)"; then
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
    echo "ci-gate-relevance: empty change set — answering true (fail closed)" >&2
    relevant=true
  fi

  echo "ci-gate-relevance: ${mode} -> ${relevant} (${count} changed path(s))" >&2
  emit "$mode" "$relevant"
}

main "$@"
