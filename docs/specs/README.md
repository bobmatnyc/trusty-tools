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
link to it via the [Spec-Linked Documentation (SLD)][sld] standard (canonical:
[DOC-38](./spec-linked-documentation.md)), and a conforming resolver (e.g.
trusty-common's `intent_source`) can resolve a changed file back to the section
that governs it.

## Policy

**New documentation in this repository follows [DOC-38 — Spec-Linked
Documentation (SLD)](./spec-linked-documentation.md).** New specs use scan-before-claim
`DOC-N` numbering, the bold-field header block, and `{#SPEC-…}` anchors (DOC-38 §4);
a source artifact that links to a spec declares it in its native idiom — `spec_refs:`
YAML frontmatter for Markdown (§2.5), a `# Spec References` comment/docstring block for
code (§3). The `sld-lint` gate (`scripts/check_sld.sh`, run in CI and as a pre-commit
hook) enforces that every declared reference resolves and that opted-in specs conform;
existing specs are grandfathered while the retrofit lands (§10 F5/F6). See DOC-38 for the
normative grammar — this note does not restate it.

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
| DOC-38 | `SPEC-SLD-01~draft` … `-03~draft` | [Spec-Linked Documentation (SLD): A Language-Agnostic Source↔Spec Reference Standard](./spec-linked-documentation.md) | documentation standard — repository/language-agnostic (informative reference: trusty-common `intent_source`) |
| DOC-39 | `SPEC-TCUI-01~draft` … `-09~draft` | [trusty-code Harness UI: Context-First Interactive Surface](./trusty-code-harness-ui.md) | trusty-code — API surface (JSON-RPC + events); SPA (web/Tauri) client downstream |
| DOC-40 | `SPEC-BGATTACH-01~draft` … `-07~draft` | [Durable Background Agents: Exclusive Attach/Detach Semantics](./durable-background-agents.md) | trusty-mpm — daemon / session-manager / agent delegation; trusty-code — session registry / task executor (cross-crate) |
| DOC-41 | `SPEC-AGENTFW-01~draft` … `-06~draft` | [Eve-Style Agent Framework for trusty-agents](./trusty-agents-eve-style-agents-spec.md) | trusty-agents — agent definition / runtime / tool-calling / memory |
| DOC-46 | `SPEC-ADR-01~draft` | [Architecture Decision Records (ADR) as First-Class Documentation Artifact](./DOC-46-adr-standard.md) | documentation standard — architecture governance / consistency vetting (cross-crate) |
| DOC-47 | `SPEC-EVTING-01~draft` … `-04~draft` | [External Event Ingestion — Webhooks & Connector Push](./DOC-47-external-event-ingestion.md) | trusty-agents-common — event seam; trusty-mpm — webhook ingress + goal store; trusty-console — Tailscale Funnel binding |
| DOC-48 | `SPEC-WS-01~draft` … `-09~draft` | [tcode Workstreams: Durable Named Work Aggregation](./DOC-48-tcode-workstreams.md) | trusty-code — activation-lock exclusivity model, multi-client attach transport (shared with trusty-agents #3052), RPC/REST/CLI surfaces |
| DOC-50 | `SPEC-TTUI-01~draft` … `-09~draft` | [trusty-code Interactive TUI: Claude Code Clone over Shared REPL Layer](./DOC-50-tcode-tui-claude-code-clone.md) | trusty-code — interactive terminal UI thin client; trusty-tui shared crate (ratatui REPL extraction from trusty-agents) |
| DOC-51 | `SPEC-TCPLUGIN-01~draft` | [trusty-code Claude Code Plugin Support, Phase 1: Local-Directory Agents + Skills](./DOC-51-tcode-plugin-support-phase1.md) | trusty-code — agent/skill catalog, discovery, dispatch |
| DOC-52 | `SPEC-SHAREDWS-01~draft` … `-04~draft` | [Shared Workstream Definition: Cross-Harness Session Binding and Resource Governance](./DOC-52-shared-workstream-definition.md) | trusty-mpm, trusty-code, trusty-agents — unified workstream semantics, 1:1 session binding, resource governance (caps, scope-overlap, reclamation) |

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
> **Next free `DOC-N` = `DOC-53`** (updated 2026-07-22 — DOC-52 claimed by
> [Shared Workstream Definition](./DOC-52-shared-workstream-definition.md)):
> the highest cataloged number is now **DOC-52**, which claimed the next free number after
> **DOC-51** ([trusty-code Claude Code Plugin Support Phase 1](./DOC-51-tcode-plugin-support-phase1.md))
> per the scan-before-claim rule ([DOC-38 §4.1](./spec-linked-documentation.md) — a catalog's "next free"
> note is a *hint, not authority*; the scan is authoritative). DOC-44/45 are claimed by the
> unmerged `spec-twin-lead-architecture` branch ([DOC-44 Engineering Lead Twin Orchestration](https://github.com/bobmatnyc/trusty-tools/tree/spec-twin-lead-architecture));
> DOC-34 is assigned ([`managed-session-config-dir.md`](./managed-session-config-dir.md),
> #1999 — still a catalog gap), DOC-35/36/38/39/40/41/46/47/48/50/51/52 are cataloged (DOC-49 was pre-claimed by PR #3313), and **DOC-37** is
> self-labeled by [`trusty-search-managed-repo-awareness.md`](./trusty-search-managed-repo-awareness.md)
> (`SPEC-SEARCHREPO-01~draft`…, uncataloged). What was open PR #2792 (Eve-style agent framework
> for trusty-agents) previously also self-labeled `DOC-37` for this unrelated spec — that
> collision is now resolved: PR #2792 renumbered to **DOC-41** per the
> scan-before-claim check, and `DOC-37` remains solely claimed by
> `trusty-search-managed-repo-awareness.md`. The DOC-N assignment rule (scan-before-claim)
> and collision handling are now normative in [DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft)
> (note: `{#SPEC-…}` cross-links are best-effort on github.com — GitHub does not
> honor explicit heading IDs, DOC-38 §4.3 — so this link lands on the file; scan
> for §4.1 from there).

## Status lifecycle

A spec section's **Status** is one of `Draft → Accepted → Superseded`. A `~draft`
revision suffix marks a section that is not yet frozen; the suffix becomes a
version tag (`~v1`, `~v2`, …) when the section is accepted. Draft sections are
where most cross-crate behavior contracts begin — they are linked by code via
SLD even while `~draft` so traceability is established from the first
implementation PR.

## Spec-Linked Documentation (SLD)

**Canonical spec: [DOC-38 — Spec-Linked Documentation](./spec-linked-documentation.md).**
SLD is a **pure, implementation-neutral documentation standard** — usable in any
repository, in any language — by which a **source artifact declares which spec
section governs it**, so a *conforming resolver* (any tool that reads the
declaration) can trace a changed file back to that section. DOC-38 defines the
standard itself: the grammar, not a resolver implementation. As of DOC-38, SLD is
**language-agnostic**: Rust, Python, TypeScript/JavaScript, shell, and TOML/YAML
declare linkage inline via a `# Spec References` comment/docstring block in their
native idiom, using one canonical `(spec-ID, relative-path, anchor)` reference
grammar; **Markdown documents declare linkage canonically in `spec_refs:` YAML
frontmatter** (DOC-38 §2.5, §3.6), with an optional visible section retained for
human readability only.

The Rust form is unchanged — a module-level `//! # Spec References` (or function-level
`/// # Spec References`) rustdoc block linking the spec ID to its anchor:

```rust
//! # Spec References
//!
//! - [`SPEC-CONFORMANCE-03~draft`](docs/specs/intent-conformance.md#SPEC-CONFORMANCE-03~draft)
```

A Markdown document declares the same triple canonically in frontmatter:

```markdown
---
spec_refs:
  - id: SPEC-CONFORMANCE-03~draft
    path: docs/specs/intent-conformance.md
    anchor: SPEC-CONFORMANCE-03~draft
---
```

A conforming resolver is a *reader* of declared links only — it never invents
linkage where none is declared, and it does **not** enforce a four-status
traceability model (an explicit non-goal, DOC-15 §1.3 / DOC-38 §1.3). trusty-common's
`intent_source` (DOC-15 §6) is one *illustrative* resolver, documented as an
informative annex in DOC-38 (Annex A), not as part of the standard itself; extending
it to read SLD references is tracked as a follow-up (DOC-38 §10, F1), not implemented
by this catalog change. See DOC-38 for the normative reference grammar, the
frontmatter schema, and the per-language idioms.

[sld]: #spec-linked-documentation-sld
