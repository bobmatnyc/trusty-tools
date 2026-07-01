# What Is trusty-mpm?

trusty-mpm is a **Rust** crate at `crates/trusty-mpm/`, distributed as a single
binary — **`tm`** (also installable as `trusty-mpm`) — whose subcommands
provide the background daemon, the CLI, the TUI dashboard, and the Telegram
bot surfaces. It is the **Meta-Harness** / control plane described in
[Three-Harness Architecture](../../../docs/architecture/harnesses.md): it
manages multi-project Claude Code sessions, relays lifecycle hooks, and
exposes an MCP server — it does **not** execute coding work itself, it
delegates all coding tasks to **`trusty-code`** (`tcode`). It is **not** the
Python `claude-mpm` package: no code relation, different language (Rust vs.
Python), different maintainers, and a different distribution channel
(crates.io/Homebrew vs. PyPI).

This document exists because a Claude Code session asked "are you self-aware
of the framework?" and answered incorrectly — it shell-probed
(`pip3 show claude-mpm`, `which claude-mpm`) instead of consulting memory or a
canonical doc, and it conflated this Rust project with the unrelated Python
one. See [DOC-28 — trusty-mpm Self-Awareness](../../../docs/specs/trusty-mpm-self-awareness.md)
for the full incident writeup and the behavior contract this doc is part of.
**This is the single, stable, canonically-pointed-at answer to "what is this
framework" — consult it (and memory) instead of shell-probing.**

## The one-paragraph answer

If you are a Claude Code session running under trusty-mpm and someone asks
what framework/tool/system this is: you are working inside a project managed
by **trusty-mpm** (binary `tm`), a **Rust** Meta-Harness / control-plane
daemon that launches, observes, and coordinates Claude Code sessions across
one or more projects. trusty-mpm itself never writes code — every unit of
real implementation work is delegated to a launched session, which in turn
runs under **`trusty-code`** (`tcode`), the per-project coding harness. If the
question could plausibly be about the Python **`claude-mpm`** project: that is
a *different, unrelated* tool — different repository, different language,
different maintainers — and this doc, this session, and this repo have
nothing to do with it.

## What trusty-mpm is (and is not)

| | trusty-mpm (this project) | Python `claude-mpm` |
|---|---|---|
| Language | Rust | Python |
| Binary / package | `tm` / `trusty-mpm` | `claude-mpm` |
| Distribution | crates.io, Homebrew, GitHub Releases | PyPI |
| Role | Meta-Harness / control plane (DOC-26) | unrelated Claude Code agent-fleet + output-style layer |
| Repository | `trusty-tools` (this repo) | a separate, unrelated repository |
| Relationship to this repo | **is** this repo's `crates/trusty-mpm/` | **none** |

trusty-mpm:

- Manages multi-project sessions (`tm sessions`, `tm run`, `tm load`) and
  their lifecycle (spawn, observe, decommission).
- Exposes an MCP server (`mcp__trusty-mpm__*` tools) that Claude Code sessions
  and other trusty-* tools call into.
- Assembles and deploys the PM/SM system prompts, agents, skills, and output
  styles that a launched session runs under.
- Delegates **all** coding work — edits, builds, tests, research — to a
  launched session running `trusty-code` (`tcode`); it has no "hands" of its
  own.

See [Three-Harness Architecture](../../../docs/architecture/harnesses.md) for
the full three-harness (trusty-code / trusty-mpm / trusty-agents) delegation
graph, and
[DOC-26 — trusty-mpm alpha-1 control plane](../../../docs/specs/trusty-mpm-alpha-1-control-plane.md)
for the control-plane wire protocol and session lifecycle contract. This doc
deliberately summarizes rather than duplicates those two references — treat
them as the deeper architectural and protocol sources of truth.

## How to answer an identity question (for a running session)

Per the
[Identity & Self-Awareness Protocol](../../../docs/specs/trusty-mpm-self-awareness.md#4-r2--identityself-awareness-protocol-in-instruction-assets-specselfaware-02draft)
(bundled into `BASE_SM.md` and every output style — see that spec for the
exact wording):

1. **Consult memory first** — call `get_prompt_context()` / `memory_recall`
   against the active trusty-memory palace before answering.
2. **Then consult this doc** — read it from
   `~/.trusty-mpm/framework/docs/WHAT-IS-TRUSTY-MPM.md` (deployed by
   `tm install`), or, inside the trusty-tools repo itself, from
   `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md` via `trusty-search` or a
   direct file read.
3. **Never shell-probe for identity** — `pip3 show`, `pip show`,
   `which claude-mpm`, or grepping `site-packages`/`dist-info` interrogate the
   wrong (Python) ecosystem and cannot see this Rust binary at all. These are
   forbidden ways to answer an identity question.
4. **State the disambiguation explicitly when relevant** — this is
   `trusty-mpm` (binary `tm`), NOT `claude-mpm`.

## Memory seeding (R3 — manual step)

`get_prompt_context()` surfaces `is_fact` triples from every registered
trusty-memory palace (see `crates/trusty-memory/src/prompt_facts.rs`), but no
identity fact exists until one is seeded. Run this once per trusty-memory
install/upgrade (any palace — the fact is visible cross-palace):

```
kg_assert(
  palace: "<any palace, e.g. trusty-tools or session-manager>",
  subject: "trusty-mpm",
  predicate: "is_fact",
  object: "trusty-mpm (binary tm) is the Rust Meta-Harness / control plane, NOT the Python claude-mpm project; see crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md or ~/.trusty-mpm/framework/docs/WHAT-IS-TRUSTY-MPM.md",
  provenance: "DOC-28 self-awareness seed"
)
```

Via the MCP tool surface this is `mcp__trusty-memory__kg_assert` with the same
arguments. Re-running it is safe (idempotent in effect — it re-asserts the
same triple). An automatic, idempotent seed call from `prepare_session` is
deferred future work (DOC-28 §7 Phase 2) — until it lands, this manual step is
what makes the fact appear in `get_prompt_context()`'s "### Facts" section.

## Verifying the instructions actually loaded

`tm doctor` includes an `output_style` check: it reads the effective
`outputStyle` value (project `.claude/settings.json` if present, else the
global one) and confirms it resolves to a real, on-disk trusty-mpm style file
under `~/.claude/output-styles/`. A `Fail` here (unknown/stale style id, e.g.
a leftover `claude_mpm` value) or a missing file means the session is **not**
running under trusty-mpm's instructions at all — run `tm run`/`tm load` to
rewrite the setting correctly, or fix it by hand. See DOC-28 §6 for the full
detection contract and its limitations.
