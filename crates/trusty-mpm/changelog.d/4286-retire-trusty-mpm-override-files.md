Removed

- The five `.trusty-mpm/` PM instruction override files
  (`PM_INSTRUCTIONS_DEPLOYED.md`, `AGENT_DELEGATION.md`, `WORKFLOW.md`,
  `MEMORY.md`, `INSTRUCTIONS.md`) are retired and no longer read. Project
  customization is named sections in the project's root `CLAUDE.md`
  (`<!-- TRUSTY-MPM: <SECTION> START v=1 -->` … `END`). `.trusty-mpm/INSTRUCTIONS.md`
  also stops being a marker host; `CLAUDE.md` is the only one (#4286).

Added

- `tm doctor` check `legacy_overrides`: FAILS when a project still carries any
  retired override file, naming every file found and the migration. The prompt
  resolver logs the same signal on every session launch, so a leftover file can
  never drop a project's rules silently (#4286).
- `crates/trusty-mpm/src/assets/instructions/sections/README.md` documenting how
  framework instructions compose, the customization tiers, the pinned floor, and
  what is retired (#4286).

Changed

- Floor: the `non-overridable-rules` section now states the override files are
  retired rather than "still read by the current binary", and points at the
  `legacy_overrides` doctor check. `scripts/instruction_floor.sha256` regenerated
  for that one section (#4286).
- The PM prose-style rules gain a "do not embellish" clause with a worked
  before/after example (#4286).
