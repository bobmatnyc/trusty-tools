# 0014. Ship full native MCP support (ticketing, gworkspace, Slack/Telegram, and more)

- **Status:** Accepted
- **Date:** 2026-07-14
- **Scope:** Workspace-wide
- **Supersedes / Superseded by:** — (none)

## Context

trusty-tools already ships a shared MCP framework in `trusty-common`'s `mcp`
feature (`crates/trusty-common/src/mcp/`: `service.rs`, `openrpc.rs`,
`daemon_bridge.rs`, `memory_rpc.rs`, `mod.rs`) — JSON-RPC 2.0 types, the
`run_stdio_loop`, the `initialize` handshake, and OpenRPC discovery via
`ServiceDescriptor`. This framework is the absorbed successor of the
standalone `trusty-mcp-core` crate (commit `a90ba071`). Eight in-workspace
crates already consume it as an MCP server or client surface: trusty-memory,
trusty-search, trusty-review, trusty-mpm (`tm mcp`), trusty-agents,
trusty-analyze, trusty-console, and trusty-code.

`crates/trusty-gworkspace` (v0.1.3) is a dormant, 43-tool native Rust port of
the Python `gworkspace-mcp` server (116 tools; the Rust port consolidates CRUD
operations into action-enum tools). It is token-file-compatible with the
Python implementation (`~/.gworkspace-mcp/tokens.json`). Known gaps: the
interactive OAuth consent flow is still Python-only, there is no binary
transfer support, and Slides/Sheets coverage is partial. Migrating these gaps
is out of scope for this ADR and proceeds in a separate session.

Per-server tool registration already follows a consistent convention (a
`tools/list` builder plus a `match`-based dispatch per server, documented in
the trusty-gworkspace README), and `ServiceDescriptor` exists to merge
multiple services behind one host process. The three-layer communication
model (DOC-22/DOC-36) already routes Slack/Telegram/MCP channels through the
`SessionProxy` natively, so channel integrations have a native home to land
in rather than needing a new abstraction.

Relevant forces:

- **Single-language toolchain.** The workspace is Rust-only (MSRV 1.91);
  every daemon, MCP server, and control-plane component builds from the same
  Cargo workspace with shared conventions (`thiserror`/`anyhow` split, no
  stdout logging in daemons, `Why/What/Test` doc pattern).
- **Single source of truth.** trusty-tools consolidated seven formerly
  separate repos specifically to eliminate divergent implementations of the
  same capability (see project CLAUDE.md, "Role & Scope").
- **macOS install/signing/cdhash story.** The workspace has an established,
  hard-won install and code-signing story (`cargo install`, Developer-ID
  signing, cdhash-keyed Full Disk Access — issue #873). Every additional
  runtime (e.g. a Python interpreter and its own dependency/venv management)
  multiplies that surface and reintroduces install failure modes the Rust
  toolchain has already solved.
- **Avoiding Python runtime dependencies for daily tooling.** Third-party MCP
  servers for ticketing, Google Workspace, and chat platforms are
  predominantly Python (`pip`/`uv`/venv), which conflicts with the
  single-binary, `cargo install`-driven distribution model the rest of the
  stack relies on.
- **Consistency of error-handling/logging conventions.** In-workspace crates
  already follow uniform conventions (structured `thiserror` errors, stderr
  logging, OpenRPC-discoverable tool contracts). External MCP servers bring
  their own inconsistent conventions and no shared observability story.
- **Competitive positioning.** Shipping first-party MCP coverage for the
  product's key surfaces (ticketing, Google Workspace, chat channels) is a
  differentiator versus assembling a stack of third-party MCP servers with
  uneven quality and maintenance.

## Decision

We ship full native MCP support. New MCP-exposed product surfaces —
ticketing, Google Workspace (`trusty-gworkspace`), and Slack/Telegram channel
integrations — are built in-workspace, in Rust, on the shared `trusty-common`
`mcp` feature, following the same tool-registration convention
(`tools/list` builder + `match` dispatch, `ServiceDescriptor` for multi-service
hosting) already used by trusty-memory, trusty-search, trusty-review, tm,
trusty-agents, trusty-analyze, trusty-console, and trusty-code.

External, third-party, or non-Rust MCP servers (including the Python
`gworkspace-mcp` that `trusty-gworkspace` was ported from) are treated as
**transitional only** — acceptable as a stopgap while a native port is
incomplete (e.g. `trusty-gworkspace`'s OAuth consent flow), never as the
long-term integration point for a key product surface.

## Consequences

**Easier / positive:**

- **One install story.** No additional language runtimes, package managers,
  or venvs; every MCP server ships through the same `cargo install` /
  Developer-ID-signing pipeline already documented for the rest of the stack.
- **Shared conventions.** All MCP servers get the same JSON-RPC handling,
  OpenRPC discovery, error typing (`thiserror`), and stderr-only logging for
  free from `trusty-common::mcp`.
- **Centralized token/secret handling.** Credential and token-file management
  (e.g. `~/.gworkspace-mcp/tokens.json`) lives in one place instead of being
  reimplemented per third-party server.
- **Testability.** In-workspace Rust servers get `cargo test -p <crate>`
  coverage and workspace-wide `cargo check`/`clippy` gates; third-party
  servers have no equivalent guarantee.

**Harder / negative (honest trade-offs):**

- **API-coverage maintenance lands in-house.** Each native port (ticketing,
  gworkspace, Slack/Telegram) now owns tracking upstream API changes that a
  third-party maintainer previously absorbed.
- **OAuth consent flow must be ported.** `trusty-gworkspace`'s interactive
  OAuth consent is still Python-only; closing this gap is required work,
  tracked separately from this ADR.
- **Hand-written match dispatch per server.** The `tools/list` +  `match`
  convention scales linearly with tool count per server (e.g. 43 tools in
  trusty-gworkspace); there is no macro/codegen layer yet to reduce
  boilerplate.
- **Feature lag versus mature third-party servers during migration.** Known
  gaps (binary transfer, partial Slides/Sheets coverage in
  `trusty-gworkspace`) persist until the native ports catch up; teams
  depending on full third-party feature parity may need to wait.

## References

- `crates/trusty-common/src/mcp/` — shared MCP framework (`service.rs`,
  `openrpc.rs`, `daemon_bridge.rs`, `memory_rpc.rs`, `mod.rs`)
- `crates/trusty-gworkspace/` (v0.1.3) — native Rust port of `gworkspace-mcp`
- DOC-22 / DOC-36 — three-layer communication model routing Slack/Telegram/MCP
  through `SessionProxy`
- Commit `a90ba071` — absorption of `trusty-mcp-core` into `trusty-common`
