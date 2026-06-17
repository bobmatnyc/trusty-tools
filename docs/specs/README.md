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
