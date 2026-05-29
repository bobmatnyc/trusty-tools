# 1. Single-install, multi-binary bundling of the trusty-mpm surfaces

- **Status:** Accepted
- **Date:** 2026-05-29
- **Deciders:** trusty-mpm maintainers
- **Format:** [Nygard ADR](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions)

## Context

trusty-mpm originated as eight separately-published sub-crates —
`trusty-mpm-{core, client, mcp, daemon, cli, tui, telegram, gui}`. Publishing
them independently required a `[patch.crates-io]` dance for cross-crate
development, forced users to install and version-coordinate eight packages, and
meant a simple `cargo install trusty-mpm` did not exist. The eight surfaces also
duplicated their daemon-client HTTP layers, so each new endpoint had to be wired
three times and the UIs drifted apart.

The product is inherently multi-surface: a developer wants one CLI, a background
daemon, an in-session MCP server, a TUI, and (optionally) a Telegram bot and a
desktop GUI — but most users want only a subset, and they should not pay the
compile cost of components they do not use.

## Decision

Consolidate the sub-crates into **one Cargo crate** (`crates/trusty-mpm`) that
exposes its surfaces as **feature-gated `[[bin]]` targets** sharing common
library modules:

- Library modules `core`, `client`, and `services` are always compiled; `mcp`,
  `daemon`, `tui`, and `telegram` are `#[cfg(feature = …)]`-gated.
- Bin targets `tm` / `trusty-mpm` (`cli`), `trusty-mpmd` (`daemon`),
  `trusty-mpm-tui` (`tui`), `trusty-mpm-telegram` (`telegram`), and
  `trusty-mpm-gui` (`gui`) are each gated by `required-features`.
- A feature chain (`cli → daemon + tui + telegram`; `daemon → mcp`) makes
  `default = ["cli", "daemon"]` install the common toolset, while keeping the
  cost of each optional component opt-in.
- All five surfaces call the **same library functions**; the standalone
  `trusty-mpm-{tui,telegram,gui}` binaries are kept only as backward-compatible
  shims, with `tm <subcommand>` as the canonical entry point.
- The Tauri GUI stays in the separate `trusty-mpm-gui` crate (it owns `build.rs`
  + `tauri.conf.json`, which cannot be merged into a generic crate) and is
  wrapped as an optional dependency behind the `gui` feature.

`cargo install trusty-mpm` therefore yields one install target with the daemon,
MCP server, TUI, and Telegram bot bundled, no external runtime, and framework
assets compiled in (offline-capable).

## Consequences

**Positive**

- One install (`cargo install trusty-mpm`); no eight-package version coordination.
- No `[patch.crates-io]` dance for cross-crate development.
- The shared `client` seam means a new daemon endpoint is wired once and the
  CLI/TUI/Telegram surfaces never drift.
- Users pay compile cost only for features they enable.

**Negative / trade-offs**

- "Always-on" CLI/binary support deps (`clap`, `tracing-subscriber`, `colored`,
  `sysinfo`, `libc`) are compiled even for `--no-default-features` library
  consumers, because the bin targets pull them in.
- Consolidation concentrates surface area in one crate: `src/bin/tm.rs` has grown
  to ~4,442 lines, over the workspace 500-line cap (split tracked by #395).
- The GUI cannot be fully in-crate; the `gui` feature is a thin shim over the
  separate Tauri crate, so the bundling story has one explicit exception.

## References

- `crates/trusty-mpm/Cargo.toml` (`[features]`, `[[bin]]`)
- `crates/trusty-mpm/src/lib.rs` (module `Why` rationale)
- `crates/trusty-mpm/README.md`
- [spec/ARCHITECTURE.md §2](../spec/ARCHITECTURE.md#2-multi-binary-topology-single-install-bundling)
- Issue #343 (decouple trusty-mpm-* crates from the shared `[workspace.package]`
  version)
