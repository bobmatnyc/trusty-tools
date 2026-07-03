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
source code can link to it via the [Spec-Linked Documentation (SLD)][sld]
`# Spec References` rustdoc convention, and tooling (the intent-source resolver)
can resolve a changed file back to the section that governs it.

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

> **Catalog note — `DOC-28` self-label collision (uncataloged spec).** The file
> [`mpm-cutover-resume-native-optimization.md`](./mpm-cutover-resume-native-optimization.md)
> self-labels its header as **`DOC-28`**, which collides with the canonical DOC-28
> ([trusty-mpm Self-Awareness](./trusty-mpm-self-awareness.md), the entry above). That
> file is **not** in this catalog. The collision is flagged here rather than resolved by
> renumbering the file in-place (its self-label and any inbound references are left
> untouched); a follow-up should assign it the next free `DOC-N` (currently **DOC-33**
> — DOC-32 was claimed by [`tool-output-interception-seam.md`](./tool-output-interception-seam.md), issue #1953)
> and add a catalog row.

## Status lifecycle

A spec section's **Status** is one of `Draft → Accepted → Superseded`. A `~draft`
revision suffix marks a section that is not yet frozen; the suffix becomes a
version tag (`~v1`, `~v2`, …) when the section is accepted. Draft sections are
where most cross-crate behavior contracts begin — they are linked by code via
SLD even while `~draft` so traceability is established from the first
implementation PR.

## Spec-Linked Documentation (SLD)

Source code declares which spec section governs it using a module-level
`//! # Spec References` (or function-level `/// # Spec References`) rustdoc block
linking the spec ID to its anchor, e.g.:

```rust
//! # Spec References
//!
//! - [`SPEC-CONFORMANCE-03~draft`](docs/specs/intent-conformance.md#SPEC-CONFORMANCE-03~draft)
```

The intent-source resolver (`trusty_common::intent_source`) reads these blocks
to resolve a changed file back to the spec section that governs it. It is a
*reader* of declared links only — it never invents linkage where none is
declared, and it does **not** enforce a four-status traceability model (that is
an explicit non-goal, DOC-15 §1.3).

[sld]: #spec-linked-documentation-sld
