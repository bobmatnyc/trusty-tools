# 0003. MSRV 1.88 and per-crate Rust edition policy

- **Status:** Accepted
- **Date:** 2026-05-29
- **Scope:** Workspace-wide
- **Supersedes / Superseded by:** —

## Context

The workspace mixes crates that need cutting-edge language features with crates
that do not. The `trusty-mpm` and `open-mpm` families rely on **let-chains**
(`if let … && let …`), which are only available in **edition 2024**. Edition
2024 in turn requires a recent compiler; the let-chain stabilization the family
depends on lands at Rust **1.88**.

Pinning every crate to edition 2024 would force an unnecessarily high baseline on
library crates that compile fine on edition 2021, and would risk copying
let-chain patterns into crates that can't support them. Conversely, allowing
arbitrary MSRV per crate would make CI and contributor setup unpredictable. The
workspace `[workspace.package]` table already shares `rust-version`, `edition`
(default), `license`, `repository`, and `authors`, while each crate keeps its own
`version` (per #343).

## Decision

We will set the workspace **MSRV to `1.88`** (shared via
`[workspace.package].rust-version`) and apply a **per-crate edition policy**:

- `edition = "2024"` for the let-chain-using crates: the `trusty-mpm` family
  (`trusty-mpm`, `trusty-mpm-gui`) and the `open-mpm` family (`open-mpm`,
  `open-mpm-agent-api`, `open-mpm-local`).
- `edition = "2021"` for all other crates.

Contributors must check a crate's `Cargo.toml` before assuming its edition, and
must not copy let-chain syntax into edition-2021 crates. Prefer stable-channel
toolchains to avoid MSRV drift from picking up newer nightly syntax that fails
on CI.

## Consequences

- **Positive:** a single, predictable MSRV for CI and local setup; edition-2021
  library crates keep the lowest viable baseline while the agent-harness crates
  get the ergonomics of let-chains.
- **Negative:** the workspace is split across two editions, so contributors must
  be edition-aware; an edition-2021 crate that wants let-chains must first be
  migrated to edition 2024 (and re-validated against MSRV 1.88).
- **Neutral:** raising the MSRV in future is itself an architecturally
  significant change and should be recorded as a superseding ADR.
