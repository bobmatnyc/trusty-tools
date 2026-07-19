# DOC-46 — Architecture Decision Records (ADR) as First-Class Documentation Artifact

**Status:** DRAFT
**Subsystem:** Documentation / Architecture governance
**Owner:** Architecture / Technical Leadership
**Last-updated:** 2026-07-19
**Spec ID:** `SPEC-ADR-01` (DOC-46)
**Builds on:** `docs/adr/README.md` (existing ADR convention); DOC-38 (Spec-Linked Documentation / SLD); DOC-30 (Project Manager vision — decision vetting); the tm-adr bundled skill (`crates/trusty-mpm/src/assets/skills/tm-adr.md`)

---

## 1. Motivation & Problem Statement

### Current State

The trusty-tools repository maintains a de-facto ADR practice:
- **14 existing ADRs** (ADR-0001 through ADR-0013) document foundational architectural decisions
- ADRs are **informally opt-in**, created ad-hoc when maintainers recognize a significant choice
- Existing decisions live only in **memory or informal channels** (meetings, memory palace)
- **No consistency check**: new ADRs are not systematically vetted against prior decisions; silent contradictions can arise
- ADRs are treated as **ancillary documentation**, not peer to Specs (DOC-38 SLD) and Requirements (DOC-43 — future)
- **No mechanical enforcement** — missing ADRs, contradictions, and linting gaps are not surfaced by CI

### Problem

**Bob's directive (2026-07-19):** "Formalize ADRs so that any decision can be vetted against previous ones for consistency. Should be a peer to Spec, and Req."

The core issue: **architectural decisions are treated as optional narratives, not as a governed, first-class artifact class**. This allows:
- **Inconsistent future decisions** that contradict earlier accepted choices without acknowledgment
- **Lost decision rationale** when team members leave or memories fade
- **No audit trail** for why the system is shaped as it is — impossible to trace "did we already consider this alternative?"

### Why This Matters

A healthy architecture evolves through **intentional decision-making**, not drift. When decisions are first-class:
1. New architects understand constraints and cannot accidentally undo expensive choices
2. Consistency vetting catches contradictions **before** they land
3. The decision corpus becomes a **reference for trade-offs**, not an obstacle course

---

## 2. Vision: ADR as First-Class Artifact (Peer to Spec & Req)

### Three Artifact Classes

The repository recognizes three **co-equal, cross-linked documentation artifact classes**:

| Artifact | Purpose | Example | Decision drivers | Approval gate |
|---|---|---|---|---|
| **Requirement (Req)** | *What* must be true | "The installer must be idempotent" | Functional/product mandates | Requirements authority (Product/Leadership) |
| **Specification (Spec)** | *How* a subsystem works | "DOC-38: Spec-Linked Documentation (SLD) anchors each governed section with `{#SPEC-…}`" | Implementation detail, subsystem boundaries | Technical / Architecture review |
| **Architecture Decision Record (ADR)** | *Why* a choice was made among alternatives | "ADR-0005: unified harness event bus; chosen over monolithic topic tree because subscribers do not know message types in advance" | Architectural trade-offs, reversibility cost, constraint interactions | Architecture council; **consistency vetting (NEW)** |

**Cross-linking rules:**
- **Specs cite ADRs** for the architectural decisions that shaped them. Example: "This design follows ADR-0005 (unified event bus) rather than topic-tree subscriptions."
- **ADRs cite Specs** when a decision manifests in implementation. Example: "See DOC-20 §3.2 for how this decision is implemented."
- **Reqs may cite ADRs** when a requirement encodes a decision constraint. Example: "Per ADR-0008 (project-identity convention), the system identifies projects by git-root slug."

### ADR is Not Optional — It's Formal

**Old framing (from tm-adr skill v1.0):** "This skill is **opt-in**. Only use it for decisions that are architecturally significant AND costly to reverse."

**New framing (this spec):** ADRs are **mandatory for architectural decisions**. The decision itself is required; creating a record of it is also required. There is no "decide informally" path for architectural choices. If a decision is significant enough to shape the system, it is significant enough to record.

Operationally: ADRs remain **high-bar** (rare). A typical quarter may produce 1–3 new ADRs, not dozens. But when they are written, they are *formal artifacts*, not optional polish.

---

## 3. ADR Template & Structure

### Required Frontmatter & Fields

