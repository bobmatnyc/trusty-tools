#!/usr/bin/env bash
#
# check_instruction_floor.sh — non-overridable instruction floor guard
# (issues #3374, #4183).
#
# Why: #3374 found that framework-guaranteed conventions (the commit/PR
#   attribution footer, the proportional-documentation policy, the
#   `// #1234: <reason>` ticket-attribution convention) had drifted onto
#   user-editable surfaces with no copy in the one channel every session
#   actually receives unconditionally. Advice without a gate loses (the repo's
#   established pattern — see `check_claude_md_not_tracked.sh` for
#   #2299/#2647), so #3374 turned the finding into a mechanical CI check.
#
#   That first check was substring-based (`grep -qF` for three needles), which
#   made it theatre: it passed when every rule in the floor was INVERTED
#   ("always emit Co-Authored-By" instead of never), when the floor was reduced
#   to the three bare needles and nothing else, and when the whole floor was
#   wrapped in an HTML comment. Only an emptied or missing file failed.
#
# What: the floor is now pinned BYTE-EXACTLY. This script recomputes a sha256
#   digest for every artifact that makes up the floor and diffs the result
#   against the committed digest file `scripts/instruction_floor.sha256`. Any
#   byte difference — inversion, truncation, comment-wrapping, emptying — is a
#   hard failure. The pinned set is:
#     1. every `sections/*.md` sourced by a `customization_tier: "fixed"`
#        section of the PM instruction package manifest;
#     2. a canonical projection of the manifest itself, covering the fixed
#        sections' metadata and the blocks that fill them — so retiering a
#        floor section to "project", repointing it at another file, or deleting
#        its block is caught even when no section file changed;
#     3. the guard's own workflow, so deleting or neutering
#        `.github/workflows/instruction-floor-guard.yml` also turns CI red.
#        This script additionally runs inside ci.yml's required `Format check`
#        job, so the check outlives deletion of its own workflow file.
#
#   Plus two structural checks retained from #3374: the dead
#   `assets/instructions/CLAUDE.md` stub must not reappear, and the retired
#   monolithic `BASE_PM.md` must not come back.
#
# Deliberate floor change (the intended update path): edit the floor, then run
#
#     bash scripts/check_instruction_floor.sh --update
#
# and commit the regenerated `scripts/instruction_floor.sha256` alongside the
# floor edit. A legitimate change is a two-part commit; an unreviewed one is a
# red build.
#
# Requires: python3 (present on GitHub Actions ubuntu-latest runners and on
# macOS with the Xcode command line tools). All hashing happens in python so
# digests do not depend on sha256sum vs. shasum availability.
#
# May be run from anywhere inside the repo; paths resolve against the root.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

DEAD_ASSET="crates/trusty-mpm/src/assets/instructions/CLAUDE.md"
LEGACY_FLOOR="crates/trusty-mpm/src/assets/instructions/BASE_PM.md"
INSTRUCTIONS_DIR="crates/trusty-mpm/src/assets/instructions"
MANIFEST="$INSTRUCTIONS_DIR/pm-instruction-package.json"
GUARD_WORKFLOW=".github/workflows/instruction-floor-guard.yml"
DIGESTS="scripts/instruction_floor.sha256"

UPDATE=0
case "${1:-}" in
  --update) UPDATE=1 ;;
  "") ;;
  *)
    echo "usage: $0 [--update]" >&2
    exit 2
    ;;
esac

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

# The former monolithic BASE_PM.md must not come back: two sources for the floor
# is exactly the drift #3374 removed.
if [[ -e "$LEGACY_FLOOR" ]]; then
  echo "FAIL: $LEGACY_FLOOR has reappeared; the floor is authored per-section (#4183)."
  fail=1
fi

if [[ ! -f "$MANIFEST" ]]; then
  echo "FAIL: $MANIFEST is missing — the instruction floor manifest must exist."
  exit 1
fi

