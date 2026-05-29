# 0001 — Gate axum/tower-http behind an `http-server` cargo feature

> **Status:** Accepted
> **Date:** 2026-05-26
> **Decided in:** issue [#249](https://github.com/bobmatnyc/trusty-tools/issues/249), PR [#262](https://github.com/bobmatnyc/trusty-tools/pull/262)
> **Last reviewed:** 2026-05-29

## Context

`trusty-analyze` ships a single crate that is simultaneously a binary (HTTP
daemon + MCP HTTP/SSE), a stdio MCP server, and a library of analysis
primitives. axum + tower-http are heavyweight HTTP-server-only dependencies. A
downstream consumer that only wants the analysis core, the wire types, or the
pure JSON-RPC dispatcher should not be forced to compile the entire axum + tower
stack — the same concern already solved in `trusty-common` (`axum-server`
feature) and `trusty-memory`.

## Decision

Introduce an `http-server` cargo feature (default-on) that gates:

- `dep:axum` and `dep:tower-http`,
- the `trusty-common/axum-server` feature,
- the `service` module (axum HTTP daemon), and
- the `mcp::sse` HTTP/SSE transport.

The `trusty-analyze` binary declares `required-features = ["http-server"]`. The
stdio MCP transport (`mcp::stdio`) stays **unconditional**, since Claude Code and
similar clients spawn the dispatcher as a subprocess and only need stdio.
Library consumers opt out of the HTTP stack with `--no-default-features`.

## Consequences

- **Positive:** `cargo install trusty-analyze`, `cargo test -p trusty-analyze`,
  and the default daemon build are unchanged (feature is default-on). Library
  consumers can drop axum entirely. The rule is consistent across the workspace
  (`trusty-common`, `trusty-memory`, `trusty-analyze`).
- **Negative:** module visibility now depends on a feature flag — `service` and
  `mcp::sse` only exist under `http-server`, so any code referencing them must be
  similarly gated (see the `#[cfg(feature = "http-server")]` annotations in
  `src/lib.rs` and `src/mcp/mod.rs`).

## Evidence

- `crates/trusty-analyze/Cargo.toml` — `[features]` block with the
  `http-server` definition and the `[[bin]]` `required-features` entry, both
  carrying `# Why (issue #249)` comments.
- `crates/trusty-analyze/src/lib.rs` — `#[cfg(feature = "http-server")] pub mod service;`.
- `crates/trusty-analyze/src/mcp/mod.rs` — `#[cfg(feature = "http-server")] pub mod sse;`.
- Commit `48545cc` "fix(trusty-analyze,trusty-embedderd): gate axum behind
  feature flags (closes #249 #250) (#262)".
