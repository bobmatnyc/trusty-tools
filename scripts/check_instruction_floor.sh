#!/usr/bin/env bash
#
# check_instruction_floor.sh — framework-guaranteed-conventions floor guard
# (issue #3374).
#
# Why: #3374 found that framework-guaranteed conventions (the commit/PR
#   attribution footer, the proportional-documentation policy, the
#   `// #1234: <reason>` ticket-attribution convention) had drifted onto
#   user-editable surfaces (a bundled skill a user can locally modify without
#   the deployer ever re-syncing it; a project-scoped-only override file) with
#   no copy in the one channel every session actually receives unconditionally
#   — `crates/trusty-mpm/src/assets/instructions/BASE_PM.md`. The same PR also
#   found a dead, unread duplicate of the attribution text at
#   `crates/trusty-mpm/src/assets/instructions/CLAUDE.md`, registered but never
#   consumed by any code path — a trap for a future accidental re-wiring.
#   Advice without a gate loses (the repo's established pattern — see
#   `check_claude_md_not_tracked.sh` for #2299/#2647); this script turns both
#   findings into a mechanical CI check.
#
# What: two checks, both against the repo root:
#   1. the dead asset `crates/trusty-mpm/src/assets/instructions/CLAUDE.md`
#      must not reappear.
#   2. `crates/trusty-mpm/src/assets/instructions/BASE_PM.md` must contain a
#      recognizable statement of each of the three framework-guaranteed
#      conventions (grep-based substring checks, not full-text matches, so
#      minor wording edits don't false-fail this gate).
#
# Must be run from the repo root so paths resolve unambiguously.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

DEAD_ASSET="crates/trusty-mpm/src/assets/instructions/CLAUDE.md"
FLOOR="crates/trusty-mpm/src/assets/instructions/BASE_PM.md"

fail=0

if [[ -e "$DEAD_ASSET" ]]; then
  echo "FAIL: $DEAD_ASSET has reappeared."
  echo
  echo "  Issue #3374 deleted this asset because it was registered"
  echo "  (bundle.rs CLAUDE_STUB, bundle_all.rs SeedOnce entry, paths.rs"
  echo "  claude_stub()) but never read back by any production code path —"
  echo "  a dead duplicate of the attribution footer that risked a future"
  echo "  accidental re-wiring into a second, drift-prone delivery channel."
  echo "  The real project CLAUDE.md seed lives in CLAUDE_MD_STUB in"
  echo "  instruction_pipeline.rs. Do not reintroduce this file."
  fail=1
fi

if [[ ! -f "$FLOOR" ]]; then
  echo "FAIL: $FLOOR is missing — the non-overridable instruction floor must exist."
  exit 1
fi

check_convention() {
  local needle="$1"
  local label="$2"
  if ! grep -qF -- "$needle" "$FLOOR"; then
    echo "FAIL: $FLOOR is missing the $label convention."
    echo "  Expected to find: $needle"
    fail=1
  fi
}

check_convention "Generated with trusty-mpm" "commit/PR attribution footer"
check_convention "Proportional documentation" "proportional-documentation policy"
check_convention "Ticket attribution at the change site" "ticket-attribution-at-change-site"

if [[ "$fail" -ne 0 ]]; then
  echo
  echo "  Framework-guaranteed conventions must be stated in $FLOOR — the"
  echo "  only channel appended unconditionally to every PM prompt"
  echo "  (core/instruction_overrides.rs::resolve_pm_prompt). Bundled skills"
  echo "  and project-scoped files may restate them as elaboration, but must"
  echo "  never be their sole home. See issue #3374."
  exit 1
fi

echo "PASS: instruction floor is intact and carries the guaranteed conventions (see #3374)."
exit 0
