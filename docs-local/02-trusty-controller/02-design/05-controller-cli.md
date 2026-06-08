# DOC-5 — Controller CLI Command Surface + Dispatch

**Status:** Draft — stub
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Define the controller's own CLI (the spec's example operations) plus the
manifest-driven dispatch that fans verbs out to the stack's tools.

## Open Questions / Decisions to Resolve

- Lock the command surface: `install stack`,
  `show available updates [+ changelog headlines]`, `upgrade stack`,
  `restart [all daemons + UI]`, `stack health`, `stack doctor`, with `--scope` on each.
- Specify a dispatch architecture that proves zero tool-specific logic: every
  command = read manifest → invoke the contract verb per member → roll up.
- Design the warn-before-blast-radius UX for system-mutating ops.
- Apply Unix-philosophy sharp-tool conventions: JSON-out, composability, exit codes.
- Define the clap 4 derive structure (reuse `toolchains-rust-cli-clap` patterns).

## Dependencies

### Consumes (inputs)
- DOC-1 (contract verbs to dispatch).
- DOC-2 (manifest = the dispatch registry).
- DOC-3 (scope semantics + default-scope behavior).
- DOC-4 (rollup logic the commands render).

### Produces (consumed by)
- DOC-7, DOC-8, DOC-9, DOC-10.

## Grounding (exists vs. net-new)

- **Consistent template:** all four tools use clap 4 derive.
- **Reusable dispatch primitives:** `<tool> port` and the daemon HTTP clients in
  `trusty_common`.
- **Net-new:** the controller crate itself (a new `crates/<name>/`).

## Cross-cutting notes

- **Contract-versioning behavior:** include an "older-contract behavior"
  subsection covering dispatch fallbacks when a member advertises an older contract.

## TODO

- [ ] Resolve open questions above
- [ ] Define schemas/contracts
- [ ] Review with team
