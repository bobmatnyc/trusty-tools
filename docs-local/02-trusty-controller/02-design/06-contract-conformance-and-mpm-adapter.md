# DOC-6 — Per-Tool Contract Conformance + claude-mpm Python Adapter

**Status:** Draft — stub
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Audit each existing tool against DOC-1, enumerate per-tool gap-closure work, and
specify how the Python-based claude-mpm satisfies the same contract.

## Open Questions / Decisions to Resolve

- Build a per-tool gap table: add `version --json` + `contract_version`
  everywhere; add `--json` to `doctor`/`health`; bring `trusty-review` up to the
  baseline (only `serve` today); standardize `restart`/`config`.
- Decide where retrofits live: per-crate vs a shared `trusty_common` contract
  module. Recommend the latter — update/launchd/shutdown plumbing already lives there.
- Decide the claude-mpm strategy: native `claude-mpm doctor --json` /
  `version --json` in its own repo, vs a controller-side Python adapter that maps
  `mpm-doctor` / skill output to the contract JSON. (claude-mpm is external
  Python; today only a brand adapter exists in `trusty-agents-common`; it has an
  `mpm-doctor` skill but no machine-contract surface.)
- Decide ownership and sequencing of the cross-repo work.

## Dependencies

### Consumes (inputs)
- DOC-1 (the contract to conform to).
- DOC-2 (the manifest enumeration of which tools to audit).

### Produces (consumed by)
- DOC-4 and DOC-5 (which can only roll up / dispatch to conformant tools).
- Gates DOC-10 (isolation testing can only validate conformant tools).

## Grounding (exists vs. net-new)

- **Most anchored in investigation.** `trusty-search` is richest (`doctor --fix`,
  `config`, `status`/`health`, `start`/`stop`, `serve`, `service` [launchd],
  `port`, `upgrade`, `integrate`). memory/analyze have most of these;
  `trusty-review` has only `serve`.
- `trusty_common` is the natural home for a shared contract envelope.

## Cross-cutting notes

- **Project-identity:** each tool must honor the shared identity convention
  (reference DOC-3) so project-scoped verbs bind to the right cwd.

## TODO

- [ ] Resolve open questions above
- [ ] Define schemas/contracts
- [ ] Review with team
