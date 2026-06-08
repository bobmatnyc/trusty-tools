# DOC-9 — Upgrade Flow (UUC3)

**Status:** Draft — stub
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Specify cross-tool update detection, changelog-headline rendering, the upgrade
action, and ensuring new versions take effect (restart).

## Open Questions / Decisions to Resolve

- Update detection across members: per-crate crates.io check vs the manifest's
  "latest known-good".
- Render changelog headlines (current → newest) per tool — needs DOC-2's
  structured changelog format.
- "New versions must take effect": orchestrate `cargo install` + graceful daemon
  restart (launchctl bootout/bootstrap) across the stack, including ordering and
  blast-radius warnings.
- Stack-version-aware upgrades: move to a known-good BOM tuple vs piecemeal-latest.

## Dependencies

### Consumes (inputs)
- DOC-2 (manifest/BOM + structured changelog).
- DOC-1 (`version --json` for detection).
- DOC-3 (scope ordering + blast-radius for restart).
- DOC-5 (the `upgrade stack` command surface).

### Produces (consumed by)
- DOC-10.

## Grounding (exists vs. net-new)

- **STRONGLY REUSABLE.** `trusty_common::update` (`check_crates_io`,
  `perform_upgrade`, `upgrade_and_restart`, `is_launchd_supervised`) plus the
  graceful-restart support (0.10.0) implement the single-tool path; this doc
  orchestrates that across the BOM.
- **Only net-new dependency:** changelog headlines (depends on DOC-2).

## Cross-cutting notes

- **Isolation-testability:** the upgrade path must be exercisable non-interactively
  in a VM/container (DOC-10).
- **Contract-versioning behavior:** include an "older-contract behavior"
  subsection — upgrade is itself the remediation for an out-of-date contract.

## TODO

- [ ] Resolve open questions above
- [ ] Define schemas/contracts
- [ ] Review with team