Every ADR must carry:

```markdown
# NNNN. <Short Title of Decision>

- **Status:** Proposed | Accepted | Rejected | Superseded by MMMM | Amended by MMMM
- **Date:** YYYY-MM-DD
- **Scope:** Workspace-wide (or: crate `<name>` or subsystem `<name>`)
- **Reversibility Cost:** (Low | Medium | High) — cost to undo this later
- **Decision Drivers:** (Comma-separated list of forces: e.g., "MSRV constraint, performance ceiling, cross-crate boundary")
- **Cross-ref to related decisions:** (see "Related Decisions" section below)
```

### Core Sections (Nygard Format)

1. **Context** — the forces, constraints, and alternatives. Answer: why was a decision needed now?
2. **Decision** — what was decided, stated in active voice: "We will…". Unambiguous.
3. **Consequences** — what becomes easier or harder. Honest tradeoffs; both positive and negative.

### NEW: "Related Decisions" Section — The Consistency-Vetting Protocol

**This is the core innovation.** Before an ADR can transition to **Accepted**, the author (or reviewer) MUST sweep the existing ADR corpus and record the outcome.

```markdown
## Related Decisions

Vetted against prior ADRs on 2026-07-19:

- **ADR-0097 (Hypothetical: Use X for event bus):** Consistent. This ADR refines event-bus boundary per ADR-0097; no conflict.
- **ADR-0098 (Hypothetical: Project-identity scheme):** Extends. We adopt ADR-0098's full-path slug; this ADR adds git-root marker-file identity.
- **ADR-0099 (Hypothetical: MSRV floor policy):** Supersedes. This ADR replaces ADR-0099's 1.88 floor with 1.91 (driven by dependency constraints).
- **ADR-0100 (Hypothetical: Daemon lifecycle model):** Conflict(resolved). ADR-0100 restricted daemon to headless mode; this ADR introduces interactive mode. **Resolution:** Amended ADR-0100 status to "Amended by ADR-NNNN".

No prior decisions contradict this choice. Summary: Consistent with existing decisions; no silent contradictions.

*(Example uses fictional ADR numbers 0097–0100 to illustrate verdict codes and consequences; these are not real decisions.)*
```

**Verdict codes:**
- **Consistent** — aligns with prior decision; no conflict.
- **Extends** — builds on a prior decision; compatible evolution.
- **Supersedes** — replaces a prior decision. **Action:** flip old ADR's status to `Superseded by NNNN`.
- **Amended by** — refines (rather than replaces) a prior decision. **Action:** old ADR status becomes `Amended by NNNN`.
- **Conflict (resolved)** — contradicts a prior decision. **Action:** must document *how* the conflict was resolved (e.g., which takes precedence, or both are valid in different scopes). **Note:** shipping an ADR that conflicts with an accepted one without resolving the conflict is a lint failure and blocks PR merge.

**Vetting scope:**
- All Accepted ADRs (prior decisions still in force)
- Memory palace decision drawers (via trusty-memory MCP or project memory, if populated)
- Cross-crate `docs/<crate>/decisions/` (if that crate maintains its own ADR sequence)

**Outcome of vetting:**
- Every new ADR must record *some* vetting result in "Related Decisions"
- If no prior ADRs exist or none are affected, state: "No prior decisions to vet against."
- A new ADR that contradicts an Accepted decision **must either**:
  - Resolve the contradiction explicitly in the "Related Decisions" section, **or**
  - Be reworked to eliminate the conflict, **or**
  - Be rejected (never silently land with a contradiction)

---

## 4. ADR Status Lifecycle

```
┌─────────┐     ┌──────────┐     ┌───────────────────┐
│Proposed │────▶│ Accepted │────▶│ Superseded by NNN │
└─────────┘     └──────────┘     └───────────────────┘
      │                                      ▲
      └─────────────────────────────────────┘
           (direct to superseded if
            already superseded by the
            time it lands)

      ┌────────┐
      │Rejected│
      └────────┘
      (considered but not adopted)

      ┌──────────────────┐
      │ Amended by NNNN  │
      └──────────────────┘
      (refined by, not replaced by, a later ADR)
```

