# Behavior-Contract Specs

This directory holds the **workspace-wide engineering specs** for trusty-tools.

## What is a spec?

A spec is a **behavior contract**: it states *what* a subsystem does — its
inputs, outputs, pre/postconditions, error behavior, and the rationale behind
the design — without prescribing the implementation. Specs are engineering-owned
and complement the product-facing PRDs: a PRD says *why* and *for whom*; a spec
says *what the code must do*.

Each spec carries a `DOC-N` document number and one or more stable spec IDs in
the `SPEC-{SUBSYSTEM}-{NN}~{rev}` grammar (e.g. `SPEC-CONFORMANCE-01~draft`).
Every governed section anchors its ID with a `{#SPEC-…}` heading marker so that
any source artifact — code in any language, config, or a Markdown document — can
link to it via the [Spec-Linked Documentation (SLD)][sld] `# Spec References`
convention (canonical: [DOC-38](./spec-linked-documentation.md)), and tooling (the
intent-source resolver) can resolve a changed file back to the section that governs it.

## Spec catalog

| DOC | Spec ID | Title | Subsystem |
|-----|---------|-------|-----------|
| DOC-13 | `SPEC-TUI-COORD-01~draft` | [Coordinator TUI (`tm coordinator`)](./tui-coordinator.md) | trusty-mpm — TUI / coordinator |
| DOC-14 | `SPEC-SM-AGENT-01~draft` | [Session Manager (SM) Agent](./session-manager-agent.md) | trusty-mpm — daemon / session-manager agent |
| DOC-15 | `SPEC-CONFORMANCE-01~draft` … `-03~draft` | [Intent / Method Conformance](./intent-conformance.md) | cross-crate — trusty-mpm + trusty-review + trusty-common |
| DOC-16 | `SPEC-SM-TUI-01~draft` | [Interactive Sessions TUI (`tm sessions tui`)](./sessions-tui-interactive.md) | trusty-mpm — TUI / sessions |
| DOC-17 | `SPEC-HARNESS-01~draft` | [Autonomous Multi-Session Managed Harness Runner](./harness-runner-vision.md) | trusty-mpm — harness runner / provisioning |
| DOC-18 | `SPEC-METACODING-01~draft` | [Metacoding — the trusty-tools product north-star](./metacoding-vision.md) | trusty-tools — product vision (cross-crate) |
| DOC-19 | `SPEC-TELUI-01~draft` | [TELUI: the Telegram UI for trusty-mpm](./telui-telegram-ui.md) | trusty-mpm — control surface / Telegram |
| DOC-20 | `SPEC-CHAT-CORE-01~draft` | [Chat-Core: the shared command nucleus](./chat-core.md) | trusty-mpm — control surface / shared client |
| DOC-21 | `SPEC-HARNESS-UNDERSTANDING-01~draft` | [Harness Understanding](./harness-understanding.md) | trusty-agents-common + trusty-mpm |
| DOC-22 | `SPEC-MULTIREPO-01~draft` | [Multi-Repo Session Routing](./multi-repo-session-routing.md) | trusty-mpm — session-manager / routing |
| DOC-23 | `SPEC-AUTONOMY-AUTO-01~draft` … `-08~draft` | [Learned-Autonomy Auto-Answer](./learned-autonomy-auto-answer.md) | trusty-mpm — decision-adjudication / autonomy |
| DOC-24 | `SPEC-STANDALONE-MPM-01~draft` … `-08~draft` | [Standalone Managed `trusty-mpm` Driver](./standalone-managed-trusty-mpm.md) | trusty-mpm — standalone driver / managed config |
| DOC-25 | `SPEC-VOICE-01~draft` … `-08~draft` | [trusty-voice — Streaming Voice Interface to the Coding Agent](./trusty-voice.md) | trusty-voice (new) — voice client / trusty-mpm chat surface |
| DOC-26 | `SPEC-SESSCTL-01~draft` | [trusty-mpm alpha-1 unified project/session control plane](./trusty-mpm-alpha-1-control-plane.md) | trusty-mpm — control plane / session manager |
| DOC-27 | `SPEC-MCPSVC-01~draft` … `-07~draft` | [trusty-mcp-service — Unified Native Sidecar Services via MCP-A](./SPEC-MCPSVC-01-trusty-mcp-service.md) | trusty-mcp-service (new) — gworkspace / slack / telegram domains / MCP-A gateway |
| DOC-28 | `SPEC-SELFAWARE-01~draft` … `-04~draft` | [trusty-mpm Self-Awareness and Instruction-Load Verification](./trusty-mpm-self-awareness.md) | trusty-mpm — identity / instruction pipeline; trusty-memory — prompt-facts |
| DOC-29 | `SPEC-MPM-BEHAVIOR-01~draft` … `-06~draft` | [Primary trusty-mpm Harness Behaviors — Conformance Matrix](./mpm-behavior-conformance.md) | trusty-mpm — behavior conformance / cross-spec verification |
| DOC-30 | `SPEC-PM-01~draft` | [Project Manager: Vision & Lifecycle Orchestrator](./DOC-30-project-manager-vision.md) | trusty-mpm — project-level orchestration / user-facing surface |
| DOC-31 | `SPEC-PROVISION-01~draft` … `-08~draft` | [SYSTEM vs PROJECT Agents & Skills — Provisioning, In-Project Migration, Requirement-Driven Pulls](./system-project-agents-skills.md) | trusty-mpm — content provisioning / agent + skill deploy pipeline |
| DOC-32 | `SPEC-TOOLPROXY-01~draft` | [Live Tool-Output Interception Seam for Native `tm` Sessions](./tool-output-interception-seam.md) | trusty-mpm / trusty-agents — MCP tool-output proxy / live token compression |
| DOC-33 | `SPEC-METALOG-01~draft` … `-04~draft` | [tm Meta-Harness Logging — Per-Delegation Observability, Verbosity CLI, and Log Pruning](./tm-meta-harness-logging.md) | trusty-mpm — observability / CLI / log retention |
| DOC-35 | `SPEC-PROJCTL-01~draft` … `-08~draft` | [`tm project`: Deterministic Project/Session Control Plane (CLI + Multipane TUI)](./tm-project-control-plane.md) | trusty-mpm — control plane / CLI / TUI / daemon API |
| DOC-36 | `SPEC-TMMGR-01~approved` … `-06~approved` | [`tm manager`: Layer-3 Chat-Based Portfolio Project Manager](./tm-manager-vision.md) | trusty-mpm — daemon / inference layer / external channels |
| DOC-38 | `SPEC-SLD-01~draft` … `-05~draft` | [Spec-Linked Documentation (SLD): Language-Agnostic Source↔Spec Linkage](./spec-linked-documentation.md) | cross-crate — `docs/specs` conventions + trusty-common `intent_source` (all languages) |

