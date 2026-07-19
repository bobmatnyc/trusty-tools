# trusty-code UI Design Handoff

This directory contains the normative screen mockups for trusty-code's harness UI, referenced throughout **DOC-39** (`docs/specs/trusty-code-harness-ui.md`). These 14 PNG mockups and the accompanying PDF explainer form the **design-system and layout foundation** for the trusty-code interactive harness shell.

## Normative Reference

The mockups landed **2026-07-19** (3 days after initial proposal 2026-07-16) as DOC-39 §8 visual anchors. They were previously untracked in the repository and became the source of truth for the GUI shell skeleton rebuilt in #3153.

**Design System:** The mockups predate the **Foundry design system** (v1, `docs/design/UI/design-system/`). Where they conflict on colors, typography, or tokens, **Foundry wins**. Where they show **structure, layout, and interaction flow**, the mockups are authoritative and are the source of truth for the #3153 shell rebuild.

## Screens & DOC-39 Section Map

Each filename maps to a normative DOC-39 section ID cited below.

| ID | Filename | DOC-39 Sections |
|---|---|---|
| **7a** | `7a-open-project.png` | §2 (shell cold-start empty state), §4.2 (project binding states), §4.2.1 (daemon-served directory picker), §4.3 (index readiness), §7A (project-first entry flow), §7B (service hydration on selection), §7E (project creation) |
| **7b** | `7b-project-attached.png` | §4.2 (project name attaches to workstream switcher and status bar on bind) |
| **8a** | `8a-docked-monitors.png` | §4.6 (monitor lifecycle — live state, docked to right rail), §4.8 (attribution: recall score + injected status), §6.2 (Phase 1 inline monitor card) |
| **8b** | `8b-inline-monitors.png` | §4.6 (monitor lifecycle — settled state, collapsed into inline thread card), §4.6.1 (inline card as durable record), §4.8 (recall attribution), §6.2 (Phase 1 surface) |
| **9a** | `9a-service-dropdown.png` | §4.6 (progressive-disclosure ladder rung 2: mini activity dropdown on dot-click), §6.2 (Phase 1) |
| **9b** | `9b-monitor-columns.png` | §4.6 (progressive-disclosure ladder rung 3: pinnable monitor columns), §4.6.1 (demoted to transient trace mode, not default layout), §4.8 (recall attribution in columns) |
| **10a** | `10a-project-ide.png` | §1.3 (Project/IDE half is scope-flagged, not scope-committed), §4.7 (⌘K find box, operator-initiated browsing), §4.8 (per-line gutter attribution), §4.10 (back-nav from artifacts), §5.1 (tool attribution in gutter) |
| **10b** | `10b-agents.png` | §5.1 (agent roster with per-agent todos), §5.4 (agent snapshot endpoint, `model` field deferred), §6.3 (agent-roster `model` field explicitly deferred) |
| **10c** | `10c-memory.png` | §4.8 (memory entries show requester, workstream, recall count), §5.1 (recall attribution) |
| **10d** | `10d-search.png` | §4.7 (Search tab as audit trail of agent search operations — **not a search input field**), §5.1 (structured search event: lane, query, hit count, latency, requesting agent), §5.2 (search audit trail requires `agent_id`), §6.2 (Phase 1 surface) |
| **10e** | `10e-workflow.png` | §4.9 (workflow guardrails delivery pipeline: spec → epic → issue → branch → PR → review → deploy; new subsystem deferred), §5.1 (branch/PR/dirty-tree subsystem), §6.3 (explicitly deferred Phase 2+) |
| **10f** | `10f-files.png` | §4.10 (back-nav from artifacts deep-links into activity viewer) |
| **11a** | `11a-artifacts.png` | §4.10 (agent file artifacts link into activity viewer with back-nav), §5.1 (artifact deep-links), §5.2 (tool attribution powers artifact links) |
| **11b** | `11b-activity-viewer.png` | §4.10 (activity viewer with breadcrumb back-nav, deep-linked from artifacts; shows "what drove this change"), §5.2 (tool attribution for diff attribution) |

## Files in This Directory

- **`Harness UI Rethink Proposal Explainer.pdf`** — 2.2 MB design handoff document; design rationale, 10 principles, visual system, tokens, shell skeleton. Read this first to understand the design intent.

- **`handoff/`** — 14 normative screen mockups (PNG, ~4.3 MB total):
  - `7a-open-project.png` (292 KB) — cold-start, project picker
  - `7b-project-attached.png` (259 KB) — project attached to workstream
  - `8a-docked-monitors.png` (392 KB) — live service monitors in right rail
  - `8b-inline-monitors.png` (371 KB) — settled monitors as thread cards
  - `9a-service-dropdown.png` (277 KB) — mini activity dropdown (rung 2)
  - `9b-monitor-columns.png` (307 KB) — pinned monitor columns (rung 3, trace mode)
  - `10a-project-ide.png` (300 KB) — Project/IDE half (scope-flagged)
  - `10b-agents.png` (312 KB) — agent roster with todos
  - `10c-memory.png` (279 KB) — memory entries with attribution
  - `10d-search.png` (292 KB) — Search tab audit trail
  - `10e-workflow.png` (366 KB) — workflow delivery pipeline (deferred)
  - `10f-files.png` (281 KB) — file viewer
  - `11a-artifacts.png` (214 KB) — artifact panel
  - `11b-activity-viewer.png` (275 KB) — activity viewer with back-nav

## Design System

These mockups use a visual token system and design language that **predate** the **Foundry design system** (v1). Foundry is the **authoritative design system** going forward — see `docs/design/UI/design-system/` for tokens, component states, typography, and color roles.

**When the mockups and Foundry conflict:**
- **Tokens, colors, typography, spacing:** Use Foundry.
- **Layout, nesting, interaction flow, state transitions:** Use the mockups (they are more recent and more precise for the specific harness context).

## References

- **DOC-39 — trusty-code Harness UI:** `docs/specs/trusty-code-harness-ui.md` — The normative functional spec. Every screen in this directory is referenced by section ID in §8 (Visual system) and throughout §4 (10 design principles as functional requirements) and §5 (API surface).
- **Epic #3153 — GUI Shell Rebuild:** These mockups are the source of truth for the trusty-code-gui rebuild launched in #3153 after the scaffold was built without them.
- **Foundry Design System (v1):** `docs/design/UI/design-system/` — Robot-themed, rust-colored; defines tokens, typography, components, and light/dark theme ("Night Shift") integration. Normative for visual implementation.
- **DOC-38 (Spec-Linked Documentation):** `docs/specs/README.md` — DOC-39 is catalogued here.
