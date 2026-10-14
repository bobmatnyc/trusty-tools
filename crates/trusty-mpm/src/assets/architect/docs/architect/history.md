# The Architect: name and history (instance record)

Moved from architect.md "Name and history" on 2026-10-07 (restructure). This file names the owner and this host's paths, so it stays in the instance tree. The framework core keeps one generic line.

The Architect is the owner's fleet supervisor, named after the Architect in The Matrix Reloaded. Its canonical operating manual is `architect.md`, which tm composes into its instructions (trusty-tools #8436). `CLAUDE.md`, the `architect` agent and the skills in `.claude/skills/` point there. Since 2026-10-04 (the owner's rename ruling) the project home is `<projects>/architect` (tm project `architect`), the tmux sessions are `tm-architect` and `tm-architect-poll`, and the Claude and Remote Control name is `tm-architect`. Names that still say "supervisor" (the `[supervisor]` config table, some script paths and skill directories) stay until the Architect launches natively from `tm` (trusty-mpm 2.0, `docs/specs/SPEC-ARCHITECT-01.md`).

Everything specific to one user's fleet (watch set, dashboard, action log) lives in `local/`, which is tracked in this repository; per-project records live in each project's palace.

This file was originally named `AGENTS.md` until 2026-10-04 (the owner's ruling), when it was renamed to serve as the Architect's system prompt alongside the project-specific override layer in `CLAUDE.md`. On 2026-10-07 (the owner's layout ruling, 19:0xZ) the framework core moved back to a separate file and the instance layer became `AGENTS.md`. In trusty-tools #8436 the core became `content/instructions/architect/architect.md`, composed by tm; no copy of it lives in this directory.