# Emit `<sha256>  <label>` for every artifact in the pinned floor set, sorted by
# label so the listing is stable regardless of manifest ordering.
compute_digests() {
  python3 - "$MANIFEST" "$INSTRUCTIONS_DIR" "$GUARD_WORKFLOW" <<'PYEOF'
import hashlib
import json
import os
import sys

manifest_path, instructions_dir, guard_workflow = sys.argv[1:4]

with open(manifest_path, "rb") as fh:
    manifest = json.loads(fh.read().decode("utf-8"))

fixed_sections = [
    s for s in manifest["sections"] if s.get("customization_tier") == "fixed"
]
if not fixed_sections:
    sys.exit(
        'FAIL: the manifest declares no `customization_tier: "fixed"` section '
        "— the non-overridable floor is gone."
    )

fixed_ids = {s["id"] for s in fixed_sections}
fixed_blocks = [b for b in manifest["blocks"] if b.get("section") in fixed_ids]

# Canonical projection of everything in the manifest that defines the floor:
# the fixed sections' own metadata and the block stream that fills them. Any
# retiering, repointing, reordering or deletion changes this digest even when
# no section file changed.
projection = {"fixed_sections": fixed_sections, "fixed_blocks": fixed_blocks}
canonical = json.dumps(
    projection, sort_keys=True, ensure_ascii=False, separators=(",", ":")
).encode("utf-8")

entries = [("manifest-projection:fixed-sections", hashlib.sha256(canonical).hexdigest())]

paths = [guard_workflow]
for block in fixed_blocks:
    body = block.get("body", {})
    if body.get("kind") == "file":
        paths.append(os.path.join(instructions_dir, body["path"]))

missing = []
for path in paths:
    try:
        with open(path, "rb") as fh:
            entries.append((path, hashlib.sha256(fh.read()).hexdigest()))
    except FileNotFoundError:
        missing.append(path)

if missing:
    sys.exit("FAIL: pinned floor artifact(s) missing: " + ", ".join(sorted(missing)))

for label, digest in sorted(entries):
    print(f"{digest}  {label}")
PYEOF
}

DIGEST_HEADER="# Byte-exact sha256 digests of the non-overridable PM instruction floor.
#
# Generated by \`bash scripts/check_instruction_floor.sh --update\`. Do not hand-edit.
#
# A deliberate floor change is a two-part commit: the floor edit AND the
# regenerated digests below. Regenerating these without a reviewed floor change
# defeats the guard (issues #3374, #4183)."

if ! actual="$(compute_digests)"; then
  # compute_digests already printed its own FAIL line via sys.exit().
  exit 1
fi

if [[ "$UPDATE" -eq 1 ]]; then
  printf '%s\n%s\n' "$DIGEST_HEADER" "$actual" >"$DIGESTS"
  echo "UPDATED: $DIGESTS regenerated from the working tree."
  echo "  Commit it together with the floor change it pins."
  exit 0
fi

if [[ ! -f "$DIGESTS" ]]; then
  echo "FAIL: $DIGESTS is missing — the floor has no pinned digests to verify against."
  echo "  Regenerate with: bash scripts/check_instruction_floor.sh --update"
  exit 1
fi

expected="$(grep -v -e '^#' -e '^[[:space:]]*$' "$DIGESTS" || true)"

if [[ "$actual" != "$expected" ]]; then
  echo "FAIL: the non-overridable instruction floor does not match its pinned digests."
  echo
  echo "  '-' = committed $DIGESTS, '+' = working tree:"
  diff <(printf '%s\n' "$expected") <(printf '%s\n' "$actual") | sed 's/^/    /' || true
  echo
  echo "  Every byte of the floor is pinned, so this fires on ANY edit —"
  echo "  including an inversion, a truncation, a comment-wrap or an emptying"
  echo "  that a substring check would have waved through."
  echo
  echo "  If the floor change is DELIBERATE and reviewed, regenerate the pin:"
  echo
  echo "      bash scripts/check_instruction_floor.sh --update"
  echo
  echo "  and commit $DIGESTS alongside the floor edit. If you did not intend"
  echo "  to change the floor, revert the edit instead."
  fail=1
fi

if [[ "$fail" -ne 0 ]]; then
  echo
  echo "  The framework-guaranteed conventions live in the fixed-tier sections"
  echo "  of $MANIFEST — the only channel"
  echo "  appended unconditionally to every PM prompt"
  echo "  (core/instruction_overrides.rs::resolve_pm_prompt). Bundled skills"
  echo "  and project-scoped files may restate them as elaboration, but must"
  echo "  never be their sole home. See issue #3374."
  exit 1
fi

echo "PASS: instruction floor matches its pinned byte-exact digests (see #3374, #4183)."
exit 0