- **Proposed** — draft, under review. Consistency vetting is optional while Proposed, but required before acceptance.
- **Accepted** — approved, in force. All Accepted ADRs form the current decision set; they govern system architecture and future decisions.
- **Superseded by NNNN** — replaced by a later ADR. Links to the new ADR. Old decision is no longer in force; the new one takes precedence.
- **Amended by NNNN** — refined (not replaced) by a later ADR. The prior decision is still in force, but qualified by the amendment.
- **Rejected** — considered but not adopted. Kept for the record so the same choice is not re-litigated.

---

## 5. ADR Index & Discovery

### `docs/adr/INDEX.md` (NEW — replaces generic README)

The index is the **single source of truth** for the ADR corpus and the **cheap surface for vetting**.

```markdown
# ADR Index — Accepted Decisions

Last updated: 2026-07-19 | Format version: 1.0

| # | Title | Status | One-line decision | Scope |
|---|---|---|---|---|
| 0001 | Design/research/ADR docs live in top-level `docs/` | Accepted | All documentation lives in `docs/` | Workspace |
| 0002 | Single-install convention | Accepted | All major crates install to same location | Workspace |
| 0003 | MSRV 1.88 and per-crate edition policy | Superseded by 0010 | MSRV floor drives edition choice | Workspace |
| ... | ... | ... | ... | ... |
```

**Index purposes:**
- **Vetting surface:** author can sweep this table to find related priors in seconds
- **CI gate:** script verifies index is in sync with files on disk (every ADR file has an index entry; every index entry has a file)
- **Discoverability:** anyone can read the index to understand the decision landscape without reading 14 full documents

### Crate-Specific Decisions

Crates may maintain their own `docs/<crate>/decisions/` directory with independent numbering:
- Example: `docs/trusty-search/decisions/0001-bm25-scoring-model.md`
- Numbering is independent per crate
- Workspace ADRs and crate-specific decisions can cross-reference each other

---

## 6. Mechanical Enforcement & CI Gates

### Phase 1: Linting Script (Scoped, Pragmatic)

Propose a new script: `scripts/check_adr.sh` (or fold into an extended `scripts/check_sld.sh`).

**Scope — verify only these checks; implementation is a follow-up issue:**

1. **Unique numbering:** each file matches pattern `docs/adr/NNNN-*.md` with NNNN in 0000–9999 range; no duplicates.
2. **Sequential continuity:** if files are numbered 0001–0013, there are no gaps (e.g., no 0002 missing).
3. **Status field validity:** Status is one of {Proposed, Accepted, Rejected, Superseded by NNNN, Amended by NNNN}.
4. **Supersedes bidirectionality:** if ADR-0003 says "Superseded by 0010", then ADR-0010 must cite ADR-0003 in its "Related Decisions" section. (Detect orphaned supersedes links.)
5. **INDEX.md in sync:** every ADR file in `docs/adr/` with number NNNN ≥ 0001 must have an entry in `docs/adr/INDEX.md`; conversely, every index entry must have a corresponding file on disk.

**Phase 2 (Human/LLM Review, NOT Mechanical Check):**
- **Semantic consistency:** if two Accepted ADRs make mutually exclusive claims, identify the contradiction and require manual resolution in "Related Decisions" or status change. This check is advisory and requires human or LLM judgment — not automated.

**Recommended command-line interface:**

```bash
bash scripts/check_adr.sh               # exit 0 if all checks pass
bash scripts/check_adr.sh --index-only  # only verify INDEX.md sync
bash scripts/check_adr.sh --verbose     # detailed per-ADR report
```

**CI integration:** Add to `.github/workflows/` as a gated check (fail on violation). The check runs on all PRs that touch `docs/adr/` or `docs/specs/`.

### Phase 2: Knowledge Graph Integration (Future)

If trusty-memory is wired, decisions can be stored in a "decisions" drawer, keyed by ADR number. The check script could query the drawer to find "undocumented decisions" (decisions in memory but without a recorded ADR).

---

## 7. Workflow Integration

### When to Create an ADR

Write an ADR when:
1. **Architectural decision arises** — a spec discusses a subsystem design choice, or a PR introduces a major change
2. **Decision is significant & durable** — will shape the codebase for months/years, not a one-off
3. **Decision encodes trade-offs** — choosing *this* approach means *not* choosing alternatives; consequences matter

