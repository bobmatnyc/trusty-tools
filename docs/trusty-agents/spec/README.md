# open-mpm — Specification Set

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** research synthesis + code/docs/tickets audit

This directory holds the canonical product and engineering specification for the
`open-mpm` crate (`crates/open-mpm/`). It is the single authoritative reference
for *what open-mpm is meant to be*, *what it is today*, and *what gaps remain*.

## What is open-mpm?

`open-mpm` is a Rust-native AI agent orchestration harness. Its product north
star is to be a **superset of both Warp and Claude Code**: a persistent,
multi-project coordination layer ("agentic assistant manager") *and* an original
coding harness that can dispatch any task to **any model via any backend**.
A long-running **CTRL** controller coordinates per-project **PM** (project
manager) actors; each PM delegates work to specialized **sub-agents** that run
either in-process (fast, read-only) or as isolated OS subprocesses (file/shell
agents) communicating over NDJSON IPC. The defining differentiator is
**model-agnostic dispatch**: any agent role can be backed by OpenRouter (500+
models), the direct Anthropic API, AWS Bedrock, or the `claude` CLI OAuth path,
assignable per-agent via a two-line TOML change with no code modifications.

## Documents in this set

| Document | Read it when you want to know… |
|---|---|
| **[PRD.md](./PRD.md)** | The product: vision & mission, goals/non-goals, personas, the full functional-requirement catalog (tagged by implementation status), success criteria, and the open-question roadmap. Start here for *why* and *what*. |
| **[ARCHITECTURE.md](./ARCHITECTURE.md)** | The system shape: process model + topology diagram, NDJSON IPC message formats, the `Event` enum and its subscribers, the source-module map, and a thorough treatment of **model-agnostic dispatch** (credential priority, per-agent TOML, the three runner kinds, four LLM backend paths, session override, model qualification, thinking mode). Start here for *how it fits together*. |
| **[COMPONENTS.md](./COMPONENTS.md)** | Per-subsystem specs: PM/CTRL orchestration, the sub-agent subprocess model, tool-using agents, skill injection, the workflow engine, token compression, memory & search, global infrastructure, and the three UI surfaces. Each component states responsibility, key types/modules (with `src/` paths), current state, and known gaps. Start here for *the detail of one subsystem*. |

## Reading order

1. **New to open-mpm?** PRD → ARCHITECTURE → COMPONENTS.
2. **Implementing a feature?** Jump to the relevant COMPONENTS section, then
   cross-check the dispatch model in ARCHITECTURE.
3. **Evaluating product direction?** PRD vision + success criteria, then the
   gap callouts throughout COMPONENTS.

## Status legend (used throughout this set)

Every requirement and component is framed as **Vision / Current / Gap** and
tagged inline with one of:

| Tag | Meaning |
|---|---|
| ✅ **Implemented** | Built and working today. |
| 🟡 **Partial** | Partly built; usable but incomplete or with known caveats. |
| 🔵 **Designed-not-built** | Design exists (types, scaffolding, or plan) but no working path. |
| ⚪ **Aspirational** | North-star intent; no design committed yet. |

## Provenance & maintenance

These are derived from a research synthesis cross-checked against the
`crates/open-mpm/src/` tree, crate docs, and the open/closed issue backlog
(notably the 500-line-cap refactor sweep, #356 and #358–#366, and the three
remaining oversized-file tickets #170/#171/#172). When the code changes
materially, update the relevant document and bump the *Last reviewed* date.
Source-path citations reflect the layout at the time of review; some files
cited as single `.rs` modules in the originating research have since been split
into same-named subdirectories during the cap sweep.
