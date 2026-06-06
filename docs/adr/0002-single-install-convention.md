# 0002. Single-install convention for main crates

- **Status:** Accepted
- **Date:** 2026-05-29
- **Scope:** Workspace-wide
- **Supersedes / Superseded by:** —

## Context

The trusty-* ecosystem ships several user-facing tools, each of which is really a
*family* of binaries: `trusty-mpm` provides `tm` and `trusty-mpm` (two install
names for the same binary), with `daemon`, `tui`, `telegram`, and `gui`
functionality exposed as subcommands rather than separate binaries;
`trusty-search` provides `trusty-search` plus its `trusty-embedderd` sidecar.

If these binaries lived in separate crates, a user would have to run multiple
`cargo install` commands and keep their versions in lock-step, and the daemon
could drift out of sync with its CLI. The workspace already standardizes on a
release/install flow (CLAUDE.md "Git Tag / Release Convention") that uses
`cargo install --path crates/<dir> --locked` and warns specifically against
`cp`-ing release binaries on macOS (stale `cdhash` code-signing cache → SIGKILL).
A one-crate-per-tool model makes that single, atomic install path possible.

The crate manifests confirm the pattern: `crates/trusty-mpm/Cargo.toml` declares
a single `[[bin]]` target (compiled under two names: `tm` and `trusty-mpm`) that
exposes all functionality via subcommands, and `crates/trusty-search/Cargo.toml`
declares both `trusty-search` and `trusty-embedderd` as `[[bin]]` targets of one
crate.

## Decision

We will keep each main user-facing tool as **one crate that bundles all of its
required binaries via `[[bin]]` shims** over a shared library, so a single
`cargo install --path crates/<name> --locked` puts the whole tool family
(CLI + daemon + sidecars) on `PATH` at one consistent version. Shared *library*
crates (e.g. `trusty-common`) are published to crates.io so external consumers
can depend on them; in-workspace consumers resolve them by path.

## Consequences

- **Positive:** one install command per tool; the daemon, CLI, TUI, and sidecars
  can never version-skew because they're built from the same crate.
- **Positive:** the macOS code-signing pitfall is avoided by always using
  `cargo install` (atomic rename), never `cp`.
- **Negative:** a single crate carries a complex binary target, so a change to
  any subcommand triggers a rebuild of the whole crate; large families (trusty-mpm)
  must guard against the 500-line file cap by splitting modules, not binaries.
- **Neutral:** crates are still versioned and tagged independently
  (`<crate>-v<version>`); bundling binaries does not change per-crate release
  granularity.
