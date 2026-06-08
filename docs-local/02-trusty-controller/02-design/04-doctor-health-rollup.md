# DOC-4 — Doctor/Health Rollup Model

**Status:** Draft — stub
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Define how the controller aggregates per-tool doctor/health JSON into a stack
verdict and renders a tools × scope matrix, including a comprehensive stack doctor.

## Open Questions / Decisions to Resolve

- Define aggregation rules: per-check `status` + `scope` → per-tool verdict →
  stack verdict. System failures are global; project "pending" is local.
- Decide handling of older `contract_version` tools: render degraded, not failure.
- Distinguish a *missing* tool (in manifest, not installed) from a *down* tool.
- Define remediation surfacing: contract `remediation` hints → actionable output.
- Define exit-code semantics for `stack doctor` / `stack health` in CI.

## Dependencies

### Consumes (inputs)
- DOC-1 (the per-verb JSON schemas).
- DOC-2 (the manifest enumeration of members to roll up).
- DOC-3 (scope semantics: system-global vs project-local).

### Produces (consumed by)
- DOC-5, DOC-7, DOC-10.

## Grounding (exists vs. net-new)

- **Exists but unusable for rollup:** per-tool `doctor` / `status` exist but are
  text-only and not aggregatable.
- **Only structured signal today:** the daemon `GET /health` JSON.
- **Net-new but thin:** rollup logic is straightforward once DOC-1 lands and tools
  emit structured JSON.

## Cross-cutting notes

- **Contract-versioning behavior:** include an "older-contract behavior"
  subsection — when a tool advertises an older `contract_version`, render its
  rollup row as *degraded*, do not fail the whole stack verdict.

## TODO

- [ ] Resolve open questions above
- [ ] Define schemas/contracts
- [ ] Review with team
