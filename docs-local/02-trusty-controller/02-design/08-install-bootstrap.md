# DOC-8 — Install/Bootstrap Flow (UUC1, UUC2)

**Status:** Draft — stub
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Specify the zero-knowledge install (UUC2) and per-project auto-config on
claude-mpm launch (UUC1), including the Rust-toolchain hard dependency and a
progressive-readiness UX.

## Open Questions / Decisions to Resolve

- Bootstrap from a vanilla machine: detect/require `cargo` (hard dep),
  `cargo install` each member, install launch agents, then run per-project ensure.
- UUC1 auto-config: define what fires when claude-mpm launches in a project dir —
  `.mcp.json` patching + index/palace creation, idempotent per DOC-3.
- Progressive-readiness UX (spec's explicit question): trusty-search sets up
  immediately but needs time to index — what does waiting look like? (search has
  SSE reindex progress to reuse.)
- Bootstrap ordering: ensure system → then project.

## Dependencies

### Consumes (inputs)
- DOC-2 (manifest: what to install and from where).
- DOC-3 (scope ordering + idempotency for `ensure project`).
- DOC-5 (the `install stack` command surface).

### Produces (consumed by)
- DOC-10.

## Grounding (exists vs. net-new)

- **Exists:** `cargo install` per-crate; `trusty_common::launchd` installs agents;
  `claude_config::patch_mcp_server` + `trusty-search integrate` patch `.mcp.json`;
  SSE reindex progress exists for the waiting UX.
- **Non-cargo path:** claude-mpm install is Python (pipx/uvx) — the one path not
  driven by `cargo install`.

## Cross-cutting notes

- **Isolation-testability:** the whole flow must be runnable non-interactively and
  side-effect-scoped so DOC-10 can exercise it in a VM/container.

## TODO

- [ ] Resolve open questions above
- [ ] Define schemas/contracts
- [ ] Review with team