**Typical sources:**
- Design reviews: "We decided on X protocol instead of Y"
- Dependency/tooling decisions: "We will use Rust 1.91 MSRV"
- Project conventions: "Code goes in `docs/`, not scattered"
- Deprecations: "We are retiring component X"

**NOT an ADR:**
- Routine bug fixes, feature PRs, refactors (unless they involve an architectural choice)
- Library version bumps (unless they force an MSRV change or API boundary shift)
- Style/lint config changes

### Delivery Chain: Where ADR Fits

```
1. Decision arises in spec/PR/issue discussion
          ↓
2. Author (or reviewer) drafts ADR as Proposed
          ↓
3. ADR PR review: check "Related Decisions" vetting — is it consistent?
          ↓
4. If consistent: approve & merge; flip status to Accepted
   If contradicts: require resolution in "Related Decisions" or reject
          ↓
5. New ADR is live; cited from specs, code, and memory
```

### PM / Architecture Council Responsibility

- **Bob (or architecture lead) reviews ADR PRs** before acceptance. Decision is not final until ADR is Accepted.
- **New ADR PRs trigger consistency check:** "Has the author swept priors?" Review the "Related Decisions" section.
- **Memory palace updates:** If a design decision is significant & already in memory, the ADR is the opportunity to surface it. PM records Bob-level decisions as ADRs during sprint planning.

---

## 8. Peer Parity: ADR, Spec, and Req

### Equivalence Table