> **Catalog note — `DOC-34` gap.** `DOC-34` (`SPEC-CFGDIR-01~draft`…`-05~draft`,
> [Managed sessions launch with a tm-owned `CLAUDE_CONFIG_DIR`](./managed-session-config-dir.md))
> was assigned (#1999) but not yet added as a catalog row; noted here rather than fixed in this
> PR to keep this change scoped to DOC-35.

> **Catalog note — `DOC-28` self-label collision (uncataloged spec).** The file
> [`mpm-cutover-resume-native-optimization.md`](./mpm-cutover-resume-native-optimization.md)
> self-labels its header as **`DOC-28`**, which collides with the canonical DOC-28
> ([trusty-mpm Self-Awareness](./trusty-mpm-self-awareness.md), the entry above). That
> file is **not** in this catalog. The collision is flagged here rather than resolved by
> renumbering the file in-place (its self-label and any inbound references are left
> untouched); a follow-up should assign it the next free `DOC-N` and add a catalog row.
> **Next free `DOC-N` = `DOC-39`** (scan of the whole `docs/` tree, 2026-07-16): the
> highest claimed number is **DOC-38** ([Spec-Linked Documentation](./spec-linked-documentation.md),
> the entry above). DOC-34 is assigned ([`managed-session-config-dir.md`](./managed-session-config-dir.md),
> #1999 — still a catalog gap), DOC-35/36/38 are cataloged, and **DOC-37** is
> self-labeled by [`trusty-search-managed-repo-awareness.md`](./trusty-search-managed-repo-awareness.md)
> (`SPEC-SEARCHREPO-01~draft`…, uncataloged). The DOC-N assignment rule (scan-before-claim)
> and collision handling are now normative in [DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft).

## Status lifecycle

A spec section's **Status** is one of `Draft → Accepted → Superseded`. A `~draft`
revision suffix marks a section that is not yet frozen; the suffix becomes a
version tag (`~v1`, `~v2`, …) when the section is accepted. Draft sections are
where most cross-crate behavior contracts begin — they are linked by code via
SLD even while `~draft` so traceability is established from the first
implementation PR.

## Spec-Linked Documentation (SLD)

**Canonical spec: [DOC-38 — Spec-Linked Documentation](./spec-linked-documentation.md).**
SLD is the convention by which a **source artifact declares which spec section
governs it**, so the intent-source resolver (`trusty_common::intent_source`) can
resolve a changed file back to that section. As of DOC-38, SLD is **language-agnostic**:
Rust, Python, TypeScript/JavaScript, shell, TOML/YAML, and **Markdown** documents all
declare linkage via a `# Spec References` block in their native comment/docstring idiom,
using one canonical `(spec-ID, relative-path, anchor)` reference grammar.

The Rust form is unchanged — a module-level `//! # Spec References` (or function-level
`/// # Spec References`) rustdoc block linking the spec ID to its anchor:

```rust
//! # Spec References
//!
//! - [`SPEC-CONFORMANCE-03~draft`](docs/specs/intent-conformance.md#SPEC-CONFORMANCE-03~draft)
```

Markdown documents declare linkage via a **visible `## Spec References` section**
(DOC-38 §3.6); the other languages use their native comment/docstring idioms
(DOC-38 §3). The resolver is a *reader* of declared links only — it never invents
linkage where none is declared, and it does **not** enforce a four-status
traceability model (an explicit non-goal, DOC-15 §1.3 / DOC-38 §1.3). See DOC-38
for the normative reference grammar, per-language idioms, and the resolver contract.

[sld]: #spec-linked-documentation-sld
