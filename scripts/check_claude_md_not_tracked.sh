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

# #4618: the scan floor. This gate's whole verdict came from ONE probe whose
# failure mode is silence: `git ls-files --error-unmatch CLAUDE.md 2>/dev/null`
# is non-zero both when the file is untracked (the pass) and when git itself
# broke (not a pass at all). Two guards close that:
#   1. a positive control — git must be able to list the index at all, and the
#      index must hold a plausible number of files. If `git ls-files` returns
#      nothing, the probe below proves nothing.
#   2. exit-code discrimination — --error-unmatch returns exactly 1 for "no such
#      tracked path" and 128 for a git failure, so only 1 is read as untracked.
MIN_TRACKED_FILES=500

if ! tracked_list="$(git ls-files 2>&1)"; then
  echo "FAIL: TOOL ERROR — 'git ls-files' failed; the index could not be read:" >&2
  printf '%s\n' "$tracked_list" | sed 's/^/       /' >&2
  echo "      The tracked/untracked probe below would prove nothing (issue #4618)." >&2
  exit 1
fi

tracked_count="$(printf '%s\n' "$tracked_list" | grep -c '[^[:space:]]' || true)"
if [ "${tracked_count:-0}" -lt "$MIN_TRACKED_FILES" ]; then
  echo "FAIL: SCAN FLOOR — git reports only ${tracked_count} tracked file(s), below the" >&2
  echo "      declared minimum of ${MIN_TRACKED_FILES}. A 'CLAUDE.md is not tracked' verdict drawn" >&2
  echo "      from an index this empty is meaningless (issue #4618)." >&2
  exit 1
fi

probe_rc=0
probe_err="$(git ls-files --error-unmatch CLAUDE.md 2>&1 >/dev/null)" || probe_rc=$?

if [ "$probe_rc" -ne 0 ] && [ "$probe_rc" -ne 1 ]; then
  echo "FAIL: TOOL ERROR — 'git ls-files --error-unmatch CLAUDE.md' exited ${probe_rc}" >&2
  echo "      (expected 0 = tracked, 1 = untracked):" >&2
  printf '%s\n' "$probe_err" | sed 's/^/       /' >&2
  echo "      A git failure is not the same as 'untracked' (issue #4618)." >&2
  exit 1
fi

if [ "$probe_rc" -eq 0 ]; then
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

echo "PASS: root CLAUDE.md is not tracked — probed against ${tracked_count} tracked file(s) (see #2299, #4618)."
exit 0
