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
| DOC-45 | `SPEC-CREDAUTH-01~draft` … `-11~draft` | [The Credential Authority Model: Principals, Scoping, Revocation, Audit, and the Sub-Agent Boundary](./DOC-45-credential-authority-model.md) | `trusty-common` — the authority (principal, `CredentialRef`, ACL default-deny, revocation, audit, delivery, at-rest storage); `trusty-agents` — assistant/sub-agent principals, MCP delivery, routed-shell environment scrubbing; `trusty-code` — service principals. Cross-product per owner decision ("#4040 yes for agents and code"). Decision record: [ADR-0026](../adr/0026-credential-grants-do-not-survive-delegation.md) |
| DOC-46 | `SPEC-ADR-01~draft` | [Architecture Decision Records (ADR) as First-Class Documentation Artifact](./DOC-46-adr-standard.md) | documentation standard — architecture governance / consistency vetting (cross-crate) |
| DOC-47 | `SPEC-EVTING-01~draft` … `-04~draft` | [External Event Ingestion — Webhooks & Connector Push](./DOC-47-external-event-ingestion.md) | trusty-agents-common — event seam; trusty-mpm — webhook ingress + goal store; trusty-console — Tailscale Funnel binding |
| DOC-48 | `SPEC-WS-01~draft` … `-09~draft` | [tcode Workstreams: Durable Named Work Aggregation](./DOC-48-tcode-workstreams.md) | trusty-code — activation-lock exclusivity model, multi-client attach transport (shared with trusty-agents #3052), RPC/REST/CLI surfaces |
| DOC-50 | `SPEC-TTUI-01~draft` … `-09~draft` | [trusty-code Interactive TUI: Claude Code Clone over Shared REPL Layer](./DOC-50-tcode-tui-claude-code-clone.md) | trusty-code — interactive terminal UI thin client; trusty-tui shared crate (ratatui REPL extraction from trusty-agents) |
| DOC-51 | `SPEC-TCPLUGIN-01~draft` | [trusty-code Claude Code Plugin Support, Phase 1: Local-Directory Agents + Skills](./DOC-51-tcode-plugin-support-phase1.md) | trusty-code — agent/skill catalog, discovery, dispatch |
| DOC-52 | `SPEC-SHAREDWS-01~draft` … `-06~draft` | [Workstream, Task, and Session: The Canonical Cross-Product Glossary](./DOC-52-shared-workstream-definition.md) | trusty-mpm, trusty-code, trusty-agents, trusty-memory — **AUTHORITATIVE for the terms _workstream_, _task_, and _session_ repository-wide** (a workstream contains many tasks; many sessions attach over its lifetime, one active at a time; trusty-mpm's session ≡ workstream is a sanctioned permanent exception). Also carries workstream lifecycle + resource governance (caps, scope-overlap, reclamation), and reconciles DOC-39/DOC-48/DOC-54 |
| DOC-53 | `SPEC-WSCLAIM-01~draft` … `-04~draft` | [Workstream Claim-Drawer Convention: Cross-Workstream Coordination via trusty-memory](./DOC-53-workstream-claim-drawer-convention.md) | trusty-memory — attribution / drawer conventions; trusty-mpm — PM dispatch protocol |
| DOC-54 | `SPEC-AGENTS-01~draft` … `-08~draft` | [Trusty Agents Product Specification](./trusty-agents-product-spec.md) | trusty-agents — product vision / agent model / eventstream processing / GUI |
| DOC-55 | `SPEC-OKGIMPORT-01~draft` … `-07~draft` | [Universal OKG Importer: Any File Type, Any Connectable System, Assistant-Driven](./okg-universal-importer.md) | trusty-kb — format extraction / connector framework / ingest engine; trusty-agents — connector adapters, assistant-facing tools, deterministic CLI surface |
| DOC-56 | `SPEC-AGENTSYNC-01~draft` … `-07~draft` | [Agent Configuration Sync: The `trusty-agents-agents` Private Monorepo](./trusty-agents-agents-sync.md) | trusty-agents — agent configuration lifecycle, provisioning merge (subsumes #3844), multi-machine sync |
| DOC-57 | `SPEC-AGENTCFG-01~draft` … `-09~draft` | [Five-Section Agent Configuration: Personality / Knowledge / Skills / Listeners / Permissions](./agent-config-five-sections.md) | trusty-agents — agent configuration model, capability declaration (tool→skill wrapping), permissions surface, GUI config pane (**supersedes DOC-54 §5**) |
| DOC-58 | `SPEC-KDIDX-01~draft` … `-06~draft` | [Knowledge Section Addendum: K-d Attached Search Indexes](./DOC-58-knowledge-kd-attached-indexes.md) | trusty-agents — Knowledge section (DOC-57 §4) fourth sub-surface, `tools.search_indexes` config; trusty-search — arbitrary attached indexes (**extends DOC-57 §4 and amends #3935's scope; edits neither in place**) |
| DOC-59 | `SPEC-PMINSTR-01~draft` … `-07~draft` | [P1/P2 Instruction Restructure: Tiered, Cache-Stable, Customizable PM System Prompt Composition](./SPEC-PMINSTR-01-p1-p2-instruction-restructure.md) | trusty-mpm — PM instruction pipeline (`instruction_pipeline.rs`, `instruction_overrides.rs`, `stack_profile.rs`); session-manager (workstream/session persistence) — motivated by issue #4071 |
| DOC-60 | `SPEC-AGENTBUS-01~draft` | [Unified Agent Communication: User ↔ Assistant, Assistant ↔ Sub-Agent, Assistant ↔ Assistant](./DOC-60-bus-based-agent-messaging.md) | trusty-mpm (bus host, daemon) — trusty-agents (assistants, sub-agents, ctrl) — trusty-channels (Slack/Telegram/etc.) — trusty-memory (consolidation target) — trusty-search (index target) |
| DOC-61 | `SPEC-AGENTSTD-01~draft` | [Canonical Agent Standard: A Shared Source Model for trusty-mpm, trusty-code, and trusty-agents](./DOC-61-canonical-agent-standard.md) | cross-crate — trusty-mpm (source model owner today), trusty-code (per-product builder, prospective), trusty-agents (assistant/sub-agent split, `agents::config`) |
| DOC-62 | `SPEC-STYLE-01~draft` … `-10~draft` | [Style Modes for Coding Delegation: `hack` / `vibe` / `engineer`](./DOC-62-style-modes-coding-delegation.md) | cross-crate — trusty-agents (delegation surface, `HandoffContext`, preamble carriage); trusty-code (style parameter, internal pipeline selection); trusty-mpm/GUI (style selector, downstream) |
| DOC-63 | `SPEC-OKGSRC-01~draft` … `-14~draft` | [OKG Sources: Per-Assistant Knowledge Sources, Scheduled Refresh, and the Untrusted-Content Boundary](./DOC-63-okg-sources.md) | trusty-agents — assistant home / OKG store, source catalog, scheduled refresh, credential consumption, Knowledge config pane; trusty-kb — `okg` engine; trusty-search — index over the store |
| DOC-64 | `SPEC-CREDPANEL-01~draft` … `-09~draft` | [The Credentials Panel: Per-Assistant Credential Sets, Transfer, and the User-Granted Copy](./DOC-64-credentials-panel.md) | trusty-agents — the assistant configuration surface (panel, backing route, audited actions); trusty-common — the authority it is a **client** of (`Principal`, `CredentialRef`, grants, revocation state, audit stream). Encodes the owner's 2026-08-03 #4040 answers (one store per instance; assistant asks, only the user grants; one audit stream at per-call grain) and finds that DOC-45 §9.1's landed record shape **requires amendment** to carry the panel's events (§10.3) |

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
> **Next free `DOC-N` = `DOC-65`** (updated 2026-08-03 — DOC-64 claimed by
> [The Credentials Panel](./DOC-64-credentials-panel.md) (#4663), verified free four
> ways per the scan-before-claim rule: no filename claim or header self-label under
> `docs/specs/**` or `docs/trusty-installer/research/02-design/**` on `origin/main`
> (`99f085a3`); no claim in any **OPEN** pull request (#4526, #4578, #4640, #4641 —
> none a spec), the check `scripts/check_doc_numbers.sh` structurally cannot make;
> no claim on any remote branch, the only unmerged numbered spec being
> `spec-twin-lead-architecture`'s `DOC-44`; and `check_doc_numbers.sh` clean at
> 95 docs / 89 claims. Previously — updated 2026-08-01 — DOC-63 claimed by
> [OKG Sources](./DOC-63-okg-sources.md), and DOC-62 by the concurrently-authored
> Style Modes for Coding Delegation spec (PR #4529) — the two claimed DOC-62
> simultaneously and OKG Sources renumbered; DOC-60 claimed by
> [Unified Agent Communication](./DOC-60-bus-based-agent-messaging.md) (retitled from
> "Bus-Based Agent Messaging" in its Rev 1 update to reflect its now-unified
> user↔assistant / assistant↔sub-agent / assistant↔assistant scope; filename
> unchanged) and DOC-61 by
> [Canonical Agent Standard](./DOC-61-canonical-agent-standard.md); DOC-59 claimed by
> [P1/P2 Instruction Restructure](./SPEC-PMINSTR-01-p1-p2-instruction-restructure.md),
> DOC-58 claimed by
> [Knowledge Section Addendum: K-d Attached Search Indexes](./DOC-58-knowledge-kd-attached-indexes.md),
> DOC-55 claimed by
> [Universal OKG Importer](./okg-universal-importer.md), DOC-56 by
> [Agent Configuration Sync](./trusty-agents-agents-sync.md), and DOC-57 by
> [Five-Section Agent Configuration](./agent-config-five-sections.md)):
> the highest cataloged number is now **DOC-62**; before it, **DOC-59** claimed the next free number after
> **DOC-58**, itself claimed after
> **DOC-54** ([Trusty Agents Product Specification](./trusty-agents-product-spec.md))
> per the scan-before-claim rule ([DOC-38 §4.1](./spec-linked-documentation.md) — a catalog's "next free"
> note is a *hint, not authority*; the scan is authoritative). DOC-44/45 are claimed by the
> unmerged `spec-twin-lead-architecture` branch ([DOC-44 Engineering Lead Twin Orchestration](https://github.com/bobmatnyc/trusty-tools/tree/spec-twin-lead-architecture));
> DOC-34 is assigned ([`managed-session-config-dir.md`](./managed-session-config-dir.md),
> #1999 — still a catalog gap), DOC-35/36/38/39/40/41/46/47/48/50/51/52/53/54/55/56/57/58 are cataloged (DOC-49 was pre-claimed by PR #3313), and **DOC-37** is
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

> **Catalog note — `DOC-45` claimed, and a correction to the note above.** The
> note above states that *"DOC-44/45 are claimed by the unmerged
> `spec-twin-lead-architecture` branch"*. That is correct for **`DOC-44`** and
> **wrong for `DOC-45`**: the branch carries exactly one numbered spec file
> (`docs/specs/DOC-44-engineering-lead-twin-orchestration.md`, renumbered from
> `DOC-42` on 2026-07-18), and that branch's own catalog note reads *"Next free
> `DOC-N` = `DOC-45`"* — it explicitly does **not** claim 45. The only other
> claimant `DOC-45` ever had was PR **#3039** (*"docs(spec): DOC-45 — Remote MCP
> credential delivery for fleet sessions"*), which is **CLOSED**; its subject
> (#3038) was folded into epic #4040 and is now carried by the `DOC-45` row above
> plus #4568. **`DOC-45` is claimed as of 2026-08-01** by [The Credential
> Authority Model](./DOC-45-credential-authority-model.md) (#4563) — the number
> #4040's own body reserved for this work. Verified free four ways per the
> scan-before-claim rule: no filename claim or header self-label under
> `docs/specs/**` or `docs/trusty-installer/research/02-design/**` on
> `origin/main`; no claim in any **OPEN** pull request — the check
> `scripts/check_doc_numbers.sh` structurally cannot make (its own header says so),
> and the gap that produced the `DOC-62` collision; no claim on the
> `spec-twin-lead-architecture` branch; and no live claimant among closed PRs.
> **`DOC-44` remains claimed by that branch and is still not free.** The next-free
> hint is unchanged at **`DOC-64`** — `DOC-45` is a back-fill of a reserved
> number, not an advance of the high-water mark.

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
