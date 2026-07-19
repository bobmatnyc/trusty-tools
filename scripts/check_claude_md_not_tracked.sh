#!/usr/bin/env bash
#
# check_claude_md_not_tracked.sh — root CLAUDE.md re-track guard (issue #2299).
#
# Why: issue #2299 moved the root CLAUDE.md to .trusty-mpm/INSTRUCTIONS.md
#   because a tracked root CLAUDE.md causes duplicate context loading
#   (~11k tokens/session) — Claude Code auto-loads both the tracked file
#   AND the project's own instruction injection. #2647 was a regression of
#   this fix: CLAUDE.md got re-tracked at the repo root and the duplicate
#   loading came back. Advice without a gate loses — this script turns
#   "don't re-track it" into a mechanical CI check.
#
# What: checks only the repo-root `CLAUDE.md` path (nested `crates/*/CLAUDE.md`
#   files are fine and out of scope). If `git ls-files --error-unmatch CLAUDE.md`
#   succeeds (exit 0, meaning the file IS tracked by git), this guard FAILS
#   (exit 1) with a message pointing back at #2299 and
#   .trusty-mpm/INSTRUCTIONS.md. If `git ls-files` reports the path as
#   untracked (non-zero exit), this guard PASSES (exit 0).
#
# Must be run from the repo root so the pathspec resolves unambiguously to
# the root-level file, not any nested CLAUDE.md.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if git ls-files --error-unmatch CLAUDE.md >/dev/null 2>&1; then
  echo "FAIL: root CLAUDE.md is tracked by git."
  echo
  echo "  Issue #2299 moved the root CLAUDE.md to .trusty-mpm/INSTRUCTIONS.md"
  echo "  specifically to avoid duplicate context loading (~11k tokens/session)"
  echo "  when Claude Code auto-loads both the tracked file and the project's"
  echo "  own instruction injection. #2647 was a prior regression of this."
  echo
  echo "  Fix: git rm --cached CLAUDE.md and put any new project-instruction"
  echo "  content in .trusty-mpm/INSTRUCTIONS.md instead. Nested"
  echo "  crates/*/CLAUDE.md files are unaffected — this guard only checks"
  echo "  the repo root."
  exit 1
fi

echo "PASS: root CLAUDE.md is not tracked (see #2299)."
exit 0
