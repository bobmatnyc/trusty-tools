# Design Archive

This directory contains superseded design system versions.

## gui-v1 (Archived 2026-07-19)

The Foundry v1 design system, from PR #3175 (2026-07-15). This directory is a complete duplicate of the v2 system files now located at `docs/design/UI/` (from PR #3170, 2026-07-18).

- **Status:** Superseded by v2 (2026-07-18)
- **Why archived:** v1 and v2 are byte-identical; v1 is unreferenced in the codebase; maintaining one canonical copy simplifies updates and reduces confusion
- **Why not deleted:** Preserved in git history for safe recovery if needed

**To recover:** `git checkout HEAD~N -- docs/design/gui/` (find the commit before archival in git log), or reference the git history of files now at `docs/design/UI/`.

Canonical design system is now at `docs/design/UI/`. See `docs/design/UI/README.md` for current guidance.