| Aspect | ADR | Spec (DOC-38) | Req (DOC-43) |
|---|---|---|---|
| **Purpose** | Why a choice was made | How a system works | What must be true |
| **Scope** | Architectural decisions | Subsystem design & boundaries | Functional mandates |
| **Governance** | Architecture council | Technical review | Product/leadership |
| **Approval** | ADR-to-Accepted gate | Spec review (internal consistency) | Requirement validation |
| **Immutability** | Yes (supersede, don't edit) | Yes (SLD anchors are stable) | Yes (version by requirement ID) |
| **Cross-ref** | Specs cite ADRs; ADRs cite specs; reqs may cite ADRs | Cites ADRs when decisions are shaped the design | May cite ADRs for constraint traceability |
| **Deprecation** | Superseded by ADR-NNNN | Replaced by new spec (DOC-NN) | Versioned or deprecated via req ID |
| **Naming** | `docs/adr/NNNN-kebab.md` | `docs/specs/DOC-NN-title.md` | `docs/requirements/REQ-NNN-title.md` (future) |
| **Index** | `docs/adr/INDEX.md` | `docs/specs/INDEX.md` (assumed) | `docs/requirements/INDEX.md` (future) |

---

## 9. Migration & Bootstrapping

### Existing ADRs (0001–0013)

Current ADRs are **grandfathered as Accepted** without re-vetting. They form the baseline decision set.

**Action (Phase 1, this PR):**
- Seed `docs/adr/INDEX.md` from existing ADR files
- Record their statuses as currently documented
- No changes to existing ADR content (unless an ADR references another and needs updating)

### "Organizational Memory" → ADR Conversion

Decisions currently living in memory-palace drawers, Slack, or meeting notes are **candidates for ADR conversion**. This is **selective, opt-in**:
- Not a bulk backfill (that defeats the purpose)
- PM or architect proposes: "This decision is significant & durable enough to warrant an ADR"
- Author drafts the ADR, vets it, and merges as Accepted

Example: "Unified harness event bus" (ADR-0005) was a key design choice; "per-instance GUID identity" (ADR-0012) was another. Future decisions (e.g., "multi-session persistence strategy", "MCP authentication model") should follow the same path: memory → ADR → Accepted.

---

## 10. Governance & Change Control

### ADR PR Checklist (for reviewers)

When reviewing an ADR PR (new or updated), ensure:

- [ ] **Title is clear & imperative** — e.g., "Use X for Y", not "Thoughts on X"
- [ ] **Status is appropriate** — Proposed for new ADRs; Accepted if vetting is complete and decided
- [ ] **Context section is factual** — cites constraints, prior art, issues; no opinions
- [ ] **Decision is unambiguous** — active voice, one choice per ADR
- [ ] **Consequences are honest** — lists positive, negative, and tradeoffs; no glossing over cost
- [ ] **"Related Decisions" is complete** — author has swept priors; verdict codes are accurate
- [ ] **No silent contradictions** — if a conflict exists, it is resolved in the "Related Decisions" section
- [ ] **Supersedes links are bidirectional** — old ADR status is updated if this one supersedes it
- [ ] **Numbering follows convention** — NNNN is the next sequential number
- [ ] **File matches `docs/adr/NNNN-kebab-title.md`** — no spaces, lowercase, kebab-case

### What Blocks ADR Acceptance

- [ ] Contradicts an Accepted ADR without resolving the conflict
- [ ] "Related Decisions" is empty or incomplete (author didn't vet priors)
- [ ] Numbering violates sequence or duplicates an existing ADR
- [ ] Status field is invalid or missing

---

## 11. Example: ADR-0014 (Placeholder)

**Use case:** A new ADR proposed for decision on multi-session persistence strategy.

```markdown
# 0014. Centralized session-state persistence via trusty-memory

- **Status:** Proposed
- **Date:** 2026-07-19
- **Scope:** Workspace-wide
- **Reversibility Cost:** High (multi-session logic relies on persistent state)
- **Decision Drivers:** Multi-session autonomy (DOC-17), session resumption, failover

## Context

... (context on multi-session architecture challenges) ...

## Decision

We will persist session state (pane buffers, task history, undo stack) in trusty-memory
rather than in ephemeral tmux panes or disk-based SQLite.

## Consequences

**Positive:**
- Session data survives daemon restarts
- Easier to share state across sessions (single source of truth)

**Negative:**
- Adds network round-trips for every pane read
- trusty-memory becomes critical path; failure degrades the whole system

**Tradeoffs:**
- Can be mitigated by local cache + eventual consistency

## Related Decisions

Vetted against prior ADRs on 2026-07-19:

- **ADR-0099 (Hypothetical: Use X for caching):** Consistent. Session persistence is orthogonal to caching strategy.
- **ADR-0100 (Hypothetical: Daemon lifecycle model):** Extends. This ADR refines daemon lifecycle per ADR-0100's overall model; no conflict.
- **ADR-0101 (Hypothetical: Instance identity scheme):** Extends. Per-session GUIDs are keys for lookups per ADR-0101's scheme.

No conflicts. Ready to accept.

*(Example uses fictional ADR numbers 0099–0101 to illustrate verdict codes; these are not real decisions.)*
```

---

## 12. Success Criteria

This spec is successful when:

1. **ADRs are first-class** — treated as peer to Specs and Reqs in documentation governance
2. **Consistency is enforced** — new ADRs are vetted against priors before acceptance; "Related Decisions" section prevents silent contradictions
3. **Discovery is cheap** — `docs/adr/INDEX.md` is the quick reference; anyone can scan for related decisions in seconds
4. **Workflow is clear** — PMs and architects follow a consistent process: draft → vet (consistency check) → approve → accept
5. **CI catches violations** — `scripts/check_adr.sh` runs on all ADR PRs and fails if numbering, status, or bidirectional-link rules are broken (checks 1–5 in §6)

---

## Appendix: Files Affected by This Spec

| File | Change |
|---|---|
| `docs/adr/README.md` | Update: reflect formal status; replace "opt-in" with "mandatory"; link to DOC-46 |
| `docs/adr/INDEX.md` | NEW: seeded from existing ADRs, format per §5 |
| `docs/adr/template.md` | Update: add "Related Decisions" section; add frontmatter fields (Status, Scope, Reversibility Cost, Decision Drivers) |
| `docs/reference/documentation-layout.md` | Update: mention ADRs alongside Specs and Reqs as first-class artifacts |
| `crates/trusty-mpm/src/assets/skills/tm-adr.md` | Update: remove "opt-in" framing; link to DOC-46 for formal standard (bundled asset v2.0.0) |
| `CHANGELOG.md` | Entry (per CHANGELOG-per-PR convention): "docs(spec): DOC-46 — formalize ADRs as first-class artifact with consistency-vetting protocol" |
| `scripts/check_adr.sh` | FUTURE (follow-up issue): implement linting checks |

---

## Appendix: References

- Michael Nygard, "Documenting Architecture Decisions" (2011): https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions
- `docs/adr/README.md` — existing ADR convention for trusty-tools
- DOC-38 — Spec-Linked Documentation (SLD) standard
- DOC-30 — Project Manager vision & lifecycle orchestration
- The tm-adr bundled skill — current opt-in convention
