Removed

- The five `.trusty-mpm/` PM instruction override files
  (`PM_INSTRUCTIONS_DEPLOYED.md`, `AGENT_DELEGATION.md`, `WORKFLOW.md`,
  `MEMORY.md`, `INSTRUCTIONS.md`) are retired and no longer read. Project
  customization is named sections in the project's root `CLAUDE.md`.
  `.trusty-mpm/INSTRUCTIONS.md` also stops being a marker host; `CLAUDE.md` is
  the only one (#4286).
- The non-overridable framework floor, in full: `SectionId::is_floor()`, the
  `FloorNotFixed` and `OverridableAfterFloor` validation rules,
  `validate_floor_is_last`, `scripts/check_instruction_floor.sh`,
  `scripts/instruction_floor.sha256`, and
  `.github/workflows/instruction-floor-guard.yml` (plus its duplicated step in
  `ci.yml`). A project owns its own `CLAUDE.md`, so the floor was the appearance
  of a control rather than a control (#4286).

Changed

- `core` is now the ONLY section a named-section override cannot replace.
  `identity`, `enforcement`, `non-overridable-rules` and
  `framework-guaranteed-conventions` become tier `project` and are overridable
  like every other section; no content moved between sections. `validate`
  enforces the tier assignment as an iff, so both retiering `core` away from
  `fixed` and marking a second section `fixed` are hard errors (#4286).
- The seeded project `CLAUDE.md` stub now documents the marker grammar, lists
  the accepted tokens, and points at `.trusty-mpm/last-instructions.md` as the
  record of what the session actually received. It no longer refers to
  `BASE_PM.md`, which has not existed since #4183 (#4286).

Added

- `tm doctor` check `legacy_overrides`: FAILS when a project still carries any
  retired override file, naming every file found and the migration. The prompt
  resolver logs the same signal on every session launch, so a leftover file can
  never drop a project's rules silently (#4286).
- `crates/trusty-mpm/src/assets/instructions/sections/README.md` documenting how
  framework instructions compose, the customization tiers, why there is no
  floor, and what must not be reintroduced (#4286).

Fixed

- The seeded `CLAUDE.md` stub could declare a live override. A worked marker
  example in the stub was parsed as a real `WORKFLOW` block — marker recognition
  is whole-line and knows nothing about code fences — so every newly seeded
  project silently lost its entire bundled workflow section and received the
  placeholder prose instead. Found by running a real `tm` instance during
  acceptance; guarded by `seeded_claude_md_declares_no_overrides` (#4286).
