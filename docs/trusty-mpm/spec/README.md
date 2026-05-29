# trusty-mpm — Specification Set

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** code/docs/tickets audit

This directory holds the canonical product and engineering specification for the
`trusty-mpm` crate (`crates/trusty-mpm/`). It is the single authoritative
reference for *what trusty-mpm is meant to be*, *what it is today*, and *what
gaps remain*.

## What is trusty-mpm?

`trusty-mpm` is the **unified MPM platform** — a Rust reimplementation of the
Python `claude-mpm` meta-harness, packaged as **one install with five surfaces**.
A single resident daemon (`trusty-mpmd`) runs **one instance per machine** and
**coordinates every Claude Code session process on that host**: it spawns, tracks,
reaps, and observes sessions through an HTTP API + session registry, relays their
lifecycle hooks, and shapes their behaviour by deploying a PM-orchestration
framework (agents, skills, instructions, MCP wiring) into the project and
`~/.claude/`. The coordinated sessions are always **stock `claude` binaries** —
trusty-mpm never forks Claude Code; it shapes behaviour through deployed
artifacts and an appended system prompt.

The crate ships as a **single `cargo install trusty-mpm`** that produces one CLI
(`tm` / `trusty-mpm`) bundling — behind feature-gated `[[bin]]` targets — the
daemon (`trusty-mpmd`), an in-session MCP server, a ratatui TUI
(`trusty-mpm-tui`), and a Telegram bot (`trusty-mpm-telegram`). All five surfaces
share the `core` + `client` library modules so they never drift apart.

### Relationship to `open-mpm`

`trusty-mpm` and `open-mpm` are **independent crates** — there is no Cargo
dependency in either direction, and neither imports the other's library. They
are two distinct products that share a heritage and a vocabulary (PM, agents,
delegation, circuit breakers), not a codebase:

| | `open-mpm` | `trusty-mpm` |
|---|---|---|
| **What it is** | A Rust-native AI agent *orchestration engine* — an original coding harness with model-agnostic dispatch (OpenRouter / Anthropic / Bedrock / `claude` CLI) and an in-process CTRL→PM→sub-agent actor hierarchy over NDJSON IPC. | A *meta-harness around stock Claude Code* — a per-machine daemon that coordinates external `claude` session processes and deploys a PM framework into them. |
| **Runs the model** | Yes — dispatches tasks to any model via any backend itself. | No — every session is a stock `claude` process; model selection is advisory PM instruction (plus an optional LLM overseer/chat). |
| **Coordination unit** | In-process actors (sub-agents run in-process or as NDJSON subprocesses). | OS processes — multiple Claude Code sessions hosted in tmux panes / native terminals. |
| **Code link** | None. | None — only doc-comment lineage notes (the tmux helpers in `src/core/tmux.rs` / `src/daemon/tmux.rs` were modelled on open-mpm's `tm` module). |

In short: **open-mpm is the orchestration engine; trusty-mpm is the unified
product that wraps and coordinates Claude Code.** Both descend from the
`claude-mpm` product model but are built and shipped separately.

## Documents in this set

| Document | Read it when you want to know… |
|---|---|
| **[PRD.md](./PRD.md)** | The product: vision & mission (one install, five surfaces; one daemon, many sessions), goals/non-goals, the four personas, the full functional-requirement catalog grouped by surface + capability and tagged by implementation status, success criteria, and open questions. Start here for *why* and *what*. |
| **[ARCHITECTURE.md](./ARCHITECTURE.md)** | The system shape: the single-install multi-binary topology, how the binaries share the `core`/`client` modules, the single-daemon coordination model, the hook relay + enforcement point, MCP stdio framing (stdout reserved, logs to stderr), the daemon HTTP API surface, feature gating, and the filesystem layout. Start here for *how it fits together*. |
| **[COMPONENTS.md](./COMPONENTS.md)** | Per-binary and per-subsystem specs: the CLI, daemon, MCP server, TUI, Telegram bot, plus agent delegation, circuit breakers, session management, memory protection, instruction assembly, and agent/skill deployment. Each states responsibility, key types/modules (with `src/` paths), current state, and known gaps. Start here for *the detail of one subsystem*. |

## Reading order

1. **New to trusty-mpm?** PRD → ARCHITECTURE → COMPONENTS.
2. **Integrating a Claude Code host with the MCP surface?** ARCHITECTURE §MCP →
   COMPONENTS §MCP server.
3. **Implementing a gap-remediation ticket?** Jump to the relevant COMPONENTS
   section, cross-check the gap callout, then the ticket linked from PRD §Open
   Questions.

## Status legend (used throughout this set)

Every requirement and component is framed as **Vision / Current / Gap** and
tagged inline with one of:

| Tag | Meaning |
|---|---|
| ✅ **Implemented** | Built and working today. |
| 🟡 **Partial** | Partly built; usable but incomplete or with known caveats. |
| 🔵 **Designed-not-built** | Design exists (types, scaffolding, or asset) but no working path. |
| ⚪ **Aspirational** | North-star intent; no design committed yet. |

## Provenance & maintenance

These documents synthesise the two reconstructed research docs
([`../research/prd-2026-05-29.md`](../research/prd-2026-05-29.md),
[`../research/architecture-spec-2026-05-29.md`](../research/architecture-spec-2026-05-29.md),
[`../research/tm-services-discovery-spec-2026-05-28.md`](../research/tm-services-discovery-spec-2026-05-28.md))
cross-checked against the `crates/trusty-mpm/src/` tree, the crate manifest, the
embedded instruction assets, and the open/closed issue backlog — notably the
gap-remediation epic [#380](https://github.com/bobmatnyc/trusty-tools/issues/380)
and its children (#382–#395, #94). When the code changes materially, update the
relevant document and bump the *Last reviewed* date. Source-path citations
reflect the layout at the time of review.
