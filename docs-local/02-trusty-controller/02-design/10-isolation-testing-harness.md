# DOC-10 — Isolation Testing Harness (MUC1, MUC2)

**Status:** Draft — stub
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Specify how a maintainer tests install/upgrade of the whole stack in a vanilla
container/VM — macOS primary, Linux secondary — without contaminating their own
machine.

## Open Questions / Decisions to Resolve

- macOS isolation (primary): VM (tart/UTM) vs an ephemeral user. launchd is
  per-user (`gui/$(id -u)`), so true isolation likely needs a VM, not just a
  clean `$HOME`.
- Linux isolation (secondary): containers are straightforward — no launchd; use
  systemd or foreground processes.
- What the harness asserts: bootstrap from zero → `stack doctor` green → upgrade →
  still green.
- CI vs maintainer-run.
- How to fetch/build the BOM tuple under test.

## Dependencies

### Consumes (inputs)
- DOC-8 (install/bootstrap flow under test).
- DOC-9 (upgrade flow under test).
- DOC-5 (the CLI driven by the harness).
- DOC-4 (the `stack doctor` verdict asserted).
- DOC-2 (the BOM tuple under test).

### Produces (consumed by)
- Terminal — nothing depends on the harness.

## Grounding (exists vs. net-new)

- **Net-new harness.**
- **Constraints:** launchd is per-user (`gui/$(id -u)`); the macOS codesign/cdhash
  caveat means VM binary installs must use `cargo install` (atomic rename), never
  `cp`.

## Cross-cutting notes

- **Isolation-testability** is the whole subject of this doc — it validates the
  side-effect-scoping that DOC-8 and DOC-9 must provide.

## TODO

- [ ] Resolve open questions above
- [ ] Define schemas/contracts
- [ ] Review with team
