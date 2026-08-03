Fixed

- Swept shipped asset text that still advertised the five retired
  `.trusty-mpm/` per-file PM instruction overrides (#4286): the `tm-workflow`
  skill described them as the live customization mechanism and told the PM to
  write to them; three output styles (`trusty-mpm`, `trusty-mpm-research`,
  `trusty-mpm-teacher`) described the compiled prompt as four monolithic files
  (`PM_INSTRUCTIONS.md` + `WORKFLOW.md` + `AGENT_DELEGATION.md` +
  `BASE_PM.md`) that haven't existed since #4183; `tm-delegation-patterns`,
  `tm-circuit-breaker`, and `tm-pr-workflow` pointed at `AGENT_DELEGATION.md`
  / `.trusty-mpm/INSTRUCTIONS.md` as if still reachable. All now describe the
  current model: framework instructions compose from
  `assets/instructions/sections/*.md`, and the sole project-customization
  channel is a named-section marker in the project's root `CLAUDE.md` — `core`
  is the only section such a marker cannot replace.
- `assets/instructions/sections/workflow.md` pointed the project test-ladder
  lookup at the retired `.trusty-mpm/INSTRUCTIONS.md`; it now points at
  `CLAUDE.md`. This is a compiled-prompt change, so the `pm-prompt-bundled-
  fallback.md` and `pm-prompt-roster-absent.md` golden fixtures were
  regenerated (`UPDATE_GOLDEN=1 cargo test -p trusty-mpm golden`) to match.
- Verified no scaffolding path (`tm-init`, `tm project init`) creates any of
  the five retired override files — the only writers found were the
  `legacy_overrides` doctor check's own fixtures.
