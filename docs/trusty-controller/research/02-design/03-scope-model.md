# DOC-3 — Scope Model (System vs Project)

**Status:** Draft — stub
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Specify the behavioral model behind the scope axis: layered readiness,
idempotency, blast radius, config precedence, and ensure-system-then-project
ordering.

## Open Questions / Decisions to Resolve

- Formalize the readiness ladders:
  `system: installed → running → healthy → version-ok`;
  `project: configured → exists → fresh → ready`. An unindexed project is
  *system-ready, project-pending* — NOT broken.
- Define verb scope-polymorphism and default scope:
  `install`/`upgrade`/`restart` → system; `health`/`doctor`/`config` → both;
  `index`/`palace-create` → project. Default `all` in a project dir, else `system`.
- Define idempotency: `install` runs once; `ensure project` runs every launch and
  must no-op when set up (the UUC1 auto-config engine).
- Define blast-radius tagging: system-mutating ops warn before acting; project
  ops never implicitly trigger system ops.
- Define the shared project-identity convention (git root → index-id / palace-id).
- Define config precedence: project overrides system.

## Dependencies

### Consumes (inputs)
- DOC-0 (the chosen `<name>`).
- DOC-1 — bidirectional (DOC-1 owns the `scope` wire format; this doc owns its model).

### Produces (consumed by)
- DOC-4, DOC-5, DOC-8, and DOC-1 (feeds the `scope` schema fields back into the contract).

## Grounding (exists vs. net-new)

- **Partially exists:** project-identity logic already exists
  (`trusty_common::project_discovery::discover_claude_projects`; search `IndexId`
  is keyed on the git root). The daemons-as-singletons + per-project-state
  architecture is already in place.
- **Net-new:** the cross-tool formalization of the scope axis (ladders,
  polymorphism, blast-radius tagging) as a shared contract concept.

## Cross-cutting notes

- **Project-identity convention** is DEFINED HERE (git root → index-id/palace-id)
  and referenced by DOC-6 and DOC-8.

## TODO

- [ ] Resolve open questions above
- [ ] Define schemas/contracts
- [ ] Review with team
